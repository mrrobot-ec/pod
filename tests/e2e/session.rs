use std::ffi::OsStr;
use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

pub fn run(name: &str) {
    assert_eq!(
        std::env::var("POD_E2E").as_deref(),
        Ok("1"),
        "run ignored e2e tests through scripts/e2e.sh"
    );
    match name {
        "e2e_run_create_worker_start_ask_reply_heartbeat_done_release" => happy_path(),
        "e2e_nudge_delivered_to_idle_fake_agent_and_consumed_by_check" => nudge(),
        "e2e_pane_closed_by_user_becomes_exited_and_release_leaves_it" => pane_closed(),
        "e2e_herdr_restart_reconciles_and_ladder_count_persists" => restart(),
        "e2e_two_workers_specialist_ask_first_reply_wins" => specialist(),
        "e2e_user_gate_wakes_check_wait_decision_gate" => user_gate(),
        "e2e_runs_on_linux" => linux(),
        other => panic!("unknown e2e scenario {other}"),
    }
}

struct Session {
    _temp: tempfile::TempDir,
    repo: PathBuf,
    pod: PathBuf,
    herdr: PathBuf,
    name: String,
    socket: PathBuf,
    state_root: PathBuf,
    config: PathBuf,
    xdg_config: PathBuf,
    xdg_state: PathBuf,
    home: PathBuf,
    trace: PathBuf,
    path: String,
    coordinator_pane: String,
    server: Option<Child>,
}

#[derive(Clone, Debug)]
struct Worker {
    dispatch: String,
    pane: String,
}

impl Session {
    fn new() -> Self {
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let built_pod = PathBuf::from(env!("CARGO_BIN_EXE_pod"));
        let herdr = which("herdr");
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
            "e2e suite requires herdr 0.9.3 or newer; got {version}"
        );

        // Unix-domain sockets have a small path limit (104 bytes on macOS),
        // so keep the isolated named-session root deliberately short.
        let temp = tempfile::Builder::new()
            .prefix("pod-e2e-")
            .tempdir_in("/tmp")
            .expect("create e2e temp directory");
        let xdg_config = temp.path().join("xdg-config");
        let xdg_state = temp.path().join("xdg-state");
        let home = temp.path().join("home");
        let state_root = temp.path().join("pod-state");
        let trace = temp.path().join("trace");
        let bin = temp.path().join("bin");
        for dir in [&xdg_config, &xdg_state, &home, &state_root, &trace, &bin] {
            fs::create_dir_all(dir).expect("create isolated e2e directory");
        }
        let config = temp.path().join("config.toml");
        let fake = repo.join("tests/e2e/fake_agent.sh");
        let pi = bin.join("pi");
        let pod = bin.join("pod");
        fs::copy(&fake, &pi).expect("install isolated pi shim");
        fs::copy(&built_pod, &pod).expect("install short-path pod binary");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&pi, fs::Permissions::from_mode(0o700))
                .expect("make isolated pi shim executable");
            fs::set_permissions(&pod, fs::Permissions::from_mode(0o700))
                .expect("make isolated pod binary executable");
        }
        fs::write(
            &config,
            format!("[agents.pi]\ncommand = \"{}\"\n", fake.display()),
        )
        .expect("write pod e2e config");

        let name = format!("pod-e2e-{}", std::process::id());
        let socket = xdg_config
            .join("herdr/sessions")
            .join(&name)
            .join("herdr.sock");
        let bin_dir = pod.parent().expect("pod binary has a parent");
        let path = format!(
            "{}:{}:{}:{}",
            bin.display(),
            bin_dir.display(),
            repo.join("tests/e2e").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut session = Self {
            _temp: temp,
            repo,
            pod,
            herdr,
            name,
            socket,
            state_root,
            config,
            xdg_config,
            xdg_state,
            home,
            trace,
            path,
            coordinator_pane: String::new(),
            server: None,
        };
        session.link_plugin();
        session.start_server();
        session.coordinator_pane = session.create_workspace();
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
        ] {
            command.env_remove(inherited);
        }
        command
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.xdg_config)
            .env("XDG_STATE_HOME", &self.xdg_state)
            .env(
                "HERDR_CONFIG_PATH",
                self.xdg_config.join("herdr/config.toml"),
            )
            .env("POD_STATE_DIR", &self.state_root)
            .env("POD_CONFIG", &self.config)
            .env("POD_E2E_POD", &self.pod)
            .env("POD_E2E_TRACE_DIR", &self.trace)
            .env("PATH", &self.path);
    }

    fn herdr_command(&self) -> Command {
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
        let deadline = Instant::now() + Duration::from_secs(10);
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

    /// Stops the isolated server. A failed stop panics only when `strict`;
    /// teardown in `Drop` must neither panic while unwinding nor wait forever
    /// on a server that ignored the stop, so it kills the child instead.
    fn stop_server(&mut self, strict: bool) {
        let mut command = Command::new(&self.herdr);
        self.env(&mut command);
        let output = command
            .args(["session", "stop", &self.name, "--json"])
            .output();
        if strict
            && let Ok(output) = &output
            && !output.status.success()
            && self.socket.exists()
        {
            successful(output.clone(), "session stop");
        }
        if let Some(mut server) = self.server.take() {
            let deadline = Instant::now() + Duration::from_secs(10);
            while server.try_wait().ok().flatten().is_none() {
                if Instant::now() >= deadline {
                    let _ = server.kill();
                    let _ = server.wait();
                    assert!(!strict, "isolated herdr server ignored session stop");
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    }

    fn restart_server(&mut self) {
        self.stop_server(true);
        self.start_server();
    }

    fn create_workspace(&self) -> String {
        let envs = [
            format!("POD_STATE_DIR={}", self.state_root.display()),
            format!("POD_CONFIG={}", self.config.display()),
            format!("POD_E2E_POD={}", self.pod.display()),
            format!("POD_E2E_TRACE_DIR={}", self.trace.display()),
            format!("PATH={}", self.path),
        ];
        let mut command = self.herdr_command();
        command
            .args(["workspace", "create", "--cwd"])
            .arg(&self.repo)
            .arg("--label")
            .arg("pod e2e")
            .arg("--no-focus");
        for value in envs {
            command.arg("--env").arg(value);
        }
        let value = json(successful(
            command.output().expect("create coordinator workspace"),
            "workspace create",
        ));
        string_at(&value, &["result", "root_pane", "pane_id"])
    }

    fn pod_command(&self, pane: &str) -> Command {
        let mut command = Command::new(&self.pod);
        self.env(&mut command);
        command
            .current_dir(&self.repo)
            .env("HERDR_ENV", "1")
            .env("HERDR_PANE_ID", pane)
            .env("HERDR_SOCKET_PATH", &self.socket);
        command
    }

    fn pod(&self, args: &[&str]) -> Value {
        let mut command = self.pod_command(&self.coordinator_pane);
        command.args(args).arg("--json");
        json(successful(
            command.output().expect("run pod"),
            &args.join(" "),
        ))
    }

    fn pod_failure(&self, pane: &str, args: &[&str]) -> Value {
        let mut command = self.pod_command(pane);
        command.args(args).arg("--json");
        let output = command.output().expect("run failing pod command");
        assert!(
            !output.status.success(),
            "command unexpectedly succeeded: {args:?}"
        );
        json_stderr(&output)
    }

    fn create_run(&self) -> String {
        string_at(
            &self.pod(&["run-create", "--objective", "pod e2e"]),
            &["run", "id"],
        )
    }

    fn start_worker(&self, mode: &str) -> Worker {
        let spec = format!("POD_E2E_MODE={mode}");
        let args = [
            "worker-start",
            "--spec",
            &spec,
            "--agent",
            "pi",
            "--worktree",
            "current",
            "--timeout-ms",
            "20000",
        ];
        let mut command = self.pod_command(&self.coordinator_pane);
        command.args(args).arg("--json");
        // pod itself waits for a new tab's shell prompt before agent_start,
        // so a start is not retried here.
        let output = command.output().expect("start worker");
        let value = json(output.stdout.clone());
        if !output.status.success() {
            let pane = value["worker"]["paneId"].as_str().unwrap_or_default();
            let expected_len = (|| {
                use std::str::FromStr;
                let run = self.pod(&["run-show", "--id", value["dispatch"]["runId"].as_str()?]);
                let context = pod::engine::preamble::PreambleContext {
                    pod: self.pod.display().to_string(),
                    coordinator_handle: run["run"]["coordinator_handle"].as_str()?.to_owned(),
                    task_id: pod::ids::TaskId::from_str(value["dispatch"]["task_id"].as_str()?)
                        .ok()?,
                    dispatch_id: pod::ids::DispatchId::from_str(value["dispatch"]["id"].as_str()?)
                        .ok()?,
                    worker_handle: value["dispatch"]["assigneeHandle"].as_str()?.to_owned(),
                    heartbeat_minutes: 5,
                    specialists: Vec::new(),
                    decision: None,
                    sub_dispatch: None,
                    spec: spec.clone(),
                    // Only the length in this failure message is approximate.
                    brief_path: std::path::PathBuf::new(),
                };
                Some(pod::engine::preamble::render_preamble(&context).len())
            })();
            let screen = (!pane.is_empty()).then(|| {
                let mut read = self.herdr_command();
                read.args([
                    "pane",
                    "read",
                    pane,
                    "--source",
                    "recent-unwrapped",
                    "--lines",
                    "200",
                ]);
                read.output()
                    .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
                    .unwrap_or_default()
            });
            panic!(
                "worker-start failed ({}, expected preamble bytes {expected_len:?}):\nstdout:\n{}\nstderr:\n{}\nscreen:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
                screen.unwrap_or_default()
            );
        }
        assert_eq!(value["worker"]["preambleInput"], "delivered");
        Worker {
            dispatch: string_at(&value, &["dispatch", "id"]),
            pane: string_at(&value, &["worker", "paneId"]),
        }
    }

    fn wait_worker_idle(&self, worker: &Worker) {
        self.wait_until(
            || {
                self.pane_text(&worker.pane)
                    .lines()
                    .rev()
                    .find(|line| !line.trim().is_empty())
                    .is_some_and(|line| line.trim() == ">")
            },
            "fake agent idle prompt",
        );
    }

    fn pane_text(&self, pane: &str) -> String {
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
        String::from_utf8(successful(
            command.output().expect("read fake agent pane"),
            "pane read",
        ))
        .expect("pane text is UTF-8")
    }

    fn check_wait(&self, kind: &str, ack: Option<&str>) -> Value {
        let mut args = vec!["check", "--wait", "--types", kind, "--timeout-ms", "15000"];
        if let Some(ack) = ack {
            args.extend(["--ack", ack]);
        }
        self.pod(&args)
    }

    fn herdr(&self, args: &[&str]) -> Value {
        let mut command = self.herdr_command();
        command.args(args);
        json(successful(
            command.output().expect("run herdr"),
            &args.join(" "),
        ))
    }

    fn wait_until(&self, mut predicate: impl FnMut() -> bool, message: &str) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if predicate() {
                return;
            }
            thread::sleep(Duration::from_millis(100));
        }
        panic!("timed out waiting for {message}");
    }

    fn trace(&self, worker: &Worker, kind: &str) -> Value {
        let path = self.trace.join(format!("{}.{kind}.json", worker.dispatch));
        json(fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display())))
    }

    fn ladder_attempts(&self, worker: &Worker) -> Vec<Value> {
        self.pod(&["worker-show", "--dispatch", &worker.dispatch])["wakeLadder"]["attempts"]
            .as_array()
            .cloned()
            .expect("worker-show reports the wake ladder")
    }

    fn delivered_wake_count(&self, mailbox: &str) -> i64 {
        let db = self.state_root.join(&self.name).join("pod.db");
        let connection = rusqlite::Connection::open(db).expect("open pod e2e database");
        connection
            .query_row(
                "SELECT count(*) FROM wake_attempts WHERE mailbox = ?1 AND outcome = 'delivered'",
                [mailbox],
                |row| row.get(0),
            )
            .expect("count delivered wake attempts")
    }

    fn wake_count(&self, mailbox: &str) -> i64 {
        let db = self.state_root.join(&self.name).join("pod.db");
        let connection = rusqlite::Connection::open(db).expect("open pod e2e database");
        connection
            .query_row(
                "SELECT count(*) FROM wake_attempts WHERE mailbox = ?1",
                [mailbox],
                |row| row.get(0),
            )
            .expect("count wake attempts")
    }

    fn read_message_count(&self, mailbox: &str) -> i64 {
        let db = self.state_root.join(&self.name).join("pod.db");
        let connection = rusqlite::Connection::open(db).expect("open pod e2e database");
        connection
            .query_row(
                "SELECT count(*) FROM messages WHERE mailbox = ?1 AND read = 1",
                [mailbox],
                |row| row.get(0),
            )
            .expect("count consumed messages")
    }

    fn observation_count(&self, dispatch: &str) -> i64 {
        let db = self.state_root.join(&self.name).join("pod.db");
        let connection = rusqlite::Connection::open(db).expect("open pod e2e database");
        connection
            .query_row(
                "SELECT count(*) FROM observations WHERE dispatch_id = ?1",
                [dispatch],
                |row| row.get(0),
            )
            .expect("count observations")
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop_server(false);
        let mut unlink = self.herdr_command();
        let _ = unlink.args(["plugin", "unlink", "pod"]).output();
        let mut delete = Command::new(&self.herdr);
        self.env(&mut delete);
        let _ = delete
            .args(["session", "delete", &self.name, "--json"])
            .output();
    }
}

fn happy_path() {
    let session = Session::new();
    let run = session.create_run();
    let worker = session.start_worker("happy");
    let question = session.check_wait("question", None);
    let message = string_at(&question, &["messages", "0", "id"]);
    let delivery = string_at(&question, &["delivery", "id"]);
    session.pod(&["reply", "--id", &message, "--body", "approved"]);
    let done = session.check_wait("worker_done", Some(&delivery));
    let worker_done = done["messages"]
        .as_array()
        .and_then(|messages| {
            messages
                .iter()
                .find(|message| message["type"] == "worker_done")
        })
        .expect("worker_done is present in the coordinator delivery");
    assert_eq!(worker_done["payload"]["outcome"], "succeeded");
    let answer = session.trace(&worker, "ask");
    assert_eq!(answer["question"]["status"], "answered", "{answer}");
    assert_eq!(answer["question"]["answer_body"], "approved", "{answer}");
    let show = session.pod(&["worker-show", "--dispatch", &worker.dispatch]);
    assert!(
        !show["dispatch"]["lastHeartbeatAt"].is_null(),
        "heartbeat was not recorded: {show}"
    );
    let release = session.pod(&["worker-release", "--dispatch", &worker.dispatch]);
    assert_eq!(release["worker"]["terminalState"], "released");
    let list = session.pod(&["worker-list", "--terminal-state", "reclaimable"]);
    assert_eq!(list["workers"].as_array().map(Vec::len), Some(0));
    let show = session.pod(&["worker-show", "--dispatch", &worker.dispatch]);
    assert_eq!(show["worker"]["terminalState"], "released", "{show}");

    // The sequence scripts/pilot.sh tells a person to look for.
    let log = session.pod(&["log", "--run", &run]);
    let types = log["events"]
        .as_array()
        .expect("log lists events")
        .iter()
        .filter_map(|event| event["type"].as_str())
        .collect::<Vec<_>>();
    let mut expected = [
        "run.created",
        "task.created",
        "dispatch.dispatched",
        "dispatch.completed",
    ]
    .into_iter()
    .peekable();
    for kind in &types {
        expected.next_if_eq(kind);
    }
    assert!(expected.peek().is_none(), "unexpected log order: {types:?}");
    let board = session.pod(&["board", "--run", &run]);
    let board_file = PathBuf::from(string_at(&board, &["boardFile"]));
    let exported = json(fs::read(&board_file).expect("read exported run.json"));
    assert!(exported.is_object(), "{exported}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: &std::path::Path| {
            fs::metadata(path)
                .unwrap_or_else(|error| panic!("stat {}: {error}", path.display()))
                .permissions()
                .mode()
                & 0o777
        };
        let state = session.state_root.join(&session.name);
        assert_eq!(mode(&state), 0o700, "{}", state.display());
        assert_eq!(mode(&state.join("pod.db")), 0o600);
        assert_eq!(mode(&board_file), 0o600, "{}", board_file.display());
    }

    // Spec 12: with a dagr on PATH, `pod board` opens it in a new herdr tab
    // whose shell has DAGR_RUN set. The stand-in dagr records what it got.
    let opened = session.trace.join("dagr.run");
    let dagr = session.pod.with_file_name("dagr");
    fs::write(
        &dagr,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$DAGR_RUN\" > '{}'\n",
            opened.display()
        ),
    )
    .expect("write stand-in dagr");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dagr, fs::Permissions::from_mode(0o700))
            .expect("make stand-in dagr executable");
    }
    let board = session.pod(&["board", "--run", &run]);
    assert_eq!(board["opened"], true, "{board}");
    session.wait_until(
        || {
            fs::read_to_string(&opened)
                .is_ok_and(|text| text.trim() == board_file.display().to_string())
        },
        "dagr started with DAGR_RUN",
    );
}

fn nudge() {
    let session = Session::new();
    session.create_run();
    let worker = session.start_worker("nudge");
    session.wait_worker_idle(&worker);
    // The idle hook recorded the pane's screen; mail that arrives once the
    // 3-second quiet window has passed is typed on the first attempt
    // without the sender waiting (commands-messages.md).
    thread::sleep(Duration::from_millis(3_500));
    session.pod(&[
        "send",
        "--to",
        &format!("dispatch:{}", worker.dispatch),
        "--type",
        "status",
        "--subject",
        "e2e nudge",
        "--body",
        "consume this",
    ]);
    let done = session.check_wait("worker_done", None);
    if !done["messages"].as_array().is_some_and(|messages| {
        messages.iter().any(|message| {
            message["type"] == "worker_done" && message["payload"]["outcome"] == "succeeded"
        })
    }) {
        let mut state = Vec::new();
        if let Ok(entries) = fs::read_dir(session.state_root.join(&session.name)) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let content = if name.ends_with(".quiet") {
                    fs::read_to_string(entry.path()).unwrap_or_default()
                } else {
                    String::new()
                };
                state.push(format!("{name} {}", content.trim()));
            }
        }
        let db = session.state_root.join(&session.name).join("pod.db");
        let connection = rusqlite::Connection::open(db).expect("open pod e2e database");
        let mut statement = connection
            .prepare("SELECT observed_at, herdr_status, activity, presence, evidence_source FROM observations ORDER BY rowid")
            .unwrap();
        let observations = statement
            .query_map([], |row| {
                Ok(format!(
                    "{} {:?} {} {:?} {}",
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?
                ))
            })
            .unwrap()
            .collect::<Vec<_>>();
        panic!(
            "missing worker_done: {done}; ladder: {:?}; state files: {state:?}; observations: {observations:?}",
            session.ladder_attempts(&worker)
        );
    }
    let mailbox = format!("dispatch:{}", worker.dispatch);
    assert!(session.delivered_wake_count(&mailbox) >= 1);
    assert_eq!(session.read_message_count(&mailbox), 1);
    let attempts = session.ladder_attempts(&worker);
    assert!(
        attempts.is_empty(),
        "consuming check left the ladder: {attempts:?}"
    );
    session.pod(&["worker-release", "--dispatch", &worker.dispatch]);
}

fn pane_closed() {
    let session = Session::new();
    session.create_run();
    let worker = session.start_worker("hold");
    session.wait_worker_idle(&worker);
    session.herdr(&["pane", "close", &worker.pane]);
    session.wait_until(
        || {
            let show = session.pod(&["worker-show", "--dispatch", &worker.dispatch]);
            show["projection"]["liveness"] == "exited"
                && show["projection"]["outcome"] == "outcome_unknown"
        },
        "pane close reconciliation",
    );
    let untouched = session.pod(&["worker-release", "--dispatch", &worker.dispatch]);
    assert_eq!(untouched["outcome"], "active");
    assert!(
        untouched["leftAlone"]
            .as_array()
            .is_some_and(|v| !v.is_empty())
    );
    session.pod(&["worker-abandon", "--dispatch", &worker.dispatch]);
    let released = session.pod(&["worker-release", "--dispatch", &worker.dispatch]);
    assert_eq!(released["closedTabs"].as_array().map(Vec::len), Some(0));
    let list = session.pod(&["worker-list", "--terminal-state", "reclaimable"]);
    assert_eq!(list["workers"].as_array().map(Vec::len), Some(0), "{list}");
}

fn restart() {
    let mut session = Session::new();
    session.create_run();
    let worker = session.start_worker("hold");
    session.wait_worker_idle(&worker);
    let mailbox = format!("dispatch:{}", worker.dispatch);
    session.pod(&[
        "send",
        "--to",
        &mailbox,
        "--type",
        "status",
        "--subject",
        "persist ladder",
    ]);
    let before = session.wake_count(&mailbox);
    assert!(before >= 1, "the initial commit should start the ladder");
    let ladder = session.ladder_attempts(&worker);
    assert_eq!(ladder.len() as i64, before);
    let observations = session.observation_count(&worker.dispatch);
    session.restart_server();
    session.wait_until(
        || session.observation_count(&worker.dispatch) > observations,
        "startup reconciliation after herdr restart",
    );
    assert_eq!(session.wake_count(&mailbox), before);
    assert_eq!(session.ladder_attempts(&worker), ladder);
}

fn specialist() {
    let session = Session::new();
    session.create_run();
    let specialist = session.start_worker("hold");
    session.wait_worker_idle(&specialist);
    let asker = session.start_worker("specialist");
    let question = session.check_wait("question", None);
    let message = string_at(&question, &["messages", "0", "id"]);
    let delivery = string_at(&question, &["delivery", "id"]);
    assert_eq!(
        question["messages"][0]["payload"]["routed_to"],
        format!("dispatch:{}", specialist.dispatch)
    );
    session.pod(&["reply", "--id", &message, "--body", "coordinator wins"]);
    let error = session.pod_failure(
        &specialist.pane,
        &["reply", "--id", &message, "--body", "specialist loses"],
    );
    assert_eq!(error["error"]["code"], "question_answered");
    session.check_wait("worker_done", Some(&delivery));
    let answer = session.trace(&asker, "ask");
    assert_eq!(
        answer["question"]["answer_body"], "coordinator wins",
        "{answer}"
    );
    session.pod(&["worker-release", "--dispatch", &asker.dispatch]);
    session.pod(&["worker-stop", "--dispatch", &specialist.dispatch]);
    session.pod(&["worker-release", "--dispatch", &specialist.dispatch]);
}

fn user_gate() {
    let session = Session::new();
    session.create_run();
    let task = string_at(
        &session.pod(&["task-create", "--spec", "user decision e2e"]),
        &["task", "id"],
    );
    let gate = string_at(
        &session.pod(&[
            "gate-create",
            "--for",
            "user",
            "--task",
            &task,
            "--question",
            "ship it?",
            "--options",
            "[\"yes\",\"no\"]",
            "--evidence",
            "e2e",
            "--recommendation",
            "yes",
        ]),
        &["gate", "id"],
    );
    let mut wait = session.pod_command(&session.coordinator_pane);
    wait.args([
        "check",
        "--wait",
        "--types",
        "decision_gate",
        "--timeout-ms",
        "10000",
        "--json",
    ]);
    let waiter = thread::spawn(move || wait.output().expect("wait for decision gate"));
    // An unresolved gate stores no message, so the wait must still block.
    thread::sleep(Duration::from_millis(500));
    assert!(
        !waiter.is_finished(),
        "check --wait returned before gate-resolve"
    );
    let resolved = Instant::now();
    session.pod(&["gate-resolve", "--id", &gate, "--resolution", "yes"]);
    let receipt = json(successful(
        waiter.join().expect("join check waiter"),
        "check --wait decision_gate",
    ));
    // The 1-second fallback poll bounds a missed file-watch wake.
    assert!(
        resolved.elapsed() < Duration::from_secs(3),
        "decision gate wake took {:?}",
        resolved.elapsed()
    );
    assert_eq!(receipt["messages"][0]["type"], "decision_gate");
    assert_eq!(receipt["messages"][0]["payload"]["resolution"], "yes");
    assert_eq!(receipt["timedOut"], false);
}

fn linux() {
    if !cfg!(target_os = "linux") {
        return;
    }
    let session = Session::new();
    let status = session.pod(&["status"]);
    assert_eq!(status["herdr"]["reachable"], true);
}

fn successful(output: Output, context: &str) -> Vec<u8> {
    assert!(
        output.status.success(),
        "{context} failed ({}):\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn json(bytes: Vec<u8>) -> Value {
    serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        panic!(
            "invalid JSON ({error}): {}",
            String::from_utf8_lossy(&bytes)
        )
    })
}

fn json_stderr(output: &Output) -> Value {
    serde_json::from_slice(&output.stderr).unwrap_or_else(|error| {
        panic!(
            "invalid error JSON ({error}): {}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn string_at(value: &Value, path: &[&str]) -> String {
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
    let output = Command::new("sh")
        .args(["-c", "command -v \"$1\"", "sh"])
        .arg(program)
        .output()
        .expect("look up executable");
    PathBuf::from(
        String::from_utf8(successful(output, "command -v herdr"))
            .expect("executable path is UTF-8")
            .trim(),
    )
}
