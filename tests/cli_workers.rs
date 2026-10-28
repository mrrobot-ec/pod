mod support;

use serde_json::Value;

use support::TestEnv;

#[test]
fn cli_worker_receipt_shapes_and_exit_zero_only_for_ready() {
    let env = TestEnv::new();
    env.pod(Some("term-coord"))
        .args(["run-create", "--objective", "workers", "--json"])
        .assert()
        .success();
    let request = "req_00000000abcd";
    let first = env
        .pod(Some("term-coord"))
        .args([
            "worker-start",
            "--spec",
            "receipt work",
            "--agent",
            "codex",
            "--retry-request",
            request,
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let second = env
        .pod(Some("term-coord"))
        .args([
            "worker-start",
            "--spec",
            "receipt work",
            "--agent",
            "codex",
            "--retry-request",
            request,
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let first: Value = serde_json::from_slice(&first).unwrap();
    let second: Value = serde_json::from_slice(&second).unwrap();
    assert_eq!(first["state"], "ready");
    assert!(first["dispatchId"].as_str().unwrap().starts_with("ctx_"));
    assert_eq!(first["mutation"]["replayed"], false);
    assert_eq!(first["dispatchId"], second["dispatchId"]);
    assert_eq!(second["mutation"]["replayed"], true);

    env.pod(Some("term-coord"))
        .args([
            "worker-start",
            "--spec",
            "x",
            "--agent",
            "codex",
            "--on",
            "remote",
            "--json",
        ])
        .assert()
        .failure()
        .code(1);

    env.pod(Some("term-coord"))
        .args([
            "worker-start",
            "--spec",
            "x",
            "--agent",
            "codex",
            "--setup",
            "run",
            "--json",
        ])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn cli_t7_worker_commands_parse_documented_flags() {
    let cases: &[&[&str]] = &[
        &[
            "pod",
            "worker-start",
            "--task",
            "task_000000000001",
            "--agent",
            "codex",
            "--worktree",
            "new-top-level",
            "--name",
            "b",
            "--repo",
            "path:/r",
            "--base-branch",
            "main",
            "--display-name",
            "d",
            "--comment",
            "c",
            "--setup",
            "skip",
            "--model",
            "m",
            "--effort",
            "high",
            "--fast",
            "--timeout-ms",
            "1000",
            "--run",
            "run_000000000001",
        ],
        &[
            "pod",
            "worker-start",
            "--spec",
            "s",
            "--terminal",
            "term_1",
            "--task-title",
            "t",
            "--deps",
            "[]",
            "--parent",
            "task_000000000001",
            "--retry-of",
            "ctx_000000000001",
            "--on",
            "host",
        ],
        &["pod", "worker-show", "--dispatch", "ctx_000000000001"],
        &[
            "pod",
            "worker-list",
            "--run",
            "run_000000000001",
            "--terminal-state",
            "active",
            "--limit",
            "100",
            "--cursor",
            "1",
        ],
        &["pod", "worker-stop", "--dispatch", "ctx_000000000001"],
        &["pod", "worker-abandon", "--dispatch", "ctx_000000000001"],
        &[
            "pod",
            "worker-retain",
            "--dispatch",
            "ctx_000000000001",
            "--reason",
            "review",
        ],
        &["pod", "worker-release", "--dispatch", "ctx_000000000001"],
        &[
            "pod",
            "dispatch",
            "--task",
            "task_000000000001",
            "--to",
            "term_1",
            "--inject",
            "--dry-run",
            "--return-preamble",
        ],
        &["pod", "dispatch-show", "--task", "task_000000000001"],
    ];
    for args in cases {
        pod::cli::clap_command()
            .try_get_matches_from(*args)
            .unwrap();
    }
}

#[test]
fn worker_show_and_worker_list_print_the_brief_path() {
    let env = TestEnv::new();
    env.pod(Some("term-coord"))
        .args(["run-create", "--objective", "workers", "--json"])
        .assert()
        .success();
    let started = env
        .pod(Some("term-coord"))
        .args([
            "worker-start",
            "--spec",
            "brief path work",
            "--agent",
            "codex",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let started: Value = serde_json::from_slice(&started).unwrap();
    let dispatch = started["dispatchId"].as_str().unwrap();
    let suffix = format!("preambles/{dispatch}.txt");

    let shown = env
        .pod(Some("term-coord"))
        .args(["worker-show", "--dispatch", dispatch, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let shown: Value = serde_json::from_slice(&shown).unwrap();
    assert!(shown["briefPath"].as_str().unwrap().ends_with(&suffix));
    // A Codex worker has no reported cost: `null`, never `0`.
    assert_eq!(shown.get("costUsd"), Some(&Value::Null));

    let listed = env
        .pod(Some("term-coord"))
        .args(["worker-list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let listed: Value = serde_json::from_slice(&listed).unwrap();
    let row = &listed["workers"][0];
    assert_eq!(row["dispatchId"], dispatch);
    assert!(row["briefPath"].as_str().unwrap().ends_with(&suffix));
    assert_eq!(row.get("costUsd"), Some(&Value::Null));
}

#[test]
fn cli_failed_start_exits_1_with_failure_receipt_and_stop_release_shapes() {
    let env = TestEnv::new();
    env.pod(Some("term-coord"))
        .args(["run-create", "--objective", "workers", "--json"])
        .assert()
        .success();
    let failed = env
        .pod(Some("term-coord"))
        .args([
            "worker-start",
            "--spec",
            "no such branch",
            "--agent",
            "codex",
            "--worktree",
            "branch:missing",
            "--json",
        ])
        .assert()
        .failure()
        .code(1)
        .get_output()
        .stdout
        .clone();
    let failed: Value = serde_json::from_slice(&failed).unwrap();
    assert_eq!(failed["state"], "failed");
    assert_eq!(failed["failedStage"], "placement");
    assert_eq!(failed["dispatch"]["status"], "failed");
    assert_eq!(failed["worker"]["state"], "failed");
    assert!(failed["effects"].is_array() && failed["residualResources"].is_array());
    // Placement failed before any pane existed: nothing to recover.
    assert!(failed.get("nextCommands").is_none());
    assert_eq!(failed["setup"]["source"], "existing_worktree");
    assert!(failed["runId"].is_string() && failed["timeoutMs"].is_number());
    assert_eq!(failed["setup"]["state"], "none");

    let remote = env
        .pod(Some("term-coord"))
        .args([
            "worker-start",
            "--spec",
            "x",
            "--agent",
            "codex",
            "--on",
            "h",
            "--json",
        ])
        .assert()
        .failure()
        .code(1)
        .get_output()
        .stderr
        .clone();
    let remote: Value = serde_json::from_slice(&remote).unwrap();
    assert_eq!(remote["error"]["code"], "remote_unsupported");

    env.pod(Some("term-coord"))
        .args([
            "worker-start",
            "--spec",
            "x",
            "--terminal",
            "codex-0000",
            "--worktree",
            "path:/tmp",
            "--json",
        ])
        .assert()
        .failure()
        .code(2);

    let started = env
        .pod(Some("term-coord"))
        .args([
            "worker-start",
            "--spec",
            "work",
            "--agent",
            "codex",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let started: Value = serde_json::from_slice(&started).unwrap();
    let dispatch = started["dispatchId"].as_str().unwrap().to_owned();
    assert_eq!(started["worker"]["terminalState"], "active");
    assert_eq!(started["launch"]["requested"]["agent"], "codex");

    let stopped = env
        .pod(Some("term-coord"))
        .args(["worker-stop", "--dispatch", &dispatch, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stopped: Value = serde_json::from_slice(&stopped).unwrap();
    assert_eq!(stopped["dispatch"]["terminationReason"], "canceled");
    assert_eq!(stopped["worker"]["state"], "stopped");
    assert_eq!(stopped["closedPanes"].as_array().unwrap().len(), 1);

    let released = env
        .pod(Some("term-coord"))
        .args(["worker-release", "--dispatch", &dispatch, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let released: Value = serde_json::from_slice(&released).unwrap();
    assert_eq!(released["worker"]["terminalState"], "released");
    assert!(released["worker"].get("terminal_state").is_none());
    assert!(released["closedTabs"].is_array() && released["leftAlone"].is_array());
}

/// `pod status` lists the workers that look stuck; a live agent waiting on a
/// person comes first, then a silent one, then one idle with an open Dispatch.
#[test]
fn status_reports_a_worker_blocked_on_a_prompt_as_stalled() {
    let env = TestEnv::new();
    env.pod(Some("term-coord"))
        .args(["run-create", "--objective", "stalled", "--json"])
        .assert()
        .success();
    let started = env
        .pod(Some("term-coord"))
        .args([
            "worker-start",
            "--spec",
            "stuck work",
            "--agent",
            "codex",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let started: Value = serde_json::from_slice(&started).unwrap();
    let dispatch = started["dispatchId"].as_str().unwrap().to_owned();

    let observe = |seconds: i64, activity: &str, needs_input: &str| {
        let at = (chrono::Utc::now() + chrono::Duration::seconds(seconds)).to_rfc3339();
        let db = rusqlite::Connection::open(&env.engine.paths.db_path).unwrap();
        db.execute(
            "INSERT INTO observations (dispatch_id, liveness, activity, presence, needs_input, evidence_source, observed_at) VALUES (?1, 'live', ?2, 'present', ?3, 'herdr_state', ?4)",
            (&dispatch, activity, needs_input, &at),
        )
        .unwrap();
    };
    let stalled = || -> Vec<Value> {
        let status = env
            .pod(Some("term-coord"))
            .args(["status", "--json"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let status: Value = serde_json::from_slice(&status).unwrap();
        status["workers"]["stalled"].as_array().unwrap().clone()
    };

    observe(5, "idle_at_prompt", r#"{"count":1,"reasons":["blocked"]}"#);
    let blocked = stalled();
    assert_eq!(blocked.len(), 1, "{blocked:?}");
    assert_eq!(blocked[0]["dispatchId"], dispatch.as_str());
    assert_eq!(blocked[0]["reason"], "blocked_on_prompt");
    assert_eq!(blocked[0]["nextAction"]["kind"], "worker-read");

    observe(10, "idle_at_prompt", r#"{"count":0,"reasons":[]}"#);
    let idle = stalled();
    assert_eq!(idle.len(), 1, "{idle:?}");
    assert_eq!(idle[0]["reason"], "idle_with_open_dispatch");

    observe(15, "working", r#"{"count":0,"reasons":[]}"#);
    assert!(stalled().is_empty());
}

/// A stalled escalation from pod puts the worker in `pod status` under
/// `no_progress`, distinct from `stale`.
#[test]
fn status_lists_a_worker_with_a_stalled_escalation_as_no_progress() {
    let env = TestEnv::new();
    env.pod(Some("term-coord"))
        .args(["run-create", "--objective", "no progress", "--json"])
        .assert()
        .success();
    let started = env
        .pod(Some("term-coord"))
        .args([
            "worker-start",
            "--spec",
            "quiet work",
            "--agent",
            "codex",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let started: Value = serde_json::from_slice(&started).unwrap();
    let dispatch = started["dispatchId"].as_str().unwrap().to_owned();
    let run = started["runId"].as_str().unwrap().to_owned();

    let db = rusqlite::Connection::open(&env.engine.paths.db_path).unwrap();
    let at = (chrono::Utc::now() + chrono::Duration::seconds(30)).to_rfc3339();
    db.execute(
        "INSERT INTO messages (id, run_id, mailbox, from_handle, subject, type, priority, payload, read, created_at) VALUES ('msg_000000000903', ?1, ?2, 'pod', 'Stalled: worker (9 min without progress)', 'escalation', 'high', ?3, 0, ?4)",
        (
            &run,
            format!("run:{run}"),
            serde_json::json!({"dispatch_id": dispatch, "source": "pod", "reason": "stalled"})
                .to_string(),
            &at,
        ),
    )
    .unwrap();
    let status = env
        .pod(Some("term-coord"))
        .args(["status", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let status: Value = serde_json::from_slice(&status).unwrap();
    let stalled = status["workers"]["stalled"].as_array().unwrap();
    assert_eq!(stalled.len(), 1, "{stalled:?}");
    assert_eq!(stalled[0]["dispatchId"], dispatch.as_str());
    assert_eq!(stalled[0]["reason"], "no_progress");
    assert_eq!(stalled[0]["nextAction"]["kind"], "worker-read");
}

/// A worker whose brief herdr accepted but the transcript never showed is
/// listed as stalled with reason `brief_unconfirmed`.
#[test]
fn status_reports_an_unconfirmed_brief_as_stalled() {
    let env = TestEnv::new();
    env.pod(Some("term-coord"))
        .args(["run-create", "--objective", "unproven", "--json"])
        .assert()
        .success();
    let started = env
        .pod(Some("term-coord"))
        .args([
            "worker-start",
            "--spec",
            "unread work",
            "--agent",
            "codex",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let started: Value = serde_json::from_slice(&started).unwrap();
    let dispatch = started["dispatchId"].as_str().unwrap().to_owned();
    let db = rusqlite::Connection::open(&env.engine.paths.db_path).unwrap();
    db.execute(
        "UPDATE workers SET preamble_input = 'rendered_unconfirmed' WHERE dispatch_id = ?1",
        [&dispatch],
    )
    .unwrap();
    let status = env
        .pod(Some("term-coord"))
        .args(["status", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let status: Value = serde_json::from_slice(&status).unwrap();
    let stalled = status["workers"]["stalled"].as_array().unwrap();
    assert_eq!(stalled.len(), 1, "{stalled:?}");
    assert_eq!(stalled[0]["dispatchId"], dispatch.as_str());
    assert_eq!(stalled[0]["reason"], "brief_unconfirmed");
    assert_eq!(stalled[0]["nextAction"]["kind"], "worker-read");
}
