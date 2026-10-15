use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::ids::DispatchId;
use crate::store::Tx;

use super::runs::{parse_id, parse_timestamp, timestamp};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WorkerArchiveRow {
    pub dispatch_id: DispatchId,
    pub kind: String,
    pub content: String,
    pub created_at: DateTime<Utc>,
}

pub fn insert(tx: &Tx<'_>, row: &WorkerArchiveRow) -> Result<()> {
    tx.0.execute(
        "INSERT INTO worker_archives (dispatch_id, kind, content, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![row.dispatch_id.as_str(), row.kind, row.content, timestamp(&row.created_at)],
    )?;
    Ok(())
}

pub fn get(tx: &Tx<'_>, dispatch: &DispatchId, kind: &str) -> Result<Option<WorkerArchiveRow>> {
    Ok(tx.0.query_row(
        "SELECT dispatch_id, kind, content, created_at FROM worker_archives WHERE dispatch_id = ?1 AND kind = ?2",
        params![dispatch.as_str(), kind], from_row).optional()?)
}

pub fn list(tx: &Tx<'_>, limit: usize) -> Result<Vec<WorkerArchiveRow>> {
    let mut statement = tx.0.prepare("SELECT dispatch_id, kind, content, created_at FROM worker_archives ORDER BY created_at DESC, rowid DESC LIMIT ?1")?;
    Ok(statement
        .query_map([limit as i64], from_row)?
        .collect::<rusqlite::Result<_>>()?)
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<WorkerArchiveRow> {
    Ok(WorkerArchiveRow {
        dispatch_id: parse_id(row.get(0)?, 0)?,
        kind: row.get(1)?,
        content: row.get(2)?,
        created_at: parse_timestamp(row.get(3)?, 3)?,
    })
}
