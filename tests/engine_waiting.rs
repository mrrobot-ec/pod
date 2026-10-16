mod support;

use std::str::FromStr;
use std::thread;
use std::time::{Duration, Instant};

use pod::Clock;
use pod::engine::waiting::{self, AskOutcome};
use pod::ids::{MessageId, RunId};
use pod::store::rows::{messages, questions, wake_attempts};
use pod::{Engine, Store, SystemClock};
use support::TestEnv;

#[test]
fn t8_cli_forms_parse() {
    for args in [
        vec!["pod", "hook", "startup", "--json"],
        vec!["pod", "hook", "event", "--json"],
        vec![
            "pod",
            "check",
            "--wait",
            "--types",
            "status",
            "--timeout-ms",
            "1",
        ],
        vec![
            "pod",
            "ask",
            "--resume",
            "msg_000000000001",
            "--timeout-ms",
            "1",
        ],
    ] {
        pod::cli::clap_command().try_get_matches_from(args).unwrap();
    }
}

const RUN: &str = "run_000000000821";
const QUESTION: &str = "msg_000000000821";

fn seed_run(env: &mut TestEnv) {
    env.as_caller("term-coordinator", std::process::id());
    let now = Clock::now(env.clock.as_ref());
    env.engine.store.tx(|tx| {
        tx.0.execute("INSERT INTO runs (id, objective, coordinator_terminal_id, coordinator_handle, consumer_generation, home_cwd, depth, created_at, updated_at) VALUES (?1, 'wait', 'term-coordinator', 'coordinator', 1, '/tmp', 0, ?2, ?2)", (RUN, now.to_rfc3339()))?;
        Ok(())
    }).unwrap();
}

fn insert_message(engine: &Engine, id: &str, kind: &str) {
    let now = engine.clock.now();
    engine
        .store
        .tx(|tx| {
            messages::insert(
                tx,
                &messages::MessageRow {
                    sequence: None,
                    id: MessageId::from_str(id)?,
                    run_id: RunId::from_str(RUN)?,
                    mailbox: format!("run:{RUN}"),
                    from_handle: "worker".into(),
                    subject: "subject".into(),
                    body: Some("body".into()),
                    r#type: kind.into(),
                    priority: "normal".into(),
                    thread_id: None,
                    payload: None,
                    read: false,
                    created_at: now,
                    delivered_at: None,
                    sender_terminal_id: Some("term-worker".into()),
                    rejected_reason: None,
                    evidence: None,
                    recommendation: None,
                },
            )?;
            Ok(())
        })
        .unwrap();
}

fn reopen(env: &TestEnv) -> Engine {
    Engine::new(
        Store::open(&env.engine.paths).unwrap(),
        Box::new((*env.herdr).clone()),
        Box::new(SystemClock),
        env.engine.config.clone(),
        env.engine.paths.clone(),
        env.engine.caller.clone(),
    )
}

fn seed_question(env: &TestEnv) -> MessageId {
    insert_message(&env.engine, QUESTION, "question");
    let id = MessageId::from_str(QUESTION).unwrap();
    let now = env.engine.clock.now();
    env.engine
        .store
        .tx(|tx| {
            questions::insert(
                tx,
                &questions::QuestionRow {
                    message_id: id.clone(),
                    run_id: RunId::from_str(RUN)?,
                    dispatch_id: None,
                    asker_handle: "worker".into(),
                    status: "pending".into(),
                    answer_message_id: None,
                    answer_body: None,
                    created_at: now,
                    answered_at: None,
                    closed_at: None,
                },
            )
        })
        .unwrap();
    id
}

#[test]
fn check_wait_single_waiter_waiter_exists() {
    let mut env = TestEnv::new();
    seed_run(&mut env);
    let first = reopen(&env);
    let handle = thread::spawn(move || waiting::check_wait(&first, &[], 300));
    thread::sleep(Duration::from_millis(50));
    let error = waiting::check_wait(&env.engine, &[], 10).unwrap_err();
    assert_eq!(error.code, pod::ErrorCode::WaiterExists);
    handle.join().unwrap().unwrap();
}

#[test]
fn ask_does_not_occupy_waiter_slot() {
    let mut env = TestEnv::new();
    seed_run(&mut env);
    let id = seed_question(&env);
    let first = reopen(&env);
    let handle = thread::spawn(move || waiting::check_wait(&first, &[], 200));
    thread::sleep(Duration::from_millis(50));
    assert!(matches!(
        waiting::ask_wait(&env.engine, &id, 1).unwrap(),
        AskOutcome::Timeout(_)
    ));
    handle.join().unwrap().unwrap();
}

/// Spec 16 timing; a debug build under emulation measured 263 ms, so the
/// assertion runs only in `cargo test --release` (deterministic everywhere).
#[test]
#[cfg_attr(debug_assertions, ignore = "timing test: run with --release")]
fn wake_latency_under_100ms() {
    let mut env = TestEnv::new();
    seed_run(&mut env);
    let waiter = reopen(&env);
    let handle = thread::spawn(move || waiting::check_wait(&waiter, &["status".into()], 2_000));
    thread::sleep(Duration::from_millis(50));
    let started = Instant::now();
    insert_message(&env.engine, "msg_000000000822", "status");
    let delivery = handle.join().unwrap().unwrap();
    assert_eq!(delivery.0.messages.len(), 1);
    assert!(
        started.elapsed() < Duration::from_millis(100),
        "wake took {:?}",
        started.elapsed()
    );
}

#[test]
fn ask_timeout_leaves_pending_and_resume_does_not_duplicate() {
    let mut env = TestEnv::new();
    seed_run(&mut env);
    let id = seed_question(&env);
    assert!(
        matches!(waiting::ask_wait(&env.engine, &id, 5).unwrap(), AskOutcome::Timeout(ref value) if value == &id)
    );
    let count: i64 = env
        .engine
        .store
        .read(|tx| {
            Ok(tx
                .0
                .query_row("SELECT COUNT(*) FROM questions", [], |row| row.get(0))?)
        })
        .unwrap();
    assert_eq!(count, 1);
    env.engine
        .store
        .tx(|tx| {
            questions::update_status(tx, &id, "answered")?;
            questions::update_answer_body(tx, &id, Some("answer"))
        })
        .unwrap();
    assert_eq!(
        waiting::ask_wait(&env.engine, &id, 5).unwrap(),
        AskOutcome::Answered("answer".into())
    );
    let count: i64 = env
        .engine
        .store
        .read(|tx| {
            Ok(tx
                .0
                .query_row("SELECT COUNT(*) FROM questions", [], |row| row.get(0))?)
        })
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn killed_waiter_leaves_no_stale_slot() {
    let mut env = TestEnv::new();
    seed_run(&mut env);
    // A check --wait killed before it could clean up leaves its file behind.
    std::fs::write(
        env.engine
            .paths
            .state_dir
            .join(format!("check-wait-{RUN}.lock")),
        br#"{"mailbox":"run:run_000000000821","types":["status"]}"#,
    )
    .unwrap();
    let delivery = waiting::check_wait(&env.engine, &[], 10).unwrap();
    assert!(delivery.0.messages.is_empty());

    // Nor does it stand in for a waiter: the nudge is attempted (and skipped
    // here, because no coordinator agent is detected).
    insert_message(&env.engine, "msg_000000000823", "status");
    let row = pod::engine::wake::after_commit_nudge(&env.engine, &format!("run:{RUN}"))
        .unwrap()
        .unwrap();
    assert_eq!(row.outcome, "skipped");
}

#[test]
fn matching_waiter_replaces_the_nudge() {
    let mut env = TestEnv::new();
    seed_run(&mut env);
    let waiter = reopen(&env);
    let handle =
        thread::spawn(move || waiting::check_wait(&waiter, &["decision_gate".into()], 2_000));
    thread::sleep(Duration::from_millis(100));
    insert_message(&env.engine, "msg_000000000824", "decision_gate");
    assert!(
        pod::engine::wake::after_commit_nudge(&env.engine, &format!("run:{RUN}"))
            .unwrap()
            .is_none()
    );
    assert_eq!(handle.join().unwrap().unwrap().0.messages.len(), 1);
    let attempts: i64 = env
        .engine
        .store
        .read(|tx| {
            Ok(tx
                .0
                .query_row("SELECT COUNT(*) FROM wake_attempts", [], |row| row.get(0))?)
        })
        .unwrap();
    assert_eq!(attempts, 0);
}

#[test]
fn worker_check_wait_is_refused_without_taking_the_slot() {
    let mut env = TestEnv::new();
    seed_run(&mut env);
    let now = Clock::now(env.clock.as_ref());
    env.engine.store.tx(|tx| {
        tx.0.execute("INSERT INTO tasks (id, run_id, spec, status, deps, created_by_terminal_id, created_at) VALUES ('task_000000000821', ?1, 'spec', 'dispatched', '[]', 'term-coordinator', ?2)", (RUN, now.to_rfc3339()))?;
        tx.0.execute("INSERT INTO dispatches (id, run_id, task_id, status, failure_count, depth, assignee_handle, assignee_terminal_id, agent_kind, created_at) VALUES ('ctx_000000000821', ?1, 'task_000000000821', 'dispatched', 0, 1, 'worker', 'term-worker', 'codex', ?2)", (RUN, now.to_rfc3339()))?;
        Ok(())
    }).unwrap();
    let output = env
        .pod(Some("term-worker"))
        .args(["check", "--wait", "--timeout-ms", "10", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("check --terminal"), "{stderr}");
    // The coordinator's slot is still free.
    waiting::check_wait(&env.engine, &[], 10).unwrap();
}

#[test]
fn huge_timeout_does_not_panic() {
    let mut env = TestEnv::new();
    seed_run(&mut env);
    let id = seed_question(&env);
    env.engine
        .store
        .tx(|tx| {
            questions::update_status(tx, &id, "answered")?;
            questions::update_answer_body(tx, &id, Some("yes"))
        })
        .unwrap();
    assert_eq!(
        waiting::ask_wait(&env.engine, &id, u64::MAX).unwrap(),
        AskOutcome::Answered("yes".into())
    );
}

#[test]
fn check_wait_timeout_with_only_non_matching_mail_reports_timed_out() {
    let mut env = TestEnv::new();
    seed_run(&mut env);
    insert_message(&env.engine, "msg_000000000825", "status");
    let output = env
        .pod(Some("term-coordinator"))
        .args([
            "check",
            "--wait",
            "--types",
            "decision_gate",
            "--timeout-ms",
            "50",
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["timedOut"], true);
    assert_eq!(value["count"], 1);
}

#[test]
fn check_wait_ticks_the_ladder_while_it_waits() {
    let mut env = TestEnv::new();
    seed_run(&mut env);
    env.engine.tick_interval = Duration::from_millis(200);
    // Mail the wait does not match, whose first nudge went out at commit and
    // was skipped (no agent in the coordinator's pane yet).
    insert_message(&env.engine, "msg_000000000826", "status");
    let mailbox = format!("run:{RUN}");
    pod::engine::wake::after_commit_nudge(&env.engine, &mailbox).unwrap();
    env.clock.advance(Duration::from_secs(61));
    let (_delivery, woke) =
        waiting::check_wait(&env.engine, &["decision_gate".into()], 1_500).unwrap();
    assert_eq!(woke, waiting::WokeOn::Timeout);
    // The timeout's consuming check clears the ladder, so read every row.
    let attempts = env
        .engine
        .store
        .read(|tx| wake_attempts::list(tx, 100))
        .unwrap();
    assert_eq!(attempts.len(), 2, "the waiter ticked once: {attempts:?}");
}

#[test]
fn check_wait_wakes_on_a_worker_that_goes_stale() {
    let mut env = TestEnv::new();
    seed_run(&mut env);
    env.engine.tick_interval = Duration::from_millis(200);
    let now = Clock::now(env.clock.as_ref());
    env.engine.store.tx(|tx| {
        tx.0.execute("INSERT INTO tasks (id, run_id, spec, status, deps, created_by_terminal_id, created_at) VALUES ('task_000000000827', ?1, 'spec', 'dispatched', '[]', 'term-coordinator', ?2)", (RUN, now.to_rfc3339()))?;
        tx.0.execute("INSERT INTO dispatches (id, run_id, task_id, status, failure_count, depth, assignee_handle, assignee_terminal_id, agent_kind, created_at) VALUES ('ctx_000000000827', ?1, 'task_000000000827', 'dispatched', 0, 1, 'worker', 'term-worker', 'codex', ?2)", (RUN, now.to_rfc3339()))?;
        Ok(())
    }).unwrap();
    // The worker is quiet for 11 minutes while the coordinator waits.
    let clock = env.clock.clone();
    let advancer = thread::spawn(move || {
        thread::sleep(Duration::from_millis(300));
        clock.advance(Duration::from_secs(11 * 60));
    });
    let started = Instant::now();
    let (_delivery, woke) = waiting::check_wait(&env.engine, &[], 5_000).unwrap();
    advancer.join().unwrap();
    assert_eq!(woke, waiting::WokeOn::Attention);
    assert!(started.elapsed() < Duration::from_secs(3));
    let run = RunId::from_str(RUN).unwrap();
    let projection = env
        .engine
        .store
        .read(|tx| {
            pod::engine::observe::project(
                tx,
                env.engine.clock.now(),
                &pod::ids::DispatchId::from_str("ctx_000000000827")?,
            )
        })
        .unwrap();
    assert!(
        projection
            .attention
            .categories
            .contains(&"stale".to_owned())
    );
    assert!(
        waiting::attention_set(&env.engine, &run)
            .unwrap()
            .contains("ctx_000000000827")
    );
    // Spec A5.3 notifies on `requiresAction: true` only; `stale` is false.
    assert!(
        !env.herdr
            .calls()
            .iter()
            .any(|call| call.starts_with("notification_show:")),
        "{:?}",
        env.herdr.calls()
    );
}

const WORKER_TASK: &str = "task_000000000823";
const WORKER_DISPATCH: &str = "ctx_000000000823";

fn seed_worker(env: &TestEnv) -> pod::ids::DispatchId {
    let now = env.engine.clock.now().to_rfc3339();
    env.engine.store.tx(|tx| {
        tx.0.execute("INSERT INTO tasks (id, run_id, spec, status, deps, created_by_terminal_id, created_at) VALUES (?1, ?2, 'spec', 'dispatched', '[]', 'term-coordinator', ?3)", (WORKER_TASK, RUN, &now))?;
        tx.0.execute("INSERT INTO dispatches (id, run_id, task_id, status, failure_count, depth, assignee_handle, created_at) VALUES (?1, ?2, ?3, 'dispatched', 0, 1, 'worker', ?4)", (WORKER_DISPATCH, RUN, WORKER_TASK, &now))?;
        tx.0.execute("INSERT INTO workers (dispatch_id, state, terminal_state, owns_pane, created_at, updated_at) VALUES (?1, 'ready', 'active', 1, ?2, ?2)", (WORKER_DISPATCH, &now))?;
        Ok(())
    }).unwrap();
    pod::ids::DispatchId::from_str(WORKER_DISPATCH).unwrap()
}

fn block_worker(env: &TestEnv, dispatch: &pod::ids::DispatchId) {
    env.engine
        .store
        .tx(|tx| {
            pod::engine::observe::record_herdr_state(
                tx,
                env.engine.clock.now(),
                dispatch,
                pod::herdr::AgentStatus::Blocked,
                &["approval".into()],
                Some("present"),
                None,
            )
        })
        .unwrap();
}

/// A waiter on the test's fake clock, so observations written at fake time
/// are fresh to it.
struct SharedClock(std::sync::Arc<pod::FakeClock>);

impl Clock for SharedClock {
    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        Clock::now(self.0.as_ref())
    }
}

fn reopen_on_fake_clock(env: &TestEnv) -> Engine {
    Engine::new(
        Store::open(&env.engine.paths).unwrap(),
        Box::new((*env.herdr).clone()),
        Box::new(SharedClock(env.clock.clone())),
        env.engine.config.clone(),
        env.engine.paths.clone(),
        env.engine.caller.clone(),
    )
}

fn notifications(env: &TestEnv) -> Vec<String> {
    env.herdr
        .calls()
        .into_iter()
        .filter(|call| call.starts_with("notification_show:"))
        .collect()
}

#[test]
fn a_newly_stuck_worker_notifies_once_and_an_unchanged_one_does_not() {
    let mut env = TestEnv::new();
    seed_run(&mut env);
    let dispatch = seed_worker(&env);
    let waiter = reopen_on_fake_clock(&env);
    // A wait that does not ask for escalations: the prompt pod stores for a
    // blocked worker is mail, which a wait on its type would return instead.
    let types = vec!["worker_done".to_owned()];
    let wait_types = types.clone();
    let handle = thread::spawn(move || waiting::check_wait(&waiter, &wait_types, 5_000));
    // The waiter must hold its slot and have taken its baseline before the
    // worker blocks, or the worker would already be known to it.
    let slot = env
        .engine
        .paths
        .state_dir
        .join(format!("check-wait-{RUN}.lock"));
    let deadline = Instant::now() + Duration::from_secs(4);
    while std::fs::metadata(&slot).map_or(true, |meta| meta.len() == 0) {
        assert!(Instant::now() < deadline, "the waiter never started");
        thread::sleep(Duration::from_millis(10));
    }
    thread::sleep(Duration::from_millis(300));
    block_worker(&env, &dispatch);
    let (delivery, woke) = handle.join().unwrap().unwrap();
    assert_eq!(woke, waiting::WokeOn::Attention);
    // The batch carries the prompt pod stored, though the wait did not ask
    // for its type.
    assert_eq!(delivery.messages.len(), 1);
    assert_eq!(delivery.messages[0].r#type, "escalation");
    let projection = env
        .engine
        .store
        .read(|tx| pod::engine::observe::project(tx, env.engine.clock.now(), &dispatch))
        .unwrap();
    assert!(projection.attention.requires_action);
    let action = projection.next_action.argv.join(" ");
    assert!(!action.is_empty());
    assert_eq!(
        notifications(&env),
        vec![format!("notification_show:wait:{action}:request")]
    );

    // The same attention is already known to the next wait: no second notice.
    let (_, woke) = waiting::check_wait(&env.engine, &types, 150).unwrap();
    assert_eq!(woke, waiting::WokeOn::Timeout);
    assert_eq!(notifications(&env).len(), 1);
}

/// Seen live 2026-10-16: a worker sat on a model-switch selector for ten
/// minutes because the only signal was an attention wake with count 0, which
/// a coordinator that looks at message counts discards. The prompt is mail
/// now: a wait on `escalation` returns with it, and it replays until
/// acknowledged.
#[test]
fn check_wait_returns_a_blocked_worker_as_mail() {
    let mut env = TestEnv::new();
    seed_run(&mut env);
    let dispatch = seed_worker(&env);
    block_worker(&env, &dispatch);
    let types = vec!["escalation".to_owned()];
    let (delivery, woke) = waiting::check_wait(&env.engine, &types, 1_000).unwrap();
    assert_eq!(woke, waiting::WokeOn::Message);
    assert_eq!(delivery.messages.len(), 1);
    let message = &delivery.messages[0];
    assert_eq!(message.r#type, "escalation");
    assert_eq!(message.from_handle, "pod");
    assert_eq!(message.payload.as_ref().unwrap()["reason"], "approval");

    // Not acknowledged: the next wait returns at once with the same message.
    let started = Instant::now();
    let (again, woke) = waiting::check_wait(&env.engine, &types, 5_000).unwrap();
    assert_eq!(woke, waiting::WokeOn::Message);
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(again.id, delivery.id);
    assert_eq!(again.message_ids, delivery.message_ids);

    // Acknowledged: the prompt is answered for the mailbox and the wait times out.
    pod::engine::deliveries::check_ack(&env.engine, &delivery.id).unwrap();
    let (empty, woke) = waiting::check_wait(&env.engine, &types, 150).unwrap();
    assert_eq!(woke, waiting::WokeOn::Timeout);
    assert!(empty.messages.is_empty());
}

/// Spec A2.1: `ask` ticks like `check --wait` while it blocks.
#[test]
fn ask_wait_ticks_the_ladder_while_it_waits() {
    let mut env = TestEnv::new();
    seed_run(&mut env);
    env.engine.tick_interval = Duration::from_millis(200);
    let id = seed_question(&env);
    // Mail in the Run mailbox whose first nudge was skipped at commit.
    insert_message(&env.engine, "msg_000000000829", "status");
    let mailbox = format!("run:{RUN}");
    pod::engine::wake::after_commit_nudge(&env.engine, &mailbox).unwrap();
    env.clock.advance(Duration::from_secs(61));
    assert!(matches!(
        waiting::ask_wait(&env.engine, &id, 1_000).unwrap(),
        AskOutcome::Timeout(_)
    ));
    let attempts = env
        .engine
        .store
        .read(|tx| wake_attempts::list(tx, 100))
        .unwrap();
    assert_eq!(attempts.len(), 2, "the ask ticked once: {attempts:?}");
}
