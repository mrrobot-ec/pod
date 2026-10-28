use chrono::{DateTime, Duration, Utc};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use crate::error::{PodError, Result};
use crate::herdr::AgentStatus;
use crate::ids::{DispatchId, MessageId, RunId};
use crate::store::Tx;
use crate::store::rows::{dispatches, messages, observations, workers};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct NeedsInput {
    pub count: usize,
    pub reasons: Vec<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attention {
    pub categories: Vec<String>,
    pub requires_action: bool,
}

/// The coordinator's next move as a literal argv (spec 7.6): `kind` is the
/// pod command, or `none` with an empty argv when the right move is to wait.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NextAction {
    pub kind: String,
    pub argv: Vec<String>,
}

impl NextAction {
    pub fn none() -> Self {
        Self {
            kind: "none".into(),
            argv: Vec::new(),
        }
    }

    fn command(command: &str, dispatch: &DispatchId) -> Self {
        Self {
            kind: command.into(),
            argv: vec![
                "pod".into(),
                command.into(),
                "--dispatch".into(),
                dispatch.to_string(),
            ],
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Projection {
    pub liveness: String,
    pub activity: String,
    pub presence: Option<String>,
    pub needs_input: NeedsInput,
    pub attention: Attention,
    pub next_action: NextAction,
    pub outcome: String,
}

pub fn record_self_report(
    tx: &Tx<'_>,
    now: DateTime<Utc>,
    dispatch: &DispatchId,
    activity: &str,
) -> Result<()> {
    observations::insert(
        tx,
        &observations::ObservationRow {
            dispatch_id: dispatch.clone(),
            liveness: "unverifiable".into(),
            activity: normalize_activity(activity),
            presence: None,
            needs_input: serde_json::json!({"count": 0, "reasons": []}),
            evidence_source: "self_report".into(),
            reason: Some("process presence has not been established by herdr".into()),
            herdr_status: None,
            attention: None,
            observed_at: now,
        },
    )
}

/// Prefix of the observation reason that marks a person typing into a worker
/// pane.
pub const USER_TYPING: &str = "user_typing:";

/// The observation reason a bare read of the worker's input writes once a
/// person's typing was seen: the only thing that clears `interruption`.
pub const INPUT_BARE: &str = "input_bare";

/// What the latest read of the worker's input saw: the text a person typed,
/// or `None` once a bare read cleared it. Other observations (herdr state
/// from hooks or a wake's refresh) say nothing about the input line, so
/// they neither set nor clear it.
pub fn user_typing(tx: &Tx<'_>, dispatch: &DispatchId) -> Result<Option<String>> {
    let reason: Option<String> =
        tx.0.query_row(
            "SELECT reason FROM observations
             WHERE dispatch_id = ?1 AND (reason LIKE 'user_typing:%' OR reason = ?2)
             ORDER BY observed_at DESC, rowid DESC LIMIT 1",
            (dispatch.as_str(), INPUT_BARE),
            |row| row.get(0),
        )
        .optional()?;
    Ok(reason.and_then(|reason| {
        reason
            .strip_prefix(USER_TYPING)
            .map(|text| text.trim_start().to_owned())
    }))
}

/// A person typed into the worker's input (`Some(text)`) or the input is
/// bare again (`None`, the clearing observation). The liveness, activity and
/// presence last known are carried over; typing proves nothing about them.
pub fn record_user_typing(
    tx: &Tx<'_>,
    now: DateTime<Utc>,
    dispatch: &DispatchId,
    text: Option<&str>,
) -> Result<()> {
    let previous = observations::get(tx, dispatch)?;
    let (liveness, activity, presence, needs_input, herdr_status) = match previous {
        Some(row) => (
            row.liveness,
            row.activity,
            row.presence,
            row.needs_input,
            row.herdr_status,
        ),
        None => (
            "unverifiable".into(),
            "unknown".into(),
            None,
            serde_json::json!({"count": 0, "reasons": []}),
            None,
        ),
    };
    observations::insert(
        tx,
        &observations::ObservationRow {
            dispatch_id: dispatch.clone(),
            liveness,
            activity,
            presence,
            needs_input,
            evidence_source: "herdr_state".into(),
            reason: Some(match text {
                Some(text) => format!(
                    "{USER_TYPING} {}",
                    text.chars().take(80).collect::<String>()
                ),
                None => INPUT_BARE.to_owned(),
            }),
            herdr_status,
            attention: None,
            observed_at: now,
        },
    )
}

/// The mail `record_herdr_state` stored for a worker that began waiting on a
/// prompt; the caller runs the commit hook for its Run (spec 7.6).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromptEscalation {
    pub run_id: RunId,
    pub message_id: MessageId,
}

/// Lines of the worker's screen an escalation carries, and the width each
/// line is cut to.
const SCREEN_LINES: usize = 40;
const SCREEN_WIDTH: usize = 200;

/// Records herdr's state for the Dispatch. `screen` is the worker pane's last
/// lines, which a caller reads only when the status is `blocked`: the first
/// observation of a blocked episode stores one `escalation` from pod in the
/// Run mailbox (spec 7.6), so the prompt rides the mail machinery.
pub fn record_herdr_state(
    tx: &Tx<'_>,
    now: DateTime<Utc>,
    dispatch: &DispatchId,
    status: AgentStatus,
    needs_input: &[String],
    presence: Option<&str>,
    screen: Option<&str>,
) -> Result<Option<PromptEscalation>> {
    let activity = match status {
        AgentStatus::Working => "working",
        AgentStatus::Idle | AgentStatus::Blocked | AgentStatus::Done => "idle_at_prompt",
        AgentStatus::Unknown => "unknown",
    };
    let liveness = match presence {
        Some("present") => "live",
        Some("exited" | "absent") => "exited",
        _ => "unverifiable",
    };
    // herdr 0.9.3 reports a Codex approval prompt as `blocked` with no
    // state label (seen live 2026-10-13), so the status itself is the
    // evidence that a person is being asked for something.
    let blocked_fallback = ["blocked".to_owned()];
    let needs_input = if needs_input.is_empty() && matches!(status, AgentStatus::Blocked) {
        &blocked_fallback[..]
    } else {
        needs_input
    };
    let escalation = if presence == Some("present") && !needs_input.is_empty() {
        begin_prompt_episode(tx, now, dispatch, needs_input, screen)?
    } else {
        None
    };
    observations::insert(
        tx,
        &observations::ObservationRow {
            dispatch_id: dispatch.clone(),
            liveness: liveness.into(),
            activity: activity.into(),
            presence: presence.map(str::to_owned),
            needs_input: serde_json::json!({
                "count": needs_input.len(),
                "reasons": needs_input,
            }),
            evidence_source: "herdr_state".into(),
            reason: (liveness == "unverifiable")
                .then(|| "process presence could not be established".into()),
            herdr_status: Some(status_name(status).into()),
            attention: None,
            observed_at: now,
        },
    )?;
    Ok(escalation)
}

/// Stores the escalation for a worker that just began waiting on a prompt: the
/// latest earlier herdr observation of the Dispatch had nothing waiting, the
/// Dispatch is still active, and pod has no unread escalation for it. Run
/// before the new observation is inserted, inside the same transaction, so a
/// hook and a waiter recording one event store a single message.
fn begin_prompt_episode(
    tx: &Tx<'_>,
    now: DateTime<Utc>,
    dispatch: &DispatchId,
    needs_input: &[String],
    screen: Option<&str>,
) -> Result<Option<PromptEscalation>> {
    let Some(row) = dispatches::get(tx, dispatch)? else {
        return Ok(None);
    };
    if !matches!(row.status.as_str(), "pending" | "dispatched") {
        return Ok(None);
    }
    let previous: Option<String> =
        tx.0.query_row(
            "SELECT needs_input FROM observations
             WHERE dispatch_id = ?1 AND evidence_source = 'herdr_state'
             ORDER BY observed_at DESC, rowid DESC LIMIT 1",
            [dispatch.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(previous) = previous {
        let previous: serde_json::Value = serde_json::from_str(&previous).map_err(|error| {
            PodError::store(format!("invalid needs_input observation: {error}"))
        })?;
        if parse_needs_input(&previous)?.count > 0 {
            return Ok(None);
        }
    }
    let mailbox = format!("run:{}", row.run_id);
    let unread: i64 = tx.0.query_row(
        "SELECT COUNT(*) FROM messages
         WHERE mailbox = ?1 AND type = 'escalation' AND from_handle = 'pod' AND read = 0
           AND json_extract(payload, '$.dispatch_id') = ?2",
        (&mailbox, dispatch.as_str()),
        |row| row.get(0),
    )?;
    if unread > 0 {
        return Ok(None);
    }
    let handle = row
        .assignee_handle
        .clone()
        .unwrap_or_else(|| dispatch.to_string());
    let reason = if needs_input.iter().any(|reason| reason == "approval") {
        "approval"
    } else {
        "input"
    };
    let pane = workers::get(tx, dispatch)?
        .and_then(|worker| worker.pane_id)
        .unwrap_or_else(|| "unknown".into());
    let mut body = format!(
        "Worker {handle} is waiting on a prompt that a person must answer; pod never answers it. Last screen lines:"
    );
    if let Some(tail) = screen.map(screen_tail).filter(|tail| !tail.is_empty()) {
        body.push('\n');
        body.push_str(&tail);
    }
    let id = MessageId::new();
    messages::insert(
        tx,
        &messages::MessageRow {
            sequence: None,
            id: id.clone(),
            run_id: row.run_id.clone(),
            mailbox,
            from_handle: "pod".into(),
            subject: format!("Blocked on a prompt: {handle} ({reason})"),
            body: Some(body),
            r#type: "escalation".into(),
            priority: "high".into(),
            thread_id: None,
            payload: Some(serde_json::json!({
                "dispatch_id": dispatch,
                "task_id": row.task_id,
                "reason": reason,
                "source": "pod",
                "needs_input": needs_input,
            })),
            read: false,
            created_at: now,
            delivered_at: None,
            sender_terminal_id: None,
            rejected_reason: None,
            evidence: Some(format!("pane:{pane}")),
            recommendation: Some(format!(
                "Run pod worker-read --dispatch {dispatch} --source terminal and answer the prompt in the pane as a person would, or pod worker-stop --dispatch {dispatch} and restart the task with other launch options."
            )),
        },
    )?;
    Ok(Some(PromptEscalation {
        run_id: row.run_id,
        message_id: id,
    }))
}

/// The last lines of a screen: trailing blank lines dropped, at most
/// [`SCREEN_LINES`] lines, each cut to [`SCREEN_WIDTH`] characters.
pub(crate) fn screen_tail(screen: &str) -> String {
    let lines: Vec<&str> = screen.lines().collect();
    let end = lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .map_or(0, |index| index + 1);
    let lines = &lines[..end];
    lines[lines.len().saturating_sub(SCREEN_LINES)..]
        .iter()
        .map(|line| line.chars().take(SCREEN_WIDTH).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The latest sign of progress the store holds for an active Dispatch: when
/// it was dispatched, its last heartbeat, the newest message its worker sent
/// the Run, and the newest observation of it `working`. The stall sweep adds
/// the transcript's modification time, which `project` has no engine to read.
pub(crate) fn store_progress_at(
    tx: &Tx<'_>,
    row: &dispatches::DispatchRow,
) -> Result<DateTime<Utc>> {
    let mut latest = row.dispatched_at.unwrap_or(row.created_at);
    if let Some(heartbeat) = row.last_heartbeat_at {
        latest = latest.max(heartbeat);
    }
    if let Some(handle) = &row.assignee_handle {
        let sent: Option<String> =
            tx.0.query_row(
                "SELECT created_at FROM messages WHERE run_id = ?1 AND from_handle = ?2
                 ORDER BY created_at DESC, sequence DESC LIMIT 1",
                (row.run_id.as_str(), handle),
                |found| found.get(0),
            )
            .optional()?;
        if let Some(sent) = sent {
            latest = latest.max(parse_time(&sent)?);
        }
    }
    let working: Option<String> =
        tx.0.query_row(
            "SELECT observed_at FROM observations
             WHERE dispatch_id = ?1 AND activity = 'working'
             ORDER BY observed_at DESC, rowid DESC LIMIT 1",
            [row.id.as_str()],
            |found| found.get(0),
        )
        .optional()?;
    if let Some(working) = working {
        latest = latest.max(parse_time(&working)?);
    }
    Ok(latest)
}

/// When pod last stored a `stalled` escalation for the Dispatch.
pub(crate) fn stalled_escalation_at(
    tx: &Tx<'_>,
    run: &RunId,
    dispatch: &DispatchId,
) -> Result<Option<DateTime<Utc>>> {
    let created: Option<String> =
        tx.0.query_row(
            "SELECT created_at FROM messages
             WHERE mailbox = ?1 AND type = 'escalation' AND from_handle = 'pod'
               AND json_extract(payload, '$.dispatch_id') = ?2
               AND json_extract(payload, '$.reason') = 'stalled'
             ORDER BY sequence DESC LIMIT 1",
            (format!("run:{run}"), dispatch.as_str()),
            |found| found.get(0),
        )
        .optional()?;
    created.map(|created| parse_time(&created)).transpose()
}

pub fn project(tx: &Tx<'_>, now: DateTime<Utc>, dispatch: &DispatchId) -> Result<Projection> {
    let dispatch_row = dispatches::get(tx, dispatch)?
        .ok_or_else(|| PodError::usage(format!("unknown dispatch {dispatch}")))?;
    let worker = workers::get(tx, dispatch)?;
    let observations = evidence_rows(tx, dispatch)?;
    let latest = observations.first();
    let herdr = observations
        .iter()
        .find(|row| row.evidence_source == "herdr_state");

    let activity = latest
        .map(|row| row.activity.clone())
        .unwrap_or_else(|| "unknown".into());
    let presence = herdr.and_then(|row| row.presence.clone());
    let needs_input = herdr
        .map(|row| parse_needs_input(&row.needs_input))
        .transpose()?
        .unwrap_or_default();
    let liveness = match presence.as_deref() {
        Some("present") => "live",
        Some("exited" | "absent") => "exited",
        _ => "unverifiable",
    }
    .to_owned();

    let outcome = recorded_outcome(
        worker.as_ref().map(|row| row.state.as_str()),
        &dispatch_row.status,
    )
    .map(str::to_owned)
    .unwrap_or_else(|| {
        if liveness == "exited" {
            "outcome_unknown".into()
        } else {
            "active".into()
        }
    });
    let unsettled = outcome == "active" || outcome == "outcome_unknown";
    let mut categories = Vec::new();
    let mut requires_action = false;
    if liveness == "live" && needs_input.count > 0 {
        for reason in &needs_input.reasons {
            let category = if reason == "approval" {
                "approval"
            } else {
                "input"
            };
            if !categories.iter().any(|value| value == category) {
                categories.push(category.to_owned());
            }
        }
        requires_action = true;
    } else if liveness == "exited" && unsettled {
        categories.push("failure".into());
        requires_action = true;
    } else if liveness == "unverifiable" {
        categories.push("unverifiable".into());
    }

    // A silent worker is a warning while its attempt is still running; a
    // settled or exited attempt has nothing left to report.
    let freshness = dispatch_row
        .last_heartbeat_at
        .unwrap_or(dispatch_row.created_at);
    if outcome == "active" && now.signed_duration_since(freshness) > Duration::minutes(10) {
        categories.push("stale".into());
    }
    // A stall pod reported stays until the worker shows progress after the
    // report; a missed heartbeat while the transcript moves is only `stale`.
    let stalled = outcome == "active"
        && liveness != "exited"
        && match stalled_escalation_at(tx, &dispatch_row.run_id, dispatch)? {
            Some(reported) => reported > store_progress_at(tx, &dispatch_row)?,
            None => false,
        };
    if stalled {
        categories.push("stalled".into());
        requires_action = true;
    }
    let mailbox = format!("dispatch:{dispatch}");
    let unreachable = crate::engine::wake::ladder_position(tx, &mailbox)?.exhausted;
    if unreachable {
        categories.push("unreachable".into());
        requires_action = true;
    }

    // A person typing into the pane is left alone; the category clears when
    // a later read finds the input bare (spec 7.4).
    let typing = user_typing(tx, dispatch)?.is_some();
    if typing && outcome == "active" {
        categories.push("interruption".into());
    }

    // A question or escalation the coordinator has left unanswered for 5
    // minutes needs guidance (spec 7.6).
    let guidance = if outcome == "active" {
        unanswered(tx, now, &dispatch_row.run_id, dispatch)?
    } else {
        None
    };
    if guidance.is_some() {
        categories.push("guidance".into());
        requires_action = true;
    }

    // Only proof moves the coordinator: an exit or an exhausted ladder is
    // inspected, a prompt waiting on a person is read, and a settled worker
    // still holding a pane is accounted for (7.4). `unverifiable` alone never
    // suggests stop, abandon, retry, or release.
    let terminal_state = worker.as_ref().map(|row| row.terminal_state.as_str());
    let next_action = if unreachable || (liveness == "exited" && unsettled) {
        NextAction::command("worker-show", dispatch)
    } else if (liveness == "live" && needs_input.count > 0) || stalled {
        NextAction::command("worker-read", dispatch)
    } else if let Some(action) = guidance {
        action
    } else if typing && outcome == "active" {
        NextAction::command("worker-read", dispatch)
    } else if !unsettled
        && matches!(
            terminal_state,
            Some("reclaimable" | "release_pending" | "release_unknown")
        )
    {
        NextAction::command("worker-release", dispatch)
    } else {
        NextAction::none()
    };

    Ok(Projection {
        liveness,
        activity,
        presence,
        needs_input,
        attention: Attention {
            categories,
            requires_action,
        },
        next_action,
        outcome,
    })
}

/// How long a worker's question or escalation may wait for the coordinator
/// before it needs `guidance`.
const GUIDANCE_AFTER: Duration = Duration::minutes(5);

/// The next move for the oldest question or escalation of `dispatch` that the
/// coordinator has not answered for [`GUIDANCE_AFTER`]. Either is answered
/// by any later message to the Dispatch mailbox (a `reply` or a `send`); a
/// question also by its row leaving `pending`.
fn unanswered(
    tx: &Tx<'_>,
    now: DateTime<Utc>,
    run: &RunId,
    dispatch: &DispatchId,
) -> Result<Option<NextAction>> {
    let mut found: Vec<(DateTime<Utc>, NextAction)> = Vec::new();
    let mailbox = format!("dispatch:{dispatch}");
    let mut statement = tx.0.prepare(
        "SELECT q.message_id, q.created_at FROM questions q
         JOIN messages m ON m.id = q.message_id
         WHERE q.dispatch_id = ?1 AND q.status = 'pending'
           AND NOT EXISTS (SELECT 1 FROM messages r
                           WHERE r.mailbox = ?2 AND r.sequence > m.sequence)
         ORDER BY q.created_at",
    )?;
    for row in statement.query_map((dispatch.as_str(), &mailbox), |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })? {
        let (id, created_at) = row?;
        found.push((
            parse_time(&created_at)?,
            NextAction {
                kind: "reply".into(),
                argv: vec!["pod".into(), "reply".into(), "--id".into(), id],
            },
        ));
    }
    let mut statement = tx.0.prepare(
        "SELECT m.created_at FROM messages m
         WHERE m.mailbox = ?1 AND m.type = 'escalation'
           AND json_extract(m.payload, '$.dispatch_id') = ?2
           AND m.from_handle != 'pod'
           AND NOT EXISTS (SELECT 1 FROM messages r
                           WHERE r.mailbox = ?3 AND r.sequence > m.sequence)
         ORDER BY m.sequence",
    )?;
    for created_at in statement
        .query_map((format!("run:{run}"), dispatch.as_str(), &mailbox), |row| {
            row.get::<_, String>(0)
        })?
    {
        found.push((
            parse_time(&created_at?)?,
            NextAction::command("worker-read", dispatch),
        ));
    }
    Ok(found
        .into_iter()
        .filter(|(created_at, _)| now.signed_duration_since(*created_at) > GUIDANCE_AFTER)
        .min_by_key(|(created_at, _)| *created_at)
        .map(|(_, action)| action))
}

fn parse_time(value: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| PodError::store(format!("invalid message timestamp: {error}")))
}

/// The independent activity and presence evidence used by the wake
/// preconditions, with the older timestamp so both axes must be fresh.
pub(crate) fn wake_evidence(tx: &Tx<'_>, dispatch: &DispatchId) -> Result<Option<WakeEvidence>> {
    let rows = evidence_rows(tx, dispatch)?;
    let Some(activity) = rows.first() else {
        return Ok(None);
    };
    let Some(presence) = rows.iter().find(|row| row.evidence_source == "herdr_state") else {
        return Ok(None);
    };
    Ok(Some(WakeEvidence {
        activity: activity.activity.clone(),
        presence: presence.presence.clone(),
        observed_at: activity.observed_at.min(presence.observed_at),
    }))
}

pub(crate) struct WakeEvidence {
    pub activity: String,
    pub presence: Option<String>,
    pub observed_at: DateTime<Utc>,
}

struct Evidence {
    activity: String,
    presence: Option<String>,
    needs_input: serde_json::Value,
    evidence_source: String,
    observed_at: DateTime<Utc>,
}

fn evidence_rows(tx: &Tx<'_>, dispatch: &DispatchId) -> Result<Vec<Evidence>> {
    let mut statement = tx.0.prepare(
        "SELECT activity, presence, needs_input, evidence_source, observed_at
         FROM observations WHERE dispatch_id = ?1
         ORDER BY observed_at DESC, rowid DESC",
    )?;
    let encoded = statement
        .query_map([dispatch.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    encoded
        .into_iter()
        .map(
            |(activity, presence, needs_input, evidence_source, observed_at)| {
                Ok(Evidence {
                    activity,
                    presence,
                    needs_input: serde_json::from_str(&needs_input).map_err(|error| {
                        PodError::store(format!("invalid needs_input observation: {error}"))
                    })?,
                    evidence_source,
                    observed_at: DateTime::parse_from_rfc3339(&observed_at)
                        .map(|value| value.with_timezone(&Utc))
                        .map_err(|error| {
                            PodError::store(format!("invalid observation timestamp: {error}"))
                        })?,
                })
            },
        )
        .collect()
}

fn normalize_activity(value: &str) -> String {
    match value {
        "working" => "working",
        "idle" | "idle_at_prompt" | "blocked" | "done" => "idle_at_prompt",
        _ => "unknown",
    }
    .into()
}

fn status_name(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Idle => "idle",
        AgentStatus::Working => "working",
        AgentStatus::Blocked => "blocked",
        AgentStatus::Done => "done",
        AgentStatus::Unknown => "unknown",
    }
}

fn parse_needs_input(value: &serde_json::Value) -> Result<NeedsInput> {
    serde_json::from_value(value.clone())
        .map_err(|error| PodError::store(format!("invalid needs_input observation: {error}")))
}

fn recorded_outcome(worker: Option<&str>, dispatch: &str) -> Option<&'static str> {
    for value in [worker, Some(dispatch)].into_iter().flatten() {
        match value {
            "succeeded" | "completed" => return Some("succeeded"),
            "failed" | "circuit_broken" => return Some("failed"),
            "stopping" | "stopped" | "stop_unknown" => return Some("stopped"),
            "abandoned" => return Some("abandoned"),
            _ => {}
        }
    }
    None
}
