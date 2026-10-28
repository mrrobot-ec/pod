//! Stall detection: a dispatched worker that makes no progress for
//! `stall_minutes` and never reports gets one `escalation` from pod per
//! stall episode (seen live 2026-10-22: a worker idled behind a hung
//! background shell for 11 minutes and only `stale`, which asks nothing of
//! the coordinator, said so).

use chrono::{DateTime, Duration, SecondsFormat, SubsecRound, Utc};

use crate::engine::observe::{self, screen_tail};
use crate::engine::{Engine, transcripts};
use crate::error::Result;
use crate::ids::{DispatchId, MessageId, RunId, TaskId};
use crate::store::rows::{dispatches, messages, worker_archives, workers};

/// A dispatch that looks stalled from the store alone.
struct Candidate {
    dispatch: DispatchId,
    task: TaskId,
    handle: String,
    kind: Option<String>,
    pane: Option<String>,
    activity: String,
    store_progress: DateTime<Utc>,
    heartbeat_at: Option<DateTime<Utc>>,
    released: bool,
}

/// Stores the stall escalations that are due for `run` and returns their
/// IDs. Cheap by construction (the tab bar runs it every 30 seconds under a
/// 5-second budget): one store read for the Run, one file `metadata()` per
/// dispatch that already looks stalled, and a pane read only for an
/// escalation about to be stored.
pub fn sweep(engine: &Engine, run: &RunId) -> Result<Vec<MessageId>> {
    let minutes = engine.config.defaults.stall_minutes;
    if minutes == 0 {
        return Ok(Vec::new());
    }
    let threshold = Duration::minutes(i64::try_from(minutes).unwrap_or(i64::MAX));
    let now = engine.clock.now();
    let candidates = engine
        .store
        .read(|tx| candidates(tx, run, now, threshold))?;
    let mut stored = Vec::new();
    for candidate in candidates {
        if let Some(id) = escalate(engine, run, now, threshold, candidate)? {
            stored.push(id);
        }
    }
    if !stored.is_empty() {
        // The sweep runs at the start of a tick, before its own wake pass,
        // so the commit hook cannot re-enter it (`after_commit` makes
        // ladder attempts and never ticks).
        engine.after_commit(run, &[format!("run:{run}")])?;
    }
    Ok(stored)
}

fn candidates(
    tx: &crate::store::Tx<'_>,
    run: &RunId,
    now: DateTime<Utc>,
    threshold: Duration,
) -> Result<Vec<Candidate>> {
    let mut found = Vec::new();
    for row in dispatches::list_by_run(tx, run)? {
        if row.status != "dispatched" {
            continue;
        }
        let store_progress = observe::store_progress_at(tx, &row)?;
        if now.signed_duration_since(store_progress) < threshold {
            continue;
        }
        let projection = observe::project(tx, now, &row.id)?;
        if projection.outcome != "active"
            || projection.liveness == "exited"
            || projection.needs_input.count > 0
        {
            continue;
        }
        let worker = workers::get(tx, &row.id)?;
        found.push(Candidate {
            handle: row
                .assignee_handle
                .clone()
                .unwrap_or_else(|| row.id.to_string()),
            task: row.task_id.clone(),
            kind: row.agent_kind.clone(),
            pane: worker.and_then(|worker| worker.pane_id),
            activity: projection.activity,
            store_progress,
            heartbeat_at: row.last_heartbeat_at,
            released: worker_archives::get(tx, &row.id, "final_screen")?.is_some(),
            dispatch: row.id,
        });
    }
    Ok(found)
}

fn escalate(
    engine: &Engine,
    run: &RunId,
    now: DateTime<Utc>,
    threshold: Duration,
    candidate: Candidate,
) -> Result<Option<MessageId>> {
    let transcript_at = transcript_modified(engine, &candidate);
    // A working agent with no transcript evidence is left alone: a long turn
    // looks exactly like a stall from the store.
    if candidate.activity == "working" && transcript_at.is_none() {
        return Ok(None);
    }
    let since = transcript_at
        .map_or(candidate.store_progress, |at| {
            at.max(candidate.store_progress)
        })
        .trunc_subsecs(0);
    let silent = now.signed_duration_since(since);
    if silent < threshold {
        return Ok(None);
    }
    let since_text = since.to_rfc3339_opts(SecondsFormat::Secs, true);
    let mailbox = format!("run:{run}");
    if already_reported(engine, &mailbox, &candidate.dispatch, &since_text)? {
        return Ok(None);
    }

    let minutes = silent.num_minutes();
    let pane = candidate.pane.as_deref().unwrap_or("unknown");
    let screen = candidate
        .pane
        .as_deref()
        .and_then(|pane| engine.herdr.pane_read(pane, 2000).ok())
        .map(|screen| screen_tail(&screen))
        .unwrap_or_default();
    let mut body = Vec::new();
    body.push(match transcript_at {
        Some(at) => format!(
            "Last transcript write: {} min ago",
            now.signed_duration_since(at).num_minutes()
        ),
        None => "Last transcript write: no transcript located".to_owned(),
    });
    body.push(format!("Activity: {}", candidate.activity));
    body.push(match candidate.heartbeat_at {
        Some(at) => format!(
            "Last heartbeat: {} min ago",
            now.signed_duration_since(at).num_minutes()
        ),
        None => "Last heartbeat: none".to_owned(),
    });
    if let Some(line) = screen.lines().rev().find(|line| is_shell_footer(line)) {
        body.push(format!("Footer: {}", line.trim()));
    }
    body.push("Last screen lines:".to_owned());
    if !screen.is_empty() {
        body.push(screen);
    }

    let id = MessageId::new();
    let dispatch = &candidate.dispatch;
    let row = messages::MessageRow {
        sequence: None,
        id: id.clone(),
        run_id: run.clone(),
        mailbox: mailbox.clone(),
        from_handle: "pod".into(),
        subject: format!(
            "Stalled: {} ({minutes} min without progress)",
            candidate.handle
        ),
        body: Some(body.join("\n")),
        r#type: "escalation".into(),
        priority: "high".into(),
        thread_id: None,
        payload: Some(serde_json::json!({
            "dispatch_id": dispatch,
            "task_id": candidate.task,
            "source": "pod",
            "reason": "stalled",
            "since": since_text,
            "minutes": minutes,
            "activity": candidate.activity,
            "transcript_at": transcript_at.map(|at| at.to_rfc3339()),
            "heartbeat_at": candidate.heartbeat_at.map(|at| at.to_rfc3339()),
        })),
        read: false,
        created_at: now,
        delivered_at: None,
        sender_terminal_id: None,
        rejected_reason: None,
        evidence: Some(format!("pane:{pane}")),
        recommendation: Some(format!(
            "Run pod worker-read --dispatch {dispatch}. If it is waiting on a hung command, pod worker-stop --dispatch {dispatch} and restart the task; if it just stopped reporting, nudge it with pod send --to {} ...",
            candidate.handle
        )),
    };
    // Checked again inside the write transaction: a tab bar tick and a
    // waiting coordinator can sweep at the same moment.
    let stored = engine.store.tx(|tx| {
        if reported_in(tx, &mailbox, dispatch, &since_text)? {
            return Ok(false);
        }
        messages::insert(tx, &row)?;
        Ok(true)
    })?;
    Ok(stored.then_some(id))
}

/// The modification time of the worker's located transcript.
fn transcript_modified(engine: &Engine, candidate: &Candidate) -> Option<DateTime<Utc>> {
    let mut warnings = Vec::new();
    let (_, path) = transcripts::locate_for_worker(
        engine,
        &candidate.dispatch,
        candidate.kind.as_deref(),
        candidate.released,
        &mut warnings,
    )
    .ok()
    .flatten()?;
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(DateTime::<Utc>::from(modified))
}

fn already_reported(
    engine: &Engine,
    mailbox: &str,
    dispatch: &DispatchId,
    since: &str,
) -> Result<bool> {
    engine
        .store
        .read(|tx| reported_in(tx, mailbox, dispatch, since))
}

fn reported_in(
    tx: &crate::store::Tx<'_>,
    mailbox: &str,
    dispatch: &DispatchId,
    since: &str,
) -> Result<bool> {
    Ok(tx.0.query_row(
        "SELECT EXISTS(SELECT 1 FROM messages
         WHERE mailbox = ?1 AND type = 'escalation' AND from_handle = 'pod'
           AND json_extract(payload, '$.dispatch_id') = ?2
           AND json_extract(payload, '$.reason') = 'stalled'
           AND json_extract(payload, '$.since') = ?3)",
        (mailbox, dispatch.as_str(), since),
        |row| row.get::<_, bool>(0),
    )?)
}

/// The agent footer about background shells, such as `2 shells still
/// running`.
fn is_shell_footer(line: &str) -> bool {
    let line = line.to_ascii_lowercase();
    line.contains("shell") && line.contains("running")
}
