use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::Result;

#[cfg(any(test, feature = "fake-herdr"))]
pub mod fake;
pub mod socket;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HerdrInfo {
    pub version: String,
    pub protocol: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PaneInfo {
    pub pane_id: String,
    pub terminal_id: String,
    pub workspace_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub shell_pid: u32,
    pub foreground_pid: u32,
    pub pids: Vec<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TabInfo {
    pub tab_id: String,
    pub pane_id: String,
}

/// A workspace herdr created for a folder, with its first tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceCreated {
    pub workspace_id: String,
    pub tab: TabInfo,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub workspace_id: String,
    pub name: Option<String>,
    pub path: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorktreeCreate {
    pub repo: PathBuf,
    pub name: String,
    pub base_branch: Option<String>,
    pub display_name: Option<String>,
    pub comment: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorktreeInfo {
    pub workspace_id: String,
    pub path: PathBuf,
    pub branch: String,
    pub tab_id: String,
    pub pane_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Idle,
    Working,
    Blocked,
    Done,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentInfo {
    pub name: String,
    pub kind: String,
    pub pane_id: String,
    pub pid: u32,
    pub status: AgentStatus,
    pub session_id: Option<String>,
    pub session_path: Option<PathBuf>,
    pub needs_input: Vec<String>,
    pub observed_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptOutcome {
    Submitted,
    Stalled,
    Blocked,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HerdrEvent {
    PaneExited {
        pane_id: String,
    },
    PaneClosed {
        pane_id: String,
    },
    AgentStatusChanged {
        pane_id: String,
        status: AgentStatus,
        needs_input: Vec<String>,
    },
}

pub trait EventStream {
    fn next(&mut self, timeout: Duration) -> Result<Option<HerdrEvent>>;
}

pub trait Herdr: Send + Sync {
    fn ping(&self) -> Result<HerdrInfo>;
    fn pane_current(&self) -> Result<PaneInfo>;
    fn pane_get(&self, pane_id: &str) -> Result<Option<PaneInfo>>;
    fn pane_process_info(&self, pane_id: &str) -> Result<ProcessInfo>;
    fn pane_read(&self, pane_id: &str, lines: u32) -> Result<String>;
    /// The same screen with its styling kept (ANSI escapes), so a caller can
    /// tell dim text (an agent's prompt suggestion) from typed input. The
    /// default serves the plain screen for adapters without styles.
    fn pane_read_styled(&self, pane_id: &str, lines: u32) -> Result<String> {
        self.pane_read(pane_id, lines)
    }
    fn pane_send_text(&self, pane_id: &str, text: &str) -> Result<()>;
    fn pane_send_keys(&self, pane_id: &str, keys: &[&str]) -> Result<()>;
    fn pane_close(&self, pane_id: &str) -> Result<()>;
    /// A new tab whose shell starts with `env` added to its environment.
    fn tab_create(
        &self,
        workspace_id: &str,
        name: &str,
        cwd: &Path,
        focus: bool,
        env: &[(&str, &str)],
    ) -> Result<TabInfo>;
    fn tab_close(&self, tab_id: &str) -> Result<()>;
    /// Sets the tab's label (herdr `tab.rename`).
    fn tab_rename(&self, tab_id: &str, label: &str) -> Result<()>;
    /// How many panes the tab holds now (herdr `tab.get`); `None` when the
    /// tab is gone.
    fn tab_pane_count(&self, tab_id: &str) -> Result<Option<u32>>;
    fn workspace_get(&self, workspace_id: &str) -> Result<Option<WorkspaceInfo>>;
    /// The workspace of a pane whose cwd is exactly `folder`, if any
    /// (herdr `pane.list`).
    fn workspace_for_folder(&self, folder: &Path) -> Result<Option<String>>;
    /// A new, unfocused workspace rooted at `folder` (herdr `workspace.create`).
    fn workspace_create(&self, folder: &Path, label: &str) -> Result<WorkspaceCreated>;
    /// Sets (`Some`) or clears (`None`) pod's `status` token in the
    /// workspace's sidebar metadata.
    fn workspace_report_metadata(&self, workspace_id: &str, status: Option<&str>) -> Result<()>;
    /// Shows a herdr notification (`notification.show`); `sound` is a herdr
    /// sound name such as `request`.
    fn notification_show(&self, title: &str, body: Option<&str>, sound: &str) -> Result<()>;
    fn worktree_create(&self, req: &WorktreeCreate) -> Result<WorktreeInfo>;
    /// The worktrees of the repository containing `cwd` (herdr's
    /// `worktree.list` with `cwd`). Without one herdr answers for its focused
    /// workspace, whatever the person is looking at, which may be no
    /// repository at all.
    fn worktree_list(&self, cwd: Option<&Path>) -> Result<Vec<WorktreeInfo>>;
    fn worktree_open(&self, path: &Path) -> Result<WorktreeInfo>;
    fn agent_start(
        &self,
        name: &str,
        kind: &str,
        pane_id: &str,
        argv: &[String],
        timeout_ms: u64,
    ) -> Result<AgentInfo>;
    fn agent_get(&self, name: &str) -> Result<Option<AgentInfo>>;
    fn agent_list(&self) -> Result<Vec<AgentInfo>>;
    fn agent_prompt(&self, name: &str, text: &str, wait_ms: Option<u64>) -> Result<PromptOutcome>;
    fn agent_read(&self, name: &str, lines: u32) -> Result<String>;
    fn events_subscribe(&self, pane_ids: &[String]) -> Result<Box<dyn EventStream>>;

    /// Identifies this client within the process, for state kept per herdr
    /// server (safe input's unconfirmed-paste guard). The default is the
    /// object's address, which a dropped client can hand to a new one; a
    /// test double that is created many times per process overrides it.
    fn instance_id(&self) -> u64 {
        self as *const Self as *const () as usize as u64
    }
}
