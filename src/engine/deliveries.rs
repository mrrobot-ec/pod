use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::engine::Engine;
use crate::error::{ErrorCode, PodError, Result};
use crate::ids::{DeliveryId, MessageId, RunId};
use crate::store::Tx;
use crate::store::rows::deliveries::{self, DeliveryRow};
use crate::store::rows::{dispatches, events, messages, runs};

pub use crate::store::rows::messages::MessageRow;

/// A Delivery with its messages. An empty consuming check is an internal
/// `empty` Delivery that is never stored; the CLI prints `delivery: null`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Delivery {
    pub id: DeliveryId,
    pub run_id: RunId,
    pub mailbox: String,
    pub consumer_generation: u32,
    pub message_ids: Vec<MessageId>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub acknowledged_at: Option<DateTime<Utc>>,
    pub messages: Vec<MessageRow>,
}

impl Delivery {
    fn from_row(row: DeliveryRow, messages: Vec<MessageRow>) -> Self {
        Self {
            id: row.id,
            run_id: row.run_id,
            mailbox: row.mailbox,
            consumer_generation: row.consumer_generation,
            message_ids: row.message_ids,
            status: row.status,
            created_at: row.created_at,
            acknowledged_at: row.acknowledged_at,
            messages,
        }
    }
}

pub fn check_consume(engine: &Engine, mailbox: &str) -> Result<Delivery> {
    let now = engine.clock.now();
    let consumer = consumer(engine);
    let delivery = engine
        .store
        .tx(|tx| check_consume_in(tx, now, mailbox, &consumer))?;
    engine.after_commit(&delivery.run_id, &[])?;
    Ok(delivery)
}

/// The consuming check inside a caller's transaction (for receipts): replays
/// the outstanding Delivery of the current generation, or fences an older one
/// and issues the next FIFO batch of up to 50.
pub fn check_consume_in(
    tx: &Tx<'_>,
    now: DateTime<Utc>,
    mailbox: &str,
    consumer: &str,
) -> Result<Delivery> {
    let run = run_for_mailbox(tx, mailbox)?;
    let generation = runs::get(tx, &run)?
        .ok_or_else(|| PodError::usage(format!("unknown Run {run}")))?
        .consumer_generation;
    if let Some(row) = deliveries::outstanding(tx, mailbox)? {
        if row.consumer_generation == generation {
            let rows = messages::get_many(tx, &row.message_ids)?;
            record_consumed(tx, now, &run, mailbox, Some(&row.id), consumer)?;
            return Ok(Delivery::from_row(row, rows));
        }
        deliveries::update_status(tx, &row.id, "fenced")?;
    }

    let rows = messages::select(tx, mailbox, &[], true, 50, false)?;
    let row = DeliveryRow {
        id: DeliveryId::new(),
        run_id: run.clone(),
        mailbox: mailbox.to_owned(),
        consumer_generation: generation,
        message_ids: rows.iter().map(|row| row.id.clone()).collect(),
        status: if rows.is_empty() {
            "empty"
        } else {
            "outstanding"
        }
        .to_owned(),
        created_at: now,
        acknowledged_at: None,
    };
    if !rows.is_empty() {
        deliveries::insert(tx, &row)?;
    }
    record_consumed(
        tx,
        now,
        &run,
        mailbox,
        (!rows.is_empty()).then_some(&row.id),
        consumer,
    )?;
    Ok(Delivery::from_row(row, rows))
}

pub fn check_ack(engine: &Engine, delivery: &DeliveryId) -> Result<Delivery> {
    let now = engine.clock.now();
    let delivery = engine.store.tx(|tx| check_ack_in(tx, now, delivery))?;
    engine.after_commit(&delivery.run_id, &[])?;
    Ok(delivery)
}

pub fn check_ack_in(tx: &Tx<'_>, now: DateTime<Utc>, delivery: &DeliveryId) -> Result<Delivery> {
    let mut row = deliveries::get(tx, delivery)?
        .ok_or_else(|| PodError::usage(format!("unknown delivery {delivery}")))?;
    if row.status == "outstanding" {
        deliveries::update_status(tx, delivery, "acknowledged")?;
        deliveries::update_acknowledged_at(tx, delivery, Some(now))?;
        for message in &row.message_ids {
            messages::mark_read(tx, message, now)?;
        }
        row.status = "acknowledged".into();
        row.acknowledged_at = Some(now);
    }
    let rows = messages::get_many(tx, &row.message_ids)?;
    Ok(Delivery::from_row(row, rows))
}

pub fn check_peek(engine: &Engine, mailbox: &str, types: &[String]) -> Result<Vec<MessageRow>> {
    engine.store.read(|tx| {
        run_for_mailbox(tx, mailbox)?;
        messages::select(tx, mailbox, types, true, 100, true)
    })
}

pub fn check_all(engine: &Engine, mailbox: &str, types: &[String]) -> Result<Vec<MessageRow>> {
    engine.store.read(|tx| {
        run_for_mailbox(tx, mailbox)?;
        messages::select(tx, mailbox, types, false, 100, true)
    })
}

/// The worker form of `check`: `terminal` is the worker handle the preamble
/// names. Only the pane that owns that handle's active Dispatch may consume it.
pub fn worker_check(engine: &Engine, terminal: &str) -> Result<Vec<MessageRow>> {
    let caller = engine.caller.as_ref().ok_or_else(|| {
        PodError::new(
            ErrorCode::NotInHerdr,
            "check --terminal marks messages read and needs a caller inside herdr",
            &["Run it from the worker's pane, or use pod inbox --terminal <handle> --run <id> to read without consuming."],
        )
    })?;
    let now = engine.clock.now();
    let rows = engine
        .store
        .tx(|tx| worker_check_in(tx, now, &caller.terminal_id, terminal))?;
    if let Some(row) = rows.first() {
        engine.after_commit(&row.run_id, &[])?;
    }
    Ok(rows)
}

/// The worker form inside a caller's transaction: marks the unread messages
/// of the caller's own Dispatch mailbox read and returns them.
pub fn worker_check_in(
    tx: &Tx<'_>,
    now: DateTime<Utc>,
    caller_terminal: &str,
    handle: &str,
) -> Result<Vec<MessageRow>> {
    let dispatch = dispatches::active_for_terminal(tx, caller_terminal)?
        .filter(|dispatch| {
            dispatch.status == "dispatched" && dispatch.assignee_handle.as_deref() == Some(handle)
        })
        .ok_or_else(|| {
            PodError::new(
                ErrorCode::ConsumerFenced,
                format!("no active Dispatch of worker {handle} belongs to this terminal"),
                &["Stop work. Do not send worker_done. Go idle and wait for a new brief."],
            )
        })?;
    let mailbox = format!("dispatch:{}", dispatch.id);
    let rows = messages::select(tx, &mailbox, &[], true, 100, false)?;
    for row in &rows {
        messages::mark_read(tx, &row.id, now)?;
    }
    record_consumed(tx, now, &dispatch.run_id, &mailbox, None, caller_terminal)?;
    Ok(rows)
}

/// The Run a `run:<id>` or `dispatch:<id>` mailbox belongs to. An unknown Run
/// or Dispatch is a usage error, as for any missing `--run`.
pub(crate) fn run_for_mailbox(tx: &Tx<'_>, mailbox: &str) -> Result<RunId> {
    if let Some(run) = mailbox.strip_prefix("run:") {
        let run: RunId = run.parse()?;
        if runs::get(tx, &run)?.is_none() {
            return Err(PodError::usage(format!("unknown Run {run}")));
        }
        return Ok(run);
    }
    if let Some(dispatch) = mailbox.strip_prefix("dispatch:") {
        let id = dispatch
            .parse()
            .map_err(|_| PodError::usage(format!("unknown Dispatch {dispatch}")))?;
        return dispatches::get(tx, &id)?
            .map(|row| row.run_id)
            .ok_or_else(|| PodError::usage(format!("unknown Dispatch {dispatch}")));
    }
    Err(PodError::new(
        ErrorCode::InvalidRecipient,
        format!("invalid mailbox {mailbox}"),
        &["Address a Run or Dispatch mailbox."],
    ))
}

pub(crate) fn consumer(engine: &Engine) -> String {
    engine
        .caller
        .as_ref()
        .map_or_else(|| "pod".to_owned(), |caller| caller.terminal_id.clone())
}

/// The event that clears a mailbox's wake ladder (T1 decision; read by
/// `wake_attempts::count_since_last_consume`).
fn record_consumed(
    tx: &Tx<'_>,
    now: DateTime<Utc>,
    run: &RunId,
    mailbox: &str,
    delivery: Option<&DeliveryId>,
    consumer: &str,
) -> Result<()> {
    events::append(
        tx,
        &events::EventRow {
            sequence: None,
            run_id: run.clone(),
            at: now,
            r#type: "mailbox.consumed".to_owned(),
            task_id: None,
            dispatch_id: mailbox
                .strip_prefix("dispatch:")
                .and_then(|id| id.parse().ok()),
            actor: consumer.to_owned(),
            detail: Some(
                json!({"mailbox": mailbox, "delivery_id": delivery, "consumer": consumer}),
            ),
        },
    )
}
