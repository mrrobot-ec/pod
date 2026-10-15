use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{ErrorCode, PodError, Result};
use crate::ids::{DispatchId, RunId, TaskId};
use crate::store::Tx;
use crate::store::rows::{dispatches, events, observations, runs, tasks, workers};

pub use crate::store::rows::dispatches::DispatchRow;
pub use crate::store::rows::tasks::TaskRow;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    Succeeded,
    Failed,
}

impl Outcome {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            _ => Err(PodError::usage("--outcome must be succeeded or failed")),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Settlement {
    pub task_status: String,
    pub dispatch_status: String,
    pub failure_count: u32,
    pub promoted: Vec<TaskId>,
}

#[allow(clippy::too_many_arguments)]
pub fn create(
    tx: &Tx<'_>,
    now: DateTime<Utc>,
    run: &RunId,
    spec: &str,
    title: Option<&str>,
    display: Option<&str>,
    deps: &[TaskId],
    parent: Option<&TaskId>,
    creator: &str,
) -> Result<TaskRow> {
    if runs::get(tx, run)?.is_none() {
        return Err(PodError::usage(format!("Run {run} was not found")));
    }
    for dependency in deps {
        ensure_task_in_run(tx, dependency, run)?;
    }
    if let Some(parent) = parent {
        ensure_task_in_run(tx, parent, run)?;
    }
    let status = if unmet_dependencies(tx, deps)?.is_empty() {
        "ready"
    } else {
        "pending"
    };
    let row = TaskRow {
        id: TaskId::new(),
        run_id: run.clone(),
        parent_id: parent.cloned(),
        title: title.map(str::to_owned),
        display_name: display.map(str::to_owned),
        spec: spec.to_owned(),
        status: status.to_owned(),
        deps: deps.to_vec(),
        result: None,
        created_by_terminal_id: creator.to_owned(),
        created_at: now,
        completed_at: None,
    };
    tasks::insert(tx, &row)?;
    append_event(
        tx,
        &row.run_id,
        now,
        "task.created",
        Some(&row.id),
        None,
        creator,
        serde_json::json!({"status": status}),
    )?;
    Ok(row)
}

/// Promotes each `pending` dependant of `completed` whose dependencies are now
/// all `completed` (state machine T3).
pub fn promote_dependants(
    tx: &Tx<'_>,
    now: DateTime<Utc>,
    completed: &TaskId,
) -> Result<Vec<TaskId>> {
    let mut promoted = Vec::new();
    for task in tasks::dependants(tx, completed)? {
        if task.status == "pending" && unmet_dependencies(tx, &task.deps)?.is_empty() {
            tasks::update_status(tx, &task.id, "ready")?;
            append_event(
                tx,
                &task.run_id,
                now,
                "task.ready",
                Some(&task.id),
                None,
                "pod",
                serde_json::json!({"dependency": completed}),
            )?;
            promoted.push(task.id);
        }
    }
    Ok(promoted)
}

/// The single place that settles a Dispatch on a worker report (spec 7.5):
/// `failure_count` is cumulative per Task; a failure below 3 returns the Task
/// to `ready`, the third breaks the circuit and fails it; `denied` and
/// `canceled` fail the Task directly without counting; success completes it
/// and promotes its dependants.
pub fn settle_dispatch(
    tx: &Tx<'_>,
    now: DateTime<Utc>,
    dispatch: &DispatchId,
    outcome: Outcome,
    reason: &str,
    target: Option<&str>,
) -> Result<Settlement> {
    let attempt = dispatches::get(tx, dispatch)?.ok_or_else(|| {
        PodError::new(
            ErrorCode::InactiveDispatch,
            format!("Dispatch {dispatch} was not found"),
            &["Do not retry from an inactive Dispatch."],
        )
    })?;
    if !matches!(attempt.status.as_str(), "pending" | "dispatched") {
        return Err(PodError::new(
            ErrorCode::InactiveDispatch,
            format!("Dispatch {dispatch} is already {}", attempt.status),
            &["Do not retry or send another lifecycle message from the inactive Dispatch."],
        ));
    }
    let task = tasks::get(tx, &attempt.task_id)?.ok_or_else(|| task_not_found(&attempt.task_id))?;
    let current_count = dispatches::max_failure_count_for_task(tx, &task.id)?;
    let (dispatch_status, task_status, failure_count) = match outcome {
        Outcome::Succeeded => ("completed", "completed", current_count),
        Outcome::Failed if matches!(reason, "denied" | "canceled") => {
            ("failed", "failed", current_count)
        }
        Outcome::Failed => {
            let next = current_count + 1;
            if next >= 3 {
                ("circuit_broken", "failed", next)
            } else {
                ("failed", "ready", next)
            }
        }
    };
    dispatches::update_status(tx, dispatch, dispatch_status)?;
    dispatches::update_failure_count(tx, dispatch, failure_count)?;
    if outcome == Outcome::Failed {
        dispatches::update_last_failure(tx, dispatch, Some(reason))?;
    }
    dispatches::update_termination_reason(tx, dispatch, Some(reason))?;
    dispatches::update_termination_target(tx, dispatch, target)?;
    dispatches::update_completed_at(tx, dispatch, Some(now))?;
    tasks::update_status(tx, &task.id, task_status)?;
    tasks::update_completed_at(tx, &task.id, (task_status == "completed").then_some(now))?;
    // W5/W6 and R2: a ready worker records the outcome and its pane becomes
    // reclaimable; any later worker state (stopping, abandoned) is kept.
    if let Some(worker) = workers::get(tx, dispatch)? {
        if worker.state == "ready" {
            let state = match outcome {
                Outcome::Succeeded => "succeeded",
                Outcome::Failed => "failed",
            };
            workers::update_state(tx, dispatch, state)?;
        }
        if worker.terminal_state == "active" {
            workers::update_terminal_state(tx, dispatch, "reclaimable")?;
        }
        workers::update_updated_at(tx, dispatch, now)?;
    }
    append_event(
        tx,
        &task.run_id,
        now,
        if outcome == Outcome::Succeeded {
            "dispatch.completed"
        } else {
            "dispatch.failed"
        },
        Some(&task.id),
        Some(dispatch),
        attempt.assignee_handle.as_deref().unwrap_or("pod"),
        serde_json::json!({"termination_reason": reason, "termination_target": target, "dispatch_status": dispatch_status, "task_status": task_status}),
    )?;
    let promoted = if outcome == Outcome::Succeeded {
        promote_dependants(tx, now, &task.id)?
    } else {
        Vec::new()
    };
    Ok(Settlement {
        task_status: task_status.to_owned(),
        dispatch_status: dispatch_status.to_owned(),
        failure_count,
        promoted,
    })
}

/// The settlement an already settled Dispatch reports on a duplicate
/// `worker_done` (nothing is promoted again).
pub fn settlement_of(tx: &Tx<'_>, dispatch: &DispatchId) -> Result<Settlement> {
    let attempt =
        dispatches::get(tx, dispatch)?.ok_or_else(|| PodError::store("Dispatch disappeared"))?;
    let task = tasks::get(tx, &attempt.task_id)?.ok_or_else(|| task_not_found(&attempt.task_id))?;
    Ok(Settlement {
        task_status: task.status,
        dispatch_status: attempt.status,
        failure_count: attempt.failure_count,
        promoted: Vec::new(),
    })
}

pub fn update_status(
    tx: &Tx<'_>,
    now: DateTime<Utc>,
    task: &TaskId,
    status: &str,
    result: Option<Value>,
    worker_live: bool,
) -> Result<TaskRow> {
    if !matches!(
        status,
        "pending" | "ready" | "dispatched" | "completed" | "failed" | "blocked"
    ) {
        return Err(PodError::usage(format!("invalid Task status {status}")));
    }
    let mut row = tasks::get(tx, task)?.ok_or_else(|| task_not_found(task))?;
    let active = dispatches::active_for_task(tx, task)?;
    let guard = if status == "dispatched" && active.is_none() {
        Some(("active_dispatch_required", "requires an active Dispatch"))
    } else if status != "dispatched" && active.is_some() {
        Some(("dispatch_active", "is refused while a Dispatch is active"))
    } else if matches!(status, "completed" | "failed") && worker_live {
        Some((
            "worker_live",
            "is refused while a supervised worker is live",
        ))
    } else {
        None
    };
    if let Some((guard, explanation)) = guard {
        return Err(PodError::new(
            ErrorCode::TaskGuardRefused,
            format!("task-update of Task {task} to {status} {explanation}"),
            &["Inspect the Task and its worker first; retry once the guard holds."],
        )
        .with_data("guard", guard));
    }
    let completed_at = (status == "completed").then_some(now);
    tasks::update_status(tx, task, status)?;
    tasks::update_completed_at(tx, task, completed_at)?;
    if result.is_some() {
        tasks::update_result(tx, task, result.as_ref())?;
        row.result = result;
    }
    row.status = status.to_owned();
    row.completed_at = completed_at;
    append_event(
        tx,
        &row.run_id,
        now,
        "task.updated",
        Some(task),
        None,
        "pod",
        serde_json::json!({"status": status}),
    )?;
    Ok(row)
}

pub fn startable(tx: &Tx<'_>, task: &TaskId, retry_of: Option<&DispatchId>) -> Result<()> {
    let row = tasks::get(tx, task)?.ok_or_else(|| task_not_found(task))?;
    let unmet = unmet_dependencies(tx, &row.deps)?;
    let retry_valid = match retry_of {
        Some(dispatch) if matches!(row.status.as_str(), "failed" | "blocked") => {
            dispatches::get(tx, dispatch)?.is_some_and(|attempt| {
                attempt.task_id == *task
                    && matches!(attempt.status.as_str(), "failed" | "circuit_broken")
            })
        }
        _ => false,
    };
    if (row.status == "ready" && retry_of.is_none() && unmet.is_empty())
        || (retry_valid && unmet.is_empty())
    {
        return Ok(());
    }
    Err(PodError::new(
        ErrorCode::TaskNotStartable,
        format!(
            "Task {task} is {} and has unmet dependencies: {}",
            row.status,
            unmet
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        &["Finish the unmet dependencies, or pass --retry-of for a failed or blocked Task."],
    )
    .with_data("status", row.status)
    .with_data("unmetDependencies", serde_json::json!(unmet)))
}

pub fn get(tx: &Tx<'_>, task: &TaskId) -> Result<Option<TaskRow>> {
    tasks::get(tx, task)
}

/// The Run's Tasks, newest first, optionally of one status.
pub fn list_by_run(tx: &Tx<'_>, run: &RunId, status: Option<&str>) -> Result<Vec<TaskRow>> {
    let mut rows = tasks::list_by_run(tx, run, status)?;
    rows.reverse();
    Ok(rows)
}

pub fn waiting_on_failed_dependency(tx: &Tx<'_>, task: &TaskId) -> Result<bool> {
    Ok(failed_dependency(tx, task)?.is_some())
}

/// The first direct dependency of `task` that is `failed`, if any.
pub fn failed_dependency(tx: &Tx<'_>, task: &TaskId) -> Result<Option<TaskId>> {
    let row = tasks::get(tx, task)?.ok_or_else(|| task_not_found(task))?;
    for dependency in row.deps {
        if tasks::get(tx, &dependency)?.is_some_and(|row| row.status == "failed") {
            return Ok(Some(dependency));
        }
    }
    Ok(None)
}

/// Whether a supervised worker of `task` is live: an unreleased worker whose
/// latest observation says `live`.
pub fn has_live_worker(tx: &Tx<'_>, task: &TaskId) -> Result<bool> {
    for attempt in dispatches::list_by_task(tx, task)? {
        if is_live_worker(tx, &attempt.id)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether the worker of `dispatch` is unreleased and last observed `live`.
pub fn is_live_worker(tx: &Tx<'_>, dispatch: &DispatchId) -> Result<bool> {
    let Some(worker) = workers::get(tx, dispatch)? else {
        return Ok(false);
    };
    Ok(worker.terminal_state != "released"
        && observations::get(tx, dispatch)?
            .is_some_and(|observation| observation.liveness == "live"))
}

/// The closure of `task`'s newest ended Dispatch, if it has one.
pub fn latest_closure(tx: &Tx<'_>, task: &TaskId) -> Result<Option<DispatchRow>> {
    Ok(dispatches::list_by_task(tx, task)?
        .into_iter()
        .rev()
        .find(|attempt| !matches!(attempt.status.as_str(), "pending" | "dispatched")))
}

fn ensure_task_in_run(tx: &Tx<'_>, task: &TaskId, run: &RunId) -> Result<()> {
    if tasks::get(tx, task)?.is_some_and(|row| row.run_id == *run) {
        Ok(())
    } else {
        Err(task_not_found(task))
    }
}

pub(crate) fn unmet_dependencies(tx: &Tx<'_>, deps: &[TaskId]) -> Result<Vec<TaskId>> {
    let mut unmet = Vec::new();
    for dependency in deps {
        if tasks::get(tx, dependency)?.is_none_or(|task| task.status != "completed") {
            unmet.push(dependency.clone());
        }
    }
    Ok(unmet)
}

pub(crate) fn task_not_found(task: &TaskId) -> PodError {
    PodError::new(
        ErrorCode::TaskNotFound,
        format!("Task {task} was not found in this Run"),
        &["Run pod task-list to see the Run's Tasks, then retry with a valid ID."],
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn append_event(
    tx: &Tx<'_>,
    run: &RunId,
    now: DateTime<Utc>,
    kind: &str,
    task: Option<&TaskId>,
    dispatch: Option<&DispatchId>,
    actor: &str,
    detail: Value,
) -> Result<()> {
    events::append(
        tx,
        &events::EventRow {
            sequence: None,
            run_id: run.clone(),
            at: now,
            r#type: kind.to_owned(),
            task_id: task.cloned(),
            dispatch_id: dispatch.cloned(),
            actor: actor.to_owned(),
            detail: Some(detail),
        },
    )
}
