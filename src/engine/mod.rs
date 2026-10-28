use crate::clock::Clock;
use crate::config::Config;
use crate::herdr::Herdr;
use crate::identity::Caller;
use crate::ids::RunId;
use crate::paths::Paths;
use crate::store::Store;

pub mod deliveries;
pub mod gates;
pub mod groups;
pub mod lifecycle;
pub mod limits;
pub mod messages;
pub mod observe;
pub mod placement;
pub mod preamble;
pub mod questions;
pub mod receipts;
pub mod reconcile;
pub mod runs;
pub mod safe_input;
pub mod stall;
pub mod tasks;
pub mod transcripts;
pub mod trust;
pub mod waiting;
pub mod wake;

pub struct Engine {
    pub store: Store,
    pub herdr: Box<dyn Herdr>,
    pub clock: Box<dyn Clock>,
    pub config: Config,
    pub paths: Paths,
    pub caller: Option<Caller>,
    /// How long a worker's prompt must stay idle and unchanged before pod
    /// pastes into it (`POD_INPUT_SETTLE_MS`, default 3000; tests use 0).
    pub input_settle: std::time::Duration,
    /// How often a waiting coordinator (`check --wait`, `ask`) runs one wake
    /// ladder pass for its Run (default 60 s; tests use 200 ms).
    pub tick_interval: std::time::Duration,
    /// How long the paste fallback waits for an agent's empty input line
    /// (20 seconds; tests shorten it).
    pub ready_wait: std::time::Duration,
    /// How long the brief's transcript proof looks for the brief after herdr
    /// accepted it (`POD_BRIEF_PROOF_MS`, default 5 seconds; tests shorten it).
    pub brief_proof_wait: std::time::Duration,
    /// Warnings from optional effects (a notification) that the command's
    /// receipt carries; see [`Engine::take_warnings`].
    warnings: std::sync::Mutex<Vec<String>>,
}

/// The settle window from `POD_INPUT_SETTLE_MS`, default three seconds.
pub fn input_settle_from_env() -> std::time::Duration {
    std::env::var("POD_INPUT_SETTLE_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(std::time::Duration::from_millis)
        .unwrap_or(std::time::Duration::from_millis(3000))
}

/// The brief proof window from `POD_BRIEF_PROOF_MS`, default five seconds.
pub fn brief_proof_wait_from_env() -> std::time::Duration {
    std::env::var("POD_BRIEF_PROOF_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(std::time::Duration::from_millis)
        .unwrap_or(std::time::Duration::from_millis(5000))
}

impl Engine {
    pub fn new(
        store: Store,
        herdr: Box<dyn Herdr>,
        clock: Box<dyn Clock>,
        config: Config,
        paths: Paths,
        caller: Option<Caller>,
    ) -> Self {
        Self {
            store,
            herdr,
            clock,
            config,
            paths,
            caller,
            input_settle: input_settle_from_env(),
            tick_interval: std::time::Duration::from_secs(60),
            ready_wait: std::time::Duration::from_millis(safe_input::READY_WAIT_MS),
            brief_proof_wait: brief_proof_wait_from_env(),
            warnings: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// The warnings optional effects left since the last call, for the
    /// receipt's `warnings`.
    pub fn take_warnings(&self) -> Vec<String> {
        std::mem::take(&mut *self.warnings.lock().expect("warnings mutex poisoned"))
    }

    fn warn(&self, warning: String) {
        self.warnings
            .lock()
            .expect("warnings mutex poisoned")
            .push(warning);
    }

    /// Runs after every committing mutation of `run`: rewrites the Run's
    /// board file (`board::export`, then `board::write`), writes each worker
    /// workspace's herdr sidebar status, then makes the wake attempts of
    /// 8.2: the first attempt for each mailbox this mutation wrote, then the
    /// one eligible retry a mutating command is for every other mailbox of
    /// the Run with unread mail not already in its consumer's hands.
    ///
    /// The board and the sidebar are derived views rewritten after every
    /// commit, so a failure is reported on stderr and never fails the
    /// mutation that already committed (a failure would invite a retry). A
    /// Run that no longer exists (after `reset`) has its board directory
    /// removed instead.
    pub fn after_commit(&self, run: &RunId, mailboxes: &[String]) -> crate::error::Result<()> {
        let now = self.clock.now();
        let exported = self.store.read(|tx| {
            if crate::store::rows::runs::get(tx, run)?.is_none() {
                return Ok(None);
            }
            let usage = crate::usage::UsageStore::new(self.paths.state_dir.join("usage"));
            crate::board::export_with_costs(tx, run, now, &usage).map(Some)
        });
        let written = match exported {
            Ok(Some(value)) => crate::board::write(&self.paths, run, &value).map(drop),
            Ok(None) => crate::board::remove(&self.paths, run),
            Err(error) => Err(error),
        };
        if let Err(error) = written {
            eprintln!("pod: board export for {run} failed: {}", error.message);
        }
        self.write_sidebar(run);
        let mut pending: Vec<String> = mailboxes.to_vec();
        match wake::retry_mailboxes(self, run) {
            Ok(more) => pending.extend(more.into_iter().filter(|m| !mailboxes.contains(m))),
            Err(error) => eprintln!("pod: wake retries for {run} failed: {}", error.message),
        }
        for mailbox in &pending {
            if let Err(error) = wake::on_trigger(self, mailbox, wake::Trigger::MutatingCommand) {
                eprintln!("pod: wake for {mailbox} failed: {}", error.message);
            }
        }
        Ok(())
    }

    /// One notification event: a herdr notification, then the `[notify]
    /// command` with `NOTIFY_TITLE` and `NOTIFY_BODY` in its environment
    /// (5 second limit). Optional like the sidebar: an unreachable herdr is
    /// skipped silently and every other failure is one warning for the
    /// receipt ([`Engine::take_warnings`]), never an error, so a notification
    /// can never fail the mutation behind it.
    pub fn notify(&self, title: &str, body: &str) {
        match self.herdr.notification_show(title, Some(body), "request") {
            Ok(()) => {}
            Err(error) if error.code == crate::error::ErrorCode::HerdrUnreachable => {}
            Err(error) => self.warn(format!("herdr notification not shown: {}", error.message)),
        }
        let Some((program, args)) = self.config.notify.command.split_first() else {
            return;
        };
        if let Err(message) = run_notify_command(program, args, title, body) {
            self.warn(format!("notify command failed: {message}"));
        }
    }

    /// Spec 12: a short status in each worker workspace's herdr sidebar.
    /// Optional (spec 19): an unreachable herdr is skipped silently and a
    /// refusal is one warning, never an error.
    fn write_sidebar(&self, run: &RunId) {
        let Ok(statuses) = self
            .store
            .read(|tx| crate::board::sidebar_statuses(tx, run))
        else {
            return;
        };
        for (workspace, status) in statuses {
            // A workspace closes with its last tab; clearing a status there
            // is nothing to do, not a warning on every later command.
            if status.is_none() && matches!(self.herdr.workspace_get(&workspace), Ok(None)) {
                continue;
            }
            match self
                .herdr
                .workspace_report_metadata(&workspace, status.as_deref())
            {
                Ok(()) => {}
                Err(error) if error.code == crate::error::ErrorCode::HerdrUnreachable => return,
                Err(error) => {
                    eprintln!(
                        "pod: warning: herdr sidebar status not written: {}",
                        error.message
                    );
                    return;
                }
            }
        }
    }
}

const NOTIFY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

fn run_notify_command(
    program: &str,
    args: &[String],
    title: &str,
    body: &str,
) -> std::result::Result<(), String> {
    let mut child = std::process::Command::new(program)
        .args(args)
        .env("NOTIFY_TITLE", title)
        .env("NOTIFY_BODY", body)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|error| format!("{program}: {error}"))?;
    let deadline = std::time::Instant::now() + NOTIFY_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(format!("{program} exited with {status}")),
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{program} did not finish in 5 seconds"));
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
            Err(error) => return Err(format!("{program}: {error}")),
        }
    }
}
