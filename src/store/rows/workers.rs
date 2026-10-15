use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;
use crate::ids::DispatchId;
use crate::store::Tx;

use super::runs::{
    optional_json, parse_id, parse_optional_json, parse_timestamp, timestamp, update_column,
};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WorkerRow {
    pub dispatch_id: DispatchId,
    pub state: String,
    pub stage: Option<String>,
    pub placement: Option<Value>,
    pub worktree_path: Option<String>,
    pub workspace_id: Option<String>,
    pub tab_id: Option<String>,
    pub pane_id: Option<String>,
    pub terminal_id: Option<String>,
    pub owns_pane: bool,
    pub user_owned: bool,
    pub user_owned_reason: Option<String>,
    pub setup_state: Option<String>,
    pub setup_pane_id: Option<String>,
    pub preamble_input: Option<String>,
    pub effects: Option<Value>,
    pub residual_resources: Option<Value>,
    pub terminal_state: String,
    pub retained_reason: Option<String>,
    pub session_id: Option<String>,
    pub transcript_path: Option<String>,
    pub transcript_proof: Option<String>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const COLUMNS: &str = "dispatch_id, state, stage, placement, worktree_path, workspace_id, tab_id, pane_id, terminal_id, owns_pane, user_owned, user_owned_reason, setup_state, setup_pane_id, preamble_input, effects, residual_resources, terminal_state, retained_reason, session_id, transcript_path, transcript_proof, last_error, created_at, updated_at";

pub fn insert(tx: &Tx<'_>, row: &WorkerRow) -> Result<()> {
    tx.0.execute(
        "INSERT INTO workers (dispatch_id, state, stage, placement, worktree_path, workspace_id, tab_id, pane_id, terminal_id, owns_pane, user_owned, user_owned_reason, setup_state, setup_pane_id, preamble_input, effects, residual_resources, terminal_state, retained_reason, session_id, transcript_path, transcript_proof, last_error, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25)",
        params![row.dispatch_id.as_str(), row.state, row.stage, optional_json(row.placement.as_ref()), row.worktree_path, row.workspace_id, row.tab_id, row.pane_id, row.terminal_id, row.owns_pane, row.user_owned, row.user_owned_reason, row.setup_state, row.setup_pane_id, row.preamble_input, optional_json(row.effects.as_ref()), optional_json(row.residual_resources.as_ref()), row.terminal_state, row.retained_reason, row.session_id, row.transcript_path, row.transcript_proof, row.last_error, timestamp(&row.created_at), timestamp(&row.updated_at)],
    )?;
    Ok(())
}

pub fn get(tx: &Tx<'_>, id: &DispatchId) -> Result<Option<WorkerRow>> {
    Ok(tx
        .0
        .query_row(
            &format!("SELECT {COLUMNS} FROM workers WHERE dispatch_id = ?1"),
            [id.as_str()],
            from_row,
        )
        .optional()?)
}

/// Whether any worker other than `except` still holds a pane in
/// `workspace_id`: active, retained, or finished but not yet released.
pub fn live_in_workspace(tx: &Tx<'_>, workspace_id: &str, except: &DispatchId) -> Result<bool> {
    let count: i64 = tx.0.query_row(
        "SELECT COUNT(*) FROM workers WHERE workspace_id = ?1 AND dispatch_id != ?2 AND terminal_state IN ('active','retained','reclaimable','release_pending','release_unknown')",
        [workspace_id, except.as_str()],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

pub fn list(tx: &Tx<'_>, limit: usize) -> Result<Vec<WorkerRow>> {
    let mut statement = tx.0.prepare(&format!(
        "SELECT {COLUMNS} FROM workers ORDER BY created_at DESC, rowid DESC LIMIT ?1"
    ))?;
    Ok(statement
        .query_map([limit as i64], from_row)?
        .collect::<rusqlite::Result<_>>()?)
}

macro_rules! string_setter {
    ($name:ident, $column:literal) => {
        pub fn $name(tx: &Tx<'_>, id: &DispatchId, value: Option<&str>) -> Result<()> {
            update_column(tx, "workers", $column, "dispatch_id", id.as_str(), &value)
        }
    };
}

pub fn update_state(tx: &Tx<'_>, id: &DispatchId, value: &str) -> Result<()> {
    update_column(tx, "workers", "state", "dispatch_id", id.as_str(), value)
}
string_setter!(update_stage, "stage");
pub fn update_placement(tx: &Tx<'_>, id: &DispatchId, value: Option<&Value>) -> Result<()> {
    update_column(
        tx,
        "workers",
        "placement",
        "dispatch_id",
        id.as_str(),
        &optional_json(value),
    )
}
string_setter!(update_worktree_path, "worktree_path");
string_setter!(update_workspace_id, "workspace_id");
string_setter!(update_tab_id, "tab_id");
string_setter!(update_pane_id, "pane_id");
string_setter!(update_terminal_id, "terminal_id");
pub fn update_owns_pane(tx: &Tx<'_>, id: &DispatchId, value: bool) -> Result<()> {
    update_column(
        tx,
        "workers",
        "owns_pane",
        "dispatch_id",
        id.as_str(),
        &value,
    )
}
pub fn update_user_owned(
    tx: &Tx<'_>,
    id: &DispatchId,
    value: bool,
    reason: Option<&str>,
) -> Result<()> {
    tx.0.execute(
        "UPDATE workers SET user_owned = ?1, user_owned_reason = ?2 WHERE dispatch_id = ?3",
        params![value, reason, id.as_str()],
    )?;
    Ok(())
}
string_setter!(update_setup_state, "setup_state");
string_setter!(update_setup_pane_id, "setup_pane_id");
string_setter!(update_preamble_input, "preamble_input");
pub fn update_effects(tx: &Tx<'_>, id: &DispatchId, value: Option<&Value>) -> Result<()> {
    update_column(
        tx,
        "workers",
        "effects",
        "dispatch_id",
        id.as_str(),
        &optional_json(value),
    )
}
pub fn update_residual_resources(
    tx: &Tx<'_>,
    id: &DispatchId,
    value: Option<&Value>,
) -> Result<()> {
    update_column(
        tx,
        "workers",
        "residual_resources",
        "dispatch_id",
        id.as_str(),
        &optional_json(value),
    )
}
pub fn update_terminal_state(tx: &Tx<'_>, id: &DispatchId, value: &str) -> Result<()> {
    update_column(
        tx,
        "workers",
        "terminal_state",
        "dispatch_id",
        id.as_str(),
        value,
    )
}
string_setter!(update_retained_reason, "retained_reason");
string_setter!(update_session_id, "session_id");
string_setter!(update_transcript_path, "transcript_path");
string_setter!(update_transcript_proof, "transcript_proof");
string_setter!(update_last_error, "last_error");
pub fn update_updated_at(tx: &Tx<'_>, id: &DispatchId, value: DateTime<Utc>) -> Result<()> {
    update_column(
        tx,
        "workers",
        "updated_at",
        "dispatch_id",
        id.as_str(),
        &timestamp(&value),
    )
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<WorkerRow> {
    Ok(WorkerRow {
        dispatch_id: parse_id(row.get(0)?, 0)?,
        state: row.get(1)?,
        stage: row.get(2)?,
        placement: parse_optional_json(row.get(3)?, 3)?,
        worktree_path: row.get(4)?,
        workspace_id: row.get(5)?,
        tab_id: row.get(6)?,
        pane_id: row.get(7)?,
        terminal_id: row.get(8)?,
        owns_pane: row.get(9)?,
        user_owned: row.get(10)?,
        user_owned_reason: row.get(11)?,
        setup_state: row.get(12)?,
        setup_pane_id: row.get(13)?,
        preamble_input: row.get(14)?,
        effects: parse_optional_json(row.get(15)?, 15)?,
        residual_resources: parse_optional_json(row.get(16)?, 16)?,
        terminal_state: row.get(17)?,
        retained_reason: row.get(18)?,
        session_id: row.get(19)?,
        transcript_path: row.get(20)?,
        transcript_proof: row.get(21)?,
        last_error: row.get(22)?,
        created_at: parse_timestamp(row.get(23)?, 23)?,
        updated_at: parse_timestamp(row.get(24)?, 24)?,
    })
}
