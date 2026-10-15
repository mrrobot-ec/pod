//! Wave 2 integration: T7's lifecycle, T8's wake ladder and projection, and
//! T9's worker-read on the wave 1 engines, through the commit hook and the
//! board, driven as a coordinator and the worker it started.

mod support;

use pod::cli::commands::workers::read::{self, Source};
use pod::engine::lifecycle::{self, Setup, StartInput, Worktree};
use pod::engine::messages::{self, SendInput};
use pod::engine::{deliveries, reconcile, runs, tasks};
use pod::herdr::HerdrEvent;
use pod::ids::{DispatchId, RunId, TaskId};
use pod::store::rows::{events, wake_attempts, workers};
use serde_json::Value;
use support::TestEnv;

const COORDINATOR: &str = "term-coordinator";

fn start(task: &TaskId, run: &RunId) -> StartInput {
    StartInput {
        task: Some(task.clone()),
        spec: None,
        agent: Some("claude".into()),
        terminal: None,
        worktree: Worktree::Current,
        name: None,
        repo: None,
        base_branch: None,
        display_name: None,
        comment: None,
        setup: Setup::Run,
        model: None,
        effort: None,
        fast: false,
        yolo: false,
        task_title: None,
        deps: Vec::new(),
        parent: None,
        retry_of: None,
        timeout_ms: Some(100),
        run: Some(run.clone()),
        handle: None,
    }
}

fn message(to: Option<&str>, kind: &str, dispatch: Option<&DispatchId>) -> SendInput {
    SendInput {
        to: to.map(str::to_owned),
        subject: kind.into(),
        body: None,
        r#type: kind.into(),
        priority: "normal".into(),
        thread_id: None,
        payload: None,
        task_id: None,
        dispatch_id: dispatch.cloned(),
        outcome: (kind == "worker_done").then(|| "succeeded".into()),
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

/// The board the commit hook last wrote, checked against dagr's v3 schema.
fn board(env: &TestEnv, run: &RunId) -> Value {
    let path = env
        .engine
        .paths
        .board_dir
        .join(run.as_str())
        .join("run.json");
    let value: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let schema: Value = serde_json::from_str(include_str!("board/dagr-v3.schema.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let errors: Vec<String> = validator
        .iter_errors(&value)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:?}\n{value}");
    value
}

fn card(board: &Value, task: &TaskId) -> Value {
    board["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|card| card["id"] == task.as_str())
        .cloned()
        .unwrap()
}

#[test]
fn a_started_worker_is_nudged_reports_and_is_released_through_one_store() {
    let mut env = TestEnv::new();
    env.as_caller(COORDINATOR, std::process::id());
    let caller = env.engine.caller.clone().unwrap();
    let now = env.engine.clock.now();
    let (run, task, failing) = env
        .engine
        .store
        .tx(|tx| {
            let run = runs::create(tx, now, "ship", &caller, env.tmp.path())?;
            let create = |spec: &str| {
                tasks::create(tx, now, &run.id, spec, None, None, &[], None, COORDINATOR)
            };
            let task = create("limiter")?;
            let failing = create("flaky")?;
            Ok((run.id, task.id, failing.id))
        })
        .unwrap();

    // Start: the board shows the Task working, herdr's start proves presence.
    let started = lifecycle::worker_start(&env.engine, start(&task, &run)).unwrap();
    assert_eq!(started.state, "ready");
    let dispatch = started.dispatch_id.clone();
    assert_eq!(card(&board(&env, &run), &task)["state"], "working");
    let view = lifecycle::show(&env.engine, &dispatch).unwrap();
    assert_eq!(view.projection.liveness, "live");
    let worker = view.worker;

    // herdr reports the worker idle (its hook records the screen), and the
    // coordinator's mail arrives more than 3 s later: the commit hook's first
    // ladder attempt finds the quiet window proven and types at once.
    pod::engine::reconcile::event(
        &env.engine,
        pod::herdr::HerdrEvent::AgentStatusChanged {
            pane_id: worker.pane_id.clone().unwrap(),
            status: pod::herdr::AgentStatus::Idle,
            needs_input: Vec::new(),
        },
    )
    .unwrap();
    env.clock.advance(std::time::Duration::from_secs(3));
    let mailbox = format!("dispatch:{dispatch}");
    messages::send(&env.engine, message(Some(&mailbox), "status", None)).unwrap();
    let attempts = env
        .engine
        .store
        .read(|tx| wake_attempts::list_since_last_consume(tx, &mailbox))
        .unwrap();
    assert_eq!(attempts.len(), 1, "{attempts:?}");
    assert_eq!(attempts[0].attempt, 1);
    assert_eq!(attempts[0].outcome, "delivered", "{attempts:?}");
    assert!(
        env.herdr
            .submissions()
            .iter()
            .any(|(_, text)| text.contains("orchestration message(s)"))
    );

    // The worker reads its mail (clearing the ladder) and reports.
    env.as_caller(worker.terminal_id.as_deref().unwrap(), std::process::id());
    let mail = deliveries::worker_check(&env.engine, &started.handle).unwrap();
    assert_eq!(mail.len(), 1);
    assert_eq!(
        lifecycle::show(&env.engine, &dispatch)
            .unwrap()
            .wake_ladder
            .attempts
            .len(),
        0
    );
    let done = messages::send(&env.engine, message(None, "worker_done", Some(&dispatch))).unwrap();
    assert_eq!(done.settled.unwrap().task_status, "completed");
    let reported = card(&board(&env, &run), &task);
    assert_eq!(reported["state"], "done");
    assert_eq!(reported["attempts"][0]["outcome"]["evidence"], "reported");

    // The coordinator accounts for it: release archives and closes the tab.
    env.as_caller(COORDINATOR, std::process::id());
    let view = lifecycle::show(&env.engine, &dispatch).unwrap();
    assert_eq!(view.projection.next_action.kind, "worker-release");
    env.herdr
        .set_screen(worker.pane_id.as_deref().unwrap(), "all tests pass\n");
    let released = lifecycle::release(&env.engine, &dispatch).unwrap();
    assert_eq!(released.state, "released");
    assert_eq!(released.archive.status, "preserved");
    assert_eq!(released.closed_tabs, [worker.tab_id.clone().unwrap()]);
    // herdr's close hook for the tab pod closed marks nothing user-owned.
    reconcile::event(
        &env.engine,
        HerdrEvent::PaneClosed {
            pane_id: worker.pane_id.clone().unwrap(),
        },
    )
    .unwrap();
    let after = env
        .engine
        .store
        .read(|tx| workers::get(tx, &dispatch))
        .unwrap()
        .unwrap();
    assert!(!after.user_owned);
    assert_eq!(after.transcript_proof.as_deref(), Some("pod_assigned"));
    let screen = read::worker_read(&env.engine, &dispatch, Source::Terminal, 10, None).unwrap();
    assert_eq!(screen.rows[0].text, "all tests pass");
    assert_eq!(card(&board(&env, &run), &task)["state"], "done");

    // A failed start settles its Dispatch; the Task goes back to the queue.
    env.herdr.fail_agent_start("agent never became ready");
    let failed = lifecycle::worker_start(&env.engine, start(&failing, &run)).unwrap();
    assert_eq!(failed.failed_stage.as_deref(), Some("agent"));
    let card = card(&board(&env, &run), &failing);
    assert_eq!(card["state"], "queued", "{card}");
    assert_eq!(card["attempts"][0]["state"], "failed", "{card}");
    let listed = lifecycle::list(&env.engine, Some(&run), Some("reclaimable"), 10, None).unwrap();
    assert_eq!(listed.workers.len(), 1);
    assert_eq!(listed.workers[0].dispatch.id, failed.dispatch_id);

    // One writer per action: one start, one ending, one consume per mailbox.
    let types: Vec<String> = env
        .engine
        .store
        .read(|tx| events::list(tx, &run))
        .unwrap()
        .into_iter()
        .map(|row| row.r#type)
        .collect();
    let count = |kind: &str| types.iter().filter(|t| *t == kind).count();
    assert_eq!(count("dispatch.dispatched"), 1, "{types:?}");
    assert_eq!(count("dispatch.completed"), 1, "{types:?}");
    assert_eq!(count("dispatch.start_failed"), 1, "{types:?}");
    assert_eq!(count("worker.user_owned"), 0, "{types:?}");
}
