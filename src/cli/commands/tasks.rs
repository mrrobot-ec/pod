use std::str::FromStr;

use clap::Args;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cli::{Ctx, output};
use crate::engine::{receipts, tasks};
use crate::error::{PodError, Result};
use crate::ids::TaskId;

use super::runs;

#[derive(Clone, Debug, Args)]
pub struct TaskCreateArgs {
    #[arg(long)]
    pub spec: String,
    #[arg(long)]
    pub task_title: Option<String>,
    #[arg(long)]
    pub display_name: Option<String>,
    #[arg(long)]
    pub deps: Option<String>,
    #[arg(long)]
    pub parent: Option<String>,
}

#[derive(Clone, Debug, Args)]
pub struct TaskListArgs {
    #[arg(long)]
    pub status: Option<String>,
    #[arg(long)]
    pub ready: bool,
    #[arg(long)]
    pub brief: bool,
    #[arg(long)]
    pub run: Option<String>,
}

#[derive(Clone, Debug, Args)]
pub struct TaskUpdateArgs {
    #[arg(long)]
    pub id: String,
    #[arg(long)]
    pub status: String,
    #[arg(long)]
    pub result: Option<String>,
}

pub fn task_create(ctx: &Ctx, args: TaskCreateArgs) -> Result<()> {
    let engine = runs::engine(ctx)?;
    let caller = runs::require_caller(&engine)?.clone();
    let run = runs::selected_run(&engine, None)?;
    let deps: Vec<TaskId> = args
        .deps
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|error| {
            PodError::usage(format!("--deps must be a JSON array of Task IDs: {error}"))
        })?
        .unwrap_or_default();
    let parent = args.parent.as_deref().map(TaskId::from_str).transpose()?;
    let request = runs::request_id(ctx)?;
    let hash = runs::payload_hash(
        &serde_json::json!({"run": run, "spec": args.spec, "title": args.task_title, "display": args.display_name, "deps": deps, "parent": parent}),
    );
    let (receipt, replayed) =
        receipts::with_receipt_replay(&engine, request.as_ref(), "task-create", &hash, |tx| {
            tasks::create(
                tx,
                engine.clock.now(),
                &run,
                &args.spec,
                args.task_title.as_deref(),
                args.display_name.as_deref(),
                &deps,
                parent.as_ref(),
                &caller.terminal_id,
            )
            .map(|task| TaskReceipt { task })
        })?;
    engine.after_commit(&run, &[])?;
    super::emit_mutation(ctx, &receipt, replayed)
}

pub fn task_list(ctx: &Ctx, args: TaskListArgs) -> Result<()> {
    let engine = ctx.read_engine()?;
    let run = runs::selected_run(&engine, args.run.as_deref())?;
    let filter = if args.ready {
        Some("ready")
    } else {
        args.status.as_deref()
    };
    if let Some(status) = filter
        && !matches!(
            status,
            "pending" | "ready" | "dispatched" | "completed" | "failed" | "blocked"
        )
    {
        return Err(PodError::usage(format!("invalid Task status {status}")));
    }
    let rows = engine.store.read(|tx| {
        let rows = tasks::list_by_run(tx, &run, filter)?;
        rows.into_iter()
            .map(|row| {
                let failed = tasks::failed_dependency(tx, &row.id)?;
                let mut value = serde_json::to_value(&row)
                    .map_err(|error| PodError::store(error.to_string()))?;
                if let Value::Object(fields) = &mut value {
                    if args.brief {
                        fields.remove("spec");
                    }
                    if let Some(failed) = failed {
                        fields.insert(
                            "attention".into(),
                            Value::String("waiting_on_failed_dependency".into()),
                        );
                        // Retry the dependency or change the plan (coordinator
                        // guide): first find the Dispatch to pass to --retry-of.
                        fields.insert(
                            "nextAction".into(),
                            serde_json::json!(["pod", "dispatch-show", "--task", failed]),
                        );
                    }
                    if let Some(ended) = tasks::latest_closure(tx, &row.id)? {
                        fields.insert(
                            "termination_reason".into(),
                            ended
                                .termination_reason
                                .map(Value::String)
                                .unwrap_or(Value::Null),
                        );
                        fields.insert(
                            "termination_target".into(),
                            ended
                                .termination_target
                                .map(Value::String)
                                .unwrap_or(Value::Null),
                        );
                    }
                }
                Ok(value)
            })
            .collect::<Result<Vec<_>>>()
    })?;
    let count = rows.len();
    output::emit_ok(
        ctx.json,
        &serde_json::json!({"runId": run, "tasks": rows, "count": count}),
    );
    Ok(())
}

pub fn task_update(ctx: &Ctx, args: TaskUpdateArgs) -> Result<()> {
    let engine = runs::engine(ctx)?;
    let task_id = TaskId::from_str(&args.id)?;
    let result: Option<Value> = args
        .result
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|error| PodError::usage(format!("--result must be JSON: {error}")))?;
    let request = runs::request_id(ctx)?;
    let hash = runs::payload_hash(
        &serde_json::json!({"id": task_id, "status": args.status, "result": result}),
    );
    let (receipt, replayed) =
        receipts::with_receipt_replay(&engine, request.as_ref(), "task-update", &hash, |tx| {
            let live = tasks::has_live_worker(tx, &task_id)?;
            let task = tasks::update_status(
                tx,
                engine.clock.now(),
                &task_id,
                &args.status,
                result.clone(),
                live,
            )?;
            let promoted = if args.status == "completed" {
                tasks::promote_dependants(tx, engine.clock.now(), &task_id)?
            } else {
                Vec::new()
            };
            Ok(TaskUpdateReceipt { task, promoted })
        })?;
    engine.after_commit(&receipt.task.run_id, &[])?;
    super::emit_mutation(ctx, &receipt, replayed)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct TaskReceipt {
    task: tasks::TaskRow,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct TaskUpdateReceipt {
    task: tasks::TaskRow,
    promoted: Vec<TaskId>,
}
