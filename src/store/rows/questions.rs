use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::ids::{DispatchId, MessageId, RunId};
use crate::store::Tx;

use super::runs::{
    optional_timestamp, parse_id, parse_optional_timestamp, parse_timestamp, timestamp,
    update_column,
};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct QuestionRow {
    pub message_id: MessageId,
    pub run_id: RunId,
    pub dispatch_id: Option<DispatchId>,
    pub asker_handle: String,
    pub status: String,
    pub answer_message_id: Option<MessageId>,
    pub answer_body: Option<String>,
    pub created_at: DateTime<Utc>,
    pub answered_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
}

const COLUMNS: &str = "message_id, run_id, dispatch_id, asker_handle, status, answer_message_id, answer_body, created_at, answered_at, closed_at";
pub fn insert(tx: &Tx<'_>, row: &QuestionRow) -> Result<()> {
    tx.0.execute("INSERT INTO questions (message_id, run_id, dispatch_id, asker_handle, status, answer_message_id, answer_body, created_at, answered_at, closed_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)", params![row.message_id.as_str(), row.run_id.as_str(), row.dispatch_id.as_ref().map(DispatchId::as_str), row.asker_handle, row.status, row.answer_message_id.as_ref().map(MessageId::as_str), row.answer_body, timestamp(&row.created_at), optional_timestamp(row.answered_at.as_ref()), optional_timestamp(row.closed_at.as_ref())])?;
    Ok(())
}
pub fn get(tx: &Tx<'_>, id: &MessageId) -> Result<Option<QuestionRow>> {
    Ok(tx
        .0
        .query_row(
            &format!("SELECT {COLUMNS} FROM questions WHERE message_id = ?1"),
            [id.as_str()],
            from_row,
        )
        .optional()?)
}
pub fn list(tx: &Tx<'_>, limit: usize) -> Result<Vec<QuestionRow>> {
    let mut statement = tx.0.prepare(&format!(
        "SELECT {COLUMNS} FROM questions ORDER BY created_at DESC, rowid DESC LIMIT ?1"
    ))?;
    Ok(statement
        .query_map([limit as i64], from_row)?
        .collect::<rusqlite::Result<_>>()?)
}
pub fn update_status(tx: &Tx<'_>, id: &MessageId, value: &str) -> Result<()> {
    update_column(tx, "questions", "status", "message_id", id.as_str(), value)
}
pub fn update_answer_message_id(
    tx: &Tx<'_>,
    id: &MessageId,
    value: Option<&MessageId>,
) -> Result<()> {
    update_column(
        tx,
        "questions",
        "answer_message_id",
        "message_id",
        id.as_str(),
        &value.map(MessageId::as_str),
    )
}
pub fn update_answer_body(tx: &Tx<'_>, id: &MessageId, value: Option<&str>) -> Result<()> {
    update_column(
        tx,
        "questions",
        "answer_body",
        "message_id",
        id.as_str(),
        &value,
    )
}
pub fn update_answered_at(tx: &Tx<'_>, id: &MessageId, value: Option<DateTime<Utc>>) -> Result<()> {
    update_column(
        tx,
        "questions",
        "answered_at",
        "message_id",
        id.as_str(),
        &optional_timestamp(value.as_ref()),
    )
}
pub fn update_closed_at(tx: &Tx<'_>, id: &MessageId, value: Option<DateTime<Utc>>) -> Result<()> {
    update_column(
        tx,
        "questions",
        "closed_at",
        "message_id",
        id.as_str(),
        &optional_timestamp(value.as_ref()),
    )
}
/// Closes every `pending` question of `dispatch`; returns how many.
pub fn close_pending_for_dispatch(
    tx: &Tx<'_>,
    dispatch: &DispatchId,
    now: DateTime<Utc>,
) -> Result<u32> {
    Ok(tx.0.execute(
        "UPDATE questions SET status = 'closed', closed_at = ?2 WHERE dispatch_id = ?1 AND status = 'pending'",
        params![dispatch.as_str(), timestamp(&now)],
    )? as u32)
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<QuestionRow> {
    Ok(QuestionRow {
        message_id: parse_id(row.get(0)?, 0)?,
        run_id: parse_id(row.get(1)?, 1)?,
        dispatch_id: row
            .get::<_, Option<String>>(2)?
            .map(|value| parse_id(value, 2))
            .transpose()?,
        asker_handle: row.get(3)?,
        status: row.get(4)?,
        answer_message_id: row
            .get::<_, Option<String>>(5)?
            .map(|value| parse_id(value, 5))
            .transpose()?,
        answer_body: row.get(6)?,
        created_at: parse_timestamp(row.get(7)?, 7)?,
        answered_at: parse_optional_timestamp(row.get(8)?, 8)?,
        closed_at: parse_optional_timestamp(row.get(9)?, 9)?,
    })
}
