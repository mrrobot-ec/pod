use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;
use crate::ids::DispatchId;
use crate::store::Tx;

use super::runs::{
    json, optional_json, parse_id, parse_json, parse_optional_json, parse_timestamp, timestamp,
};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ObservationRow {
    pub dispatch_id: DispatchId,
    pub liveness: String,
    pub activity: String,
    pub presence: Option<String>,
    pub needs_input: Value,
    pub evidence_source: String,
    pub reason: Option<String>,
    pub herdr_status: Option<String>,
    pub attention: Option<Value>,
    pub observed_at: DateTime<Utc>,
}

pub fn insert(tx: &Tx<'_>, row: &ObservationRow) -> Result<()> {
    tx.0.execute(
        "INSERT INTO observations (dispatch_id, liveness, activity, presence, needs_input, evidence_source, reason, herdr_status, attention, observed_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![row.dispatch_id.as_str(), row.liveness, row.activity, row.presence, json(&row.needs_input), row.evidence_source, row.reason, row.herdr_status, optional_json(row.attention.as_ref()), timestamp(&row.observed_at)],
    )?;
    Ok(())
}

pub fn get(tx: &Tx<'_>, dispatch: &DispatchId) -> Result<Option<ObservationRow>> {
    Ok(tx.0.query_row(
        "SELECT dispatch_id, liveness, activity, presence, needs_input, evidence_source, reason, herdr_status, attention, observed_at FROM observations WHERE dispatch_id = ?1 ORDER BY observed_at DESC, rowid DESC LIMIT 1",
        [dispatch.as_str()], from_row).optional()?)
}

pub fn list(tx: &Tx<'_>, limit: usize) -> Result<Vec<ObservationRow>> {
    let mut statement = tx.0.prepare("SELECT dispatch_id, liveness, activity, presence, needs_input, evidence_source, reason, herdr_status, attention, observed_at FROM observations ORDER BY observed_at DESC, rowid DESC LIMIT ?1")?;
    Ok(statement
        .query_map([limit as i64], from_row)?
        .collect::<rusqlite::Result<_>>()?)
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<ObservationRow> {
    Ok(ObservationRow {
        dispatch_id: parse_id(row.get(0)?, 0)?,
        liveness: row.get(1)?,
        activity: row.get(2)?,
        presence: row.get(3)?,
        needs_input: parse_json(row.get(4)?, 4)?,
        evidence_source: row.get(5)?,
        reason: row.get(6)?,
        herdr_status: row.get(7)?,
        attention: parse_optional_json(row.get(8)?, 8)?,
        observed_at: parse_timestamp(row.get(9)?, 9)?,
    })
}
