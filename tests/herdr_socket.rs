use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use chrono::Utc;
use pod::herdr::socket::SocketHerdr;
use pod::herdr::{AgentInfo, AgentStatus, Herdr, HerdrEvent, PromptOutcome};
use pod::{ErrorCode, FakeHerdr};
use serde_json::{Value, json};

struct MockServer {
    _dir: tempfile::TempDir,
    path: PathBuf,
    requests: Receiver<String>,
    thread: Option<JoinHandle<()>>,
}

fn write_frame(stream: &mut UnixStream, value: &Value) {
    serde_json::to_writer(&mut *stream, value).expect("write mock JSON");
    stream.write_all(b"\n").expect("finish mock frame");
}

impl MockServer {
    fn one(response: impl FnOnce(&Value) -> Value + Send + 'static) -> Self {
        Self::session(move |request, stream| {
            let reply = response(request);
            write_frame(stream, &reply);
        })
    }

    /// Serves the first non-empty request; `serve` writes whatever frames it
    /// wants to the open connection (a reply, then events for a subscription).
    fn session(serve: impl FnOnce(&Value, &mut UnixStream) + Send + 'static) -> Self {
        let dir = tempfile::tempdir().expect("create mock socket directory");
        let path = dir.path().join("herdr.sock");
        let listener = UnixListener::bind(&path).expect("bind mock Unix socket");
        let (sender, requests) = mpsc::channel();
        let thread = thread::spawn(move || {
            loop {
                let (stream, _) = listener.accept().expect("accept mock client");
                let mut reader = BufReader::new(stream);
                let mut request = String::new();
                reader.read_line(&mut request).expect("read request frame");
                if request.is_empty() {
                    continue;
                }
                let value: Value = serde_json::from_str(&request).expect("valid request JSON");
                sender.send(request).expect("send captured request");
                serve(&value, reader.get_mut());
                break;
            }
        });
        Self {
            _dir: dir,
            path,
            requests,
            thread: Some(thread),
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn request(&self) -> String {
        self.requests
            .recv_timeout(Duration::from_secs(2))
            .expect("receive captured request")
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            thread.join().expect("join mock server");
        }
    }
}

fn agent(name: &str, status: AgentStatus) -> AgentInfo {
    AgentInfo {
        name: name.into(),
        kind: "codex".into(),
        pane_id: "w1:p1".into(),
        pid: 42,
        status,
        session_id: Some("session-1".into()),
        session_path: None,
        needs_input: Vec::new(),
        observed_at: Utc::now(),
    }
}

#[test]
fn fake_records_calls_and_scripts_agent_status() {
    let fake = FakeHerdr::new();
    fake.set_agent("worker", agent("worker", AgentStatus::Idle));
    fake.script_agent_status("worker", AgentStatus::Working);
    fake.script_agent_status("worker", AgentStatus::Done);

    assert_eq!(
        fake.agent_get("worker")
            .expect("first scripted response")
            .expect("agent exists")
            .status,
        AgentStatus::Working
    );
    assert_eq!(
        fake.agent_get("worker")
            .expect("second scripted response")
            .expect("agent exists")
            .status,
        AgentStatus::Done
    );
    assert_eq!(fake.calls(), vec!["agent_get:worker", "agent_get:worker"]);
}

#[test]
fn fake_send_text_appends_to_screen_and_enter_submits() {
    let fake = FakeHerdr::new();
    fake.set_screen("w1:p1", "existing ");

    fake.pane_send_text("w1:p1", "prompt").expect("paste text");
    assert_eq!(fake.screen("w1:p1"), "existing prompt");
    assert!(fake.submissions().is_empty());

    fake.pane_send_keys("w1:p1", &["Enter"])
        .expect("submit text");
    assert_eq!(fake.screen("w1:p1"), "existing prompt\n");
    assert_eq!(
        fake.submissions(),
        vec![("w1:p1".to_owned(), "existing prompt".to_owned())]
    );
}

#[test]
fn fake_events_delivered_in_order_and_timeout_returns_none() {
    let fake = FakeHerdr::new();
    fake.push_event(HerdrEvent::PaneExited {
        pane_id: "w1:p1".into(),
    });
    fake.push_event(HerdrEvent::PaneClosed {
        pane_id: "w1:p2".into(),
    });
    let mut stream = fake
        .events_subscribe(&["w1:p1".into(), "w1:p2".into()])
        .expect("subscribe");

    assert_eq!(
        stream.next(Duration::from_millis(1)).expect("first event"),
        Some(HerdrEvent::PaneExited {
            pane_id: "w1:p1".into()
        })
    );
    assert_eq!(
        stream.next(Duration::from_millis(1)).expect("second event"),
        Some(HerdrEvent::PaneClosed {
            pane_id: "w1:p2".into()
        })
    );
    assert_eq!(
        stream.next(Duration::from_millis(1)).expect("timeout"),
        None
    );
}

#[test]
fn socket_framing_roundtrip() {
    let server = MockServer::one(|request| {
        json!({
            "id": request["id"],
            "result": {"type": "pong", "version": "0.9.3", "protocol": 22}
        })
    });
    let herdr = SocketHerdr::connect(server.path()).expect("connect socket client");

    assert_eq!(
        herdr.ping().expect("ping response"),
        pod::herdr::HerdrInfo {
            version: "0.9.3".into(),
            protocol: 22
        }
    );
    let frame = server.request();
    assert!(frame.ends_with('\n'));
    let request: Value = serde_json::from_str(&frame).expect("request JSON");
    assert_eq!(request["method"], "ping");
    assert_eq!(request["params"], json!({}));
    assert!(request["id"].as_str().is_some_and(|id| !id.is_empty()));
}

#[test]
fn socket_error_maps_to_herdr_error_with_next_steps() {
    let server = MockServer::one(|request| {
        json!({
            "id": request["id"],
            "error": {"code": "bad_request", "message": "mock failure"}
        })
    });
    let herdr = SocketHerdr::connect(server.path()).expect("connect socket client");

    let error = herdr.ping().expect_err("herdr error response");
    assert_eq!(error.code, ErrorCode::HerdrError);
    assert_eq!(error.message, "mock failure");
    assert_eq!(error.next_steps, vec!["herdr status server"]);
}

#[test]
fn socket_pane_get_not_found_returns_none() {
    let server = MockServer::one(|request| {
        json!({
            "id": request["id"],
            "error": {"code": "pane_not_found", "message": "pane not found"}
        })
    });
    let herdr = SocketHerdr::connect(server.path()).expect("connect socket client");
    assert_eq!(herdr.pane_get("missing").expect("not-found result"), None);
}

#[test]
fn socket_workspace_get_not_found_returns_none() {
    let server = MockServer::one(|request| {
        json!({
            "id": request["id"],
            "error": {"code": "workspace_not_found", "message": "workspace not found"}
        })
    });
    let herdr = SocketHerdr::connect(server.path()).expect("connect socket client");
    assert_eq!(
        herdr.workspace_get("missing").expect("not-found result"),
        None
    );
}

#[test]
fn socket_agent_get_not_found_returns_none() {
    let server = MockServer::one(|request| {
        json!({
            "id": request["id"],
            "error": {"code": "agent_not_found", "message": "agent not found"}
        })
    });
    let herdr = SocketHerdr::connect(server.path()).expect("connect socket client");
    assert_eq!(herdr.agent_get("missing").expect("not-found result"), None);
}

#[test]
fn socket_unreachable_maps_to_herdr_unreachable() {
    let dir = tempfile::tempdir().expect("create temp directory");
    let error = SocketHerdr::connect(&dir.path().join("missing.sock"))
        .err()
        .expect("missing socket must fail");
    assert_eq!(error.code, ErrorCode::HerdrUnreachable);
    assert_eq!(error.next_steps, vec!["herdr status server"]);
}

#[test]
fn socket_prompt_stalled_and_blocked_are_typed_outcomes() {
    for (code, expected) in [
        ("agent_prompt_stalled", PromptOutcome::Stalled),
        ("agent_blocked", PromptOutcome::Blocked),
    ] {
        let server = MockServer::one(move |request| {
            json!({
                "id": request["id"],
                "error": {"code": code, "message": code}
            })
        });
        let herdr = SocketHerdr::connect(server.path()).expect("connect socket client");
        assert_eq!(
            herdr
                .agent_prompt("worker", "hello", Some(50))
                .expect("typed prompt outcome"),
            expected
        );
    }
}

#[test]
#[ignore = "requires POD_E2E=1 and HERDR_SOCKET_PATH"]
fn live_ping_and_pane_current() {
    if std::env::var("POD_E2E").as_deref() != Ok("1") {
        return;
    }
    let socket = std::env::var_os("HERDR_SOCKET_PATH").expect("HERDR_SOCKET_PATH is set");
    let herdr = SocketHerdr::connect(Path::new(&socket)).expect("connect live herdr");
    let ping = herdr.ping().expect("live ping");
    assert_eq!(ping.protocol, 22);
    assert!(
        !herdr
            .pane_current()
            .expect("live pane.current")
            .pane_id
            .is_empty()
    );
}

#[test]
#[ignore = "requires POD_E2E=1, HERDR_SOCKET_PATH, and POD_E2E_AGENT"]
fn live_agent_prompt_stalled_is_reported() {
    if std::env::var("POD_E2E").as_deref() != Ok("1") {
        return;
    }
    let socket = std::env::var_os("HERDR_SOCKET_PATH").expect("HERDR_SOCKET_PATH is set");
    let target = std::env::var("POD_E2E_AGENT").expect("POD_E2E_AGENT is set");
    let herdr = SocketHerdr::connect(Path::new(&socket)).expect("connect live herdr");
    assert_eq!(
        herdr
            .agent_prompt(&target, "pod e2e prompt", Some(10_000))
            .expect("live prompt outcome"),
        PromptOutcome::Stalled
    );
}

#[test]
fn fake_events_are_filtered_to_subscribed_panes() {
    let fake = FakeHerdr::new();
    fake.push_event(HerdrEvent::PaneExited {
        pane_id: "w1:p9".into(),
    });
    fake.push_event(HerdrEvent::PaneClosed {
        pane_id: "w1:p1".into(),
    });
    let mut stream = fake.events_subscribe(&["w1:p1".into()]).expect("subscribe");
    assert_eq!(
        stream
            .next(Duration::from_millis(1))
            .expect("subscribed event"),
        Some(HerdrEvent::PaneClosed {
            pane_id: "w1:p1".into()
        })
    );
    assert_eq!(
        stream.next(Duration::from_millis(1)).expect("timeout"),
        None
    );
}

/// Frames as herdr 0.9.3 sends them: global events use `EventKind` names
/// (`pane_exited`), pane-scoped subscriptions use `pane.agent_status_changed`.
#[test]
fn socket_events_use_observed_wire_names_and_filter_panes() {
    let server = MockServer::session(|request, stream| {
        write_frame(
            stream,
            &json!({"id": request["id"], "result": {"type": "subscription_started"}}),
        );
        for frame in [
            json!({"event": "pane_exited", "data": {"type": "pane_exited", "pane_id": "w1:p9", "workspace_id": "w1"}}),
            json!({"event": "pane_exited", "data": {"type": "pane_exited", "pane_id": "w1:p1", "workspace_id": "w1"}}),
            json!({"event": "pane_closed", "data": {"type": "pane_closed", "pane_id": "w1:p1", "workspace_id": "w1"}}),
            json!({"event": "pane.agent_status_changed", "data": {
                "pane_id": "w1:p1", "workspace_id": "w1", "agent_status": "blocked",
                "state_labels": {"approval": "permission prompt"}
            }}),
        ] {
            write_frame(stream, &frame);
        }
    });
    let herdr = SocketHerdr::connect(server.path()).expect("connect socket client");
    let mut stream = herdr
        .events_subscribe(&["w1:p1".into()])
        .expect("subscribe");

    let request: Value = serde_json::from_str(&server.request()).expect("request JSON");
    assert_eq!(request["method"], "events.subscribe");
    assert_eq!(
        request["params"]["subscriptions"],
        json!([
            {"type": "pane.exited"},
            {"type": "pane.closed"},
            {"type": "pane.agent_status_changed", "pane_id": "w1:p1"}
        ])
    );
    let wait = Duration::from_secs(2);
    assert_eq!(
        stream.next(wait).expect("exit event"),
        Some(HerdrEvent::PaneExited {
            pane_id: "w1:p1".into()
        })
    );
    assert_eq!(
        stream.next(wait).expect("close event"),
        Some(HerdrEvent::PaneClosed {
            pane_id: "w1:p1".into()
        })
    );
    assert_eq!(
        stream.next(wait).expect("status event"),
        Some(HerdrEvent::AgentStatusChanged {
            pane_id: "w1:p1".into(),
            status: AgentStatus::Blocked,
            needs_input: vec!["permission prompt".into()],
        })
    );
}

#[test]
fn socket_event_frame_split_across_timeout_is_kept() {
    let (release, released) = mpsc::channel::<()>();
    let server = MockServer::session(move |request, stream| {
        write_frame(
            stream,
            &json!({"id": request["id"], "result": {"type": "subscription_started"}}),
        );
        let frame = br#"{"event":"pane_exited","data":{"type":"pane_exited","pane_id":"w1:p1","workspace_id":"w1"}}"#;
        let (head, tail) = frame.split_at(20);
        stream.write_all(head).expect("write first half");
        stream.flush().expect("flush first half");
        released
            .recv_timeout(Duration::from_secs(5))
            .expect("client timed out once");
        stream.write_all(tail).expect("write second half");
        stream.write_all(b"\n").expect("finish frame");
    });
    let herdr = SocketHerdr::connect(server.path()).expect("connect socket client");
    let mut stream = herdr
        .events_subscribe(&["w1:p1".into()])
        .expect("subscribe");

    assert_eq!(
        stream
            .next(Duration::from_millis(100))
            .expect("partial frame times out"),
        None
    );
    release.send(()).expect("release second half");
    assert_eq!(
        stream
            .next(Duration::from_secs(2))
            .expect("completed frame"),
        Some(HerdrEvent::PaneExited {
            pane_id: "w1:p1".into()
        })
    );
}

/// herdr 0.9.3 lists foreground processes children-first; the agent is the
/// process-group leader.
#[test]
fn socket_process_info_prefers_process_group_leader() {
    let server = MockServer::one(|request| {
        json!({
            "id": request["id"],
            "result": {"type": "pane_process_info", "process_info": {
                "pane_id": "w1:p1",
                "shell_pid": 100,
                "foreground_process_group_id": 200,
                "foreground_processes": [
                    {"pid": 202, "name": "cat"},
                    {"pid": 201, "name": "sleep"},
                    {"pid": 200, "name": "bash"}
                ]
            }}
        })
    });
    let herdr = SocketHerdr::connect(server.path()).expect("connect socket client");
    let info = herdr.pane_process_info("w1:p1").expect("process info");
    assert_eq!(info.shell_pid, 100);
    assert_eq!(info.foreground_pid, 200);
    assert_eq!(info.pids, vec![100, 202, 201, 200]);
}

#[test]
fn socket_prompt_wait_returns_on_observed_turn_and_timeout_is_stalled() {
    let server = MockServer::one(|request| {
        json!({
            "id": request["id"],
            "error": {"code": "timeout", "message": "timed out waiting for agent status"}
        })
    });
    let herdr = SocketHerdr::connect(server.path()).expect("connect socket client");
    assert_eq!(
        herdr
            .agent_prompt("worker", "hello", Some(8000))
            .expect("typed prompt outcome"),
        PromptOutcome::Stalled
    );
    let request: Value = serde_json::from_str(&server.request()).expect("request JSON");
    assert_eq!(request["method"], "agent.prompt");
    assert_eq!(
        request["params"],
        json!({
            "target": "worker",
            "text": "hello",
            "wait": {"timeout_ms": 8000, "until": ["working", "blocked"]}
        })
    );
}

#[test]
fn socket_prompt_without_wait_timeout_error_is_herdr_error() {
    let server = MockServer::one(|request| {
        json!({
            "id": request["id"],
            "error": {"code": "timeout", "message": "timed out"}
        })
    });
    let herdr = SocketHerdr::connect(server.path()).expect("connect socket client");
    let error = herdr
        .agent_prompt("worker", "hello", None)
        .expect_err("timeout without a wait is not a prompt outcome");
    assert_eq!(error.code, ErrorCode::HerdrError);
}

#[test]
fn socket_tab_pane_count_reads_tab_get_and_missing_tab_is_none() {
    let server = MockServer::one(|request| {
        json!({
            "id": request["id"],
            "result": {"type": "tab_info", "tab": {
                "tab_id": "t_7", "workspace_id": "w_1", "number": 2, "label": "terra-k3x9",
                "focused": false, "pane_count": 2, "agent_status": "idle"
            }}
        })
    });
    let herdr = SocketHerdr::connect(server.path()).expect("connect socket client");
    assert_eq!(herdr.tab_pane_count("t_7").expect("tab.get"), Some(2));
    let request: Value = serde_json::from_str(&server.request()).expect("request JSON");
    assert_eq!(request["method"], "tab.get");
    assert_eq!(request["params"], json!({"tab_id": "t_7"}));

    let server = MockServer::one(|request| {
        json!({
            "id": request["id"],
            "error": {"code": "tab_not_found", "message": "tab not found"}
        })
    });
    let herdr = SocketHerdr::connect(server.path()).expect("connect socket client");
    assert_eq!(
        herdr.tab_pane_count("gone").expect("not-found result"),
        None
    );
}

#[test]
fn socket_tab_create_sends_env_and_metadata_clears_with_null() {
    let server = MockServer::one(|request| {
        json!({
            "id": request["id"],
            "result": {"type": "tab_created",
                "tab": {"tab_id": "t_9", "workspace_id": "w_1", "number": 3, "label": "board",
                        "focused": true, "pane_count": 1, "agent_status": "unknown"},
                "root_pane": {"pane_id": "p_9", "terminal_id": "term_9", "workspace_id": "w_1",
                              "tab_id": "t_9", "focused": true, "agent_status": "unknown", "revision": 0}}
        })
    });
    let herdr = SocketHerdr::connect(server.path()).expect("connect socket client");
    let tab = herdr
        .tab_create(
            "w_1",
            "board",
            Path::new("/tmp"),
            true,
            &[("DAGR_RUN", "/state/runs/run_1/run.json")],
        )
        .expect("tab.create");
    assert_eq!(tab.pane_id, "p_9");
    let request: Value = serde_json::from_str(&server.request()).expect("request JSON");
    assert_eq!(
        request["params"]["env"],
        json!({"DAGR_RUN": "/state/runs/run_1/run.json"})
    );

    let server = MockServer::one(|request| {
        json!({"id": request["id"], "result": {"type": "pane_list", "panes": [
            {"pane_id": "p_1", "tab_id": "t_1", "workspace_id": "w_1", "cwd": "/home/me/library", "terminal_id": "term_1"},
            {"pane_id": "p_2", "tab_id": "t_2", "workspace_id": "w_2", "cwd": "/home/me/monaco", "terminal_id": "term_2"}
        ]}})
    });
    let herdr = SocketHerdr::connect(server.path()).expect("connect socket client");
    let found = herdr
        .workspace_for_folder(Path::new("/home/me/monaco"))
        .expect("pane list");
    assert_eq!(found.as_deref(), Some("w_2"));
    let request: Value = serde_json::from_str(&server.request()).expect("request JSON");
    assert_eq!(request["method"], "pane.list");

    let server = MockServer::one(|request| {
        json!({"id": request["id"], "result": {"type": "workspace_created",
            "workspace": {"workspace_id": "w_4", "label": "monaco"},
            "tab": {"tab_id": "t_4", "label": "1"},
            "root_pane": {"pane_id": "p_4", "tab_id": "t_4", "workspace_id": "w_4", "terminal_id": "term_4"}}})
    });
    let herdr = SocketHerdr::connect(server.path()).expect("connect socket client");
    let created = herdr
        .workspace_create(Path::new("/home/me/monaco"), "monaco")
        .expect("workspace created");
    assert_eq!(created.workspace_id, "w_4");
    assert_eq!(created.tab.tab_id, "t_4");
    assert_eq!(created.tab.pane_id, "p_4");
    let request: Value = serde_json::from_str(&server.request()).expect("request JSON");
    assert_eq!(request["method"], "workspace.create");
    assert_eq!(
        request["params"],
        json!({"cwd": "/home/me/monaco", "label": "monaco", "focus": false})
    );

    // herdr answers tab.rename with the renamed tab, not a bare ok.
    let server = MockServer::one(
        |request| json!({"id": request["id"], "result": {"type": "tab_info", "tab": {"tab_id": "t_9", "label": "codex · review"}}}),
    );
    let herdr = SocketHerdr::connect(server.path()).expect("connect socket client");
    herdr
        .tab_rename("t_9", "codex · review")
        .expect("tab renamed");
    let request: Value = serde_json::from_str(&server.request()).expect("request JSON");
    assert_eq!(request["method"], "tab.rename");
    assert_eq!(
        request["params"],
        json!({"tab_id": "t_9", "label": "codex · review"})
    );

    let server = MockServer::one(|request| json!({"id": request["id"], "result": {"type": "ok"}}));
    let herdr = SocketHerdr::connect(server.path()).expect("connect socket client");
    herdr
        .workspace_report_metadata("w_1", None)
        .expect("metadata cleared");
    let request: Value = serde_json::from_str(&server.request()).expect("request JSON");
    assert_eq!(request["method"], "workspace.report_metadata");
    assert_eq!(request["params"]["tokens"], json!({"status": null}));
    assert_eq!(request["params"]["source"], "pod");

    // herdr answers notification.show with `notification_show`, not a bare ok.
    let server = MockServer::one(
        |request| json!({"id": request["id"], "result": {"type": "notification_show", "shown": true}}),
    );
    let herdr = SocketHerdr::connect(server.path()).expect("connect socket client");
    herdr
        .notification_show("pod: decision", Some("Ship or wait?"), "request")
        .expect("notification shown");
    let request: Value = serde_json::from_str(&server.request()).expect("request JSON");
    assert_eq!(request["method"], "notification.show");
    assert_eq!(
        request["params"],
        json!({"title": "pod: decision", "body": "Ship or wait?", "sound": "request"})
    );
}
