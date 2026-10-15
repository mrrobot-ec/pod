mod support;

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Mutex;
use std::time::Duration;

use chrono::Utc;
use pod::engine::reconcile;
use pod::engine::safe_input::{InputOutcome, type_and_submit, wait_until_ready};
use pod::engine::wake::{self, Trigger};
use pod::error::Result;
use pod::herdr::{
    AgentInfo, AgentStatus, EventStream, Herdr, HerdrEvent, HerdrInfo, PaneInfo, ProcessInfo,
    PromptOutcome, TabInfo, WorkspaceInfo, WorktreeCreate, WorktreeInfo, WorktreeListing,
};
use pod::ids::{DeliveryId, DispatchId, MessageId, RunId};
use pod::store::rows::{deliveries, messages, observations};
use pod::{Clock, ErrorCode, PodError, Store};
use serde_json::json;
use support::TestEnv;

const RUN: &str = "run_000000000801";
const TASK: &str = "task_000000000801";
const DISPATCH: &str = "ctx_000000000801";
const PANE: &str = "pane-wake";
const MAILBOX: &str = "dispatch:ctx_000000000801";
const COORDINATOR_PANE: &str = "pane-coordinator";

fn seed(env: &TestEnv) {
    let now = Clock::now(env.clock.as_ref());
    env.engine
        .store
        .tx(|tx| {
            tx.0.execute(
                "INSERT INTO runs (id, objective, coordinator_terminal_id, coordinator_handle, consumer_generation, home_cwd, depth, created_at, updated_at) VALUES (?1, 'wake', 'term-coordinator', 'coordinator', 1, '/tmp', 0, ?2, ?2)",
                (RUN, now.to_rfc3339()),
            )?;
            tx.0.execute(
                "INSERT INTO tasks (id, run_id, spec, status, deps, created_by_terminal_id, created_at) VALUES (?1, ?2, 'spec', 'dispatched', '[]', 'term-coordinator', ?3)",
                (TASK, RUN, now.to_rfc3339()),
            )?;
            tx.0.execute(
                "INSERT INTO dispatches (id, run_id, task_id, status, failure_count, depth, assignee_handle, assignee_terminal_id, agent_kind, created_at) VALUES (?1, ?2, ?3, 'dispatched', 0, 1, 'worker', 'term-worker', 'codex', ?4)",
                (DISPATCH, RUN, TASK, now.to_rfc3339()),
            )?;
            tx.0.execute(
                "INSERT INTO workers (dispatch_id, state, pane_id, terminal_id, terminal_state, owns_pane, created_at, updated_at) VALUES (?1, 'ready', ?2, 'term-worker', 'active', 1, ?3, ?3)",
                (DISPATCH, PANE, now.to_rfc3339()),
            )?;
            observations::insert(
                tx,
                &observations::ObservationRow {
                    dispatch_id: DispatchId::from_str(DISPATCH)?,
                    liveness: "live".into(),
                    activity: "idle_at_prompt".into(),
                    presence: Some("present".into()),
                    needs_input: json!({"count": 0, "reasons": []}),
                    evidence_source: "herdr_state".into(),
                    reason: None,
                    herdr_status: Some("idle".into()),
                    attention: None,
                    observed_at: now,
                },
            )?;
            Ok(())
        })
        .unwrap();
    env.herdr.add_pane(PANE, "term-worker", "ws");
    env.herdr.set_screen(PANE, "› ");
    env.herdr.set_agent(
        "worker",
        AgentInfo {
            name: "worker".into(),
            kind: "codex".into(),
            pane_id: PANE.into(),
            pid: 10,
            status: AgentStatus::Idle,
            session_id: None,
            session_path: None,
            needs_input: vec![],
            observed_at: Utc::now(),
        },
    );
}

fn add_message(env: &TestEnv, id: &str, kind: &str) {
    add_message_to(env, MAILBOX, id, kind);
}

fn add_message_to(env: &TestEnv, mailbox: &str, id: &str, kind: &str) {
    let now = Clock::now(env.clock.as_ref());
    env.engine
        .store
        .tx(|tx| {
            messages::insert(
                tx,
                &messages::MessageRow {
                    sequence: None,
                    id: MessageId::from_str(id)?,
                    run_id: RunId::from_str(RUN)?,
                    mailbox: mailbox.into(),
                    from_handle: "coordinator".into(),
                    subject: "mail".into(),
                    body: None,
                    r#type: kind.into(),
                    priority: "normal".into(),
                    thread_id: None,
                    payload: None,
                    read: false,
                    created_at: now,
                    delivered_at: None,
                    sender_terminal_id: Some("term-coordinator".into()),
                    rejected_reason: None,
                    evidence: None,
                    recommendation: None,
                },
            )?;
            Ok(())
        })
        .unwrap();
}

fn worker_agent(pane: &str, status: AgentStatus) -> AgentInfo {
    AgentInfo {
        name: "worker".into(),
        kind: "codex".into(),
        pane_id: pane.into(),
        pid: 10,
        status,
        session_id: None,
        session_path: None,
        needs_input: vec![],
        observed_at: Utc::now(),
    }
}

/// herdr's idle hook recorded the pane's screen 3 seconds ago, so the quiet
/// window is already proven when the send's own attempt runs.
fn prove_quiet(env: &TestEnv) {
    wake::note_screen(&env.engine, PANE);
    env.clock.advance(Duration::from_secs(3));
}

fn first_attempt(env: &TestEnv) -> (String, String) {
    prove_quiet(env);
    let row = wake::after_commit_nudge(&env.engine, MAILBOX)
        .unwrap()
        .unwrap();
    (row.outcome, row.detail.unwrap_or_default())
}

/// 8.2: an absent, stale, or mid-delivery recipient is a recorded `skipped`
/// attempt. A working recipient, an unproven quiet window, and a wake in
/// flight are "not now": nothing is recorded and nothing is typed.
#[test]
fn nudge_skipped_unless_present_idle_fresh_quiet_no_outstanding_delivery_no_inflight() {
    // Presence: herdr detects no agent in the pane and the stored evidence
    // has null presence.
    let env = TestEnv::new();
    seed(&env);
    env.herdr
        .set_agent("worker", worker_agent("elsewhere", AgentStatus::Idle));
    add_message(&env, "msg_000000000801", "status");
    env.engine
        .store
        .tx(|tx| {
            observations::insert(
                tx,
                &observations::ObservationRow {
                    dispatch_id: DispatchId::from_str(DISPATCH)?,
                    liveness: "unverifiable".into(),
                    activity: "idle_at_prompt".into(),
                    presence: None,
                    needs_input: json!({"count": 0, "reasons": []}),
                    evidence_source: "herdr_state".into(),
                    reason: Some("unknown".into()),
                    herdr_status: Some("unknown".into()),
                    attention: None,
                    observed_at: Clock::now(env.clock.as_ref()),
                },
            )?;
            Ok(())
        })
        .unwrap();
    let (outcome, detail) = first_attempt(&env);
    assert_eq!(outcome, "skipped");
    assert!(detail.contains("presence"), "{detail}");

    // Idle: herdr reports the agent working. Not now, nothing recorded.
    let env = TestEnv::new();
    seed(&env);
    env.herdr
        .set_agent("worker", worker_agent(PANE, AgentStatus::Working));
    add_message(&env, "msg_000000000801", "status");
    prove_quiet(&env);
    assert!(
        wake::after_commit_nudge(&env.engine, MAILBOX)
            .unwrap()
            .is_none()
    );
    assert_eq!(position(&env).attempts, 0);
    assert!(env.herdr.submissions().is_empty());

    // Fresh: no live evidence, and the stored idle observation is 16 s old.
    let env = TestEnv::new();
    seed(&env);
    env.herdr
        .set_agent("worker", worker_agent("elsewhere", AgentStatus::Idle));
    add_message(&env, "msg_000000000801", "status");
    env.clock.advance(Duration::from_secs(16));
    let (outcome, detail) = first_attempt(&env);
    assert_eq!(outcome, "skipped");
    assert!(detail.contains("stale"), "{detail}");

    // Quiet: the pane changed after its last snapshot, so the 3-second
    // window restarts and the send, which cannot wait, records nothing.
    let env = TestEnv::new();
    seed(&env);
    add_message(&env, "msg_000000000801", "status");
    wake::note_screen(&env.engine, PANE);
    env.herdr.set_screen(PANE, "› \nworking...");
    env.clock.advance(Duration::from_secs(3));
    assert!(
        wake::after_commit_nudge(&env.engine, MAILBOX)
            .unwrap()
            .is_none()
    );
    assert_eq!(position(&env).attempts, 0);
    assert!(env.herdr.submissions().is_empty());

    // Outstanding unacknowledged Delivery.
    let env = TestEnv::new();
    seed(&env);
    add_message(&env, "msg_000000000802", "status");
    let now = Clock::now(env.clock.as_ref());
    env.engine
        .store
        .tx(|tx| {
            deliveries::insert(
                tx,
                &deliveries::DeliveryRow {
                    id: DeliveryId::from_str("dlv_000000000801")?,
                    run_id: RunId::from_str(RUN)?,
                    mailbox: MAILBOX.into(),
                    consumer_generation: 1,
                    message_ids: vec![],
                    status: "outstanding".into(),
                    created_at: now,
                    acknowledged_at: None,
                },
            )?;
            Ok(())
        })
        .unwrap();
    let (outcome, detail) = first_attempt(&env);
    assert_eq!(outcome, "skipped");
    assert!(detail.contains("outstanding"), "{detail}");

    // In flight: another process holds the mailbox's wake lock.
    let env = TestEnv::new();
    seed(&env);
    add_message(&env, "msg_000000000803", "status");
    let lock = std::fs::File::create(
        env.engine
            .paths
            .state_dir
            .join("wake-dispatch_ctx_000000000801.lock"),
    )
    .unwrap();
    lock.try_lock().unwrap();
    prove_quiet(&env);
    assert!(
        wake::after_commit_nudge(&env.engine, MAILBOX)
            .unwrap()
            .is_none()
    );
    drop(lock);
    assert_eq!(position(&env).attempts, 0);
    assert!(env.herdr.submissions().is_empty());
}

fn position(env: &TestEnv) -> wake::Ladder {
    env.engine
        .store
        .read(|tx| wake::ladder_position(tx, MAILBOX))
        .unwrap()
}

#[test]
fn stale_wake_lock_from_a_killed_process_does_not_block() {
    let env = TestEnv::new();
    seed(&env);
    add_message(&env, "msg_000000000804", "status");
    // A process killed mid-wake leaves its lock file behind, unlocked.
    std::fs::write(
        env.engine
            .paths
            .state_dir
            .join("wake-dispatch_ctx_000000000801.lock"),
        b"",
    )
    .unwrap();
    let (outcome, _) = first_attempt(&env);
    assert_eq!(outcome, "delivered");
}

#[test]
fn nudge_uses_live_herdr_state_for_a_long_idle_worker() {
    // The worker went idle (and was observed) ten minutes ago; no event has
    // arrived since. herdr still reports it idle now, which is fresh evidence.
    let env = TestEnv::new();
    seed(&env);
    env.clock.advance(Duration::from_secs(600));
    add_message(&env, "msg_000000000805", "status");
    let (outcome, detail) = first_attempt(&env);
    assert_eq!(outcome, "delivered", "{detail}");
}

#[test]
fn done_agent_is_at_its_prompt() {
    let env = TestEnv::new();
    seed(&env);
    env.herdr
        .set_agent("worker", worker_agent(PANE, AgentStatus::Done));
    add_message(&env, "msg_000000000806", "status");
    let (outcome, detail) = first_attempt(&env);
    assert_eq!(outcome, "delivered", "{detail}");
}

#[test]
fn nudge_text_matches_spec() {
    let env = TestEnv::new();
    seed(&env);
    add_message(&env, "msg_000000000803", "status");
    add_message(&env, "msg_000000000804", "question");
    prove_quiet(&env);
    let row = wake::after_commit_nudge(&env.engine, MAILBOX)
        .unwrap()
        .unwrap();
    assert_eq!(row.outcome, "delivered");
    // No brief file exists for this seeded Dispatch, so the brief sentence is
    // omitted rather than naming a path that does not exist.
    assert_eq!(
        env.herdr.submissions()[0].1,
        "You have 2 orchestration message(s). Run `pod check --terminal worker`."
    );
}

#[test]
fn nudge_text_names_the_handle_and_the_brief_only_when_they_exist() {
    assert_eq!(
        wake::nudge_text(
            2,
            Some("codex-limiter"),
            Some(Path::new("/s/runs/r/preambles/ctx_1.txt"))
        ),
        "You have 2 orchestration message(s). Run `pod check --terminal codex-limiter`. Your brief: /s/runs/r/preambles/ctx_1.txt."
    );
    // The Run mailbox (the coordinator) has neither a worker handle nor a brief.
    assert_eq!(
        wake::nudge_text(1, None, None),
        "You have 1 orchestration message(s). Run `pod check`."
    );
}

#[test]
fn a_run_mailbox_nudge_has_no_brief_sentence() {
    let env = TestEnv::new();
    seed(&env);
    env.herdr
        .add_pane("pane-coordinator", "term-coordinator", "ws");
    env.herdr.set_screen("pane-coordinator", "› ");
    env.herdr.set_agent(
        "coordinator",
        worker_agent("pane-coordinator", AgentStatus::Idle),
    );
    let mailbox = format!("run:{RUN}");
    env.engine
        .store
        .tx(|tx| {
            messages::insert(
                tx,
                &messages::MessageRow {
                    sequence: None,
                    id: MessageId::from_str("msg_000000000851")?,
                    run_id: RunId::from_str(RUN)?,
                    mailbox: mailbox.clone(),
                    from_handle: "worker".into(),
                    subject: "done".into(),
                    body: None,
                    r#type: "status".into(),
                    priority: "normal".into(),
                    thread_id: None,
                    payload: None,
                    read: false,
                    created_at: Clock::now(env.clock.as_ref()),
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
    wake::note_screen(&env.engine, "pane-coordinator");
    env.clock.advance(Duration::from_secs(3));
    let row = wake::after_commit_nudge(&env.engine, &mailbox)
        .unwrap()
        .unwrap();
    assert_eq!(row.outcome, "delivered", "{:?}", row.detail);
    assert_eq!(
        env.herdr.submissions()[0].1,
        "You have 1 orchestration message(s). Run `pod check`."
    );
}

#[test]
fn a_worker_nudge_carries_its_brief_path() {
    let env = TestEnv::new();
    seed(&env);
    add_message(&env, "msg_000000000850", "status");
    let brief = pod::engine::lifecycle::brief_path(
        &env.engine.paths,
        &RunId::from_str(RUN).unwrap(),
        &DispatchId::from_str(DISPATCH).unwrap(),
    );
    std::fs::create_dir_all(brief.parent().unwrap()).unwrap();
    std::fs::write(&brief, "the brief").unwrap();
    let (outcome, detail) = first_attempt(&env);
    assert_eq!(outcome, "delivered", "{detail}");
    assert_eq!(
        env.herdr.submissions()[0].1,
        format!(
            "You have 1 orchestration message(s). Run `pod check --terminal worker`. Your brief: {}.",
            brief.display()
        )
    );
}

#[test]
fn safe_input_pastes_confirms_then_enters() {
    let herdr = MinimalHerdr::new(true);
    assert_eq!(
        type_and_submit(&herdr, PANE, "hello\nworld").unwrap(),
        InputOutcome::Delivered
    );
    assert_eq!(
        herdr.calls.lock().unwrap().as_slice(),
        [
            "agent_list",
            "pane_read",
            "pane_send_text",
            "pane_read",
            "pane_send_keys"
        ]
    );
}

#[test]
fn safe_input_long_paste_reads_enough_to_keep_prompt() {
    let text = (1..=60)
        .map(|line| format!("line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let herdr = MinimalHerdr::new(true).with_read_limit();
    assert_eq!(
        type_and_submit(&herdr, PANE, &text).unwrap(),
        InputOutcome::Delivered
    );
    assert!(herdr.sent_enter());
}

#[test]
fn safe_input_waits_briefly_for_redraw_without_resending() {
    let herdr =
        MinimalHerdr::screens("codex", "pane-lagging-redraw", "› ", None).with_lagging_reads(3);
    assert_eq!(
        type_and_submit(&herdr, "pane-lagging-redraw", "hello").unwrap(),
        InputOutcome::Delivered
    );
    assert_eq!(herdr.pastes(), 1);
    assert!(herdr.sent_enter());
}

#[test]
fn safe_input_redraw_wait_is_bounded_and_withholds_enter() {
    let herdr = MinimalHerdr::screens("codex", "pane-never-redraws", "› ", None)
        .with_lagging_reads(u32::MAX);
    let started = std::time::Instant::now();
    assert_eq!(
        type_and_submit(&herdr, "pane-never-redraws", "hello").unwrap(),
        InputOutcome::RenderedUnconfirmed
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    assert_eq!(herdr.pastes(), 1);
    assert!(!herdr.sent_enter());
}

#[test]
fn unconfirmed_paste_withholds_enter_and_later_attempts_skip_until_cleared() {
    let herdr = MinimalHerdr::new(false);
    assert_eq!(
        type_and_submit(&herdr, PANE, "hello").unwrap(),
        InputOutcome::RenderedUnconfirmed
    );
    assert!(
        !herdr
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call == "pane_send_keys")
    );
    assert!(matches!(
        type_and_submit(&herdr, PANE, "again").unwrap(),
        InputOutcome::Skipped(_)
    ));
}

#[test]
fn never_bare_enter() {
    let herdr = MinimalHerdr::new(true);
    assert!(matches!(
        type_and_submit(&herdr, PANE, "").unwrap(),
        InputOutcome::Skipped(_)
    ));
    assert!(
        !herdr
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call == "pane_send_keys")
    );
}

/// Five nudges typed a minute apart with no consuming check make the
/// mailbox unreachable; a trigger inside the minute, or a read-only one,
/// does nothing.
#[test]
fn ladder_max_5_60s_apart_eligible_triggers_only_then_unreachable() {
    let env = TestEnv::new();
    seed(&env);
    add_message(&env, "msg_000000000805", "status");
    let (outcome, _) = first_attempt(&env);
    assert_eq!(outcome, "delivered");
    assert!(
        wake::on_trigger(&env.engine, MAILBOX, Trigger::ReadOnlyCommand)
            .unwrap()
            .is_none()
    );
    env.clock.advance(Duration::from_secs(59));
    assert!(
        wake::on_trigger(&env.engine, MAILBOX, Trigger::MutatingCommand)
            .unwrap()
            .is_none(),
        "a retry within 60 s of the last attempt"
    );
    env.clock.advance(Duration::from_secs(1));
    for expected in 2..=5 {
        if expected > 2 {
            env.clock.advance(Duration::from_secs(60));
        }
        let row = wake::on_trigger(&env.engine, MAILBOX, Trigger::MutatingCommand)
            .unwrap()
            .unwrap();
        assert_eq!((row.attempt, row.outcome.as_str()), (expected, "delivered"));
    }
    assert_eq!(env.herdr.submissions().len(), 5);
    let ladder = env
        .engine
        .store
        .read(|tx| wake::ladder_position(tx, MAILBOX))
        .unwrap();
    assert_eq!(ladder.attempts, 5);
    assert!(ladder.exhausted);
    env.clock.advance(Duration::from_secs(60));
    assert!(
        wake::on_trigger(&env.engine, MAILBOX, Trigger::IdleHook)
            .unwrap()
            .is_none()
    );
}

#[test]
fn tick_runs_one_attempt_per_mailbox_and_honours_the_sixty_second_rule() {
    let env = TestEnv::new();
    seed(&env);
    add_message(&env, "msg_000000000810", "status");
    prove_quiet(&env);
    let run = RunId::from_str(RUN).unwrap();
    let first = wake::tick(&env.engine, &run, Trigger::WaitTick).unwrap();
    assert_eq!(first.mailboxes, 1);
    assert_eq!(first.attempts.len(), 1);
    // Inside the minute nothing happens; after it, attempt 2.
    env.clock.advance(Duration::from_secs(30));
    assert!(
        wake::tick(&env.engine, &run, Trigger::WaitTick)
            .unwrap()
            .attempts
            .is_empty()
    );
    env.clock.advance(Duration::from_secs(31));
    let third = wake::tick(&env.engine, &run, Trigger::WaitTick).unwrap();
    assert_eq!(third.attempts[0].attempt, 2);
    assert_eq!(third.attempts[0].mailbox, MAILBOX);
    // A read-only trigger never ticks.
    assert!(
        wake::tick(&env.engine, &run, Trigger::ReadOnlyCommand)
            .unwrap()
            .attempts
            .is_empty()
    );
}

/// A recipient pod could not find (stale evidence, no live agent) and an
/// input herdr failed are attempts: both count.
#[test]
fn ladder_counts_skipped_and_failed() {
    let env = TestEnv::new();
    seed(&env);
    add_message(&env, "msg_000000000806", "status");
    env.herdr.remove_agent("worker");
    env.clock.advance(Duration::from_secs(16));
    let row = wake::after_commit_nudge(&env.engine, MAILBOX)
        .unwrap()
        .unwrap();
    assert_eq!((row.attempt, row.outcome.as_str()), (1, "skipped"));
    assert!(row.detail.unwrap().contains("stale"));
    env.clock.advance(Duration::from_secs(60));
    env.herdr
        .set_agent("worker", worker_agent(PANE, AgentStatus::Idle));
    prove_quiet(&env);
    env.herdr
        .fail_agent_prompt_with(ErrorCode::HerdrUnreachable, "herdr did not answer");
    let row = wake::on_trigger(&env.engine, MAILBOX, Trigger::IdleHook)
        .unwrap()
        .unwrap();
    assert_eq!((row.attempt, row.outcome.as_str()), (2, "failed"));
    assert_eq!(position(&env).attempts, 2);
}

#[test]
fn new_mail_does_not_restart_ladder() {
    let env = TestEnv::new();
    seed(&env);
    add_message(&env, "msg_000000000807", "status");
    let (outcome, _) = first_attempt(&env);
    assert_eq!(outcome, "delivered");
    add_message(&env, "msg_000000000808", "status");
    assert!(
        wake::after_commit_nudge(&env.engine, MAILBOX)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        env.engine
            .store
            .read(|tx| wake::ladder_position(tx, MAILBOX))
            .unwrap()
            .attempts,
        1
    );
}

#[test]
fn consuming_check_clears_ladder() {
    let mut env = TestEnv::new();
    seed(&env);
    env.as_caller("term-worker", std::process::id());
    add_message(&env, "msg_000000000809", "status");
    wake::after_commit_nudge(&env.engine, MAILBOX).unwrap();
    pod::engine::deliveries::worker_check(&env.engine, "worker").unwrap();
    env.clock.advance(Duration::from_micros(1));
    assert_eq!(
        env.engine
            .store
            .read(|tx| wake::ladder_position(tx, MAILBOX))
            .unwrap()
            .attempts,
        0
    );
}

#[test]
fn read_only_commands_never_type() {
    let env = TestEnv::new();
    seed(&env);
    add_message(&env, "msg_00000000080a", "status");
    wake::on_trigger(&env.engine, MAILBOX, Trigger::ReadOnlyCommand).unwrap();
    assert!(env.herdr.submissions().is_empty());
}

/// The test's fake clock, shared with an engine built over the same store.
struct SameClock(std::sync::Arc<pod::FakeClock>);

impl Clock for SameClock {
    fn now(&self) -> chrono::DateTime<Utc> {
        Clock::now(self.0.as_ref())
    }
}

#[test]
fn wake_ladder_survives_restart() {
    let env = TestEnv::new();
    seed(&env);
    add_message(&env, "msg_00000000080b", "status");
    let (outcome, _) = first_attempt(&env);
    assert_eq!(outcome, "delivered");
    for _ in 0..3 {
        env.clock.advance(Duration::from_secs(60));
        wake::on_trigger(&env.engine, MAILBOX, Trigger::IdleHook)
            .unwrap()
            .unwrap();
    }
    // A new process over the same store, after herdr restarted and ran the
    // startup hook (Review Focus 5).
    let restarted = pod::Engine::new(
        Store::open(&env.engine.paths).unwrap(),
        Box::new((*env.herdr).clone()),
        Box::new(SameClock(env.clock.clone())),
        env.engine.config.clone(),
        env.engine.paths.clone(),
        None,
    );
    pod::engine::reconcile::startup(&restarted).unwrap();
    let ladder = restarted
        .store
        .read(|tx| wake::ladder_position(tx, MAILBOX))
        .unwrap();
    assert_eq!(ladder.attempts, 4);
    assert!(!ladder.exhausted);

    env.clock.advance(Duration::from_secs(60));
    let fifth = wake::on_trigger(&restarted, MAILBOX, Trigger::IdleHook)
        .unwrap()
        .unwrap();
    assert_eq!(fifth.attempt, 5);
    let projection = restarted
        .store
        .read(|tx| {
            pod::engine::observe::project(
                tx,
                Clock::now(env.clock.as_ref()),
                &DispatchId::from_str(DISPATCH)?,
            )
        })
        .unwrap();
    assert!(
        projection
            .attention
            .categories
            .contains(&"unreachable".into())
    );
    assert!(projection.attention.requires_action);
}

/// The bottom of real screens read with `herdr pane read` on herdr 0.9.3 (T8
/// review, isolated server): the input line sits above a border and footer,
/// and an empty input shows placeholder text (dim only in ANSI).
const CLAUDE_BORDER: &str = "───────────────────────────────────────────────────────";

fn claude_screen(input: &str) -> String {
    format!(
        " ▐▛███▛█   Claude Code v2.1.296\n\n\n{CLAUDE_BORDER}\n❯ {input}\n{CLAUDE_BORDER}\n\n  ⏵⏵ auto mode on (shift+tab to cycle) · ← 7 agents\n"
    )
}

fn codex_screen(input: &str) -> String {
    format!(
        "  >_ OpenAI Codex (v0.160.1)\n\n  The source is strong with this one.\n\n\n› {input}\n\n  GPT-6-Astra xhigh · /private/tmp/work\n  ← for agents · ? for shortcuts                ⚠ 1 warning · f2 to view\n"
    )
}

const NUDGE: &str = "You have 1 orchestration message(s). Run `pod check`.";

/// Grok's composer is a box whose bottom frame carries a label
/// (`╰──── Grok 4`, fixture captured 2026-10-14): a frame line under the
/// input line is not a continuation of the input.
#[test]
fn safe_input_treats_a_boxed_composer_footer_as_frame() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/grok-1.0.52-idle.txt");
    let idle = std::fs::read_to_string(fixture).unwrap();
    assert!(idle.contains("│ ❯"), "fixture holds the boxed input line");
    let herdr = MinimalHerdr::screens("grok", "pane-grok-idle", &idle, None);
    assert!(wait_until_ready(
        &herdr,
        "pane-grok-idle",
        Duration::from_millis(500),
        Duration::ZERO
    ));
}

/// Claude Code draws a prompt suggestion in the composer as dim text
/// (`ESC[2m ... ESC[0m`, seen live 2026-10-14: "yes, execute the plan"). It
/// is not input: readiness ignores dim text and the paste goes through.
#[test]
fn safe_input_ignores_a_dim_composer_suggestion() {
    let ghost = claude_screen("\x1b[0m\x1b[2myes, execute the plan\x1b[0m");
    let herdr = MinimalHerdr::screens(
        "claude",
        "pane-claude-ghost",
        &ghost,
        Some(claude_screen("[Pasted text #1 +1 lines]")),
    );
    assert_eq!(
        type_and_submit(&herdr, "pane-claude-ghost", "line one\nline two").unwrap(),
        InputOutcome::Delivered
    );
    assert!(herdr.sent_enter());
    // Typed text that is not dim is still input.
    let typed = MinimalHerdr::screens(
        "claude",
        "pane-claude-typed",
        &claude_screen("yes, execute the plan"),
        None,
    );
    assert_eq!(
        type_and_submit(&typed, "pane-claude-typed", "line one\nline two").unwrap(),
        InputOutcome::Skipped("pane input is not proven empty and ready".into())
    );
}

/// Claude Code collapses a multi-line paste into `[Pasted text #1 +N lines]`,
/// so a Claude composer that shows only the tail of the text is a truncated
/// paste (seen live 2026-10-14: the head was lost while the agent answered a
/// stray startup prompt), never a scrolled one. Enter is withheld.
#[test]
fn safe_input_withholds_enter_for_a_claude_paste_that_shows_only_its_tail() {
    let head = "Target: write the report. Change: survey every tool you can find and compare it with pod.\n";
    let tail = "Constraints: web search is required; cite URLs; no edits to the pod repository; do not run git commands that change state. Ownership: only the report file. Acceptance: the file exists with sections Tools, Comparison table, Verdict, Sources; the worker_done body gives the three strongest competitors in one sentence each.";
    assert!(
        tail.len() > 200,
        "the tail alone would satisfy the scrolled rule"
    );
    let text = format!("{head}{tail}");
    let herdr = MinimalHerdr::screens(
        "claude",
        "pane-claude-tail",
        &claude_screen(""),
        Some(claude_screen(tail)),
    );
    assert_eq!(
        type_and_submit(&herdr, "pane-claude-tail", &text).unwrap(),
        InputOutcome::RenderedUnconfirmed
    );
    assert!(!herdr.sent_enter());

    // The collapsed form is still accepted.
    let herdr = MinimalHerdr::screens(
        "claude",
        "pane-claude-collapsed",
        &claude_screen(""),
        Some(claude_screen("[Pasted text #1 +1 lines]")),
    );
    assert_eq!(
        type_and_submit(&herdr, "pane-claude-collapsed", &text).unwrap(),
        InputOutcome::Delivered
    );
}

/// Readiness must hold for the settle window before a paste: a prompt that
/// is idle for an instant between a stray startup prompt and its answer is
/// not ready.
#[test]
fn wait_until_ready_requires_an_idle_prompt_to_settle() {
    let herdr = MinimalHerdr::screens("claude", "pane-settle", &claude_screen(""), None);
    let started = std::time::Instant::now();
    assert!(wait_until_ready(
        &herdr,
        "pane-settle",
        Duration::from_secs(5),
        Duration::from_millis(300)
    ));
    assert!(started.elapsed() >= Duration::from_millis(300));
    let calls = herdr.calls.lock().unwrap().clone();
    assert!(
        calls.iter().filter(|call| *call == "agent_list").count() >= 2,
        "readiness is re-checked across the settle window: {calls:?}"
    );

    // A pane that never becomes ready times out as before.
    let busy = MinimalHerdr::screens("claude", "pane-busy", &claude_screen("typing"), None);
    assert!(!wait_until_ready(
        &busy,
        "pane-busy",
        Duration::from_millis(300),
        Duration::from_millis(100)
    ));
}

#[test]
fn safe_input_reads_live_claude_and_codex_screens() {
    let placeholder = "Try \"write a test for <filepath>\"";
    let preamble = (1..=30)
        .map(|n| format!("line {n:02} of the preamble"))
        .collect::<Vec<_>>()
        .join("\n");

    // Claude: empty input with its placeholder, nudge rendered on the line.
    let herdr = MinimalHerdr::screens(
        "claude",
        "pane-claude-1",
        &claude_screen(placeholder),
        Some(claude_screen(NUDGE)),
    );
    assert_eq!(
        type_and_submit(&herdr, "pane-claude-1", NUDGE).unwrap(),
        InputOutcome::Delivered
    );

    // Claude: someone's unsent text is in the input; nothing is pasted.
    let herdr = MinimalHerdr::screens(
        "claude",
        "pane-claude-2",
        &claude_screen("hello world"),
        None,
    );
    assert!(matches!(
        type_and_submit(&herdr, "pane-claude-2", NUDGE).unwrap(),
        InputOutcome::Skipped(_)
    ));
    assert!(!herdr.pasted());

    // Claude collapses a multi-line paste and counts its newlines.
    let herdr = MinimalHerdr::screens(
        "claude",
        "pane-claude-3",
        &claude_screen(placeholder),
        Some(claude_screen("[Pasted text #1 +29 lines]")),
    );
    assert_eq!(
        type_and_submit(&herdr, "pane-claude-3", &preamble).unwrap(),
        InputOutcome::Delivered
    );

    // A summary of a different size is not this paste: Enter is withheld.
    let herdr = MinimalHerdr::screens(
        "claude",
        "pane-claude-4",
        &claude_screen(placeholder),
        Some(claude_screen("[Pasted text #1 +3 lines]")),
    );
    assert_eq!(
        type_and_submit(&herdr, "pane-claude-4", &preamble).unwrap(),
        InputOutcome::RenderedUnconfirmed
    );
    assert!(!herdr.sent_enter());

    // Codex: empty input with its placeholder; a short multi-line paste is
    // shown in full with continuation lines indented.
    let indented = preamble.replace('\n', "\n  ");
    let herdr = MinimalHerdr::screens(
        "codex",
        "pane-codex-1",
        &codex_screen("Ask Codex to do anything"),
        Some(codex_screen(&indented)),
    );
    assert_eq!(
        type_and_submit(&herdr, "pane-codex-1", &preamble).unwrap(),
        InputOutcome::Delivered
    );

    // Codex: unsent text in the input.
    let herdr = MinimalHerdr::screens("codex", "pane-codex-2", &codex_screen("hello world"), None);
    assert!(matches!(
        type_and_submit(&herdr, "pane-codex-2", NUDGE).unwrap(),
        InputOutcome::Skipped(_)
    ));
    assert!(!herdr.pasted());

    // Codex collapses a paste over 1,000 characters and counts them.
    let long = format!("{preamble}\n{}", "x".repeat(1_200));
    let herdr = MinimalHerdr::screens(
        "codex",
        "pane-codex-3",
        &codex_screen("Ask Codex to do anything"),
        Some(codex_screen(&format!(
            "[Pasted Content {} chars]",
            long.chars().count()
        ))),
    );
    assert_eq!(
        type_and_submit(&herdr, "pane-codex-3", &long).unwrap(),
        InputOutcome::Delivered
    );

    // A blank first input line with text under it is not empty.
    let herdr = MinimalHerdr::screens(
        "codex",
        "pane-codex-4",
        &codex_screen("\n  typed below"),
        None,
    );
    assert!(matches!(
        type_and_submit(&herdr, "pane-codex-4", NUDGE).unwrap(),
        InputOutcome::Skipped(_)
    ));
}

/// Captured from Codex 0.160.1 on herdr 0.9.3 (pane.read, recent_unwrapped,
/// 80 lines) 100 ms after `pane.send_text` of a 60-line, 4.6 KB paste: the
/// composer scrolled to the cursor, so the marker line shows line 25 and the
/// box ends at line 60. The ten-researcher pilot of 2026-10-13 hit this on
/// every start and withheld Enter.
#[test]
fn safe_input_accepts_codex_composer_scrolled_to_the_cursor() {
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let screen =
        std::fs::read_to_string(fixtures.join("codex-0.160.1-scrolled-paste.txt")).unwrap();
    let text =
        std::fs::read_to_string(fixtures.join("codex-0.160.1-scrolled-paste.input.txt")).unwrap();
    assert!(
        screen.contains("› Line 25 of"),
        "fixture is the scrolled screen"
    );
    assert!(
        !screen.contains("Line 01 of"),
        "the first pasted line is off-screen"
    );
    let herdr = MinimalHerdr::screens(
        "codex",
        "pane-codex-scrolled",
        &codex_screen("Ask Codex to do anything"),
        Some(screen),
    );
    assert_eq!(
        type_and_submit(&herdr, "pane-codex-scrolled", &text).unwrap(),
        InputOutcome::Delivered
    );
    assert!(herdr.sent_enter());

    // The same screen is not some other paste: a text whose tail differs is
    // not confirmed, and Enter is withheld.
    let other = text.replace("Line 60 of", "Line 61 of");
    let herdr = MinimalHerdr::screens(
        "codex",
        "pane-codex-scrolled-2",
        &codex_screen("Ask Codex to do anything"),
        Some(std::fs::read_to_string(fixtures.join("codex-0.160.1-scrolled-paste.txt")).unwrap()),
    );
    assert_eq!(
        type_and_submit(&herdr, "pane-codex-scrolled-2", &other).unwrap(),
        InputOutcome::RenderedUnconfirmed
    );
    assert!(!herdr.sent_enter());

    // A short paste must be visible in full: its last line alone is not it.
    let herdr = MinimalHerdr::screens(
        "codex",
        "pane-codex-scrolled-3",
        &codex_screen("Ask Codex to do anything"),
        Some(codex_screen("line 3")),
    );
    assert_eq!(
        type_and_submit(&herdr, "pane-codex-scrolled-3", "line 1\nline 2\nline 3").unwrap(),
        InputOutcome::RenderedUnconfirmed
    );
}

/// Codex 0.160.1 soft-wraps long pasted lines inside its composer (a 110
/// column box breaks `pod send --from ... --task-id ...` across two screen
/// lines) and scrolls the box to the cursor, so the live worker preamble
/// was confirmed by neither the line-by-line nor the scrolled-tail rule
/// (2026-10-13). Whitespace-free comparison of the visible tail is.
#[test]
fn safe_input_accepts_codex_soft_wrapped_and_scrolled_preamble() {
    let preamble = (1..=40)
        .map(|n| format!(
            "/usr/local/bin/pod send --from codex-3e27 --type heartbeat --subject \"alive {n:02}\" --task-id task_be5fe7f06715 --dispatch-id ctx_2a5f3330e66f --phase \"<investigating|implementing>\""
        ))
        .collect::<Vec<_>>()
        .join("\n");
    // The composer shows lines 25 to 40, each wrapped at 100 columns with
    // the continuation indented, after a marker line holding a wrapped tail.
    let mut shown =
        vec!["--dispatch-id ctx_2a5f3330e66f --phase \"<investigating|implementing>\"".to_owned()];
    for line in preamble.lines().skip(24) {
        let (head, tail) = line.split_at(100);
        shown.push(head.to_owned());
        shown.push(format!("  {}", tail.trim_start()));
    }
    let screen = codex_screen(&shown.join("\n  "));
    let herdr = MinimalHerdr::screens(
        "codex",
        "pane-codex-wrapped",
        &codex_screen("Ask Codex to do anything"),
        Some(screen.clone()),
    );
    assert_eq!(
        type_and_submit(&herdr, "pane-codex-wrapped", &preamble).unwrap(),
        InputOutcome::Delivered
    );

    // The same screen is not a paste whose end differs.
    let other = format!("{preamble}\nnot what is on screen");
    let herdr = MinimalHerdr::screens(
        "codex",
        "pane-codex-wrapped-2",
        &codex_screen("Ask Codex to do anything"),
        Some(screen),
    );
    assert_eq!(
        type_and_submit(&herdr, "pane-codex-wrapped-2", &other).unwrap(),
        InputOutcome::RenderedUnconfirmed
    );
}

#[test]
fn safe_input_accepts_size_matched_pi_paste_summary() {
    let text = "line one\nline two";
    let herdr = MinimalHerdr::screens(
        "pi",
        "pane-pi-summary",
        "> ",
        Some(format!("> [Pasted Content {} chars]", text.len())),
    );
    assert_eq!(
        type_and_submit(&herdr, "pane-pi-summary", text).unwrap(),
        InputOutcome::Delivered
    );
}

#[test]
fn safe_input_withholds_enter_for_wrong_pi_paste_summary_count() {
    let herdr = MinimalHerdr::screens(
        "pi",
        "pane-pi-mismatch",
        "> ",
        Some("> [Pasted Content 3 chars]".into()),
    );
    assert_eq!(
        type_and_submit(&herdr, "pane-pi-mismatch", "sixteen chars!!!").unwrap(),
        InputOutcome::RenderedUnconfirmed
    );
    assert!(!herdr.sent_enter());
}

struct MinimalHerdr {
    screen: Mutex<String>,
    calls: Mutex<Vec<String>>,
    confirm: bool,
    kind: &'static str,
    pane: &'static str,
    /// The whole screen after a paste, when the agent redraws its input box
    /// instead of appending.
    rendered: Option<String>,
    enforce_read_limit: bool,
    /// Post-paste reads that still return the pre-paste screen, as when
    /// herdr acknowledges the write before the agent redraws.
    lagging_reads: Mutex<(u32, Option<String>)>,
}

impl MinimalHerdr {
    fn new(confirm: bool) -> Self {
        Self {
            screen: Mutex::new("› ".into()),
            calls: Mutex::new(Vec::new()),
            confirm,
            kind: "codex",
            pane: PANE,
            rendered: None,
            enforce_read_limit: false,
            lagging_reads: Mutex::new((0, None)),
        }
    }

    /// A live agent screen; each case uses its own pane so an unconfirmed
    /// paste in one case cannot mark another's input uncertain.
    fn screens(
        kind: &'static str,
        pane: &'static str,
        idle: &str,
        rendered: Option<String>,
    ) -> Self {
        Self {
            screen: Mutex::new(idle.into()),
            calls: Mutex::new(Vec::new()),
            confirm: true,
            kind,
            pane,
            rendered,
            enforce_read_limit: false,
            lagging_reads: Mutex::new((0, None)),
        }
    }

    fn with_read_limit(mut self) -> Self {
        self.enforce_read_limit = true;
        self
    }

    fn with_lagging_reads(self, reads: u32) -> Self {
        self.lagging_reads.lock().unwrap().0 = reads;
        self
    }

    fn pastes(&self) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| *call == "pane_send_text")
            .count()
    }

    fn sent_enter(&self) -> bool {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call == "pane_send_keys")
    }

    fn pasted(&self) -> bool {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call == "pane_send_text")
    }

    fn unsupported<T>(&self) -> Result<T> {
        Err(PodError::new(
            ErrorCode::HerdrError,
            "test herdr method is unsupported",
            &[],
        ))
    }
}

impl Herdr for MinimalHerdr {
    fn ping(&self) -> Result<HerdrInfo> {
        self.unsupported()
    }
    fn pane_current(&self) -> Result<PaneInfo> {
        self.unsupported()
    }
    fn pane_get(&self, _: &str) -> Result<Option<PaneInfo>> {
        self.unsupported()
    }
    fn pane_process_info(&self, _: &str) -> Result<ProcessInfo> {
        self.unsupported()
    }
    fn pane_read(&self, _: &str, lines: u32) -> Result<String> {
        self.calls.lock().unwrap().push("pane_read".into());
        let mut lagging = self.lagging_reads.lock().unwrap();
        if lagging.0 > 0
            && let Some(stale) = lagging.1.clone()
        {
            lagging.0 -= 1;
            return Ok(stale);
        }
        drop(lagging);
        let screen = self.screen.lock().unwrap().clone();
        if self.confirm {
            if self.enforce_read_limit {
                let rows = screen.lines().collect::<Vec<_>>();
                let start = rows.len().saturating_sub(lines as usize);
                Ok(rows[start..].join("\n"))
            } else {
                Ok(screen)
            }
        } else {
            Ok("› ".into())
        }
    }
    fn pane_send_text(&self, _: &str, text: &str) -> Result<()> {
        self.calls.lock().unwrap().push("pane_send_text".into());
        self.lagging_reads.lock().unwrap().1 = Some(self.screen.lock().unwrap().clone());
        match &self.rendered {
            Some(screen) => *self.screen.lock().unwrap() = screen.clone(),
            None => self.screen.lock().unwrap().push_str(text),
        }
        Ok(())
    }
    fn pane_send_keys(&self, _: &str, _: &[&str]) -> Result<()> {
        self.calls.lock().unwrap().push("pane_send_keys".into());
        Ok(())
    }
    fn pane_close(&self, _: &str) -> Result<()> {
        self.unsupported()
    }
    fn tab_create(
        &self,
        _: &str,
        _: &str,
        _: &Path,
        _: bool,
        _: &[(&str, &str)],
    ) -> Result<TabInfo> {
        self.unsupported()
    }
    fn tab_close(&self, _: &str) -> Result<()> {
        self.unsupported()
    }
    fn tab_rename(&self, _: &str, _: &str) -> Result<()> {
        self.unsupported()
    }
    fn workspace_get(&self, _: &str) -> Result<Option<WorkspaceInfo>> {
        self.unsupported()
    }
    fn workspace_close(&self, _: &str) -> Result<()> {
        self.unsupported()
    }
    fn workspace_for_folder(&self, _: &Path) -> Result<Option<String>> {
        self.unsupported()
    }
    fn workspace_create(&self, _: &Path, _: &str) -> Result<pod::herdr::WorkspaceCreated> {
        self.unsupported()
    }
    fn tab_pane_count(&self, _: &str) -> Result<Option<u32>> {
        self.unsupported()
    }
    fn workspace_report_metadata(&self, _: &str, _: Option<&str>) -> Result<()> {
        self.unsupported()
    }
    fn notification_show(&self, _: &str, _: Option<&str>, _: &str) -> Result<()> {
        self.unsupported()
    }
    fn worktree_create(&self, _: &WorktreeCreate) -> Result<WorktreeInfo> {
        self.unsupported()
    }
    fn worktree_list(&self, _cwd: Option<&Path>) -> Result<WorktreeListing> {
        Ok(WorktreeListing::default())
    }

    fn worktree_open(&self, _: &Path, _: &Path) -> Result<WorktreeInfo> {
        self.unsupported()
    }
    fn agent_start(&self, _: &str, _: &str, _: &str, _: &[String], _: u64) -> Result<AgentInfo> {
        self.unsupported()
    }
    fn agent_get(&self, _: &str) -> Result<Option<AgentInfo>> {
        self.unsupported()
    }
    fn agent_list(&self) -> Result<Vec<AgentInfo>> {
        self.calls.lock().unwrap().push("agent_list".into());
        Ok(vec![AgentInfo {
            name: "worker".into(),
            kind: self.kind.into(),
            pane_id: self.pane.into(),
            pid: 1,
            status: AgentStatus::Idle,
            session_id: None,
            session_path: Some(PathBuf::new()),
            needs_input: vec![],
            observed_at: Utc::now(),
        }])
    }
    fn agent_prompt(&self, _: &str, _: &str, _: Option<u64>) -> Result<PromptOutcome> {
        self.unsupported()
    }
    fn agent_read(&self, _: &str, _: u32) -> Result<String> {
        self.unsupported()
    }
    fn events_subscribe(&self, _: &[String]) -> Result<Box<dyn EventStream>> {
        self.unsupported()
    }
}

#[test]
fn preamble_is_injected_and_recorded_once() {
    let env = TestEnv::new();
    seed(&env);
    let dispatch = DispatchId::from_str(DISPATCH).unwrap();
    let preamble = "=== TASK ===\nDo the thing.\nReport with worker_done.";
    assert_eq!(
        pod::engine::safe_input::inject_preamble(&env.engine, &dispatch, "worker", preamble)
            .unwrap(),
        InputOutcome::Delivered
    );
    assert_eq!(env.herdr.submissions().len(), 1);
    assert!(matches!(
        pod::engine::safe_input::inject_preamble(&env.engine, &dispatch, "worker", preamble)
            .unwrap(),
        InputOutcome::Skipped(_)
    ));
    assert_eq!(env.herdr.submissions().len(), 1, "never typed twice");
    let worker = env
        .engine
        .store
        .read(|tx| pod::store::rows::workers::get(tx, &dispatch))
        .unwrap()
        .unwrap();
    assert_eq!(worker.preamble_input.as_deref(), Some("delivered"));

    // A pane a person has typed into: skipped, with the reason kept, before
    // herdr is asked to prompt or the fallback pastes.
    let env = TestEnv::new();
    seed(&env);
    env.herdr.set_screen(PANE, "› half-typed text");
    assert!(matches!(
        pod::engine::safe_input::inject_preamble(&env.engine, &dispatch, "worker", preamble)
            .unwrap(),
        InputOutcome::Skipped(_)
    ));
    let worker = env
        .engine
        .store
        .read(|tx| pod::store::rows::workers::get(tx, &dispatch))
        .unwrap()
        .unwrap();
    assert_eq!(worker.preamble_input.as_deref(), Some("skipped"));
    let last = worker.last_error.unwrap();
    assert!(last.contains("person is typing: half-typed text"), "{last}");
    assert!(env.herdr.submissions().is_empty());
}

/// Spec 16 against 8.2's 3-second quiet window: a send returns at once. With
/// no proof of quiet yet it records nothing but the screen; the next
/// eligible trigger, as soon as 3 s later, finds the pane unchanged and
/// types without waiting, as attempt 1.
#[test]
fn a_send_never_waits_for_the_quiet_window() {
    let env = TestEnv::new();
    seed(&env);
    add_message(&env, "msg_000000000807", "status");
    let started = std::time::Instant::now();
    assert!(
        wake::after_commit_nudge(&env.engine, MAILBOX)
            .unwrap()
            .is_none()
    );
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(position(&env).attempts, 0);
    assert!(env.herdr.submissions().is_empty());

    env.clock.advance(Duration::from_secs(5));
    let started = std::time::Instant::now();
    let row = wake::on_trigger(&env.engine, MAILBOX, Trigger::MutatingCommand)
        .unwrap()
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!((row.attempt, row.outcome.as_str()), (1, "delivered"));
}

/// herdr's hook process has no caller waiting on it, so with no snapshot it
/// still waits out the 3-second window before typing (spec 8.2).
#[test]
fn the_idle_hook_waits_out_the_quiet_window() {
    let env = TestEnv::new();
    seed(&env);
    add_message(&env, "msg_000000000808", "status");
    let started = std::time::Instant::now();
    let row = wake::on_trigger(&env.engine, MAILBOX, Trigger::IdleHook)
        .unwrap()
        .unwrap();
    assert!(started.elapsed() >= Duration::from_secs(3));
    assert_eq!(row.outcome, "delivered", "{row:?}");
}

#[test]
fn fake_agent_prompt_records_text_as_a_submission_and_can_fail() {
    let herdr = pod::herdr::fake::FakeHerdr::new();
    herdr.add_pane("p1", "t1", "ws");
    herdr.set_agent(
        "w1",
        AgentInfo {
            name: "w1".into(),
            kind: "claude".into(),
            pane_id: "p1".into(),
            pid: 7,
            status: AgentStatus::Idle,
            session_id: None,
            session_path: None,
            needs_input: Vec::new(),
            observed_at: Utc::now(),
        },
    );
    assert_eq!(
        herdr.agent_prompt("w1", "hello", Some(5000)).unwrap(),
        PromptOutcome::Submitted
    );
    assert!(herdr.calls().iter().any(|c| c == "agent_prompt:w1:hello"));
    assert_eq!(
        herdr.submissions(),
        vec![("p1".to_string(), "hello".to_string())]
    );
    herdr.fail_agent_prompt("agent.prompt is not available");
    assert!(herdr.agent_prompt("w1", "again", Some(5000)).is_err());
}

#[test]
fn a_nudge_is_prompted_not_pasted_and_a_stalled_nudge_counts_as_an_attempt() {
    let env = TestEnv::new();
    seed(&env);
    add_message(&env, "msg_000000000809", "status");
    let row = wake::on_trigger(&env.engine, MAILBOX, Trigger::IdleHook)
        .unwrap()
        .unwrap();
    assert_eq!(row.outcome, "delivered", "{row:?}");
    let calls = env.herdr.calls();
    assert!(
        calls
            .iter()
            .any(|c| c.starts_with("agent_prompt:worker:You have 1 orchestration message")),
        "{calls:?}"
    );
    assert!(!calls.iter().any(|c| c.starts_with("pane_send_text")));
    env.herdr.script_prompt_outcome(PromptOutcome::Stalled);
    env.clock.advance(Duration::from_secs(61));
    let row = wake::on_trigger(&env.engine, MAILBOX, Trigger::IdleHook)
        .unwrap()
        .unwrap();
    assert_eq!(
        (row.attempt, row.outcome.as_str()),
        (2, "rendered_unconfirmed")
    );
}

fn agent_of(kind: &str) -> AgentInfo {
    AgentInfo {
        name: "w".into(),
        kind: kind.into(),
        pane_id: "p".into(),
        pid: 1,
        status: AgentStatus::Idle,
        session_id: None,
        session_path: None,
        needs_input: Vec::new(),
        observed_at: Utc::now(),
    }
}

#[test]
fn foreign_input_is_the_text_a_person_typed() {
    use pod::engine::safe_input::foreign_input;
    assert_eq!(foreign_input(&agent_of("claude"), &claude_screen("")), None);
    assert_eq!(
        foreign_input(
            &agent_of("codex"),
            &codex_screen("Ask Codex to do anything")
        ),
        None
    );
    assert_eq!(
        foreign_input(&agent_of("claude"), &claude_screen("fix the tests first")).as_deref(),
        Some("fix the tests first")
    );
}

/// A dim prompt suggestion is the agent's, never a person typing (hotfix of
/// 2026-10-14); only the screen read that drops dim runs may feed
/// `foreign_input`.
#[test]
fn foreign_input_ignores_a_dim_suggestion_but_not_typed_text() {
    use pod::engine::safe_input::{foreign_input, screen};
    let ghost = claude_screen("\x1b[0m\x1b[2myes, execute the plan\x1b[0m");
    let herdr = MinimalHerdr::screens("claude", "pane-ghost", &ghost, None);
    let seen = screen(&herdr, "pane-ghost", 20).unwrap();
    assert_eq!(foreign_input(&agent_of("claude"), &seen), None);
    let typed = MinimalHerdr::screens(
        "claude",
        "pane-typed",
        &claude_screen("yes, execute the plan"),
        None,
    );
    let seen = screen(&typed, "pane-typed", 20).unwrap();
    assert_eq!(
        foreign_input(&agent_of("claude"), &seen).as_deref(),
        Some("yes, execute the plan")
    );
}

#[test]
fn a_prompt_is_withheld_while_a_person_is_typing_and_recorded_as_interruption() {
    let env = TestEnv::new();
    seed(&env);
    add_message(&env, "msg_000000000811", "status");
    let dispatch = DispatchId::from_str(DISPATCH).unwrap();
    env.herdr
        .set_screen(PANE, &codex_screen("let me finish this thought"));
    prove_quiet(&env);
    let row = wake::on_trigger(&env.engine, MAILBOX, Trigger::IdleHook)
        .unwrap()
        .unwrap();
    assert_eq!(row.outcome, "skipped", "{row:?}");
    assert!(row.detail.as_deref().unwrap().contains("person is typing"));
    assert!(
        !env.herdr
            .calls()
            .iter()
            .any(|c| c.starts_with("agent_prompt:"))
    );
    let view = pod::engine::lifecycle::show(&env.engine, &dispatch).unwrap();
    let projection = view.projection;
    assert!(
        projection
            .attention
            .categories
            .contains(&"interruption".to_string())
    );
    assert!(!projection.attention.requires_action);
    assert_eq!(projection.next_action.kind, "worker-read");
    assert_eq!(
        view.observation.unwrap().reason.as_deref(),
        Some("user_typing: let me finish this thought")
    );

    // The person deleted the text: the next read clears it and the nudge goes through.
    env.herdr.set_screen(PANE, &codex_screen(""));
    env.clock.advance(Duration::from_secs(61));
    prove_quiet(&env);
    let row = wake::on_trigger(&env.engine, MAILBOX, Trigger::IdleHook)
        .unwrap()
        .unwrap();
    assert_eq!(row.outcome, "delivered", "{row:?}");
    let view = pod::engine::lifecycle::show(&env.engine, &dispatch).unwrap();
    assert!(
        !view
            .projection
            .attention
            .categories
            .contains(&"interruption".to_string())
    );
}

/// Seen live 2026-10-14: a waiting coordinator ticked every minute against a
/// worker that was working for 20 minutes, and five `skipped` attempts made
/// the mailbox unreachable while the worker was fine. Finding the recipient
/// working is not an attempt, whatever the trigger (8.2).
#[test]
fn nothing_is_recorded_against_a_working_recipient() {
    let env = TestEnv::new();
    seed(&env);
    add_message(&env, "msg_000000000820", "status");
    prove_quiet(&env);
    let run = RunId::from_str(RUN).unwrap();
    env.herdr
        .set_agent("worker", worker_agent(PANE, AgentStatus::Working));
    for _ in 0..6 {
        env.clock.advance(Duration::from_secs(61));
        for trigger in [Trigger::WaitTick, Trigger::BarTick] {
            let report = wake::tick(&env.engine, &run, trigger).unwrap();
            assert_eq!(report.mailboxes, 1);
            assert!(report.attempts.is_empty(), "{:?}", report.attempts);
        }
    }
    let ladder = env
        .engine
        .store
        .read(|tx| wake::ladder_position(tx, MAILBOX))
        .unwrap();
    assert_eq!((ladder.attempts, ladder.exhausted), (0, false));
    assert!(env.herdr.submissions().is_empty());

    // Neither does a mutating command or an idle hook against the same busy
    // worker (the mail that lands during a turn).
    for trigger in [Trigger::MutatingCommand, Trigger::IdleHook] {
        assert!(
            wake::on_trigger(&env.engine, MAILBOX, trigger)
                .unwrap()
                .is_none()
        );
    }
    assert_eq!(position(&env).attempts, 0);

    // Idle again: the first attempt, at once.
    env.herdr
        .set_agent("worker", worker_agent(PANE, AgentStatus::Idle));
    let report = wake::tick(&env.engine, &run, Trigger::WaitTick).unwrap();
    assert_eq!(report.attempts.len(), 1);
    assert_eq!(report.attempts[0].attempt, 1);
    env.clock.advance(Duration::from_secs(30));
    assert!(
        wake::tick(&env.engine, &run, Trigger::BarTick)
            .unwrap()
            .attempts
            .is_empty()
    );
    env.clock.advance(Duration::from_secs(31));
    let report = wake::tick(&env.engine, &run, Trigger::BarTick).unwrap();
    assert_eq!(report.attempts[0].attempt, 2);
}

/// Review focus 2: the waiting coordinator's tick and the tab bar tick run
/// in two processes. The per-mailbox lock and the 60-second rule, checked
/// again under the lock, let exactly one of them type.
#[test]
fn a_waiting_coordinator_and_the_bar_tick_never_nudge_twice() {
    let env = TestEnv::new();
    seed(&env);
    add_message(&env, "msg_000000000821", "status");
    prove_quiet(&env);
    let bar = env.second_engine();
    let run = RunId::from_str(RUN).unwrap();
    // herdr holds each prompt while it waits for the turn, so the two
    // passes overlap.
    env.herdr.delay_agent_prompt(Duration::from_millis(300));
    let start = std::sync::Barrier::new(2);
    let (waiter, ticker) = std::thread::scope(|scope| {
        let waiter = scope.spawn(|| {
            start.wait();
            wake::tick(&env.engine, &run, Trigger::WaitTick).unwrap()
        });
        let ticker = scope.spawn(|| {
            start.wait();
            wake::tick(&bar, &run, Trigger::BarTick).unwrap()
        });
        (waiter.join().unwrap(), ticker.join().unwrap())
    });
    assert_eq!(env.herdr.submissions().len(), 1, "one nudge, typed once");
    assert_eq!(waiter.attempts.len() + ticker.attempts.len(), 1);
    let ladder = env
        .engine
        .store
        .read(|tx| wake::ladder_position(tx, MAILBOX))
        .unwrap();
    assert_eq!(ladder.attempts, 1);

    // Both again inside the minute: nothing.
    env.clock.advance(Duration::from_secs(30));
    assert!(
        wake::tick(&env.engine, &run, Trigger::WaitTick)
            .unwrap()
            .attempts
            .is_empty()
    );
    assert!(
        wake::tick(&bar, &run, Trigger::BarTick)
            .unwrap()
            .attempts
            .is_empty()
    );
    assert_eq!(env.herdr.submissions().len(), 1);
}

/// Review focus 2: the tab bar command has a 5-second timeout. Its nudge
/// never sleeps for the quiet window, never retries herdr, never waits for
/// a prompt before a fallback paste, and asks herdr for no turn wait.
#[test]
fn a_bar_tick_never_sleeps() {
    let mut env = TestEnv::new();
    env.engine.ready_wait = Duration::from_secs(3);
    seed(&env);
    add_message(&env, "msg_000000000822", "status");
    let run = RunId::from_str(RUN).unwrap();

    // No snapshot yet: the quiet window is not proven and the bar cannot
    // wait, so it records the screen and nothing else.
    let started = std::time::Instant::now();
    let report = wake::tick(&env.engine, &run, Trigger::BarTick).unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(report.attempts.is_empty(), "{report:?}");
    assert!(env.herdr.submissions().is_empty());

    // Proven quiet, and herdr still calls the agent inactive: no retry loop
    // and no readiness wait, the paste fallback runs at once.
    env.clock.advance(Duration::from_secs(61));
    env.herdr
        .fail_agent_prompt("agent worker is not an active named agent");
    let started = std::time::Instant::now();
    let report = wake::tick(&env.engine, &run, Trigger::BarTick).unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(report.attempts[0].outcome, "delivered", "{report:?}");
    assert_eq!(env.herdr.prompt_waits(), vec![None]);
}

/// A nudge from a command lets herdr wait for the turn but never sleeps in
/// pod: the launch-only retry and readiness wait do not apply.
#[test]
fn a_command_nudge_never_sleeps_in_pod() {
    let mut env = TestEnv::new();
    env.engine.ready_wait = Duration::from_secs(3);
    seed(&env);
    add_message(&env, "msg_000000000823", "status");
    prove_quiet(&env);
    env.herdr
        .fail_agent_prompt("agent worker is not an active named agent");
    let started = std::time::Instant::now();
    let row = wake::after_commit_nudge(&env.engine, MAILBOX)
        .unwrap()
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(row.outcome, "delivered", "{row:?}");
    assert_eq!(env.herdr.prompt_waits(), vec![Some(5000)]);
}

/// Spec A1.2: a transport error fails the input. The prompt may have gone in
/// before the connection broke, so a paste could type it twice.
#[test]
fn an_unreachable_herdr_fails_the_nudge_without_a_paste() {
    let env = TestEnv::new();
    seed(&env);
    add_message(&env, "msg_000000000824", "status");
    prove_quiet(&env);
    env.herdr
        .fail_agent_prompt_with(ErrorCode::HerdrUnreachable, "herdr did not answer");
    let row = wake::after_commit_nudge(&env.engine, MAILBOX)
        .unwrap()
        .unwrap();
    assert_eq!(row.outcome, "failed", "{row:?}");
    assert!(
        !env.herdr
            .calls()
            .iter()
            .any(|call| call.starts_with("pane_send_text"))
    );
}

fn interrupted(env: &TestEnv) -> bool {
    let dispatch = DispatchId::from_str(DISPATCH).unwrap();
    pod::engine::lifecycle::show(&env.engine, &dispatch)
        .unwrap()
        .projection
        .attention
        .categories
        .contains(&"interruption".to_string())
}

/// Spec 7.4: only a later read that finds the input bare clears
/// `interruption`. herdr state from a hook or a wake's refresh, an attempt
/// that never read the input, and a read that failed say nothing about the
/// input line. The recorded text is cut at 80 characters.
#[test]
fn interruption_clears_only_on_a_bare_read() {
    let env = TestEnv::new();
    seed(&env);
    add_message(&env, "msg_000000000825", "status");
    let dispatch = DispatchId::from_str(DISPATCH).unwrap();
    let typed = "x".repeat(100);
    env.herdr.set_screen(PANE, &codex_screen(&typed));
    prove_quiet(&env);
    wake::on_trigger(&env.engine, MAILBOX, Trigger::IdleHook)
        .unwrap()
        .unwrap();
    assert!(interrupted(&env));
    let reason = pod::engine::lifecycle::show(&env.engine, &dispatch)
        .unwrap()
        .observation
        .unwrap()
        .reason
        .unwrap();
    assert_eq!(reason, format!("user_typing: {}", "x".repeat(80)));

    // An idle hook's herdr state for the pane.
    let now = Clock::now(env.clock.as_ref());
    env.engine
        .store
        .tx(|tx| {
            pod::engine::observe::record_herdr_state(
                tx,
                now,
                &dispatch,
                AgentStatus::Idle,
                &[],
                Some("present"),
            )
        })
        .unwrap();
    assert!(interrupted(&env), "herdr state is not a read of the input");

    // The person keeps typing, so the screen changes: a command cannot prove
    // the quiet window, records nothing, and never reads the input.
    env.herdr
        .set_screen(PANE, &codex_screen(&format!("{typed} more")));
    env.clock.advance(Duration::from_secs(61));
    assert!(
        wake::on_trigger(&env.engine, MAILBOX, Trigger::MutatingCommand)
            .unwrap()
            .is_none()
    );
    assert!(interrupted(&env), "a trigger that read no input");

    // A screen without an input line proves nothing either.
    env.herdr.set_screen(PANE, "Working (12s)");
    env.clock.advance(Duration::from_secs(61));
    prove_quiet(&env);
    wake::on_trigger(&env.engine, MAILBOX, Trigger::IdleHook)
        .unwrap()
        .unwrap();
    assert!(interrupted(&env), "no input line on the screen");

    // The bare read clears it.
    env.herdr.set_screen(PANE, &codex_screen(""));
    env.clock.advance(Duration::from_secs(61));
    prove_quiet(&env);
    let row = wake::on_trigger(&env.engine, MAILBOX, Trigger::IdleHook)
        .unwrap()
        .unwrap();
    assert_eq!(row.outcome, "delivered", "{row:?}");
    assert!(!interrupted(&env));
}

/// Spec A1.2: herdr refuses a `blocked` agent before typing; the nudge is
/// `skipped` with `agent_blocked` and nothing is pasted.
#[test]
fn a_blocked_agent_skips_the_nudge_without_a_paste() {
    let env = TestEnv::new();
    seed(&env);
    add_message(&env, "msg_000000000826", "status");
    prove_quiet(&env);
    env.herdr.script_prompt_outcome(PromptOutcome::Blocked);
    let row = wake::after_commit_nudge(&env.engine, MAILBOX)
        .unwrap()
        .unwrap();
    assert_eq!(row.outcome, "skipped");
    assert!(row.detail.unwrap().contains("agent_blocked"));
    assert!(
        !env.herdr
            .calls()
            .iter()
            .any(|call| call.starts_with("pane_send_text"))
    );
}

/// Seen in the e2e suite once nudges went through `agent.prompt`: the worker
/// reads its mail while herdr still waits for the turn. The attempt is
/// stamped when it started, before that consume, so the consume clears it
/// and the next message still gets its commit-time nudge.
#[test]
fn a_consume_during_the_prompt_clears_the_attempt_it_answered() {
    let env = TestEnv::new();
    seed(&env);
    add_message(&env, "msg_000000000827", "status");
    prove_quiet(&env);
    let worker = env.second_engine();
    env.herdr.delay_agent_prompt(Duration::from_millis(300));
    std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(100));
            env.clock.advance(Duration::from_secs(1));
            pod::engine::deliveries::check_consume(&worker, MAILBOX).unwrap();
            env.clock.advance(Duration::from_secs(1));
        });
        let row = wake::after_commit_nudge(&env.engine, MAILBOX)
            .unwrap()
            .unwrap();
        assert_eq!(row.outcome, "delivered");
    });
    let ladder = env
        .engine
        .store
        .read(|tx| wake::ladder_position(tx, MAILBOX))
        .unwrap();
    assert_eq!(ladder.attempts, 0, "the consume cleared the ladder");
}

/// The coordinator's pane as herdr reports it, observed on the test clock.
fn coordinator_agent(env: &TestEnv, status: AgentStatus) {
    env.herdr
        .add_pane(COORDINATOR_PANE, "term-coordinator", "ws");
    env.herdr.set_agent(
        "coordinator",
        AgentInfo {
            name: "coordinator".into(),
            kind: "claude".into(),
            pane_id: COORDINATOR_PANE.into(),
            pid: 2,
            status,
            session_id: None,
            session_path: None,
            needs_input: vec![],
            observed_at: Clock::now(env.clock.as_ref()),
        },
    );
}

/// Seen live 2026-10-15: a `worker_done` landed while a person was typing in
/// the coordinator's pane. The commit could not prove the quiet window and
/// recorded a skipped attempt, and the coordinator's idle hook 52 seconds
/// later was refused by the 60-second rule. Nothing else triggered, so the
/// completion sat unread until the person asked. "Not now" is not an
/// attempt: nothing is recorded, and the next idle hook delivers.
#[test]
fn the_coordinators_next_idle_delivers_mail_that_landed_while_its_screen_was_changing() {
    let env = TestEnv::new();
    seed(&env);
    let mailbox = format!("run:{RUN}");
    coordinator_agent(&env, AgentStatus::Idle);
    wake::note_screen(&env.engine, COORDINATOR_PANE);
    env.clock.advance(Duration::from_secs(3));
    env.herdr
        .set_screen(COORDINATOR_PANE, "› is the advisor done ye");
    add_message_to(&env, &mailbox, "msg_000000000830", "worker_done");
    assert!(
        wake::after_commit_nudge(&env.engine, &mailbox)
            .unwrap()
            .is_none()
    );
    let ladder = env
        .engine
        .store
        .read(|tx| wake::ladder_position(tx, &mailbox))
        .unwrap();
    assert_eq!((ladder.attempts, ladder.last_at), (0, None));
    assert!(env.herdr.submissions().is_empty());

    // The person sends their message and the coordinator answers; 52 s after
    // the commit herdr reports it idle again and its screen has settled.
    env.clock.advance(Duration::from_secs(52));
    env.herdr.set_screen(COORDINATOR_PANE, "› ");
    coordinator_agent(&env, AgentStatus::Idle);
    reconcile::event(
        &env.engine,
        HerdrEvent::AgentStatusChanged {
            pane_id: COORDINATOR_PANE.into(),
            status: AgentStatus::Idle,
            needs_input: vec![],
        },
    )
    .unwrap();
    assert_eq!(env.herdr.submissions().len(), 1, "{:?}", env.herdr.calls());
    let ladder = env
        .engine
        .store
        .read(|tx| wake::ladder_position(tx, &mailbox))
        .unwrap();
    assert_eq!((ladder.attempts, ladder.exhausted), (1, false));
}

/// The same for a recipient that is working when the mail lands: the commit
/// records nothing, and the idle hook inside the minute delivers attempt 1.
#[test]
fn mail_that_lands_during_a_turn_is_delivered_at_the_recipients_next_idle() {
    let env = TestEnv::new();
    seed(&env);
    env.herdr
        .set_agent("worker", worker_agent(PANE, AgentStatus::Working));
    add_message(&env, "msg_000000000831", "status");
    assert!(
        wake::after_commit_nudge(&env.engine, MAILBOX)
            .unwrap()
            .is_none()
    );
    env.clock.advance(Duration::from_secs(20));
    env.herdr
        .set_agent("worker", worker_agent(PANE, AgentStatus::Idle));
    prove_quiet(&env);
    let row = wake::on_trigger(&env.engine, MAILBOX, Trigger::IdleHook)
        .unwrap()
        .unwrap();
    assert_eq!((row.attempt, row.outcome.as_str()), (1, "delivered"));
    assert_eq!(env.herdr.submissions().len(), 1);
}
