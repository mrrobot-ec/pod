use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;

use super::{
    AgentInfo, AgentStatus, EventStream, Herdr, HerdrEvent, HerdrInfo, PaneInfo, ProcessInfo,
    PromptOutcome, TabInfo, WorkspaceCreated, WorkspaceInfo, WorktreeCreate, WorktreeInfo,
    WorktreeListing,
};
use crate::error::{ErrorCode, PodError, Result};

/// Distinct per `FakeHerdr::new`, unlike an address a dropped fake frees.
static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub struct FakeHerdr {
    state: Arc<Mutex<State>>,
}

struct State {
    info: HerdrInfo,
    panes: BTreeMap<String, PaneInfo>,
    screens: BTreeMap<String, String>,
    agents: BTreeMap<String, AgentInfo>,
    worktrees: Vec<WorktreeInfo>,
    worktree_list_error: Option<String>,
    worktree_source: Option<std::path::PathBuf>,
    folder_workspaces: BTreeMap<std::path::PathBuf, String>,
    removed_workspaces: std::collections::BTreeSet<String>,
    agent_start_error: Option<String>,
    agent_start_failures: VecDeque<String>,
    agent_start_kind: Option<String>,
    shell_busy_reads: BTreeMap<String, u32>,
    tab_panes: BTreeMap<String, u32>,
    metadata_error: Option<String>,
    agent_start_session: Option<(Option<String>, Option<std::path::PathBuf>)>,
    agent_start_argv: Vec<Vec<String>>,
    pane_send_text_error: Option<String>,
    render_sent_text: bool,
    setup_exit: Option<i32>,
    agent_statuses: BTreeMap<String, VecDeque<AgentStatus>>,
    prompt_outcomes: VecDeque<PromptOutcome>,
    agent_prompt_error: Option<(ErrorCode, String)>,
    prompt_waits: Vec<Option<u64>>,
    prompt_delay: std::time::Duration,
    events: Arc<Mutex<VecDeque<HerdrEvent>>>,
    submissions: Vec<(String, String)>,
    calls: Vec<String>,
    next_id: u64,
    instance: u64,
}

impl FakeHerdr {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                info: HerdrInfo {
                    version: "0.9.3".into(),
                    protocol: 22,
                },
                panes: BTreeMap::new(),
                screens: BTreeMap::new(),
                agents: BTreeMap::new(),
                worktrees: Vec::new(),
                worktree_list_error: None,
                worktree_source: None,
                folder_workspaces: BTreeMap::new(),
                removed_workspaces: std::collections::BTreeSet::new(),
                agent_start_error: None,
                agent_start_failures: VecDeque::new(),
                agent_start_kind: None,
                shell_busy_reads: BTreeMap::new(),
                tab_panes: BTreeMap::new(),
                metadata_error: None,
                agent_start_session: None,
                agent_start_argv: Vec::new(),
                pane_send_text_error: None,
                render_sent_text: true,
                setup_exit: None,
                agent_statuses: BTreeMap::new(),
                prompt_outcomes: VecDeque::new(),
                agent_prompt_error: None,
                prompt_waits: Vec::new(),
                prompt_delay: std::time::Duration::ZERO,
                events: Arc::new(Mutex::new(VecDeque::new())),
                submissions: Vec::new(),
                calls: Vec::new(),
                next_id: 1,
                instance: NEXT_INSTANCE.fetch_add(1, Ordering::Relaxed),
            })),
        }
    }

    /// The fake a `pod` subprocess gets under `POD_FAKE_HERDR=1`, seeded from
    /// the environment so caller-bound commands work in CLI tests:
    ///
    /// - `POD_FAKE_PANES`: comma-separated `pane_id=terminal_id`, each in
    ///   workspace `ws_fake`. With `HERDR_ENV=1` and `HERDR_PANE_ID` naming one
    ///   of them, `identity::resolve` returns that pane's terminal as caller.
    /// - `POD_FAKE_AGENTS`: comma-separated `name=kind:pane_id:pid`, each idle.
    /// - `POD_FAKE_AGENT_START_ERROR`: every `agent_start` fails with it.
    /// - `POD_FAKE_RENDER_SENT_TEXT=0`: pasted text never shows on screen, so
    ///   safe input reports `rendered_unconfirmed`.
    pub fn from_env(env: &dyn Fn(&str) -> Option<String>) -> Self {
        let fake = Self::new();
        // Each `pod` process gets a fresh fake; numbering its tabs and panes
        // from the process ID keeps two processes' terminals distinct, as
        // herdr's are, in one shared store.
        fake.state
            .lock()
            .expect("fake herdr mutex poisoned")
            .next_id = u64::from(std::process::id()) * 1_000;
        if let Some(message) = env("POD_FAKE_AGENT_START_ERROR") {
            fake.fail_agent_start(&message);
        }
        if env("POD_FAKE_RENDER_SENT_TEXT").as_deref() == Some("0") {
            fake.set_render_sent_text(false);
        }
        let entries = |name: &str| {
            env(name)
                .unwrap_or_default()
                .split(',')
                .filter(|entry| !entry.is_empty())
                .filter_map(|entry| {
                    entry
                        .split_once('=')
                        .map(|(key, value)| (key.to_owned(), value.to_owned()))
                })
                .collect::<Vec<_>>()
        };
        for (pane, terminal) in entries("POD_FAKE_PANES") {
            fake.add_pane(&pane, &terminal, "ws_fake");
        }
        for (name, spec) in entries("POD_FAKE_AGENTS") {
            let mut parts = spec.splitn(3, ':');
            let (Some(kind), Some(pane), Some(pid)) = (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            fake.set_agent(
                &name,
                AgentInfo {
                    name: name.clone(),
                    kind: kind.to_owned(),
                    pane_id: pane.to_owned(),
                    pid: pid.parse().unwrap_or_default(),
                    status: AgentStatus::Idle,
                    session_id: None,
                    session_path: None,
                    needs_input: Vec::new(),
                    observed_at: Utc::now(),
                },
            );
        }
        fake
    }

    pub fn add_pane(&self, pane_id: &str, terminal_id: &str, workspace_id: &str) {
        self.state
            .lock()
            .expect("fake herdr mutex poisoned")
            .panes
            .insert(
                pane_id.into(),
                PaneInfo {
                    pane_id: pane_id.into(),
                    terminal_id: terminal_id.into(),
                    workspace_id: workspace_id.into(),
                },
            );
    }

    /// Adds or replaces an agent. A pane with no screen yet shows the kind's
    /// empty input line, as a started agent does, so safe input can type.
    pub fn set_agent(&self, name: &str, info: AgentInfo) {
        let mut state = self.state.lock().expect("fake herdr mutex poisoned");
        state
            .screens
            .entry(info.pane_id.clone())
            .or_insert_with(|| bare_prompt(&info.kind).to_owned());
        state.agents.insert(name.into(), info);
    }

    /// herdr stops detecting an agent; its pane stays.
    pub fn remove_agent(&self, name: &str) {
        self.state
            .lock()
            .expect("fake herdr mutex poisoned")
            .agents
            .remove(name);
    }

    /// Makes `workspace_get` answer `None` for `workspace_id`, as herdr does
    /// for a workspace that closed with its last tab.
    pub fn remove_workspace(&self, workspace_id: &str) {
        self.state
            .lock()
            .expect("fake herdr mutex poisoned")
            .removed_workspaces
            .insert(workspace_id.to_owned());
    }

    /// A workspace whose panes sit in `folder`, for `workspace_for_folder`.
    pub fn add_folder_workspace(&self, folder: &Path, workspace_id: &str) {
        self.state
            .lock()
            .expect("fake herdr mutex poisoned")
            .folder_workspaces
            .insert(folder.to_path_buf(), workspace_id.to_owned());
    }

    pub fn add_worktree(&self, path: &Path, branch: &str) {
        self.state
            .lock()
            .expect("fake herdr mutex poisoned")
            .worktrees
            .push(WorktreeInfo {
                workspace_id: String::new(),
                path: path.to_path_buf(),
                branch: branch.to_owned(),
                tab_id: String::new(),
                pane_id: String::new(),
            });
    }

    pub fn fail_agent_start(&self, message: &str) {
        self.state
            .lock()
            .expect("fake herdr mutex poisoned")
            .agent_start_error = Some(message.to_owned());
    }

    /// The next `agent_start` call fails once with `message`, before starting
    /// anything (herdr's "not an available shell" refusal on a new tab).
    pub fn fail_agent_start_once(&self, message: &str) {
        self.state
            .lock()
            .expect("fake herdr mutex poisoned")
            .agent_start_failures
            .push_back(message.to_owned());
    }

    /// Later `agent_start` calls return this detected kind instead of the
    /// requested one, as herdr does at detection for an agent run under a
    /// command alias; the agent then identifies itself, so `agent_get` and
    /// `agent_list` report the requested kind.
    pub fn report_agent_kind(&self, kind: &str) {
        self.state
            .lock()
            .expect("fake herdr mutex poisoned")
            .agent_start_kind = Some(kind.to_owned());
    }

    /// `pane_process_info` for `pane_id` (`*`: any pane without its own
    /// entry) shows a foreground command, the shell not yet at its prompt,
    /// for the next `reads` calls.
    pub fn script_shell_busy(&self, pane_id: &str, reads: u32) {
        self.state
            .lock()
            .expect("fake herdr mutex poisoned")
            .shell_busy_reads
            .insert(pane_id.to_owned(), reads);
    }

    /// A person splits another pane into `tab_id` (herdr gives pod no event
    /// for it; `tab_pane_count` shows it).
    pub fn split_into_tab(&self, tab_id: &str) {
        *self
            .state
            .lock()
            .expect("fake herdr mutex poisoned")
            .tab_panes
            .entry(tab_id.to_owned())
            .or_insert(1) += 1;
    }

    /// Every `workspace_report_metadata` call is refused with `message`, as
    /// a herdr without plugin sidebar metadata would refuse it.
    pub fn refuse_metadata(&self, message: &str) {
        self.state
            .lock()
            .expect("fake herdr mutex poisoned")
            .metadata_error = Some(message.to_owned());
    }

    /// Makes later `agent_start` calls report this session, as herdr does
    /// once its agent integration is installed.
    pub fn report_agent_session(&self, id: Option<&str>, path: Option<&Path>) {
        self.state
            .lock()
            .expect("fake herdr mutex poisoned")
            .agent_start_session = Some((id.map(str::to_owned), path.map(Path::to_path_buf)));
    }

    /// The argv of every `agent_start` call, oldest first.
    pub fn agent_start_argv(&self) -> Vec<Vec<String>> {
        self.state
            .lock()
            .expect("fake herdr mutex poisoned")
            .agent_start_argv
            .clone()
    }

    /// The source checkout every `worktree_list` names, as herdr does for a
    /// folder inside a repository it knows.
    pub fn set_worktree_source(&self, path: &Path) {
        self.state
            .lock()
            .expect("fake herdr mutex poisoned")
            .worktree_source = Some(path.to_path_buf());
    }

    /// Makes every `worktree_list` fail with `message`, as herdr does when
    /// the folder it looks at is no repository.
    pub fn fail_worktree_list(&self, message: &str) {
        self.state
            .lock()
            .expect("fake herdr mutex poisoned")
            .worktree_list_error = Some(message.to_owned());
    }

    pub fn fail_pane_send_text(&self, message: &str) {
        self.state
            .lock()
            .expect("fake herdr mutex poisoned")
            .pane_send_text_error = Some(message.to_owned());
    }

    pub fn set_render_sent_text(&self, value: bool) {
        self.state
            .lock()
            .expect("fake herdr mutex poisoned")
            .render_sent_text = value;
    }

    /// Scripts pod's repo setup: Enter on a pane showing a typed setup command
    /// prints that command's exit sentinel with `code`.
    pub fn script_setup_exit(&self, code: i32) {
        self.state
            .lock()
            .expect("fake herdr mutex poisoned")
            .setup_exit = Some(code);
    }

    pub fn push_event(&self, event: HerdrEvent) {
        let events = self
            .state
            .lock()
            .expect("fake herdr mutex poisoned")
            .events
            .clone();
        events
            .lock()
            .expect("fake event mutex poisoned")
            .push_back(event);
    }

    pub fn script_agent_status(&self, name: &str, status: AgentStatus) {
        self.state
            .lock()
            .expect("fake herdr mutex poisoned")
            .agent_statuses
            .entry(name.into())
            .or_default()
            .push_back(status);
    }

    pub fn script_prompt_outcome(&self, outcome: PromptOutcome) {
        self.state
            .lock()
            .expect("fake herdr mutex poisoned")
            .prompt_outcomes
            .push_back(outcome);
    }

    /// The next `agent_prompt` fails with a herdr error, once.
    pub fn fail_agent_prompt(&self, message: &str) {
        self.fail_agent_prompt_with(ErrorCode::HerdrError, message);
    }

    /// The next `agent_prompt` fails with `code`, once: `HerdrUnreachable`
    /// is a broken connection, where the text may already have gone in.
    pub fn fail_agent_prompt_with(&self, code: ErrorCode, message: &str) {
        self.state
            .lock()
            .expect("fake herdr mutex poisoned")
            .agent_prompt_error = Some((code, message.into()));
    }

    /// The `wait_ms` of every `agent_prompt` call, in order.
    pub fn prompt_waits(&self) -> Vec<Option<u64>> {
        self.state
            .lock()
            .expect("fake herdr mutex poisoned")
            .prompt_waits
            .clone()
    }

    /// Every later `agent_prompt` takes this long, as herdr does while it
    /// waits for the turn; lets a test overlap two processes' wakes.
    pub fn delay_agent_prompt(&self, delay: std::time::Duration) {
        self.state
            .lock()
            .expect("fake herdr mutex poisoned")
            .prompt_delay = delay;
    }

    pub fn calls(&self) -> Vec<String> {
        self.state
            .lock()
            .expect("fake herdr mutex poisoned")
            .calls
            .clone()
    }

    pub fn screen(&self, pane_id: &str) -> String {
        self.state
            .lock()
            .expect("fake herdr mutex poisoned")
            .screens
            .get(pane_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn set_screen(&self, pane_id: &str, text: &str) {
        self.state
            .lock()
            .expect("fake herdr mutex poisoned")
            .screens
            .insert(pane_id.into(), text.into());
    }

    pub fn submissions(&self) -> Vec<(String, String)> {
        self.state
            .lock()
            .expect("fake herdr mutex poisoned")
            .submissions
            .clone()
    }

    fn record(&self, call: String) {
        self.state
            .lock()
            .expect("fake herdr mutex poisoned")
            .calls
            .push(call);
    }
}

/// The empty input line an agent of `kind` draws (marker and a space).
fn bare_prompt(kind: &str) -> &'static str {
    match kind {
        "codex" => "› ",
        "claude" => "❯ ",
        _ => "> ",
    }
}

impl Default for FakeHerdr {
    fn default() -> Self {
        Self::new()
    }
}

struct FakeEventStream {
    events: Arc<Mutex<VecDeque<HerdrEvent>>>,
    pane_ids: Vec<String>,
}

impl EventStream for FakeEventStream {
    /// Pops the oldest queued event for a subscribed pane, like the socket
    /// stream's pane filter; events for other panes stay queued.
    fn next(&mut self, _timeout: Duration) -> Result<Option<HerdrEvent>> {
        let mut events = self.events.lock().expect("fake event mutex poisoned");
        let position = events.iter().position(|event| {
            let pane_id = match event {
                HerdrEvent::PaneExited { pane_id }
                | HerdrEvent::PaneClosed { pane_id }
                | HerdrEvent::AgentStatusChanged { pane_id, .. } => pane_id,
            };
            self.pane_ids.is_empty() || self.pane_ids.contains(pane_id)
        });
        Ok(position.and_then(|position| events.remove(position)))
    }
}

impl Herdr for FakeHerdr {
    fn ping(&self) -> Result<HerdrInfo> {
        self.record("ping".into());
        Ok(self
            .state
            .lock()
            .expect("fake herdr mutex poisoned")
            .info
            .clone())
    }

    fn pane_current(&self) -> Result<PaneInfo> {
        self.record("pane_current".into());
        self.state
            .lock()
            .expect("fake herdr mutex poisoned")
            .panes
            .values()
            .next()
            .cloned()
            .ok_or_else(|| {
                PodError::new(
                    ErrorCode::HerdrError,
                    "fake pane_current without a pane",
                    &[],
                )
            })
    }

    fn pane_get(&self, pane_id: &str) -> Result<Option<PaneInfo>> {
        self.record(format!("pane_get:{pane_id}"));
        Ok(self
            .state
            .lock()
            .expect("fake herdr mutex poisoned")
            .panes
            .get(pane_id)
            .cloned())
    }

    fn pane_process_info(&self, pane_id: &str) -> Result<ProcessInfo> {
        self.record(format!("pane_process_info:{pane_id}"));
        let busy = {
            let mut state = self.state.lock().expect("fake herdr mutex poisoned");
            let key = if state.shell_busy_reads.contains_key(pane_id) {
                pane_id
            } else {
                "*"
            };
            match state.shell_busy_reads.get_mut(key) {
                Some(reads) if *reads > 0 => {
                    *reads -= 1;
                    true
                }
                _ => false,
            }
        };
        Ok(ProcessInfo {
            shell_pid: 1,
            foreground_pid: if busy { 2 } else { 1 },
            pids: vec![1, std::process::id()],
        })
    }

    fn pane_read(&self, pane_id: &str, _lines: u32) -> Result<String> {
        self.record(format!("pane_read:{pane_id}"));
        Ok(self.screen(pane_id))
    }

    fn pane_send_text(&self, pane_id: &str, text: &str) -> Result<()> {
        self.record(format!("pane_send_text:{pane_id}"));
        let mut state = self.state.lock().expect("fake herdr mutex poisoned");
        if let Some(message) = state.pane_send_text_error.clone() {
            return Err(PodError::new(
                crate::error::ErrorCode::HerdrError,
                message,
                &["Inspect the scripted fake herdr failure."],
            ));
        }
        if state.render_sent_text {
            state
                .screens
                .entry(pane_id.into())
                .or_default()
                .push_str(text);
        }
        Ok(())
    }

    fn pane_send_keys(&self, pane_id: &str, keys: &[&str]) -> Result<()> {
        self.record(format!("pane_send_keys:{pane_id}:{}", keys.join(",")));
        let mut state = self.state.lock().expect("fake herdr mutex poisoned");
        for key in keys {
            if key.eq_ignore_ascii_case("enter") {
                let submitted = state.screens.get(pane_id).cloned().unwrap_or_default();
                let sentinel = submitted.rfind("__POD_SETUP_").and_then(|start| {
                    let rest = &submitted[start + "__POD_SETUP_".len()..];
                    rest.find("__").map(|end| {
                        submitted[start..start + "__POD_SETUP_".len() + end + 2].to_owned()
                    })
                });
                state.submissions.push((pane_id.into(), submitted));
                state.screens.entry(pane_id.into()).or_default().push('\n');
                // An agent takes the submitted text and draws an empty input
                // line again; a plain shell pane just moves to a new line.
                let kind = state
                    .agents
                    .values()
                    .find(|agent| agent.pane_id == pane_id)
                    .map(|agent| agent.kind.clone());
                if let Some(kind) = kind {
                    state
                        .screens
                        .entry(pane_id.into())
                        .or_default()
                        .push_str(bare_prompt(&kind));
                }
                if let (Some(code), Some(sentinel)) = (state.setup_exit, sentinel) {
                    state
                        .screens
                        .entry(pane_id.into())
                        .or_default()
                        .push_str(&format!("{sentinel}{code}\n"));
                }
            }
        }
        Ok(())
    }

    fn pane_close(&self, pane_id: &str) -> Result<()> {
        self.record(format!("pane_close:{pane_id}"));
        self.state
            .lock()
            .expect("fake herdr mutex poisoned")
            .panes
            .remove(pane_id);
        Ok(())
    }

    fn tab_create(
        &self,
        workspace_id: &str,
        name: &str,
        _cwd: &Path,
        focus: bool,
        env: &[(&str, &str)],
    ) -> Result<TabInfo> {
        self.record(format!("tab_create:{workspace_id}:{name}"));
        if focus {
            self.record(format!("tab_focus:{name}"));
        }
        for (key, value) in env {
            self.record(format!("tab_env:{name}:{key}={value}"));
        }
        let mut state = self.state.lock().expect("fake herdr mutex poisoned");
        let id = state.next_id;
        state.next_id += 1;
        state.tab_panes.insert(format!("tab_{id}"), 1);
        let pane_id = format!("pane_{id}");
        state.panes.insert(
            pane_id.clone(),
            PaneInfo {
                pane_id: pane_id.clone(),
                terminal_id: format!("term_{id}"),
                workspace_id: workspace_id.into(),
            },
        );
        Ok(TabInfo {
            tab_id: format!("tab_{id}"),
            pane_id,
        })
    }

    fn tab_close(&self, tab_id: &str) -> Result<()> {
        self.record(format!("tab_close:{tab_id}"));
        self.state
            .lock()
            .expect("fake herdr mutex poisoned")
            .tab_panes
            .remove(tab_id);
        Ok(())
    }

    fn tab_rename(&self, tab_id: &str, label: &str) -> Result<()> {
        self.record(format!("tab_rename:{tab_id}:{label}"));
        Ok(())
    }

    fn tab_pane_count(&self, tab_id: &str) -> Result<Option<u32>> {
        self.record(format!("tab_pane_count:{tab_id}"));
        Ok(self
            .state
            .lock()
            .expect("fake herdr mutex poisoned")
            .tab_panes
            .get(tab_id)
            .copied())
    }

    fn workspace_for_folder(&self, folder: &Path) -> Result<Option<String>> {
        self.record(format!("workspace_for_folder:{}", folder.display()));
        Ok(self
            .state
            .lock()
            .expect("fake herdr mutex poisoned")
            .folder_workspaces
            .get(folder)
            .cloned())
    }

    fn workspace_create(&self, folder: &Path, label: &str) -> Result<WorkspaceCreated> {
        self.record(format!("workspace_create:{}:{label}", folder.display()));
        let mut state = self.state.lock().expect("fake herdr mutex poisoned");
        let id = state.next_id;
        state.next_id += 1;
        let workspace_id = format!("ws_{id}");
        state
            .folder_workspaces
            .insert(folder.to_path_buf(), workspace_id.clone());
        state.tab_panes.insert(format!("tab_{id}"), 1);
        let pane_id = format!("pane_{id}");
        state.panes.insert(
            pane_id.clone(),
            PaneInfo {
                pane_id: pane_id.clone(),
                terminal_id: format!("term_{id}"),
                workspace_id: workspace_id.clone(),
            },
        );
        Ok(WorkspaceCreated {
            workspace_id,
            tab: TabInfo {
                tab_id: format!("tab_{id}"),
                pane_id,
            },
        })
    }

    fn workspace_get(&self, workspace_id: &str) -> Result<Option<WorkspaceInfo>> {
        self.record(format!("workspace_get:{workspace_id}"));
        if self
            .state
            .lock()
            .expect("fake herdr mutex poisoned")
            .removed_workspaces
            .contains(workspace_id)
        {
            return Ok(None);
        }
        Ok(Some(WorkspaceInfo {
            workspace_id: workspace_id.into(),
            name: None,
            path: None,
        }))
    }

    fn workspace_report_metadata(&self, workspace_id: &str, status: Option<&str>) -> Result<()> {
        self.record(format!(
            "workspace_report_metadata:{workspace_id}:{}",
            status.unwrap_or("<clear>")
        ));
        match self
            .state
            .lock()
            .expect("fake herdr mutex poisoned")
            .metadata_error
            .clone()
        {
            Some(message) => Err(PodError::new(
                ErrorCode::HerdrError,
                message,
                &["Inspect the scripted fake herdr refusal."],
            )),
            None => Ok(()),
        }
    }

    fn notification_show(&self, title: &str, body: Option<&str>, sound: &str) -> Result<()> {
        self.record(format!(
            "notification_show:{title}:{}:{sound}",
            body.unwrap_or("<none>")
        ));
        Ok(())
    }

    fn worktree_create(&self, req: &WorktreeCreate) -> Result<WorktreeInfo> {
        self.record(format!(
            "worktree_create:{}:{}:{}",
            req.name,
            req.display_name.as_deref().unwrap_or("<none>"),
            req.repo.display()
        ));
        let mut state = self.state.lock().expect("fake herdr mutex poisoned");
        let id = state.next_id;
        state.next_id += 1;
        let workspace_id = format!("workspace_{}", req.name);
        state.tab_panes.insert(format!("tab_{id}"), 1);
        let pane_id = format!("pane_{id}");
        state.panes.insert(
            pane_id.clone(),
            PaneInfo {
                pane_id: pane_id.clone(),
                terminal_id: format!("term_{id}"),
                workspace_id: workspace_id.clone(),
            },
        );
        Ok(WorktreeInfo {
            workspace_id,
            path: req.repo.join(&req.name),
            branch: req.name.clone(),
            tab_id: format!("tab_{id}"),
            pane_id,
        })
    }

    fn worktree_list(&self, cwd: Option<&Path>) -> Result<WorktreeListing> {
        self.record(match cwd {
            Some(cwd) => format!("worktree_list:{}", cwd.display()),
            None => "worktree_list".into(),
        });
        let state = self.state.lock().expect("fake herdr mutex poisoned");
        if let Some(message) = &state.worktree_list_error {
            return Err(PodError::new(
                ErrorCode::HerdrError,
                message.clone(),
                &["herdr status server"],
            ));
        }
        Ok(WorktreeListing {
            source_checkout: state.worktree_source.clone(),
            worktrees: state.worktrees.clone(),
        })
    }

    fn worktree_open(&self, path: &Path, source: &Path) -> Result<WorktreeInfo> {
        self.record(format!(
            "worktree_open:{}:{}",
            path.display(),
            source.display()
        ));
        Ok(WorktreeInfo {
            workspace_id: "workspace_1".into(),
            path: path.to_path_buf(),
            branch: "main".into(),
            tab_id: "tab_1".into(),
            pane_id: "pane_1".into(),
        })
    }

    fn agent_start(
        &self,
        name: &str,
        kind: &str,
        pane_id: &str,
        argv: &[String],
        _timeout_ms: u64,
    ) -> Result<AgentInfo> {
        self.record(format!("agent_start:{name}:{kind}:{pane_id}"));
        let (error, session, detected) = {
            let mut state = self.state.lock().expect("fake herdr mutex poisoned");
            state.agent_start_argv.push(argv.to_vec());
            let error = state
                .agent_start_failures
                .pop_front()
                .or_else(|| state.agent_start_error.clone());
            (
                error,
                state.agent_start_session.clone().unwrap_or_default(),
                state.agent_start_kind.clone(),
            )
        };
        if let Some(message) = error {
            return Err(PodError::new(
                crate::error::ErrorCode::HerdrError,
                message,
                &["Inspect the scripted fake herdr failure."],
            ));
        }
        let info = AgentInfo {
            name: name.into(),
            kind: kind.into(),
            pane_id: pane_id.into(),
            pid: 1,
            status: AgentStatus::Idle,
            session_id: session.0,
            session_path: session.1,
            needs_input: Vec::new(),
            observed_at: Utc::now(),
        };
        self.set_agent(name, info.clone());
        Ok(AgentInfo {
            kind: detected.unwrap_or(info.kind.clone()),
            ..info
        })
    }

    fn agent_get(&self, name: &str) -> Result<Option<AgentInfo>> {
        self.record(format!("agent_get:{name}"));
        let mut state = self.state.lock().expect("fake herdr mutex poisoned");
        let scripted = state
            .agent_statuses
            .get_mut(name)
            .and_then(VecDeque::pop_front);
        if let (Some(status), Some(agent)) = (scripted, state.agents.get_mut(name)) {
            agent.status = status;
            agent.observed_at = Utc::now();
        }
        Ok(state.agents.get(name).cloned())
    }

    fn agent_list(&self) -> Result<Vec<AgentInfo>> {
        self.record("agent_list".into());
        Ok(self
            .state
            .lock()
            .expect("fake herdr mutex poisoned")
            .agents
            .values()
            .cloned()
            .collect())
    }

    fn agent_prompt(&self, name: &str, text: &str, wait_ms: Option<u64>) -> Result<PromptOutcome> {
        self.record(format!("agent_prompt:{name}:{text}"));
        let delay = {
            let mut state = self.state.lock().expect("fake herdr mutex poisoned");
            state.prompt_waits.push(wait_ms);
            state.prompt_delay
        };
        std::thread::sleep(delay);
        let mut state = self.state.lock().expect("fake herdr mutex poisoned");
        if let Some((code, message)) = state.agent_prompt_error.take() {
            return Err(PodError::new(code, message, &[]));
        }
        let pane_id = state
            .agents
            .get(name)
            .map(|agent| agent.pane_id.clone())
            .ok_or_else(|| {
                PodError::new(
                    ErrorCode::UnknownAgent,
                    format!("fake herdr has no agent {name}"),
                    &[],
                )
            })?;
        state.submissions.push((pane_id, text.into()));
        // A pane that never shows what was sent never shows a turn either.
        let unscripted = if state.render_sent_text {
            PromptOutcome::Submitted
        } else {
            PromptOutcome::Stalled
        };
        Ok(state.prompt_outcomes.pop_front().unwrap_or(unscripted))
    }

    fn agent_read(&self, name: &str, _lines: u32) -> Result<String> {
        self.record(format!("agent_read:{name}"));
        let pane = self
            .state
            .lock()
            .expect("fake herdr mutex poisoned")
            .agents
            .get(name)
            .map(|agent| agent.pane_id.clone());
        Ok(pane.map(|pane| self.screen(&pane)).unwrap_or_default())
    }

    fn instance_id(&self) -> u64 {
        self.state
            .lock()
            .expect("fake herdr mutex poisoned")
            .instance
    }

    fn events_subscribe(&self, pane_ids: &[String]) -> Result<Box<dyn EventStream>> {
        self.record(format!("events_subscribe:{}", pane_ids.join(",")));
        let events = self
            .state
            .lock()
            .expect("fake herdr mutex poisoned")
            .events
            .clone();
        Ok(Box::new(FakeEventStream {
            events,
            pane_ids: pane_ids.to_vec(),
        }))
    }
}
