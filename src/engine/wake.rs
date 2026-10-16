use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::thread;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use crate::engine::Engine;
use crate::engine::safe_input::{self, InputOutcome, Patience};
use crate::engine::waiting::StateLock;
use crate::error::{PodError, Result};
use crate::herdr::{AgentInfo, AgentStatus};
use crate::ids::{DispatchId, RunId, WakeId};
use crate::store::Tx;
use crate::store::rows::{deliveries, dispatches, messages, runs, wake_attempts, workers};

pub use crate::store::rows::wake_attempts::WakeAttemptRow;

const MAX_ATTEMPTS: u8 = 5;
const RETRY_SECONDS: i64 = 60;
const FRESH_SECONDS: i64 = 15;
const QUIET_FOR: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Trigger {
    /// herdr's `pod hook event` process: nobody waits on it, so it may wait
    /// out the 3-second quiet window.
    IdleHook,
    MutatingCommand,
    ReadOnlyCommand,
    /// A waiting coordinator's own once-a-minute pass: nobody waits on the
    /// tick itself, so it may spend the quiet check.
    WaitTick,
    /// The tab bar entry (`pod usage --tick`). The bar command has a 5-second
    /// timeout, so it never waits.
    BarTick,
}

impl Trigger {
    /// Whether this caller may sleep for the quiet check. A command a person
    /// or agent is waiting on must return within spec 16's 50 ms.
    fn can_wait(self) -> bool {
        matches!(self, Trigger::IdleHook | Trigger::WaitTick)
    }

    /// How long a nudge from this trigger may hold herdr's prompt call.
    fn patience(self) -> Patience {
        match self {
            Trigger::BarTick => Patience::Bar,
            _ => Patience::Nudge,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Ladder {
    pub attempts: u8,
    pub exhausted: bool,
    pub last_at: Option<DateTime<Utc>>,
}

/// The attempt a commit makes in the sender's process for a mailbox it
/// wrote: the ladder's first attempt, or the retry this mutating command is
/// eligible for (8.2).
pub fn after_commit_nudge(engine: &Engine, mailbox: &str) -> Result<Option<WakeAttemptRow>> {
    on_trigger(engine, mailbox, Trigger::MutatingCommand)
}

/// One ladder attempt for an eligible trigger: at most once per invocation,
/// never on a read-only command, never once the ladder is exhausted, and no
/// sooner than 60 seconds after the prior recorded attempt. A trigger that
/// finds "not now" records nothing (see `input_outcome`), so it neither
/// spends a rung nor starts that minute.
pub fn on_trigger(
    engine: &Engine,
    mailbox: &str,
    trigger: Trigger,
) -> Result<Option<WakeAttemptRow>> {
    if trigger == Trigger::ReadOnlyCommand {
        return Ok(None);
    }
    let now = engine.clock.now();
    let eligible = |ladder: &Ladder| {
        !ladder.exhausted
            && !ladder
                .last_at
                .is_some_and(|last| now.signed_duration_since(last).num_seconds() < RETRY_SECONDS)
    };
    let ladder = engine.store.read(|tx| ladder_position(tx, mailbox))?;
    if !eligible(&ladder) {
        return Ok(None);
    }
    attempt(engine, mailbox, trigger, eligible)
}

#[derive(Clone, Debug)]
pub struct TickReport {
    pub mailboxes: usize,
    pub attempts: Vec<WakeAttemptRow>,
}

/// One ladder pass for every mailbox of `run` that holds unread mail: the Run
/// mailbox and each `dispatch:<id>` with an unread row. Each goes through
/// `on_trigger`, so the 60-second rule, the lock, and the ladder count hold,
/// and a mailbox whose earlier triggers all found "not now" (and so recorded
/// nothing) is retried like any other.
pub fn tick(engine: &Engine, run: &RunId, trigger: Trigger) -> Result<TickReport> {
    if trigger == Trigger::ReadOnlyCommand {
        return Ok(TickReport {
            mailboxes: 0,
            attempts: Vec::new(),
        });
    }
    let mailboxes: Vec<String> = engine.store.read(|tx| {
        let mut statement = tx.0.prepare(
            "SELECT DISTINCT mailbox FROM messages
             WHERE run_id = ?1 AND read = 0 ORDER BY mailbox",
        )?;
        Ok(statement
            .query_map([run.as_str()], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    })?;
    let mut attempts = Vec::new();
    for mailbox in &mailboxes {
        if let Some(row) = on_trigger(engine, mailbox, trigger)? {
            attempts.push(row);
        }
    }
    Ok(TickReport {
        mailboxes: mailboxes.len(),
        attempts,
    })
}

/// `tick` for every Run that has a Dispatch with status `dispatched`.
pub fn tick_all(engine: &Engine, trigger: Trigger) -> Result<Vec<(RunId, TickReport)>> {
    let runs: Vec<RunId> = engine.store.read(|tx| {
        let mut statement = tx.0.prepare(
            "SELECT DISTINCT run_id FROM dispatches WHERE status = 'dispatched' ORDER BY run_id",
        )?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.iter().map(|id| RunId::from_str(id)).collect()
    })?;
    runs.into_iter()
        .map(|run| tick(engine, &run, trigger).map(|report| (run, report)))
        .collect()
}

/// The mailboxes a mutating command of `run` retries (8.2): every one with
/// unread mail that is not already in its consumer's hands through an
/// outstanding Delivery, so a consuming `check` never nudges itself and a
/// recipient reading a batch is left to acknowledge it.
pub(crate) fn retry_mailboxes(engine: &Engine, run: &RunId) -> Result<Vec<String>> {
    engine.store.read(|tx| {
        let mut statement = tx.0.prepare(
            "SELECT DISTINCT m.mailbox FROM messages m
             WHERE m.run_id = ?1 AND m.read = 0 AND NOT EXISTS (
                 SELECT 1 FROM deliveries d
                 WHERE d.mailbox = m.mailbox AND d.status = 'outstanding'
             )
             ORDER BY m.mailbox",
        )?;
        Ok(statement
            .query_map([run.as_str()], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    })
}

pub fn ladder_position(tx: &Tx<'_>, mailbox: &str) -> Result<Ladder> {
    let attempts = wake_attempts::count_since_last_consume(tx, mailbox)?.min(u8::MAX as u32) as u8;
    let last_at =
        tx.0.query_row(
            "SELECT created_at FROM wake_attempts
             WHERE mailbox = ?1 AND created_at > COALESCE((
                 SELECT at FROM events
                 WHERE type = 'mailbox.consumed' AND json_extract(detail, '$.mailbox') = ?1
                 ORDER BY at DESC, sequence DESC LIMIT 1
             ), '')
             ORDER BY created_at DESC, rowid DESC LIMIT 1",
            [mailbox],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|value| {
            DateTime::parse_from_rfc3339(&value)
                .map(|value| value.with_timezone(&Utc))
                .map_err(|error| PodError::store(format!("invalid wake timestamp: {error}")))
        })
        .transpose()?;
    Ok(Ladder {
        attempts,
        exhausted: attempts >= MAX_ATTEMPTS,
        last_at,
    })
}

/// Makes one attempt and records it when it is one (8.2). A `check --wait`
/// waiter that matches the unread mail takes the place of a nudge. `eligible`
/// is checked again once this process owns the mailbox's in-flight lock, so
/// two processes that both saw an eligible ladder cannot both type; the
/// process holding the lock records the attempt, and one that only found the
/// lock taken has nothing of its own to record.
fn attempt(
    engine: &Engine,
    mailbox: &str,
    trigger: Trigger,
    eligible: impl Fn(&Ladder) -> bool,
) -> Result<Option<WakeAttemptRow>> {
    if crate::engine::waiting::waiter_matches(engine, mailbox)? {
        return Ok(None);
    }
    // The attempt is stamped when it starts, not after herdr returns: a
    // worker can consume its mail while herdr still waits for the turn, and
    // an attempt stamped after that consume would outlive it in the ladder.
    let started = engine.clock.now();
    let run = engine
        .store
        .read(|tx| crate::engine::deliveries::run_for_mailbox(tx, mailbox))?;
    let unread = engine
        .store
        .read(|tx| messages::select(tx, mailbox, &[], true, 10_000, false))?;
    if unread.is_empty() {
        return Ok(None);
    }

    let path = engine
        .paths
        .state_dir
        .join(format!("wake-{}.lock", mailbox.replace([':', '/'], "_")));
    let _flight = match StateLock::try_acquire(&path) {
        Ok(Some(lock)) => lock,
        Ok(None) => return Ok(None),
        Err(error) => {
            let detail = format!("could not take the wake lock: {error}");
            return record(engine, started, &run, mailbox, InputOutcome::Failed(detail));
        }
    };
    let ladder = engine.store.read(|tx| ladder_position(tx, mailbox))?;
    if !eligible(&ladder) {
        return Ok(None);
    }

    match input_outcome(engine, mailbox, unread.len(), trigger) {
        Some(outcome) => record(engine, started, &run, mailbox, outcome),
        None => Ok(None),
    }
}

/// The outcome of one attempt, or `None` for "not now": the recipient is
/// present and working, or its pane is not yet proven quiet for 3 seconds
/// (the screen changed, or this trigger cannot wait out the window). Neither
/// says anything about the mailbox, so nothing is recorded: the ladder keeps
/// its position and the next eligible trigger tries again at once. Until
/// 2026-10-15 both were recorded `skipped`, so each spent a rung and started
/// the 60-second spacing, and a completion that landed during a turn, or
/// while a person typed in the pane, was refused at the recipient's very
/// next idle hook with nothing left to retry it (seen live 2026-10-14, a
/// tick during a long turn, and 2026-10-15, a `worker_done` while typing).
fn input_outcome(
    engine: &Engine,
    mailbox: &str,
    count: usize,
    trigger: Trigger,
) -> Option<InputOutcome> {
    let recipient = match recipient(engine, mailbox) {
        Ok(Some(recipient)) => recipient,
        Ok(None) => {
            return Some(InputOutcome::Skipped(
                "recipient presence is not proven".into(),
            ));
        }
        Err(error) => return Some(InputOutcome::Failed(error.message)),
    };
    if recipient.presence.as_deref() != Some("present") {
        return Some(InputOutcome::Skipped(
            "recipient presence is not present".into(),
        ));
    }
    if recipient.activity == "working" {
        return None;
    }
    if recipient.activity != "idle_at_prompt" {
        return Some(InputOutcome::Skipped(
            "recipient activity is not idle_at_prompt".into(),
        ));
    }
    if engine
        .clock
        .now()
        .signed_duration_since(recipient.observed_at)
        .num_seconds()
        > FRESH_SECONDS
    {
        return Some(InputOutcome::Skipped(
            "recipient observation is stale".into(),
        ));
    }
    let outstanding = match engine.store.read(|tx| deliveries::outstanding(tx, mailbox)) {
        Ok(value) => value,
        Err(error) => return Some(InputOutcome::Failed(error.message)),
    };
    if outstanding.is_some() {
        return Some(InputOutcome::Skipped(
            "mailbox has an outstanding Delivery".into(),
        ));
    }
    let prior_unconfirmed = engine.store.read(|tx| {
        Ok(tx.0.query_row(
            "SELECT EXISTS(SELECT 1 FROM wake_attempts
             WHERE mailbox = ?1 AND outcome = 'rendered_unconfirmed'
               AND created_at > COALESCE((SELECT at FROM events
                 WHERE type = 'mailbox.consumed' AND json_extract(detail, '$.mailbox') = ?1
                 ORDER BY at DESC, sequence DESC LIMIT 1), ''))",
            [mailbox],
            |row| row.get::<_, bool>(0),
        )?)
    });
    match prior_unconfirmed {
        Ok(true) => {
            return Some(InputOutcome::Skipped(
                "an earlier paste is unconfirmed; input has not been proven cleared".into(),
            ));
        }
        Err(error) => return Some(InputOutcome::Failed(error.message)),
        Ok(false) => {}
    }

    // `quiet` records the screen it saw, so a trigger that cannot wait leaves
    // a snapshot the next trigger can prove the window from.
    match quiet(engine, &recipient.pane_id, trigger.can_wait()) {
        Ok(Quiet::Proven) => {}
        Ok(Quiet::Changed | Quiet::NotYetProven) => return None,
        Err(error) => return Some(InputOutcome::Failed(error.message)),
    }
    let (worker_handle, brief) = match brief_for(engine, mailbox, &recipient.handle) {
        Ok(value) => value,
        Err(error) => return Some(InputOutcome::Failed(error.message)),
    };
    let text = nudge_text(count, worker_handle.as_deref(), brief.as_deref());
    let dispatch = mailbox
        .strip_prefix("dispatch:")
        .and_then(|id| DispatchId::from_str(id).ok());
    Some(
        match safe_input::prompt_or_paste(
            engine,
            &recipient.handle,
            &recipient.pane_id,
            &text,
            dispatch.as_ref(),
            trigger.patience(),
        ) {
            Ok((outcome, _)) => outcome,
            Err(error) => InputOutcome::Failed(error.message),
        },
    )
}

/// The text a nudge types. A worker's nudge names its handle and, when its
/// brief file exists, the file, so a worker that lost its context finds the
/// brief again. The Run mailbox (the coordinator) has neither.
pub fn nudge_text(count: usize, handle: Option<&str>, brief: Option<&Path>) -> String {
    let mut text = match handle {
        Some(handle) => format!(
            "You have {count} orchestration message(s). Run `pod check --terminal {handle}`."
        ),
        None => format!("You have {count} orchestration message(s). Run `pod check`."),
    };
    if let Some(brief) = brief {
        text.push_str(&format!(" Your brief: {}.", brief.display()));
    }
    text
}

/// The handle and brief file a nudge to `mailbox` names: only a `dispatch:`
/// mailbox has either, and the brief only while its file exists.
fn brief_for(
    engine: &Engine,
    mailbox: &str,
    handle: &str,
) -> Result<(Option<String>, Option<PathBuf>)> {
    let Some(id) = mailbox.strip_prefix("dispatch:") else {
        return Ok((None, None));
    };
    let dispatch = DispatchId::from_str(id)?;
    let run = engine
        .store
        .read(|tx| Ok(dispatches::get(tx, &dispatch)?.map(|row| row.run_id)))?;
    let brief = run
        .map(|run| crate::engine::lifecycle::brief_path(&engine.paths, &run, &dispatch))
        .filter(|path| path.exists());
    let handle = (!handle.is_empty()).then(|| handle.to_string());
    Ok((handle, brief))
}

/// The quiet check of 8.2: the pane's last 20 lines unchanged for 3 seconds.
enum Quiet {
    Proven,
    Changed,
    NotYetProven,
}

/// Proves the 3-second quiet window from the pane's screen snapshot, kept in
/// a state-directory file (`screen-<pane>.quiet`: when the current last 20
/// lines were first seen, and their hash) so separate `pod` processes share
/// it. A screen unchanged since a snapshot at least 3 s old is quiet with no
/// waiting. Otherwise the snapshot is (re)started, and only a caller that may
/// wait (herdr's hook process) sleeps out the window and reads again; a
/// command returns `NotYetProven` at once (spec 16 budget).
fn quiet(engine: &Engine, pane: &str, can_wait: bool) -> Result<Quiet> {
    let now = engine.clock.now();
    let hash = screen_hash(engine, pane)?;
    let path = snapshot_path(engine, pane);
    let since = match read_snapshot(&path) {
        Some((first_seen, stored)) if stored == hash => first_seen,
        _ => {
            write_snapshot(&path, now, &hash);
            now
        }
    };
    let elapsed = now
        .signed_duration_since(since)
        .to_std()
        .unwrap_or_default();
    if elapsed >= QUIET_FOR {
        return Ok(Quiet::Proven);
    }
    if !can_wait {
        return Ok(Quiet::NotYetProven);
    }
    thread::sleep(QUIET_FOR - elapsed);
    if screen_hash(engine, pane)? == hash {
        Ok(Quiet::Proven)
    } else {
        Ok(Quiet::Changed)
    }
}

/// Records the pane's current screen as a quiet-window snapshot unless it is
/// unchanged since the stored one. Called when herdr reports an agent idle,
/// so a later send finds the window already proven. Best effort.
pub fn note_screen(engine: &Engine, pane: &str) {
    let Ok(hash) = screen_hash(engine, pane) else {
        return;
    };
    let path = snapshot_path(engine, pane);
    if read_snapshot(&path).is_none_or(|(_, stored)| stored != hash) {
        write_snapshot(&path, engine.clock.now(), &hash);
    }
}

fn screen_hash(engine: &Engine, pane: &str) -> Result<String> {
    use sha2::{Digest, Sha256};
    let screen = engine.herdr.pane_read(pane, 20)?;
    Ok(hex::encode(Sha256::digest(
        last_lines(&screen, 20).as_bytes(),
    )))
}

fn snapshot_path(engine: &Engine, pane: &str) -> std::path::PathBuf {
    let name: String = pane
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    engine.paths.state_dir.join(format!("screen-{name}.quiet"))
}

fn read_snapshot(path: &std::path::Path) -> Option<(DateTime<Utc>, String)> {
    let text = std::fs::read_to_string(path).ok()?;
    let (at, hash) = text.trim().split_once(' ')?;
    let at = DateTime::parse_from_rfc3339(at).ok()?.with_timezone(&Utc);
    Some((at, hash.to_owned()))
}

/// Written through a private temp file and a rename, so a reader never sees
/// half a snapshot. A failed write only loses the head start.
fn write_snapshot(path: &std::path::Path, at: DateTime<Utc>, hash: &str) {
    use std::io::Write;
    let temp = path.with_extension(format!("{}.tmp", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let written = options
        .open(&temp)
        .and_then(|mut file| file.write_all(format!("{} {hash}\n", at.to_rfc3339()).as_bytes()))
        .and_then(|()| std::fs::rename(&temp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
}

fn last_lines(value: &str, count: usize) -> String {
    let lines = value.lines().collect::<Vec<_>>();
    lines[lines.len().saturating_sub(count)..].join("\n")
}

struct Recipient {
    /// The herdr agent name `agent.prompt` addresses.
    handle: String,
    pane_id: String,
    presence: Option<String>,
    activity: String,
    observed_at: DateTime<Utc>,
}

fn recipient(engine: &Engine, mailbox: &str) -> Result<Option<Recipient>> {
    if let Some(id) = mailbox.strip_prefix("dispatch:") {
        let dispatch = DispatchId::from_str(id)?;
        let pane = engine
            .store
            .read(|tx| Ok(workers::get(tx, &dispatch)?.and_then(|worker| worker.pane_id)))?;
        let Some(pane_id) = pane else {
            return Ok(None);
        };
        let handle = engine
            .store
            .read(|tx| Ok(dispatches::get(tx, &dispatch)?.and_then(|row| row.assignee_handle)))?
            .unwrap_or_default();
        refresh_evidence(engine, &dispatch, &pane_id)?;
        return engine.store.read(|tx| {
            let Some(evidence) = crate::engine::observe::wake_evidence(tx, &dispatch)? else {
                return Ok(None);
            };
            Ok(Some(Recipient {
                handle,
                pane_id,
                presence: evidence.presence,
                activity: evidence.activity,
                observed_at: evidence.observed_at,
            }))
        });
    }
    let Some(id) = mailbox.strip_prefix("run:") else {
        return Ok(None);
    };
    let run = RunId::from_str(id)?;
    let Some(run) = engine.store.read(|tx| runs::get(tx, &run))? else {
        return Ok(None);
    };
    let agents = engine.herdr.agent_list()?;
    for agent in agents {
        if pane_terminal(engine, &agent)? == Some(run.coordinator_terminal_id.clone()) {
            let activity = activity(&agent);
            return Ok(Some(Recipient {
                handle: agent.name,
                pane_id: agent.pane_id,
                presence: Some("present".into()),
                activity,
                observed_at: agent.observed_at,
            }));
        }
    }
    Ok(None)
}

/// Records herdr's current agent state for the worker's pane. A worker that
/// went idle minutes ago produces no further events, so stored evidence alone
/// would never be within the 15-second bound. No detected agent records
/// nothing: absence from the list proves nothing.
fn refresh_evidence(engine: &Engine, dispatch: &DispatchId, pane_id: &str) -> Result<()> {
    let Some(agent) = engine
        .herdr
        .agent_list()?
        .into_iter()
        .find(|agent| agent.pane_id == pane_id)
    else {
        return Ok(());
    };
    let screen = crate::engine::reconcile::blocked_screen(engine, pane_id, &agent.status);
    // A prompt this refresh finds is stored as mail like any other; the wake
    // pass that called for the refresh is already under way, so its commit
    // hook is left to the next trigger rather than started from inside it.
    engine
        .store
        .tx(|tx| {
            crate::engine::observe::record_herdr_state(
                tx,
                engine.clock.now(),
                dispatch,
                agent.status.clone(),
                &agent.needs_input,
                Some("present"),
                screen.as_deref(),
            )
        })
        .map(drop)
}

fn pane_terminal(engine: &Engine, agent: &AgentInfo) -> Result<Option<String>> {
    Ok(engine
        .herdr
        .pane_get(&agent.pane_id)?
        .map(|pane| pane.terminal_id))
}

fn activity(agent: &AgentInfo) -> String {
    match agent.status {
        AgentStatus::Idle | AgentStatus::Blocked | AgentStatus::Done => "idle_at_prompt",
        AgentStatus::Working => "working",
        AgentStatus::Unknown => "unknown",
    }
    .into()
}

fn record(
    engine: &Engine,
    now: DateTime<Utc>,
    run: &RunId,
    mailbox: &str,
    outcome: InputOutcome,
) -> Result<Option<WakeAttemptRow>> {
    let row = engine.store.tx(|tx| {
        let ladder = ladder_position(tx, mailbox)?;
        if ladder.exhausted {
            return Ok(None);
        }
        let row = WakeAttemptRow {
            id: WakeId::new(),
            run_id: run.clone(),
            mailbox: mailbox.to_owned(),
            delivery_id: None,
            attempt: u32::from(ladder.attempts) + 1,
            outcome: outcome.stored_name().to_owned(),
            detail: outcome.detail().map(str::to_owned),
            created_at: now,
        };
        wake_attempts::insert(tx, &row)?;
        // Spec 8.2: an exhausted Run mailbox is written to the board for the
        // user (`events[]` in run.json); a worker mailbox's exhaustion is the
        // coordinator's `worker-show` next action instead.
        if row.attempt == u32::from(MAX_ATTEMPTS) && mailbox.starts_with("run:") {
            crate::store::rows::events::append(
                tx,
                &crate::store::rows::events::EventRow {
                    sequence: None,
                    run_id: run.clone(),
                    at: now,
                    r#type: "mailbox.unreachable".into(),
                    task_id: None,
                    dispatch_id: None,
                    actor: "pod".into(),
                    detail: Some(serde_json::json!({
                        "mailbox": mailbox,
                        "attempts": row.attempt,
                        "nextAction": ["pod", "check"],
                    })),
                },
            )?;
        }
        Ok(Some(row))
    })?;
    Ok(row)
}
