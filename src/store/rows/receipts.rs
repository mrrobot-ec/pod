use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;
use crate::ids::RequestId;
use crate::store::Tx;

use super::runs::{optional_json, parse_id, parse_optional_json, parse_timestamp, timestamp};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ReceiptRow {
    pub caller_fingerprint: String,
    pub request_id: RequestId,
    pub method: String,
    pub payload_hash: String,
    pub state: String,
    pub receipt: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const COLUMNS: &str =
    "caller_fingerprint, request_id, method, payload_hash, state, receipt, created_at, updated_at";
pub fn insert(tx: &Tx<'_>, row: &ReceiptRow) -> Result<()> {
    tx.0.execute("INSERT INTO receipts (caller_fingerprint, request_id, method, payload_hash, state, receipt, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)", params![row.caller_fingerprint, row.request_id.as_str(), row.method, row.payload_hash, row.state, optional_json(row.receipt.as_ref()), timestamp(&row.created_at), timestamp(&row.updated_at)])?;
    Ok(())
}
pub fn get(tx: &Tx<'_>, fingerprint: &str, request_id: &RequestId) -> Result<Option<ReceiptRow>> {
    Ok(tx
        .0
        .query_row(
            &format!(
                "SELECT {COLUMNS} FROM receipts WHERE caller_fingerprint = ?1 AND request_id = ?2"
            ),
            params![fingerprint, request_id.as_str()],
            from_row,
        )
        .optional()?)
}
pub fn list(tx: &Tx<'_>, limit: usize) -> Result<Vec<ReceiptRow>> {
    let mut statement = tx.0.prepare(&format!(
        "SELECT {COLUMNS} FROM receipts ORDER BY created_at DESC, rowid DESC LIMIT ?1"
    ))?;
    Ok(statement
        .query_map([limit as i64], from_row)?
        .collect::<rusqlite::Result<_>>()?)
}
pub fn update_state(
    tx: &Tx<'_>,
    fingerprint: &str,
    request_id: &RequestId,
    value: &str,
) -> Result<()> {
    update_composite(tx, fingerprint, request_id, "state", value)
}
pub fn update_receipt(
    tx: &Tx<'_>,
    fingerprint: &str,
    request_id: &RequestId,
    value: Option<&Value>,
) -> Result<()> {
    update_composite(
        tx,
        fingerprint,
        request_id,
        "receipt",
        &optional_json(value),
    )
}
pub fn update_updated_at(
    tx: &Tx<'_>,
    fingerprint: &str,
    request_id: &RequestId,
    value: DateTime<Utc>,
) -> Result<()> {
    update_composite(
        tx,
        fingerprint,
        request_id,
        "updated_at",
        &timestamp(&value),
    )
}

pub fn prune(tx: &Tx<'_>, now: DateTime<Utc>) -> Result<()> {
    let cutoff = timestamp(&(now - chrono::Duration::days(30)));
    tx.0.execute("DELETE FROM receipts WHERE updated_at < ?1", [&cutoff])?;
    tx.0.execute(
        "DELETE FROM receipts WHERE rowid IN (
             SELECT rowid FROM receipts ORDER BY updated_at DESC, rowid DESC LIMIT -1 OFFSET 10000
         )",
        [],
    )?;
    Ok(())
}

fn update_composite<V: rusqlite::types::ToSql + ?Sized>(
    tx: &Tx<'_>,
    fingerprint: &str,
    request_id: &RequestId,
    column: &str,
    value: &V,
) -> Result<()> {
    tx.0.execute(
        &format!(
            "UPDATE receipts SET {column} = ?1 WHERE caller_fingerprint = ?2 AND request_id = ?3"
        ),
        params![value, fingerprint, request_id.as_str()],
    )?;
    Ok(())
}
fn from_row(row: &Row<'_>) -> rusqlite::Result<ReceiptRow> {
    Ok(ReceiptRow {
        caller_fingerprint: row.get(0)?,
        request_id: parse_id(row.get(1)?, 1)?,
        method: row.get(2)?,
        payload_hash: row.get(3)?,
        state: row.get(4)?,
        receipt: parse_optional_json(row.get(5)?, 5)?,
        created_at: parse_timestamp(row.get(6)?, 6)?,
        updated_at: parse_timestamp(row.get(7)?, 7)?,
    })
}

/// Deletes every receipt (`reset --scope all`).
pub fn delete_all(tx: &Tx<'_>) -> Result<()> {
    tx.0.execute("DELETE FROM receipts", [])?;
    Ok(())
}
