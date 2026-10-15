use std::collections::BTreeMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde_json::json;

use crate::error::{PodError, Result};
use crate::identity::Caller;
use crate::ids::RunId;
use crate::store::Tx;
use crate::store::rows::{deliveries, dispatches, events, runs, tasks, workers};
use crate::usage::UsageStore;

pub use crate::store::rows::runs::RunRow;

pub fn create(
    tx: &Tx<'_>,
    now: DateTime<Utc>,
    objective: &str,
    caller: &Caller,
    home_cwd: &Path,
) -> Result<RunRow> {
    create_with_budget(tx, now, objective, caller, home_cwd, None)
}

/// `create` with a soft budget in USD (`run-create --budget-usd`).
pub fn create_with_budget(
    tx: &Tx<'_>,
    now: DateTime<Utc>,
    objective: &str,
    caller: &Caller,
    home_cwd: &Path,
    budget_usd: Option<f64>,
) -> Result<RunRow> {
    let row = RunRow {
        id: RunId::new(),
        objective: objective.to_owned(),
        coordinator_terminal_id: caller.terminal_id.clone(),
        coordinator_handle: caller.terminal_id.clone(),
        consumer_generation: 1,
        home_cwd: home_cwd.display().to_string(),
        depth: 0,
        budget_usd,
        created_at: now,
        updated_at: now,
    };
    runs::insert(tx, &row)?;
    append_event(tx, &row.id, now, "run.created", &caller.terminal_id)?;
    Ok(row)
}

/// Binds `caller` to `run` as coordinator, bumps `consumer_generation`, and
/// fences every outstanding Delivery of an older generation.
pub fn use_run(tx: &Tx<'_>, now: DateTime<Utc>, run: &RunId, caller: &Caller) -> Result<RunRow> {
    use_run_fencing(tx, now, run, caller).map(|(row, _)| row)
}

/// `use_run`, also returning how many outstanding Deliveries it fenced.
pub fn use_run_fencing(
    tx: &Tx<'_>,
    now: DateTime<Utc>,
    run: &RunId,
    caller: &Caller,
) -> Result<(RunRow, u32)> {
    let Some(mut row) = runs::get(tx, run)? else {
        return Err(PodError::usage(format!("Run {run} was not found")));
    };
    row.consumer_generation += 1;
    row.coordinator_terminal_id = caller.terminal_id.clone();
    row.coordinator_handle = caller.terminal_id.clone();
    row.updated_at = now;
    runs::update_coordinator_terminal_id(tx, run, &caller.terminal_id)?;
    runs::update_coordinator_handle(tx, run, &caller.terminal_id)?;
    runs::update_consumer_generation(tx, run, row.consumer_generation)?;
    runs::update_updated_at(tx, run, now)?;
    let fenced = deliveries::fence_below_generation(tx, run, row.consumer_generation)?;
    append_event(tx, run, now, "run.used", &caller.terminal_id)?;
    Ok((row, fenced))
}

/// Task counts by status and worker counts by `terminal_state` for one Run,
/// and its number of active Dispatches.
pub struct RunCounts {
    pub tasks: BTreeMap<String, u32>,
    pub workers: BTreeMap<String, u32>,
    pub active_workers: u32,
}

pub fn counts(tx: &Tx<'_>, run: &RunId) -> Result<RunCounts> {
    let tasks = tasks::count_by_status(tx, run)?.into_iter().collect();
    let mut workers = BTreeMap::new();
    let mut active_workers = 0;
    for dispatch in dispatches::list_by_run(tx, run)? {
        if matches!(dispatch.status.as_str(), "pending" | "dispatched") {
            active_workers += 1;
        }
        if let Some(worker) = workers::get(tx, &dispatch.id)? {
            *workers.entry(worker.terminal_state).or_insert(0) += 1;
        }
    }
    Ok(RunCounts {
        tasks,
        workers,
        active_workers,
    })
}

/// What the Run's workers reported spending: the sum of the cost of each
/// distinct Claude session its Dispatches matched (a reused pane carries its
/// session to the next Dispatch, which must not count twice). Dispatches with
/// no matched session cost nothing.
pub fn spent_usd(tx: &Tx<'_>, usage: &UsageStore, run: &RunId) -> Result<f64> {
    let mut sessions = std::collections::BTreeSet::new();
    for dispatch in dispatches::list_by_run(tx, run)? {
        if let Some(worker) = workers::get(tx, &dispatch.id)?
            && let Some(session) = worker.session_id
        {
            sessions.insert(session);
        }
    }
    Ok(sessions
        .iter()
        .filter_map(|session| usage.load_session(session))
        .filter_map(|record| record.cost_usd)
        .sum())
}

pub fn bound_run(tx: &Tx<'_>, caller: &Caller) -> Result<Option<RunRow>> {
    runs::bound_to_terminal(tx, &caller.terminal_id)
}

pub fn get(tx: &Tx<'_>, run: &RunId) -> Result<Option<RunRow>> {
    runs::get(tx, run)
}

fn append_event(
    tx: &Tx<'_>,
    run: &RunId,
    now: DateTime<Utc>,
    kind: &str,
    actor: &str,
) -> Result<()> {
    events::append(
        tx,
        &events::EventRow {
            sequence: None,
            run_id: run.clone(),
            at: now,
            r#type: kind.to_owned(),
            task_id: None,
            dispatch_id: None,
            actor: actor.to_owned(),
            detail: Some(json!({})),
        },
    )
}
