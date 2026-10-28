mod support;

use std::path::Path;
use std::str::FromStr;
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use pod::Clock;
use pod::engine::observe::{self, Projection};
use pod::engine::{stall, waiting};
use pod::herdr::AgentStatus;
use pod::ids::{DispatchId, RunId};
use pod::store::rows::messages;
use support::TestEnv;

const RUN: &str = "run_000000000901";
const TASK: &str = "task_000000000901";
const DISPATCH: &str = "ctx_000000000901";
const PANE: &str = "pane-stall-worker";

fn now(env: &TestEnv) -> DateTime<Utc> {
    Clock::now(env.clock.as_ref())
}

fn advance_minutes(env: &TestEnv, minutes: u64) {
    env.clock.advance(Duration::from_secs(minutes * 60));
}

/// A Run with one dispatched worker (`worker`, a Claude agent in pane
/// `PANE`) that was dispatched at the clock's current time.
fn seed(env: &mut TestEnv) -> DispatchId {
    env.as_caller("term-coordinator", std::process::id());
    let at = now(env).to_rfc3339();
    env.engine.store.tx(|tx| {
        tx.0.execute("INSERT INTO runs (id, objective, coordinator_terminal_id, coordinator_handle, consumer_generation, home_cwd, depth, created_at, updated_at) VALUES (?1, 'stall', 'term-coordinator', 'coordinator', 1, '/tmp', 0, ?2, ?2)", (RUN, &at))?;
        tx.0.execute("INSERT INTO tasks (id, run_id, spec, status, deps, created_by_terminal_id, created_at) VALUES (?1, ?2, 'spec', 'dispatched', '[]', 'term-coordinator', ?3)", (TASK, RUN, &at))?;
        tx.0.execute("INSERT INTO dispatches (id, run_id, task_id, status, failure_count, depth, assignee_handle, agent_kind, dispatched_at, created_at) VALUES (?1, ?2, ?3, 'dispatched', 0, 1, 'worker', 'claude', ?4, ?4)", (DISPATCH, RUN, TASK, &at))?;
        tx.0.execute("INSERT INTO workers (dispatch_id, state, terminal_state, pane_id, owns_pane, created_at, updated_at) VALUES (?1, 'ready', 'active', ?2, 1, ?3, ?3)", (DISPATCH, PANE, &at))?;
        Ok(())
    }).unwrap();
    env.herdr.set_screen(
        PANE,
        "claude> waiting for the test\n\n  2 shells still running\n",
    );
    DispatchId::from_str(DISPATCH).unwrap()
}

fn observe_state(env: &TestEnv, dispatch: &DispatchId, status: AgentStatus, needs: &[String]) {
    env.engine
        .store
        .tx(|tx| {
            observe::record_herdr_state(
                tx,
                now(env),
                dispatch,
                status,
                needs,
                Some("present"),
                None,
            )
        })
        .unwrap();
}

fn heartbeat(env: &TestEnv) {
    let at = now(env).to_rfc3339();
    env.engine
        .store
        .tx(|tx| {
            tx.0.execute(
                "UPDATE dispatches SET last_heartbeat_at = ?1 WHERE id = ?2",
                (&at, DISPATCH),
            )?;
            Ok(())
        })
        .unwrap();
}

fn sweep(env: &TestEnv) -> usize {
    stall::sweep(&env.engine, &RunId::from_str(RUN).unwrap())
        .unwrap()
        .len()
}

fn project(env: &TestEnv, dispatch: &DispatchId) -> Projection {
    env.engine
        .store
        .read(|tx| observe::project(tx, now(env), dispatch))
        .unwrap()
}

/// The stalled escalations in the order they were stored.
fn stalled_escalations(env: &TestEnv) -> Vec<messages::MessageRow> {
    let mut rows: Vec<messages::MessageRow> = env
        .engine
        .store
        .read(|tx| messages::list(tx, 1_000))
        .unwrap()
        .into_iter()
        .filter(|row| {
            row.r#type == "escalation"
                && row.from_handle == "pod"
                && row.payload.as_ref().and_then(|p| p["reason"].as_str()) == Some("stalled")
        })
        .collect();
    rows.sort_by_key(|row| row.sequence);
    rows
}

/// A transcript for the worker, modified `age_minutes` before the fake now.
fn give_transcript(env: &TestEnv, age_minutes: u64) -> std::path::PathBuf {
    let path = env.tmp.path().join("session.jsonl");
    std::fs::write(&path, "{}\n").unwrap();
    let modified: SystemTime = (now(env) - chrono::Duration::minutes(age_minutes as i64)).into();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(modified)
        .unwrap();
    attach(env, &path);
    path
}

fn attach(env: &TestEnv, path: &Path) {
    env.engine
        .store
        .tx(|tx| {
            tx.0.execute(
                "UPDATE workers SET transcript_proof = 'pod_assigned', session_id = 'sess-1', transcript_path = ?1 WHERE dispatch_id = ?2",
                (path.display().to_string(), DISPATCH),
            )?;
            Ok(())
        })
        .unwrap();
}

#[test]
fn an_idle_worker_gets_one_stalled_escalation_per_episode() {
    let mut env = TestEnv::new();
    let dispatch = seed(&mut env);
    let started = now(&env);
    observe_state(&env, &dispatch, AgentStatus::Idle, &[]);

    // Under the threshold: nothing.
    advance_minutes(&env, 7);
    assert_eq!(sweep(&env), 0);

    advance_minutes(&env, 2);
    assert_eq!(sweep(&env), 1);
    let escalations = stalled_escalations(&env);
    assert_eq!(escalations.len(), 1);
    let message = &escalations[0];
    assert_eq!(message.mailbox, format!("run:{RUN}"));
    assert_eq!(message.priority, "high");
    assert_eq!(message.subject, "Stalled: worker (9 min without progress)");
    assert_eq!(
        message.evidence.as_deref(),
        Some(format!("pane:{PANE}").as_str())
    );
    let payload = message.payload.as_ref().unwrap();
    assert_eq!(payload["dispatch_id"], DISPATCH);
    assert_eq!(payload["task_id"], TASK);
    assert_eq!(payload["source"], "pod");
    assert_eq!(payload["reason"], "stalled");
    assert_eq!(payload["minutes"], 9);
    assert_eq!(payload["activity"], "idle_at_prompt");
    assert_eq!(
        payload["since"],
        started.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    );
    assert!(payload["transcript_at"].is_null());
    assert!(payload["heartbeat_at"].is_null());
    let body = message.body.as_deref().unwrap();
    assert!(body.contains("2 shells still running"), "{body}");
    assert!(body.contains("Last screen lines:"), "{body}");
    assert!(body.contains("claude> waiting for the test"), "{body}");
    assert!(
        message
            .recommendation
            .as_deref()
            .unwrap()
            .contains(&format!("pod worker-read --dispatch {DISPATCH}"))
    );

    // The same episode is stored once.
    assert_eq!(sweep(&env), 0);
    assert_eq!(stalled_escalations(&env).len(), 1);

    let projection = project(&env, &dispatch);
    assert!(projection.attention.categories.contains(&"stalled".into()));
    assert!(projection.attention.requires_action);
    assert_eq!(projection.next_action.kind, "worker-read");
    assert_eq!(
        projection.next_action.argv,
        vec!["pod", "worker-read", "--dispatch", DISPATCH]
    );
}

#[test]
fn progress_ends_the_episode_and_a_later_stall_is_a_new_one() {
    let mut env = TestEnv::new();
    let dispatch = seed(&mut env);
    observe_state(&env, &dispatch, AgentStatus::Idle, &[]);
    advance_minutes(&env, 9);
    assert_eq!(sweep(&env), 1);

    advance_minutes(&env, 1);
    heartbeat(&env);
    let projection = project(&env, &dispatch);
    assert!(!projection.attention.categories.contains(&"stalled".into()));
    assert!(!projection.attention.requires_action);
    assert_eq!(sweep(&env), 0);
    assert_eq!(stalled_escalations(&env).len(), 1);

    let second_since = now(&env);
    advance_minutes(&env, 9);
    assert_eq!(sweep(&env), 1);
    let escalations = stalled_escalations(&env);
    assert_eq!(escalations.len(), 2);
    assert_eq!(
        escalations[1].payload.as_ref().unwrap()["since"],
        second_since.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    );
    assert_eq!(
        escalations[1].payload.as_ref().unwrap()["heartbeat_at"],
        second_since.to_rfc3339()
    );
    assert!(
        project(&env, &dispatch)
            .attention
            .categories
            .contains(&"stalled".into())
    );
}

#[test]
fn a_message_from_the_worker_is_progress() {
    let mut env = TestEnv::new();
    let dispatch = seed(&mut env);
    observe_state(&env, &dispatch, AgentStatus::Idle, &[]);
    advance_minutes(&env, 5);
    let at = now(&env).to_rfc3339();
    env.engine.store.tx(|tx| {
        tx.0.execute("INSERT INTO messages (id, run_id, mailbox, from_handle, subject, type, priority, read, created_at) VALUES ('msg_000000000902', ?1, ?2, 'worker', 'status', 'status', 'normal', 0, ?3)", (RUN, format!("run:{RUN}"), &at))?;
        Ok(())
    }).unwrap();
    advance_minutes(&env, 5);
    assert_eq!(sweep(&env), 0);
    advance_minutes(&env, 4);
    assert_eq!(sweep(&env), 1);
    assert!(
        project(&env, &dispatch)
            .attention
            .categories
            .contains(&"stalled".into())
    );
}

#[test]
fn a_working_worker_is_stalled_only_when_its_transcript_is_old() {
    // A transcript last written 9 minutes ago.
    let mut env = TestEnv::new();
    let dispatch = seed(&mut env);
    observe_state(&env, &dispatch, AgentStatus::Working, &[]);
    advance_minutes(&env, 9);
    let path = give_transcript(&env, 9);
    assert_eq!(sweep(&env), 1);
    let escalations = stalled_escalations(&env);
    let payload = escalations[0].payload.as_ref().unwrap();
    assert_eq!(payload["activity"], "working");
    assert_eq!(payload["minutes"], 9);
    assert!(payload["transcript_at"].is_string());
    drop(path);

    // A fresh transcript is progress.
    let mut env = TestEnv::new();
    let dispatch = seed(&mut env);
    observe_state(&env, &dispatch, AgentStatus::Working, &[]);
    advance_minutes(&env, 9);
    give_transcript(&env, 0);
    assert_eq!(sweep(&env), 0);
    assert!(
        !project(&env, &dispatch)
            .attention
            .categories
            .contains(&"stalled".into())
    );

    // Working with no transcript evidence is left alone.
    let mut env = TestEnv::new();
    let dispatch = seed(&mut env);
    observe_state(&env, &dispatch, AgentStatus::Working, &[]);
    advance_minutes(&env, 30);
    assert_eq!(sweep(&env), 0);
    assert!(stalled_escalations(&env).is_empty());
    assert!(
        !project(&env, &dispatch)
            .attention
            .categories
            .contains(&"stalled".into())
    );
}

#[test]
fn an_idle_worker_with_a_recent_transcript_is_not_stalled() {
    let mut env = TestEnv::new();
    let dispatch = seed(&mut env);
    observe_state(&env, &dispatch, AgentStatus::Idle, &[]);
    advance_minutes(&env, 20);
    give_transcript(&env, 2);
    assert_eq!(sweep(&env), 0);
    let _ = dispatch;
}

#[test]
fn zero_minutes_disables_detection_and_a_prompt_is_not_a_stall() {
    let mut env = TestEnv::new();
    let dispatch = seed(&mut env);
    observe_state(&env, &dispatch, AgentStatus::Idle, &[]);
    advance_minutes(&env, 30);
    env.engine.config.defaults.stall_minutes = 0;
    assert_eq!(sweep(&env), 0);
    env.engine.config.defaults.stall_minutes = 8;

    let mut env = TestEnv::new();
    let dispatch = seed(&mut env);
    observe_state(&env, &dispatch, AgentStatus::Blocked, &["approval".into()]);
    advance_minutes(&env, 30);
    assert_eq!(sweep(&env), 0);
    assert!(stalled_escalations(&env).is_empty());
    let projection = project(&env, &dispatch);
    assert!(!projection.attention.categories.contains(&"stalled".into()));
}

#[test]
fn stale_and_stalled_are_distinct_and_may_appear_together() {
    let mut env = TestEnv::new();
    let dispatch = seed(&mut env);
    observe_state(&env, &dispatch, AgentStatus::Idle, &[]);
    advance_minutes(&env, 9);
    assert_eq!(sweep(&env), 1);
    let projection = project(&env, &dispatch);
    assert!(!projection.attention.categories.contains(&"stale".into()));
    advance_minutes(&env, 2);
    let projection = project(&env, &dispatch);
    assert!(projection.attention.categories.contains(&"stale".into()));
    assert!(projection.attention.categories.contains(&"stalled".into()));
}

#[test]
fn a_wait_on_escalations_returns_the_stalled_message() {
    let mut env = TestEnv::new();
    let dispatch = seed(&mut env);
    observe_state(&env, &dispatch, AgentStatus::Idle, &[]);
    advance_minutes(&env, 9);
    env.engine.tick_interval = Duration::from_millis(50);
    let types = vec!["escalation".to_owned()];
    let (delivery, woke) = waiting::check_wait(&env.engine, &types, 3_000).unwrap();
    assert_eq!(woke, waiting::WokeOn::Message);
    assert_eq!(delivery.messages.len(), 1);
    let message = &delivery.messages[0];
    assert_eq!(message.from_handle, "pod");
    assert_eq!(message.payload.as_ref().unwrap()["reason"], "stalled");
}
