use clap::Args;
use serde_json::{Value, json};
use std::str::FromStr;

use super::runs;
use crate::cli::Ctx;
use crate::error::{PodError, Result};
use crate::ids::{DeliveryId, DispatchId, MessageId, RequestId, RunId, TaskId};
use crate::store::rows::messages as message_rows;
use crate::store::rows::questions as question_rows;
use crate::{cli::output, engine};

#[derive(Clone, Debug, Args)]
pub struct SendArgs {
    #[arg(long)]
    pub subject: String,
    #[arg(long)]
    pub to: Option<String>,
    #[arg(long)]
    pub body: Option<String>,
    #[arg(long = "type")]
    pub message_type: Option<String>,
    #[arg(long)]
    pub priority: Option<String>,
    #[arg(long)]
    pub thread_id: Option<String>,
    #[arg(
        long,
        conflicts_with_all = [
            "task_id",
            "dispatch_id",
            "outcome",
            "reason",
            "reason_target",
            "files_modified",
            "report_path",
            "phase",
            "evidence",
            "recommendation"
        ]
    )]
    pub payload: Option<String>,
    #[arg(long)]
    pub task_id: Option<String>,
    #[arg(long)]
    pub dispatch_id: Option<String>,
    #[arg(long)]
    pub outcome: Option<String>,
    #[arg(long)]
    pub reason: Option<String>,
    #[arg(long)]
    pub reason_target: Option<String>,
    #[arg(long)]
    pub files_modified: Option<String>,
    #[arg(long)]
    pub report_path: Option<String>,
    #[arg(long)]
    pub phase: Option<String>,
    #[arg(long)]
    pub evidence: Option<String>,
    #[arg(long)]
    pub recommendation: Option<String>,
    #[arg(long)]
    pub from: Option<String>,
    #[arg(long)]
    pub dispatch_capability: Option<String>,
}

#[derive(Clone, Debug, Args)]
#[command(group(clap::ArgGroup::new("check_mode").multiple(false).args(["unread", "peek", "all"])))]
pub struct CheckArgs {
    #[arg(long)]
    pub terminal: Option<String>,
    #[arg(long)]
    pub run: Option<String>,
    #[arg(long)]
    pub ack: Option<String>,
    #[arg(long)]
    pub unread: bool,
    #[arg(long)]
    pub peek: bool,
    #[arg(long)]
    pub all: bool,
    #[arg(long)]
    pub types: Option<String>,
    #[arg(long)]
    pub format: bool,
    #[arg(long)]
    pub wait: bool,
    #[arg(long)]
    pub timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, Args)]
#[command(group(clap::ArgGroup::new("question_mode").required(true).multiple(false).args(["question", "resume"])))]
pub struct AskArgs {
    #[arg(long)]
    pub question: Option<String>,
    #[arg(long)]
    pub options: Option<String>,
    #[arg(long)]
    pub timeout_ms: Option<u64>,
    #[arg(long)]
    pub resume: Option<String>,
    #[arg(long)]
    pub to: Option<String>,
    #[arg(long)]
    pub from: Option<String>,
}

#[derive(Clone, Debug, Args)]
pub struct ReplyArgs {
    #[arg(long)]
    pub id: String,
    #[arg(long)]
    pub body: String,
}

#[derive(Clone, Debug, Args)]
pub struct InboxArgs {
    #[arg(long)]
    pub limit: Option<u32>,
    #[arg(long)]
    pub terminal: Option<String>,
    #[arg(long)]
    pub full: bool,
    #[arg(long)]
    pub run: Option<String>,
}

// ---- handlers -------------------------------------------------------------
// One function per command, owned by T3 (T8 adds the blocking paths of
// `check --wait` and `ask`). Every storing or consuming form runs under the
// `--retry-request` receipt, runs the commit hook, and carries `mutation`.

pub fn send(ctx: &Ctx, args: SendArgs) -> Result<()> {
    let engine = ctx.engine()?;
    let payload = args
        .payload
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|error| PodError::usage(format!("invalid --payload JSON: {error}")))?;
    let task_id = args.task_id.as_deref().map(TaskId::from_str).transpose()?;
    let dispatch_id = args
        .dispatch_id
        .as_deref()
        .map(DispatchId::from_str)
        .transpose()?;
    let input = engine::messages::SendInput {
        to: args.to,
        subject: args.subject,
        body: args.body,
        r#type: args.message_type.unwrap_or_else(|| "status".into()),
        priority: args.priority.unwrap_or_else(|| "normal".into()),
        thread_id: args.thread_id,
        payload,
        task_id,
        dispatch_id,
        outcome: args.outcome,
        reason: args.reason,
        reason_target: args.reason_target,
        files_modified: args
            .files_modified
            .map(|value| {
                value
                    .split(',')
                    .filter(|path| !path.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        report_path: args.report_path,
        phase: args.phase,
        evidence: args.evidence,
        recommendation: args.recommendation,
        from: args.from,
    };
    let _ignored_compatibility_capability = args.dispatch_capability;
    let request = request_id(ctx)?;
    let (receipt, replayed) =
        engine::messages::send_with_receipt(&engine, input, request.as_ref())?;
    let rows = engine
        .store
        .read(|tx| message_rows::get_many(tx, &receipt.message_ids))?;
    let mut value = json!({"message": rows.first()});
    if rows.len() > 1 {
        value["messages"] = json!(rows);
        value["mailbox"] = json!(receipt.mailbox);
    }
    if let Some(settled) = &receipt.settled {
        value["settled"] = json!(settled);
    }
    if receipt.duplicate {
        value["duplicate"] = json!(true);
    }
    if !receipt.warnings.is_empty() {
        value["warnings"] = json!(receipt.warnings);
    }
    super::emit_mutation(ctx, &value, replayed)
}

pub fn check(ctx: &Ctx, args: CheckArgs) -> Result<()> {
    let engine = ctx.engine()?;
    let types = csv(args.types.as_deref());
    if args.wait {
        if args.peek || args.all {
            return Err(PodError::usage(
                "--wait cannot be combined with --peek or --all",
            ));
        }
        // `check --wait` is the coordinator's wait on the Run mailbox, the
        // Run's single waiter slot (8.3). A worker's mailbox is read with
        // `check --terminal`, which marks read on return; waiting on it here
        // would hand the worker a replay-until-ack Delivery and take the
        // coordinator's slot.
        if args.terminal.is_some() || engine::messages::caller_worker_handle(&engine)?.is_some() {
            return Err(PodError::usage(
                "check --wait waits on the Run mailbox for the coordinator; a worker reads its mail with `pod check --terminal <handle>` and waits for answers with `pod ask`",
            ));
        }
        let mailbox = if let Some(run) = args.run.as_deref() {
            format!("run:{}", RunId::from_str(run)?)
        } else {
            engine::messages::mailbox_for(&engine, None)?
        };
        // Acknowledging a worker_done releases its worker (spec 7.4), and
        // the wait first releases every other acknowledged one: going back
        // to waiting means the completion choice was made.
        let mut released = Vec::new();
        if let Some(ack) = args.ack.as_deref() {
            let acked = engine::deliveries::check_ack(&engine, &DeliveryId::from_str(ack)?)?;
            released.extend(engine::lifecycle::release_acknowledged(&engine, &acked)?);
        }
        if engine.config.defaults.auto_release
            && let Some(run) = mailbox.strip_prefix("run:")
        {
            released.extend(
                engine::lifecycle::gc(&engine, Some(&RunId::from_str(run)?), true)?.released,
            );
        }
        let (delivery, woke_on) = engine::waiting::check_wait_mailbox(
            &engine,
            &mailbox,
            &types,
            args.timeout_ms.unwrap_or(600_000),
        )?;
        let messages = &delivery.messages;
        // The wait ended on a matching message, on a worker that newly
        // requires action, or else it timed out; the batch waiting at that
        // moment is returned either way.
        let woke = messages
            .iter()
            .any(|message| types.is_empty() || types.contains(&message.r#type));
        let mut value = if messages.is_empty() {
            json!({"delivery": Value::Null, "messages": [], "count": 0, "timedOut": !woke && woke_on != engine::waiting::WokeOn::Attention})
        } else {
            let mut delivery_view = json!(delivery);
            if let Value::Object(fields) = &mut delivery_view {
                fields.remove("messages");
            }
            json!({"delivery": delivery_view, "messages": messages, "count": messages.len(), "timedOut": !woke && woke_on != engine::waiting::WokeOn::Attention})
        };
        value["wokeOn"] = json!(if woke {
            engine::waiting::WokeOn::Message
        } else {
            woke_on
        });
        value["attention"] = json!(attention_view(&engine, &delivery.run_id)?);
        value["released"] = released_view(&released);
        let warnings = engine.take_warnings();
        if !warnings.is_empty() {
            value["warnings"] = json!(warnings);
        }
        if args.format {
            value["formatted"] = json!(format_messages(messages));
        }
        return super::emit_mutation(ctx, &value, false);
    }
    let request = request_id(ctx)?;
    let now = engine.clock.now();
    // A worker that runs a bare `pod check` (the nudge text says exactly that)
    // reads its own Dispatch mailbox, never the coordinator's Run mailbox.
    let worker = match (&args.terminal, &args.run) {
        (Some(handle), _) => Some(handle.clone()),
        (None, None) => engine::messages::caller_worker_handle(&engine)?,
        (None, Some(_)) => None,
    };
    if let Some(handle) = worker {
        let caller = engine.caller.as_ref().ok_or_else(|| {
            PodError::new(
                crate::ErrorCode::NotInHerdr,
                "check --terminal marks messages read and needs a caller inside herdr",
                &["Run it from the worker's pane, or use pod inbox --terminal <handle> --run <id> to read without consuming."],
            )
        })?;
        let hash = runs::payload_hash(&json!({"terminal": handle}));
        let (messages, replayed) = engine::receipts::with_receipt_replay(
            &engine,
            request.as_ref(),
            "check",
            &hash,
            |tx| engine::deliveries::worker_check_in(tx, now, &caller.terminal_id, &handle),
        )?;
        if let Some(row) = messages.first() {
            engine.after_commit(&row.run_id, &[])?;
        }
        let mut value = json!({"messages": messages, "count": messages.len()});
        if args.format {
            value["formatted"] = json!(format_messages(&messages));
        }
        return super::emit_mutation(ctx, &value, replayed);
    }
    let mailbox = if let Some(run) = args.run.as_deref() {
        format!("run:{run}")
    } else {
        engine::messages::mailbox_for(&engine, None)?
    };
    let ack = args.ack.as_deref().map(DeliveryId::from_str).transpose()?;
    if args.peek || args.all {
        let mut replayed = None;
        let mut released = Vec::new();
        if let Some(ack) = &ack {
            let hash = runs::payload_hash(&json!({"ack": ack}));
            let (delivery, again) = engine::receipts::with_receipt_replay(
                &engine,
                request.as_ref(),
                "check",
                &hash,
                |tx| engine::deliveries::check_ack_in(tx, now, ack),
            )?;
            engine.after_commit(&delivery.run_id, &[])?;
            if !again {
                released = engine::lifecycle::release_acknowledged(&engine, &delivery)?;
            }
            replayed = Some(again);
        }
        let messages = if args.peek {
            engine::deliveries::check_peek(&engine, &mailbox, &types)?
        } else {
            engine::deliveries::check_all(&engine, &mailbox, &types)?
        };
        let mut value = json!({"messages": messages, "count": messages.len()});
        if ack.is_some() {
            value["released"] = released_view(&released);
        }
        if args.format {
            value["formatted"] = json!(format_messages(&messages));
        }
        return match replayed {
            Some(replayed) => super::emit_mutation(ctx, &value, replayed),
            None => {
                output::emit_ok(ctx.json, &value);
                Ok(())
            }
        };
    }
    let consumer = engine::deliveries::consumer(&engine);
    let hash = runs::payload_hash(&json!({"mailbox": mailbox, "ack": ack}));
    let mut acknowledged = None;
    let (delivery, replayed) =
        engine::receipts::with_receipt_replay(&engine, request.as_ref(), "check", &hash, |tx| {
            if let Some(ack) = &ack {
                acknowledged = Some(engine::deliveries::check_ack_in(tx, now, ack)?);
            }
            engine::deliveries::check_consume_in(tx, now, &mailbox, &consumer)
        })?;
    engine.after_commit(&delivery.run_id, &[])?;
    // Acknowledging a worker_done releases its worker (spec 7.4); a replayed
    // receipt acknowledged nothing new.
    let released = match acknowledged {
        Some(acked) => engine::lifecycle::release_acknowledged(&engine, &acked)?,
        None => Vec::new(),
    };
    let messages = &delivery.messages;
    let mut value = if messages.is_empty() {
        json!({"delivery": Value::Null, "messages": [], "count": 0, "timedOut": false})
    } else {
        let mut delivery_view = json!(delivery);
        if let Value::Object(fields) = &mut delivery_view {
            fields.remove("messages");
        }
        json!({"delivery": delivery_view, "messages": messages, "count": messages.len()})
    };
    if ack.is_some() {
        value["released"] = released_view(&released);
    }
    if args.format {
        value["formatted"] = json!(format_messages(messages));
    }
    let _explicit_unread = args.unread;
    super::emit_mutation(ctx, &value, replayed)
}

/// The workers a check released, for the receipt's `released`.
fn released_view(released: &[engine::lifecycle::WorkerReceipt]) -> Value {
    json!(
        released
            .iter()
            .map(|worker| {
                json!({
                    "dispatchId": worker.dispatch_id,
                    "state": worker.state,
                    "closedTabs": worker.closed_tabs,
                    "closedPanes": worker.closed_panes,
                    "closedWorkspaces": worker.closed_workspaces,
                    "leftAlone": worker.left_alone,
                })
            })
            .collect::<Vec<_>>()
    )
}

pub fn ask(ctx: &Ctx, args: AskArgs) -> Result<()> {
    let engine = ctx.engine()?;
    let timeout_ms = args.timeout_ms.unwrap_or(600_000);
    if timeout_ms > 1_800_000 {
        return Err(PodError::usage("--timeout-ms may not exceed 1800000"));
    }
    if let Some(resume) = args.resume {
        let message = MessageId::from_str(&resume)?;
        return emit_ask_outcome(ctx, &engine, &message, timeout_ms, false);
    }
    let question = args
        .question
        .ok_or_else(|| PodError::usage("--question is required"))?;
    if let Some(requested) = &args.from
        && engine::messages::caller_worker_handle(&engine)?
            .is_some_and(|handle| &handle != requested)
    {
        return Err(PodError::new(
            crate::ErrorCode::CallerMismatch,
            "--from does not match the resolved caller",
            &["Leave out --from, or pass your own handle."],
        ));
    }
    let caller = runs::require_caller(&engine)?.clone();
    let options = csv(args.options.as_deref());
    let request = request_id(ctx)?;
    let now = engine.clock.now();
    let hash =
        runs::payload_hash(&json!({"question": question, "options": options, "to": args.to}));
    let ((run_copy, specialist_copy, run, mailboxes), replayed) =
        engine::receipts::with_receipt_replay(&engine, request.as_ref(), "ask", &hash, |tx| {
            let (run_copy, specialist_copy) = engine::questions::ask_store_in(
                tx,
                now,
                &caller.terminal_id,
                &question,
                &options,
                args.to.as_deref(),
            )?;
            let run_row = message_rows::get(tx, &run_copy)?;
            let run = run_row.as_ref().map(|row| row.run_id.clone());
            let mut mailboxes = run_row
                .map(|row| row.mailbox)
                .into_iter()
                .collect::<Vec<_>>();
            if let Some(copy) = specialist_copy.as_ref()
                && let Some(row) = message_rows::get(tx, copy)?
            {
                mailboxes.push(row.mailbox);
            }
            Ok((run_copy, specialist_copy, run, mailboxes))
        })?;
    if let Some(run) = &run {
        engine.after_commit(run, &mailboxes)?;
    }
    let _ = specialist_copy;
    emit_ask_outcome(ctx, &engine, &run_copy, timeout_ms, replayed)
}

fn emit_ask_outcome(
    ctx: &Ctx,
    engine: &crate::engine::Engine,
    message: &MessageId,
    timeout_ms: u64,
    replayed: bool,
) -> Result<()> {
    let outcome = engine::waiting::ask_wait(engine, message, timeout_ms)?;
    let question = engine
        .store
        .read(|tx| question_rows::get(tx, message))?
        .ok_or_else(|| PodError::usage(format!("unknown question {message}")))?;
    match outcome {
        engine::waiting::AskOutcome::Answered(body) => super::emit_mutation(
            ctx,
            &json!({"question": {"message_id": message, "status": "answered", "answer_body": body}}),
            replayed,
        ),
        engine::waiting::AskOutcome::Timeout(_) => {
            if question.status == "closed" {
                return super::emit_mutation(
                    ctx,
                    &json!({"question": {"message_id": message, "status": "closed"}}),
                    replayed,
                );
            }
            output::emit_ok(
                ctx.json,
                &json!({
                    "question": {"message_id": question.message_id, "status": "pending"},
                    "timedOut": true,
                    "resume": format!("pod ask --resume {message}")
                }),
            );
            Ok(())
        }
    }
}

pub fn reply(ctx: &Ctx, args: ReplyArgs) -> Result<()> {
    let engine = ctx.engine()?;
    let caller = engine.caller.clone().ok_or_else(|| {
        PodError::new(
            crate::ErrorCode::NotInHerdr,
            "ask and reply require a caller inside herdr",
            &["Run the command from a pane inside herdr."],
        )
    })?;
    let message = MessageId::from_str(&args.id)?;
    let request = request_id(ctx)?;
    let now = engine.clock.now();
    let hash = runs::payload_hash(&json!({"id": message, "body": args.body}));
    let (reply, replayed) =
        engine::receipts::with_receipt_replay(&engine, request.as_ref(), "reply", &hash, |tx| {
            engine::questions::reply_in(tx, now, &caller.terminal_id, &message, &args.body)
        })?;
    engine.after_commit(
        &reply.question.run_id,
        std::slice::from_ref(&reply.answer.mailbox),
    )?;
    super::emit_mutation(
        ctx,
        &json!({"message": reply.answer, "question": reply.question}),
        replayed,
    )
}

/// Read-only. Rows are the full message rows `check` returns, newest first;
/// without `--full` each body is cut to its first 200 characters.
pub fn inbox(ctx: &Ctx, args: InboxArgs) -> Result<()> {
    let engine = ctx.read_engine()?;
    let mailbox = if let Some(handle) = args.terminal.as_deref() {
        engine::messages::mailbox_for(&engine, Some(handle))?
    } else if let Some(run) = args.run.as_deref() {
        format!("run:{run}")
    } else {
        engine::messages::mailbox_for(&engine, None)?
    };
    let mut rows = engine::deliveries::check_all(&engine, &mailbox, &[])?;
    rows.truncate(args.limit.unwrap_or(100).min(100) as usize);
    if !args.full {
        for row in &mut rows {
            if let Some(body) = row.body.as_mut()
                && let Some((cut, _)) = body.char_indices().nth(INBOX_BODY_CHARS)
            {
                body.truncate(cut);
            }
        }
    }
    let count = rows.len();
    output::emit_ok(
        ctx.json,
        &json!({"mailbox": mailbox, "messages": rows, "count": count}),
    );
    Ok(())
}

/// Characters of each body `inbox` shows without `--full`.
const INBOX_BODY_CHARS: usize = 200;

fn request_id(ctx: &Ctx) -> Result<Option<RequestId>> {
    runs::request_id(ctx)
}

fn csv(value: Option<&str>) -> Vec<String> {
    value
        .map(|value| {
            value
                .split(',')
                .filter(|part| !part.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// The workers of `run` that require action right now, for a `check --wait`
/// receipt: the coordinator sees a prompt, an exit, or an exhausted ladder
/// without a second command.
fn attention_view(engine: &engine::Engine, run: &RunId) -> Result<Vec<Value>> {
    let now = engine.clock.now();
    // One store read at a time: the set is computed before the projections.
    let set = engine::waiting::attention_set(engine, run)?;
    engine.store.read(|tx| {
        let mut rows = Vec::new();
        for dispatch in set {
            let id = DispatchId::from_str(&dispatch)?;
            let projection = engine::observe::project(tx, now, &id)?;
            rows.push(json!({
                "dispatchId": dispatch,
                "categories": projection.attention.categories,
                "nextAction": projection.next_action,
            }));
        }
        Ok(rows)
    })
}

fn format_messages(messages: &[engine::deliveries::MessageRow]) -> String {
    messages
        .iter()
        .map(|message| {
            let tag = match message.priority.as_str() {
                "high" => "[HIGH] ",
                "urgent" => "[URGENT] ",
                _ => "",
            };
            let mut line = format!(
                "{tag}[{}] {} from {}: {}",
                message.r#type, message.id, message.from_handle, message.subject
            );
            if let Some(body) = message.body.as_deref().filter(|body| !body.is_empty()) {
                line.push_str("\n  ");
                line.push_str(&body.replace('\n', "\n  "));
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}
