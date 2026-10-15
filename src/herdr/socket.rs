use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use chrono::Utc;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::{
    AgentInfo, AgentStatus, EventStream, Herdr, HerdrEvent, HerdrInfo, PaneInfo, ProcessInfo,
    PromptOutcome, TabInfo, WorkspaceCreated, WorkspaceInfo, WorktreeCreate, WorktreeInfo,
    WorktreeListing,
};
use crate::error::{ErrorCode, PodError, Result};

static REQUEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Upper bound for one ordinary request. herdr answers in milliseconds; this
/// only keeps a wedged server from hanging a `pod` process forever.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// `worktree.create`/`worktree.open` run git in herdr's app runtime.
const WORKTREE_TIMEOUT: Duration = Duration::from_secs(120);

pub struct SocketHerdr {
    socket_path: PathBuf,
}

impl SocketHerdr {
    pub fn connect(socket_path: &Path) -> Result<Self> {
        UnixStream::connect(socket_path).map_err(|error| unreachable_error(socket_path, &error))?;
        Ok(Self {
            socket_path: socket_path.to_path_buf(),
        })
    }

    fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        match self.request_reply(method, params, timeout)? {
            WireReply::Success(result) => Ok(result),
            WireReply::Error(error) => Err(herdr_error(error.message)),
        }
    }

    fn request_reply(&self, method: &str, params: Value, timeout: Duration) -> Result<WireReply> {
        let stream = UnixStream::connect(&self.socket_path)
            .map_err(|error| unreachable_error(&self.socket_path, &error))?;
        stream
            .set_read_timeout(Some(timeout))
            .and_then(|()| stream.set_write_timeout(Some(timeout)))
            .map_err(|error| unreachable_error(&self.socket_path, &error))?;
        let mut reader = BufReader::new(stream);
        let id = request_id();
        write_request(reader.get_mut(), &self.socket_path, &id, method, params)?;
        read_reply(&mut reader, &self.socket_path, &id, method)
    }

    fn expected(&self, method: &str, params: Value, result_type: &str) -> Result<Value> {
        self.expected_within(method, params, result_type, REQUEST_TIMEOUT)
    }

    fn expected_within(
        &self,
        method: &str,
        params: Value,
        result_type: &str,
        timeout: Duration,
    ) -> Result<Value> {
        let result = self.request(method, params, timeout)?;
        expect_result_type(method, result, result_type)
    }

    fn optional(
        &self,
        method: &str,
        params: Value,
        result_type: &str,
        target_kind: &str,
    ) -> Result<Option<Value>> {
        match self.request_reply(method, params, REQUEST_TIMEOUT)? {
            WireReply::Success(result) => expect_result_type(method, result, result_type).map(Some),
            WireReply::Error(error) if target_not_found(&error, target_kind) => Ok(None),
            WireReply::Error(error) => Err(herdr_error(error.message)),
        }
    }

    fn agent_from_value(&self, value: &Value, fallback_name: Option<&str>) -> Result<AgentInfo> {
        let raw: WireAgent = serde_json::from_value(value.clone())
            .map_err(|error| invalid_response("agent", error))?;
        let process = self.pane_process_info(&raw.pane_id)?;
        let (session_id, session_path) = match raw.agent_session {
            Some(session) if session.kind == "id" => (Some(session.value), None),
            Some(session) if session.kind == "path" => (None, Some(PathBuf::from(session.value))),
            _ => (None, None),
        };
        Ok(AgentInfo {
            name: raw
                .name
                .or_else(|| fallback_name.map(str::to_owned))
                .unwrap_or_default(),
            kind: raw.agent.unwrap_or_else(|| "unknown".into()),
            pane_id: raw.pane_id,
            pid: process.foreground_pid,
            status: raw.agent_status,
            session_id,
            session_path,
            needs_input: needs_input(&raw.state_labels),
            observed_at: Utc::now(),
        })
    }
}

fn expect_result_type(method: &str, result: Value, result_type: &str) -> Result<Value> {
    if result.get("type").and_then(Value::as_str) != Some(result_type) {
        return Err(herdr_error(format!(
            "herdr returned an unexpected result for {method}: {}",
            compact_json(&result)
        )));
    }
    Ok(result)
}

pub fn default_socket_path() -> PathBuf {
    let env = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    socket_path_from_env(&env)
}

fn socket_path_from_env(env: &dyn Fn(&str) -> Option<String>) -> PathBuf {
    env("HERDR_SOCKET_PATH")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| crate::paths::default_herdr_sockets(env).into_iter().next())
        .unwrap_or_else(|| PathBuf::from(".config/herdr/herdr.sock"))
}

pub fn open_from_env(env: &dyn Fn(&str) -> Option<String>) -> Result<Box<dyn Herdr>> {
    #[cfg(any(test, feature = "fake-herdr"))]
    if env("POD_FAKE_HERDR").as_deref() == Some("1") {
        return Ok(Box::new(super::fake::FakeHerdr::from_env(env)));
    }
    Ok(Box::new(SocketHerdr {
        socket_path: socket_path_from_env(env),
    }))
}

impl Herdr for SocketHerdr {
    fn ping(&self) -> Result<HerdrInfo> {
        let result = self.expected("ping", json!({}), "pong")?;
        from_result(result, "ping")
    }

    fn pane_current(&self) -> Result<PaneInfo> {
        let result = self.expected("pane.current", json!({}), "pane_current")?;
        pane_info(field(&result, "pane", "pane.current")?)
    }

    fn pane_get(&self, pane_id: &str) -> Result<Option<PaneInfo>> {
        let Some(result) =
            self.optional("pane.get", json!({"pane_id": pane_id}), "pane_info", "pane")?
        else {
            return Ok(None);
        };
        pane_info(field(&result, "pane", "pane.get")?).map(Some)
    }

    fn pane_process_info(&self, pane_id: &str) -> Result<ProcessInfo> {
        let result = self.expected(
            "pane.process_info",
            json!({"pane_id": pane_id}),
            "pane_process_info",
        )?;
        let raw: WireProcessInfo =
            serde_json::from_value(field(&result, "process_info", "pane.process_info")?.clone())
                .map_err(|error| invalid_response("pane.process_info", error))?;
        let shell_pid = raw.shell_pid.unwrap_or_default();
        let mut pids = Vec::with_capacity(raw.foreground_processes.len() + 1);
        if shell_pid != 0 {
            pids.push(shell_pid);
        }
        for process in &raw.foreground_processes {
            if !pids.contains(&process.pid) {
                pids.push(process.pid);
            }
        }
        // herdr does not order `foreground_processes` (0.9.3 lists children
        // before their parent), so the process-group leader is the process the
        // shell started, e.g. the agent rather than one of its tool children.
        let leader = raw.foreground_process_group_id.filter(|pgid| {
            raw.foreground_processes
                .iter()
                .any(|process| process.pid == *pgid)
        });
        let foreground_pid = leader
            .or_else(|| raw.foreground_processes.first().map(|process| process.pid))
            .or(raw.foreground_process_group_id)
            .unwrap_or(shell_pid);
        Ok(ProcessInfo {
            shell_pid,
            foreground_pid,
            pids,
        })
    }

    fn pane_read(&self, pane_id: &str, lines: u32) -> Result<String> {
        let result = self.expected(
            "pane.read",
            json!({
                "pane_id": pane_id,
                "source": "recent_unwrapped",
                "format": "text",
                "strip_ansi": true,
                "lines": lines
            }),
            "pane_read",
        )?;
        read_text(&result, "pane.read")
    }

    fn pane_read_styled(&self, pane_id: &str, lines: u32) -> Result<String> {
        let result = self.expected(
            "pane.read",
            json!({
                "pane_id": pane_id,
                "source": "recent_unwrapped",
                "format": "ansi",
                "strip_ansi": false,
                "lines": lines
            }),
            "pane_read",
        )?;
        read_text(&result, "pane.read")
    }

    fn pane_send_text(&self, pane_id: &str, text: &str) -> Result<()> {
        self.expected(
            "pane.send_text",
            json!({"pane_id": pane_id, "text": text}),
            "ok",
        )?;
        Ok(())
    }

    fn pane_send_keys(&self, pane_id: &str, keys: &[&str]) -> Result<()> {
        self.expected(
            "pane.send_keys",
            json!({"pane_id": pane_id, "keys": keys}),
            "ok",
        )?;
        Ok(())
    }

    fn pane_close(&self, pane_id: &str) -> Result<()> {
        self.expected("pane.close", json!({"pane_id": pane_id}), "ok")?;
        Ok(())
    }

    fn tab_create(
        &self,
        workspace_id: &str,
        name: &str,
        cwd: &Path,
        focus: bool,
        env: &[(&str, &str)],
    ) -> Result<TabInfo> {
        let env: serde_json::Map<String, Value> = env
            .iter()
            .map(|(key, value)| ((*key).to_owned(), Value::from(*value)))
            .collect();
        let result = self.expected(
            "tab.create",
            json!({
                "workspace_id": workspace_id,
                "label": name,
                "cwd": cwd,
                "focus": focus,
                "env": env
            }),
            "tab_created",
        )?;
        let tab: WireTab = parse_field(&result, "tab", "tab.create")?;
        let pane: WirePane = parse_field(&result, "root_pane", "tab.create")?;
        Ok(TabInfo {
            tab_id: tab.tab_id,
            pane_id: pane.pane_id,
        })
    }

    fn tab_close(&self, tab_id: &str) -> Result<()> {
        self.expected("tab.close", json!({"tab_id": tab_id}), "ok")?;
        Ok(())
    }

    fn workspace_close(&self, workspace_id: &str) -> Result<()> {
        self.expected(
            "workspace.close",
            json!({"workspace_id": workspace_id}),
            "ok",
        )?;
        Ok(())
    }

    fn tab_rename(&self, tab_id: &str, label: &str) -> Result<()> {
        self.expected(
            "tab.rename",
            json!({"tab_id": tab_id, "label": label}),
            "tab_info",
        )?;
        Ok(())
    }

    fn tab_pane_count(&self, tab_id: &str) -> Result<Option<u32>> {
        let Some(result) =
            self.optional("tab.get", json!({"tab_id": tab_id}), "tab_info", "tab")?
        else {
            return Ok(None);
        };
        let tab = field(&result, "tab", "tab.get")?;
        tab.get("pane_count")
            .and_then(Value::as_u64)
            .and_then(|count| u32::try_from(count).ok())
            .map(Some)
            .ok_or_else(|| herdr_error("invalid herdr tab.get response: missing pane_count"))
    }

    fn workspace_for_folder(&self, folder: &Path) -> Result<Option<String>> {
        let result = self.expected("pane.list", json!({}), "pane_list")?;
        let panes: Vec<WireListedPane> = parse_field(&result, "panes", "pane.list")?;
        Ok(panes
            .into_iter()
            .find(|pane| pane.cwd.as_deref() == Some(folder))
            .map(|pane| pane.workspace_id))
    }

    fn workspace_create(&self, folder: &Path, label: &str) -> Result<WorkspaceCreated> {
        let result = self.expected(
            "workspace.create",
            json!({"cwd": folder, "label": label, "focus": false}),
            "workspace_created",
        )?;
        let workspace: WireWorkspace = parse_field(&result, "workspace", "workspace.create")?;
        let tab: WireTab = parse_field(&result, "tab", "workspace.create")?;
        let pane: WirePane = parse_field(&result, "root_pane", "workspace.create")?;
        Ok(WorkspaceCreated {
            workspace_id: workspace.workspace_id,
            tab: TabInfo {
                tab_id: tab.tab_id,
                pane_id: pane.pane_id,
            },
        })
    }

    fn workspace_get(&self, workspace_id: &str) -> Result<Option<WorkspaceInfo>> {
        let Some(result) = self.optional(
            "workspace.get",
            json!({"workspace_id": workspace_id}),
            "workspace_info",
            "workspace",
        )?
        else {
            return Ok(None);
        };
        workspace_info(field(&result, "workspace", "workspace.get")?).map(Some)
    }

    fn workspace_report_metadata(&self, workspace_id: &str, status: Option<&str>) -> Result<()> {
        self.expected(
            "workspace.report_metadata",
            json!({
                "workspace_id": workspace_id,
                "source": "pod",
                "tokens": {"status": status}
            }),
            "ok",
        )?;
        Ok(())
    }

    fn notification_show(&self, title: &str, body: Option<&str>, sound: &str) -> Result<()> {
        let mut params = json!({"title": title, "sound": sound});
        if let Some(body) = body {
            params["body"] = json!(body);
        }
        self.expected("notification.show", params, "notification_show")?;
        Ok(())
    }

    fn worktree_create(&self, req: &WorktreeCreate) -> Result<WorktreeInfo> {
        let result = self.expected_within(
            "worktree.create",
            json!({
                "cwd": req.repo,
                "branch": req.name,
                "base": req.base_branch,
                "label": req.display_name,
                "focus": false,
                "trust_repository": false
            }),
            "worktree_created",
            WORKTREE_TIMEOUT,
        )?;
        worktree_info(&result, "worktree.create")
    }

    fn worktree_list(&self, cwd: Option<&Path>) -> Result<WorktreeListing> {
        let mut params = json!({"trust_repository": false});
        if let Some(cwd) = cwd {
            params["cwd"] = json!(cwd);
        }
        let result =
            self.expected_within("worktree.list", params, "worktree_list", WORKTREE_TIMEOUT)?;
        let source_checkout = result
            .get("source")
            .and_then(|source| source.get("source_checkout_path"))
            .and_then(Value::as_str)
            .map(PathBuf::from);
        let values = result
            .get("worktrees")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                herdr_error("invalid herdr worktree.list response: missing worktrees")
            })?;
        let worktrees = values
            .iter()
            .map(|value| {
                let worktree: WireWorktree = serde_json::from_value(value.clone())
                    .map_err(|error| invalid_response("worktree.list", error))?;
                Ok(WorktreeInfo {
                    already_open: worktree.open_workspace_id.is_some(),
                    workspace_id: worktree.open_workspace_id.unwrap_or_default(),
                    path: worktree.path,
                    branch: worktree.branch.unwrap_or_default(),
                    tab_id: String::new(),
                    pane_id: String::new(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(WorktreeListing {
            source_checkout,
            worktrees,
        })
    }

    fn worktree_open(&self, path: &Path, source: &Path) -> Result<WorktreeInfo> {
        // `cwd` is the folder herdr starts from and must be the repository's
        // source checkout. Without it herdr searches its focused workspace's
        // repository, whatever the person is looking at (`worktree_not_found`
        // for a worktree of another repository, git's "not a git repository"
        // while the focused folder was none), and a linked worktree is refused
        // as the start (`linked_worktree_source`); all seen live 2026-10-15.
        let result = self.expected_within(
            "worktree.open",
            json!({"cwd": source, "path": path, "focus": false, "trust_repository": false}),
            "worktree_opened",
            WORKTREE_TIMEOUT,
        )?;
        worktree_info(&result, "worktree.open")
    }

    fn agent_start(
        &self,
        name: &str,
        kind: &str,
        pane_id: &str,
        argv: &[String],
        timeout_ms: u64,
    ) -> Result<AgentInfo> {
        let result = self.expected_within(
            "agent.start",
            json!({
                "name": name,
                "kind": kind,
                "pane_id": pane_id,
                "args": argv,
                "timeout_ms": timeout_ms
            }),
            "agent_started",
            Duration::from_millis(timeout_ms).saturating_add(REQUEST_TIMEOUT),
        )?;
        self.agent_from_value(field(&result, "agent", "agent.start")?, Some(name))
    }

    fn agent_get(&self, name: &str) -> Result<Option<AgentInfo>> {
        let Some(result) =
            self.optional("agent.get", json!({"target": name}), "agent_info", "agent")?
        else {
            return Ok(None);
        };
        self.agent_from_value(field(&result, "agent", "agent.get")?, Some(name))
            .map(Some)
    }

    fn agent_list(&self) -> Result<Vec<AgentInfo>> {
        let result = self.expected("agent.list", json!({}), "agent_list")?;
        let agents = field(&result, "agents", "agent.list")?
            .as_array()
            .ok_or_else(|| {
                herdr_error("invalid herdr agent.list response: agents is not an array")
            })?;
        agents
            .iter()
            .map(|agent| self.agent_from_value(agent, None))
            .collect()
    }

    fn agent_prompt(&self, name: &str, text: &str, wait_ms: Option<u64>) -> Result<PromptOutcome> {
        // `until` makes herdr return as soon as the turn is observed instead
        // of waiting for it to settle, which would turn every turn longer than
        // `wait_ms` into a `timeout` error after the text was submitted.
        let wait = wait_ms
            .map(|timeout_ms| json!({"timeout_ms": timeout_ms, "until": ["working", "blocked"]}));
        let timeout = wait_ms
            .map(Duration::from_millis)
            .unwrap_or_default()
            .saturating_add(REQUEST_TIMEOUT);
        match self.request_reply(
            "agent.prompt",
            json!({"target": name, "text": text, "wait": wait}),
            timeout,
        )? {
            WireReply::Success(result)
                if result.get("type").and_then(Value::as_str) == Some("agent_prompted") =>
            {
                Ok(PromptOutcome::Submitted)
            }
            WireReply::Success(result) => Err(herdr_error(format!(
                "herdr returned an unexpected result for agent.prompt: {}",
                compact_json(&result)
            ))),
            WireReply::Error(error) if error.code == "agent_prompt_stalled" => {
                Ok(PromptOutcome::Stalled)
            }
            // herdr reports submission before it waits, so a caller timeout
            // means the text went in but no turn was observed in time.
            WireReply::Error(error) if wait_ms.is_some() && error.code == "timeout" => {
                Ok(PromptOutcome::Stalled)
            }
            WireReply::Error(error) if error.code == "agent_blocked" => Ok(PromptOutcome::Blocked),
            WireReply::Error(error) => Err(herdr_error(error.message)),
        }
    }

    fn agent_read(&self, name: &str, lines: u32) -> Result<String> {
        let result = self.expected(
            "agent.read",
            json!({
                "target": name,
                "source": "recent_unwrapped",
                "format": "text",
                "strip_ansi": true,
                "lines": lines
            }),
            "pane_read",
        )?;
        read_text(&result, "agent.read")
    }

    fn events_subscribe(&self, pane_ids: &[String]) -> Result<Box<dyn EventStream>> {
        let stream = UnixStream::connect(&self.socket_path)
            .map_err(|error| unreachable_error(&self.socket_path, &error))?;
        let mut reader = BufReader::new(stream);
        let id = request_id();
        let mut subscriptions = vec![
            json!({"type": "pane.exited"}),
            json!({"type": "pane.closed"}),
        ];
        subscriptions.extend(
            pane_ids
                .iter()
                .map(|pane_id| json!({"type": "pane.agent_status_changed", "pane_id": pane_id})),
        );
        reader
            .get_ref()
            .set_read_timeout(Some(REQUEST_TIMEOUT))
            .and_then(|()| reader.get_ref().set_write_timeout(Some(REQUEST_TIMEOUT)))
            .map_err(|error| unreachable_error(&self.socket_path, &error))?;
        write_request(
            reader.get_mut(),
            &self.socket_path,
            &id,
            "events.subscribe",
            json!({"subscriptions": subscriptions}),
        )?;
        match read_reply(&mut reader, &self.socket_path, &id, "events.subscribe")? {
            WireReply::Success(result)
                if result.get("type").and_then(Value::as_str) == Some("subscription_started") =>
            {
                Ok(Box::new(SocketEventStream {
                    reader,
                    socket_path: self.socket_path.clone(),
                    pane_ids: pane_ids.iter().cloned().collect(),
                    pending: Vec::new(),
                }))
            }
            WireReply::Success(result) => Err(herdr_error(format!(
                "herdr returned an unexpected result for events.subscribe: {}",
                compact_json(&result)
            ))),
            WireReply::Error(error) => Err(herdr_error(error.message)),
        }
    }
}

struct SocketEventStream {
    reader: BufReader<UnixStream>,
    socket_path: PathBuf,
    pane_ids: BTreeSet<String>,
    /// Bytes of a frame whose newline has not arrived yet; kept across a
    /// timeout so the next call resumes the same frame.
    pending: Vec<u8>,
}

impl EventStream for SocketEventStream {
    fn next(&mut self, timeout: Duration) -> Result<Option<HerdrEvent>> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            // macOS refuses SO_RCVTIMEO (EINVAL) once herdr has closed its end.
            // A shut-down socket never blocks, so the read below still drains
            // the frames herdr sent before closing and then reports EOF.
            let _ = self.reader.get_ref().set_read_timeout(Some(remaining));
            match self.reader.read_until(b'\n', &mut self.pending) {
                Ok(_) if self.pending.last() == Some(&b'\n') => {}
                Ok(_) => {
                    return Err(unreachable_error(
                        &self.socket_path,
                        &std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "herdr event stream closed",
                        ),
                    ));
                }
                Err(error) if is_timeout(&error) => return Ok(None),
                Err(error) => return Err(unreachable_error(&self.socket_path, &error)),
            }
            let frame = std::mem::take(&mut self.pending);
            let envelope: Value = serde_json::from_slice(&frame)
                .map_err(|error| invalid_response("events.subscribe", error))?;
            if let Some(event) = parse_event(&envelope)?
                && (self.pane_ids.is_empty() || self.pane_ids.contains(event_pane_id(&event)))
            {
                return Ok(Some(event));
            }
        }
    }
}

enum WireReply {
    Success(Value),
    Error(WireError),
}

#[derive(Deserialize)]
struct WireError {
    code: String,
    message: String,
}

#[derive(Deserialize)]
struct WirePane {
    pane_id: String,
    terminal_id: String,
    workspace_id: String,
}

#[derive(Deserialize)]
struct WireTab {
    tab_id: String,
}

#[derive(Deserialize)]
struct WireListedPane {
    workspace_id: String,
    cwd: Option<PathBuf>,
}

#[derive(Deserialize)]
struct WireWorkspace {
    workspace_id: String,
    label: Option<String>,
    worktree: Option<WireWorkspaceWorktree>,
    #[serde(default)]
    tab_count: Option<u32>,
    #[serde(default)]
    pane_count: Option<u32>,
    #[serde(default)]
    agent_status: Option<String>,
}

#[derive(Deserialize)]
struct WireWorkspaceWorktree {
    checkout_path: PathBuf,
}

#[derive(Deserialize)]
struct WireWorktree {
    path: PathBuf,
    branch: Option<String>,
    #[serde(default)]
    open_workspace_id: Option<String>,
}

#[derive(Deserialize)]
struct WireProcessInfo {
    shell_pid: Option<u32>,
    foreground_process_group_id: Option<u32>,
    #[serde(default)]
    foreground_processes: Vec<WireProcess>,
}

#[derive(Deserialize)]
struct WireProcess {
    pid: u32,
}

#[derive(Deserialize)]
struct WireAgent {
    name: Option<String>,
    agent: Option<String>,
    pane_id: String,
    agent_status: AgentStatus,
    agent_session: Option<WireAgentSession>,
    #[serde(default)]
    state_labels: Map<String, Value>,
}

#[derive(Deserialize)]
struct WireAgentSession {
    kind: String,
    value: String,
}

fn request_id() -> String {
    format!(
        "pod:{}:{}",
        std::process::id(),
        REQUEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

fn write_request(
    stream: &mut UnixStream,
    socket_path: &Path,
    id: &str,
    method: &str,
    params: Value,
) -> Result<()> {
    // One write per frame: an I/O failure is then always herdr_unreachable,
    // never a half-sent request reported as an encoding error.
    let mut frame = serde_json::to_vec(&json!({"id": id, "method": method, "params": params}))
        .map_err(|error| herdr_error(format!("could not encode herdr request: {error}")))?;
    frame.push(b'\n');
    stream
        .write_all(&frame)
        .and_then(|()| stream.flush())
        .map_err(|error| io_failure(socket_path, method, &error))
}

fn read_reply(
    reader: &mut BufReader<UnixStream>,
    socket_path: &Path,
    expected_id: &str,
    method: &str,
) -> Result<WireReply> {
    let mut frame = String::new();
    let bytes = reader
        .read_line(&mut frame)
        .map_err(|error| io_failure(socket_path, method, &error))?;
    if bytes == 0 {
        return Err(unreachable_error(
            socket_path,
            &std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "herdr closed the socket before replying",
            ),
        ));
    }
    let mut envelope: Value = serde_json::from_str(&frame)
        .map_err(|error| invalid_response("response envelope", error))?;
    if envelope.get("id").and_then(Value::as_str) != Some(expected_id) {
        return Err(herdr_error("herdr response ID did not match the request"));
    }
    if let Some(error) = envelope.get("error") {
        let error = serde_json::from_value(error.clone())
            .map_err(|error| invalid_response("error envelope", error))?;
        return Ok(WireReply::Error(error));
    }
    let result = envelope
        .as_object_mut()
        .and_then(|object| object.remove("result"))
        .ok_or_else(|| herdr_error("herdr response contained neither result nor error"))?;
    Ok(WireReply::Success(result))
}

fn pane_info(value: &Value) -> Result<PaneInfo> {
    let raw: WirePane =
        serde_json::from_value(value.clone()).map_err(|error| invalid_response("pane", error))?;
    Ok(PaneInfo {
        pane_id: raw.pane_id,
        terminal_id: raw.terminal_id,
        workspace_id: raw.workspace_id,
    })
}

fn workspace_info(value: &Value) -> Result<WorkspaceInfo> {
    let raw: WireWorkspace = serde_json::from_value(value.clone())
        .map_err(|error| invalid_response("workspace", error))?;
    Ok(WorkspaceInfo {
        workspace_id: raw.workspace_id,
        name: raw.label,
        path: raw.worktree.map(|worktree| worktree.checkout_path),
        tab_count: raw.tab_count,
        pane_count: raw.pane_count,
        agent_status: raw.agent_status,
    })
}

fn worktree_info(result: &Value, method: &str) -> Result<WorktreeInfo> {
    let workspace: WireWorkspace = parse_field(result, "workspace", method)?;
    let tab: WireTab = parse_field(result, "tab", method)?;
    let pane: WirePane = parse_field(result, "root_pane", method)?;
    let worktree: WireWorktree = parse_field(result, "worktree", method)?;
    Ok(WorktreeInfo {
        workspace_id: workspace.workspace_id,
        path: worktree.path,
        branch: worktree.branch.unwrap_or_default(),
        tab_id: tab.tab_id,
        pane_id: pane.pane_id,
        already_open: result
            .get("already_open")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn read_text(result: &Value, method: &str) -> Result<String> {
    field(result, "read", method)?
        .get("text")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            herdr_error(format!(
                "invalid herdr {method} response: missing read.text"
            ))
        })
}

fn parse_event(envelope: &Value) -> Result<Option<HerdrEvent>> {
    let Some(kind) = envelope.get("event").and_then(Value::as_str) else {
        return Err(herdr_error("invalid herdr event envelope: missing event"));
    };
    let data = envelope
        .get("data")
        .ok_or_else(|| herdr_error("invalid herdr event envelope: missing data"))?;
    let pane_id = || {
        data.get("pane_id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| herdr_error(format!("invalid herdr {kind} event: missing pane_id")))
    };
    // herdr 0.9.3 sends global events under their `EventKind` names
    // (`pane_exited`, `pane_closed`) and pane-scoped subscription events under
    // `SubscriptionEventKind` names (`pane.agent_status_changed`). Both
    // spellings are accepted for each.
    match kind {
        "pane_exited" | "pane.exited" => Ok(Some(HerdrEvent::PaneExited {
            pane_id: pane_id()?,
        })),
        "pane_closed" | "pane.closed" => Ok(Some(HerdrEvent::PaneClosed {
            pane_id: pane_id()?,
        })),
        "pane.agent_status_changed" | "pane_agent_status_changed" => {
            let status = data
                .get("agent_status")
                .cloned()
                .ok_or_else(|| herdr_error("invalid herdr agent status event: missing status"))?;
            let status = serde_json::from_value(status)
                .map_err(|error| invalid_response("agent status event", error))?;
            let labels = data
                .get("state_labels")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            Ok(Some(HerdrEvent::AgentStatusChanged {
                pane_id: pane_id()?,
                status,
                needs_input: needs_input(&labels),
            }))
        }
        _ => Ok(None),
    }
}

fn event_pane_id(event: &HerdrEvent) -> &str {
    match event {
        HerdrEvent::PaneExited { pane_id }
        | HerdrEvent::PaneClosed { pane_id }
        | HerdrEvent::AgentStatusChanged { pane_id, .. } => pane_id,
    }
}

fn needs_input(labels: &Map<String, Value>) -> Vec<String> {
    labels
        .iter()
        .map(|(key, value)| {
            value
                .as_str()
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| key.clone())
        })
        .collect()
}

fn target_not_found(error: &WireError, target_kind: &str) -> bool {
    let exact_code = match target_kind {
        "pane" => matches!(
            error.code.as_str(),
            "pane_not_found" | "target_pane_not_found"
        ),
        "workspace" => error.code == "workspace_not_found",
        "tab" => error.code == "tab_not_found",
        "agent" => matches!(
            error.code.as_str(),
            "agent_not_found" | "agent_name_not_found" | "agent_pane_not_found"
        ),
        _ => false,
    };
    if exact_code {
        return true;
    }
    let message = error.message.to_ascii_lowercase();
    message.contains("unknown target")
        || (message.contains(target_kind)
            && (message.contains("not found") || message.contains("does not exist")))
}

fn field<'a>(value: &'a Value, name: &str, method: &str) -> Result<&'a Value> {
    value
        .get(name)
        .ok_or_else(|| herdr_error(format!("invalid herdr {method} response: missing {name}")))
}

fn parse_field<T: for<'de> Deserialize<'de>>(value: &Value, name: &str, method: &str) -> Result<T> {
    serde_json::from_value(field(value, name, method)?.clone())
        .map_err(|error| invalid_response(method, error))
}

fn from_result<T: for<'de> Deserialize<'de>>(value: Value, method: &str) -> Result<T> {
    serde_json::from_value(value).map_err(|error| invalid_response(method, error))
}

fn invalid_response(context: &str, error: impl std::fmt::Display) -> PodError {
    herdr_error(format!("invalid herdr {context} response: {error}"))
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<invalid JSON>".into())
}

fn unreachable_error(socket_path: &Path, error: &std::io::Error) -> PodError {
    PodError::new(
        ErrorCode::HerdrUnreachable,
        format!(
            "herdr is not reachable at {}: {error}",
            socket_path.display()
        ),
        &["herdr status server"],
    )
}

fn is_timeout(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

fn io_failure(socket_path: &Path, method: &str, error: &std::io::Error) -> PodError {
    if !is_timeout(error) {
        return unreachable_error(socket_path, error);
    }
    PodError::new(
        ErrorCode::HerdrUnreachable,
        format!(
            "herdr did not answer {method} in time at {}",
            socket_path.display()
        ),
        &["herdr status server"],
    )
}

fn herdr_error(message: impl Into<String>) -> PodError {
    PodError::new(ErrorCode::HerdrError, message, &["herdr status server"])
}
