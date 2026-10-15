use std::str::FromStr;

use clap::Args;
use serde::{Deserialize, Serialize};

use crate::cli::{Ctx, output};
use crate::engine::{Engine, receipts, runs};
use crate::error::{ErrorCode, PodError, Result};
use crate::identity::Caller;
use crate::ids::{RequestId, RunId};

#[derive(Clone, Debug, Args)]
pub struct RunCreateArgs {
    #[arg(long)]
    pub objective: String,
    /// A soft budget in USD: `worker-start` is refused once the Run's workers
    /// have reported spending it. Cost is the agents' own report.
    #[arg(long)]
    pub budget_usd: Option<f64>,
}

#[derive(Clone, Debug, Args)]
pub struct RunUseArgs {
    #[arg(long)]
    pub id: String,
}

#[derive(Clone, Debug, Args)]
pub struct RunShowArgs {
    #[arg(long)]
    pub id: String,
}

pub fn run_create(ctx: &Ctx, args: RunCreateArgs) -> Result<()> {
    if let Some(budget) = args.budget_usd
        && !(budget.is_finite() && budget > 0.0)
    {
        return Err(PodError::usage("--budget-usd must be a positive number"));
    }
    let engine = engine(ctx)?;
    let caller = require_caller(&engine)?.clone();
    let request = request_id(ctx)?;
    let hash = payload_hash(&match args.budget_usd {
        Some(budget) => serde_json::json!({"objective": args.objective, "budgetUsd": budget}),
        None => serde_json::json!({"objective": args.objective}),
    });
    let cwd = std::env::current_dir().map_err(|error| PodError::usage(error.to_string()))?;
    let (receipt, replayed) =
        receipts::with_receipt_replay(&engine, request.as_ref(), "run-create", &hash, |tx| {
            runs::create_with_budget(
                tx,
                engine.clock.now(),
                &args.objective,
                &caller,
                &cwd,
                args.budget_usd,
            )
            .map(|run| RunReceipt {
                budget_usd: run.budget_usd,
                run,
            })
        })?;
    engine.after_commit(&receipt.run.id, &[])?;
    super::emit_mutation(ctx, &receipt, replayed)
}

pub fn run_use(ctx: &Ctx, args: RunUseArgs) -> Result<()> {
    let engine = engine(ctx)?;
    let caller = require_caller(&engine)?.clone();
    let run = RunId::from_str(&args.id)?;
    let request = request_id(ctx)?;
    let hash = payload_hash(&serde_json::json!({"id": args.id}));
    let (receipt, replayed) =
        receipts::with_receipt_replay(&engine, request.as_ref(), "run-use", &hash, |tx| {
            let (run, fenced_deliveries) =
                runs::use_run_fencing(tx, engine.clock.now(), &run, &caller)?;
            Ok(RunUseReceipt {
                run,
                fenced_deliveries,
            })
        })?;
    engine.after_commit(&receipt.run.id, &[])?;
    super::emit_mutation(ctx, &receipt, replayed)
}

pub fn run_current(ctx: &Ctx) -> Result<()> {
    let engine = ctx.read_engine()?;
    let run = if let Some(caller) = engine.caller.as_ref() {
        engine.store.read(|tx| runs::bound_run(tx, caller))?
    } else {
        None
    };
    output::emit_ok(
        ctx.json,
        &serde_json::json!({"run": run, "source": "bound"}),
    );
    Ok(())
}

/// Every Run, most recently bound first, as full rows with `tasks` and
/// `activeWorkers` counts. There is no paging, so `nextCursor` is null.
pub fn run_list(ctx: &Ctx) -> Result<()> {
    let engine = ctx.read_engine()?;
    let values = engine.store.read(|tx| {
        crate::store::rows::runs::list_by_updated(tx)?
            .into_iter()
            .map(|run| {
                let counts = runs::counts(tx, &run.id)?;
                let mut value = serde_json::to_value(&run)
                    .map_err(|error| PodError::store(error.to_string()))?;
                value["tasks"] = counts.tasks.values().sum::<u32>().into();
                value["activeWorkers"] = counts.active_workers.into();
                Ok(value)
            })
            .collect::<Result<Vec<_>>>()
    })?;
    output::emit_ok(
        ctx.json,
        &serde_json::json!({"runs": values, "nextCursor": null}),
    );
    Ok(())
}

pub fn run_show(ctx: &Ctx, args: RunShowArgs) -> Result<()> {
    let engine = ctx.read_engine()?;
    let id = RunId::from_str(&args.id)?;
    let usage = crate::usage::UsageStore::new(engine.paths.state_dir.join("usage"));
    let report = engine.store.read(|tx| {
        let run = runs::get(tx, &id)?
            .ok_or_else(|| PodError::usage(format!("Run {id} was not found")))?;
        let counts = runs::counts(tx, &id)?;
        let mut report =
            serde_json::json!({"run": run, "tasks": counts.tasks, "workers": counts.workers});
        // Only a Run with a budget reports spend, so other Runs keep the
        // Orca contract's shape.
        if let Some(budget) = run.budget_usd {
            report["budgetUsd"] = budget.into();
            report["spentUsd"] = runs::spent_usd(tx, &usage, &id)?.into();
        }
        Ok(report)
    })?;
    output::emit_ok(ctx.json, &report);
    Ok(())
}

/// `budgetUsd` repeats the Run's budget at the top level, and only when set,
/// so a Run without one keeps the Orca contract's shape.
#[derive(Deserialize, Serialize)]
struct RunReceipt {
    run: runs::RunRow,
    #[serde(default, rename = "budgetUsd", skip_serializing_if = "Option::is_none")]
    budget_usd: Option<f64>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RunUseReceipt {
    run: runs::RunRow,
    fenced_deliveries: u32,
}

pub(crate) fn engine(ctx: &Ctx) -> Result<Engine> {
    ctx.engine()
}

pub(crate) fn require_caller(engine: &Engine) -> Result<&Caller> {
    engine.caller.as_ref().ok_or_else(|| {
        PodError::new(
            ErrorCode::NotInHerdr,
            "this mutation requires a caller inside herdr",
            &["Run the command from a pane inside herdr."],
        )
    })
}

pub(crate) fn request_id(ctx: &Ctx) -> Result<Option<RequestId>> {
    ctx.retry_request
        .as_deref()
        .map(RequestId::from_str)
        .transpose()
}

pub(crate) fn payload_hash(value: &serde_json::Value) -> String {
    receipts::payload_hash(value)
}

pub(crate) fn selected_run(engine: &Engine, explicit: Option<&str>) -> Result<RunId> {
    if let Some(id) = explicit {
        // A Run named by --run that does not exist is a usage error
        // (commands.md), not an empty listing.
        let run = RunId::from_str(id)?;
        if engine.store.read(|tx| runs::get(tx, &run))?.is_none() {
            return Err(PodError::usage(format!("Run {run} was not found")));
        }
        return Ok(run);
    }
    // Read-only commands work outside herdr with --run (commands.md); with
    // no caller there is no bound Run, which is a usage error like T3's reads.
    let caller = engine
        .caller
        .as_ref()
        .ok_or_else(|| PodError::usage("outside herdr there is no bound Run; pass --run <id>"))?;
    engine.store.read(|tx| {
        runs::bound_run(tx, caller)?
            .map(|run| run.id)
            .ok_or_else(|| {
                PodError::usage(
                    "no Run is bound to this terminal; use run-create, run-use, or --run",
                )
            })
    })
}
