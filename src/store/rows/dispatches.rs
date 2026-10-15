use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;
use crate::ids::{DispatchId, RunId, TaskId};
use crate::store::Tx;

use super::runs::{
    optional_json, optional_timestamp, parse_id, parse_optional_json, parse_optional_timestamp,
    parse_timestamp, timestamp, update_column,
};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DispatchRow {
    pub id: DispatchId,
    pub run_id: RunId,
    pub task_id: TaskId,
    pub status: String,
    pub retry_of_dispatch_id: Option<DispatchId>,
    pub failure_count: u32,
    pub last_failure: Option<String>,
    pub termination_reason: Option<String>,
    pub termination_target: Option<String>,
    pub depth: u32,
    pub assignee_handle: Option<String>,
    pub assignee_terminal_id: Option<String>,
    pub assignee_pid: Option<u32>,
    pub agent_kind: Option<String>,
    pub launch_requested: Option<Value>,
    pub launch_effective: Option<Value>,
    pub dispatched_at: Option<DateTime<Utc>>,
    pub last_heartbeat_at: Option<DateTime<Utc>>,
    pub last_phase: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

const COLUMNS: &str = "id, run_id, task_id, status, retry_of_dispatch_id, failure_count, last_failure, termination_reason, termination_target, depth, assignee_handle, assignee_terminal_id, assignee_pid, agent_kind, launch_requested, launch_effective, dispatched_at, last_heartbeat_at, last_phase, completed_at, created_at";

pub fn insert(tx: &Tx<'_>, row: &DispatchRow) -> Result<()> {
    tx.0.execute(
        "INSERT INTO dispatches (id, run_id, task_id, status, retry_of_dispatch_id, failure_count, last_failure, termination_reason, termination_target, depth, assignee_handle, assignee_terminal_id, assignee_pid, agent_kind, launch_requested, launch_effective, dispatched_at, last_heartbeat_at, last_phase, completed_at, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)",
        params![row.id.as_str(), row.run_id.as_str(), row.task_id.as_str(), row.status, row.retry_of_dispatch_id.as_ref().map(DispatchId::as_str), row.failure_count, row.last_failure, row.termination_reason, row.termination_target, row.depth, row.assignee_handle, row.assignee_terminal_id, row.assignee_pid, row.agent_kind, optional_json(row.launch_requested.as_ref()), optional_json(row.launch_effective.as_ref()), optional_timestamp(row.dispatched_at.as_ref()), optional_timestamp(row.last_heartbeat_at.as_ref()), row.last_phase, optional_timestamp(row.completed_at.as_ref()), timestamp(&row.created_at)],
    )?;
    Ok(())
}

pub fn get(tx: &Tx<'_>, id: &DispatchId) -> Result<Option<DispatchRow>> {
    Ok(tx
        .0
        .query_row(
            &format!("SELECT {COLUMNS} FROM dispatches WHERE id = ?1"),
            [id.as_str()],
            from_row,
        )
        .optional()?)
}

pub fn list(tx: &Tx<'_>, limit: usize) -> Result<Vec<DispatchRow>> {
    let mut statement = tx.0.prepare(&format!(
        "SELECT {COLUMNS} FROM dispatches ORDER BY created_at DESC, rowid DESC LIMIT ?1"
    ))?;
    Ok(statement
        .query_map([limit as i64], from_row)?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn active_for_task(tx: &Tx<'_>, task: &TaskId) -> Result<Option<DispatchRow>> {
    Ok(tx.0.query_row(&format!("SELECT {COLUMNS} FROM dispatches WHERE task_id = ?1 AND status IN ('pending','dispatched')"), [task.as_str()], from_row).optional()?)
}

pub fn active_for_terminal(tx: &Tx<'_>, terminal: &str) -> Result<Option<DispatchRow>> {
    Ok(tx.0.query_row(&format!("SELECT {COLUMNS} FROM dispatches WHERE assignee_terminal_id = ?1 AND status IN ('pending','dispatched')"), [terminal], from_row).optional()?)
}

pub fn update_status(tx: &Tx<'_>, id: &DispatchId, value: &str) -> Result<()> {
    update_column(tx, "dispatches", "status", "id", id.as_str(), value)
}
pub fn update_failure_count(tx: &Tx<'_>, id: &DispatchId, value: u32) -> Result<()> {
    update_column(tx, "dispatches", "failure_count", "id", id.as_str(), &value)
}
pub fn update_last_failure(tx: &Tx<'_>, id: &DispatchId, value: Option<&str>) -> Result<()> {
    update_column(tx, "dispatches", "last_failure", "id", id.as_str(), &value)
}
pub fn update_termination_reason(tx: &Tx<'_>, id: &DispatchId, value: Option<&str>) -> Result<()> {
    update_column(
        tx,
        "dispatches",
        "termination_reason",
        "id",
        id.as_str(),
        &value,
    )
}
pub fn update_termination_target(tx: &Tx<'_>, id: &DispatchId, value: Option<&str>) -> Result<()> {
    update_column(
        tx,
        "dispatches",
        "termination_target",
        "id",
        id.as_str(),
        &value,
    )
}
pub fn update_assignee_handle(tx: &Tx<'_>, id: &DispatchId, value: Option<&str>) -> Result<()> {
    update_column(
        tx,
        "dispatches",
        "assignee_handle",
        "id",
        id.as_str(),
        &value,
    )
}
pub fn update_assignee_terminal_id(
    tx: &Tx<'_>,
    id: &DispatchId,
    value: Option<&str>,
) -> Result<()> {
    update_column(
        tx,
        "dispatches",
        "assignee_terminal_id",
        "id",
        id.as_str(),
        &value,
    )
}
pub fn update_assignee_pid(tx: &Tx<'_>, id: &DispatchId, value: Option<u32>) -> Result<()> {
    update_column(tx, "dispatches", "assignee_pid", "id", id.as_str(), &value)
}
pub fn update_agent_kind(tx: &Tx<'_>, id: &DispatchId, value: Option<&str>) -> Result<()> {
    update_column(tx, "dispatches", "agent_kind", "id", id.as_str(), &value)
}
pub fn update_launch_requested(tx: &Tx<'_>, id: &DispatchId, value: Option<&Value>) -> Result<()> {
    update_column(
        tx,
        "dispatches",
        "launch_requested",
        "id",
        id.as_str(),
        &optional_json(value),
    )
}
pub fn update_launch_effective(tx: &Tx<'_>, id: &DispatchId, value: Option<&Value>) -> Result<()> {
    update_column(
        tx,
        "dispatches",
        "launch_effective",
        "id",
        id.as_str(),
        &optional_json(value),
    )
}
pub fn update_dispatched_at(
    tx: &Tx<'_>,
    id: &DispatchId,
    value: Option<DateTime<Utc>>,
) -> Result<()> {
    update_column(
        tx,
        "dispatches",
        "dispatched_at",
        "id",
        id.as_str(),
        &optional_timestamp(value.as_ref()),
    )
}
pub fn update_last_heartbeat_at(
    tx: &Tx<'_>,
    id: &DispatchId,
    value: Option<DateTime<Utc>>,
) -> Result<()> {
    update_column(
        tx,
        "dispatches",
        "last_heartbeat_at",
        "id",
        id.as_str(),
        &optional_timestamp(value.as_ref()),
    )
}
pub fn update_last_phase(tx: &Tx<'_>, id: &DispatchId, value: Option<&str>) -> Result<()> {
    update_column(tx, "dispatches", "last_phase", "id", id.as_str(), &value)
}
pub fn update_completed_at(
    tx: &Tx<'_>,
    id: &DispatchId,
    value: Option<DateTime<Utc>>,
) -> Result<()> {
    update_column(
        tx,
        "dispatches",
        "completed_at",
        "id",
        id.as_str(),
        &optional_timestamp(value.as_ref()),
    )
}

/// Ends an active Dispatch without replacing a previously accepted ending.
pub fn fence(
    tx: &Tx<'_>,
    id: &DispatchId,
    status: &str,
    reason: &str,
    completed_at: DateTime<Utc>,
) -> Result<bool> {
    Ok(tx.0.execute(
        "UPDATE dispatches SET status = ?1, termination_reason = COALESCE(termination_reason, ?2), completed_at = COALESCE(completed_at, ?3) WHERE id = ?4 AND status IN ('pending','dispatched')",
        params![status, reason, timestamp(&completed_at), id.as_str()],
    )? == 1)
}

/// The Dispatches of `task`, oldest first.
pub fn list_by_task(tx: &Tx<'_>, task: &TaskId) -> Result<Vec<DispatchRow>> {
    let mut statement = tx.0.prepare(&format!(
        "SELECT {COLUMNS} FROM dispatches WHERE task_id = ?1 ORDER BY created_at ASC, rowid ASC"
    ))?;
    Ok(statement
        .query_map([task.as_str()], from_row)?
        .collect::<rusqlite::Result<_>>()?)
}

/// The Dispatches of `run`, oldest first.
pub fn list_by_run(tx: &Tx<'_>, run: &RunId) -> Result<Vec<DispatchRow>> {
    let mut statement = tx.0.prepare(&format!(
        "SELECT {COLUMNS} FROM dispatches WHERE run_id = ?1 ORDER BY created_at ASC, rowid ASC"
    ))?;
    Ok(statement
        .query_map([run.as_str()], from_row)?
        .collect::<rusqlite::Result<_>>()?)
}

/// The newest Dispatch ever assigned to `terminal`, in any status.
pub fn latest_for_terminal(tx: &Tx<'_>, terminal: &str) -> Result<Option<DispatchRow>> {
    Ok(tx.0.query_row(&format!("SELECT {COLUMNS} FROM dispatches WHERE assignee_terminal_id = ?1 ORDER BY created_at DESC, rowid DESC LIMIT 1"), [terminal], from_row).optional()?)
}

/// The newest `dispatched` Dispatch whose worker handle is `handle`, within
/// `run` when given.
pub fn dispatched_by_handle(
    tx: &Tx<'_>,
    run: Option<&RunId>,
    handle: &str,
) -> Result<Option<DispatchRow>> {
    Ok(tx.0.query_row(&format!("SELECT {COLUMNS} FROM dispatches WHERE (?1 IS NULL OR run_id = ?1) AND assignee_handle = ?2 AND status = 'dispatched' ORDER BY created_at DESC, rowid DESC LIMIT 1"), params![run.map(RunId::as_str), handle], from_row).optional()?)
}

/// The cumulative failure count of `task`: the highest count any of its
/// Dispatches carries (spec 7.5; a retry Dispatch inherits it).
pub fn max_failure_count_for_task(tx: &Tx<'_>, task: &TaskId) -> Result<u32> {
    Ok(tx.0.query_row(
        "SELECT COALESCE(MAX(failure_count), 0) FROM dispatches WHERE task_id = ?1",
        [task.as_str()],
        |row| row.get(0),
    )?)
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<DispatchRow> {
    Ok(DispatchRow {
        id: parse_id(row.get(0)?, 0)?,
        run_id: parse_id(row.get(1)?, 1)?,
        task_id: parse_id(row.get(2)?, 2)?,
        status: row.get(3)?,
        retry_of_dispatch_id: row
            .get::<_, Option<String>>(4)?
            .map(|value| parse_id(value, 4))
            .transpose()?,
        failure_count: row.get(5)?,
        last_failure: row.get(6)?,
        termination_reason: row.get(7)?,
        termination_target: row.get(8)?,
        depth: row.get(9)?,
        assignee_handle: row.get(10)?,
        assignee_terminal_id: row.get(11)?,
        assignee_pid: row.get(12)?,
        agent_kind: row.get(13)?,
        launch_requested: parse_optional_json(row.get(14)?, 14)?,
        launch_effective: parse_optional_json(row.get(15)?, 15)?,
        dispatched_at: parse_optional_timestamp(row.get(16)?, 16)?,
        last_heartbeat_at: parse_optional_timestamp(row.get(17)?, 17)?,
        last_phase: row.get(18)?,
        completed_at: parse_optional_timestamp(row.get(19)?, 19)?,
        created_at: parse_timestamp(row.get(20)?, 20)?,
    })
}
