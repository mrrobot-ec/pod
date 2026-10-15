use std::str::FromStr;

use clap::Args;
use serde::{Deserialize, Serialize};

use crate::cli::{Ctx, output};
use crate::engine::{gates, receipts, tasks};
use crate::error::{ErrorCode, PodError, Result};
use crate::ids::{GateId, TaskId};

use super::runs;

#[derive(Clone, Debug, Args)]
pub struct GateCreateArgs {
    #[arg(long = "for")]
    pub audience: Option<String>,
    #[arg(long)]
    pub task: Option<String>,
    #[arg(long)]
    pub question: String,
    #[arg(long)]
    pub options: Option<String>,
    #[arg(long)]
    pub evidence: Option<String>,
    #[arg(long)]
    pub recommendation: Option<String>,
}

#[derive(Clone, Debug, Args)]
pub struct GateResolveArgs {
    #[arg(long)]
    pub id: String,
    #[arg(long)]
    pub resolution: String,
}

#[derive(Clone, Debug, Args)]
pub struct GateListArgs {
    #[arg(long)]
    pub task: Option<String>,
    #[arg(long)]
    pub run: Option<String>,
}

pub fn gate_create(ctx: &Ctx, args: GateCreateArgs) -> Result<()> {
    let engine = runs::engine(ctx)?;
    runs::require_caller(&engine)?;
    let run = runs::selected_run(&engine, None)?;
    let audience = args.audience.as_deref().unwrap_or("coordinator");
    let task = args.task.as_deref().map(TaskId::from_str).transpose()?;
    let options: Vec<String> = args
        .options
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|error| {
            PodError::usage(format!(
                "--options must be a JSON array of strings: {error}"
            ))
        })?
        .unwrap_or_default();
    let request = runs::request_id(ctx)?;
    let hash = runs::payload_hash(
        &serde_json::json!({"run": run, "audience": audience, "task": task, "question": args.question, "options": options, "evidence": args.evidence, "recommendation": args.recommendation}),
    );
    let (receipt, replayed) =
        receipts::with_receipt_replay(&engine, request.as_ref(), "gate-create", &hash, |tx| {
            let live = task
                .as_ref()
                .map(|task| tasks::has_live_worker(tx, task))
                .transpose()?
                .unwrap_or(false);
            let gate = gates::create(
                tx,
                engine.clock.now(),
                &run,
                audience,
                task.as_ref(),
                &args.question,
                &options,
                args.evidence.as_deref(),
                args.recommendation.as_deref(),
                live,
            )?;
            let task_status = task
                .as_ref()
                .map(|task| tasks::get(tx, task))
                .transpose()?
                .flatten()
                .map(|task| task.status);
            Ok(GateCreateReceipt {
                gate,
                task: task_status.map(|status| TaskStatus { status }),
            })
        })?;
    engine.after_commit(&run, &[])?;
    if !replayed {
        gates::notify_created(&engine, &receipt.gate);
    }
    let mut value = serde_json::to_value(&receipt)
        .map_err(|error| PodError::store(format!("could not serialize receipt: {error}")))?;
    let warnings = engine.take_warnings();
    if !warnings.is_empty() {
        value["warnings"] = serde_json::json!(warnings);
    }
    super::emit_mutation(ctx, &value, replayed)
}

pub fn gate_resolve(ctx: &Ctx, args: GateResolveArgs) -> Result<()> {
    let engine = runs::engine(ctx)?;
    let gate_id = GateId::from_str(&args.id)?;
    let request = runs::request_id(ctx)?;
    let hash =
        runs::payload_hash(&serde_json::json!({"id": gate_id, "resolution": args.resolution}));
    let (receipt, replayed) =
        receipts::with_receipt_replay(&engine, request.as_ref(), "gate-resolve", &hash, |tx| {
            let duplicate = gates::get(tx, &gate_id)?.is_some_and(|gate| gate.status == "resolved");
            let (gate, message) =
                gates::resolve(tx, engine.clock.now(), &gate_id, &args.resolution)?;
            let task = gate
                .task_id
                .as_ref()
                .map(|task| tasks::get(tx, task))
                .transpose()?
                .flatten();
            Ok(GateResolveReceipt {
                gate,
                task,
                message,
                duplicate,
            })
        })?;
    let mailboxes = receipt
        .message
        .as_ref()
        .map(|message| vec![message.mailbox.clone()])
        .unwrap_or_default();
    engine.after_commit(&receipt.gate.run_id, &mailboxes)?;
    super::emit_mutation(ctx, &receipt, replayed)
}

pub fn gate_list(ctx: &Ctx, args: GateListArgs) -> Result<()> {
    let engine = ctx.read_engine()?;
    let run = runs::selected_run(&engine, args.run.as_deref())?;
    let task = args.task.as_deref().map(TaskId::from_str).transpose()?;
    let rows = engine.store.read(|tx| {
        if let Some(task) = task.as_ref() {
            let exists = tasks::get(tx, task)?.is_some_and(|row| row.run_id == run);
            if !exists {
                return Err(PodError::new(
                    ErrorCode::TaskNotFound,
                    format!("Task {task} was not found in this Run"),
                    &["Run pod task-list to see the Run's Tasks, then retry with a valid ID."],
                ));
            }
        }
        gates::list(tx, &run, task.as_ref())
    })?;
    let count = rows.len();
    output::emit_ok(
        ctx.json,
        &serde_json::json!({"runId": run, "gates": rows, "count": count}),
    );
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct TaskStatus {
    status: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct GateCreateReceipt {
    gate: gates::GateRow,
    task: Option<TaskStatus>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct GateResolveReceipt {
    gate: gates::GateRow,
    task: Option<tasks::TaskRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<gates::MessageRow>,
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    duplicate: bool,
}
