use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;
use crate::ids::{DeliveryId, MessageId, RunId};
use crate::store::Tx;

use super::runs::{
    optional_timestamp, parse_id, parse_json, parse_optional_timestamp, parse_timestamp, timestamp,
    update_column,
};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DeliveryRow {
    pub id: DeliveryId,
    pub run_id: RunId,
    pub mailbox: String,
    pub consumer_generation: u32,
    pub message_ids: Vec<MessageId>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub acknowledged_at: Option<DateTime<Utc>>,
}

const COLUMNS: &str =
    "id, run_id, mailbox, consumer_generation, message_ids, status, created_at, acknowledged_at";

pub fn insert(tx: &Tx<'_>, row: &DeliveryRow) -> Result<()> {
    tx.0.execute("INSERT INTO deliveries (id, run_id, mailbox, consumer_generation, message_ids, status, created_at, acknowledged_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)", params![row.id.as_str(), row.run_id.as_str(), row.mailbox, row.consumer_generation, ids_json(&row.message_ids), row.status, timestamp(&row.created_at), optional_timestamp(row.acknowledged_at.as_ref())])?;
    Ok(())
}

pub fn get(tx: &Tx<'_>, id: &DeliveryId) -> Result<Option<DeliveryRow>> {
    Ok(tx
        .0
        .query_row(
            &format!("SELECT {COLUMNS} FROM deliveries WHERE id = ?1"),
            [id.as_str()],
            from_row,
        )
        .optional()?)
}
pub fn list(tx: &Tx<'_>, limit: usize) -> Result<Vec<DeliveryRow>> {
    let mut statement = tx.0.prepare(&format!(
        "SELECT {COLUMNS} FROM deliveries ORDER BY created_at DESC, rowid DESC LIMIT ?1"
    ))?;
    Ok(statement
        .query_map([limit as i64], from_row)?
        .collect::<rusqlite::Result<_>>()?)
}
pub fn outstanding(tx: &Tx<'_>, mailbox: &str) -> Result<Option<DeliveryRow>> {
    Ok(tx
        .0
        .query_row(
            &format!(
                "SELECT {COLUMNS} FROM deliveries WHERE mailbox = ?1 AND status = 'outstanding'"
            ),
            [mailbox],
            from_row,
        )
        .optional()?)
}
pub fn update_status(tx: &Tx<'_>, id: &DeliveryId, value: &str) -> Result<()> {
    update_column(tx, "deliveries", "status", "id", id.as_str(), value)
}
pub fn update_acknowledged_at(
    tx: &Tx<'_>,
    id: &DeliveryId,
    value: Option<DateTime<Utc>>,
) -> Result<()> {
    update_column(
        tx,
        "deliveries",
        "acknowledged_at",
        "id",
        id.as_str(),
        &optional_timestamp(value.as_ref()),
    )
}

/// Fences every outstanding Delivery of `run` issued for a consumer generation
/// below `generation`; returns how many were fenced.
pub fn fence_below_generation(tx: &Tx<'_>, run: &RunId, generation: u32) -> Result<u32> {
    Ok(tx.0.execute(
        "UPDATE deliveries SET status = 'fenced' WHERE run_id = ?1 AND status = 'outstanding' AND consumer_generation < ?2",
        params![run.as_str(), generation],
    )? as u32)
}

fn ids_json(ids: &[MessageId]) -> String {
    Value::from(ids.iter().map(MessageId::as_str).collect::<Vec<_>>()).to_string()
}

fn parse_ids(value: String, column: usize) -> rusqlite::Result<Vec<MessageId>> {
    match parse_json(value, column)? {
        Value::Array(items) => items
            .into_iter()
            .map(|item| parse_id(item.as_str().unwrap_or_default().to_owned(), column))
            .collect(),
        _ => Ok(Vec::new()),
    }
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<DeliveryRow> {
    Ok(DeliveryRow {
        id: parse_id(row.get(0)?, 0)?,
        run_id: parse_id(row.get(1)?, 1)?,
        mailbox: row.get(2)?,
        consumer_generation: row.get(3)?,
        message_ids: parse_ids(row.get(4)?, 4)?,
        status: row.get(5)?,
        created_at: parse_timestamp(row.get(6)?, 6)?,
        acknowledged_at: parse_optional_timestamp(row.get(7)?, 7)?,
    })
}
