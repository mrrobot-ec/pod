mod support;

use std::path::Path;

use pod::engine::{runs, tasks};
use pod::{Clock, ErrorCode};

use support::TestEnv;

#[test]
fn reset_refused_with_live_workers_unless_force() {
    let mut env = TestEnv::new();
    env.as_caller("term-a", 1);
    let caller = env.engine.caller.as_ref().unwrap().clone();
    let run = env
        .engine
        .store
        .tx(|tx| runs::create(tx, env.clock.now(), "run", &caller, Path::new("/tmp/repo")))
        .unwrap();
    let task = env
        .engine
        .store
        .tx(|tx| {
            tasks::create(
                tx,
                env.clock.now(),
                &run.id,
                "work",
                None,
                None,
                &[],
                None,
                "term-a",
            )
        })
        .unwrap();
    env.engine.store.tx(|tx| {
        let now = env.clock.now().to_rfc3339();
        tx.0.execute("INSERT INTO dispatches (id, run_id, task_id, status, failure_count, depth, assignee_handle, assignee_terminal_id, assignee_pid, created_at) VALUES ('ctx_000000000001', ?1, ?2, 'dispatched', 0, 0, 'worker', 'term-worker', 2, ?3)", rusqlite::params![run.id.as_str(), task.id.as_str(), now])?;
        tx.0.execute("INSERT INTO workers (dispatch_id, state, owns_pane, terminal_state, created_at, updated_at) VALUES ('ctx_000000000001', 'ready', 1, 'active', ?1, ?1)", [&now])?;
        tx.0.execute("INSERT INTO observations (dispatch_id, liveness, activity, presence, needs_input, evidence_source, observed_at) VALUES ('ctx_000000000001', 'live', 'working', 'present', '{\"count\":0,\"reasons\":[]}', 'herdr_state', ?1)", [&now])?;
        Ok(())
    }).unwrap();
    let err =
        pod::cli::commands::recovery::reset_engine(&env.engine, &format!("run:{}", run.id), false)
            .unwrap_err();
    assert_eq!(err.code, ErrorCode::ResetRequiresForce);
    assert!(
        pod::cli::commands::recovery::reset_engine(&env.engine, &format!("run:{}", run.id), true)
            .unwrap()
    );
}

#[test]
fn reset_all_requires_force() {
    let env = TestEnv::new();
    let err = pod::cli::commands::recovery::reset_engine(&env.engine, "all", false).unwrap_err();
    assert_eq!(err.code, ErrorCode::ResetRequiresForce);
}

#[test]
fn reset_missing_run_is_usage() {
    let env = TestEnv::new();
    let error =
        pod::cli::commands::recovery::reset_engine(&env.engine, "run:run_000000000099", false)
            .unwrap_err();
    assert_eq!(error.code, ErrorCode::Usage);
    assert!(error.message.contains("run_000000000099"));
}

#[test]
fn log_lists_events_in_sequence() {
    let mut env = TestEnv::new();
    env.as_caller("term-a", 1);
    let caller = env.engine.caller.as_ref().unwrap().clone();
    let run = env
        .engine
        .store
        .tx(|tx| runs::create(tx, env.clock.now(), "run", &caller, Path::new("/tmp/repo")))
        .unwrap();
    env.engine.store.tx(|tx| {
        tx.0.execute("INSERT INTO events (run_id, at, type, actor, detail) VALUES (?1, ?2, 'one', 'term-a', '{}')", rusqlite::params![run.id.as_str(), env.clock.now().to_rfc3339()])?;
        tx.0.execute("INSERT INTO events (run_id, at, type, actor, detail) VALUES (?1, ?2, 'two', 'term-a', '{}')", rusqlite::params![run.id.as_str(), env.clock.now().to_rfc3339()])?;
        Ok(())
    }).unwrap();
    let events = pod::cli::commands::env::list_events(&env.engine, &run.id).unwrap();
    assert_eq!(events.len(), 3); // run.created plus the two explicit rows
    assert!(
        events
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence)
    );
}

#[test]
fn cli_run_create_task_create_deps_task_list_ready_brief_gate_request_reset_shapes() {
    use assert_cmd::Command;
    use serde_json::Value;
    use tempfile::tempdir;

    let tmp = tempdir().unwrap();
    let run_cmd = |args: &[&str]| {
        let output = Command::cargo_bin("pod")
            .unwrap()
            .args(args)
            .arg("--json")
            .env("HOME", tmp.path())
            .env("POD_STATE_DIR", tmp.path().join("state"))
            .env("HERDR_SOCKET_PATH", "/tmp/herdr/cli-test.sock")
            .env("POD_FAKE_HERDR", "1")
            .env("HERDR_ENV", "1")
            .env("HERDR_PANE_ID", "p_cli")
            .env("POD_FAKE_PANES", "p_cli=term-cli")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };

    let created = run_cmd(&[
        "run-create",
        "--objective",
        "ship",
        "--retry-request",
        "req_000000000010",
    ]);
    let run_id = created["run"]["id"].as_str().unwrap().to_owned();
    let used = run_cmd(&["run-use", "--id", &run_id]);
    assert_eq!(used["run"]["consumer_generation"], 2);
    let current = run_cmd(&["run-current"]);
    assert_eq!(current["run"]["id"], run_id);
    let status = run_cmd(&["status"]);
    assert_eq!(status["run"]["id"], run_id);
    let runs = run_cmd(&["run-list"]);
    assert_eq!(runs["runs"].as_array().unwrap().len(), 1);
    let shown_run = run_cmd(&["run-show", "--id", &run_id]);
    assert_eq!(shown_run["run"]["id"], run_id);
    let completed = run_cmd(&["request-show", "--request", "req_000000000010"]);
    assert_eq!(completed["state"], "completed");
    let first = run_cmd(&["task-create", "--spec", "first"]);
    let first_id = first["task"]["id"].as_str().unwrap().to_owned();
    let deps = format!("[\"{first_id}\"]");
    let second = run_cmd(&[
        "task-create",
        "--spec",
        "second",
        "--task-title",
        "Second task",
        "--display-name",
        "Second",
        "--deps",
        &deps,
        "--parent",
        &first_id,
    ]);
    let second_id = second["task"]["id"].as_str().unwrap().to_owned();
    assert_eq!(second["task"]["status"], "pending");
    let updated = run_cmd(&[
        "task-update",
        "--id",
        &first_id,
        "--status",
        "completed",
        "--result",
        "{\"verified\":true}",
    ]);
    assert_eq!(updated["task"]["status"], "completed");
    assert_eq!(updated["promoted"][0], second_id);
    let list = run_cmd(&[
        "task-list",
        "--status",
        "ready",
        "--run",
        &run_id,
        "--ready",
        "--brief",
    ]);
    assert!(
        list["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|task| task.get("spec").is_none())
    );
    let coordinator_gate = run_cmd(&[
        "gate-create",
        "--for",
        "coordinator",
        "--task",
        &second_id,
        "--question",
        "continue?",
        "--options",
        "[\"yes\",\"no\"]",
    ]);
    let coordinator_gate_id = coordinator_gate["gate"]["id"].as_str().unwrap().to_owned();
    run_cmd(&[
        "gate-resolve",
        "--id",
        &coordinator_gate_id,
        "--resolution",
        "yes",
    ]);
    let task_gates = run_cmd(&["gate-list", "--task", &second_id, "--run", &run_id]);
    assert_eq!(task_gates["gates"].as_array().unwrap().len(), 1);
    let gate = run_cmd(&[
        "gate-create",
        "--for",
        "user",
        "--question",
        "choose",
        "--options",
        "[\"a\"]",
        "--evidence",
        "msg",
        "--recommendation",
        "a",
    ]);
    let gate_id = gate["gate"]["id"].as_str().unwrap().to_owned();
    let gates = run_cmd(&["gate-list", "--run", &run_id]);
    assert_eq!(gates["gates"].as_array().unwrap().len(), 2);
    let resolved = run_cmd(&["gate-resolve", "--id", &gate_id, "--resolution", "a"]);
    assert_eq!(resolved["message"]["type"], "decision_gate");
    let duplicate = run_cmd(&["gate-resolve", "--id", &gate_id, "--resolution", "a"]);
    assert_eq!(duplicate["duplicate"], true);
    assert!(duplicate["message"].is_null());
    let log = run_cmd(&["log", "--run", &run_id]);
    assert!(!log["events"].as_array().unwrap().is_empty());
    let absent = run_cmd(&["request-show", "--request", "req_000000000001"]);
    assert_eq!(absent["state"], "absent");

    let caller = pod::Caller {
        terminal_id: "term-cli".into(),
        pane_id: None,
        pid: 1,
        session: "cli-test".into(),
    };
    let connection = rusqlite::Connection::open(tmp.path().join("state/cli-test/pod.db")).unwrap();
    connection.execute(
        "INSERT INTO receipts (caller_fingerprint, request_id, method, payload_hash, state, created_at, updated_at) VALUES (?1, 'req_000000000011', 'task-create', 'hash', 'pending', '2026-10-12T12:00:00Z', '2026-10-12T12:00:00Z')",
        [caller.fingerprint()],
    ).unwrap();
    drop(connection);
    let pending = run_cmd(&["request-show", "--request", "req_000000000011"]);
    assert_eq!(pending["state"], "pending");
    let reset = run_cmd(&["reset", "--scope", &format!("run:{run_id}")]);
    assert_eq!(reset["reset"]["deleted"], true);
}

#[test]
fn reset_all_own_retry_receipt_survives() {
    use assert_cmd::Command;
    use serde_json::Value;
    use tempfile::tempdir;

    let tmp = tempdir().unwrap();
    let run_cmd = |args: &[&str]| {
        let output = Command::cargo_bin("pod")
            .unwrap()
            .args(args)
            .arg("--json")
            .env("HOME", tmp.path())
            .env("POD_STATE_DIR", tmp.path().join("state"))
            .env("HERDR_SOCKET_PATH", "/tmp/herdr/reset-test.sock")
            .env("POD_FAKE_HERDR", "1")
            .env("HERDR_ENV", "1")
            .env("HERDR_PANE_ID", "p_cli")
            .env("POD_FAKE_PANES", "p_cli=term-cli")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    run_cmd(&["run-create", "--objective", "temporary"]);
    let first = run_cmd(&[
        "reset",
        "--scope",
        "all",
        "--force",
        "--retry-request",
        "req_000000000012",
    ]);
    assert_eq!(first["reset"]["deleted"], true);
    let shown = run_cmd(&["request-show", "--request", "req_000000000012"]);
    assert_eq!(shown["state"], "completed");
    let replay = run_cmd(&[
        "reset",
        "--scope",
        "all",
        "--force",
        "--retry-request",
        "req_000000000012",
    ]);
    assert_eq!(replay["reset"]["deleted"], true);
}

#[test]
fn status_reports_unreachable_herdr_and_reset_names_live_workers() {
    use assert_cmd::Command;
    use serde_json::Value;
    use tempfile::tempdir;

    let tmp = tempdir().unwrap();
    let pod = |args: &[&str], fake_herdr: bool| {
        let mut command = Command::cargo_bin("pod").unwrap();
        command
            .args(args)
            .arg("--json")
            .env("HOME", tmp.path())
            .env("POD_STATE_DIR", tmp.path().join("state"))
            .env("HERDR_SOCKET_PATH", tmp.path().join("missing/herdr.sock"))
            .env("HERDR_ENV", "1")
            .env("HERDR_PANE_ID", "p_cli")
            .env("POD_FAKE_PANES", "p_cli=term-cli");
        if fake_herdr {
            command.env("POD_FAKE_HERDR", "1");
        } else {
            command.env("HERDR_ENV", "1").env("HERDR_PANE_ID", "p_1");
        }
        command.output().unwrap()
    };

    let created = pod(&["run-create", "--objective", "status"], true);
    assert!(created.status.success());
    let created: Value = serde_json::from_slice(&created.stdout).unwrap();
    let run_id = created["run"]["id"].as_str().unwrap().to_owned();

    let status = pod(&["status"], false);
    assert!(
        status.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&status.stderr)
    );
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["herdr"]["reachable"], false);
    // No herdr, no caller: the bound Run cannot be resolved, and is not shown.
    assert!(status["run"].is_null());
    let status: Value = serde_json::from_slice(&pod(&["status"], true).stdout).unwrap();
    assert_eq!(status["run"]["id"], run_id);

    let task = pod(&["task-create", "--spec", "work"], true);
    let task: Value = serde_json::from_slice(&task.stdout).unwrap();
    let task_id = task["task"]["id"].as_str().unwrap().to_owned();
    let db = rusqlite::Connection::open(tmp.path().join("state/missing/pod.db")).unwrap();
    db.execute_batch(&format!(
        "INSERT INTO dispatches (id, run_id, task_id, status, failure_count, depth, assignee_handle, assignee_terminal_id, assignee_pid, created_at) VALUES ('ctx_000000000001', '{run_id}', '{task_id}', 'dispatched', 0, 0, 'sol-1a2b', 'term-worker', 2, '2026-10-12T12:00:00+00:00');
         INSERT INTO workers (dispatch_id, state, owns_pane, terminal_state, created_at, updated_at) VALUES ('ctx_000000000001', 'ready', 1, 'active', '2026-10-12T12:00:00+00:00', '2026-10-12T12:00:00+00:00');
         INSERT INTO observations (dispatch_id, liveness, activity, presence, needs_input, evidence_source, observed_at) VALUES ('ctx_000000000001', 'live', 'working', 'present', '{{}}', 'herdr_state', '2026-10-12T12:00:00+00:00');"
    ))
    .unwrap();
    drop(db);
    let refused = pod(&["reset", "--scope", &format!("run:{run_id}")], true);
    assert_eq!(refused.status.code(), Some(1));
    let error: Value = serde_json::from_slice(&refused.stderr).unwrap();
    assert_eq!(error["error"]["code"], "reset_requires_force");
    assert_eq!(error["error"]["data"]["liveWorkers"][0], "sol-1a2b");
    assert!(error["error"]["data"]["nextSteps"].is_array());
}

#[test]
fn task_list_points_waiting_dependants_at_the_failed_dependency() {
    use assert_cmd::Command;
    use serde_json::Value;
    use tempfile::tempdir;

    let tmp = tempdir().unwrap();
    let run_cmd = |args: &[&str]| {
        let output = Command::cargo_bin("pod")
            .unwrap()
            .args(args)
            .arg("--json")
            .env("HOME", tmp.path())
            .env("POD_STATE_DIR", tmp.path().join("state"))
            .env("HERDR_SOCKET_PATH", "/tmp/herdr/waiting-test.sock")
            .env("POD_FAKE_HERDR", "1")
            .env("HERDR_ENV", "1")
            .env("HERDR_PANE_ID", "p_cli")
            .env("POD_FAKE_PANES", "p_cli=term-cli")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    run_cmd(&["run-create", "--objective", "waiting"]);
    let first = run_cmd(&["task-create", "--spec", "first"]);
    let first_id = first["task"]["id"].as_str().unwrap().to_owned();
    let deps = format!("[\"{first_id}\"]");
    let second = run_cmd(&["task-create", "--spec", "second", "--deps", &deps]);
    let second_id = second["task"]["id"].as_str().unwrap().to_owned();
    run_cmd(&["task-update", "--id", &first_id, "--status", "failed"]);
    let list = run_cmd(&["task-list", "--status", "pending"]);
    let row = &list["tasks"][0];
    assert_eq!(row["id"], second_id);
    assert_eq!(row["attention"], "waiting_on_failed_dependency");
    assert_eq!(
        row["nextAction"],
        serde_json::json!(["pod", "dispatch-show", "--task", first_id])
    );
}

#[test]
fn id_addressed_mutations_work_outside_herdr() {
    use assert_cmd::Command;
    use serde_json::Value;
    use tempfile::tempdir;

    let tmp = tempdir().unwrap();
    let pod = |args: &[&str], inside: bool| {
        let mut command = Command::cargo_bin("pod").unwrap();
        command
            .args(args)
            .arg("--json")
            .env("HOME", tmp.path())
            .env("POD_STATE_DIR", tmp.path().join("state"))
            .env("HERDR_SOCKET_PATH", "/tmp/herdr/outside-test.sock")
            .env("POD_FAKE_HERDR", "1");
        if inside {
            command
                .env("HERDR_ENV", "1")
                .env("HERDR_PANE_ID", "p_cli")
                .env("POD_FAKE_PANES", "p_cli=term-cli");
        } else {
            // A plain shell, even when the test itself runs inside a herdr pane.
            command.env_remove("HERDR_ENV").env_remove("HERDR_PANE_ID");
        }
        let output = command.output().unwrap();
        (output.status.code(), output.stdout, output.stderr)
    };
    let ok = |result: (Option<i32>, Vec<u8>, Vec<u8>)| {
        assert_eq!(
            result.0,
            Some(0),
            "stderr={}",
            String::from_utf8_lossy(&result.2)
        );
        serde_json::from_slice::<Value>(&result.1).unwrap()
    };
    let run = ok(pod(&["run-create", "--objective", "outside"], true));
    let run_id = run["run"]["id"].as_str().unwrap().to_owned();
    let task = ok(pod(&["task-create", "--spec", "work"], true));
    let task_id = task["task"]["id"].as_str().unwrap().to_owned();
    let gate = ok(pod(
        &[
            "gate-create",
            "--for",
            "user",
            "--question",
            "which?",
            "--options",
            "[\"a\",\"b\"]",
            "--evidence",
            "docs/plan.md",
            "--recommendation",
            "a",
        ],
        true,
    ));
    let gate_id = gate["gate"]["id"].as_str().unwrap().to_owned();

    // The person answers from a plain shell: no herdr caller.
    let resolved = ok(pod(
        &["gate-resolve", "--id", &gate_id, "--resolution", "b"],
        false,
    ));
    assert_eq!(resolved["message"]["type"], "decision_gate");
    let updated = ok(pod(
        &["task-update", "--id", &task_id, "--status", "blocked"],
        false,
    ));
    assert_eq!(updated["task"]["status"], "blocked");
    let (code, _, stderr) = pod(
        &[
            "task-update",
            "--id",
            &task_id,
            "--status",
            "ready",
            "--retry-request",
            "req_000000000020",
        ],
        false,
    );
    assert_eq!(code, Some(1));
    let error: Value = serde_json::from_slice(&stderr).unwrap();
    assert_eq!(error["error"]["code"], "not_in_herdr");
    let reset = ok(pod(&["reset", "--scope", &format!("run:{run_id}")], false));
    assert_eq!(reset["reset"]["deleted"], true);
}

#[test]
fn run_create_takes_a_budget_and_run_show_reports_spent_against_it() {
    use assert_cmd::Command;
    use serde_json::Value;
    use tempfile::tempdir;

    let tmp = tempdir().unwrap();
    let pod = |args: &[&str]| {
        Command::cargo_bin("pod")
            .unwrap()
            .args(args)
            .arg("--json")
            .env("HOME", tmp.path())
            .env("POD_STATE_DIR", tmp.path().join("state"))
            .env("HERDR_SOCKET_PATH", "/tmp/herdr/cli-test.sock")
            .env("POD_FAKE_HERDR", "1")
            .env("HERDR_ENV", "1")
            .env("HERDR_PANE_ID", "p_cli")
            .env("POD_FAKE_PANES", "p_cli=term-cli")
            .output()
            .unwrap()
    };
    let json = |output: std::process::Output| -> Value {
        assert!(
            output.status.success(),
            "stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    };
    let created = json(pod(&[
        "run-create",
        "--objective",
        "budgeted",
        "--budget-usd",
        "5",
    ]));
    assert_eq!(created["budgetUsd"], 5.0);
    let run_id = created["run"]["id"].as_str().unwrap().to_owned();
    let shown = json(pod(&["run-show", "--id", &run_id]));
    assert_eq!(shown["budgetUsd"], 5.0);
    assert_eq!(shown["spentUsd"], 0.0);
    // A Run without a budget keeps the Orca contract's shape.
    let plain = json(pod(&["run-create", "--objective", "plain"]));
    assert!(plain.get("budgetUsd").is_none());
    let plain_id = plain["run"]["id"].as_str().unwrap().to_owned();
    let shown = json(pod(&["run-show", "--id", &plain_id]));
    assert!(shown.get("budgetUsd").is_none() && shown.get("spentUsd").is_none());
    // A budget must be a positive number.
    let refused = pod(&["run-create", "--objective", "free", "--budget-usd", "0"]);
    assert_eq!(refused.status.code(), Some(2));
}

/// Spec A5.4: a `[notify] command` that fails is a warning in the
/// `gate-create` receipt, never a failed mutation.
#[test]
fn a_failing_notify_command_is_a_warning_on_the_gate_create_receipt() {
    use serde_json::Value;

    let env = TestEnv::new();
    let config = env.tmp.path().join("notify.toml");
    std::fs::write(
        &config,
        "[notify]\ncommand = [\"sh\", \"-c\", \"exit 7\"]\n",
    )
    .unwrap();
    let run = |args: &[&str]| {
        let output = env
            .pod(Some("term-cli"))
            .args(args)
            .arg("--json")
            .env("POD_CONFIG", &config)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(0),
            "stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    run(&["run-create", "--objective", "notify"]);
    let gate = run(&[
        "gate-create",
        "--for",
        "user",
        "--question",
        "which?",
        "--options",
        "[\"a\",\"b\"]",
        "--evidence",
        "docs/plan.md",
        "--recommendation",
        "a",
    ]);
    assert_eq!(gate["gate"]["status"], "pending");
    let warnings = gate["warnings"].as_array().expect("warnings");
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0]
            .as_str()
            .unwrap()
            .starts_with("notify command failed: sh exited with"),
        "{warnings:?}"
    );
}
