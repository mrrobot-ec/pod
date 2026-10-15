use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::ids::{DeliveryId, RunId, WakeId};
use crate::store::Tx;

use super::runs::{parse_id, parse_timestamp, timestamp};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WakeAttemptRow {
    pub id: WakeId,
    pub run_id: RunId,
    pub mailbox: String,
    pub delivery_id: Option<DeliveryId>,
    pub attempt: u32,
    pub outcome: String,
    pub detail: Option<String>,
    pub created_at: DateTime<Utc>,
}

const COLUMNS: &str = "id, run_id, mailbox, delivery_id, attempt, outcome, detail, created_at";

pub fn insert(tx: &Tx<'_>, row: &WakeAttemptRow) -> Result<()> {
    tx.0.execute("INSERT INTO wake_attempts (id, run_id, mailbox, delivery_id, attempt, outcome, detail, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)", params![row.id.as_str(), row.run_id.as_str(), row.mailbox, row.delivery_id.as_ref().map(DeliveryId::as_str), row.attempt, row.outcome, row.detail, timestamp(&row.created_at)])?;
    Ok(())
}
pub fn get(tx: &Tx<'_>, id: &WakeId) -> Result<Option<WakeAttemptRow>> {
    Ok(tx
        .0
        .query_row(
            &format!("SELECT {COLUMNS} FROM wake_attempts WHERE id = ?1"),
            [id.as_str()],
            from_row,
        )
        .optional()?)
}
pub fn list(tx: &Tx<'_>, limit: usize) -> Result<Vec<WakeAttemptRow>> {
    let mut statement = tx.0.prepare(&format!(
        "SELECT {COLUMNS} FROM wake_attempts ORDER BY created_at DESC, rowid DESC LIMIT ?1"
    ))?;
    Ok(statement
        .query_map([limit as i64], from_row)?
        .collect::<rusqlite::Result<_>>()?)
}

/// The attempts of `mailbox`'s current ladder (since its last consuming
/// check), oldest first.
pub fn list_since_last_consume(tx: &Tx<'_>, mailbox: &str) -> Result<Vec<WakeAttemptRow>> {
    let mut statement = tx.0.prepare(&format!(
        "SELECT {COLUMNS} FROM wake_attempts
         WHERE mailbox = ?1 AND created_at > COALESCE((
             SELECT at FROM events
             WHERE type = 'mailbox.consumed' AND json_extract(detail, '$.mailbox') = ?1
             ORDER BY at DESC, sequence DESC LIMIT 1
         ), '')
         ORDER BY created_at, rowid"
    ))?;
    Ok(statement
        .query_map([mailbox], from_row)?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn count_since_last_consume(tx: &Tx<'_>, mailbox: &str) -> Result<u32> {
    Ok(tx.0.query_row(
        "SELECT COUNT(*) FROM wake_attempts
         WHERE mailbox = ?1 AND created_at > COALESCE((
             SELECT at FROM events
             WHERE type = 'mailbox.consumed' AND json_extract(detail, '$.mailbox') = ?1
             ORDER BY at DESC, sequence DESC LIMIT 1
         ), '')",
        [mailbox],
        |row| row.get(0),
    )?)
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<WakeAttemptRow> {
    Ok(WakeAttemptRow {
        id: parse_id(row.get(0)?, 0)?,
        run_id: parse_id(row.get(1)?, 1)?,
        mailbox: row.get(2)?,
        delivery_id: row
            .get::<_, Option<String>>(3)?
            .map(|value| parse_id(value, 3))
            .transpose()?,
        attempt: row.get(4)?,
        outcome: row.get(5)?,
        detail: row.get(6)?,
        created_at: parse_timestamp(row.get(7)?, 7)?,
    })
}
