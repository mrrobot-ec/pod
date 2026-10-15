use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;
use crate::ids::{GateId, RunId, TaskId};
use crate::store::Tx;

use super::runs::{
    optional_timestamp, parse_id, parse_optional_timestamp, parse_timestamp, timestamp,
    update_column,
};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct GateRow {
    pub id: GateId,
    pub run_id: RunId,
    pub task_id: Option<TaskId>,
    pub audience: String,
    pub question: String,
    pub options: Vec<String>,
    pub evidence: Option<String>,
    pub recommendation: Option<String>,
    pub status: String,
    pub resolution: Option<String>,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

const COLUMNS: &str = "id, run_id, task_id, audience, question, options, evidence, recommendation, status, resolution, created_at, resolved_at";
pub fn insert(tx: &Tx<'_>, row: &GateRow) -> Result<()> {
    tx.0.execute("INSERT INTO gates (id, run_id, task_id, audience, question, options, evidence, recommendation, status, resolution, created_at, resolved_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)", params![row.id.as_str(), row.run_id.as_str(), row.task_id.as_ref().map(TaskId::as_str), row.audience, row.question, Value::from(row.options.clone()).to_string(), row.evidence, row.recommendation, row.status, row.resolution, timestamp(&row.created_at), optional_timestamp(row.resolved_at.as_ref())])?;
    Ok(())
}
pub fn get(tx: &Tx<'_>, id: &GateId) -> Result<Option<GateRow>> {
    Ok(tx
        .0
        .query_row(
            &format!("SELECT {COLUMNS} FROM gates WHERE id = ?1"),
            [id.as_str()],
            from_row,
        )
        .optional()?)
}
pub fn list(tx: &Tx<'_>, limit: usize) -> Result<Vec<GateRow>> {
    let mut statement = tx.0.prepare(&format!(
        "SELECT {COLUMNS} FROM gates ORDER BY created_at DESC, rowid DESC LIMIT ?1"
    ))?;
    Ok(statement
        .query_map([limit as i64], from_row)?
        .collect::<rusqlite::Result<_>>()?)
}
pub fn update_status(tx: &Tx<'_>, id: &GateId, value: &str) -> Result<()> {
    update_column(tx, "gates", "status", "id", id.as_str(), value)
}
pub fn update_resolution(tx: &Tx<'_>, id: &GateId, value: Option<&str>) -> Result<()> {
    update_column(tx, "gates", "resolution", "id", id.as_str(), &value)
}
pub fn update_resolved_at(tx: &Tx<'_>, id: &GateId, value: Option<DateTime<Utc>>) -> Result<()> {
    update_column(
        tx,
        "gates",
        "resolved_at",
        "id",
        id.as_str(),
        &optional_timestamp(value.as_ref()),
    )
}
/// Gates of `run`, optionally of one Task, newest first.
pub fn list_by_run(tx: &Tx<'_>, run: &RunId, task: Option<&TaskId>) -> Result<Vec<GateRow>> {
    let mut statement = tx.0.prepare(&format!(
        "SELECT {COLUMNS} FROM gates WHERE run_id = ?1 AND (?2 IS NULL OR task_id = ?2) ORDER BY created_at DESC, rowid DESC"
    ))?;
    Ok(statement
        .query_map(params![run.as_str(), task.map(TaskId::as_str)], from_row)?
        .collect::<rusqlite::Result<_>>()?)
}

/// Whether `task` has a gate still `pending`.
pub fn has_pending_for_task(tx: &Tx<'_>, task: &TaskId) -> Result<bool> {
    Ok(tx.0.query_row(
        "SELECT EXISTS(SELECT 1 FROM gates WHERE task_id = ?1 AND status = 'pending')",
        [task.as_str()],
        |row| row.get(0),
    )?)
}

/// Gate options are a JSON array of strings; NULL (or a legacy non-array
/// value) reads as no options.
fn parse_options(value: Option<String>) -> Vec<String> {
    value
        .and_then(|value| serde_json::from_str(&value).ok())
        .unwrap_or_default()
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<GateRow> {
    Ok(GateRow {
        id: parse_id(row.get(0)?, 0)?,
        run_id: parse_id(row.get(1)?, 1)?,
        task_id: row
            .get::<_, Option<String>>(2)?
            .map(|value| parse_id(value, 2))
            .transpose()?,
        audience: row.get(3)?,
        question: row.get(4)?,
        options: parse_options(row.get(5)?),
        evidence: row.get(6)?,
        recommendation: row.get(7)?,
        status: row.get(8)?,
        resolution: row.get(9)?,
        created_at: parse_timestamp(row.get(10)?, 10)?,
        resolved_at: parse_optional_timestamp(row.get(11)?, 11)?,
    })
}
