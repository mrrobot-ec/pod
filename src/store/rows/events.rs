//! A consuming check appends `type = "mailbox.consumed"` with detail
//! `{"mailbox":"<run:id or dispatch:id>","delivery_id":"<id or null>","consumer":"<terminal id>"}`.

use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;
use crate::ids::{DispatchId, RunId, TaskId};
use crate::store::Tx;

use super::runs::{optional_json, parse_id, parse_optional_json, parse_timestamp, timestamp};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct EventRow {
    pub sequence: Option<i64>,
    pub run_id: RunId,
    pub at: DateTime<Utc>,
    pub r#type: String,
    pub task_id: Option<TaskId>,
    pub dispatch_id: Option<DispatchId>,
    pub actor: String,
    pub detail: Option<Value>,
}

const COLUMNS: &str = "sequence, run_id, at, type, task_id, dispatch_id, actor, detail";
pub fn insert(tx: &Tx<'_>, row: &EventRow) -> Result<()> {
    append(tx, row)
}
pub fn append(tx: &Tx<'_>, row: &EventRow) -> Result<()> {
    tx.0.execute("INSERT INTO events (run_id, at, type, task_id, dispatch_id, actor, detail) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)", params![row.run_id.as_str(), timestamp(&row.at), row.r#type, row.task_id.as_ref().map(TaskId::as_str), row.dispatch_id.as_ref().map(DispatchId::as_str), row.actor, optional_json(row.detail.as_ref())])?;
    Ok(())
}
pub fn get(tx: &Tx<'_>, sequence: i64) -> Result<Option<EventRow>> {
    Ok(tx
        .0
        .query_row(
            &format!("SELECT {COLUMNS} FROM events WHERE sequence = ?1"),
            [sequence],
            from_row,
        )
        .optional()?)
}
pub fn list(tx: &Tx<'_>, run: &RunId) -> Result<Vec<EventRow>> {
    let mut statement = tx.0.prepare(&format!(
        "SELECT {COLUMNS} FROM events WHERE run_id = ?1 ORDER BY sequence ASC"
    ))?;
    Ok(statement
        .query_map([run.as_str()], from_row)?
        .collect::<rusqlite::Result<_>>()?)
}
fn from_row(row: &Row<'_>) -> rusqlite::Result<EventRow> {
    Ok(EventRow {
        sequence: row.get(0)?,
        run_id: parse_id(row.get(1)?, 1)?,
        at: parse_timestamp(row.get(2)?, 2)?,
        r#type: row.get(3)?,
        task_id: row
            .get::<_, Option<String>>(4)?
            .map(|value| parse_id(value, 4))
            .transpose()?,
        dispatch_id: row
            .get::<_, Option<String>>(5)?
            .map(|value| parse_id(value, 5))
            .transpose()?,
        actor: row.get(6)?,
        detail: parse_optional_json(row.get(7)?, 7)?,
    })
}
