//! Wave 1 integration: the T2 and T3 engines on T1's store rows, with the
//! commit hook writing T6's board, driven as a coordinator and a worker.

mod support;

use std::path::PathBuf;

use chrono::Duration;
use pod::engine::messages::{self, SendInput};
use pod::engine::{deliveries, questions, runs, tasks};
use pod::ids::{DispatchId, RunId, TaskId, WakeId};
use pod::store::rows::{dispatches, events, wake_attempts, workers};
use serde_json::Value;
use support::TestEnv;

const COORDINATOR: &str = "term-coordinator";
const WORKER: &str = "term-worker";
const HANDLE: &str = "sol-a1b2";

struct Seeded {
    run: RunId,
    task: TaskId,
    dispatch: DispatchId,
}

/// A Run coordinated by `COORDINATOR` with one Task dispatched to `WORKER`
/// (what `worker-start` will store in T7).
fn seed(env: &mut TestEnv) -> Seeded {
    env.as_caller(COORDINATOR, std::process::id());
    let caller = env.engine.caller.clone().unwrap();
    let now = pod::Clock::now(env.clock.as_ref());
    let (run, task) = env
        .engine
        .store
        .tx(|tx| {
            let run = runs::create(tx, now, "ship", &caller, std::path::Path::new("/tmp"))?;
            let task = tasks::create(
                tx,
                now,
                &run.id,
                "spec",
                Some("Limiter"),
                None,
                &[],
                None,
                COORDINATOR,
            )?;
            Ok((run.id, task.id))
        })
        .unwrap();
    let dispatch = DispatchId::new();
    env.engine
        .store
        .tx(|tx| {
            pod::store::rows::tasks::update_status(tx, &task, "dispatched")?;
            dispatches::insert(
                tx,
                &dispatches::DispatchRow {
                    id: dispatch.clone(),
                    run_id: run.clone(),
                    task_id: task.clone(),
                    status: "dispatched".into(),
                    retry_of_dispatch_id: None,
                    failure_count: 0,
                    last_failure: None,
                    termination_reason: None,
                    termination_target: None,
                    depth: 1,
                    assignee_handle: Some(HANDLE.into()),
                    assignee_terminal_id: Some(WORKER.into()),
                    assignee_pid: Some(std::process::id()),
                    agent_kind: Some("codex".into()),
                    launch_requested: None,
                    launch_effective: None,
                    dispatched_at: Some(now),
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
                    state: "ready".into(),
                    stage: None,
                    placement: None,
                    worktree_path: None,
                    workspace_id: None,
                    tab_id: None,
                    pane_id: Some(support::caller_pane(WORKER)),
                    terminal_id: Some(WORKER.into()),
                    owns_pane: true,
                    user_owned: false,
                    user_owned_reason: None,
                    setup_state: None,
                    setup_pane_id: None,
                    preamble_input: None,
                    effects: None,
                    residual_resources: None,
                    terminal_state: "active".into(),
                    retained_reason: None,
                    session_id: None,
                    transcript_path: None,
                    transcript_proof: None,
                    last_error: None,
                    created_at: now,
                    updated_at: now,
                },
            )
        })
        .unwrap();
    Seeded {
        run,
        task,
        dispatch,
    }
}

fn board_file(env: &TestEnv, run: &RunId) -> PathBuf {
    env.engine
        .paths
        .board_dir
        .join(run.as_str())
        .join("run.json")
}

fn board_task(env: &TestEnv, seeded: &Seeded) -> Value {
    let board: Value =
        serde_json::from_slice(&std::fs::read(board_file(env, &seeded.run)).unwrap()).unwrap();
    board["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["id"] == seeded.task.as_str())
        .cloned()
        .unwrap()
}

fn worker_done(dispatch: &DispatchId) -> SendInput {
    SendInput {
        to: None,
        subject: "done".into(),
        body: None,
        r#type: "worker_done".into(),
        priority: "normal".into(),
        thread_id: None,
        payload: None,
        task_id: None,
        dispatch_id: Some(dispatch.clone()),
        outcome: Some("succeeded".into()),
        reason: None,
        reason_target: None,
        files_modified: Vec::new(),
        report_path: None,
        phase: None,
        evidence: None,
        recommendation: None,
        from: None,
    }
}

#[test]
fn board_follows_ask_reply_and_worker_done_through_after_commit() {
    let mut env = TestEnv::new();
    let seeded = seed(&mut env);

    env.as_caller(WORKER, std::process::id());
    let (question, _) = questions::ask_store(&env.engine, "v1 or v2?", &[], None).unwrap();
    let task = board_task(&env, &seeded);
    assert_eq!(task["state"], "blocked", "{task}");
    assert_eq!(task["unblock"], "coordinator", "{task}");

    env.as_caller(COORDINATOR, std::process::id());
    questions::reply(&env.engine, &question, "v2").unwrap();
    assert_ne!(board_task(&env, &seeded)["state"], "blocked");

    env.as_caller(WORKER, std::process::id());
    let receipt = messages::send(&env.engine, worker_done(&seeded.dispatch)).unwrap();
    let settled = receipt.settled.unwrap();
    assert_eq!(
        (
            settled.task_status.as_str(),
            settled.dispatch_status.as_str()
        ),
        ("completed", "completed")
    );
    assert_eq!(board_task(&env, &seeded)["state"], "done");
    let worker = env
        .engine
        .store
        .read(|tx| workers::get(tx, &seeded.dispatch))
        .unwrap()
        .unwrap();
    assert_eq!(
        (worker.state.as_str(), worker.terminal_state.as_str()),
        ("succeeded", "reclaimable")
    );
    // settle_dispatch is the one writer of the ending event.
    let endings = env
        .engine
        .store
        .read(|tx| events::list(tx, &seeded.run))
        .unwrap()
        .into_iter()
        .filter(|event| event.r#type.starts_with("dispatch."))
        .count();
    assert_eq!(endings, 1);
}

#[test]
fn consuming_check_records_mailbox_consumed_and_clears_the_wake_ladder() {
    let mut env = TestEnv::new();
    let seeded = seed(&mut env);
    let mailbox = format!("dispatch:{}", seeded.dispatch);
    let now = pod::Clock::now(env.clock.as_ref());
    env.engine
        .store
        .tx(|tx| {
            wake_attempts::insert(
                tx,
                &wake_attempts::WakeAttemptRow {
                    id: WakeId::new(),
                    run_id: seeded.run.clone(),
                    mailbox: mailbox.clone(),
                    delivery_id: None,
                    attempt: 1,
                    outcome: "skipped".into(),
                    detail: None,
                    created_at: now,
                },
            )
        })
        .unwrap();
    let count = |env: &TestEnv| {
        env.engine
            .store
            .read(|tx| wake_attempts::count_since_last_consume(tx, &mailbox))
            .unwrap()
    };
    assert_eq!(count(&env), 1);
    env.clock.advance(Duration::seconds(1).to_std().unwrap());
    env.as_caller(WORKER, std::process::id());
    deliveries::worker_check(&env.engine, HANDLE).unwrap();
    assert_eq!(count(&env), 0);

    // The coordinator's consuming check records its Run mailbox the same way.
    env.as_caller(COORDINATOR, std::process::id());
    deliveries::check_consume(&env.engine, &format!("run:{}", seeded.run)).unwrap();
    let consumed = env
        .engine
        .store
        .read(|tx| events::list(tx, &seeded.run))
        .unwrap()
        .into_iter()
        .filter(|event| event.r#type == "mailbox.consumed")
        .map(|event| event.detail.unwrap()["mailbox"].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        consumed,
        vec![
            Value::from(mailbox),
            Value::from(format!("run:{}", seeded.run))
        ]
    );
}

#[test]
fn cli_worker_bare_check_reads_its_dispatch_mailbox() {
    let mut env = TestEnv::new();
    let seeded = seed(&mut env);
    env.as_caller(COORDINATOR, std::process::id());
    let mut input = worker_done(&seeded.dispatch);
    input.r#type = "status".into();
    input.subject = "use v2".into();
    input.to = Some(HANDLE.into());
    input.outcome = None;
    input.dispatch_id = None;
    messages::send(&env.engine, input).unwrap();

    let output = env
        .pod(Some(WORKER))
        .args(["check", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["messages"][0]["subject"], "use v2", "{value}");
    assert!(value.get("delivery").is_none(), "worker form: {value}");
}

#[test]
fn cli_board_defaults_to_the_bound_run_and_reset_removes_it() {
    let mut env = TestEnv::new();
    let seeded = seed(&mut env);
    let board = env
        .pod(Some(COORDINATOR))
        .args(["board", "--json"])
        .output()
        .unwrap();
    assert!(
        board.status.success(),
        "{}",
        String::from_utf8_lossy(&board.stderr)
    );
    let board: Value = serde_json::from_slice(&board.stdout).unwrap();
    assert_eq!(board["run"], seeded.run.as_str());
    assert!(board_file(&env, &seeded.run).exists());

    let reset = env
        .pod(Some(COORDINATOR))
        .args(["reset", "--scope", &format!("run:{}", seeded.run), "--json"])
        .output()
        .unwrap();
    assert!(
        reset.status.success(),
        "{}",
        String::from_utf8_lossy(&reset.stderr)
    );
    assert!(!board_file(&env, &seeded.run).parent().unwrap().exists());
}

#[test]
fn reads_with_run_work_inside_herdr_while_herdr_is_down() {
    let mut env = TestEnv::new();
    let seeded = seed(&mut env);
    let run = seeded.run.to_string();
    for args in [
        vec!["task-list", "--run", &run],
        vec!["gate-list", "--run", &run],
        vec!["run-show", "--id", &run],
        vec!["log", "--run", &run],
        vec!["inbox", "--run", &run],
    ] {
        // A real (absent) herdr socket, as inside a pane whose server died.
        let output = env
            .pod(None)
            .env_remove("POD_FAKE_HERDR")
            .env("HERDR_SOCKET_PATH", env.tmp.path().join("gone/test.sock"))
            .env("HERDR_ENV", "1")
            .env("HERDR_PANE_ID", "p_gone")
            .args(&args)
            .arg("--json")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "pod {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn an_unknown_run_is_a_usage_error_for_run_scoped_reads() {
    let env = TestEnv::new();
    for command in ["task-list", "gate-list", "board"] {
        env.pod(None)
            .args([command, "--run", "run_000000000000", "--json"])
            .assert()
            .code(2);
    }
}
