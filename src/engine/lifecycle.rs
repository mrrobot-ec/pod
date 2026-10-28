use std::fs::{self, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::{Launch, launch_argv};
use crate::engine::{Engine, questions, tasks};
use crate::error::{ErrorCode, PodError, Result};
use crate::herdr::AgentInfo;
use crate::ids::{DispatchId, RunId, TaskId};
use crate::store::rows::{
    dispatches, events, gates, observations, runs, tasks as task_rows, wake_attempts,
    worker_archives, workers,
};

use super::observe;
pub use super::observe::Projection;
use super::placement::{CreationOptions, Placement};
pub use super::placement::{Setup, Worktree};
use super::preamble::{Decision, PreambleContext, Specialist, render_preamble};
pub use super::safe_input::InputOutcome;
use super::safe_input::READY_WAIT_MS;
use crate::paths::Paths;
use crate::usage::UsageStore;

const DEFAULT_START_TIMEOUT_MS: u64 = 30_000;
/// How long a new tab's shell may take to reach its prompt before
/// `agent_start` (bounded again by the start timeout).
const SHELL_READY_MS: u64 = 10_000;
const SHELL_POLL: Duration = Duration::from_millis(50);
const SHELL_RETRY: Duration = Duration::from_millis(250);

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StartInput {
    pub task: Option<TaskId>,
    pub spec: Option<String>,
    pub agent: Option<String>,
    pub terminal: Option<String>,
    pub worktree: Worktree,
    pub name: Option<String>,
    pub repo: Option<String>,
    pub base_branch: Option<String>,
    pub display_name: Option<String>,
    pub comment: Option<String>,
    pub setup: Setup,
    pub model: Option<String>,
    pub effort: Option<String>,
    #[serde(default)]
    pub fast: bool,
    #[serde(default)]
    pub yolo: bool,
    pub task_title: Option<String>,
    pub deps: Vec<TaskId>,
    pub parent: Option<TaskId>,
    pub retry_of: Option<DispatchId>,
    pub timeout_ms: Option<u64>,
    pub run: Option<RunId>,
    /// An explicit worker handle; a live collision is a usage error.
    #[serde(default)]
    pub handle: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LaunchRequest {
    pub agent: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    #[serde(default)]
    pub fast: bool,
    #[serde(default)]
    pub yolo: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LaunchReport {
    pub requested: LaunchRequest,
    pub effective: Option<Value>,
    /// `prompt` or `paste`: which route delivered the brief pointer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inject: Option<String>,
}

/// A `worker-start` receipt (spec 7.1), in the Orca contract's names.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartReceipt {
    pub run_id: RunId,
    pub task_id: TaskId,
    pub dispatch_id: DispatchId,
    pub handle: String,
    pub state: String,
    pub stage: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_stage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub setup: SetupReport,
    pub launch: LaunchReport,
    pub timeout_ms: u64,
    pub effects: Vec<Effect>,
    pub residual_resources: Vec<Resource>,
    /// Recovery and inspection commands for a start that is not `ready`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_commands: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// Cautions that do not fail the start, such as a Run near its budget.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// What the setup stage was asked to do and did (spec 7.3). Current and
/// existing worktrees never run setup: `requested` and `effective` are
/// `not_applicable`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SetupReport {
    pub requested: String,
    pub effective: String,
    /// `current_folder`, `existing_worktree`, `new_worktree`, or `reused_pane`.
    pub source: String,
    /// Whether the repository's config has a setup argv.
    pub hook_found: bool,
    pub startup_policy: String,
    /// `workers.setup_state`.
    pub state: String,
}

impl SetupReport {
    fn new(worktree: &Worktree, setup: &Setup) -> Self {
        let new = matches!(worktree, Worktree::NewChild | Worktree::NewTopLevel);
        let requested = match (new, setup) {
            (false, _) => "not_applicable",
            (true, Setup::Run) => "run",
            (true, Setup::Skip) => "skip",
            (true, Setup::Inherit) => "inherit",
        };
        Self {
            requested: requested.into(),
            effective: if new { "skip" } else { "not_applicable" }.into(),
            source: match worktree {
                Worktree::Current => "current_folder",
                Worktree::NewChild | Worktree::NewTopLevel => "new_worktree",
                Worktree::Path(_) | Worktree::Branch(_) => "existing_worktree",
            }
            .into(),
            hook_found: false,
            startup_policy: crate::config::RepoConfig::default().setup_policy,
            state: "none".into(),
        }
    }

    fn reused() -> Self {
        Self {
            source: "reused_pane".into(),
            ..Self::new(&Worktree::Current, &Setup::Run)
        }
    }
}

/// One side effect of a start: `worktree` (`created` or `reused`, with its
/// path), `setup` (with its state), `tab` and `pane` (with their IDs).
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Effect {
    pub kind: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}

/// Something a failed start left behind. pod creates its tabs unfocused, so
/// `surface` is `background`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Resource {
    pub kind: String,
    /// `agent`, `setup`, or `workspace`.
    pub role: String,
    pub action: String,
    pub id: String,
    pub surface: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchReceipt {
    pub dispatch: Option<dispatches::DispatchRow>,
    pub task_id: TaskId,
    pub to: String,
    pub injected: bool,
    pub outcome: Option<String>,
    pub preamble: Option<String>,
    pub dry_run: bool,
}

/// The receipt of `worker-stop`, `worker-abandon`, `worker-retain`, and
/// `worker-release`. `state` is the worker's `terminal_state` afterwards.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerReceipt {
    pub dispatch_id: DispatchId,
    pub state: String,
    pub dispatch: dispatches::DispatchRow,
    pub worker: workers::WorkerRow,
    pub closed_panes: Vec<String>,
    pub closed_tabs: Vec<String>,
    /// The spaces herdr had opened for the placement that release closed
    /// again, each left with only the shell herdr gave it.
    #[serde(default)]
    pub closed_workspaces: Vec<String>,
    pub left_alone: Vec<String>,
    pub archive: Archive,
    pub outcome: String,
}

/// The final-screen archive (spec 7.4): `preserved` with its size once
/// written, `absent` when there was no screen to keep.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Archive {
    pub source: String,
    pub status: String,
    pub bytes: Option<usize>,
}

/// `worker-show`: the rows, the latest observation, the 7.6 projection, and
/// the worker mailbox's wake ladder.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerView {
    pub dispatch: dispatches::DispatchRow,
    pub worker: workers::WorkerRow,
    pub observation: Option<observations::ObservationRow>,
    pub projection: Projection,
    pub wake_ladder: WakeLadder,
    /// The file holding this Dispatch's brief (`brief_path`).
    pub brief_path: PathBuf,
    /// What the worker's own status line reported (`workers.session_id`
    /// matched to a stored session record); `None` when unknown.
    pub cost_usd: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WakeLadder {
    pub exhausted: bool,
    pub attempts: Vec<wake_attempts::WakeAttemptRow>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WorkerListScope {
    pub source: String,
    pub run: Option<RunId>,
}

/// One `worker-list` page. `total` and `counts` cover the whole scope
/// (`counts` by `terminal_state`, before the `--terminal-state` filter).
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerList {
    pub scope: WorkerListScope,
    pub workers: Vec<WorkerView>,
    pub next_cursor: Option<String>,
    pub limit: u32,
    pub total: usize,
    pub counts: std::collections::BTreeMap<String, usize>,
}

pub fn worker_start(engine: &Engine, input: StartInput) -> Result<StartReceipt> {
    let caller = require_caller(engine)?;
    if input.task.is_none() == input.spec.is_none() {
        return Err(PodError::usage("exactly one of task or spec is required"));
    }
    if input.agent.is_none() == input.terminal.is_none() {
        return Err(PodError::usage(
            "exactly one of agent or terminal is required",
        ));
    }
    if input.terminal.is_some()
        && (input.model.is_some() || input.effort.is_some() || input.fast || input.yolo)
    {
        return Err(PodError::usage(
            "--model, --effort, --fast, and --yolo cannot be used when reusing --terminal",
        ));
    }
    if input.terminal.is_some() && input.worktree != Worktree::Current {
        return Err(PodError::usage(
            "--worktree cannot be used when reusing --terminal; the reused pane keeps its folder",
        ));
    }
    if input.retry_of.is_some() && input.task.is_none() {
        return Err(PodError::usage("--retry-of requires --task"));
    }
    let creation = CreationOptions {
        name: input.name.as_deref(),
        repo: input.repo.as_deref(),
        base_branch: input.base_branch.as_deref(),
        display_name: input.display_name.as_deref(),
        comment: input.comment.as_deref(),
        setup_was_explicit: !matches!(input.setup, Setup::Run),
    };
    super::placement::validate_creation_flags(&input.worktree, &creation)?;
    let run = selected_run(engine, input.run.as_ref())?;
    let depth = caller_depth(engine, &caller.terminal_id, &run)? + 1;
    if depth > engine.config.defaults.nested_depth_max {
        return Err(depth_exceeded());
    }
    // Resolve the agent before any write, so a bad name creates no Task.
    let launch = input
        .agent
        .as_deref()
        .map(|agent| {
            engine.config.resolve_agent(
                agent,
                input.model.as_deref(),
                input.effort.as_deref(),
                input.fast,
                input.yolo,
            )
        })
        .transpose()?;
    let mut budget_warning = None;
    if let Some(launch) = &launch {
        super::limits::check(engine, &launch.kind, &run.id)?;
        budget_warning = super::limits::budget_warning(engine, &run.id)?;
    }
    let task = if let Some(task) = input.task.clone() {
        engine.store.read(|tx| {
            let row = tasks::get(tx, &task)?.ok_or_else(|| task_not_found(&task))?;
            if row.run_id != run.id {
                return Err(task_not_found(&task));
            }
            tasks::startable(tx, &task, input.retry_of.as_ref())?;
            Ok(row)
        })?
    } else {
        let spec = input.spec.as_deref().expect("validated spec");
        // `--display-name` names a new worktree, not the Task. A created Task
        // with unmet dependencies is not startable, and is rolled back.
        engine.store.tx(|tx| {
            let row = tasks::create(
                tx,
                engine.clock.now(),
                &run.id,
                spec,
                input.task_title.as_deref(),
                None,
                &input.deps,
                input.parent.as_ref(),
                &caller.terminal_id,
            )?;
            tasks::startable(tx, &row.id, None)?;
            Ok(row)
        })?
    };
    let requested_name = input
        .agent
        .as_deref()
        .or(input.terminal.as_deref())
        .expect("validated target");
    let requested = LaunchRequest {
        agent: requested_name.to_owned(),
        model: input.model.clone(),
        effort: input.effort.clone(),
        fast: input.fast,
        yolo: input.yolo,
    };
    let dispatch_id = DispatchId::new();
    let inherited_count = engine
        .store
        .read(|tx| dispatches::max_failure_count_for_task(tx, &task.id))?;

    let timeout_ms = input.timeout_ms.unwrap_or(DEFAULT_START_TIMEOUT_MS);
    if let Some(agent) = input.terminal.as_deref() {
        return reuse_start(
            engine,
            &run.id,
            &task,
            &dispatch_id,
            agent,
            input.retry_of.as_ref(),
            depth,
            inherited_count,
            requested,
            timeout_ms,
        );
    }

    let launch = launch.expect("validated agent");
    // Everything that can refuse without side effects runs before the
    // Dispatch exists; from here on every failure settles it.
    let coordinator_pane = caller
        .pane_id
        .as_deref()
        .and_then(|pane| engine.herdr.pane_get(pane).transpose())
        .transpose()?
        .ok_or_else(|| {
            PodError::new(
                ErrorCode::NotInHerdr,
                "caller pane is unavailable",
                &["Run from the coordinator pane inside herdr."],
            )
        })?;
    let existing_handles = engine
        .herdr
        .agent_list()?
        .into_iter()
        .map(|agent| agent.name)
        .collect::<Vec<_>>();
    let handle = match input.handle.as_deref() {
        Some(wanted) => {
            let handle = handle_for(wanted, None, &[]);
            if existing_handles.contains(&handle) {
                return Err(PodError::usage(format!(
                    "handle {handle} is already a live agent; pick another --handle or reuse it with --terminal {handle}"
                )));
            }
            handle
        }
        None => {
            let place = match &input.worktree {
                Worktree::NewChild | Worktree::NewTopLevel => input.name.as_deref(),
                Worktree::Path(path) => path.file_name().and_then(|name| name.to_str()),
                Worktree::Branch(branch) => Some(branch.as_str()),
                Worktree::Current => None,
            };
            handle_for(requested_name, place, &existing_handles)
        }
    };
    insert_starting(
        engine,
        &run.id,
        &task.id,
        &dispatch_id,
        input.retry_of.as_ref(),
        depth,
        &handle,
        Some(launch.kind.as_str()),
        &requested,
        Some(&launch),
        inherited_count,
        engine.clock.now(),
    )?;
    let setup = std::cell::RefCell::new(SetupReport::new(&input.worktree, &input.setup));
    let started = start_stages(
        engine,
        &StageContext {
            run: &run,
            task: &task,
            dispatch: &dispatch_id,
            handle: &handle,
            input: &input,
            creation: &creation,
            coordinator_workspace: &coordinator_pane.workspace_id,
            launch: &launch,
            setup: &setup,
        },
    );
    let frame = StartFrame {
        run: &run.id,
        task: &task.id,
        dispatch: &dispatch_id,
        handle: &handle,
        requested,
        effective: Some(launch),
        setup: setup.into_inner(),
        timeout_ms,
    };
    let receipt = match started {
        Ok(outcome) => {
            engine.after_commit(&run.id, &[])?;
            started_receipt(engine, frame, outcome)
        }
        Err(failure) => failed_start(engine, frame, failure),
    };
    receipt.map(|mut receipt| {
        receipt.warnings.extend(budget_warning);
        receipt
    })
}

struct StageContext<'a> {
    run: &'a runs::RunRow,
    task: &'a tasks::TaskRow,
    dispatch: &'a DispatchId,
    handle: &'a str,
    input: &'a StartInput,
    creation: &'a CreationOptions<'a>,
    coordinator_workspace: &'a str,
    launch: &'a Launch,
    /// Filled in once placement names the repository.
    setup: &'a std::cell::RefCell<SetupReport>,
}

/// Why a start stage failed: the stage, whether a worker pane already exists
/// (so the worker is left `reclaimable`), and the error.
struct StageFailure {
    stage: &'static str,
    pane_created: bool,
    error: PodError,
}

trait AtStage<T> {
    fn at(self, stage: &'static str, pane_created: bool) -> std::result::Result<T, StageFailure>;
}

impl<T> AtStage<T> for Result<T> {
    fn at(self, stage: &'static str, pane_created: bool) -> std::result::Result<T, StageFailure> {
        self.map_err(|error| StageFailure {
            stage,
            pane_created,
            error,
        })
    }
}

/// Placement through Record (spec 7.1 stages 2 to 7) for a newly launched
/// worker.
fn start_stages(
    engine: &Engine,
    ctx: &StageContext<'_>,
) -> std::result::Result<InputOutcome, StageFailure> {
    let dispatch = ctx.dispatch;
    let placement = super::placement::place(
        engine,
        &ctx.input.worktree,
        ctx.creation,
        ctx.coordinator_workspace,
        Path::new(&ctx.run.home_cwd),
    )
    .at("placement", false)?;
    record_placement(engine, dispatch, &placement).at("placement", false)?;
    let mut effects = vec!["placement_selected".to_owned()];
    if placement.repo.is_some() {
        effects.push("worktree_created".into());
    }
    for workspace in &placement.opened_workspaces {
        effects.push(format!("workspace_opened:{workspace}"));
    }
    store_effects(engine, dispatch, &effects).at("placement", false)?;
    let timeout_ms = ctx.input.timeout_ms.unwrap_or(DEFAULT_START_TIMEOUT_MS);
    let repo_config = placement
        .repo
        .as_ref()
        .and_then(|repo| engine.config.repos.get(repo));
    if let Some(repo) = repo_config {
        let mut report = ctx.setup.borrow_mut();
        report.hook_found = !repo.setup.is_empty();
        report.startup_policy = repo.setup_policy.clone();
    }
    let setup = repo_config
        .filter(|repo| !matches!(ctx.input.setup, Setup::Skip) && !repo.setup.is_empty());
    let mut worker_tab = placement.initial_tab.clone();
    if let Some(repo) = setup {
        let setup_tab = worker_tab.take().expect("new worktree tab");
        workers_stage(engine, dispatch, "setup").at("setup", false)?;
        start_setup(
            engine,
            dispatch,
            &setup_tab.pane_id,
            &repo.setup,
            &repo.setup_policy,
            timeout_ms,
        )
        .at("setup", true)?;
        effects.push("setup_started".into());
        store_effects(engine, dispatch, &effects).at("setup", true)?;
    }
    workers_stage(engine, dispatch, "pane").at("pane", false)?;
    let label = tab_label(ctx.handle, ctx.task);
    let tab = match worker_tab {
        Some(tab) => {
            // The worktree's own tab carries herdr's label until now.
            engine
                .herdr
                .tab_rename(&tab.tab_id, &label)
                .at("pane", false)?;
            tab
        }
        None => {
            let tab = engine
                .herdr
                .tab_create(&placement.workspace_id, &label, &placement.path, false, &[])
                .at("pane", false)?;
            effects.push("tab_created".into());
            tab
        }
    };
    effects.push("pane_created".into());
    engine
        .store
        .tx(|tx| {
            workers::update_tab_id(tx, dispatch, Some(&tab.tab_id))?;
            workers::update_pane_id(tx, dispatch, Some(&tab.pane_id))?;
            workers::update_owns_pane(tx, dispatch, true)?;
            workers::update_effects(tx, dispatch, Some(&json!(effects)))?;
            Ok(())
        })
        .at("pane", true)?;
    let pane = engine
        .herdr
        .pane_get(&tab.pane_id)
        .and_then(|pane| {
            pane.ok_or_else(|| {
                PodError::new(
                    ErrorCode::HerdrError,
                    "herdr did not return the created pane",
                    &["Inspect herdr and release the residual worker tab."],
                )
            })
        })
        .at("pane", true)?;
    engine
        .store
        .tx(|tx| {
            workers::update_terminal_id(tx, dispatch, Some(&pane.terminal_id))?;
            dispatches::update_assignee_terminal_id(tx, dispatch, Some(&pane.terminal_id))?;
            Ok(())
        })
        .at("pane", true)?;
    wait_for_shell(engine, &tab.pane_id, timeout_ms).at("pane", true)?;
    workers_stage(engine, dispatch, "agent").at("agent", true)?;
    // herdr runs the kind's own executable and appends `args`, so the
    // command name `launch_argv` leads with must not go along: the agent
    // takes a stray positional as its first prompt (seen live 2026-10-21).
    let mut argv = launch_argv(ctx.launch).split_off(1);
    // An unattended worker must get past its agent's folder-trust prompt
    // (see `trust`). Codex keys a linked worktree on the repository it came
    // from, so that root is trusted as well. A config pod cannot update is
    // a warning in the effects; the start then stops at the prompt as before.
    if ctx.launch.yolo {
        let mut folders = vec![placement.path.clone()];
        if let Some(repo) = &placement.repo
            && !folders.contains(repo)
        {
            folders.push(repo.clone());
        }
        let folders = folders
            .into_iter()
            .map(|folder| std::fs::canonicalize(&folder).unwrap_or(folder))
            .collect::<Vec<_>>();
        let config = match ctx.launch.kind.as_str() {
            "codex" => Some(&engine.paths.codex_config),
            "claude" => Some(&engine.paths.claude_config),
            _ => None,
        };
        if let Some(config) = config {
            match super::trust::ensure(&ctx.launch.kind, config, &folders) {
                Ok(added) => {
                    for folder in added {
                        effects.push(format!("folder_trusted:{}", folder.display()));
                    }
                }
                Err(error) => effects.push(format!("folder_trust_skipped:{}", error.message)),
            }
            store_effects(engine, dispatch, &effects).at("agent", true)?;
        }
    }
    let assigned_session = assign_session(ctx.launch);
    if let Some(session) = &assigned_session {
        argv.extend(["--session-id".to_owned(), session.clone()]);
    }
    let agent = start_agent(
        engine,
        ctx.handle,
        &ctx.launch.kind,
        &tab.pane_id,
        &argv,
        timeout_ms,
    )
    .at("agent", true)?;
    record_agent(
        engine,
        dispatch,
        &agent,
        &ctx.launch.kind,
        assigned_session.as_deref(),
    )
    .at("agent", true)?;
    workers_stage(engine, dispatch, "inject").at("inject", true)?;
    let preamble =
        preamble_for(engine, &ctx.run.id, ctx.task, dispatch, ctx.handle).at("inject", true)?;
    let outcome =
        inject(engine, &ctx.run.id, dispatch, ctx.handle, &preamble).at("inject", true)?;
    if let InputOutcome::Failed(detail) | InputOutcome::Skipped(detail) = &outcome {
        return Err(inject_failure(detail, true));
    }
    record_dispatched(engine, &ctx.run.id, &ctx.task.id, dispatch, ctx.handle)
        .at("record", true)?;
    Ok(outcome)
}

fn inject_failure(detail: &str, pane_created: bool) -> StageFailure {
    StageFailure {
        stage: "inject",
        pane_created,
        error: PodError::new(
            ErrorCode::HerdrError,
            format!("preamble injection typed nothing: {detail}"),
            &["Inspect the worker pane, then retry the start."],
        ),
    }
}

fn store_effects(engine: &Engine, dispatch: &DispatchId, effects: &[String]) -> Result<()> {
    engine
        .store
        .tx(|tx| workers::update_effects(tx, dispatch, Some(&json!(effects))))
}

/// What every start receipt repeats: who started, what was asked, and the
/// bound on the start stages.
struct StartFrame<'a> {
    run: &'a RunId,
    task: &'a TaskId,
    dispatch: &'a DispatchId,
    handle: &'a str,
    requested: LaunchRequest,
    effective: Option<Launch>,
    setup: SetupReport,
    timeout_ms: u64,
}

impl StartFrame<'_> {
    /// The receipt from the stored rows; the caller sets the ending.
    fn receipt(self, engine: &Engine) -> Result<(StartReceipt, workers::WorkerRow)> {
        let worker = engine
            .store
            .read(|tx| workers::get(tx, self.dispatch))?
            .expect("worker exists");
        let mut setup = self.setup;
        let effects = effects_of(&worker, &mut setup);
        let receipt = StartReceipt {
            run_id: self.run.clone(),
            task_id: self.task.clone(),
            dispatch_id: self.dispatch.clone(),
            handle: self.handle.into(),
            state: worker.state.clone(),
            stage: worker.stage.clone().unwrap_or_default(),
            failed_stage: None,
            last_error: worker.last_error.clone(),
            setup,
            launch: LaunchReport {
                requested: self.requested,
                effective: self.effective.map(|value| json!(value)),
                inject: inject_of(&worker),
            },
            timeout_ms: self.timeout_ms,
            effects,
            residual_resources: Vec::new(),
            next_commands: Vec::new(),
            outcome: None,
            warnings: Vec::new(),
        };
        Ok((receipt, worker))
    }
}

fn pod_command(command: &str, dispatch: &DispatchId) -> String {
    format!("pod {command} --dispatch {dispatch}")
}

/// Which route delivered the brief pointer, from the stored effects.
fn inject_of(worker: &workers::WorkerRow) -> Option<String> {
    let names: Vec<String> = worker
        .effects
        .clone()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();
    names
        .iter()
        .find_map(|name| name.strip_prefix("inject:"))
        .map(str::to_owned)
}

/// The stored effect names as receipt objects. Also completes the setup
/// report from the worker row.
fn effects_of(worker: &workers::WorkerRow, setup: &mut SetupReport) -> Vec<Effect> {
    let names: Vec<String> = worker
        .effects
        .clone()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();
    let has = |name: &str| names.iter().any(|effect| effect == name);
    let effect = |kind: &str, action: &str, id: Option<&str>, state: Option<&str>| Effect {
        kind: kind.into(),
        action: action.into(),
        id: id.map(str::to_owned),
        state: state.map(str::to_owned),
    };
    setup.state = worker.setup_state.clone().unwrap_or_else(|| "none".into());
    if has("setup_started") {
        setup.effective = "run".into();
    }
    let mut effects = Vec::new();
    if has("placement_selected") || has("pane_reused") {
        let action = if has("worktree_created") {
            "created"
        } else {
            "reused"
        };
        if let Some(path) = worker.worktree_path.as_deref() {
            effects.push(effect("worktree", action, Some(path), None));
        }
    }
    let setup_action = if has("setup_started") {
        "started"
    } else if setup.source == "new_worktree" {
        "skipped"
    } else {
        "not_applicable"
    };
    effects.push(effect("setup", setup_action, None, Some(&setup.state)));
    if has("tab_created")
        && let Some(tab) = worker.tab_id.as_deref()
    {
        effects.push(effect("tab", "created", Some(tab), None));
    }
    for (name, action) in [("pane_created", "created"), ("pane_reused", "reused")] {
        if has(name)
            && let Some(pane) = worker.pane_id.as_deref()
        {
            effects.push(effect("pane", action, Some(pane), None));
        }
    }
    // The spaces herdr opened for the placement (spec 7.2), closed on release.
    for workspace in names
        .iter()
        .filter_map(|name| name.strip_prefix("workspace_opened:"))
    {
        effects.push(effect("workspace", "opened", Some(workspace), None));
    }
    // The brief file every worker gets (spec 7.1 step 6).
    for path in names
        .iter()
        .filter_map(|name| name.strip_prefix("preamble_file:"))
    {
        effects.push(effect("preamble_file", "created", Some(path), None));
    }
    // Why the prompt path gave way to paste, when it did (spec 7.1 step 6).
    for reason in names
        .iter()
        .filter_map(|name| name.strip_prefix("prompt_error:"))
    {
        effects.push(effect(
            "prompt_error",
            "fell_back_to_paste",
            Some(reason),
            None,
        ));
    }
    effects
}

/// The receipt of a start that reached Record. A preamble that was typed but
/// not confirmed is `dispatched_unobserved`: the Dispatch stays recorded and
/// the preamble is never typed again (spec 7.1 step 6).
fn started_receipt(
    engine: &Engine,
    frame: StartFrame<'_>,
    outcome: InputOutcome,
) -> Result<StartReceipt> {
    let dispatch = frame.dispatch.clone();
    let (mut receipt, _) = frame.receipt(engine)?;
    receipt.warnings.extend(engine.take_warnings());
    if !matches!(outcome, InputOutcome::Delivered) {
        receipt.outcome = Some("dispatched_unobserved".into());
        receipt.last_error.get_or_insert_with(|| {
            "the preamble was pasted but pod could not confirm it on screen, so Enter was \
             withheld; the Dispatch stays recorded and the preamble is never typed again"
                .into()
        });
        receipt.next_commands = vec![
            pod_command("worker-show", &dispatch),
            pod_command("worker-read", &dispatch),
        ];
    }
    Ok(receipt)
}

#[allow(clippy::too_many_arguments)]
fn insert_starting(
    engine: &Engine,
    run: &RunId,
    task: &TaskId,
    dispatch: &DispatchId,
    retry_of: Option<&DispatchId>,
    depth: u32,
    handle: &str,
    kind: Option<&str>,
    requested: &LaunchRequest,
    effective: Option<&Launch>,
    failure_count: u32,
    now: DateTime<Utc>,
) -> Result<()> {
    engine.store.tx(|tx| {
        dispatches::insert(
            tx,
            &dispatches::DispatchRow {
                id: dispatch.clone(),
                run_id: run.clone(),
                task_id: task.clone(),
                status: "pending".into(),
                retry_of_dispatch_id: retry_of.cloned(),
                failure_count,
                last_failure: None,
                termination_reason: None,
                termination_target: None,
                depth,
                assignee_handle: Some(handle.into()),
                assignee_terminal_id: None,
                assignee_pid: None,
                agent_kind: kind.map(str::to_owned),
                launch_requested: Some(json!(requested)),
                launch_effective: effective.map(|value| json!(value)),
                dispatched_at: None,
                last_heartbeat_at: None,
                last_phase: None,
                completed_at: None,
                created_at: now,
            },
        )?;
        workers::insert(
            tx,
            &workers::WorkerRow {
                dispatch_id: dispatch.clone(),
                state: "starting".into(),
                stage: Some("placement".into()),
                placement: None,
                worktree_path: None,
                workspace_id: None,
                tab_id: None,
                pane_id: None,
                terminal_id: None,
                owns_pane: false,
                user_owned: false,
                user_owned_reason: None,
                setup_state: Some("none".into()),
                setup_pane_id: None,
                preamble_input: None,
                effects: Some(json!([])),
                residual_resources: Some(json!([])),
                terminal_state: "active".into(),
                retained_reason: None,
                session_id: None,
                transcript_path: None,
                transcript_proof: None,
                last_error: None,
                created_at: now,
                updated_at: now,
            },
        )?;
        Ok(())
    })
}

fn record_placement(engine: &Engine, dispatch: &DispatchId, placement: &Placement) -> Result<()> {
    engine.store.tx(|tx| {
        workers::update_placement(tx, dispatch, Some(&json!(placement.kind)))?;
        workers::update_worktree_path(tx, dispatch, Some(&placement.path.display().to_string()))?;
        workers::update_workspace_id(tx, dispatch, Some(&placement.workspace_id))?;
        Ok(())
    })
}

fn workers_stage(engine: &Engine, dispatch: &DispatchId, stage: &str) -> Result<()> {
    engine.store.tx(|tx| {
        workers::update_stage(tx, dispatch, Some(stage))?;
        workers::update_updated_at(tx, dispatch, engine.clock.now())
    })
}

/// The session ID pod gives a Claude worker with `--session-id`, which is
/// spec 11's second proof of session. A preset whose own arguments already
/// pick a session (`--session-id`, `--resume`, `--continue`) gets none.
fn assign_session(launch: &Launch) -> Option<String> {
    let picks_session = launch.args.iter().any(|arg| {
        matches!(
            arg.split('=').next().unwrap_or_default(),
            "--session-id" | "--resume" | "-r" | "--continue" | "-c"
        )
    });
    (launch.kind == "claude" && !picks_session).then(|| uuid::Uuid::new_v4().to_string())
}

/// The session proof of an agent pod did not launch for this Dispatch:
/// herdr's report, else the proof stored for the same agent in the same pane
/// by an earlier Dispatch of the same agent process (a reused Claude keeps
/// its `--session-id`).
/// Moves a pane's cleanup from the worker that last held it to `dispatch`
/// (spec 7.4, reuse and `dispatch --to`): a finished previous worker
/// (`reclaimable` or `retained`) is recorded `released`, and pod's ownership
/// of the pane, its tab, its worktree, and the spaces herdr opened for its
/// placement (`workspace_opened:<id>` effects, in order) carry over, so a
/// later release of the new Dispatch closes what pod created. Returns the
/// carried effects. A user takeover is permanent and
/// carries too; a retained worker is not taken over and carries nothing.
fn carry_pane(
    tx: &crate::store::Tx<'_>,
    dispatch: &DispatchId,
    previous: Option<&workers::WorkerRow>,
) -> Result<Vec<String>> {
    let Some(old) = previous else {
        return Ok(Vec::new());
    };
    if matches!(old.terminal_state.as_str(), "reclaimable" | "retained") {
        workers::update_terminal_state(tx, &old.dispatch_id, "released")?;
    }
    workers::update_tab_id(tx, dispatch, old.tab_id.as_deref())?;
    workers::update_worktree_path(tx, dispatch, old.worktree_path.as_deref())?;
    workers::update_owns_pane(tx, dispatch, old.owns_pane)?;
    if taken_over(old) {
        workers::update_user_owned(tx, dispatch, true, old.user_owned_reason.as_deref())?;
    }
    let carried: Vec<String> = opened_workspaces_of(old)
        .into_iter()
        .map(|workspace| format!("workspace_opened:{workspace}"))
        .collect();
    if !carried.is_empty() {
        workers::update_effects(tx, dispatch, Some(&json!(carried)))?;
    }
    Ok(carried)
}

/// True when a person took the pane over (spec 7.4). Before 0.2.11 `retain`
/// also set `user_owned`, with the retained reason as its reason; such a row
/// is only retained, so the two reasons being equal does not count.
fn taken_over(worker: &workers::WorkerRow) -> bool {
    worker.user_owned && worker.user_owned_reason != worker.retained_reason
}

fn carry_session(
    tx: &crate::store::Tx<'_>,
    dispatch: &DispatchId,
    agent: &AgentInfo,
    previous: Option<(&workers::WorkerRow, Option<u32>)>,
) -> Result<()> {
    let path = agent
        .session_path
        .as_ref()
        .map(|path| path.display().to_string());
    let (session_id, path, proof) = if agent.session_id.is_some() || path.is_some() {
        (agent.session_id.clone(), path, Some("herdr".to_owned()))
    } else if let Some((previous, _)) = previous.filter(|(w, pid)| {
        w.pane_id.as_deref() == Some(agent.pane_id.as_str())
            && *pid == Some(agent.pid)
            && w.transcript_proof.is_some()
    }) {
        (
            previous.session_id.clone(),
            previous.transcript_path.clone(),
            previous.transcript_proof.clone(),
        )
    } else {
        return Ok(());
    };
    workers::update_session_id(tx, dispatch, session_id.as_deref())?;
    workers::update_transcript_path(tx, dispatch, path.as_deref())?;
    workers::update_transcript_proof(tx, dispatch, proof.as_deref())
}

/// Records the agent's process and its session proof (spec 11, in order):
/// herdr's report of the pane's session, else the ID pod assigned at launch.
/// herdr starts an agent only in a pane whose shell sits at its prompt with
/// nothing else in the foreground, and a just-created tab can be a moment
/// short of that. Waits for it, bounded by the start timeout; a shell that
/// never gets there fails the start at stage `pane`.
pub(crate) fn wait_for_shell(engine: &Engine, pane: &str, timeout_ms: u64) -> Result<()> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms.min(SHELL_READY_MS));
    loop {
        let last = match engine.herdr.pane_process_info(pane) {
            Ok(info) if info.shell_pid != 0 && info.foreground_pid == info.shell_pid => {
                return Ok(());
            }
            Ok(info) => format!(
                "process {} is in the foreground, not the shell {}",
                info.foreground_pid, info.shell_pid
            ),
            Err(error) => error.message,
        };
        if Instant::now() >= deadline {
            return Err(PodError::new(
                ErrorCode::HerdrError,
                format!("the new pane's shell did not reach its prompt in time: {last}"),
                &[
                    "Inspect the worker tab; a shell startup file may run a foreground command.",
                    "Release the failed worker, then retry with a larger --timeout-ms.",
                ],
            ));
        }
        thread::sleep(SHELL_POLL);
    }
}

/// `agent_start`, retried while herdr refuses the pane as "not an available
/// shell" (it checks before starting anything), within the same bound as
/// `wait_for_shell`.
fn start_agent(
    engine: &Engine,
    handle: &str,
    kind: &str,
    pane: &str,
    argv: &[String],
    timeout_ms: u64,
) -> Result<AgentInfo> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms.min(SHELL_READY_MS));
    loop {
        match engine
            .herdr
            .agent_start(handle, kind, pane, argv, timeout_ms)
        {
            Err(error)
                if error.message.contains("not an available shell")
                    && Instant::now() < deadline =>
            {
                thread::sleep(SHELL_RETRY);
            }
            started => return started,
        }
    }
}

/// Records the agent herdr detected. The Dispatch's `agent_kind` is the kind
/// pod launched (`kind`), not herdr's detection, which reads `unknown` for an
/// agent run under a command alias until it reports itself.
fn record_agent(
    engine: &Engine,
    dispatch: &DispatchId,
    agent: &AgentInfo,
    kind: &str,
    assigned_session: Option<&str>,
) -> Result<()> {
    let reported = agent.session_id.is_some() || agent.session_path.is_some();
    let (session_id, path, proof) = if reported {
        (
            agent.session_id.as_deref(),
            agent
                .session_path
                .as_ref()
                .map(|path| path.display().to_string()),
            Some("herdr"),
        )
    } else {
        (
            assigned_session,
            None,
            assigned_session.map(|_| "pod_assigned"),
        )
    };
    let screen = super::reconcile::blocked_screen(engine, &agent.pane_id, &agent.status);
    let escalation = engine.store.tx(|tx| {
        // herdr detected the agent it started: presence is proven (7.6).
        let escalation = observe::record_herdr_state(
            tx,
            engine.clock.now(),
            dispatch,
            agent.status.clone(),
            &agent.needs_input,
            Some("present"),
            screen.as_deref(),
        )?;
        dispatches::update_assignee_pid(tx, dispatch, Some(agent.pid))?;
        dispatches::update_agent_kind(tx, dispatch, Some(kind))?;
        workers::update_session_id(tx, dispatch, session_id)?;
        workers::update_transcript_path(tx, dispatch, path.as_deref())?;
        workers::update_transcript_proof(tx, dispatch, proof)?;
        Ok(escalation)
    })?;
    super::reconcile::announce(engine, escalation)
}

fn record_dispatched(
    engine: &Engine,
    run: &RunId,
    task: &TaskId,
    dispatch: &DispatchId,
    actor: &str,
) -> Result<()> {
    let now = engine.clock.now();
    engine.store.tx(|tx| {
        dispatches::update_status(tx, dispatch, "dispatched")?;
        dispatches::update_dispatched_at(tx, dispatch, Some(now))?;
        task_rows::update_status(tx, task, "dispatched")?;
        workers::update_state(tx, dispatch, "ready")?;
        workers::update_stage(tx, dispatch, Some("record"))?;
        workers::update_updated_at(tx, dispatch, now)?;
        events::append(
            tx,
            &events::EventRow {
                sequence: None,
                run_id: run.clone(),
                at: now,
                r#type: "dispatch.dispatched".into(),
                task_id: Some(task.clone()),
                dispatch_id: Some(dispatch.clone()),
                actor: actor.into(),
                detail: Some(json!({})),
            },
        )
    })
}

fn failed_start(
    engine: &Engine,
    frame: StartFrame<'_>,
    failure: StageFailure,
) -> Result<StartReceipt> {
    let StageFailure {
        stage,
        pane_created,
        error,
    } = failure;
    let (run, task, dispatch, handle) = (frame.run, frame.task, frame.dispatch, frame.handle);
    let now = engine.clock.now();
    // A failure before any pane exists (`preflight`, `placement`) is pod's
    // or herdr's, not the worker's: nothing ran. It does not count toward the
    // breaker (spec 7.5); the Task returns to `ready` with its count kept.
    let counted = !matches!(stage, "preflight" | "placement");
    engine.store.tx(|tx| {
        let current = dispatches::max_failure_count_for_task(tx, task)?;
        let next = if counted { current + 1 } else { current };
        let broken = counted && next >= 3;
        let status = if broken { "circuit_broken" } else { "failed" };
        dispatches::update_status(tx, dispatch, status)?;
        dispatches::update_failure_count(tx, dispatch, next)?;
        dispatches::update_last_failure(tx, dispatch, Some(&error.message))?;
        dispatches::update_completed_at(tx, dispatch, Some(now))?;
        task_rows::update_status(tx, task, if broken { "failed" } else { "ready" })?;
        workers::update_state(tx, dispatch, "failed")?;
        workers::update_stage(tx, dispatch, Some(stage))?;
        workers::update_last_error(tx, dispatch, Some(&error.message))?;
        // R3: a pane left behind needs accounting. Without one there is
        // nothing to reclaim, and an `active` row would read as a live worker.
        workers::update_terminal_state(
            tx,
            dispatch,
            if pane_created {
                "reclaimable"
            } else {
                "released"
            },
        )?;
        workers::update_updated_at(tx, dispatch, now)?;
        events::append(
            tx,
            &events::EventRow {
                sequence: None,
                run_id: run.clone(),
                at: now,
                r#type: "dispatch.start_failed".into(),
                task_id: Some(task.clone()),
                dispatch_id: Some(dispatch.clone()),
                actor: handle.into(),
                detail: Some(json!({"stage": stage, "failure_count": next, "counted": counted, "dispatch_status": status, "error": error.message})),
            },
        )
    })?;
    engine.after_commit(run, &[])?;
    let (mut receipt, worker) = frame.receipt(engine)?;
    receipt.state = "failed".into();
    receipt.stage = stage.into();
    receipt.failed_stage = Some(stage.into());
    receipt.residual_resources = residual_resources(&worker, pane_created);
    if pane_created {
        receipt.next_commands = vec![
            pod_command("worker-release", dispatch),
            pod_command("worker-show", dispatch),
        ];
    }
    engine.store.tx(|tx| {
        workers::update_residual_resources(tx, dispatch, Some(&json!(receipt.residual_resources)))
    })?;
    Ok(receipt)
}

/// What a failed start leaves behind: a worktree pod created (never removed,
/// spec 7.2), the setup pane (never closed, spec 7.3), and the worker pane.
fn residual_resources(worker: &workers::WorkerRow, pane_created: bool) -> Vec<Resource> {
    let resource = |kind: &str, role: &str, id: &str| Resource {
        kind: kind.into(),
        role: role.into(),
        action: "created".into(),
        id: id.into(),
        surface: "background".into(),
    };
    let mut residual = Vec::new();
    let created_worktree = worker
        .placement
        .as_ref()
        .is_some_and(|kind| matches!(kind.as_str(), Some("new-child" | "new-top-level")));
    if created_worktree && let Some(path) = worker.worktree_path.as_deref() {
        residual.push(resource("worktree", "workspace", path));
    }
    for workspace in opened_workspaces_of(worker) {
        residual.push(resource("workspace", "opened", &workspace));
    }
    if let Some(pane) = worker.setup_pane_id.as_deref() {
        residual.push(resource("pane", "setup", pane));
    }
    if pane_created
        && let Some(pane) = worker.pane_id.as_deref()
        && worker.setup_pane_id.as_deref() != Some(pane)
    {
        residual.push(resource("pane", "agent", pane));
    }
    residual
}

#[allow(clippy::too_many_arguments)]
fn reuse_start(
    engine: &Engine,
    run: &RunId,
    task: &tasks::TaskRow,
    dispatch: &DispatchId,
    handle: &str,
    retry_of: Option<&DispatchId>,
    depth: u32,
    failure_count: u32,
    requested: LaunchRequest,
    timeout_ms: u64,
) -> Result<StartReceipt> {
    let agent = engine.herdr.agent_get(handle)?.ok_or_else(|| {
        PodError::new(
            ErrorCode::UnknownAgent,
            format!("worker handle {handle} was not found"),
            &["Use worker-list or herdr agent list and retry with a live handle."],
        )
    })?;
    let pane = engine.herdr.pane_get(&agent.pane_id)?.ok_or_else(|| {
        PodError::new(
            ErrorCode::HerdrError,
            format!("pane {} for {handle} was not found", agent.pane_id),
            &["Inspect the agent with herdr and retry."],
        )
    })?;
    if engine
        .store
        .read(|tx| dispatches::active_for_terminal(tx, &pane.terminal_id))?
        .is_some()
    {
        return Err(terminal_busy(&pane.terminal_id));
    }
    let previous = engine
        .store
        .read(|tx| dispatches::latest_for_terminal(tx, &pane.terminal_id))?;
    let previous_worker = previous
        .as_ref()
        .map(|row| engine.store.read(|tx| workers::get(tx, &row.id)))
        .transpose()?
        .flatten();
    let effective = Launch {
        kind: agent.kind.clone(),
        command: agent.kind.clone(),
        model: None,
        effort: None,
        fast: false,
        yolo: false,
        args: Vec::new(),
        table: None,
    };
    insert_starting(
        engine,
        run,
        &task.id,
        dispatch,
        retry_of,
        depth,
        handle,
        Some(&agent.kind),
        &requested,
        Some(&effective),
        failure_count,
        engine.clock.now(),
    )?;
    let reused = (|| {
        engine
            .store
            .tx(|tx| {
                workers::update_placement(tx, dispatch, Some(&json!("reused")))?;
                workers::update_pane_id(tx, dispatch, Some(&agent.pane_id))?;
                workers::update_terminal_id(tx, dispatch, Some(&pane.terminal_id))?;
                workers::update_workspace_id(tx, dispatch, Some(&pane.workspace_id))?;
                let mut effects = carry_pane(tx, dispatch, previous_worker.as_ref())?;
                effects.push("pane_reused".into());
                workers::update_effects(tx, dispatch, Some(&json!(effects)))?;
                dispatches::update_assignee_terminal_id(tx, dispatch, Some(&pane.terminal_id))?;
                dispatches::update_assignee_pid(tx, dispatch, Some(agent.pid))?;
                carry_session(
                    tx,
                    dispatch,
                    &agent,
                    previous_worker
                        .as_ref()
                        .zip(previous.as_ref().map(|row| row.assignee_pid)),
                )
            })
            .at("pane", false)?;
        // From here the reused pane's cleanup belongs to this Dispatch.
        if let Some(tab) = previous_worker.as_ref().and_then(|w| w.tab_id.as_deref()) {
            // The label follows the task; a stale label is not worth a failed start.
            let _ = engine.herdr.tab_rename(tab, &tab_label(handle, task));
        }
        workers_stage(engine, dispatch, "inject").at("inject", true)?;
        let preamble = preamble_for(engine, run, task, dispatch, handle).at("inject", true)?;
        let outcome = inject(engine, run, dispatch, handle, &preamble).at("inject", true)?;
        if let InputOutcome::Failed(detail) | InputOutcome::Skipped(detail) = &outcome {
            return Err(inject_failure(detail, true));
        }
        record_dispatched(engine, run, &task.id, dispatch, handle).at("record", true)?;
        Ok(outcome)
    })();
    let frame = StartFrame {
        run,
        task: &task.id,
        dispatch,
        handle,
        requested,
        effective: Some(effective),
        setup: SetupReport::reused(),
        timeout_ms,
    };
    match reused {
        Ok(outcome) => {
            engine.after_commit(run, &[])?;
            started_receipt(engine, frame, outcome)
        }
        Err(failure) => failed_start(engine, frame, failure),
    }
}

fn preamble_for(
    engine: &Engine,
    run: &RunId,
    task: &tasks::TaskRow,
    dispatch: &DispatchId,
    handle: &str,
) -> Result<String> {
    let (coordinator, specialists, decision) = engine.store.read(|tx| {
        let run_row = runs::get(tx, run)?.ok_or_else(|| PodError::usage("Run was not found"))?;
        let mut specialists = Vec::new();
        for attempt in dispatches::list_by_run(tx, run)? {
            if attempt.id == *dispatch || attempt.status != "dispatched" {
                continue;
            }
            let Some(worker_handle) = attempt.assignee_handle else {
                continue;
            };
            let title = task_rows::get(tx, &attempt.task_id)?
                .and_then(|row| row.title.or(row.display_name))
                .unwrap_or_else(|| attempt.task_id.to_string());
            specialists.push(Specialist {
                dispatch_id: attempt.id,
                handle: worker_handle,
                task_title: title,
            });
        }
        let decision = gates::list_by_run(tx, run, Some(&task.id))?
            .into_iter()
            .find(|gate| gate.status == "resolved")
            .and_then(|gate| {
                gate.resolution.map(|resolution| Decision {
                    question: gate.question,
                    resolution,
                })
            });
        Ok((run_row.coordinator_handle, specialists, decision))
    })?;
    let pod = std::env::current_exe()
        .unwrap_or_else(|_| PathBuf::from("pod"))
        .display()
        .to_string();
    Ok(render_preamble(&PreambleContext {
        pod,
        coordinator_handle: coordinator,
        task_id: task.id.clone(),
        dispatch_id: dispatch.clone(),
        worker_handle: handle.into(),
        heartbeat_minutes: engine.config.defaults.heartbeat_minutes,
        specialists,
        decision,
        sub_dispatch: (dispatch_depth(engine, dispatch)? < engine.config.defaults.nested_depth_max)
            .then(|| "You may dispatch bounded subtasks through pod when the coordinator brief permits it.".into()),
        spec: task.spec.clone(),
        brief_path: brief_path(&engine.paths, run, dispatch),
    }))
}

fn dispatch_depth(engine: &Engine, dispatch: &DispatchId) -> Result<u32> {
    Ok(engine
        .store
        .read(|tx| dispatches::get(tx, dispatch))?
        .map_or(0, |row| row.depth))
}

/// Writes the brief to a file and submits a one-line pointer to it, once,
/// through herdr's `agent.prompt` (hand paste only when herdr cannot prompt;
/// spec 8.2, `safe_input::inject_preamble`).
/// `Skipped` and `Failed` mean no confirmed text is in the input; once text
/// may be there the outcome is `RenderedUnconfirmed` and nothing is retyped.
fn inject(
    engine: &Engine,
    run: &RunId,
    dispatch: &DispatchId,
    handle: &str,
    preamble: &str,
) -> Result<InputOutcome> {
    let path = write_preamble_pointer(engine, run, dispatch, preamble)?;
    let text = format!(
        "The full worker brief is stored at {}. Read that file, then carry out the task.",
        path.display()
    );
    let outcome =
        super::safe_input::inject_preamble_proved(engine, dispatch, handle, &text, Some(&path))?;
    engine.store.tx(|tx| {
        let mut effects = workers::get(tx, dispatch)?
            .and_then(|worker| worker.effects)
            .and_then(|value| serde_json::from_value::<Vec<String>>(value).ok())
            .unwrap_or_default();
        effects.push(format!("preamble_file:{}", path.display()));
        workers::update_effects(tx, dispatch, Some(&json!(effects)))
    })?;
    Ok(outcome)
}

/// Where a dispatch's brief file lives.
pub fn brief_path(paths: &Paths, run: &RunId, dispatch: &DispatchId) -> PathBuf {
    paths
        .board_dir
        .join(run.as_str())
        .join("preambles")
        .join(format!("{}.txt", dispatch.as_str()))
}

fn write_preamble_pointer(
    engine: &Engine,
    run: &RunId,
    dispatch: &DispatchId,
    text: &str,
) -> Result<PathBuf> {
    let path = brief_path(&engine.paths, run, dispatch);
    let dir = path.parent().expect("brief path has a parent");
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(0o700);
    builder.create(dir).map_err(|error| {
        PodError::store(format!("could not create preamble directory: {error}"))
    })?;
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(&path)
        .map_err(|error| PodError::store(format!("could not write preamble file: {error}")))?;
    file.write_all(text.as_bytes())
        .map_err(|error| PodError::store(format!("could not write preamble file: {error}")))?;
    Ok(path)
}

fn start_setup(
    engine: &Engine,
    dispatch: &DispatchId,
    pane: &str,
    argv: &[String],
    policy: &str,
    timeout_ms: u64,
) -> Result<()> {
    let sentinel = format!("__POD_SETUP_{}__", dispatch.as_str());
    let command = setup_command(argv, &sentinel);
    match super::safe_input::type_command(engine.herdr.as_ref(), pane, &command)? {
        InputOutcome::Delivered => {}
        InputOutcome::Failed(detail) | InputOutcome::Skipped(detail) => {
            return Err(PodError::new(
                ErrorCode::HerdrError,
                format!("setup command could not be typed: {detail}"),
                &["Inspect the setup pane and retry with --setup skip if appropriate."],
            ));
        }
        InputOutcome::RenderedUnconfirmed => {
            return Err(PodError::new(
                ErrorCode::HerdrError,
                "setup command did not render in its pane; Enter withheld",
                &["Inspect the setup pane and retry with --setup skip if appropriate."],
            ));
        }
    }
    engine.store.tx(|tx| {
        workers::update_setup_pane_id(tx, dispatch, Some(pane))?;
        workers::update_setup_state(tx, dispatch, Some("running"))
    })?;
    if policy == "start-immediately" {
        return Ok(());
    }
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    while Instant::now() < deadline {
        if let Some(code) = parse_setup_exit(&engine.herdr.pane_read(pane, 2000)?, &sentinel) {
            engine.store.tx(|tx| {
                workers::update_setup_state(
                    tx,
                    dispatch,
                    Some(if code == 0 { "succeeded" } else { "failed" }),
                )
            })?;
            return if code == 0 {
                Ok(())
            } else {
                Err(PodError::new(
                    ErrorCode::HerdrError,
                    format!("repository setup exited with status {code}"),
                    &["Inspect the setup pane, fix the setup command, and retry."],
                ))
            };
        }
        thread::sleep(Duration::from_millis(25));
    }
    Err(PodError::new(
        ErrorCode::HerdrError,
        "repository setup did not finish before the start timeout",
        &["Inspect the setup pane, then retry with a larger --timeout-ms."],
    ))
}

/// The line typed into the setup pane. It runs the configured argv under
/// `sh -c` so the user's interactive shell (zsh, bash, fish) does not matter,
/// then prints `<sentinel><exit status>` on a line of its own.
pub fn setup_command(argv: &[String], sentinel: &str) -> String {
    let command = argv
        .iter()
        .map(|token| shell_quote(token))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "sh -c '\"$@\"; printf \"\\n%s%s\\n\" \"$0\" \"$?\"' {} {command}",
        shell_quote(sentinel)
    )
}

pub(crate) fn shell_quote(token: &str) -> String {
    format!("'{}'", token.replace('\'', "'\\''"))
}

pub fn parse_setup_exit(screen: &str, sentinel: &str) -> Option<i32> {
    screen.lines().find_map(|line| {
        line.trim()
            .strip_prefix(sentinel)
            .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
            .and_then(|value| value.parse().ok())
    })
}

pub fn dispatch(
    engine: &Engine,
    task_id: &TaskId,
    to: &str,
    inject_preamble: bool,
    dry_run: bool,
    return_preamble: bool,
) -> Result<DispatchReceipt> {
    let caller = require_caller(engine)?;
    let task = engine.store.read(|tx| {
        tasks::startable(tx, task_id, None)?;
        tasks::get(tx, task_id)?.ok_or_else(|| task_not_found(task_id))
    })?;
    let run = engine
        .store
        .read(|tx| runs::get(tx, &task.run_id))?
        .ok_or_else(|| PodError::usage("Run was not found"))?;
    let depth = caller_depth(engine, &caller.terminal_id, &run)? + 1;
    if depth > engine.config.defaults.nested_depth_max {
        return Err(depth_exceeded());
    }
    let agent = engine.herdr.agent_get(to)?.ok_or_else(|| {
        PodError::new(
            ErrorCode::UnknownAgent,
            format!("worker handle {to} was not found"),
            &["Use a live herdr agent handle."],
        )
    })?;
    let pane = engine.herdr.pane_get(&agent.pane_id)?.ok_or_else(|| {
        PodError::new(
            ErrorCode::HerdrError,
            "target pane was not found",
            &["Inspect herdr and retry."],
        )
    })?;
    if engine
        .store
        .read(|tx| dispatches::active_for_terminal(tx, &pane.terminal_id))?
        .is_some()
    {
        return Err(terminal_busy(&pane.terminal_id));
    }
    let id = DispatchId::new();
    let preamble = preamble_for(engine, &task.run_id, &task, &id, to)?;
    if dry_run {
        return Ok(DispatchReceipt {
            dispatch: None,
            task_id: task.id,
            to: to.into(),
            injected: false,
            outcome: None,
            preamble: return_preamble.then_some(preamble),
            dry_run: true,
        });
    }
    super::limits::check(engine, &agent.kind, &task.run_id)?;
    let requested = LaunchRequest {
        agent: to.into(),
        model: None,
        effort: None,
        fast: false,
        yolo: false,
    };
    let effective = Launch {
        kind: agent.kind.clone(),
        command: agent.kind.clone(),
        model: None,
        effort: None,
        fast: false,
        yolo: false,
        args: Vec::new(),
        table: None,
    };
    let count = engine
        .store
        .read(|tx| dispatches::max_failure_count_for_task(tx, &task.id))?;
    insert_starting(
        engine,
        &task.run_id,
        &task.id,
        &id,
        None,
        depth,
        to,
        Some(&agent.kind),
        &requested,
        Some(&effective),
        count,
        engine.clock.now(),
    )?;
    let previous = engine.store.read(|tx| {
        let Some(row) = dispatches::latest_for_terminal(tx, &pane.terminal_id)? else {
            return Ok(None);
        };
        Ok(workers::get(tx, &row.id)?.map(|worker| (worker, row.assignee_pid)))
    })?;
    let assigned = (|| {
        engine
            .store
            .tx(|tx| {
                carry_session(tx, &id, &agent, previous.as_ref().map(|(w, pid)| (w, *pid)))?;
                workers::update_placement(tx, &id, Some(&json!("existing")))?;
                workers::update_workspace_id(tx, &id, Some(&pane.workspace_id))?;
                workers::update_pane_id(tx, &id, Some(&pane.pane_id))?;
                workers::update_terminal_id(tx, &id, Some(&pane.terminal_id))?;
                // The pane's cleanup moves to this Dispatch, as on reuse
                // (spec 7.4), so a worker finished here is released and a
                // later release of this one closes what pod created.
                let _carried = carry_pane(
                    tx,
                    &id,
                    previous
                        .as_ref()
                        .map(|(worker, _)| worker)
                        .filter(|worker| worker.pane_id.as_deref() == Some(agent.pane_id.as_str())),
                )?;
                dispatches::update_assignee_terminal_id(tx, &id, Some(&pane.terminal_id))?;
                dispatches::update_assignee_pid(tx, &id, Some(agent.pid))?;
                Ok(())
            })
            .at("pane", false)?;
        let outcome = if inject_preamble {
            workers_stage(engine, &id, "inject").at("inject", false)?;
            let outcome =
                inject(engine, &task.run_id, &id, &agent.name, &preamble).at("inject", false)?;
            if let InputOutcome::Failed(detail) | InputOutcome::Skipped(detail) = &outcome {
                return Err(inject_failure(detail, false));
            }
            Some(outcome)
        } else {
            None
        };
        record_dispatched(engine, &task.run_id, &task.id, &id, &caller.terminal_id)
            .at("record", false)?;
        Ok(outcome)
    })();
    // A failed injection counts toward the circuit breaker (spec 7.5). pod
    // created no pane, so the failed attempt leaves nothing to reclaim.
    // `failed_start` runs the commit hook itself; one invocation runs it once.
    let outcome = match assigned {
        Ok(outcome) => {
            engine.after_commit(&task.run_id, &[])?;
            outcome.and_then(|value| {
                (!matches!(value, InputOutcome::Delivered)).then(|| "dispatched_unobserved".into())
            })
        }
        Err(failure) => {
            let frame = StartFrame {
                run: &task.run_id,
                task: &task.id,
                dispatch: &id,
                handle: to,
                requested,
                effective: Some(effective),
                setup: SetupReport::reused(),
                timeout_ms: READY_WAIT_MS,
            };
            failed_start(engine, frame, failure)?;
            Some("failed".into())
        }
    };
    let row = engine
        .store
        .read(|tx| dispatches::get(tx, &id))?
        .expect("dispatch exists");
    Ok(DispatchReceipt {
        dispatch: Some(row),
        task_id: task.id,
        to: to.into(),
        injected: inject_preamble && outcome.as_deref() != Some("failed"),
        outcome,
        preamble: return_preamble.then_some(preamble),
        dry_run: false,
    })
}

pub fn stop(engine: &Engine, dispatch: &DispatchId) -> Result<WorkerReceipt> {
    finish_by_coordinator(engine, dispatch, true)
}

pub fn abandon(engine: &Engine, dispatch: &DispatchId) -> Result<WorkerReceipt> {
    finish_by_coordinator(engine, dispatch, false)
}

/// `worker-stop` (`close_pane`) and `worker-abandon`. An active Dispatch is
/// fenced: recorded `failed` (or `circuit_broken`) with `canceled`, counting
/// toward the breaker (state machines D8). An accepted ending is never
/// overwritten, and a settled worker's row is left as it is, except that a
/// `stop_unknown` stop retries its close (W7).
fn finish_by_coordinator(
    engine: &Engine,
    dispatch: &DispatchId,
    close_pane: bool,
) -> Result<WorkerReceipt> {
    let (before, worker_before) = require_worker(engine, dispatch)?;
    let active = matches!(before.status.as_str(), "pending" | "dispatched");
    let retry_close = close_pane && worker_before.state == "stop_unknown";
    if !active && !retry_close {
        return receipt(
            engine,
            dispatch,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            "unchanged".into(),
        );
    }
    let now = engine.clock.now();
    engine.store.tx(|tx| {
        if active {
            fence_canceled(tx, now, &before)?;
            questions::close_questions_for_dispatch(tx, now, dispatch)?;
        }
        workers::update_state(
            tx,
            dispatch,
            if close_pane { "stopping" } else { "abandoned" },
        )?;
        if worker_before.terminal_state == "active" {
            workers::update_terminal_state(tx, dispatch, "reclaimable")?;
        }
        workers::update_updated_at(tx, dispatch, now)
    })?;
    let mut closed = Vec::new();
    let mut left = Vec::new();
    let mut outcome = if close_pane { "stopped" } else { "abandoned" }.to_owned();
    if close_pane {
        let coordinator_pane = engine.caller.as_ref().and_then(|c| c.pane_id.as_deref());
        // Only the exact supervised agent pane, and only one pod created: a
        // `dispatch --to` pane belongs to whoever opened it (spec 7.4).
        let closable = worker_before.pane_id.as_deref().filter(|pane| {
            worker_before.owns_pane
                && !taken_over(&worker_before)
                && worker_before.terminal_state != "released"
                && Some(*pane) != coordinator_pane
        });
        if worker_before.pane_id.is_some() && !worker_before.owns_pane {
            left.push("pane_not_created_by_pod".to_owned());
        }
        if let Some(pane) = closable {
            // The screen goes with the pane: keep it for worker-read, as
            // release would have (7.4). Best effort; the close still happens.
            if let Err(error) = archive_final_screen(engine, dispatch) {
                eprintln!(
                    "pod: final screen of {dispatch} not archived: {}",
                    error.message
                );
            }
            let closed_ok = match engine.herdr.pane_get(pane) {
                Ok(Some(_)) => engine.herdr.pane_close(pane).is_ok(),
                Ok(None) => true,
                Err(_) => false,
            };
            if closed_ok {
                closed.push(pane.to_owned());
            } else {
                outcome = "stop_unknown".into();
            }
        }
        engine.store.tx(|tx| {
            workers::update_state(tx, dispatch, &outcome)?;
            workers::update_updated_at(tx, dispatch, engine.clock.now())
        })?;
    }
    engine.after_commit(&before.run_id, &[])?;
    receipt(engine, dispatch, closed, Vec::new(), left, outcome)
}

/// Ends an active Dispatch as `canceled` by the coordinator. Unlike a worker's
/// own `worker_done --reason canceled`, a fence counts toward the breaker.
fn fence_canceled(
    tx: &crate::store::Tx<'_>,
    now: DateTime<Utc>,
    attempt: &dispatches::DispatchRow,
) -> Result<()> {
    let next = dispatches::max_failure_count_for_task(tx, &attempt.task_id)? + 1;
    let (status, task_status) = if next >= 3 {
        ("circuit_broken", "failed")
    } else {
        ("failed", "ready")
    };
    if !dispatches::fence(tx, &attempt.id, status, "canceled", now)? {
        return Ok(());
    }
    dispatches::update_failure_count(tx, &attempt.id, next)?;
    dispatches::update_last_failure(tx, &attempt.id, Some("canceled"))?;
    task_rows::update_status(tx, &attempt.task_id, task_status)?;
    events::append(
        tx,
        &events::EventRow {
            sequence: None,
            run_id: attempt.run_id.clone(),
            at: now,
            r#type: "dispatch.failed".into(),
            task_id: Some(attempt.task_id.clone()),
            dispatch_id: Some(attempt.id.clone()),
            actor: "pod".into(),
            detail: Some(
                json!({"termination_reason": "canceled", "termination_target": null, "dispatch_status": status, "task_status": task_status}),
            ),
        },
    )
}

pub fn retain(engine: &Engine, dispatch: &DispatchId, reason: &str) -> Result<WorkerReceipt> {
    let (attempt, worker) = require_worker(engine, dispatch)?;
    // A released worker has nothing left to keep (R6 starts from reclaimable).
    if !matches!(
        worker.terminal_state.as_str(),
        "active" | "reclaimable" | "retained"
    ) {
        return receipt(
            engine,
            dispatch,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            worker.terminal_state,
        );
    }
    let reason = if reason.is_empty() {
        "coordinator retained"
    } else {
        reason
    };
    engine.store.tx(|tx| {
        workers::update_terminal_state(tx, dispatch, "retained")?;
        workers::update_retained_reason(tx, dispatch, Some(reason))?;
        workers::update_updated_at(tx, dispatch, engine.clock.now())
    })?;
    engine.after_commit(&attempt.run_id, &[])?;
    receipt(
        engine,
        dispatch,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "retained".into(),
    )
}

/// `worker-release` (spec 7.4, state machines R7 to R10): archive the final
/// screen, then close the worker's tab only when pod created it and nothing
/// else claims the pane, or just the pane when a person split another pane
/// into that tab. Safe to repeat.
/// `worker-gc`: the receipt of releasing every finished worker of a Run.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GcReceipt {
    pub run_id: RunId,
    pub released: Vec<WorkerReceipt>,
    pub kept: Vec<GcKept>,
}

/// A worker `worker-gc` left as it was, and why.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GcKept {
    pub dispatch_id: DispatchId,
    pub terminal_state: String,
    pub reason: String,
}

/// `worker-gc`: releases every worker of the Run that is `reclaimable`,
/// `release_pending`, or `release_unknown`, each exactly as `worker-release`
/// would (spec 7.4): the final screen is archived and the tab closes only if
/// pod created it. Retained workers are kept, since retain is a choice
/// already made, unless `include_retained` releases them too; active ones
/// are still working. Kept workers are listed with their reason. With
/// `acknowledged_only`, the pass `check --wait` runs, a
/// reclaimable worker is released only once the coordinator has acknowledged
/// its `worker_done`, so a report nobody has read keeps its pane. Without
/// `run` the Run bound to the caller's terminal is swept.
pub fn gc(
    engine: &Engine,
    run: Option<&RunId>,
    acknowledged_only: bool,
    include_retained: bool,
) -> Result<GcReceipt> {
    let run_id = match run {
        Some(run) => run.clone(),
        None => {
            let caller = require_caller(engine)?;
            engine
                .store
                .read(|tx| runs::bound_to_terminal(tx, &caller.terminal_id))?
                .map(|run| run.id)
                .ok_or_else(|| {
                    PodError::usage("no Run is bound to this terminal; pass --run <id>")
                })?
        }
    };
    let candidates = engine.store.read(|tx| {
        let mut rows = Vec::new();
        for attempt in dispatches::list_by_run(tx, &run_id)? {
            if let Some(worker) = workers::get(tx, &attempt.id)? {
                let acknowledged =
                    crate::store::rows::messages::first_worker_done(tx, attempt.id.as_str())?
                        .is_some_and(|report| report.read);
                rows.push((attempt.id, worker.terminal_state, acknowledged));
            }
        }
        Ok(rows)
    })?;
    let mut released = Vec::new();
    let mut kept = Vec::new();
    for (dispatch, state, acknowledged) in candidates {
        match state.as_str() {
            "reclaimable" if acknowledged_only && !acknowledged => kept.push(GcKept {
                dispatch_id: dispatch,
                terminal_state: state,
                reason: "report_not_acknowledged".into(),
            }),
            "reclaimable" | "release_pending" | "release_unknown" => {
                released.push(release(engine, &dispatch)?);
            }
            "retained" if include_retained => released.push(release(engine, &dispatch)?),
            "retained" | "active" => kept.push(GcKept {
                dispatch_id: dispatch,
                reason: state.clone(),
                terminal_state: state,
            }),
            _ => {}
        }
    }
    Ok(GcReceipt {
        run_id,
        released,
        kept,
    })
}

pub fn release(engine: &Engine, dispatch: &DispatchId) -> Result<WorkerReceipt> {
    let (attempt, mut worker) = require_worker(engine, dispatch)?;
    if worker.terminal_state == "released" {
        return receipt(
            engine,
            dispatch,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            "released".into(),
        );
    }
    // R7 starts from reclaimable or retained: an active worker is still
    // working, so its pane and its accounting are left as they are.
    // A pane that another Dispatch now works in (reuse or `dispatch`).
    let pane_busy = worker.terminal_id.as_deref().is_some_and(|terminal| {
        engine
            .store
            .read(|tx| dispatches::active_for_terminal(tx, terminal))
            .ok()
            .flatten()
            .is_some_and(|active| active.id != *dispatch)
    });
    if worker.terminal_state == "active" {
        return receipt(
            engine,
            dispatch,
            Vec::new(),
            Vec::new(),
            vec!["active".into()],
            "active".into(),
        );
    }
    let gone = match worker.pane_id.as_deref() {
        Some(pane) => engine.herdr.pane_get(pane)?.is_none(),
        None => false,
    };
    // A pane closed by anyone but pod's own stop or release was taken over
    // (spec 7.4).
    if gone
        && engine
            .store
            .tx(|tx| pane_gone(tx, engine.clock.now(), dispatch))?
    {
        worker.user_owned = true;
        worker.user_owned_reason = Some("pane_closed_outside_pod".into());
    }
    engine
        .store
        .tx(|tx| workers::update_terminal_state(tx, dispatch, "release_pending"))?;
    archive_final_screen(engine, dispatch)?;
    let coordinator_terminal = engine
        .store
        .read(|tx| runs::get(tx, &attempt.run_id))?
        .map(|run| run.coordinator_terminal_id);
    let caller_pane = engine.caller.as_ref().and_then(|c| c.pane_id.as_deref());
    let left_reason = if !worker.owns_pane {
        Some("pane_not_created_by_pod")
    } else if taken_over(&worker) {
        Some("user_taken_over")
    } else if worker.pane_id.is_none() || worker.tab_id.is_none() {
        Some("no_worker_pane")
    } else if worker.setup_pane_id.is_some() && worker.setup_pane_id == worker.pane_id {
        Some("setup_pane")
    } else if worker.pane_id.as_deref() == caller_pane
        || worker.terminal_id.is_some() && worker.terminal_id == coordinator_terminal
    {
        Some("coordinator_pane")
    } else if pane_busy {
        Some("active")
    } else if gone {
        Some("pane_already_closed")
    } else {
        None
    };
    let mut closed_panes = Vec::new();
    let mut closed_tabs = Vec::new();
    let mut closed_workspaces = Vec::new();
    let mut left = Vec::new();
    let terminal_state = match left_reason {
        Some(reason) => {
            left.push(reason.to_owned());
            closed_workspaces =
                close_opened_workspaces(engine, &attempt.run_id, &worker, dispatch, &mut left);
            "released"
        }
        None => {
            let tab = worker.tab_id.as_deref().expect("checked");
            let pane = worker.pane_id.as_deref().expect("checked");
            // The tab goes only while the worker's pane is all it holds; a
            // pane a person split into it stays, so only pod's pane closes.
            let closed = match engine.herdr.tab_pane_count(tab) {
                Ok(Some(count)) if count <= 1 => engine
                    .herdr
                    .tab_close(tab)
                    .map(|()| closed_tabs.push(tab.to_owned())),
                Ok(_) => engine
                    .herdr
                    .pane_close(pane)
                    .map(|()| closed_panes.push(pane.to_owned())),
                Err(error) => Err(error),
            };
            if closed.is_ok() {
                closed_workspaces =
                    close_opened_workspaces(engine, &attempt.run_id, &worker, dispatch, &mut left);
                "released"
            } else {
                "release_unknown"
            }
        }
    };
    let now = engine.clock.now();
    // The mark a retain before 0.2.11 left is not a takeover; it goes with
    // the release, so `worker-show` stops reporting it.
    let clear_retain_mark = worker.user_owned && !taken_over(&worker);
    engine.store.tx(|tx| {
        workers::update_terminal_state(tx, dispatch, terminal_state)?;
        workers::update_updated_at(tx, dispatch, now)?;
        if clear_retain_mark {
            workers::update_user_owned(tx, dispatch, false, None)?;
        }
        events::append(
            tx,
            &events::EventRow {
                sequence: None,
                run_id: attempt.run_id.clone(),
                at: now,
                r#type: "worker.released".into(),
                task_id: Some(attempt.task_id.clone()),
                dispatch_id: Some(dispatch.clone()),
                actor: engine
                    .caller
                    .as_ref()
                    .map_or_else(|| "pod".to_owned(), |caller| caller.terminal_id.clone()),
                detail: Some(json!({
                    "terminal_state": terminal_state,
                    "closed_tabs": closed_tabs,
                    "closed_panes": closed_panes,
                    "closed_workspaces": closed_workspaces,
                    "left_alone": left,
                })),
            },
        )
    })?;
    engine.after_commit(&attempt.run_id, &[])?;
    let mut receipt = receipt(
        engine,
        dispatch,
        closed_panes,
        closed_tabs,
        left,
        terminal_state.into(),
    )?;
    receipt.closed_workspaces = closed_workspaces;
    Ok(receipt)
}

/// The workspaces herdr opened for the worker's placement, from its effects.
fn opened_workspaces_of(worker: &workers::WorkerRow) -> Vec<String> {
    let names: Vec<String> = worker
        .effects
        .clone()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();
    names
        .iter()
        .filter_map(|name| name.strip_prefix("workspace_opened:"))
        .map(str::to_owned)
        .collect()
}

/// Closes the spaces herdr opened for this worker's placement once its tab
/// is gone (spec 7.2): the worktree's own space first and the repository's
/// parent space after it, each only while it holds nothing but the shell
/// herdr opened it with (one tab, one pane, no agent) and no other worker of
/// pod. A space a person has since used, or that was open before the start,
/// stays. herdr refuses a parent while another worktree space of the
/// repository is open; that goes to `left` as `workspace_group_open:<id>`
/// and a later release of the Run retries it. Any other refusal is a
/// warning, never a failed release.
fn close_opened_workspaces(
    engine: &Engine,
    run: &RunId,
    worker: &workers::WorkerRow,
    dispatch: &DispatchId,
    left: &mut Vec<String>,
) -> Vec<String> {
    let mut closed = Vec::new();
    let mut opened = opened_workspaces_of(worker);
    opened.reverse();
    for workspace in opened {
        match close_plain_workspace(engine, &workspace, dispatch) {
            Some(Ok(true)) => closed.push(workspace),
            Some(Ok(false)) => left.push(format!("workspace_group_open:{workspace}")),
            Some(Err(error)) => engine.warn(format!(
                "workspace {workspace} herdr opened for this worker was not closed: {}",
                error.message
            )),
            None => {}
        }
    }
    // Spaces an earlier release had to leave open, because a worktree space
    // of the same repository was still open, close once that one is gone.
    let earlier: Vec<String> = engine
        .store
        .read(|tx| {
            let mut spaces = Vec::new();
            for attempt in dispatches::list_by_run(tx, run)? {
                if let Some(row) = workers::get(tx, &attempt.id)?
                    && row.terminal_state == "released"
                {
                    // Each row's own spaces worktree first, parent after.
                    spaces.extend(opened_workspaces_of(&row).into_iter().rev());
                }
            }
            Ok(spaces)
        })
        .unwrap_or_default();
    let mut tried = Vec::new();
    let mut refused = Vec::new();
    for workspace in earlier {
        if closed.contains(&workspace) || tried.contains(&workspace) {
            continue;
        }
        tried.push(workspace.clone());
        sweep_workspace(engine, dispatch, workspace, &mut closed, &mut refused);
    }
    // A worktree space closed later in the pass may have freed a parent
    // herdr refused earlier in it.
    for workspace in refused {
        sweep_workspace(engine, dispatch, workspace, &mut closed, &mut Vec::new());
    }
    closed
}

/// One attempt at closing a space an earlier worker opened. A refused one
/// goes to `refused` for the retry; it never reaches `left`.
fn sweep_workspace(
    engine: &Engine,
    dispatch: &DispatchId,
    workspace: String,
    closed: &mut Vec<String>,
    refused: &mut Vec<String>,
) {
    match close_plain_workspace(engine, &workspace, dispatch) {
        Some(Ok(true)) => closed.push(workspace),
        Some(Ok(false)) => refused.push(workspace),
        Some(Err(error)) => engine.warn(format!(
            "workspace {workspace} herdr opened for an earlier worker was not closed: {}",
            error.message
        )),
        None => {}
    }
}

/// Closes `workspace` when herdr still has it and it holds only the shell
/// herdr opened it with and no other pod worker. `None`: left alone;
/// `Some(Ok(false))`: herdr refused because worktree spaces of the repository
/// are open.
fn close_plain_workspace(
    engine: &Engine,
    workspace: &str,
    dispatch: &DispatchId,
) -> Option<Result<bool>> {
    let Ok(Some(info)) = engine.herdr.workspace_get(workspace) else {
        return None;
    };
    let plain = info.tab_count.is_some_and(|tabs| tabs <= 1)
        && info.pane_count.is_some_and(|panes| panes <= 1)
        && info
            .agent_status
            .as_deref()
            .is_none_or(|status| status == "unknown");
    if !plain {
        return None;
    }
    let busy = engine
        .store
        .read(|tx| workers::live_in_workspace(tx, workspace, dispatch))
        .unwrap_or(true);
    if busy {
        return None;
    }
    Some(engine.herdr.workspace_close(workspace))
}

/// Releases the workers whose `worker_done` the coordinator just
/// acknowledged (spec 7.4): acknowledging the report is the completion
/// choice, so each such worker still `reclaimable` is released exactly as
/// `worker-release` would, closing the tab pod created and the spaces herdr
/// opened for it. A worker retained before the acknowledgement keeps its
/// pane. Off with config `auto_release = false`. Seen live 2026-10-15: 41
/// workers released by hand in one session, each a minute after its report.
pub fn release_acknowledged(
    engine: &Engine,
    delivery: &super::deliveries::Delivery,
) -> Result<Vec<WorkerReceipt>> {
    if !engine.config.defaults.auto_release {
        return Ok(Vec::new());
    }
    let mut released = Vec::new();
    for message in &delivery.messages {
        if message.r#type != "worker_done" {
            continue;
        }
        let Some(id) = message
            .payload
            .as_ref()
            .and_then(|payload| payload.get("dispatch_id"))
            .and_then(Value::as_str)
            .and_then(|id| id.parse::<DispatchId>().ok())
        else {
            continue;
        };
        let reclaimable = engine
            .store
            .read(|tx| Ok(workers::get(tx, &id)?.map(|worker| worker.terminal_state)))?
            .as_deref()
            == Some("reclaimable");
        if reclaimable {
            released.push(release(engine, &id)?);
        }
    }
    Ok(released)
}

/// Records that the user took the worker's pane over (spec 7.4): stop and
/// release never close it from then on.
pub fn mark_user_owned(
    tx: &crate::store::Tx<'_>,
    now: DateTime<Utc>,
    dispatch: &DispatchId,
    reason: &str,
) -> Result<()> {
    let attempt = dispatches::get(tx, dispatch)?
        .ok_or_else(|| PodError::usage(format!("Dispatch {dispatch} was not found")))?;
    if workers::get(tx, dispatch)?
        .is_some_and(|w| w.user_owned && w.user_owned_reason.as_deref() == Some(reason))
    {
        return Ok(());
    }
    workers::update_user_owned(tx, dispatch, true, Some(reason))?;
    events::append(
        tx,
        &events::EventRow {
            sequence: None,
            run_id: attempt.run_id,
            at: now,
            r#type: "worker.user_owned".into(),
            task_id: Some(attempt.task_id),
            dispatch_id: Some(dispatch.clone()),
            actor: "pod".into(),
            detail: Some(json!({"reason": reason})),
        },
    )
}

/// The worker's pane is proven gone (a `pane_closed` hook, a reconcile, or a
/// release that finds it missing). A stop whose close was unconfirmed is now
/// `stopped` (state machines W10); otherwise a close pod did not make is a
/// takeover. Returns true when it marked the worker user-owned.
pub fn pane_gone(
    tx: &crate::store::Tx<'_>,
    now: DateTime<Utc>,
    dispatch: &DispatchId,
) -> Result<bool> {
    if workers::get(tx, dispatch)?.is_some_and(|worker| worker.state == "stop_unknown") {
        workers::update_state(tx, dispatch, "stopped")?;
        workers::update_updated_at(tx, dispatch, now)?;
        return Ok(false);
    }
    pane_closed_outside_pod(tx, now, dispatch)
}

/// herdr has no pane-input event (protocol 22), so the one takeover pod can
/// see is a pane that closed while pod was neither stopping nor releasing
/// the worker. Marks it `pane_closed_outside_pod` once; true when it did.
pub fn pane_closed_outside_pod(
    tx: &crate::store::Tx<'_>,
    now: DateTime<Utc>,
    dispatch: &DispatchId,
) -> Result<bool> {
    let Some(worker) = workers::get(tx, dispatch)? else {
        return Ok(false);
    };
    let pod_closed = matches!(
        worker.state.as_str(),
        "stopping" | "stopped" | "stop_unknown"
    ) || matches!(
        worker.terminal_state.as_str(),
        "release_pending" | "release_unknown" | "released"
    );
    if taken_over(&worker) || pod_closed || worker.pane_id.is_none() {
        return Ok(false);
    }
    mark_user_owned(tx, now, dispatch, "pane_closed_outside_pod")?;
    Ok(true)
}

// BEGIN T9 archive_final_screen
/// Archives the worker's final screen text (`final_screen`, last 2000 lines)
/// for `worker-release`, for every agent kind. The first archive stands: a
/// repeated release neither reads the pane again nor overwrites it. A worker
/// with no pane, or whose pane herdr no longer knows, has nothing to archive.
///
/// herdr's report of the pane's agent session goes away with the pane, so a
/// worker with no stored session proof keeps it as `transcript_proof =
/// 'herdr'`; the transcript stays readable after release (spec section 11).
/// That report is best effort: the screen archive is the guarantee.
pub fn archive_final_screen(engine: &Engine, dispatch: &DispatchId) -> Result<()> {
    let pane = engine.store.read(|tx| {
        if worker_archives::get(tx, dispatch, "final_screen")?.is_some() {
            return Ok(None);
        }
        Ok(workers::get(tx, dispatch)?.and_then(|worker| worker.pane_id))
    })?;
    let Some(pane) = pane else {
        return Ok(());
    };
    if engine.herdr.pane_get(&pane)?.is_none() {
        return Ok(());
    }
    let content = engine.herdr.pane_read(&pane, 2000)?;
    let session = engine.herdr.agent_list().ok().and_then(|agents| {
        agents.into_iter().find(|agent| {
            agent.pane_id == pane && (agent.session_id.is_some() || agent.session_path.is_some())
        })
    });
    let now = engine.clock.now();
    engine.store.tx(|tx| {
        if worker_archives::get(tx, dispatch, "final_screen")?.is_some() {
            return Ok(());
        }
        worker_archives::insert(
            tx,
            &worker_archives::WorkerArchiveRow {
                dispatch_id: dispatch.clone(),
                kind: "final_screen".into(),
                content,
                created_at: now,
            },
        )?;
        let unproven = workers::get(tx, dispatch)?.is_some_and(|w| w.transcript_proof.is_none());
        if let Some(agent) = session.filter(|_| unproven) {
            let path = agent.session_path.map(|p| p.display().to_string());
            workers::update_session_id(tx, dispatch, agent.session_id.as_deref())?;
            workers::update_transcript_path(tx, dispatch, path.as_deref())?;
            workers::update_transcript_proof(tx, dispatch, Some("herdr"))?;
            workers::update_updated_at(tx, dispatch, now)?;
        }
        Ok(())
    })
}
// END T9 archive_final_screen

pub fn show(engine: &Engine, dispatch: &DispatchId) -> Result<WorkerView> {
    let now = engine.clock.now();
    let usage = UsageStore::new(engine.paths.state_dir.join("usage"));
    engine
        .store
        .read(|tx| view(tx, now, &engine.paths, dispatch, &usage))
}

fn view(
    tx: &crate::store::Tx<'_>,
    now: DateTime<Utc>,
    paths: &Paths,
    dispatch: &DispatchId,
    usage: &UsageStore,
) -> Result<WorkerView> {
    let attempt = dispatches::get(tx, dispatch)?
        .ok_or_else(|| PodError::usage(format!("Dispatch {dispatch} was not found")))?;
    let worker = workers::get(tx, dispatch)?
        .ok_or_else(|| PodError::usage(format!("Dispatch {dispatch} has no worker")))?;
    let mailbox = format!("dispatch:{dispatch}");
    let cost_usd = worker
        .session_id
        .as_deref()
        .and_then(|session| usage.load_session(session))
        .and_then(|record| record.cost_usd);
    Ok(WorkerView {
        cost_usd,
        observation: observations::get(tx, dispatch)?,
        projection: observe::project(tx, now, dispatch)?,
        wake_ladder: WakeLadder {
            exhausted: super::wake::ladder_position(tx, &mailbox)?.exhausted,
            attempts: wake_attempts::list_since_last_consume(tx, &mailbox)?,
        },
        brief_path: brief_path(paths, &attempt.run_id, dispatch),
        dispatch: attempt,
        worker,
    })
}

pub fn list(
    engine: &Engine,
    run: Option<&RunId>,
    terminal_state: Option<&str>,
    limit: u32,
    cursor: Option<&str>,
) -> Result<WorkerList> {
    if !(1..=100).contains(&limit) {
        return Err(PodError::usage("--limit must be between 1 and 100"));
    }
    if let Some(state) = terminal_state
        && !matches!(
            state,
            "active"
                | "reclaimable"
                | "retained"
                | "release_pending"
                | "release_unknown"
                | "released"
        )
    {
        return Err(PodError::usage("invalid --terminal-state"));
    }
    let (scope_run, source) = if let Some(run) = run {
        (Some(run.clone()), "flag")
    } else if let Some(bound) = engine
        .caller
        .as_ref()
        .map(|caller| {
            engine
                .store
                .read(|tx| runs::bound_to_terminal(tx, &caller.terminal_id))
        })
        .transpose()?
        .flatten()
    {
        (Some(bound.id), "bound")
    } else {
        (None, "all")
    };
    let offset = cursor
        .map(|value| value.parse::<usize>())
        .transpose()
        .map_err(|_| PodError::usage("invalid worker-list cursor"))?
        .unwrap_or(0);
    let mut attempts = engine.store.read(|tx| match scope_run.as_ref() {
        Some(run) => dispatches::list_by_run(tx, run),
        None => dispatches::list(tx, usize::MAX),
    })?;
    if scope_run.is_some() {
        attempts.reverse();
    }
    let now = engine.clock.now();
    let usage = UsageStore::new(engine.paths.state_dir.join("usage"));
    let (workers, total, counts) = engine.store.read(|tx| {
        let mut counts = std::collections::BTreeMap::new();
        let mut selected = Vec::new();
        for attempt in attempts {
            let Some(worker) = workers::get(tx, &attempt.id)? else {
                continue;
            };
            *counts.entry(worker.terminal_state.clone()).or_insert(0) += 1;
            if terminal_state.is_none_or(|state| worker.terminal_state == state) {
                selected.push(attempt.id);
            }
        }
        let total = selected.len();
        let page = selected
            .iter()
            .skip(offset)
            .take(limit as usize)
            .map(|id| view(tx, now, &engine.paths, id, &usage))
            .collect::<Result<Vec<_>>>()?;
        Ok((page, total, counts))
    })?;
    let next_cursor =
        (offset + (limit as usize) < total).then(|| (offset + limit as usize).to_string());
    Ok(WorkerList {
        scope: WorkerListScope {
            source: source.into(),
            run: scope_run,
        },
        workers,
        next_cursor,
        limit,
        total,
        counts,
    })
}

fn receipt(
    engine: &Engine,
    dispatch: &DispatchId,
    closed_panes: Vec<String>,
    closed_tabs: Vec<String>,
    left_alone: Vec<String>,
    outcome: String,
) -> Result<WorkerReceipt> {
    let (dispatch, worker) = require_worker(engine, dispatch)?;
    let archive_bytes = engine
        .store
        .read(|tx| worker_archives::get(tx, &dispatch.id, "final_screen"))?
        .map(|row| row.content.len());
    Ok(WorkerReceipt {
        dispatch_id: dispatch.id.clone(),
        state: worker.terminal_state.clone(),
        dispatch,
        worker,
        closed_panes,
        closed_tabs,
        closed_workspaces: Vec::new(),
        left_alone,
        archive: Archive {
            source: "final_screen".into(),
            status: if archive_bytes.is_some() {
                "preserved"
            } else {
                "absent"
            }
            .into(),
            bytes: archive_bytes,
        },
        outcome,
    })
}

fn require_worker(
    engine: &Engine,
    dispatch: &DispatchId,
) -> Result<(dispatches::DispatchRow, workers::WorkerRow)> {
    engine.store.read(|tx| {
        let attempt = dispatches::get(tx, dispatch)?
            .ok_or_else(|| PodError::usage(format!("Dispatch {dispatch} was not found")))?;
        let worker = workers::get(tx, dispatch)?
            .ok_or_else(|| PodError::usage(format!("Dispatch {dispatch} has no worker")))?;
        Ok((attempt, worker))
    })
}

fn selected_run(engine: &Engine, explicit: Option<&RunId>) -> Result<runs::RunRow> {
    if let Some(run) = explicit {
        return engine
            .store
            .read(|tx| runs::get(tx, run))?
            .ok_or_else(|| PodError::usage(format!("Run {run} was not found")));
    }
    let caller = require_caller(engine)?;
    engine
        .store
        .read(|tx| runs::bound_to_terminal(tx, &caller.terminal_id))?
        .ok_or_else(|| PodError::usage("no Run is bound to this terminal"))
}

/// The caller's own depth: a terminal that has held a Dispatch is a worker at
/// that Dispatch's depth, whatever Run it names with `--run`.
fn caller_depth(engine: &Engine, terminal: &str, run: &runs::RunRow) -> Result<u32> {
    let own = engine
        .store
        .read(|tx| dispatches::latest_for_terminal(tx, terminal))?;
    Ok(own.map_or(run.depth, |attempt| attempt.depth.max(run.depth)))
}

fn depth_exceeded() -> PodError {
    PodError::new(
        ErrorCode::NestedWorkerDepthExceeded,
        "starting this worker would exceed defaults.nested_depth_max",
        &[
            "Ask your coordinator to start the worker instead, or raise nested_depth_max in config.toml.",
        ],
    )
}

fn require_caller(engine: &Engine) -> Result<&crate::identity::Caller> {
    engine.caller.as_ref().ok_or_else(|| {
        PodError::new(
            ErrorCode::NotInHerdr,
            "this mutation requires a caller inside herdr",
            &["Run the command from a pane inside herdr."],
        )
    })
}

fn task_not_found(task: &TaskId) -> PodError {
    PodError::new(
        ErrorCode::TaskNotFound,
        format!("Task {task} was not found in the selected Run"),
        &["Run pod task-list and retry with a valid Task ID."],
    )
}

fn terminal_busy(terminal: &str) -> PodError {
    PodError::new(
        ErrorCode::TerminalBusy,
        format!("terminal {terminal} already owns an active Dispatch"),
        &["Wait for worker_done, or stop the active Dispatch before retrying."],
    )
}

/// A herdr agent name (spec 7.1): `<base>` or `<base>-<place>`, sanitized
/// to `[a-z][a-z0-9_-]{0,31}`, with `-2`, `-3`, ... appended only while a
/// live agent already holds the name.
pub fn handle_for(base: &str, place: Option<&str>, existing: &[String]) -> String {
    let mut handle = sanitize_handle(base);
    if let Some(place) = place {
        let place = sanitize_handle(place);
        if place != "worker" {
            handle = format!("{handle}-{place}");
        }
    }
    handle.truncate(28);
    while handle.ends_with('-') {
        handle.pop();
    }
    if !existing.iter().any(|value| value == &handle) {
        return handle;
    }
    (2u32..)
        .map(|n| format!("{handle}-{n}"))
        .find(|candidate| !existing.iter().any(|value| value == candidate))
        .expect("an unbounded counter finds a free handle")
}

fn sanitize_handle(text: &str) -> String {
    let mut base = text
        .to_ascii_lowercase()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    while base.starts_with(|character: char| !character.is_ascii_lowercase()) {
        base.remove(0);
    }
    while base.contains("--") {
        base = base.replace("--", "-");
    }
    while base.ends_with('-') {
        base.pop();
    }
    if base.is_empty() {
        base = "worker".into();
    }
    base
}

/// The worker tab's label: the handle and the Task's title (or its spec's
/// first line), so a person scanning tabs sees who works on what.
fn tab_label(handle: &str, task: &tasks::TaskRow) -> String {
    let title = task
        .title
        .as_deref()
        .or_else(|| task.spec.lines().next())
        .unwrap_or("")
        .trim();
    let short: String = title.chars().take(40).collect();
    if short.is_empty() {
        handle.to_owned()
    } else {
        format!("{handle} · {short}")
    }
}
