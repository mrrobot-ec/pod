use std::error::Error as StdError;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use rusqlite::types::{ToSql, Type};
use rusqlite::{OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{PodError, Result};
use crate::ids::RunId;
use crate::store::Tx;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RunRow {
    pub id: RunId,
    pub objective: String,
    pub coordinator_terminal_id: String,
    pub coordinator_handle: String,
    pub consumer_generation: u32,
    pub home_cwd: String,
    pub depth: u32,
    /// The Run's soft budget in USD (`run-create --budget-usd`); none by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_usd: Option<f64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub fn insert(tx: &Tx<'_>, row: &RunRow) -> Result<()> {
    tx.0.execute(
        "INSERT INTO runs (id, objective, coordinator_terminal_id, coordinator_handle, consumer_generation, home_cwd, depth, created_at, updated_at, budget_usd) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![row.id.as_str(), row.objective, row.coordinator_terminal_id, row.coordinator_handle, row.consumer_generation, row.home_cwd, row.depth, timestamp(&row.created_at), timestamp(&row.updated_at), row.budget_usd],
    )?;
    Ok(())
}

pub fn get(tx: &Tx<'_>, id: &RunId) -> Result<Option<RunRow>> {
    Ok(tx
        .0
        .query_row(
            "SELECT id, objective, coordinator_terminal_id, coordinator_handle, consumer_generation, home_cwd, depth, created_at, updated_at, budget_usd FROM runs WHERE id = ?1",
            [id.as_str()],
            from_row,
        )
        .optional()?)
}

pub fn list(tx: &Tx<'_>, limit: usize) -> Result<Vec<RunRow>> {
    let mut statement = tx.0.prepare(
        "SELECT id, objective, coordinator_terminal_id, coordinator_handle, consumer_generation, home_cwd, depth, created_at, updated_at, budget_usd FROM runs ORDER BY created_at DESC, rowid DESC LIMIT ?1",
    )?;
    Ok(statement
        .query_map([limit as i64], from_row)?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn update_objective(tx: &Tx<'_>, id: &RunId, value: &str) -> Result<()> {
    update_column(tx, "runs", "objective", "id", id.as_str(), value)
}

pub fn update_coordinator_terminal_id(tx: &Tx<'_>, id: &RunId, value: &str) -> Result<()> {
    update_column(
        tx,
        "runs",
        "coordinator_terminal_id",
        "id",
        id.as_str(),
        value,
    )
}

pub fn update_coordinator_handle(tx: &Tx<'_>, id: &RunId, value: &str) -> Result<()> {
    update_column(tx, "runs", "coordinator_handle", "id", id.as_str(), value)
}

pub fn update_consumer_generation(tx: &Tx<'_>, id: &RunId, value: u32) -> Result<()> {
    update_column(tx, "runs", "consumer_generation", "id", id.as_str(), &value)
}

pub fn update_home_cwd(tx: &Tx<'_>, id: &RunId, value: &str) -> Result<()> {
    update_column(tx, "runs", "home_cwd", "id", id.as_str(), value)
}

pub fn update_depth(tx: &Tx<'_>, id: &RunId, value: u32) -> Result<()> {
    update_column(tx, "runs", "depth", "id", id.as_str(), &value)
}

pub fn update_updated_at(tx: &Tx<'_>, id: &RunId, value: DateTime<Utc>) -> Result<()> {
    update_column(
        tx,
        "runs",
        "updated_at",
        "id",
        id.as_str(),
        &timestamp(&value),
    )
}

const COLUMNS: &str = "id, objective, coordinator_terminal_id, coordinator_handle, consumer_generation, home_cwd, depth, created_at, updated_at, budget_usd";

/// The Run `terminal` coordinates. When it has bound several, the most recent
/// `run-create` or `run-use` wins (`updated_at` is written only by those two).
pub fn bound_to_terminal(tx: &Tx<'_>, terminal: &str) -> Result<Option<RunRow>> {
    Ok(tx
        .0
        .query_row(
            &format!("SELECT {COLUMNS} FROM runs WHERE coordinator_terminal_id = ?1 ORDER BY updated_at DESC, rowid DESC LIMIT 1"),
            [terminal],
            from_row,
        )
        .optional()?)
}

/// Every Run, most recently bound first.
pub fn list_by_updated(tx: &Tx<'_>) -> Result<Vec<RunRow>> {
    let mut statement = tx.0.prepare(&format!(
        "SELECT {COLUMNS} FROM runs ORDER BY updated_at DESC, rowid DESC"
    ))?;
    Ok(statement
        .query_map([], from_row)?
        .collect::<rusqlite::Result<_>>()?)
}

/// Deletes one Run; its rows in every other table cascade with it.
pub fn delete(tx: &Tx<'_>, id: &RunId) -> Result<bool> {
    Ok(tx
        .0
        .execute("DELETE FROM runs WHERE id = ?1", [id.as_str()])?
        > 0)
}

/// Deletes every Run (and, by cascade, everything under them).
pub fn delete_all(tx: &Tx<'_>) -> Result<bool> {
    Ok(tx.0.execute("DELETE FROM runs", [])? > 0)
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<RunRow> {
    Ok(RunRow {
        id: parse_id(row.get(0)?, 0)?,
        objective: row.get(1)?,
        coordinator_terminal_id: row.get(2)?,
        coordinator_handle: row.get(3)?,
        consumer_generation: row.get(4)?,
        home_cwd: row.get(5)?,
        depth: row.get(6)?,
        created_at: parse_timestamp(row.get(7)?, 7)?,
        updated_at: parse_timestamp(row.get(8)?, 8)?,
        budget_usd: row.get(9)?,
    })
}

pub(crate) fn timestamp(value: &DateTime<Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

pub(crate) fn optional_timestamp(value: Option<&DateTime<Utc>>) -> Option<String> {
    value.map(timestamp)
}

pub(crate) fn json(value: &Value) -> String {
    value.to_string()
}

pub(crate) fn optional_json(value: Option<&Value>) -> Option<String> {
    value.map(json)
}

pub(crate) fn parse_id<T>(value: String, column: usize) -> rusqlite::Result<T>
where
    T: FromStr<Err = PodError>,
{
    value.parse().map_err(|error| conversion(column, error))
}

pub(crate) fn parse_timestamp(value: String, column: usize) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| conversion(column, error))
}

pub(crate) fn parse_optional_timestamp(
    value: Option<String>,
    column: usize,
) -> rusqlite::Result<Option<DateTime<Utc>>> {
    value
        .map(|value| parse_timestamp(value, column))
        .transpose()
}

pub(crate) fn parse_json(value: String, column: usize) -> rusqlite::Result<Value> {
    serde_json::from_str(&value).map_err(|error| conversion(column, error))
}

pub(crate) fn parse_optional_json(
    value: Option<String>,
    column: usize,
) -> rusqlite::Result<Option<Value>> {
    value.map(|value| parse_json(value, column)).transpose()
}

fn conversion(column: usize, error: impl StdError + Send + Sync + 'static) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(column, Type::Text, Box::new(error))
}

pub(crate) fn update_column<V: ToSql + ?Sized>(
    tx: &Tx<'_>,
    table: &str,
    column: &str,
    key_column: &str,
    key: &str,
    value: &V,
) -> Result<()> {
    tx.0.execute(
        &format!("UPDATE {table} SET {column} = ?1 WHERE {key_column} = ?2"),
        params![value, key],
    )?;
    Ok(())
}
