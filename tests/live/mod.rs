//! Harness for the live suite (`scripts/live.sh`): an isolated herdr server
//! with the pod plugin linked, driving real `claude` and `codex` workers.
//!
//! Isolation is partial on purpose. herdr runs with its own config and state
//! directories, so the user's sessions, plugins, and pod store are never
//! touched, but `HOME` stays real because the agents authenticate there. The
//! store directory is never overridden: it derives from the socket path
//! (`paths::session_from_env`), which is what lets a worker started in a new
//! worktree find the same store. Trust entries the run adds to the agents' own
//! config files are removed again when the session drops.
#![allow(dead_code)]

use std::ffi::OsStr;
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::Once;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

pub const CLAUDE_MODEL: &str = "claude-sonnet-5-5";
pub const CODEX_MODEL: &str = "gpt-5.6-terra";
const START_TIMEOUT_MS: &str = "120000";

/// The agent kinds to exercise: both by default, or `POD_LIVE_KINDS`
/// (comma separated) to rerun one while fixing it.
pub fn kinds() -> Vec<&'static str> {
    let wanted = std::env::var("POD_LIVE_KINDS").unwrap_or_default();
    ["claude", "codex"]
        .into_iter()
        .filter(|kind| wanted.is_empty() || wanted.split(',').any(|part| part == *kind))
        .collect()
}

pub struct Worker {
    pub kind: String,
    pub receipt: Value,
    pub dispatch: String,
    pub pane: String,
    pub handle: String,
}

pub struct Session {
    temp: tempfile::TempDir,
    repo: PathBuf,
    /// The git repository the coordinator works in; `current` workers use it.
    pub project: PathBuf,
    token: String,
    pod: PathBuf,
    herdr: PathBuf,
    name: String,
    socket: PathBuf,
    xdg_config: PathBuf,
    xdg_state: PathBuf,
    path: String,
    results: PathBuf,
    pub coordinator_pane: String,
    pub run_id: String,
    server: Option<Child>,
    trust: TrustGuard,
    worktrees: std::cell::RefCell<Vec<PathBuf>>,
    failures: std::cell::RefCell<Vec<String>>,
    soft: std::cell::RefCell<Vec<String>>,
    started: std::cell::RefCell<Vec<String>>,
}

impl Session {
    pub fn new(name: &str) -> Self {
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        // `POD_LIVE_POD` runs the suite against another build of pod, for
        // example `main` while a branch still lacks a fix.
        let built_pod = std::env::var("POD_LIVE_POD")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(env!("CARGO_BIN_EXE_pod")));
        let herdr = which("herdr");
        for agent in ["claude", "codex"] {
            which(agent);
        }
        let version = Command::new(&herdr)
            .arg("--version")
            .output()
            .expect("run herdr --version");
        assert!(version.status.success(), "herdr --version failed");
        let version = String::from_utf8_lossy(&version.stdout).trim().to_owned();
        let numbers: Vec<u32> = version
            .rsplit(' ')
            .next()
            .unwrap_or_default()
            .split('.')
            .filter_map(|part| part.parse().ok())
            .collect();
        assert!(
            numbers.len() >= 3 && (numbers[0], numbers[1], numbers[2]) >= (0, 9, 3),
            "the live suite needs herdr 0.9.3 or newer; got {version}"
        );

        // Unix-domain sockets have a small path limit (104 bytes on macOS),
        // so the isolated session root stays short.
        let temp = tempfile::Builder::new()
            .prefix("pod-live-")
            .tempdir_in("/tmp")
            .expect("create live temp directory");
        let xdg_config = temp.path().join("xdg-config");
        let xdg_state = temp.path().join("xdg-state");
        let bin = temp.path().join("bin");
        let token = format!("podlive{}", std::process::id());
        let project = temp.path().join(&token);
        for dir in [&xdg_config, &xdg_state, &bin, &project] {
            fs::create_dir_all(dir).expect("create live directory");
        }
        let pod = bin.join("pod");
        fs::copy(&built_pod, &pod).expect("install short-path pod binary");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&pod, fs::Permissions::from_mode(0o700))
                .expect("make pod executable");
        }
        git(&project, &["init", "-q", "-b", "main"]);
        fs::write(project.join("README.md"), "pod live suite\n").expect("write seed file");
        git(&project, &["add", "README.md"]);
        git(
            &project,
            &[
                "-c",
                "user.name=pod live",
                "-c",
                "user.email=live@example.invalid",
                "commit",
                "-q",
                "-m",
                "seed",
            ],
        );

        // pod's defaults: the built-in Codex table already starts every
        // worker with `--no-daemon` (the desktop app's shared server dialog).
        fs::create_dir_all(xdg_config.join("pod")).expect("create pod config directory");
        fs::write(
            xdg_config.join("pod/config.toml"),
            "# the live suite runs on pod's defaults\n",
        )
        .expect("write live pod config");
        let socket = xdg_config
            .join("herdr/sessions")
            .join(name)
            .join("herdr.sock");
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let results = results_path(&repo);
        let mut session = Self {
            temp,
            repo,
            project,
            token,
            pod,
            herdr,
            name: name.to_owned(),
            socket,
            xdg_config,
            xdg_state,
            path,
            results,
            coordinator_pane: String::new(),
            run_id: String::new(),
            server: None,
            trust: TrustGuard::capture(),
            worktrees: Default::default(),
            failures: Default::default(),
            soft: Default::default(),
            started: Default::default(),
        };
        session.start_results(&version);
        session.link_plugin();
        session.start_server();
        session.coordinator_pane = session.create_workspace();
        session.run_id = string_at(
            &session.pod(&["run-create", "--objective", "pod live suite"]),
            &["run", "id"],
        );
        session
    }

    fn env(&self, command: &mut Command) {
        // Run from inside a live herdr pane, these would name the user's
        // session; nothing in the suite may reach it.
        for inherited in [
            "HERDR_ENV",
            "HERDR_SOCKET_PATH",
            "HERDR_CLIENT_SOCKET_PATH",
            "HERDR_PANE_ID",
            "HERDR_TAB_ID",
            "HERDR_WORKSPACE_ID",
            "HERDR_BIN_PATH",
            "POD_STATE_DIR",
            "POD_CONFIG",
        ] {
            command.env_remove(inherited);
        }
        command
            .env("XDG_CONFIG_HOME", &self.xdg_config)
            .env("XDG_STATE_HOME", &self.xdg_state)
            .env(
                "HERDR_CONFIG_PATH",
                self.xdg_config.join("herdr/config.toml"),
            )
            .env("PATH", &self.path);
    }

    pub fn herdr_command(&self) -> Command {
        let mut command = Command::new(&self.herdr);
        self.env(&mut command);
        command.arg("--session").arg(&self.name);
        command
    }

    fn link_plugin(&mut self) {
        let mut command = self.herdr_command();
        command
            .args(["plugin", "link"])
            .arg(&self.repo)
            .arg("--enabled");
        successful(command.output().expect("link pod plugin"), "plugin link");
    }

    fn start_server(&mut self) {
        let mut command = self.herdr_command();
        command
            .arg("server")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        self.server = Some(command.spawn().expect("start isolated herdr server"));
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if self.socket.exists() {
                let mut status = self.herdr_command();
                status
                    .args(["status", "server"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                if status.status().is_ok_and(|status| status.success()) {
                    return;
                }
            }
            if self
                .server
                .as_mut()
                .and_then(|child| child.try_wait().ok().flatten())
                .is_some()
            {
                panic!("isolated herdr server exited during startup");
            }
            thread::sleep(Duration::from_millis(50));
        }
        panic!(
            "isolated herdr server did not create {}",
            self.socket.display()
        );
    }

    /// Stops the isolated server. Teardown in `Drop` must neither panic while
    /// unwinding nor wait forever on a server that ignored the stop, so it
    /// kills the child instead.
    fn stop_server(&mut self) {
        let mut command = Command::new(&self.herdr);
        self.env(&mut command);
        let _ = command
            .args(["session", "stop", &self.name, "--json"])
            .output();
        if let Some(mut server) = self.server.take() {
            let deadline = Instant::now() + Duration::from_secs(10);
            while server.try_wait().ok().flatten().is_none() {
                if Instant::now() >= deadline {
                    let _ = server.kill();
                    let _ = server.wait();
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    }

    fn create_workspace(&self) -> String {
        let mut command = self.herdr_command();
        command
            .args(["workspace", "create", "--cwd"])
            .arg(&self.project)
            .arg("--label")
            .arg("pod live")
            .arg("--no-focus");
        command.arg("--env").arg(format!("PATH={}", self.path));
        let value = json(successful(
            command.output().expect("create coordinator workspace"),
            "workspace create",
        ));
        string_at(&value, &["result", "root_pane", "pane_id"])
    }

    pub fn pod_command(&self, pane: &str) -> Command {
        let mut command = Command::new(&self.pod);
        self.env(&mut command);
        command
            .current_dir(&self.project)
            .env("HERDR_ENV", "1")
            .env("HERDR_PANE_ID", pane)
            .env("HERDR_SOCKET_PATH", &self.socket);
        command
    }

    /// Runs a pod command as the coordinator and returns its JSON receipt.
    pub fn pod(&self, args: &[&str]) -> Value {
        let mut command = self.pod_command(&self.coordinator_pane);
        command.args(args).arg("--json");
        json(successful(
            command.output().expect("run pod"),
            &args.join(" "),
        ))
    }

    /// Like `pod`, for a command whose failure is part of the case: returns
    /// the exit status with whichever stream holds the JSON.
    pub fn pod_raw(&self, args: &[&str]) -> (bool, Value) {
        let mut command = self.pod_command(&self.coordinator_pane);
        command.args(args).arg("--json");
        let output = command.output().expect("run pod");
        let bytes = if output.stdout.is_empty() {
            output.stderr.clone()
        } else {
            output.stdout.clone()
        };
        (output.status.success(), json(bytes))
    }

    pub fn herdr(&self, args: &[&str]) -> Value {
        let mut command = self.herdr_command();
        command.args(args);
        json(successful(
            command.output().expect("run herdr"),
            &args.join(" "),
        ))
    }

    pub fn herdr_text(&self, args: &[&str]) -> String {
        let mut command = self.herdr_command();
        command.args(args);
        String::from_utf8_lossy(&successful(
            command.output().expect("run herdr"),
            &args.join(" "),
        ))
        .into_owned()
    }

    fn model(kind: &str) -> String {
        match kind {
            "claude" => std::env::var("POD_LIVE_CLAUDE_MODEL").unwrap_or(CLAUDE_MODEL.into()),
            "codex" => std::env::var("POD_LIVE_CODEX_MODEL").unwrap_or(CODEX_MODEL.into()),
            other => panic!("no live model for {other}"),
        }
    }

    /// Starts a real `kind` worker. `placement` is `current` (the project
    /// folder) or `new-child` (a worktree of the project named `live-<kind>`).
    pub fn start_worker(&self, kind: &str, placement: &str, spec: &str) -> Worker {
        let name = format!("live-{kind}");
        let mut extra: Vec<&str> = vec!["--worktree", placement];
        let repo;
        if placement == "new-child" {
            repo = format!("path:{}", self.project.display());
            extra.extend(["--name", &name, "--repo", &repo]);
        }
        self.start_worker_with(kind, &extra, spec)
    }

    /// `extra` carries placement or reuse flags (`--worktree ...` or
    /// `--terminal <handle>`); `--agent` is left out when reusing a pane.
    pub fn start_worker_with(&self, kind: &str, extra: &[&str], spec: &str) -> Worker {
        let model = Self::model(kind);
        let mut args = vec!["worker-start", "--spec", spec];
        if !extra.contains(&"--terminal") {
            args.extend(["--agent", kind, "--model", &model, "--yolo"]);
        }
        args.extend(extra.iter().copied());
        args.extend(["--timeout-ms", START_TIMEOUT_MS]);
        let mut command = self.pod_command(&self.coordinator_pane);
        command.args(&args).arg("--json");
        let output = command.output().expect("start worker");
        let receipt = json(output.stdout.clone());
        let pane = receipt["worker"]["paneId"].as_str().unwrap_or_default();
        if !output.status.success() {
            let screen = (!pane.is_empty()).then(|| self.pane_text(pane));
            panic!(
                "worker-start failed ({}):\nstdout:\n{}\nstderr:\n{}\nscreen:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
                screen.unwrap_or_default()
            );
        }
        eprintln!("worker-start {kind}: {receipt}");
        for effect in receipt["effects"].as_array().into_iter().flatten() {
            if effect["kind"] == "worktree"
                && effect["action"] == "created"
                && let Some(id) = effect["id"].as_str()
            {
                self.worktrees.borrow_mut().push(PathBuf::from(id));
            }
        }
        self.started
            .borrow_mut()
            .push(string_at(&receipt, &["dispatchId"]));
        Worker {
            kind: kind.to_owned(),
            dispatch: string_at(&receipt, &["dispatchId"]),
            pane: pane.to_owned(),
            handle: string_at(&receipt, &["handle"]),
            receipt,
        }
    }

    pub fn pane_text(&self, pane: &str) -> String {
        let mut command = self.herdr_command();
        command.args([
            "pane",
            "read",
            pane,
            "--source",
            "recent-unwrapped",
            "--lines",
            "100",
        ]);
        command
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
            .unwrap_or_default()
    }

    /// Blocks in `check --wait` for one message type, up to five minutes.
    pub fn check_wait(&self, kind: &str, ack: Option<&str>) -> Value {
        self.check_wait_for(kind, ack, 300_000)
    }

    pub fn check_wait_for(&self, kind: &str, ack: Option<&str>, timeout_ms: u64) -> Value {
        let timeout = timeout_ms.to_string();
        let mut args = vec!["check", "--wait", "--types", kind, "--timeout-ms", &timeout];
        if let Some(ack) = ack {
            args.extend(["--ack", ack]);
        }
        self.pod(&args)
    }

    pub fn wait_until(&self, mut predicate: impl FnMut() -> bool, secs: u64, message: &str) {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            if predicate() {
                return;
            }
            thread::sleep(Duration::from_millis(500));
        }
        panic!("timed out after {secs}s waiting for {message}");
    }

    /// Whether a process whose command line matches `pattern` (a `pgrep -f`
    /// regular expression) is running anywhere on this machine.
    pub fn process_running(&self, pattern: &str) -> bool {
        Command::new("pgrep")
            .args(["-f", pattern])
            .output()
            .is_ok_and(|output| output.status.success())
    }

    pub fn worker_show(&self, dispatch: &str) -> Value {
        self.pod(&["worker-show", "--dispatch", dispatch])
    }

    pub fn attempts(&self, dispatch: &str) -> Vec<Value> {
        self.worker_show(dispatch)["wakeLadder"]["attempts"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// Where the harness keeps pod's store for this session.
    pub fn state_dir(&self) -> PathBuf {
        self.xdg_state.join("pod").join(&self.name)
    }

    fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.state_dir().join("pod.db")).expect("open pod database")
    }

    /// The wake attempts for a Dispatch's mailbox as `(outcome, detail,
    /// created_at)`, oldest first. The store keeps them after a consuming
    /// `check`, which `worker-show` does not.
    pub fn wake_attempts(&self, dispatch: &str) -> Vec<(String, String, String)> {
        let db = self.db();
        let mut statement = db
            .prepare(
                "SELECT outcome, COALESCE(detail, ''), created_at FROM wake_attempts
                 WHERE mailbox = ?1 ORDER BY created_at, rowid",
            )
            .expect("prepare wake attempts query");
        statement
            .query_map([format!("dispatch:{dispatch}")], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .expect("query wake attempts")
            .map(|row| row.expect("read wake attempt"))
            .collect()
    }

    pub fn unread_messages(&self, dispatch: &str) -> i64 {
        self.db()
            .query_row(
                "SELECT count(*) FROM messages WHERE mailbox = ?1 AND read = 0",
                [format!("dispatch:{dispatch}")],
                |row| row.get(0),
            )
            .expect("count unread messages")
    }

    /// herdr's own server log for this session: the only record that a
    /// notification was shown, since herdr lists none.
    pub fn herdr_log(&self) -> String {
        fs::read_to_string(
            self.xdg_config
                .join("herdr/sessions")
                .join(&self.name)
                .join("herdr-server.log"),
        )
        .unwrap_or_default()
    }

    /// pod's exported board for the Run.
    pub fn board(&self) -> Value {
        let path = self
            .state_dir()
            .join("runs")
            .join(&self.run_id)
            .join("run.json");
        json(fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display())))
    }

    pub fn temp_dir(&self) -> &Path {
        self.temp.path()
    }

    /// The project folder as the agents see it (macOS resolves `/tmp`).
    pub fn project_root(&self) -> PathBuf {
        fs::canonicalize(&self.project).unwrap_or_else(|_| self.project.clone())
    }

    pub fn claude_trusted(&self, folder: &Path) -> bool {
        self.trust.claude_has(folder)
    }

    pub fn codex_trusted(&self, folder: &Path) -> bool {
        self.trust.codex_has(folder)
    }

    fn start_results(&self, version: &str) {
        static FIRST: Once = Once::new();
        FIRST.call_once(|| {
            fs::create_dir_all(self.results.parent().expect("results has a parent"))
                .expect("create results directory");
            let header = format!(
                "# Live suite results\n\nherdr {version}; claude model `{}`; codex model `{}`.\n\n",
                Self::model("claude"),
                Self::model("codex")
            );
            fs::write(&self.results, header).expect("write results header");
        });
    }

    pub fn results_file(&self) -> &Path {
        &self.results
    }

    /// Runs one case for one agent kind. A failed assertion is recorded with
    /// its message and reported at the end by `finish`, so the other kind
    /// still runs. `run` returns the receipt worth keeping.
    pub fn case(&self, case: &str, kind: &str, command: &str, run: impl FnOnce() -> Value) {
        self.case_with(case, kind, command, true, run);
    }

    /// `clean` false keeps what the previous case left running, for a case
    /// that continues with the same worker.
    pub fn case_with(
        &self,
        case: &str,
        kind: &str,
        command: &str,
        clean: bool,
        run: impl FnOnce() -> Value,
    ) {
        if clean {
            self.clean_up();
        }
        self.soft.borrow_mut().clear();
        let outcome = catch_unwind(AssertUnwindSafe(run));
        let soft = std::mem::take(&mut *self.soft.borrow_mut());
        let (receipt, verdict) = match outcome {
            Ok(receipt) if soft.is_empty() => (receipt, "pass".to_owned()),
            Ok(receipt) => {
                self.failures.borrow_mut().push(format!("{case} {kind}"));
                (receipt, format!("FAIL: {}", soft.join("; ")))
            }
            Err(panic) => {
                let reason = panic
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
                    .unwrap_or_else(|| "panicked".into());
                self.failures.borrow_mut().push(format!("{case} {kind}"));
                (Value::Null, format!("FAIL: {reason}"))
            }
        };
        self.record(case, kind, command, &receipt, &verdict);
    }

    /// Stops and releases workers an earlier case left behind and consumes
    /// the coordinator mail they sent, so a failed case cannot wake the next
    /// one's `check --wait` at once.
    fn clean_up(&self) {
        for dispatch in std::mem::take(&mut *self.started.borrow_mut()) {
            self.pod_raw(&["worker-stop", "--dispatch", &dispatch]);
            self.pod_raw(&["worker-release", "--dispatch", &dispatch]);
        }
        for _ in 0..10 {
            let (_, mail) = self.pod_raw(&["check"]);
            let Some(delivery) = mail["delivery"]["id"].as_str() else {
                break;
            };
            self.pod_raw(&["check", "--ack", delivery]);
        }
    }

    /// A check whose failure fails the case without stopping it, so the rest
    /// of the case still shows what works.
    pub fn soft(&self, ok: bool, message: impl Into<String>) {
        if !ok {
            self.soft.borrow_mut().push(message.into());
        }
    }

    pub fn record(&self, case: &str, kind: &str, command: &str, receipt: &Value, verdict: &str) {
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&self.results)
            .expect("open results file");
        let receipt = serde_json::to_string(&trimmed(receipt)).unwrap_or_default();
        let _ = write!(
            file,
            "## {case}, {kind}\n\nCommand: `{command}`\n\nReceipt: `{receipt}`\n\nResult: {verdict}\n\n"
        );
    }

    /// Panics if any recorded case failed.
    pub fn finish(&self) {
        let failures = self.failures.borrow();
        assert!(
            failures.is_empty(),
            "live cases failed: {failures:?}; see {}",
            self.results.display()
        );
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop_server();
        let mut unlink = self.herdr_command();
        let _ = unlink.args(["plugin", "unlink", "pod"]).output();
        let mut delete = Command::new(&self.herdr);
        self.env(&mut delete);
        let _ = delete
            .args(["session", "delete", &self.name, "--json"])
            .output();
        for worktree in self.worktrees.borrow().iter() {
            // Only folders this run named; never anything else.
            if worktree.to_string_lossy().contains(&self.token) {
                let _ = fs::remove_dir_all(worktree);
                // herdr keeps each repository's worktrees in a folder named
                // after it, which carries the token too.
                if let Some(parent) = worktree.parent()
                    && parent.to_string_lossy().ends_with(&self.token)
                {
                    let _ = fs::remove_dir(parent);
                }
            }
        }
        self.trust.restore(&self.token);
    }
}

/// The agents' trust files as they were before the run. `--yolo` launches add
/// project entries (spec section 7.1); restoring means removing the entries
/// this run added for folders it created, not rewriting the whole file, since
/// Claude Code updates `~/.claude.json` constantly while a person works.
struct TrustGuard {
    claude: PathBuf,
    codex: PathBuf,
    claude_before: Vec<String>,
    codex_before: Vec<String>,
}

impl TrustGuard {
    fn capture() -> Self {
        let home = PathBuf::from(std::env::var("HOME").expect("HOME is set"));
        let codex_home = std::env::var("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home.join(".codex"));
        let mut guard = Self {
            claude: home.join(".claude.json"),
            codex: codex_home.join("config.toml"),
            claude_before: Vec::new(),
            codex_before: Vec::new(),
        };
        guard.claude_before = guard.claude_keys();
        guard.codex_before = guard.codex_keys();
        guard
    }

    fn claude_keys(&self) -> Vec<String> {
        fs::read_to_string(&self.claude)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|value| value["projects"].as_object().cloned())
            .map(|projects| projects.keys().cloned().collect())
            .unwrap_or_default()
    }

    fn codex_keys(&self) -> Vec<String> {
        fs::read_to_string(&self.codex)
            .ok()
            .and_then(|text| text.parse::<toml_edit::DocumentMut>().ok())
            .and_then(|doc| {
                doc.get("projects")
                    .and_then(|projects| projects.as_table())
                    .map(|table| table.iter().map(|(key, _)| key.to_owned()).collect())
            })
            .unwrap_or_default()
    }

    fn claude_has(&self, folder: &Path) -> bool {
        fs::read_to_string(&self.claude)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .is_some_and(|value| {
                value["projects"][folder.to_string_lossy().as_ref()]["hasTrustDialogAccepted"]
                    == true
            })
    }

    fn codex_has(&self, folder: &Path) -> bool {
        fs::read_to_string(&self.codex)
            .ok()
            .and_then(|text| text.parse::<toml_edit::DocumentMut>().ok())
            .is_some_and(|doc| {
                doc["projects"][folder.to_string_lossy().as_ref()]["trust_level"].as_str()
                    == Some("trusted")
            })
    }

    fn restore(&self, token: &str) {
        let added = |key: &String, before: &[String]| !before.contains(key) && key.contains(token);
        if let Ok(text) = fs::read_to_string(&self.claude)
            && let Ok(mut value) = serde_json::from_str::<Value>(&text)
            && let Some(projects) = value["projects"].as_object_mut()
        {
            let doomed: Vec<String> = projects
                .keys()
                .filter(|key| added(key, &self.claude_before))
                .cloned()
                .collect();
            if !doomed.is_empty() {
                for key in doomed {
                    projects.remove(&key);
                }
                let _ = write_atomically(&self.claude, &serde_json::to_vec_pretty(&value).unwrap());
            }
        }
        if let Ok(text) = fs::read_to_string(&self.codex)
            && let Ok(mut doc) = text.parse::<toml_edit::DocumentMut>()
            && let Some(projects) = doc
                .get_mut("projects")
                .and_then(|projects| projects.as_table_mut())
        {
            let doomed: Vec<String> = projects
                .iter()
                .map(|(key, _)| key.to_owned())
                .filter(|key| added(key, &self.codex_before))
                .collect();
            if !doomed.is_empty() {
                for key in doomed {
                    projects.remove(&key);
                }
                let _ = write_atomically(&self.codex, doc.to_string().as_bytes());
            }
        }
    }
}

/// A start receipt cut down to the fields that show how the start went; any
/// other value is kept whole, up to a size that keeps the results readable.
fn trimmed(receipt: &Value) -> Value {
    if receipt.get("launch").is_none() {
        let text = receipt.to_string();
        return if text.len() <= 1500 {
            receipt.clone()
        } else {
            Value::String(format!(
                "{}...",
                text.chars().take(1500).collect::<String>()
            ))
        };
    }
    let effects: Vec<String> = receipt["effects"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|effect| {
            let id = effect["id"]
                .as_str()
                .filter(|_| effect["kind"] == "prompt_error");
            format!(
                "{}:{}{}",
                effect["kind"].as_str().unwrap_or("?"),
                effect["action"].as_str().unwrap_or("?"),
                id.map(|id| format!(" ({id})")).unwrap_or_default()
            )
        })
        .collect();
    serde_json::json!({
        "state": receipt["state"],
        "stage": receipt["stage"],
        "handle": receipt["handle"],
        "outcome": receipt["outcome"],
        "inject": receipt["launch"]["inject"],
        "effective": receipt["launch"]["effective"],
        "setup": receipt["setup"]["source"],
        "effects": effects,
        "preambleInput": receipt["worker"]["preambleInput"],
        "trusted": receipt["worker"]["effects"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|effect| effect.as_str().is_some_and(|name| name.starts_with("folder_trusted:")))
            .collect::<Vec<_>>(),
    })
}

fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let temporary = path.with_extension("pod-live-tmp");
    fs::write(&temporary, bytes)?;
    if let Ok(metadata) = fs::metadata(path) {
        let _ = fs::set_permissions(&temporary, metadata.permissions());
    }
    fs::rename(&temporary, path)
}

fn results_path(repo: &Path) -> PathBuf {
    if let Ok(path) = std::env::var("POD_LIVE_RESULTS") {
        return PathBuf::from(path);
    }
    let date = Command::new("date")
        .arg("+%F")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|text| text.trim().to_owned())
        .unwrap_or_else(|| "undated".into());
    repo.join("docs/reviews/live").join(format!("{date}.md"))
}

fn git(dir: &Path, args: &[&str]) {
    let mut command = Command::new("git");
    command.current_dir(dir).args(args);
    successful(command.output().expect("run git"), "git");
}

pub fn successful(output: Output, context: &str) -> Vec<u8> {
    assert!(
        output.status.success(),
        "{context} failed ({}):\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

pub fn json(bytes: Vec<u8>) -> Value {
    serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        panic!(
            "invalid JSON ({error}): {}",
            String::from_utf8_lossy(&bytes)
        )
    })
}

pub fn string_at(value: &Value, path: &[&str]) -> String {
    let mut current = value;
    for part in path {
        current = if let Ok(index) = part.parse::<usize>() {
            &current[index]
        } else {
            &current[*part]
        };
    }
    current
        .as_str()
        .unwrap_or_else(|| panic!("expected string at {path:?} in {value}"))
        .to_owned()
}

fn which(program: impl AsRef<OsStr>) -> PathBuf {
    let program = program.as_ref();
    let output = Command::new("sh")
        .args(["-c", "command -v \"$1\"", "sh"])
        .arg(program)
        .output()
        .expect("look up executable");
    assert!(
        output.status.success(),
        "{} is not on PATH",
        program.to_string_lossy()
    );
    PathBuf::from(
        String::from_utf8(output.stdout)
            .expect("executable path is UTF-8")
            .trim(),
    )
}
