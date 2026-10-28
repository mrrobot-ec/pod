use std::path::Path;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use clap::{Args, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cli::{Ctx, output};
use crate::engine::{Engine, reconcile, runs};
use crate::error::{PodError, Result};
use crate::identity;
use crate::ids::{DispatchId, RunId, TaskId};
use crate::store::Store;

#[derive(Clone, Debug, Args)]
pub struct SkillsArgs {
    #[command(subcommand)]
    pub command: SkillsCommand,
}

#[derive(Clone, Debug, Subcommand)]
pub enum SkillsCommand {
    Get(SkillsGetArgs),
}

#[derive(Clone, Debug, Args)]
pub struct SkillsGetArgs {
    #[arg(value_enum)]
    pub role: SkillRole,
    #[arg(long)]
    pub reference: Option<String>,
    #[arg(long)]
    pub full: bool,
}

#[derive(Clone, Debug, ValueEnum)]
pub enum SkillRole {
    Coordinator,
    Worker,
}

#[derive(Clone, Debug, Args)]
pub struct LogArgs {
    #[arg(long)]
    pub run: String,
    /// Print the events so far, then each new one as it is appended, until
    /// `--timeout-ms` (default ten minutes) elapses.
    #[arg(long)]
    pub follow: bool,
    #[arg(long)]
    pub timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, Args)]
pub struct UsageArgs {
    /// Read a Claude Code status-line payload from stdin and store it,
    /// printing nothing.
    #[arg(long)]
    pub record: bool,
    /// Wire the Claude status line and herdr's tab bar to `pod usage --tick`.
    #[arg(long)]
    pub install: bool,
    /// With --install: leave herdr's config.toml alone.
    #[arg(long)]
    pub no_herdr: bool,
    /// Collapse or expand the tab bar entry (bound to prefix+u by --install).
    #[arg(long)]
    pub toggle: bool,
    /// Reuse a cached Codex reading younger than this (default 60000).
    #[arg(long)]
    pub max_age_ms: Option<u64>,
    /// The tab bar entry: print the line, and at most every 30 seconds run
    /// one wake ladder pass for every Run with an active Dispatch.
    #[arg(long)]
    pub tick: bool,
}

#[derive(Clone, Debug, Args)]
pub struct BoardArgs {
    #[arg(long)]
    pub run: Option<String>,
}

#[derive(Clone, Debug, Args)]
pub struct HookArgs {
    #[arg(value_enum)]
    pub action: HookAction,
}

#[derive(Clone, Debug, ValueEnum)]
pub enum HookAction {
    Startup,
    Event,
}

// ---- handlers -------------------------------------------------------------
// One function per command. `doctor` and `status` are T0; `skills` is T12,
// `log` is T2, `board` is T6, `hook` is T8. Each owner replaces only its stub.

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusPaths<'a> {
    state_dir: &'a Path,
    config_path: &'a Path,
    db_path: &'a Path,
    board_dir: &'a Path,
    signal_path: &'a Path,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StoreStatus<'a> {
    path: &'a Path,
    user_version: u32,
    ok: bool,
}

#[derive(Serialize)]
struct HerdrStatus {
    reachable: bool,
    version: Option<String>,
    protocol: Option<u32>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusReport<'a> {
    version: &'static str,
    session: &'a str,
    state_dir: &'a Path,
    paths: StatusPaths<'a>,
    store: StoreStatus<'a>,
    herdr: HerdrStatus,
    run: Option<serde_json::Value>,
    /// The bound Run's workers by terminal state, and the ones that look
    /// stalled: `blocked_on_prompt` (a live agent waiting on a person),
    /// `no_progress` (pod reported a stall: no heartbeat, message, activity or
    /// transcript write for `stall_minutes`), `brief_unconfirmed` (herdr took the
    /// brief but the transcript never showed it), `stale` (no heartbeat, activity,
    /// or message for 10 minutes) or idle at
    /// a prompt with an open Dispatch. Absent without a bound
    /// Run or when herdr cannot be asked.
    #[serde(skip_serializing_if = "Option::is_none")]
    workers: Option<serde_json::Value>,
}

/// Best effort: the bound Run's workers, projected as `worker-list` does.
fn workers_summary(ctx: &Ctx, run: &crate::ids::RunId) -> Option<serde_json::Value> {
    let engine = ctx.engine().ok()?;
    let list = crate::engine::lifecycle::list(&engine, Some(run), None, 100, None).ok()?;
    let stalled: Vec<serde_json::Value> = list
        .workers
        .iter()
        .filter_map(|view| {
            let stale = view
                .projection
                .attention
                .categories
                .iter()
                .any(|category| category == "stale");
            let no_progress = view
                .projection
                .attention
                .categories
                .iter()
                .any(|category| category == "stalled");
            let brief_unconfirmed = view
                .projection
                .attention
                .categories
                .iter()
                .any(|category| category == "brief_unconfirmed");
            let blocked_on_prompt =
                view.projection.liveness == "live" && view.projection.needs_input.count > 0;
            let idle_with_open_dispatch = view.dispatch.status == "dispatched"
                && view.projection.activity == "idle_at_prompt";
            let reason = if blocked_on_prompt {
                "blocked_on_prompt"
            } else if brief_unconfirmed {
                "brief_unconfirmed"
            } else if no_progress {
                "no_progress"
            } else if stale {
                "stale"
            } else if idle_with_open_dispatch {
                "idle_with_open_dispatch"
            } else {
                return None;
            };
            Some(serde_json::json!({
                "dispatchId": view.dispatch.id,
                "handle": view.dispatch.assignee_handle,
                "reason": reason,
                "nextAction": view.projection.next_action,
            }))
        })
        .collect();
    let reclaimable: Vec<serde_json::Value> = list
        .workers
        .iter()
        .filter(|view| view.worker.terminal_state == "reclaimable")
        .map(|view| {
            serde_json::json!({
                "dispatchId": view.dispatch.id,
                "handle": view.dispatch.assignee_handle,
                "nextAction": view.projection.next_action,
            })
        })
        .collect();
    Some(serde_json::json!({"counts": list.counts, "stalled": stalled, "reclaimable": reclaimable}))
}

pub fn doctor(ctx: &Ctx) -> Result<()> {
    let paths = ctx.paths()?;
    let herdr = ctx.herdr()?;
    let report = crate::doctor::run(&paths, herdr.as_ref());
    output::emit_ok(ctx.json, &report);
    if report.failed() {
        std::process::exit(1);
    }
    Ok(())
}

const CODEX_MAX_AGE_MS: u64 = 60_000;
const CODEX_READ_TIMEOUT_MS: u64 = 8_000;

/// `pod usage`: one line for herdr's tab bar (`--json` adds both records).
/// `--record` is Claude Code's status-line command; `--install` wires both.
pub fn usage(ctx: &Ctx, args: UsageArgs) -> Result<()> {
    use crate::usage::{self, Snapshot, UsageStore};
    let paths = ctx.paths()?;
    let store = UsageStore::new(paths.state_dir.join("usage"));
    let now = Utc::now();
    if args.install {
        let claude_statusline =
            usage::install_claude_statusline(&paths.claude_settings, "pod usage --record")?;
        let herdr_bar = if args.no_herdr {
            None
        } else {
            let outcome = usage::install_herdr_bar(&paths.herdr_config, "pod usage --tick")?;
            if outcome.changed {
                // Best effort: a server that is not running has nothing to reload.
                let _ = std::process::Command::new("herdr")
                    .args(["server", "reload-config"])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
            }
            Some(outcome)
        };
        output::emit_ok(
            ctx.json,
            &serde_json::json!({
                "claude_statusline": claude_statusline,
                "claude_settings": paths.claude_settings,
                "herdr_bar": herdr_bar,
                "herdr_config": paths.herdr_config,
            }),
        );
        return Ok(());
    }
    if args.toggle {
        let collapsed = store.toggle_collapsed()?;
        output::emit_ok(ctx.json, &serde_json::json!({"collapsed": collapsed}));
        return Ok(());
    }
    if args.record {
        let mut payload = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut payload)
            .map_err(|error| PodError::usage(format!("could not read stdin: {error}")))?;
        // Records silently: the bar is the only place usage is shown, and a
        // payload without rate limits (early in a session) is simply skipped.
        if let Ok(claude) = usage::claude_from_statusline(&payload, now) {
            store.save_claude(&claude)?;
        }
        // Every payload with a session id also feeds per-worker cost, with or
        // without rate limits.
        if let Ok(session) = usage::claude_session_from_statusline(&payload, now) {
            store.save_session(&session)?;
        }
        return Ok(());
    }
    let max_age = chrono::Duration::milliseconds(
        i64::try_from(args.max_age_ms.unwrap_or(CODEX_MAX_AGE_MS)).unwrap_or(i64::MAX),
    );
    let cached = store.load_codex()?;
    let codex = match cached {
        Some(codex) if now - codex.recorded_at <= max_age => Some(codex),
        cached => {
            let binary = std::env::var_os("POD_CODEX_BIN").unwrap_or_else(|| "codex".into());
            match usage::read_codex(&binary, now, CODEX_READ_TIMEOUT_MS) {
                Ok(codex) => {
                    store.save_codex(&codex)?;
                    Some(codex)
                }
                Err(_) => cached,
            }
        }
    };
    let snapshot = Snapshot {
        claude: store.load_claude()?,
        codex,
    };
    let collapsed = store.collapsed()?;
    let line = usage::bar_entry(&snapshot, now, collapsed);
    let tick = args.tick.then(|| bar_tick(ctx, &paths.state_dir, now));
    if ctx.json {
        let mut value = serde_json::json!({"claude": snapshot.claude, "codex": snapshot.codex, "collapsed": collapsed, "line": line});
        if let Some(tick) = tick {
            value["tick"] = tick;
        }
        output::emit_ok(true, &value);
    } else {
        println!("{line}");
    }
    Ok(())
}

/// The tab bar's clock (spec 3.2): one `BarTick` ladder pass for every Run
/// with an active Dispatch, at most once every 30 seconds. The stamp is
/// written before the pass so a second bar invocation skips it. Never runs
/// `reconcile`, and `BarTick` never waits, so it returns inside the bar's
/// 5-second timeout.
fn bar_tick(ctx: &Ctx, state_dir: &Path, now: DateTime<Utc>) -> Value {
    use crate::engine::wake::{self, Trigger};
    let Ok(engine) = ctx.engine() else {
        return serde_json::json!({"ran": false, "reason": "herdr unreachable"});
    };
    let stamp = state_dir.join("tick.at");
    let due = std::fs::read_to_string(&stamp)
        .ok()
        .and_then(|text| text.trim().parse::<DateTime<Utc>>().ok())
        .is_none_or(|last| now.signed_duration_since(last).num_seconds() >= 30);
    if !due {
        return serde_json::json!({"ran": false, "reason": "ticked less than 30 s ago"});
    }
    let _ = std::fs::write(&stamp, now.to_rfc3339());
    match wake::tick_all(&engine, Trigger::BarTick) {
        Ok(reports) => serde_json::json!({
            "ran": true,
            "runs": reports.len(),
            "attempts": reports.iter().map(|(_, report)| report.attempts.len()).sum::<usize>(),
        }),
        Err(error) => serde_json::json!({"ran": false, "reason": error.message}),
    }
}

pub fn status(ctx: &Ctx) -> Result<()> {
    let paths = ctx.paths()?;
    let store = Store::open(&paths)?;
    let herdr = ctx.herdr()?;
    let integrity_ok = store.integrity_ok()?;
    let user_version = store.user_version()?;
    let herdr_status = match herdr.ping() {
        Ok(info) => HerdrStatus {
            reachable: true,
            version: Some(info.version),
            protocol: Some(info.protocol),
        },
        Err(_) => HerdrStatus {
            reachable: false,
            version: None,
            protocol: None,
        },
    };
    // Best effort: status reports a degraded herdr in the body, so a caller
    // that cannot be resolved only means no bound Run is shown.
    let caller = identity::resolve(&Ctx::env, herdr.as_ref()).ok().flatten();
    let bound = caller
        .map(|caller| store.read(|tx| runs::bound_run(tx, &caller)))
        .transpose()?
        .flatten();
    let workers = bound.as_ref().and_then(|run| workers_summary(ctx, &run.id));
    let report = StatusReport {
        version: env!("CARGO_PKG_VERSION"),
        workers,
        session: &paths.session,
        state_dir: &paths.state_dir,
        paths: StatusPaths {
            state_dir: &paths.state_dir,
            config_path: &paths.config_path,
            db_path: &paths.db_path,
            board_dir: &paths.board_dir,
            signal_path: &paths.signal_path,
        },
        store: StoreStatus {
            path: &paths.db_path,
            user_version,
            ok: integrity_ok,
        },
        herdr: herdr_status,
        run: bound.map(|run| serde_json::json!({"id": run.id, "source": "bound"})),
    };
    output::emit_ok(ctx.json, &report);
    Ok(())
}

pub fn skills(ctx: &Ctx, args: SkillsArgs) -> Result<()> {
    let SkillsCommand::Get(get) = args.command;
    let role = match get.role {
        SkillRole::Coordinator => "coordinator",
        SkillRole::Worker => "worker",
    };
    let content = crate::skills::get(role, get.reference.as_deref(), get.full)?;
    if ctx.json {
        output::emit_ok(true, &serde_json::json!({ "content": content }));
    } else {
        print!("{content}");
    }
    Ok(())
}

pub fn log(ctx: &Ctx, args: LogArgs) -> Result<()> {
    let engine = ctx.read_engine()?;
    let run = RunId::from_str(&args.run)?;
    if !args.follow {
        let events = list_events(&engine, &run)?;
        output::emit_ok(ctx.json, &serde_json::json!({"events": events}));
        return Ok(());
    }
    follow_events(&engine, &run, args.timeout_ms.unwrap_or(600_000), ctx.json)
}

/// `log --follow`: one line per event, the existing ones first, then each
/// new one within a second of its commit (the store's signal file wakes the
/// reader), until the timeout. JSON lines with `--json`, else
/// `at type actor task dispatch detail`. A feed, not a receipt: exit 0 on
/// the timeout, and nothing is written to the store.
fn follow_events(engine: &Engine, run: &RunId, timeout_ms: u64, json: bool) -> Result<()> {
    use std::io::Write;
    let deadline = std::time::Instant::now()
        .checked_add(std::time::Duration::from_millis(timeout_ms))
        .unwrap_or_else(|| {
            std::time::Instant::now() + std::time::Duration::from_secs(365 * 24 * 60 * 60)
        });
    let mut last_sequence = 0i64;
    let stdout = std::io::stdout();
    loop {
        let snapshot = crate::store::signal::snapshot(&engine.paths.signal_path);
        let events = list_events(engine, run)?;
        let mut out = stdout.lock();
        let seen = last_sequence;
        for event in events.iter().filter(|event| event.sequence > seen) {
            if json {
                writeln!(out, "{}", serde_json::to_string(event).unwrap_or_default())
            } else {
                writeln!(
                    out,
                    "{} {} {} {} {} {}",
                    event.at,
                    event.kind,
                    event.actor,
                    event.task_id.as_ref().map_or("-", |id| id.as_str()),
                    event.dispatch_id.as_ref().map_or("-", |id| id.as_str()),
                    event.detail
                )
            }
            .map_err(|error| PodError::store(format!("could not write the log: {error}")))?;
            last_sequence = event.sequence;
        }
        out.flush()
            .map_err(|error| PodError::store(format!("could not write the log: {error}")))?;
        drop(out);
        let now = std::time::Instant::now();
        if now >= deadline {
            return Ok(());
        }
        let _ = snapshot.wait(deadline.min(now + std::time::Duration::from_secs(1)))?;
    }
}

/// herdr-dagr's install command, printed when dagr is absent. The owner
/// placeholder stays until herdr-dagr has a published repository.
const DAGR_INSTALL: &str = "herdr plugin install aemrebarut/herdr-dagr";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BoardReport {
    run: String,
    board_file: std::path::PathBuf,
    opened: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pane: Option<String>,
    dagr: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    install: Option<&'static str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

/// Rewrites the Run's `run.json` and, when a `dagr` executable is on `PATH`,
/// opens it in a new herdr tab with `DAGR_RUN` pointing at that file (spec
/// 12). Without `--run` it uses the Run bound to the calling terminal, or,
/// for the plugin's `open-board` action (no caller pane), to the focused one.
/// Opening is best effort: the board is optional, so a failure is a warning.
pub fn board(ctx: &Ctx, args: BoardArgs) -> Result<()> {
    let engine = ctx.read_engine()?;
    let run = match (args.run.as_deref(), &engine.caller) {
        (None, None) => match focused_run(&engine) {
            Some(run) => run,
            None => super::runs::selected_run(&engine, None)?,
        },
        (explicit, _) => super::runs::selected_run(&engine, explicit)?,
    };
    let now = engine.clock.now();
    let usage = crate::usage::UsageStore::new(engine.paths.state_dir.join("usage"));
    let value = engine
        .store
        .read(|tx| crate::board::export_with_costs(tx, &run, now, &usage))?;
    let board_file = crate::board::write(&engine.paths, &run, &value)?;
    let dagr = find_on_path("dagr");
    let mut warnings = Vec::new();
    let pane = dagr.as_deref().and_then(|dagr| {
        open_dagr(&engine, &board_file, dagr)
            .map_err(|error| warnings.push(format!("dagr was not opened: {}", error.message)))
            .ok()
    });
    output::emit_ok(
        ctx.json,
        &BoardReport {
            run: run.as_str().to_owned(),
            board_file,
            opened: pane.is_some(),
            pane,
            dagr: if dagr.is_some() { "present" } else { "absent" },
            install: dagr.is_none().then_some(DAGR_INSTALL),
            warnings,
        },
    );
    Ok(())
}

/// The Run bound to herdr's focused pane, for a caller that is not a pane.
fn focused_run(engine: &Engine) -> Option<crate::ids::RunId> {
    let pane = engine.herdr.pane_current().ok()?;
    engine
        .store
        .read(|tx| crate::store::rows::runs::bound_to_terminal(tx, &pane.terminal_id))
        .ok()
        .flatten()
        .map(|run| run.id)
}

/// Opens herdr-dagr on `board_file`: a new, focused tab in the caller's (or
/// the focused pane's) workspace whose shell has `DAGR_RUN` set, then the
/// `dagr view <file>` typed into it through safe input (paste, confirm,
/// Enter). pod never runs dagr itself; the tab's shell does. Returns the
/// board pane.
pub fn open_dagr(engine: &Engine, board_file: &Path, dagr: &Path) -> Result<String> {
    let caller_pane = engine.caller.as_ref().and_then(|c| c.pane_id.clone());
    let workspace = match caller_pane {
        Some(pane) => engine.herdr.pane_get(&pane)?.map(|pane| pane.workspace_id),
        None => None,
    };
    let workspace = match workspace {
        Some(workspace) => workspace,
        None => engine.herdr.pane_current()?.workspace_id,
    };
    let cwd = board_file.parent().unwrap_or(Path::new("/"));
    let board = board_file.display().to_string();
    let tab = engine.herdr.tab_create(
        &workspace,
        "board",
        cwd,
        true,
        &[("DAGR_RUN", board.as_str())],
    )?;
    crate::engine::lifecycle::wait_for_shell(engine, &tab.pane_id, 10_000)?;
    // `dagr view <file>`: dagr without a subcommand only prints its usage.
    let command = format!(
        "{} view {}",
        crate::engine::lifecycle::shell_quote(&dagr.display().to_string()),
        crate::engine::lifecycle::shell_quote(&board)
    );
    match crate::engine::safe_input::type_command(engine.herdr.as_ref(), &tab.pane_id, &command)? {
        crate::engine::safe_input::InputOutcome::Delivered => Ok(tab.pane_id),
        other => Err(PodError::new(
            crate::error::ErrorCode::HerdrError,
            format!("typing dagr into the board tab was not confirmed: {other:?}"),
            &["Run dagr in the board tab yourself; DAGR_RUN is already set there."],
        )),
    }
}

/// The first executable `name` on `PATH`; looked up, never run.
fn find_on_path(name: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                candidate
                    .metadata()
                    .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
            }
            #[cfg(not(unix))]
            {
                candidate.is_file()
            }
        })
}

/// Run by herdr's plugin hooks. Produces no output on success (commands.md).
pub fn hook(ctx: &Ctx, args: HookArgs) -> Result<()> {
    let engine = ctx.read_engine()?;
    match args.action {
        HookAction::Startup => {
            reconcile::startup(&engine)?;
        }
        HookAction::Event => match hook_event() {
            Some(event) => reconcile::event(&engine, event)?,
            // An absent, malformed, or unrecognized payload still gets a safe
            // full reconciliation; no event payload is executed or treated as
            // command text.
            None => {
                reconcile::startup(&engine)?;
            }
        },
    }
    Ok(())
}

/// Reads the event herdr hands a plugin hook: the event name in
/// `HERDR_PLUGIN_EVENT` and the protocol's `EventData` object (optionally
/// wrapped in `data` or `event`) in `HERDR_PLUGIN_EVENT_JSON`.
fn hook_event() -> Option<crate::herdr::HerdrEvent> {
    hook_event_from(
        Ctx::env("HERDR_PLUGIN_EVENT").as_deref(),
        &Ctx::env("HERDR_PLUGIN_EVENT_JSON")?,
    )
}

pub fn hook_event_from(name: Option<&str>, encoded: &str) -> Option<crate::herdr::HerdrEvent> {
    if let Ok(event) = serde_json::from_str(encoded) {
        return Some(event);
    }
    let value: Value = serde_json::from_str(encoded).ok()?;
    let data = value
        .get("data")
        .or_else(|| value.get("event").filter(|value| value.is_object()))
        .unwrap_or(&value);
    let kind = name
        .map(str::to_owned)
        .or_else(|| data.get("type").and_then(Value::as_str).map(str::to_owned))
        .or_else(|| value.get("type").and_then(Value::as_str).map(str::to_owned))
        .or_else(|| {
            value
                .get("event")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_default()
        .replace(['.', '-'], "_");
    let pane_id = data
        .get("pane_id")
        .or_else(|| data.get("paneId"))
        .and_then(Value::as_str)
        .map(str::to_owned)?;
    if kind.contains("pane_exited") {
        return Some(crate::herdr::HerdrEvent::PaneExited { pane_id });
    }
    if kind.contains("pane_closed") {
        return Some(crate::herdr::HerdrEvent::PaneClosed { pane_id });
    }
    if kind.contains("agent") && kind.contains("status") {
        // Protocol 22 names these `agent_status` and `state_labels` (a map
        // of label to reason), as the socket event stream does.
        let status = data
            .get("agent_status")
            .or_else(|| data.get("status"))
            .and_then(Value::as_str)
            .and_then(parse_agent_status)
            .unwrap_or(crate::herdr::AgentStatus::Unknown);
        let needs_input = match data.get("state_labels").and_then(Value::as_object) {
            Some(labels) => labels
                .iter()
                .map(|(key, value)| {
                    value
                        .as_str()
                        .filter(|value| !value.is_empty())
                        .unwrap_or(key)
                        .to_owned()
                })
                .collect(),
            None => data
                .get("needs_input")
                .or_else(|| data.get("needsInput"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
        };
        return Some(crate::herdr::HerdrEvent::AgentStatusChanged {
            pane_id,
            status,
            needs_input,
        });
    }
    None
}

fn parse_agent_status(value: &str) -> Option<crate::herdr::AgentStatus> {
    match value {
        "idle" => Some(crate::herdr::AgentStatus::Idle),
        "working" => Some(crate::herdr::AgentStatus::Working),
        "blocked" => Some(crate::herdr::AgentStatus::Blocked),
        "done" => Some(crate::herdr::AgentStatus::Done),
        "unknown" => Some(crate::herdr::AgentStatus::Unknown),
        _ => None,
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EventRow {
    pub sequence: i64,
    pub at: DateTime<Utc>,
    #[serde(rename = "type")]
    pub kind: String,
    pub task_id: Option<TaskId>,
    pub dispatch_id: Option<DispatchId>,
    pub actor: String,
    pub detail: Value,
}

pub fn list_events(engine: &Engine, run: &RunId) -> Result<Vec<EventRow>> {
    engine.store.read(|tx| {
        if runs::get(tx, run)?.is_none() {
            return Err(PodError::usage(format!("Run {run} was not found")));
        }
        Ok(crate::store::rows::events::list(tx, run)?
            .into_iter()
            .map(|row| EventRow {
                sequence: row.sequence.unwrap_or_default(),
                at: row.at,
                kind: row.r#type,
                task_id: row.task_id,
                dispatch_id: row.dispatch_id,
                actor: row.actor,
                detail: row.detail.unwrap_or(Value::Null),
            })
            .collect())
    })
}
