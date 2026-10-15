use std::collections::BTreeSet;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::engine::Engine;
use crate::engine::deliveries::{self, Delivery};
use crate::engine::wake::{self, Trigger};
use crate::error::{ErrorCode, PodError, Result};
use crate::ids::{MessageId, RunId};
use crate::store::rows::{dispatches, messages, questions, workers};
use crate::store::signal;

const KEEPALIVE: Duration = Duration::from_secs(15);
const EVENT_POLL: Duration = Duration::from_millis(25);
/// How long each loop pass reads the herdr event stream. A zero timeout never
/// reads the socket stream at all, so the waiter would see no events.
const EVENT_READ: Duration = Duration::from_millis(1);
/// How often a waiter re-evaluates attention even when it saw no change: a
/// commit that lands between one wait returning and the next snapshot is
/// absorbed into that snapshot's baseline and would otherwise go unseen.
const ATTENTION_RECHECK: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AskOutcome {
    Answered(String),
    Timeout(MessageId),
}

/// Why a `check --wait` returned.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WokeOn {
    /// A message of a requested type exists.
    Message,
    /// A worker of the Run newly requires action (approval or input prompt,
    /// an exit without a report, an exhausted wake ladder).
    Attention,
    Timeout,
}

pub fn check_wait(
    engine: &Engine,
    types: &[String],
    timeout_ms: u64,
) -> Result<(Delivery, WokeOn)> {
    let mailbox = crate::engine::messages::mailbox_for(engine, None)?;
    check_wait_mailbox(engine, &mailbox, types, timeout_ms)
}

/// The active Dispatches of `run` the coordinator should look at: those whose
/// projection requires action, and those that went `stale`.
pub fn attention_set(engine: &Engine, run: &RunId) -> Result<BTreeSet<String>> {
    let now = engine.clock.now();
    engine.store.read(|tx| {
        let mut set = BTreeSet::new();
        for row in dispatches::list_by_run(tx, run)? {
            if !matches!(row.status.as_str(), "pending" | "dispatched") {
                continue;
            }
            let attention = crate::engine::observe::project(tx, now, &row.id)?.attention;
            if attention.requires_action || attention.categories.iter().any(|c| c == "stale") {
                set.insert(row.id.to_string());
            }
        }
        Ok(set)
    })
}

pub(crate) fn check_wait_mailbox(
    engine: &Engine,
    mailbox: &str,
    types: &[String],
    timeout_ms: u64,
) -> Result<(Delivery, WokeOn)> {
    let run = engine
        .store
        .read(|tx| deliveries::run_for_mailbox(tx, mailbox))?;
    let _waiter = WaiterGuard::acquire(engine, &run, mailbox, types)?;
    let mut events = Events::subscribe(engine, &run)?;
    let deadline = deadline_after(timeout_ms);
    let mut next_keepalive = Instant::now() + KEEPALIVE;
    let mut next_tick = Instant::now() + engine.tick_interval;
    // Workers already requiring action when the wait began are the
    // coordinator's to handle; only a worker that newly needs it ends
    // the wait, so an unhandled prompt cannot make every wait return at once.
    let known = attention_set(engine, &run)?;
    let mut store_changed = true;
    let mut next_recheck = Instant::now() + ATTENTION_RECHECK;
    loop {
        let snapshot = signal::snapshot(&engine.paths.signal_path);
        if Instant::now() >= next_recheck {
            store_changed = true;
            next_recheck = Instant::now() + ATTENTION_RECHECK;
        }
        let matching = engine.store.read(|tx| {
            messages::select(tx, mailbox, types, true, 1, false).map(|rows| !rows.is_empty())
        })?;
        if matching {
            return Ok((deliveries::check_consume(engine, mailbox)?, WokeOn::Message));
        }
        let newly_stuck: Vec<String> = if store_changed {
            attention_set(engine, &run)?
                .difference(&known)
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        if !newly_stuck.is_empty() {
            notify_stuck(engine, &run, &newly_stuck);
            return Ok((
                deliveries::check_consume(engine, mailbox)?,
                WokeOn::Attention,
            ));
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok((deliveries::check_consume(engine, mailbox)?, WokeOn::Timeout));
        }
        if now >= next_keepalive {
            eprintln!("{{\"_keepalive\": true}}");
            next_keepalive = now + KEEPALIVE;
        }
        if now >= next_tick {
            // The ladder pass may type a nudge, and `stale` is a function of
            // time, so the attention set is recomputed on the next pass.
            wake::tick(engine, &run, Trigger::WaitTick)?;
            next_tick = now + engine.tick_interval;
            store_changed = true;
            continue;
        }
        if events.poll(engine) {
            store_changed = true;
            continue;
        }
        let until = deadline
            .min(next_keepalive)
            .min(next_tick)
            .min(now + EVENT_POLL);
        store_changed = snapshot.wait(until)?;
    }
}

/// One herdr notification (and `[notify] command` run) per Dispatch that
/// newly requires action (a newly `stale` one wakes the waiter but is not
/// announced): the Run objective as title, the projection's next
/// action as body. Only a waiter calls this, never a tick, so one event is
/// never announced twice.
fn notify_stuck(engine: &Engine, run: &RunId, dispatches: &[String]) {
    let now = engine.clock.now();
    for id in dispatches {
        let Ok(id) = id.parse::<crate::ids::DispatchId>() else {
            continue;
        };
        let found = engine.store.read(|tx| {
            let objective = crate::store::rows::runs::get(tx, run)?.map(|row| row.objective);
            let projection = crate::engine::observe::project(tx, now, &id)?;
            let action = projection.next_action.argv.join(" ");
            Ok((objective, action, projection.attention.requires_action))
        });
        // `stale` wakes the waiter but asks nothing of the person (A5.3).
        let Ok((Some(objective), action, true)) = found else {
            continue;
        };
        let body = if action.is_empty() {
            format!("worker {id} needs attention")
        } else {
            action
        };
        engine.notify(&objective, &body);
    }
}

pub fn ask_wait(engine: &Engine, message: &MessageId, timeout_ms: u64) -> Result<AskOutcome> {
    let run = engine.store.read(|tx| {
        questions::get(tx, message)?
            .map(|question| question.run_id)
            .ok_or_else(|| PodError::usage(format!("unknown question {message}")))
    })?;
    let mut events = Events::subscribe(engine, &run)?;
    let deadline = deadline_after(timeout_ms);
    let mut next_keepalive = Instant::now() + KEEPALIVE;
    let mut next_tick = Instant::now() + engine.tick_interval;
    loop {
        let snapshot = signal::snapshot(&engine.paths.signal_path);
        let question = engine
            .store
            .read(|tx| questions::get(tx, message))?
            .ok_or_else(|| PodError::usage(format!("unknown question {message}")))?;
        match question.status.as_str() {
            "answered" => {
                return Ok(AskOutcome::Answered(
                    question.answer_body.unwrap_or_default(),
                ));
            }
            "closed" => return Ok(AskOutcome::Timeout(message.clone())),
            _ => {}
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(AskOutcome::Timeout(message.clone()));
        }
        if now >= next_keepalive {
            eprintln!("{{\"_keepalive\": true}}");
            next_keepalive = now + KEEPALIVE;
        }
        if now >= next_tick {
            wake::tick(engine, &run, Trigger::WaitTick)?;
            next_tick = now + engine.tick_interval;
            continue;
        }
        if events.poll(engine) {
            continue;
        }
        let until = deadline
            .min(next_keepalive)
            .min(next_tick)
            .min(now + EVENT_POLL);
        let _ = snapshot.wait(until)?;
    }
}

/// Whether an active `check --wait` for this Run matches currently unread
/// mail in `mailbox`. A matching waiter takes the place of a typed nudge.
pub(crate) fn waiter_matches(engine: &Engine, mailbox: &str) -> Result<bool> {
    let run = engine
        .store
        .read(|tx| deliveries::run_for_mailbox(tx, mailbox))?;
    let path = waiter_path(engine, &run);
    // A waiter file whose lock nobody holds is left by a finished or killed
    // waiter and proves nothing.
    if !StateLock::is_held(&path).unwrap_or(false) {
        return Ok(false);
    }
    let mut encoded = String::new();
    let Ok(mut file) = File::open(path) else {
        return Ok(false);
    };
    if file.read_to_string(&mut encoded).is_err() {
        return Ok(false);
    }
    let Ok(waiter) = serde_json::from_str::<WaiterFile>(&encoded) else {
        return Ok(false);
    };
    if waiter.mailbox != mailbox {
        return Ok(false);
    }
    engine.store.read(|tx| {
        messages::select(tx, mailbox, &waiter.types, true, 1, false).map(|rows| !rows.is_empty())
    })
}

/// The herdr event stream a waiter holds for its Run's worker panes. It only
/// keeps observations current, so a stream that cannot be opened or breaks
/// (herdr restarting) degrades to waiting on the store alone instead of
/// failing the wait.
struct Events(Option<Box<dyn crate::herdr::EventStream>>);

impl Events {
    fn subscribe(engine: &Engine, run: &RunId) -> Result<Self> {
        let panes = panes_for_run(engine, run)?;
        Ok(Self(engine.herdr.events_subscribe(&panes).ok()))
    }

    /// Handles at most one pending event; true when one was handled.
    fn poll(&mut self, engine: &Engine) -> bool {
        let Some(stream) = self.0.as_mut() else {
            return false;
        };
        match stream.next(EVENT_READ) {
            Ok(Some(event)) => {
                // An observation that cannot be written must not end the wait.
                let _ = crate::engine::reconcile::waiter_event(engine, event);
                true
            }
            Ok(None) => false,
            Err(_) => {
                self.0 = None;
                false
            }
        }
    }
}

/// `timeout_ms` from now. A timeout too large for the platform clock waits
/// for a year instead of panicking on the overflow.
fn deadline_after(timeout_ms: u64) -> Instant {
    let now = Instant::now();
    now.checked_add(Duration::from_millis(timeout_ms))
        .unwrap_or_else(|| now + Duration::from_secs(365 * 24 * 60 * 60))
}

fn panes_for_run(engine: &Engine, run: &RunId) -> Result<Vec<String>> {
    engine.store.read(|tx| {
        let mut panes = Vec::new();
        for dispatch in dispatches::list_by_run(tx, run)? {
            if !matches!(dispatch.status.as_str(), "pending" | "dispatched") {
                continue;
            }
            if let Some(pane) = workers::get(tx, &dispatch.id)?.and_then(|worker| worker.pane_id) {
                panes.push(pane);
            }
        }
        Ok(panes)
    })
}

#[derive(Deserialize, Serialize)]
struct WaiterFile {
    mailbox: String,
    types: Vec<String>,
}

/// The Run's single `check --wait` slot: an exclusive lock on its waiter file,
/// which records the mailbox and types for `waiter_matches`.
struct WaiterGuard(StateLock);

impl WaiterGuard {
    fn acquire(engine: &Engine, run: &RunId, mailbox: &str, types: &[String]) -> Result<Self> {
        let path = waiter_path(engine, run);
        let store_error = |error: io::Error| {
            PodError::store(format!(
                "could not write waiter file {}: {error}",
                path.display()
            ))
        };
        let Some(mut lock) = StateLock::try_acquire(&path).map_err(store_error)? else {
            return Err(PodError::new(
                ErrorCode::WaiterExists,
                format!("a check --wait already exists for Run {run}"),
                &[
                    "Only one check --wait may run per Run; ask waits independently. Wait for the other pod check --wait to return, or end it, then retry.",
                ],
            ));
        };
        let value = serde_json::to_vec(&WaiterFile {
            mailbox: mailbox.to_owned(),
            types: types.to_vec(),
        })
        .map_err(|error| PodError::store(format!("could not encode waiter: {error}")))?;
        lock.replace_contents(&value).map_err(store_error)?;
        Ok(Self(lock))
    }
}

impl Drop for WaiterGuard {
    fn drop(&mut self) {
        let _ = self.0.replace_contents(b"");
    }
}

/// An exclusive advisory lock on an owner-only file in the state directory.
/// The OS releases it when the holder exits, however it exits, so a killed
/// `check --wait` or wake never leaves a stale lock that refuses every later
/// one. The file stays in place; only the lock means anything.
pub(crate) struct StateLock(File);

impl StateLock {
    /// `None` when another process (or another handle in this one) holds it.
    pub(crate) fn try_acquire(path: &Path) -> io::Result<Option<Self>> {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(path)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self(file))),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(error)) => Err(error),
        }
    }

    /// Whether a live holder has the lock on `path`.
    pub(crate) fn is_held(path: &Path) -> io::Result<bool> {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        match file.try_lock_shared() {
            Ok(()) => Ok(false),
            Err(TryLockError::WouldBlock) => Ok(true),
            Err(TryLockError::Error(error)) => Err(error),
        }
    }

    fn replace_contents(&mut self, value: &[u8]) -> io::Result<()> {
        self.0.set_len(0)?;
        self.0.rewind()?;
        self.0.write_all(value)
    }
}

fn waiter_path(engine: &Engine, run: &RunId) -> PathBuf {
    engine
        .paths
        .state_dir
        .join(format!("check-wait-{}.lock", run.as_str()))
}
