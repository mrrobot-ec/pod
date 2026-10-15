mod support;

use std::str::FromStr;

use pod::ErrorCode;
use pod::engine::deliveries;
use pod::engine::groups;
use pod::engine::messages::{self, SendInput};
use pod::engine::wake;
use pod::ids::{DeliveryId, DispatchId, RunId, TaskId};
use serde_json::{Value, json};
use support::TestEnv;

const RUN: &str = "run_000000000001";
const TASK: &str = "task_000000000001";
const DISPATCH: &str = "ctx_000000000001";
const TERMINAL: &str = "term-worker";
const HANDLE: &str = "worker-a";

fn seed_worker(env: &TestEnv, status: &str) {
    let now = env.clock.0.lock().unwrap().to_rfc3339();
    env.engine
        .store
        .tx(|tx| {
            tx.0.execute(
                "INSERT INTO runs (id, objective, coordinator_terminal_id, coordinator_handle, consumer_generation, home_cwd, depth, created_at, updated_at) VALUES (?1, 'test', 'term-coordinator', 'coordinator', 1, '/tmp', 0, ?2, ?2)",
                (RUN, &now),
            )?;
            tx.0.execute(
                "INSERT INTO tasks (id, run_id, spec, status, deps, created_by_terminal_id, created_at) VALUES (?1, ?2, 'spec', 'dispatched', '[]', 'term-coordinator', ?3)",
                (TASK, RUN, &now),
            )?;
            tx.0.execute(
                "INSERT INTO dispatches (id, run_id, task_id, status, failure_count, depth, assignee_handle, assignee_terminal_id, assignee_pid, agent_kind, created_at) VALUES (?1, ?2, ?3, ?4, 0, 1, ?5, ?6, ?7, 'codex', ?8)",
                (DISPATCH, RUN, TASK, status, HANDLE, TERMINAL, std::process::id(), &now),
            )?;
            tx.0.execute(
                "INSERT INTO workers (dispatch_id, state, worktree_path, terminal_id, terminal_state, owns_pane, created_at, updated_at) VALUES (?1, 'ready', '/tmp/work', ?2, 'active', 1, ?3, ?3)",
                (DISPATCH, TERMINAL, &now),
            )?;
            Ok(())
        })
        .unwrap();
}

fn input(message_type: &str) -> SendInput {
    SendInput {
        to: None,
        subject: "subject".into(),
        body: Some("body".into()),
        r#type: message_type.into(),
        priority: "normal".into(),
        thread_id: None,
        payload: None,
        task_id: Some(TaskId::from_str(TASK).unwrap()),
        dispatch_id: Some(DispatchId::from_str(DISPATCH).unwrap()),
        outcome: None,
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

fn message_count(env: &TestEnv, message_type: &str) -> i64 {
    env.engine
        .store
        .read(|tx| {
            Ok(tx.0.query_row(
                "SELECT COUNT(*) FROM messages WHERE type = ?1",
                [message_type],
                |row| row.get(0),
            )?)
        })
        .unwrap()
}

#[test]
fn send_default_mailbox_is_run_for_active_worker() {
    let mut env = TestEnv::new();
    seed_worker(&env, "dispatched");
    env.as_caller(TERMINAL, std::process::id());
    let receipt = messages::send(&env.engine, input("status")).unwrap();
    assert_eq!(receipt.mailbox, format!("run:{RUN}"));
}

#[test]
fn task_recipient_rejected() {
    let mut env = TestEnv::new();
    seed_worker(&env, "dispatched");
    env.as_caller(TERMINAL, std::process::id());
    let mut value = input("status");
    value.to = Some(format!("task:{TASK}"));
    assert_eq!(
        messages::send(&env.engine, value).unwrap_err().code,
        ErrorCode::InvalidRecipient
    );
}

#[test]
fn worker_done_requires_reason_on_failed() {
    let mut env = TestEnv::new();
    seed_worker(&env, "dispatched");
    env.as_caller(TERMINAL, std::process::id());
    let mut value = input("worker_done");
    value.outcome = Some("failed".into());
    assert_eq!(
        messages::send(&env.engine, value).unwrap_err().code,
        ErrorCode::ClosureReasonRequired
    );
    assert_eq!(message_count(&env, "status"), 1);
}

#[test]
fn target_required_for_handed_off_blocked_on_superseded() {
    for reason in ["handed_off", "blocked_on", "superseded"] {
        let mut env = TestEnv::new();
        seed_worker(&env, "dispatched");
        env.as_caller(TERMINAL, std::process::id());
        let mut value = input("worker_done");
        value.outcome = Some("failed".into());
        value.reason = Some(reason.into());
        assert_eq!(
            messages::send(&env.engine, value).unwrap_err().code,
            ErrorCode::ClosureTargetRequired
        );
    }
}

#[test]
fn unknown_target_accepted_with_warning() {
    let mut env = TestEnv::new();
    seed_worker(&env, "dispatched");
    env.as_caller(TERMINAL, std::process::id());
    let mut value = input("worker_done");
    value.outcome = Some("failed".into());
    value.reason = Some("blocked_on".into());
    value.reason_target = Some("task_ffffffffffff".into());
    let receipt = messages::send(&env.engine, value).unwrap();
    assert_eq!(receipt.warnings.len(), 1);
}

#[test]
fn success_defaults_no_follow_on() {
    let mut env = TestEnv::new();
    seed_worker(&env, "dispatched");
    env.as_caller(TERMINAL, std::process::id());
    let mut value = input("worker_done");
    value.outcome = Some("succeeded".into());
    messages::send(&env.engine, value).unwrap();
    let reason: String = env
        .engine
        .store
        .read(|tx| {
            Ok(tx.0.query_row(
                "SELECT termination_reason FROM dispatches WHERE id = ?1",
                [DISPATCH],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(reason, "no_follow_on");
}

#[test]
fn worker_done_duplicate_same_outcome_settled_duplicate_true() {
    let mut env = TestEnv::new();
    seed_worker(&env, "dispatched");
    env.as_caller(TERMINAL, std::process::id());
    let mut value = input("worker_done");
    value.outcome = Some("succeeded".into());
    messages::send(&env.engine, value.clone()).unwrap();
    let duplicate = messages::send(&env.engine, value).unwrap();
    assert!(duplicate.duplicate);
    assert!(duplicate.settled.is_some());
}

#[test]
fn conflicting_outcome_inactive_dispatch() {
    let mut env = TestEnv::new();
    seed_worker(&env, "dispatched");
    env.as_caller(TERMINAL, std::process::id());
    let mut first = input("worker_done");
    first.outcome = Some("succeeded".into());
    messages::send(&env.engine, first).unwrap();
    let mut conflict = input("worker_done");
    conflict.outcome = Some("failed".into());
    conflict.reason = Some("no_follow_on".into());
    assert_eq!(
        messages::send(&env.engine, conflict).unwrap_err().code,
        ErrorCode::InactiveDispatch
    );
}

#[test]
fn worker_done_after_stop_is_inactive_dispatch() {
    let mut env = TestEnv::new();
    seed_worker(&env, "dispatched");
    env.as_caller(TERMINAL, std::process::id());
    // worker-stop (T7) fences the active Dispatch with T1's `dispatches::fence`.
    let now = pod::Clock::now(env.clock.as_ref());
    let fenced = env
        .engine
        .store
        .tx(|tx| {
            pod::store::rows::dispatches::fence(
                tx,
                &DispatchId::from_str(DISPATCH).unwrap(),
                "failed",
                "canceled",
                now,
            )
        })
        .unwrap();
    assert!(fenced);
    let mut value = input("worker_done");
    value.outcome = Some("failed".into());
    value.reason = Some("canceled".into());
    assert_eq!(
        messages::send(&env.engine, value).unwrap_err().code,
        ErrorCode::InactiveDispatch
    );
    let state: (String, String, String) = env
        .engine
        .store
        .read(|tx| {
            Ok(tx.0.query_row(
                "SELECT d.status,d.termination_reason,t.status FROM dispatches d JOIN tasks t ON t.id = d.task_id WHERE d.id = ?1",
                [DISPATCH],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?)
        })
        .unwrap();
    assert_eq!(
        state,
        ("failed".into(), "canceled".into(), "dispatched".into())
    );
}

#[test]
fn rejected_lifecycle_stored_as_high_status_and_exits_1() {
    let mut env = TestEnv::new();
    seed_worker(&env, "dispatched");
    env.as_caller("term-wrong", std::process::id());
    let error = messages::send(&env.engine, input("heartbeat")).unwrap_err();
    assert_eq!(error.code, ErrorCode::CallerFenced);
    assert_eq!(error.exit_code(), 1);
    // The stored status is mail like any other: the error names it, and the
    // commit hook made the coordinator's first nudge attempt (recorded
    // `skipped` here, since no coordinator agent is detected).
    let mailbox = format!("run:{RUN}");
    assert_eq!(error.data["stored"]["mailbox"], mailbox);
    assert!(
        error.data["stored"]["messageId"]
            .as_str()
            .is_some_and(|id| id.starts_with("msg_"))
    );
    let ladder = env
        .engine
        .store
        .read(|tx| wake::ladder_position(tx, &mailbox))
        .unwrap();
    assert_eq!(ladder.attempts, 1);
    let stored: (String, String, String) = env
        .engine
        .store
        .read(|tx| {
            Ok(tx.0.query_row(
        "SELECT type, priority, rejected_reason FROM messages ORDER BY sequence DESC LIMIT 1", [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?)
        })
        .unwrap();
    assert_eq!(
        stored,
        ("status".into(), "high".into(), "caller_fenced".into())
    );

    let mut fenced = TestEnv::new();
    seed_worker(&fenced, "failed");
    fenced.as_caller(TERMINAL, std::process::id());
    let error = messages::send(&fenced.engine, input("heartbeat")).unwrap_err();
    assert_eq!(error.code, ErrorCode::CallerFenced);
    assert_eq!(message_count(&fenced, "status"), 1);
}

#[test]
fn heartbeat_updates_last_heartbeat_and_phase_only_for_active_dispatch() {
    let mut env = TestEnv::new();
    seed_worker(&env, "dispatched");
    env.as_caller(TERMINAL, std::process::id());
    let mut value = input("heartbeat");
    value.phase = Some("reviewing".into());
    messages::send(&env.engine, value).unwrap();
    let phase: String = env
        .engine
        .store
        .read(|tx| {
            Ok(tx.0.query_row(
                "SELECT last_phase FROM dispatches WHERE id = ?1",
                [DISPATCH],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(phase, "reviewing");

    env.engine
        .store
        .tx(|tx| {
            tx.0.execute(
                "UPDATE dispatches SET status = 'failed' WHERE id = ?1",
                [DISPATCH],
            )?;
            Ok(())
        })
        .unwrap();
    let mut inactive = input("heartbeat");
    inactive.phase = Some("should-not-win".into());
    assert_eq!(
        messages::send(&env.engine, inactive).unwrap_err().code,
        ErrorCode::CallerFenced
    );
    let phase: String = env
        .engine
        .store
        .read(|tx| {
            Ok(tx.0.query_row(
                "SELECT last_phase FROM dispatches WHERE id = ?1",
                [DISPATCH],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(phase, "reviewing");
}

#[test]
fn escalation_requires_evidence_and_recommendation() {
    let mut env = TestEnv::new();
    seed_worker(&env, "dispatched");
    env.as_caller(TERMINAL, std::process::id());
    assert_eq!(
        messages::send(&env.engine, input("escalation"))
            .unwrap_err()
            .code,
        ErrorCode::EscalationEvidenceRequired
    );

    let mut evidence_only = input("escalation");
    evidence_only.evidence = Some("path".into());
    assert_eq!(
        messages::send(&env.engine, evidence_only).unwrap_err().code,
        ErrorCode::EscalationEvidenceRequired
    );

    let mut recommendation_only = input("escalation");
    recommendation_only.recommendation = Some("continue".into());
    assert_eq!(
        messages::send(&env.engine, recommendation_only)
            .unwrap_err()
            .code,
        ErrorCode::EscalationEvidenceRequired
    );
}

#[test]
fn escalation_stamps_dispatch_id() {
    let mut env = TestEnv::new();
    seed_worker(&env, "dispatched");
    env.as_caller(TERMINAL, std::process::id());
    let mut value = input("escalation");
    value.evidence = Some("msg_000000000001".into());
    value.recommendation = Some("continue".into());
    messages::send(&env.engine, value).unwrap();
    let payload: String = env
        .engine
        .store
        .read(|tx| {
            Ok(tx.0.query_row(
                "SELECT payload FROM messages WHERE type = 'escalation'",
                [],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&payload).unwrap()["dispatch_id"],
        DISPATCH
    );
}

fn seed_messages(env: &TestEnv, count: usize) {
    let now = env.clock.0.lock().unwrap().to_rfc3339();
    env.engine.store.tx(|tx| {
        for index in 0..count {
            tx.0.execute(
                "INSERT INTO messages (id, run_id, mailbox, from_handle, subject, type, priority, read, created_at) VALUES (?1, ?2, ?3, 'sender', ?4, ?5, 'normal', 0, ?6)",
                (format!("msg_{index:012x}"), RUN, format!("run:{RUN}"), format!("subject-{index}"), if index % 2 == 0 { "status" } else { "handoff" }, &now),
            )?;
        }
        Ok(())
    }).unwrap();
}

#[test]
fn check_fifo_batch_of_50_replays_until_ack() {
    let env = TestEnv::new();
    seed_worker(&env, "dispatched");
    seed_messages(&env, 55);
    let first = deliveries::check_consume(&env.engine, &format!("run:{RUN}")).unwrap();
    let replay = deliveries::check_consume(&env.engine, &format!("run:{RUN}")).unwrap();
    assert_eq!(first.id, replay.id);
    assert_eq!(first.messages.len(), 50);
    assert_eq!(first.messages[0].subject, "subject-0");
}

#[test]
fn ack_then_next_batch() {
    let env = TestEnv::new();
    seed_worker(&env, "dispatched");
    seed_messages(&env, 55);
    let first = deliveries::check_consume(&env.engine, &format!("run:{RUN}")).unwrap();
    let acknowledged = deliveries::check_ack(&env.engine, &first.id).unwrap();
    assert_eq!(acknowledged.status, "acknowledged");
    let second = deliveries::check_consume(&env.engine, &format!("run:{RUN}")).unwrap();
    assert_eq!(second.messages.len(), 5);
}

#[test]
fn peek_and_all_filter_by_type_and_do_not_consume() {
    let env = TestEnv::new();
    seed_worker(&env, "dispatched");
    seed_messages(&env, 6);
    let types = vec!["handoff".to_owned()];
    assert_eq!(
        deliveries::check_peek(&env.engine, &format!("run:{RUN}"), &types)
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        deliveries::check_all(&env.engine, &format!("run:{RUN}"), &types)
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        env.engine
            .store
            .read(|tx| Ok(tx.0.query_row(
                "SELECT COUNT(*) FROM messages WHERE read = 1",
                [],
                |row| row.get::<_, i64>(0)
            )?))
            .unwrap(),
        0
    );
}

#[test]
fn worker_check_marks_read_and_consumer_fenced_after_generation_bump() {
    let mut env = TestEnv::new();
    seed_worker(&env, "dispatched");
    seed_messages(&env, 1);
    env.as_caller(TERMINAL, std::process::id());
    env.engine
        .store
        .tx(|tx| {
            tx.0.execute(
                "UPDATE messages SET mailbox = ?1 WHERE run_id = ?2",
                (format!("dispatch:{DISPATCH}"), RUN),
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        deliveries::worker_check(&env.engine, HANDLE).unwrap().len(),
        1
    );
    let now = env.clock.0.lock().unwrap().to_rfc3339();
    env.engine.store.tx(|tx| {
        tx.0.execute("UPDATE dispatches SET status = 'failed' WHERE id = ?1", [DISPATCH])?;
        tx.0.execute("UPDATE runs SET consumer_generation = consumer_generation + 1 WHERE id = ?1", [RUN])?;
        tx.0.execute(
            "INSERT INTO tasks (id, run_id, spec, status, deps, created_by_terminal_id, created_at) VALUES ('task_000000000002', ?1, 'replacement', 'dispatched', '[]', 'term-coordinator', ?2)",
            (RUN, &now),
        )?;
        tx.0.execute(
            "INSERT INTO dispatches (id, run_id, task_id, status, failure_count, depth, assignee_handle, assignee_terminal_id, assignee_pid, agent_kind, created_at) VALUES ('ctx_000000000002', ?1, 'task_000000000002', 'dispatched', 0, 1, ?2, 'term-replacement', ?3, 'codex', ?4)",
            (RUN, HANDLE, std::process::id(), &now),
        )?;
        Ok(())
    }).unwrap();
    assert_eq!(
        deliveries::worker_check(&env.engine, HANDLE)
            .unwrap_err()
            .code,
        ErrorCode::ConsumerFenced
    );
}

#[test]
fn group_resolution_all_idle_kind_worktree_one_row_per_recipient_shared_thread() {
    let mut env = TestEnv::new();
    seed_worker(&env, "dispatched");
    let now = env.clock.0.lock().unwrap().to_rfc3339();
    env.engine.store.tx(|tx| {
        for (index, kind, worktree, activity) in [
            (2, "claude", "/tmp/other", "working"),
            (3, "codex", "/tmp/work", "idle_at_prompt"),
        ] {
            let task = format!("task_{index:012x}");
            let dispatch = format!("ctx_{index:012x}");
            let handle = format!("worker-{index}");
            let terminal = format!("term-worker-{index}");
            tx.0.execute(
                "INSERT INTO tasks (id, run_id, spec, status, deps, created_by_terminal_id, created_at) VALUES (?1, ?2, 'spec', 'dispatched', '[]', 'term-coordinator', ?3)",
                (&task, RUN, &now),
            )?;
            tx.0.execute(
                "INSERT INTO dispatches (id, run_id, task_id, status, failure_count, depth, assignee_handle, assignee_terminal_id, assignee_pid, agent_kind, created_at) VALUES (?1, ?2, ?3, 'dispatched', 0, 1, ?4, ?5, ?6, ?7, ?8)",
                (&dispatch, RUN, &task, &handle, &terminal, std::process::id(), kind, &now),
            )?;
            tx.0.execute(
                "INSERT INTO workers (dispatch_id, state, worktree_path, terminal_id, terminal_state, owns_pane, created_at, updated_at) VALUES (?1, 'ready', ?2, ?3, 'active', 1, ?4, ?4)",
                (&dispatch, worktree, &terminal, &now),
            )?;
            tx.0.execute(
                "INSERT INTO observations (dispatch_id,liveness,activity,presence,needs_input,evidence_source,observed_at) VALUES (?1,'live',?2,'present','{\"count\":0,\"reasons\":[]}','herdr_state',?3)",
                (&dispatch, activity, &now),
            )?;
        }
        tx.0.execute(
            "INSERT INTO observations (dispatch_id,liveness,activity,presence,needs_input,evidence_source,observed_at) VALUES (?1,'live','idle_at_prompt','present','{\"count\":0,\"reasons\":[]}','herdr_state',?2)",
            (DISPATCH, &now),
        )?;
        let run = RunId::from_str(RUN).unwrap();
        assert_eq!(groups::resolve_group(tx, &run, "@all", "term-coordinator")?.len(), 3);
        assert_eq!(groups::resolve_group(tx, &run, "@idle", "term-coordinator")?.len(), 2);
        assert_eq!(groups::resolve_group(tx, &run, "@codex", "term-coordinator")?.len(), 2);
        assert_eq!(groups::resolve_group(tx, &run, "@worktree:/tmp/work", "term-coordinator")?.len(), 2);
        Ok(())
    }).unwrap();
    env.as_caller("term-coordinator", std::process::id());
    let mut value = input("status");
    value.dispatch_id = None;
    value.task_id = None;
    value.to = Some("@all".into());
    let receipt = messages::send(&env.engine, value).unwrap();
    assert_eq!(receipt.message_ids.len(), 3);
    assert_eq!(receipt.mailbox, "group:@all");
    let distinct_threads: i64 = env
        .engine
        .store
        .read(|tx| {
            Ok(tx.0.query_row(
                "SELECT COUNT(DISTINCT thread_id) FROM messages WHERE mailbox LIKE 'dispatch:%'",
                [],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(distinct_threads, 1);
}

#[test]
fn group_zero_recipients_error() {
    let mut env = TestEnv::new();
    seed_worker(&env, "dispatched");
    env.as_caller("term-coordinator", std::process::id());
    let mut value = input("status");
    value.dispatch_id = None;
    value.task_id = None;
    value.to = Some("@claude".into());
    assert_eq!(
        messages::send(&env.engine, value).unwrap_err().code,
        ErrorCode::InvalidRecipient
    );
}

#[test]
fn lifecycle_cannot_target_group() {
    let mut env = TestEnv::new();
    seed_worker(&env, "dispatched");
    env.as_caller(TERMINAL, std::process::id());
    let mut value = input("heartbeat");
    value.to = Some("@all".into());
    assert_eq!(
        messages::send(&env.engine, value).unwrap_err().code,
        ErrorCode::InvalidRecipient
    );
}

#[test]
fn priority_tag_never_reorders() {
    let mut env = TestEnv::new();
    seed_worker(&env, "dispatched");
    env.as_caller(TERMINAL, std::process::id());
    for (subject, priority) in [("first", "normal"), ("second", "urgent")] {
        let mut value = input("status");
        value.subject = subject.into();
        value.priority = priority.into();
        messages::send(&env.engine, value).unwrap();
    }
    let delivery = deliveries::check_consume(&env.engine, &format!("run:{RUN}")).unwrap();
    assert_eq!(
        delivery
            .messages
            .iter()
            .map(|message| message.subject.as_str())
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
}

#[test]
fn explicit_delivery_id_parses() {
    assert!(DeliveryId::from_str("dlv_000000000001").is_ok());
    assert!(RunId::from_str(RUN).is_ok());
    assert_eq!(json!({"ok": true})["ok"], true);
}

/// Seen live 2026-10-15: a worker's `nohup` heartbeat loop is reparented when
/// its shell exits, so the agent is no ancestor of its `pod` calls; it still
/// runs on the agent's terminal, which is the worker's identity. Needs a
/// controlling terminal, so it is skipped where the tests run without one.
#[test]
fn a_background_job_on_the_agents_terminal_is_accepted_with_a_warning() {
    let tty = std::process::Command::new("ps")
        .args(["-o", "tty=", "-p", &std::process::id().to_string()])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_default();
    if tty.is_empty() || tty.starts_with('?') {
        eprintln!("skipped: this test process has no controlling terminal");
        return;
    }
    let mut env = TestEnv::new();
    seed_worker(&env, "dispatched");
    // A child on the same terminal that is not an ancestor of this process.
    let mut agent = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    env.engine
        .store
        .tx(|tx| {
            tx.0.execute(
                "UPDATE dispatches SET assignee_pid = ?2 WHERE id = ?1",
                (DISPATCH, agent.id()),
            )?;
            Ok(())
        })
        .unwrap();
    env.as_caller(TERMINAL, std::process::id());
    let sent = messages::send(&env.engine, input("heartbeat"));
    agent.kill().unwrap();
    agent.wait().unwrap();
    let receipt = sent.unwrap();
    assert!(
        receipt
            .warnings
            .iter()
            .any(|warning| warning.contains("background job")),
        "{:?}",
        receipt.warnings
    );
    assert_eq!(message_count(&env, "heartbeat"), 1);
}

#[test]
fn worker_identity_changed_when_agent_process_is_not_an_ancestor() {
    let mut env = TestEnv::new();
    seed_worker(&env, "dispatched");
    // A live process that is not an ancestor of this test process stands in
    // for the agent that used to own the pane.
    // In its own session, so it shares no terminal with this process either.
    let mut command = std::process::Command::new("sleep");
    command.arg("30");
    // SAFETY: `setsid` is async-signal-safe and touches nothing of the
    // parent's state between fork and exec.
    unsafe {
        std::os::unix::process::CommandExt::pre_exec(&mut command, || {
            libc::setsid();
            Ok(())
        });
    }
    let mut stranger = command.spawn().unwrap();
    env.engine
        .store
        .tx(|tx| {
            tx.0.execute(
                "UPDATE dispatches SET assignee_pid = ?2 WHERE id = ?1",
                (DISPATCH, stranger.id()),
            )?;
            Ok(())
        })
        .unwrap();
    env.as_caller(TERMINAL, std::process::id());
    let error = messages::send(&env.engine, input("heartbeat")).unwrap_err();
    stranger.kill().unwrap();
    stranger.wait().unwrap();
    assert_eq!(error.code, ErrorCode::WorkerIdentityChanged);
    assert_eq!(message_count(&env, "status"), 1);
    assert_eq!(message_count(&env, "heartbeat"), 0);
}

#[test]
fn rejected_lifecycle_records_the_calling_terminal() {
    let mut env = TestEnv::new();
    seed_worker(&env, "dispatched");
    env.as_caller("term-wrong", std::process::id());
    messages::send(&env.engine, input("heartbeat")).unwrap_err();
    let (mailbox, terminal): (String, String) = env
        .engine
        .store
        .read(|tx| {
            Ok(tx.0.query_row(
                "SELECT mailbox, sender_terminal_id FROM messages WHERE type = 'status'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?)
        })
        .unwrap();
    assert_eq!(
        (mailbox.as_str(), terminal.as_str()),
        (format!("run:{RUN}").as_str(), "term-wrong")
    );
}

#[test]
fn refused_non_lifecycle_message_is_not_stored() {
    let mut env = TestEnv::new();
    seed_worker(&env, "dispatched");
    env.as_caller(TERMINAL, std::process::id());
    let mut value = input("status");
    value.from = Some("someone-else".into());
    assert_eq!(
        messages::send(&env.engine, value).unwrap_err().code,
        ErrorCode::CallerMismatch
    );
    assert_eq!(message_count(&env, "status"), 0);
}

#[test]
fn unbound_sender_inside_herdr_gets_invalid_recipient() {
    let mut env = TestEnv::new();
    seed_worker(&env, "dispatched");
    env.as_caller("term-unbound", std::process::id());
    let mut value = input("status");
    value.task_id = None;
    value.dispatch_id = None;
    assert_eq!(
        messages::send(&env.engine, value.clone()).unwrap_err().code,
        ErrorCode::InvalidRecipient
    );
    value.to = Some(format!("run:{RUN}"));
    assert_eq!(
        messages::send(&env.engine, value).unwrap().mailbox,
        format!("run:{RUN}")
    );
}

#[test]
fn failed_worker_done_closes_pending_questions() {
    let mut env = TestEnv::new();
    seed_worker(&env, "dispatched");
    env.as_caller(TERMINAL, std::process::id());
    pod::engine::questions::ask_store(&env.engine, "question?", &[], None).unwrap();
    let mut value = input("worker_done");
    value.outcome = Some("failed".into());
    value.reason = Some("escalated".into());
    messages::send(&env.engine, value).unwrap();
    let status: String = env
        .engine
        .store
        .read(|tx| {
            Ok(tx
                .0
                .query_row("SELECT status FROM questions", [], |row| row.get(0))?)
        })
        .unwrap();
    assert_eq!(status, "closed");
}

#[test]
fn worker_check_requires_the_owning_pane() {
    let mut env = TestEnv::new();
    seed_worker(&env, "dispatched");
    assert_eq!(
        deliveries::worker_check(&env.engine, HANDLE)
            .unwrap_err()
            .code,
        ErrorCode::NotInHerdr
    );
    env.as_caller("term-coordinator", std::process::id());
    assert_eq!(
        deliveries::worker_check(&env.engine, HANDLE)
            .unwrap_err()
            .code,
        ErrorCode::ConsumerFenced
    );
}

#[test]
fn bare_check_from_a_worker_resolves_to_its_handle() {
    let mut env = TestEnv::new();
    seed_worker(&env, "dispatched");
    env.as_caller(TERMINAL, std::process::id());
    assert_eq!(
        messages::caller_worker_handle(&env.engine)
            .unwrap()
            .as_deref(),
        Some(HANDLE)
    );
    env.as_caller("term-coordinator", std::process::id());
    assert_eq!(messages::caller_worker_handle(&env.engine).unwrap(), None);
}
