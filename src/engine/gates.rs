use chrono::{DateTime, Utc};

use crate::engine::Engine;
use crate::engine::tasks::{append_event, task_not_found, unmet_dependencies};
use crate::error::{ErrorCode, PodError, Result};
use crate::ids::{GateId, MessageId, RunId, TaskId};
use crate::store::Tx;
use crate::store::rows::{dispatches, gates, messages, runs, tasks, workers};

pub use crate::store::rows::gates::GateRow;
pub use crate::store::rows::messages::MessageRow;

#[allow(clippy::too_many_arguments)]
pub fn create(
    tx: &Tx<'_>,
    now: DateTime<Utc>,
    run: &RunId,
    audience: &str,
    task: Option<&TaskId>,
    question: &str,
    options: &[String],
    evidence: Option<&str>,
    recommendation: Option<&str>,
    worker_live: bool,
) -> Result<GateRow> {
    if !matches!(audience, "coordinator" | "user") {
        return Err(PodError::usage("--for must be coordinator or user"));
    }
    if audience == "coordinator" && task.is_none() {
        return Err(PodError::usage("a coordinator gate requires --task"));
    }
    if audience == "user"
        && (evidence.is_none()
            || recommendation.is_none()
            || options.is_empty()
            || !options
                .iter()
                .any(|option| Some(option.as_str()) == recommendation))
    {
        return Err(PodError::new(
            ErrorCode::GateEvidenceRequired,
            "a user gate requires evidence, nonempty options, and a recommendation from those options",
            &["Resend with an evidence reference and a recommendation taken from the options."],
        ));
    }
    if runs::get(tx, run)?.is_none() {
        return Err(PodError::usage(format!("Run {run} was not found")));
    }
    if let Some(task) = task {
        if !tasks::get(tx, task)?.is_some_and(|row| row.run_id == *run) {
            return Err(task_not_found(task));
        }
        if worker_live {
            return Err(PodError::new(
                ErrorCode::TaskGuardRefused,
                format!("Task {task} has a live supervised worker"),
                &[
                    "Inspect the Task and its worker first; retry once the worker is no longer live.",
                ],
            )
            .with_data("guard", "worker_live"));
        }
    }
    let row = GateRow {
        id: GateId::new(),
        run_id: run.clone(),
        task_id: task.cloned(),
        audience: audience.to_owned(),
        question: question.to_owned(),
        options: options.to_vec(),
        evidence: evidence.map(str::to_owned),
        recommendation: recommendation.map(str::to_owned),
        status: "pending".to_owned(),
        resolution: None,
        created_at: now,
        resolved_at: None,
    };
    gates::insert(tx, &row)?;
    if let Some(task) = task {
        block_task(tx, task, now)?;
    }
    append_event(
        tx,
        run,
        now,
        "gate.created",
        task,
        None,
        "pod",
        serde_json::json!({"gate_id": row.id, "audience": audience}),
    )?;
    Ok(row)
}

/// Tells the person a new `user` gate needs a decision: the Run objective as
/// title, the question as body. A `coordinator` gate is the coordinator's own
/// business and notifies nobody. Called after the commit, never on a replay.
pub fn notify_created(engine: &Engine, gate: &GateRow) {
    if gate.audience != "user" {
        return;
    }
    let Ok(Some(run)) = engine.store.read(|tx| runs::get(tx, &gate.run_id)) else {
        return;
    };
    engine.notify(&run.objective, &gate.question);
}

pub fn resolve(
    tx: &Tx<'_>,
    now: DateTime<Utc>,
    gate: &GateId,
    resolution: &str,
) -> Result<(GateRow, Option<MessageRow>)> {
    let mut row = gates::get(tx, gate)?
        .ok_or_else(|| PodError::usage(format!("Gate {gate} was not found")))?;
    if row.status == "resolved" {
        return Ok((row, None));
    }
    row.status = "resolved".to_owned();
    row.resolution = Some(resolution.to_owned());
    row.resolved_at = Some(now);
    gates::update_status(tx, gate, "resolved")?;
    gates::update_resolution(tx, gate, Some(resolution))?;
    gates::update_resolved_at(tx, gate, Some(now))?;
    if let Some(task_id) = row.task_id.as_ref() {
        release_task(tx, task_id)?;
    }
    let message = if row.audience == "user" {
        Some(insert_decision_message(tx, now, &row, resolution)?)
    } else {
        None
    };
    append_event(
        tx,
        &row.run_id,
        now,
        "gate.resolved",
        row.task_id.as_ref(),
        None,
        "pod",
        serde_json::json!({"gate_id": gate, "resolution": resolution}),
    )?;
    Ok((row, message))
}

pub fn get(tx: &Tx<'_>, gate: &GateId) -> Result<Option<GateRow>> {
    gates::get(tx, gate)
}

pub fn list(tx: &Tx<'_>, run: &RunId, task: Option<&TaskId>) -> Result<Vec<GateRow>> {
    gates::list_by_run(tx, run, task)
}

/// State machine T12 (D6): only a `pending`, `ready`, or `dispatched` Task is
/// blocked, completing its active Dispatch, whose worker becomes
/// `reclaimable`. A `blocked` Task stays blocked; a `completed` or `failed`
/// Task is left as it is, so resolving the gate can never reopen settled work.
fn block_task(tx: &Tx<'_>, task: &TaskId, now: DateTime<Utc>) -> Result<()> {
    let Some(row) = tasks::get(tx, task)? else {
        return Ok(());
    };
    if !matches!(row.status.as_str(), "pending" | "ready" | "dispatched") {
        return Ok(());
    }
    tasks::update_status(tx, task, "blocked")?;
    tasks::update_completed_at(tx, task, None)?;
    if let Some(active) = dispatches::active_for_task(tx, task)? {
        if workers::get(tx, &active.id)?.is_some_and(|worker| worker.terminal_state == "active") {
            workers::update_terminal_state(tx, &active.id, "reclaimable")?;
            workers::update_updated_at(tx, &active.id, now)?;
        }
        dispatches::update_status(tx, &active.id, "completed")?;
        dispatches::update_completed_at(tx, &active.id, Some(now))?;
    }
    Ok(())
}

/// State machine T13/T19: only a `blocked` Task leaves `blocked`, and not
/// while another gate on it is still pending; it goes to `ready` only when
/// every dependency is `completed`, otherwise to `pending`.
fn release_task(tx: &Tx<'_>, task: &TaskId) -> Result<()> {
    let row = tasks::get(tx, task)?.ok_or_else(|| task_not_found(task))?;
    if row.status != "blocked" || gates::has_pending_for_task(tx, task)? {
        return Ok(());
    }
    let status = if unmet_dependencies(tx, &row.deps)?.is_empty() {
        "ready"
    } else {
        "pending"
    };
    tasks::update_status(tx, task, status)?;
    tasks::update_completed_at(tx, task, None)
}

fn insert_decision_message(
    tx: &Tx<'_>,
    now: DateTime<Utc>,
    gate: &GateRow,
    resolution: &str,
) -> Result<MessageRow> {
    let payload = serde_json::json!({"gate_id": gate.id, "question": gate.question, "options": gate.options, "evidence": gate.evidence, "recommendation": gate.recommendation, "resolution": resolution});
    let mut row = MessageRow {
        sequence: None,
        id: MessageId::new(),
        run_id: gate.run_id.clone(),
        mailbox: format!("run:{}", gate.run_id),
        from_handle: "pod".to_owned(),
        subject: gate.question.clone(),
        body: Some(resolution.to_owned()),
        r#type: "decision_gate".to_owned(),
        priority: "normal".to_owned(),
        thread_id: None,
        payload: Some(payload),
        read: false,
        created_at: now,
        delivered_at: None,
        sender_terminal_id: None,
        rejected_reason: None,
        evidence: gate.evidence.clone(),
        recommendation: gate.recommendation.clone(),
    };
    row.sequence = Some(messages::insert(tx, &row)?);
    Ok(row)
}
