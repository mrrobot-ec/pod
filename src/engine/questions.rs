use chrono::{DateTime, Utc};
use serde_json::json;

use crate::engine::Engine;
use crate::error::{ErrorCode, PodError, Result};
use crate::ids::{DispatchId, MessageId, RunId, new_id};
use crate::store::Tx;
use crate::store::rows::{dispatches, messages, questions, runs, workers};

pub use crate::store::rows::dispatches::DispatchRow;
pub use crate::store::rows::messages::MessageRow;
pub use crate::store::rows::questions::QuestionRow;

pub fn ask_store(
    engine: &Engine,
    question: &str,
    options: &[String],
    to: Option<&str>,
) -> Result<(MessageId, Option<MessageId>)> {
    let caller = engine.caller.as_ref().ok_or_else(not_in_herdr)?;
    let now = engine.clock.now();
    let (run, ids, mailboxes) = engine.store.tx(|tx| {
        let ids = ask_store_in(tx, now, &caller.terminal_id, question, options, to)?;
        let run = messages::get(tx, &ids.0)?.map(|row| row.run_id);
        let mut mailboxes = vec![messages::get(tx, &ids.0)?.map(|row| row.mailbox)];
        mailboxes.push(
            ids.1
                .as_ref()
                .map(|id| messages::get(tx, id))
                .transpose()?
                .flatten()
                .map(|row| row.mailbox),
        );
        Ok((
            run,
            ids,
            mailboxes.into_iter().flatten().collect::<Vec<_>>(),
        ))
    })?;
    if let Some(run) = run {
        engine.after_commit(&run, &mailboxes)?;
    }
    Ok(ids)
}

/// Stores a question from the caller's active Dispatch: one `questions` row,
/// its Run mailbox message, and with `to` a specialist copy in the same thread.
pub fn ask_store_in(
    tx: &Tx<'_>,
    now: DateTime<Utc>,
    terminal: &str,
    question: &str,
    options: &[String],
    to: Option<&str>,
) -> Result<(MessageId, Option<MessageId>)> {
    let asker = dispatches::active_for_terminal(tx, terminal)?
        .filter(|dispatch| dispatch.status == "dispatched")
        .ok_or_else(|| {
            PodError::new(
                ErrorCode::CallerFenced,
                "the caller has no active Dispatch",
                &["Stop work and ask the coordinator for a current Dispatch."],
            )
        })?;
    let target = match to {
        Some(value) => {
            let id = value.strip_prefix("dispatch:").unwrap_or(value);
            let target = id
                .parse::<DispatchId>()
                .ok()
                .map(|id| live_dispatch(tx, &id))
                .transpose()?
                .flatten()
                .filter(|target| target.run_id == asker.run_id && target.id != asker.id)
                .ok_or_else(recipient_not_live)?;
            Some(target)
        }
        None => None,
    };

    let asker_handle = asker.assignee_handle.clone().unwrap_or_default();
    let run_message = MessageId::new();
    let specialist_message = target.as_ref().map(|_| MessageId::new());
    let thread = new_id("thread_");
    let mut run_payload = json!({"dispatch_id": asker.id, "options": options});
    if let (Some(target), Some(copy)) = (&target, &specialist_message) {
        run_payload["routed_to"] = json!(format!("dispatch:{}", target.id));
        run_payload["copy_of"] = json!(copy);
    }
    let message = |id: &MessageId, mailbox: String, payload: serde_json::Value| MessageRow {
        sequence: None,
        id: id.clone(),
        run_id: asker.run_id.clone(),
        mailbox,
        from_handle: asker_handle.clone(),
        subject: "Question".to_owned(),
        body: Some(question.to_owned()),
        r#type: "question".to_owned(),
        priority: "normal".to_owned(),
        thread_id: Some(thread.clone()),
        payload: Some(payload),
        read: false,
        created_at: now,
        delivered_at: None,
        sender_terminal_id: Some(terminal.to_owned()),
        rejected_reason: None,
        evidence: None,
        recommendation: None,
    };
    messages::insert(
        tx,
        &message(&run_message, format!("run:{}", asker.run_id), run_payload),
    )?;
    if let (Some(target), Some(copy)) = (&target, &specialist_message) {
        let mailbox = format!("dispatch:{}", target.id);
        let payload = json!({
            "dispatch_id": asker.id,
            "options": options,
            "routed_to": mailbox,
            "copy_of": run_message.as_str(),
        });
        messages::insert(tx, &message(copy, mailbox, payload))?;
    }
    questions::insert(
        tx,
        &QuestionRow {
            message_id: run_message.clone(),
            run_id: asker.run_id.clone(),
            dispatch_id: Some(asker.id.clone()),
            asker_handle,
            status: "pending".to_owned(),
            answer_message_id: None,
            answer_body: None,
            created_at: now,
            answered_at: None,
            closed_at: None,
        },
    )?;
    Ok((run_message, specialist_message))
}

/// The reply and the stored answer message.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct Reply {
    pub question: QuestionRow,
    pub answer: MessageRow,
}

pub fn reply(engine: &Engine, message: &MessageId, body: &str) -> Result<QuestionRow> {
    let caller = engine.caller.as_ref().ok_or_else(not_in_herdr)?;
    let now = engine.clock.now();
    let reply = engine
        .store
        .tx(|tx| reply_in(tx, now, &caller.terminal_id, message, body))?;
    engine.after_commit(
        &reply.question.run_id,
        std::slice::from_ref(&reply.answer.mailbox),
    )?;
    Ok(reply.question)
}

/// Answers the question `message` belongs to (either copy, through the
/// shared thread). The first reply wins.
pub fn reply_in(
    tx: &Tx<'_>,
    now: DateTime<Utc>,
    terminal: &str,
    message: &MessageId,
    body: &str,
) -> Result<Reply> {
    let mut question = question_for_message(tx, message)?
        .ok_or_else(|| PodError::usage(format!("unknown question message {message}")))?;
    match question.status.as_str() {
        "answered" => {
            return Err(PodError::new(
                ErrorCode::QuestionAnswered,
                "the question was already answered",
                &["Do nothing; the asker can retrieve the first answer with ask --resume."],
            ));
        }
        "closed" => {
            return Err(PodError::new(
                ErrorCode::QuestionClosed,
                "the question is closed because its Dispatch ended",
                &["Do nothing; send a normal message if the answer still matters."],
            ));
        }
        _ => {}
    }
    let thread = messages::get(tx, &question.message_id)?.and_then(|row| row.thread_id);
    let mailbox = question.dispatch_id.as_ref().map_or_else(
        || format!("run:{}", question.run_id),
        |dispatch| format!("dispatch:{dispatch}"),
    );
    let mut answer = MessageRow {
        sequence: None,
        id: MessageId::new(),
        run_id: question.run_id.clone(),
        mailbox,
        from_handle: sender_handle(tx, terminal, &question.run_id)?,
        subject: "Question answered".to_owned(),
        body: Some(body.to_owned()),
        r#type: "status".to_owned(),
        priority: "normal".to_owned(),
        thread_id: thread,
        payload: Some(json!({"question_id": question.message_id, "answer": body})),
        read: false,
        created_at: now,
        delivered_at: None,
        sender_terminal_id: Some(terminal.to_owned()),
        rejected_reason: None,
        evidence: None,
        recommendation: None,
    };
    answer.sequence = Some(messages::insert(tx, &answer)?);
    questions::update_status(tx, &question.message_id, "answered")?;
    questions::update_answer_message_id(tx, &question.message_id, Some(&answer.id))?;
    questions::update_answer_body(tx, &question.message_id, Some(body))?;
    questions::update_answered_at(tx, &question.message_id, Some(now))?;
    question.status = "answered".into();
    question.answer_message_id = Some(answer.id.clone());
    question.answer_body = Some(body.to_owned());
    question.answered_at = Some(now);
    Ok(Reply { question, answer })
}

pub fn close_questions_for_dispatch(
    tx: &Tx<'_>,
    now: DateTime<Utc>,
    dispatch: &DispatchId,
) -> Result<u32> {
    questions::close_pending_for_dispatch(tx, dispatch, now)
}

/// The question row of `message` or of any message in its thread.
fn question_for_message(tx: &Tx<'_>, message: &MessageId) -> Result<Option<QuestionRow>> {
    let Some(row) = messages::get(tx, message)? else {
        return Ok(None);
    };
    if let Some(question) = questions::get(tx, &row.id)? {
        return Ok(Some(question));
    }
    let Some(thread) = row.thread_id else {
        return Ok(None);
    };
    for candidate in messages::in_thread(tx, &thread)? {
        if let Some(question) = questions::get(tx, &candidate.id)? {
            return Ok(Some(question));
        }
    }
    Ok(None)
}

/// A `dispatched` Dispatch whose worker is `ready` and `active`.
fn live_dispatch(tx: &Tx<'_>, id: &DispatchId) -> Result<Option<DispatchRow>> {
    let Some(dispatch) = dispatches::get(tx, id)?.filter(|row| row.status == "dispatched") else {
        return Ok(None);
    };
    Ok(workers::get(tx, id)?
        .is_some_and(|worker| worker.state == "ready" && worker.terminal_state == "active")
        .then_some(dispatch))
}

/// The replying terminal's handle in `run`: its newest Dispatch's worker
/// handle, else the coordinator handle when it coordinates `run`, else the
/// terminal ID.
fn sender_handle(tx: &Tx<'_>, terminal: &str, run: &RunId) -> Result<String> {
    if let Some(handle) = dispatches::list_by_run(tx, run)?
        .into_iter()
        .rev()
        .find(|dispatch| dispatch.assignee_terminal_id.as_deref() == Some(terminal))
        .and_then(|dispatch| dispatch.assignee_handle)
    {
        return Ok(handle);
    }
    Ok(runs::get(tx, run)?
        .filter(|row| row.coordinator_terminal_id == terminal)
        .map_or_else(|| terminal.to_owned(), |row| row.coordinator_handle))
}

fn not_in_herdr() -> PodError {
    PodError::new(
        ErrorCode::NotInHerdr,
        "ask and reply require a caller inside herdr",
        &["Run the command from a pane inside herdr."],
    )
}

fn recipient_not_live() -> PodError {
    PodError::new(
        ErrorCode::RecipientNotLive,
        "the specialist is not a live, different worker in this Run",
        &["Choose a worker listed under SPECIALISTS, or omit --to to ask the coordinator."],
    )
}
