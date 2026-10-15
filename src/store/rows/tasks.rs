use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;
use crate::ids::{RunId, TaskId};
use crate::store::Tx;

use super::runs::{
    optional_json, optional_timestamp, parse_id, parse_optional_json, parse_optional_timestamp,
    parse_timestamp, timestamp, update_column,
};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TaskRow {
    pub id: TaskId,
    pub run_id: RunId,
    pub parent_id: Option<TaskId>,
    pub title: Option<String>,
    pub display_name: Option<String>,
    pub spec: String,
    pub status: String,
    pub deps: Vec<TaskId>,
    pub result: Option<Value>,
    pub created_by_terminal_id: String,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

const COLUMNS: &str = "id, run_id, parent_id, title, display_name, spec, status, deps, result, created_by_terminal_id, created_at, completed_at";

pub fn insert(tx: &Tx<'_>, row: &TaskRow) -> Result<()> {
    tx.0.execute(
        "INSERT INTO tasks (id, run_id, parent_id, title, display_name, spec, status, deps, result, created_by_terminal_id, created_at, completed_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![row.id.as_str(), row.run_id.as_str(), row.parent_id.as_ref().map(TaskId::as_str), row.title, row.display_name, row.spec, row.status, deps_json(&row.deps), optional_json(row.result.as_ref()), row.created_by_terminal_id, timestamp(&row.created_at), optional_timestamp(row.completed_at.as_ref())],
    )?;
    Ok(())
}

pub fn get(tx: &Tx<'_>, id: &TaskId) -> Result<Option<TaskRow>> {
    Ok(tx.0.query_row(
        "SELECT id, run_id, parent_id, title, display_name, spec, status, deps, result, created_by_terminal_id, created_at, completed_at FROM tasks WHERE id = ?1",
        [id.as_str()], from_row).optional()?)
}

pub fn list(tx: &Tx<'_>, limit: usize) -> Result<Vec<TaskRow>> {
    let mut statement = tx.0.prepare("SELECT id, run_id, parent_id, title, display_name, spec, status, deps, result, created_by_terminal_id, created_at, completed_at FROM tasks ORDER BY created_at DESC, rowid DESC LIMIT ?1")?;
    Ok(statement
        .query_map([limit as i64], from_row)?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn list_by_run(tx: &Tx<'_>, run: &RunId, status: Option<&str>) -> Result<Vec<TaskRow>> {
    let mut statement = tx.0.prepare(
        "SELECT id, run_id, parent_id, title, display_name, spec, status, deps, result, created_by_terminal_id, created_at, completed_at FROM tasks WHERE run_id = ?1 AND (?2 IS NULL OR status = ?2) ORDER BY created_at ASC, rowid ASC",
    )?;
    Ok(statement
        .query_map(params![run.as_str(), status], from_row)?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn ready_dependants(tx: &Tx<'_>, completed_task: &TaskId) -> Result<Vec<TaskRow>> {
    let mut statement = tx.0.prepare(
        "SELECT t.id, t.run_id, t.parent_id, t.title, t.display_name, t.spec, t.status, t.deps, t.result, t.created_by_terminal_id, t.created_at, t.completed_at
         FROM tasks t
         WHERE t.status = 'pending'
           AND EXISTS (SELECT 1 FROM json_each(t.deps) dependency WHERE dependency.value = ?1)
           AND NOT EXISTS (
               SELECT 1 FROM json_each(t.deps) dependency
               LEFT JOIN tasks required ON required.id = dependency.value
               WHERE required.id IS NULL OR required.status <> 'completed'
           )
         ORDER BY t.created_at ASC, t.rowid ASC",
    )?;
    Ok(statement
        .query_map([completed_task.as_str()], from_row)?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn update_title(tx: &Tx<'_>, id: &TaskId, value: Option<&str>) -> Result<()> {
    update_column(tx, "tasks", "title", "id", id.as_str(), &value)
}

pub fn update_display_name(tx: &Tx<'_>, id: &TaskId, value: Option<&str>) -> Result<()> {
    update_column(tx, "tasks", "display_name", "id", id.as_str(), &value)
}

pub fn update_spec(tx: &Tx<'_>, id: &TaskId, value: &str) -> Result<()> {
    update_column(tx, "tasks", "spec", "id", id.as_str(), value)
}

pub fn update_status(tx: &Tx<'_>, id: &TaskId, value: &str) -> Result<()> {
    update_column(tx, "tasks", "status", "id", id.as_str(), value)
}

pub fn update_deps(tx: &Tx<'_>, id: &TaskId, value: &[TaskId]) -> Result<()> {
    update_column(tx, "tasks", "deps", "id", id.as_str(), &deps_json(value))
}

pub fn update_result(tx: &Tx<'_>, id: &TaskId, value: Option<&Value>) -> Result<()> {
    update_column(
        tx,
        "tasks",
        "result",
        "id",
        id.as_str(),
        &optional_json(value),
    )
}

pub fn update_completed_at(tx: &Tx<'_>, id: &TaskId, value: Option<DateTime<Utc>>) -> Result<()> {
    update_column(
        tx,
        "tasks",
        "completed_at",
        "id",
        id.as_str(),
        &optional_timestamp(value.as_ref()),
    )
}

/// Tasks of `run` whose `deps` name `dependency`, oldest first.
pub fn dependants(tx: &Tx<'_>, dependency: &TaskId) -> Result<Vec<TaskRow>> {
    let mut statement = tx.0.prepare(&format!(
        "SELECT {COLUMNS} FROM tasks t WHERE EXISTS (SELECT 1 FROM json_each(t.deps) d WHERE d.value = ?1) ORDER BY t.created_at ASC, t.rowid ASC"
    ))?;
    Ok(statement
        .query_map([dependency.as_str()], from_row)?
        .collect::<rusqlite::Result<_>>()?)
}

/// Task counts of `run` by status.
pub fn count_by_status(tx: &Tx<'_>, run: &RunId) -> Result<Vec<(String, u32)>> {
    let mut statement =
        tx.0.prepare("SELECT status, COUNT(*) FROM tasks WHERE run_id = ?1 GROUP BY status")?;
    Ok(statement
        .query_map([run.as_str()], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

fn deps_json(deps: &[TaskId]) -> String {
    Value::from(deps.iter().map(TaskId::as_str).collect::<Vec<_>>()).to_string()
}

fn parse_deps(value: String, column: usize) -> rusqlite::Result<Vec<TaskId>> {
    let ids: Vec<String> = serde_json::from_str(&value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })?;
    ids.into_iter().map(|id| parse_id(id, column)).collect()
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<TaskRow> {
    Ok(TaskRow {
        id: parse_id(row.get(0)?, 0)?,
        run_id: parse_id(row.get(1)?, 1)?,
        parent_id: row
            .get::<_, Option<String>>(2)?
            .map(|value| parse_id(value, 2))
            .transpose()?,
        title: row.get(3)?,
        display_name: row.get(4)?,
        spec: row.get(5)?,
        status: row.get(6)?,
        deps: parse_deps(row.get(7)?, 7)?,
        result: parse_optional_json(row.get(8)?, 8)?,
        created_by_terminal_id: row.get(9)?,
        created_at: parse_timestamp(row.get(10)?, 10)?,
        completed_at: parse_optional_timestamp(row.get(11)?, 11)?,
    })
}
