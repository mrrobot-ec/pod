use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;
use crate::ids::{MessageId, RunId};
use crate::store::Tx;

use super::runs::{
    optional_json, optional_timestamp, parse_id, parse_optional_json, parse_optional_timestamp,
    parse_timestamp, timestamp, update_column,
};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct MessageRow {
    pub sequence: Option<i64>,
    pub id: MessageId,
    pub run_id: RunId,
    pub mailbox: String,
    pub from_handle: String,
    pub subject: String,
    pub body: Option<String>,
    pub r#type: String,
    pub priority: String,
    pub thread_id: Option<String>,
    pub payload: Option<Value>,
    pub read: bool,
    pub created_at: DateTime<Utc>,
    pub delivered_at: Option<DateTime<Utc>>,
    pub sender_terminal_id: Option<String>,
    pub rejected_reason: Option<String>,
    pub evidence: Option<String>,
    pub recommendation: Option<String>,
}

const COLUMNS: &str = "sequence, id, run_id, mailbox, from_handle, subject, body, type, priority, thread_id, payload, read, created_at, delivered_at, sender_terminal_id, rejected_reason, evidence, recommendation";

/// Inserts the row and returns its `sequence`.
pub fn insert(tx: &Tx<'_>, row: &MessageRow) -> Result<i64> {
    tx.0.execute(
        "INSERT INTO messages (id, run_id, mailbox, from_handle, subject, body, type, priority, thread_id, payload, read, created_at, delivered_at, sender_terminal_id, rejected_reason, evidence, recommendation) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        params![row.id.as_str(), row.run_id.as_str(), row.mailbox, row.from_handle, row.subject, row.body, row.r#type, row.priority, row.thread_id, optional_json(row.payload.as_ref()), row.read, timestamp(&row.created_at), optional_timestamp(row.delivered_at.as_ref()), row.sender_terminal_id, row.rejected_reason, row.evidence, row.recommendation],
    )?;
    Ok(tx.0.last_insert_rowid())
}

pub fn get(tx: &Tx<'_>, id: &MessageId) -> Result<Option<MessageRow>> {
    Ok(tx
        .0
        .query_row(
            &format!("SELECT {COLUMNS} FROM messages WHERE id = ?1"),
            [id.as_str()],
            from_row,
        )
        .optional()?)
}

pub fn list(tx: &Tx<'_>, limit: usize) -> Result<Vec<MessageRow>> {
    let mut statement = tx.0.prepare(&format!(
        "SELECT {COLUMNS} FROM messages ORDER BY created_at DESC, rowid DESC LIMIT ?1"
    ))?;
    Ok(statement
        .query_map([limit as i64], from_row)?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn unread_for_mailbox(tx: &Tx<'_>, mailbox: &str, limit: usize) -> Result<Vec<MessageRow>> {
    let mut statement = tx.0.prepare(&format!("SELECT {COLUMNS} FROM messages WHERE mailbox = ?1 AND read = 0 ORDER BY sequence ASC LIMIT ?2"))?;
    Ok(statement
        .query_map(params![mailbox, limit as i64], from_row)?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn history(tx: &Tx<'_>, mailbox: &str, limit: usize) -> Result<Vec<MessageRow>> {
    let mut statement = tx.0.prepare(&format!(
        "SELECT {COLUMNS} FROM messages WHERE mailbox = ?1 ORDER BY sequence DESC LIMIT ?2"
    ))?;
    Ok(statement
        .query_map(params![mailbox, limit as i64], from_row)?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn update_read(tx: &Tx<'_>, id: &MessageId, value: bool) -> Result<()> {
    update_column(tx, "messages", "read", "id", id.as_str(), &value)
}
pub fn update_delivered_at(
    tx: &Tx<'_>,
    id: &MessageId,
    value: Option<DateTime<Utc>>,
) -> Result<()> {
    update_column(
        tx,
        "messages",
        "delivered_at",
        "id",
        id.as_str(),
        &optional_timestamp(value.as_ref()),
    )
}
pub fn update_type(tx: &Tx<'_>, id: &MessageId, value: &str) -> Result<()> {
    update_column(tx, "messages", "type", "id", id.as_str(), value)
}
pub fn update_priority(tx: &Tx<'_>, id: &MessageId, value: &str) -> Result<()> {
    update_column(tx, "messages", "priority", "id", id.as_str(), value)
}
pub fn update_rejected_reason(tx: &Tx<'_>, id: &MessageId, value: Option<&str>) -> Result<()> {
    update_column(tx, "messages", "rejected_reason", "id", id.as_str(), &value)
}

/// Messages of `mailbox`, optionally only unread ones and only the given
/// types, by `sequence` (oldest first unless `newest_first`).
pub fn select(
    tx: &Tx<'_>,
    mailbox: &str,
    types: &[String],
    unread_only: bool,
    limit: usize,
    newest_first: bool,
) -> Result<Vec<MessageRow>> {
    let mut query = format!("SELECT {COLUMNS} FROM messages WHERE mailbox = ?");
    if unread_only {
        query.push_str(" AND read = 0");
    }
    if !types.is_empty() {
        query.push_str(" AND type IN (");
        query.push_str(&vec!["?"; types.len()].join(","));
        query.push(')');
    }
    query.push_str(if newest_first {
        " ORDER BY sequence DESC LIMIT ?"
    } else {
        " ORDER BY sequence ASC LIMIT ?"
    });
    let mut values = Vec::<rusqlite::types::Value>::with_capacity(types.len() + 2);
    values.push(mailbox.to_owned().into());
    values.extend(types.iter().cloned().map(Into::into));
    values.push((limit as i64).into());
    let mut statement = tx.0.prepare(&query)?;
    Ok(statement
        .query_map(rusqlite::params_from_iter(values), from_row)?
        .collect::<rusqlite::Result<_>>()?)
}

/// The rows with these IDs, by `sequence`.
pub fn get_many(tx: &Tx<'_>, ids: &[MessageId]) -> Result<Vec<MessageRow>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let query = format!(
        "SELECT {COLUMNS} FROM messages WHERE id IN ({}) ORDER BY sequence ASC",
        vec!["?"; ids.len()].join(",")
    );
    let mut statement = tx.0.prepare(&query)?;
    Ok(statement
        .query_map(
            rusqlite::params_from_iter(ids.iter().map(MessageId::as_str)),
            from_row,
        )?
        .collect::<rusqlite::Result<_>>()?)
}

/// Marks a message read, keeping the first `delivered_at`.
pub fn mark_read(tx: &Tx<'_>, id: &MessageId, now: DateTime<Utc>) -> Result<()> {
    tx.0.execute(
        "UPDATE messages SET read = 1, delivered_at = COALESCE(delivered_at, ?2) WHERE id = ?1",
        params![id.as_str(), timestamp(&now)],
    )?;
    Ok(())
}

/// The first accepted `worker_done` whose payload names `dispatch`.
pub fn first_worker_done(tx: &Tx<'_>, dispatch: &str) -> Result<Option<MessageRow>> {
    Ok(tx
        .0
        .query_row(
            &format!("SELECT {COLUMNS} FROM messages WHERE type = 'worker_done' AND rejected_reason IS NULL AND json_extract(payload, '$.dispatch_id') = ?1 ORDER BY sequence ASC LIMIT 1"),
            [dispatch],
            from_row,
        )
        .optional()?)
}

/// The messages that share `thread`, oldest first.
pub fn in_thread(tx: &Tx<'_>, thread: &str) -> Result<Vec<MessageRow>> {
    let mut statement = tx.0.prepare(&format!(
        "SELECT {COLUMNS} FROM messages WHERE thread_id = ?1 ORDER BY sequence ASC"
    ))?;
    Ok(statement
        .query_map([thread], from_row)?
        .collect::<rusqlite::Result<_>>()?)
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<MessageRow> {
    Ok(MessageRow {
        sequence: row.get(0)?,
        id: parse_id(row.get(1)?, 1)?,
        run_id: parse_id(row.get(2)?, 2)?,
        mailbox: row.get(3)?,
        from_handle: row.get(4)?,
        subject: row.get(5)?,
        body: row.get(6)?,
        r#type: row.get(7)?,
        priority: row.get(8)?,
        thread_id: row.get(9)?,
        payload: parse_optional_json(row.get(10)?, 10)?,
        read: row.get(11)?,
        created_at: parse_timestamp(row.get(12)?, 12)?,
        delivered_at: parse_optional_timestamp(row.get(13)?, 13)?,
        sender_terminal_id: row.get(14)?,
        rejected_reason: row.get(15)?,
        evidence: row.get(16)?,
        recommendation: row.get(17)?,
    })
}
