use std::str::FromStr;

use clap::Args;
use serde::{Deserialize, Serialize};

use crate::cli::{Ctx, output};
use crate::engine::{Engine, receipts, tasks};
use crate::error::{ErrorCode, PodError, Result};
use crate::ids::{RequestId, RunId};
use crate::store::rows::{dispatches as dispatch_rows, receipts as receipt_rows, runs as run_rows};

use super::runs;

#[derive(Clone, Debug, Args)]
pub struct RequestShowArgs {
    #[arg(long)]
    pub request: String,
}

#[derive(Clone, Debug, Args)]
pub struct ResetArgs {
    #[arg(long)]
    pub scope: String,
    #[arg(long)]
    pub force: bool,
}

pub fn request_show(ctx: &Ctx, args: RequestShowArgs) -> Result<()> {
    let engine = runs::engine(ctx)?;
    let request = RequestId::from_str(&args.request)?;
    let value = match receipts::show(&engine, &request)? {
        Some(row) => serde_json::json!({
            "requestId": row.request_id,
            "state": row.state,
            "method": row.method,
            "createdAt": row.created_at,
            "updatedAt": row.updated_at,
            "receipt": row.receipt,
            "interpretation": interpretation(&row.state),
        }),
        None => serde_json::json!({
            "requestId": request,
            "state": "absent",
            "interpretation": interpretation("absent"),
        }),
    };
    output::emit_ok(ctx.json, &value);
    Ok(())
}

/// What the caller should do next for each receipt state (commands.md).
fn interpretation(state: &str) -> &'static str {
    match state {
        "completed" => "The mutation committed; receipt is its original result. Do not retry.",
        "pending" => "The mutation did not complete. Replay the same command with --retry-request.",
        _ => "No mutation was recorded under this request ID. Inspect state before retrying.",
    }
}

pub fn reset(ctx: &Ctx, args: ResetArgs) -> Result<()> {
    let engine = runs::engine(ctx)?;
    let request = runs::request_id(ctx)?;
    let hash = runs::payload_hash(&serde_json::json!({"scope": args.scope, "force": args.force}));
    let (receipt, replayed) =
        receipts::with_receipt_replay(&engine, request.as_ref(), "reset", &hash, |tx| {
            let (deleted, removed) = reset_in_tx(tx, &args.scope, args.force)?;
            Ok(ResetReceipt {
                reset: ResetResult {
                    scope: args.scope.clone(),
                    deleted,
                },
                removed,
            })
        })?;
    // The commit hook removes the board directory of a Run that is gone.
    for run in &receipt.removed {
        engine.after_commit(run, &[])?;
    }
    super::emit_mutation(ctx, &receipt, replayed)
}

pub fn reset_engine(engine: &Engine, scope: &str, force: bool) -> Result<bool> {
    let (deleted, removed) = engine.store.tx(|tx| reset_in_tx(tx, scope, force))?;
    for run in &removed {
        engine.after_commit(run, &[])?;
    }
    Ok(deleted)
}

/// Deletes the scope's Runs (rows in every other table cascade) and returns
/// whether anything was deleted and which Runs are gone.
fn reset_in_tx(tx: &crate::store::Tx<'_>, scope: &str, force: bool) -> Result<(bool, Vec<RunId>)> {
    if scope == "all" {
        if !force {
            return Err(reset_requires_force(
                "reset --scope all requires --force",
                &[],
            ));
        }
        let removed = run_rows::list_by_updated(tx)?
            .into_iter()
            .map(|run| run.id)
            .collect::<Vec<_>>();
        let deleted = run_rows::delete_all(tx)?;
        receipt_rows::delete_all(tx)?;
        return Ok((deleted, removed));
    }
    let id = scope
        .strip_prefix("run:")
        .ok_or_else(|| PodError::usage("--scope must be run:<id> or all"))?;
    let run = RunId::from_str(id)?;
    if run_rows::get(tx, &run)?.is_none() {
        return Err(PodError::usage(format!("Run {run} was not found")));
    }
    let live_workers = live_workers(tx, &run)?;
    if !force && !live_workers.is_empty() {
        return Err(reset_requires_force(
            &format!("Run {run} has live workers: {}", live_workers.join(", ")),
            &live_workers,
        ));
    }
    Ok((run_rows::delete(tx, &run)?, vec![run]))
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ResetReceipt {
    reset: ResetResult,
    #[serde(skip)]
    removed: Vec<RunId>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ResetResult {
    scope: String,
    deleted: bool,
}

/// The handles (else terminal or Dispatch IDs) of the Run's live workers.
fn live_workers(tx: &crate::store::Tx<'_>, run: &RunId) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for dispatch in dispatch_rows::list_by_run(tx, run)? {
        if tasks::is_live_worker(tx, &dispatch.id)? {
            let name = dispatch
                .assignee_handle
                .or(dispatch.assignee_terminal_id)
                .unwrap_or_else(|| dispatch.id.to_string());
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    Ok(names)
}

fn reset_requires_force(message: &str, workers: &[String]) -> PodError {
    PodError::new(
        ErrorCode::ResetRequiresForce,
        message,
        &["Stop or release the live workers listed, or rerun with --force."],
    )
    .with_data("liveWorkers", serde_json::json!(workers))
}
