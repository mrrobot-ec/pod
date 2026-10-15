//! Board export (spec section 12): `run.json` in herdr-dagr contract v3.
//! Reads with raw SQL over the transaction, so it depends on no store row
//! type. Also derives the herdr sidebar status of each worker workspace.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::error::{ErrorCode, PodError, Result};
use crate::ids::RunId;
use crate::paths::Paths;
use crate::store::Tx;
use crate::usage::UsageStore;

pub mod contract;

use contract::{
    Attempt, Board, Cause, DispatchView, EventCard, Liveness, Locator, Orchestrator, Outcome,
    Progress, Project, RunCard, TaskCard, TaskView,
};

struct DispatchData {
    id: String,
    task_id: String,
    status: String,
    retry_of: Option<String>,
    termination_reason: Option<String>,
    termination_target: Option<String>,
    assignee_handle: Option<String>,
    launch_requested: Option<String>,
    launch_effective: Option<String>,
    dispatched_at: Option<String>,
    last_heartbeat_at: Option<String>,
    last_phase: Option<String>,
    completed_at: Option<String>,
    created_at: String,
    pane_id: Option<String>,
    preamble_input: Option<String>,
    session_id: Option<String>,
}

struct TaskData {
    id: String,
    parent_id: Option<String>,
    title: Option<String>,
    display_name: Option<String>,
    status: String,
    deps: String,
    /// When the Task last changed: its latest event, else its creation.
    updated_at: String,
}

/// The board without worker costs.
pub fn export(tx: &Tx, run: &RunId, now: DateTime<Utc>) -> Result<Value> {
    export_inner(tx, run, now, None)
}

/// The board with each task's reported cost appended to its note: dagr's
/// contract has no cost field, and the note is free text.
pub fn export_with_costs(
    tx: &Tx,
    run: &RunId,
    now: DateTime<Utc>,
    usage: &UsageStore,
) -> Result<Value> {
    export_inner(tx, run, now, Some(usage))
}

fn export_inner(
    tx: &Tx,
    run: &RunId,
    now: DateTime<Utc>,
    usage: Option<&UsageStore>,
) -> Result<Value> {
    let conn = tx.0;
    let run_id = run.as_str();

    let run_card = conn
        .query_row(
            "SELECT objective, coordinator_terminal_id, coordinator_handle, created_at \
             FROM runs WHERE id = ?1",
            [run_id],
            |r| {
                Ok(RunCard {
                    id: run_id.to_owned(),
                    title: r.get(0)?,
                    started_at: r.get(3)?,
                    orchestrator: Orchestrator { agent: r.get(2)? },
                })
            },
        )
        .map_err(|error| match error {
            // A missing Run named by `--run` is a usage error (errors.md).
            rusqlite::Error::QueryReturnedNoRows => PodError::new(
                ErrorCode::Usage,
                format!("run {run_id} not found"),
                &["Run `pod run-list` to see known runs."],
            ),
            other => other.into(),
        })?;

    let tasks: Vec<TaskData> = conn
        .prepare_cached(
            "SELECT t.id, t.parent_id, t.title, t.display_name, t.status, t.deps, \
             COALESCE((SELECT MAX(e.at) FROM events e WHERE e.task_id = t.id), t.created_at) \
             FROM tasks t WHERE t.run_id = ?1 ORDER BY t.created_at, t.rowid",
        )?
        .query_map([run_id], |r| {
            Ok(TaskData {
                id: r.get(0)?,
                parent_id: r.get(1)?,
                title: r.get(2)?,
                display_name: r.get(3)?,
                status: r.get(4)?,
                deps: r.get(5)?,
                updated_at: r.get(6)?,
            })
        })?
        .collect::<std::result::Result<_, _>>()?;

    let dispatches: Vec<DispatchData> = conn
        .prepare_cached(
            "SELECT d.id, d.task_id, d.status, d.retry_of_dispatch_id, d.termination_reason, \
             d.termination_target, d.assignee_handle, d.launch_requested, \
             d.launch_effective, d.dispatched_at, d.last_heartbeat_at, d.last_phase, \
             d.completed_at, d.created_at, w.pane_id, w.preamble_input, w.session_id \
             FROM dispatches d LEFT JOIN workers w ON w.dispatch_id = d.id \
             WHERE d.run_id = ?1 ORDER BY d.created_at, d.rowid",
        )?
        .query_map([run_id], |r| {
            Ok(DispatchData {
                id: r.get(0)?,
                task_id: r.get(1)?,
                status: r.get(2)?,
                retry_of: r.get(3)?,
                termination_reason: r.get(4)?,
                termination_target: r.get(5)?,
                assignee_handle: r.get(6)?,
                launch_requested: r.get(7)?,
                launch_effective: r.get(8)?,
                dispatched_at: r.get(9)?,
                last_heartbeat_at: r.get(10)?,
                last_phase: r.get(11)?,
                completed_at: r.get(12)?,
                created_at: r.get(13)?,
                pane_id: r.get(14)?,
                preamble_input: r.get(15)?,
                session_id: r.get(16)?,
            })
        })?
        .collect::<std::result::Result<_, _>>()?;

    // Latest observation per dispatch: rows arrive oldest first, later ones win.
    let mut liveness: HashMap<String, (String, String)> = HashMap::new();
    let mut stmt = conn.prepare_cached(
        "SELECT o.dispatch_id, o.liveness, o.observed_at FROM observations o \
         JOIN dispatches d ON d.id = o.dispatch_id WHERE d.run_id = ?1 \
         ORDER BY o.observed_at, o.rowid",
    )?;
    for row in stmt.query_map([run_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })? {
        let (dispatch, value, at) = row?;
        liveness.insert(dispatch, (value, at));
    }

    // Dispatches ended by an accepted `worker_done`: only these are `evidence: reported`.
    // A fence (`worker-stop`, `canceled`) or a gate also stores an ending, but no report.
    let mut reported: HashSet<String> = HashSet::new();
    let mut stmt = conn.prepare_cached(
        "SELECT json_extract(payload, '$.dispatch_id') FROM messages \
         WHERE run_id = ?1 AND type = 'worker_done' AND rejected_reason IS NULL \
         AND json_valid(payload)",
    )?;
    for row in stmt.query_map([run_id], |r| r.get::<_, Option<String>>(0))? {
        if let Some(dispatch) = row? {
            reported.insert(dispatch);
        }
    }

    let by_id: HashMap<&str, &DispatchData> =
        dispatches.iter().map(|d| (d.id.as_str(), d)).collect();

    // The latest message each worker sent (`--dispatch-id` in its payload):
    // its subject becomes the task's `note`, and when it was sent counts as
    // output for the attempt's freshness (dagr's staleness banner).
    struct LastMessage {
        kind: String,
        subject: String,
        at: String,
    }
    let mut last_message: HashMap<String, LastMessage> = HashMap::new();
    let mut stmt = conn.prepare_cached(
        "SELECT json_extract(payload, '$.dispatch_id'), type, subject, created_at FROM messages \
         WHERE run_id = ?1 AND rejected_reason IS NULL AND json_valid(payload) \
         AND json_extract(payload, '$.dispatch_id') IS NOT NULL ORDER BY sequence",
    )?;
    for row in stmt.query_map([run_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    })? {
        let (dispatch, kind, subject, at) = row?;
        last_message.insert(dispatch, LastMessage { kind, subject, at });
    }

    // Pending questions of active dispatches, and where each one was routed.
    let mut thread_of: HashMap<String, Option<String>> = HashMap::new();
    let mut routed: Vec<(String, Option<String>, String)> = Vec::new(); // (message id, thread, mailbox)
    let mut stmt = conn.prepare_cached(
        "SELECT id, thread_id, mailbox FROM messages WHERE run_id = ?1 AND type = 'question'",
    )?;
    for row in stmt.query_map([run_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))? {
        let (id, thread, mailbox): (String, Option<String>, String) = row?;
        thread_of.insert(id.clone(), thread.clone());
        routed.push((id, thread, mailbox));
    }
    let mut unblock_by_task: HashMap<String, String> = HashMap::new();
    let mut stmt = conn.prepare_cached(
        "SELECT q.message_id, q.dispatch_id FROM questions q \
         WHERE q.run_id = ?1 AND q.status = 'pending' AND q.dispatch_id IS NOT NULL \
         ORDER BY q.created_at, q.rowid",
    )?;
    for row in stmt.query_map([run_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })? {
        let (message_id, dispatch_id) = row?;
        let Some(asker) = by_id.get(dispatch_id.as_str()) else {
            continue;
        };
        if !matches!(asker.status.as_str(), "pending" | "dispatched") {
            continue;
        }
        let thread = thread_of.get(&message_id).cloned().flatten();
        let specialist = routed
            .iter()
            .filter(|(id, t, _)| *id == message_id || (thread.is_some() && *t == thread))
            .find_map(|(_, _, mailbox)| mailbox.strip_prefix("dispatch:"))
            .map(|target| {
                by_id
                    .get(target)
                    .and_then(|d| d.assignee_handle.clone())
                    .unwrap_or_else(|| target.to_owned())
            });
        unblock_by_task.insert(
            asker.task_id.clone(),
            specialist.unwrap_or_else(|| "coordinator".into()),
        );
    }

    // Latest pending gate per task; a gate without a task becomes its own
    // `question` card below.
    struct GateData {
        id: String,
        audience: String,
        question: String,
        note: String,
    }
    let mut gates: HashMap<String, GateData> = HashMap::new();
    let mut untasked: Vec<GateData> = Vec::new();
    let mut stmt = conn.prepare_cached(
        "SELECT id, task_id, audience, question, options, evidence, recommendation FROM gates \
         WHERE run_id = ?1 AND status = 'pending' \
         AND (task_id IS NOT NULL OR audience = 'user') \
         ORDER BY created_at, rowid",
    )?;
    for row in stmt.query_map([run_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, Option<String>>(5)?,
            r.get::<_, Option<String>>(6)?,
        ))
    })? {
        let (id, task, audience, question, options, evidence, recommendation) = row?;
        let options = options.map(|raw| {
            serde_json::from_str::<Vec<String>>(&raw)
                .unwrap_or_else(|_| raw.split(',').map(|s| s.trim().to_owned()).collect())
        });
        let mut parts = Vec::new();
        if let Some(options) = options.filter(|options| !options.is_empty()) {
            parts.push(format!("options: {}", options.join(" | ")));
        }
        if let Some(recommendation) = recommendation.filter(|value| !value.is_empty()) {
            parts.push(format!("recommended: {recommendation}"));
        }
        if let Some(evidence) = evidence.filter(|value| !value.is_empty()) {
            parts.push(format!("evidence: {evidence}"));
        }
        let data = GateData {
            id,
            audience,
            question,
            note: parts.join(" · "),
        };
        match task {
            Some(task) => {
                gates.insert(task, data);
            }
            None => untasked.push(data),
        }
    }

    let mut attempts: HashMap<&str, Vec<Attempt>> = HashMap::new();
    let mut last_of_task: HashMap<&str, &DispatchData> = HashMap::new();
    for d in &dispatches {
        let list = attempts.entry(d.task_id.as_str()).or_default();
        let n = u32::try_from(list.len() + 1).unwrap_or(u32::MAX);
        // dagr: `initial` is for the first attempt only; a later attempt
        // names the one before it (`--retry-of`, or simply the previous
        // attempt when the Task was started again), else dagr draws it as
        // a separate node and keeps showing the first attempt's failure.
        let previous = by_id
            .get(d.retry_of.as_deref().unwrap_or(""))
            .copied()
            .or_else(|| last_of_task.get(d.task_id.as_str()).copied());
        let mut record = attempt(d, n, previous, &liveness, reported.contains(&d.id));
        if let (Some(live), Some(message)) = (record.liveness.as_mut(), last_message.get(&d.id))
            && live.last_output_at.as_deref() < Some(message.at.as_str())
        {
            live.last_output_at = Some(message.at.clone());
        }
        list.push(record);
        last_of_task.insert(d.task_id.as_str(), d);
    }
    // The newest message among a task's attempts is its note.
    let mut note_of_task: HashMap<&str, (&str, String)> = HashMap::new();
    for d in &dispatches {
        if let Some(message) = last_message.get(&d.id) {
            let note = format!("{}: {}", message.kind, message.subject);
            match note_of_task.get(d.task_id.as_str()) {
                Some((at, _)) if *at >= message.at.as_str() => {}
                _ => {
                    note_of_task.insert(d.task_id.as_str(), (message.at.as_str(), note));
                }
            }
        }
    }

    // What each task's workers reported spending, summed over its attempts'
    // distinct sessions (a retry in the same pane carries the session over).
    let mut sessions_of_task: HashMap<&str, BTreeSet<&str>> = HashMap::new();
    for d in &dispatches {
        if let Some(session) = d.session_id.as_deref() {
            sessions_of_task
                .entry(d.task_id.as_str())
                .or_default()
                .insert(session);
        }
    }
    let mut cost_of_task: HashMap<&str, f64> = HashMap::new();
    for (task, sessions) in &sessions_of_task {
        let costs: Vec<f64> = sessions
            .iter()
            .filter_map(|session| usage.and_then(|usage| usage.load_session(session)))
            .filter_map(|record| record.cost_usd)
            .collect();
        if !costs.is_empty() {
            cost_of_task.insert(task, costs.iter().sum());
        }
    }

    let mut projects = Vec::new();
    let mut seen_projects: HashMap<&str, ()> = HashMap::new();
    let titles: HashMap<&str, String> =
        tasks.iter().map(|t| (t.id.as_str(), title_of(t))).collect();
    let mut cards = Vec::with_capacity(tasks.len());
    for t in &tasks {
        if let Some(parent) = t.parent_id.as_deref()
            && seen_projects.insert(parent, ()).is_none()
        {
            projects.push(Project {
                id: parent.to_owned(),
                title: titles
                    .get(parent)
                    .cloned()
                    .unwrap_or_else(|| parent.to_owned()),
            });
        }
        let mut task_attempts = attempts.remove(t.id.as_str()).unwrap_or_default();
        let latest = task_attempts.last().map(|attempt| attempt.state);
        let state = contract::project_state(&t.status, latest, false);
        if contract::needs_coordinator_attempt(state, latest) {
            // The coordinator settled the Task (task-update, a gate) with no
            // worker attempt matching: that settlement is its own attempt,
            // a bare claim with no worker report behind it.
            let n = u32::try_from(task_attempts.len() + 1).unwrap_or(u32::MAX);
            let reference = task_attempts.last().map(|attempt| attempt.id.clone());
            task_attempts.push(Attempt {
                id: format!("{}·a{n}", t.id),
                n,
                cause: Cause {
                    kind: if reference.is_some() {
                        "followup"
                    } else {
                        "initial"
                    },
                    reference,
                },
                actor: Some(run_card.orchestrator.agent.clone()),
                model: None,
                locator: None,
                state,
                started_at: Some(t.updated_at.clone()),
                ended_at: Some(t.updated_at.clone()),
                outcome: Some(Outcome {
                    result: state,
                    evidence: "asserted",
                    reason: None,
                    receipt: Some(format!(
                        "Task status {} set by the coordinator without a worker attempt",
                        t.status
                    )),
                }),
                progress: None,
                liveness: None,
            });
        }
        let latest = task_attempts.last().map(|attempt| attempt.state);
        let any_working = task_attempts
            .iter()
            .any(|attempt| attempt.state == "working");
        let owner = task_attempts
            .iter()
            .rev()
            .find_map(|attempt| attempt.actor.clone());
        let mut card = TaskCard {
            id: t.id.clone(),
            title: title_of(t),
            kind: "impl",
            state: contract::project_state(&t.status, latest, any_working),
            deps: serde_json::from_str(&t.deps).unwrap_or_default(),
            project: t.parent_id.clone(),
            owner,
            unblock: None,
            criteria: None,
            note: None,
            attempts: task_attempts,
        };
        if card.state == "blocked" && t.status != "blocked" {
            // A working task whose attempt was lost: the coordinator must look.
            card.unblock = Some("coordinator".into());
        }
        if let Some((_, note)) = note_of_task.remove(t.id.as_str()) {
            card.note = Some(note);
        }
        if let Some(cost) = cost_of_task.get(t.id.as_str()) {
            card.note = Some(match card.note.take() {
                // "reported": the agent's own figure, which may differ from the bill.
                Some(note) => format!("{note} · ${cost:.2} reported"),
                None => format!("${cost:.2} reported"),
            });
        }
        if let Some(unblock) = unblock_by_task.remove(&t.id) {
            card.state = "blocked";
            card.unblock = Some(unblock);
        }
        if let Some(gate) = gates.remove(&t.id) {
            card.kind = "question";
            card.state = "blocked";
            card.unblock = Some(gate.audience.clone());
            card.owner = Some(gate.audience);
            card.criteria = Some(gate.question);
            card.note = (!gate.note.is_empty()).then_some(gate.note);
        }
        if card.state == "blocked" && card.unblock.is_none() {
            card.unblock = Some("coordinator".into());
        }
        cards.push(card);
    }
    // An untasked user gate has no Task and no attempt: dagr still renders
    // it as a blocked `question` the user must answer.
    for gate in untasked {
        cards.push(TaskCard {
            id: format!("gate:{}", gate.id),
            title: gate.question.chars().take(GATE_TITLE_MAX_CHARS).collect(),
            kind: "question",
            state: "blocked",
            deps: Vec::new(),
            project: None,
            owner: Some(gate.audience.clone()),
            unblock: Some(gate.audience),
            criteria: Some(gate.question),
            note: (!gate.note.is_empty()).then_some(gate.note),
            attempts: Vec::new(),
        });
    }

    // dagr's event vocabulary is closed and `detail` is a string: the pod
    // event name and its detail JSON travel inside the detail text.
    let known_tasks: HashSet<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
    let events: Vec<EventCard> = conn
        .prepare_cached(
            "SELECT at, type, task_id, dispatch_id, actor, detail FROM events \
             WHERE run_id = ?1 ORDER BY sequence",
        )?
        .query_map([run_id], |r| {
            let kind: String = r.get(1)?;
            let task: Option<String> = r.get(2)?;
            let attempt: Option<String> = r.get(3)?;
            let detail: Option<String> = r.get(4 + 1)?;
            Ok((
                kind,
                r.get::<_, String>(0)?,
                task,
                attempt,
                r.get::<_, String>(4)?,
                detail,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .map(|(kind, at, task, attempt, actor, detail)| {
            // A reference to a Task or Dispatch this export does not carry
            // would dangle (E171), so it becomes part of the note instead.
            let task = task.filter(|id| known_tasks.contains(id.as_str()));
            let attempt = attempt.filter(|id| by_id.contains_key(id.as_str()));
            let dagr_kind = contract::event_kind(&kind, task.is_some() && attempt.is_some());
            let detail = match detail.as_deref() {
                None | Some("") | Some("{}") | Some("null") => kind.clone(),
                Some(raw) => format!("{kind}: {raw}"),
            };
            EventCard {
                at,
                kind: dagr_kind,
                task,
                attempt,
                actor,
                detail,
            }
        })
        .collect();

    let board = Board {
        dagr: contract::CONTRACT_VERSION,
        generated_at: now.to_rfc3339(),
        run: run_card,
        projects,
        tasks: cards,
        events,
    };
    serde_json::to_value(board).map_err(|e| PodError::store(format!("board serialization: {e}")))
}

fn title_of(t: &TaskData) -> String {
    t.title
        .clone()
        .or_else(|| t.display_name.clone())
        .unwrap_or_else(|| t.id.clone())
}

/// dagr renders `model` verbatim in a 12-cell column and suggests a short
/// `model·effort` chip (`terra5.6·max`). pod builds one from the launch:
/// the vendor prefix goes, the family name comes first, the version follows
/// it, and the effort is appended when set (`gpt-5.6-terra` at `high` is
/// `terra5.6·high`; `claude-opus-5-5` is `opus5.5`). An unrecognised name
/// is shown as is.
fn model_of(raw: Option<&str>) -> Option<String> {
    let value: Value = serde_json::from_str(raw?).ok()?;
    let model = value.get("model")?.as_str()?;
    let effort = value.get("effort").and_then(Value::as_str);
    let chip = model_chip(model);
    Some(match effort {
        Some(effort) if !effort.is_empty() => format!("{chip}·{effort}"),
        _ => chip,
    })
}

fn model_chip(model: &str) -> String {
    let parts: Vec<&str> = model.split('-').collect();
    let is_version =
        |part: &str| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit() || c == '.');
    match parts.as_slice() {
        // gpt-5.6-terra, gpt-6.1-sol
        ["gpt", version, family] if is_version(version) => format!("{family}{version}"),
        // claude-opus-5-5, claude-sonnet-5-5, claude-fable-5-1
        ["claude", family, major, minor] if is_version(major) && is_version(minor) => {
            format!("{family}{major}.{minor}")
        }
        ["claude", family, version] if is_version(version) => format!("{family}{version}"),
        _ => model.to_owned(),
    }
}

fn attempt(
    d: &DispatchData,
    n: u32,
    previous: Option<&DispatchData>,
    liveness: &HashMap<String, (String, String)>,
    reported: bool,
) -> Attempt {
    let observed = liveness.get(&d.id);
    let exited = observed.is_some_and(|(l, _)| l == "exited");
    let state = match d.status.as_str() {
        "completed" => "done",
        "failed" | "circuit_broken" => "failed",
        _ if exited => "lost",
        "dispatched" => "working",
        _ => "queued",
    };
    // dagr: `result` equals the state; an ending pod recorded without an
    // accepted worker_done (a fence, a gate, a circuit break) is inferred,
    // never the worker's own report.
    let outcome = matches!(state, "done" | "failed").then(|| Outcome {
        result: state,
        evidence: if reported { "reported" } else { "heuristic" },
        reason: d.termination_reason.clone(),
        receipt: Some(match (&reported, &d.termination_target) {
            (true, Some(target)) => format!("worker_done accepted; target {target}"),
            (true, None) => "worker_done accepted".into(),
            (false, Some(target)) => format!("ending recorded by pod; target {target}"),
            (false, None) => "ending recorded by pod".into(),
        }),
    });
    let started_at = d
        .dispatched_at
        .clone()
        .or_else(|| Some(d.created_at.clone()));
    let ended_at = d.completed_at.clone().or_else(|| {
        // A settled or lost attempt ends when pod saw it end; with no
        // observation at all, its start is the only time on record.
        (state != "working" && state != "queued")
            .then(|| {
                observed
                    .map(|(_, at)| at.clone())
                    .or_else(|| started_at.clone())
            })
            .flatten()
    });
    let live_liveness = (state == "working").then(|| Liveness {
        prompt_acknowledged: d.preamble_input.as_deref() == Some("delivered"),
        last_output_at: d
            .last_heartbeat_at
            .clone()
            .or_else(|| observed.map(|(_, at)| at.clone()))
            .or_else(|| started_at.clone()),
        queued_input: 0,
    });
    let cause = match previous {
        None => Cause {
            kind: "initial",
            reference: None,
        },
        Some(p) => Cause {
            kind: if p.status == "completed" {
                "followup"
            } else {
                "sent_back"
            },
            reference: Some(p.id.clone()),
        },
    };
    // A locator is a live runtime address: only a working attempt has one.
    let locator = (state == "working")
        .then(|| {
            (d.pane_id.is_some() || d.assignee_handle.is_some()).then(|| Locator {
                pane: d.pane_id.clone(),
                agent: d.assignee_handle.clone(),
            })
        })
        .flatten();
    Attempt {
        id: d.id.clone(),
        n,
        cause,
        actor: d.assignee_handle.clone(),
        model: model_of(d.launch_effective.as_deref())
            .or_else(|| model_of(d.launch_requested.as_deref())),
        locator,
        state,
        started_at,
        ended_at,
        outcome,
        progress: d.last_phase.clone().map(|note| Progress { note }),
        liveness: live_liveness,
    }
}

/// Writes `run.json` atomically (temporary file, then rename) as `0600`.
/// Readers see the old file or the new one, never a partial write.
pub fn write(paths: &Paths, run: &RunId, value: &Value) -> Result<PathBuf> {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let dir = paths.board_dir.join(run.as_str());
    create_private_dir(&dir)?;
    let target = dir.join("run.json");
    // Unique per call, so two writers in one process never share a temp file.
    let temp = dir.join(format!(
        ".run.json.{}.{}.tmp",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let io = |what: &str, e: std::io::Error| PodError::store(format!("board {what}: {e}"));
    let bytes =
        serde_json::to_vec(value).map_err(|e| PodError::store(format!("board encode: {e}")))?;
    // A leftover from a crashed process with the same pid would keep its old
    // mode through `open`, so remove it and always create the file fresh.
    let _ = fs::remove_file(&temp);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let written = options
        .open(&temp)
        .map_err(|e| io("temp file", e))
        .and_then(|mut file| file.write_all(&bytes).map_err(|e| io("write", e)))
        .and_then(|()| fs::rename(&temp, &target).map_err(|e| io("rename", e)));
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written.map(|()| target)
}

/// Removes a deleted Run's board directory (`reset`). A missing directory is
/// not an error.
pub fn remove(paths: &Paths, run: &RunId) -> Result<()> {
    match fs::remove_dir_all(paths.board_dir.join(run.as_str())) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(PodError::store(format!("board remove: {error}"))),
    }
}

/// Longest sidebar status pod writes; longer joins end in `…`.
const SIDEBAR_MAX_CHARS: usize = 120;
const GATE_TITLE_MAX_CHARS: usize = 80;
const DECISION_QUESTION_MAX_CHARS: usize = 40;

/// The herdr sidebar status of every workspace that holds or held a worker
/// of `run` (spec 12): the active Dispatches' `sidebar_status`, oldest first,
/// joined with `, `; `None` clears a workspace whose workers have all ended.
pub fn sidebar_statuses(tx: &Tx, run: &RunId) -> Result<Vec<(String, Option<String>)>> {
    let mut statement = tx.0.prepare(
        "SELECT w.workspace_id, d.status, d.last_phase, t.id, t.title, t.display_name
         FROM workers w
         JOIN dispatches d ON d.id = w.dispatch_id
         JOIN tasks t ON t.id = d.task_id
         WHERE d.run_id = ?1 AND w.workspace_id IS NOT NULL
         ORDER BY d.created_at, d.rowid",
    )?;
    let rows = statement.query_map([run.as_str()], |row| {
        let workspace: String = row.get(0)?;
        let dispatch = DispatchView {
            status: row.get(1)?,
            last_phase: row.get(2)?,
        };
        let task = TaskView {
            id: row.get(3)?,
            title: row.get(4)?,
            display_name: row.get(5)?,
        };
        Ok((workspace, task, dispatch))
    })?;
    let mut statuses: Vec<(String, Vec<String>)> = Vec::new();
    for row in rows {
        let (workspace, task, dispatch) = row?;
        let index = match statuses.iter().position(|(id, _)| *id == workspace) {
            Some(index) => index,
            None => {
                statuses.push((workspace, Vec::new()));
                statuses.len() - 1
            }
        };
        if matches!(dispatch.status.as_str(), "pending" | "dispatched") {
            statuses[index].1.push(sidebar_status(&task, &dispatch));
        }
    }
    // While a user gate is open, the oldest one leads every workspace status.
    let decision: Option<String> =
        tx.0.query_row(
            "SELECT question FROM gates WHERE run_id = ?1 AND status = 'pending' \
             AND audience = 'user' ORDER BY created_at, rowid LIMIT 1",
            [run.as_str()],
            |row| row.get(0),
        )
        .map(Some)
        .or_else(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    Ok(statuses
        .into_iter()
        .map(|(workspace, active)| {
            let mut joined = active.join(", ");
            if let Some(question) = &decision {
                let prefix = format!(
                    "DECISION · {}",
                    question
                        .chars()
                        .take(DECISION_QUESTION_MAX_CHARS)
                        .collect::<String>()
                );
                joined = if joined.is_empty() {
                    prefix
                } else {
                    format!("{prefix} · {joined}")
                };
            }
            let status = if joined.chars().count() > SIDEBAR_MAX_CHARS {
                let mut cut: String = joined.chars().take(SIDEBAR_MAX_CHARS - 1).collect();
                cut.push('…');
                cut
            } else {
                joined
            };
            (workspace, (!status.is_empty()).then_some(status))
        })
        .collect())
}

/// Short status for the herdr workspace sidebar, e.g. `T12 · implementing`.
pub fn sidebar_status(task: &TaskView, dispatch: &DispatchView) -> String {
    let label = task
        .display_name
        .as_deref()
        .or(task.title.as_deref())
        .unwrap_or(&task.id);
    let phase = dispatch
        .last_phase
        .as_deref()
        .unwrap_or(match dispatch.status.as_str() {
            "pending" => "starting",
            "dispatched" => "working",
            other => other,
        });
    format!("{label} · {phase}")
}

fn create_private_dir(path: &std::path::Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .map_err(|e| PodError::store(format!("could not create {}: {e}", path.display())))?;
    // `DirBuilder::mode` applies only to directories it creates; force it on an existing one.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|e| {
            PodError::store(format!(
                "could not set permissions on {}: {e}",
                path.display()
            ))
        })?;
    }
    Ok(())
}
