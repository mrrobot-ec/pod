mod support;

use std::path::Path;

use chrono::Utc;
use pod::engine::{runs, tasks};
use pod::ids::{DispatchId, TaskId};
use pod::{Clock, ErrorCode};
use serde_json::json;

use support::TestEnv;

fn run(env: &mut TestEnv) -> pod::engine::runs::RunRow {
    env.as_caller("term-coordinator", 10);
    let caller = env.engine.caller.as_ref().unwrap().clone();
    env.engine
        .store
        .tx(|tx| {
            runs::create(
                tx,
                env.clock.now(),
                "test run",
                &caller,
                Path::new("/tmp/repo"),
            )
        })
        .unwrap()
}

fn dispatch(env: &TestEnv, task: &TaskId, count: u32) -> DispatchId {
    let id = DispatchId::new();
    let now = env.clock.now().to_rfc3339();
    let run_id: String = env
        .engine
        .store
        .read(|tx| {
            Ok(tx.0.query_row(
                "SELECT run_id FROM tasks WHERE id = ?1",
                [task.as_str()],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    env.engine
        .store
        .tx(|tx| {
            tx.0.execute(
                "INSERT INTO dispatches (id, run_id, task_id, status, failure_count, depth, assignee_handle, assignee_terminal_id, assignee_pid, created_at, dispatched_at) VALUES (?1, ?2, ?3, 'dispatched', ?4, 0, 'worker', ?5, 11, ?6, ?6)",
                rusqlite::params![id.as_str(), run_id, task.as_str(), count, format!("term-{id}"), now],
            )?;
            tx.0.execute(
                "UPDATE tasks SET status = 'dispatched' WHERE id = ?1",
                [task.as_str()],
            )?;
            Ok(())
        })
        .unwrap();
    id
}

fn task_status(env: &TestEnv, task: &TaskId) -> String {
    env.engine
        .store
        .read(|tx| {
            Ok(tx.0.query_row(
                "SELECT status FROM tasks WHERE id = ?1",
                [task.as_str()],
                |row| row.get(0),
            )?)
        })
        .unwrap()
}

#[test]
fn task_created_ready_when_deps_completed_else_pending() {
    let mut env = TestEnv::new();
    let run = run(&mut env);
    let first = env
        .engine
        .store
        .tx(|tx| {
            tasks::create(
                tx,
                env.clock.now(),
                &run.id,
                "first",
                None,
                None,
                &[],
                None,
                "term-coordinator",
            )
        })
        .unwrap();
    assert_eq!(first.status, "ready");
    let dependent = env
        .engine
        .store
        .tx(|tx| {
            tasks::create(
                tx,
                env.clock.now(),
                &run.id,
                "dependent",
                None,
                None,
                std::slice::from_ref(&first.id),
                None,
                "term-coordinator",
            )
        })
        .unwrap();
    assert_eq!(dependent.status, "pending");
    env.engine
        .store
        .tx(|tx| tasks::update_status(tx, env.clock.now(), &first.id, "completed", None, false))
        .unwrap();
    let ready = env
        .engine
        .store
        .tx(|tx| {
            tasks::create(
                tx,
                env.clock.now(),
                &run.id,
                "ready",
                None,
                None,
                std::slice::from_ref(&first.id),
                None,
                "term-coordinator",
            )
        })
        .unwrap();
    assert_eq!(ready.status, "ready");
}

#[test]
fn promotion_on_dependency_completion() {
    let mut env = TestEnv::new();
    let run = run(&mut env);
    let dependency = env
        .engine
        .store
        .tx(|tx| {
            tasks::create(
                tx,
                env.clock.now(),
                &run.id,
                "dep",
                None,
                None,
                &[],
                None,
                "term-coordinator",
            )
        })
        .unwrap();
    let dependant = env
        .engine
        .store
        .tx(|tx| {
            tasks::create(
                tx,
                env.clock.now(),
                &run.id,
                "child",
                None,
                None,
                std::slice::from_ref(&dependency.id),
                None,
                "term-coordinator",
            )
        })
        .unwrap();
    let promoted = env
        .engine
        .store
        .tx(|tx| {
            tasks::update_status(
                tx,
                env.clock.now(),
                &dependency.id,
                "completed",
                Some(json!({"ok": true})),
                false,
            )?;
            tasks::promote_dependants(tx, env.clock.now(), &dependency.id)
        })
        .unwrap();
    assert!(promoted.contains(&dependant.id));
    assert_eq!(task_status(&env, &dependant.id), "ready");
}

#[test]
fn failed_dependency_keeps_dependants_pending_with_waiting_attention() {
    let mut env = TestEnv::new();
    let run = run(&mut env);
    let dependency = env
        .engine
        .store
        .tx(|tx| {
            tasks::create(
                tx,
                env.clock.now(),
                &run.id,
                "dep",
                None,
                None,
                &[],
                None,
                "term-coordinator",
            )
        })
        .unwrap();
    let dependant = env
        .engine
        .store
        .tx(|tx| {
            tasks::create(
                tx,
                env.clock.now(),
                &run.id,
                "child",
                None,
                None,
                std::slice::from_ref(&dependency.id),
                None,
                "term-coordinator",
            )
        })
        .unwrap();
    env.engine
        .store
        .tx(|tx| tasks::update_status(tx, env.clock.now(), &dependency.id, "failed", None, false))
        .unwrap();
    assert_eq!(task_status(&env, &dependant.id), "pending");
    let waiting = env
        .engine
        .store
        .read(|tx| tasks::waiting_on_failed_dependency(tx, &dependant.id))
        .unwrap();
    assert!(waiting);
}

#[test]
fn breaker_counts_failed_reports_cumulatively_across_retries() {
    let mut env = TestEnv::new();
    let run = run(&mut env);
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
                "term-coordinator",
            )
        })
        .unwrap();
    for expected in 1..=3 {
        let attempt = dispatch(&env, &task.id, expected - 1);
        let settled = env
            .engine
            .store
            .tx(|tx| {
                tasks::settle_dispatch(
                    tx,
                    env.clock.now(),
                    &attempt,
                    tasks::Outcome::Failed,
                    "blocked_on",
                    None,
                )
            })
            .unwrap();
        assert_eq!(settled.failure_count, expected);
        if expected < 3 {
            assert_eq!(settled.dispatch_status, "failed");
            assert_eq!(settled.task_status, "ready");
        } else {
            assert_eq!(settled.dispatch_status, "circuit_broken");
            assert_eq!(settled.task_status, "failed");
        }
    }
}

#[test]
fn denied_and_canceled_fail_task_directly_without_increment() {
    for reason in ["denied", "canceled"] {
        let mut env = TestEnv::new();
        let run = run(&mut env);
        let task = env
            .engine
            .store
            .tx(|tx| {
                tasks::create(
                    tx,
                    env.clock.now(),
                    &run.id,
                    reason,
                    None,
                    None,
                    &[],
                    None,
                    "term-coordinator",
                )
            })
            .unwrap();
        let attempt = dispatch(&env, &task.id, 2);
        let settled = env
            .engine
            .store
            .tx(|tx| {
                tasks::settle_dispatch(
                    tx,
                    env.clock.now(),
                    &attempt,
                    tasks::Outcome::Failed,
                    reason,
                    None,
                )
            })
            .unwrap();
        assert_eq!(settled.failure_count, 2);
        assert_eq!(settled.dispatch_status, "failed");
        assert_eq!(settled.task_status, "failed");
    }
}

#[test]
fn task_update_guards() {
    let mut env = TestEnv::new();
    let run = run(&mut env);
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
                "term-coordinator",
            )
        })
        .unwrap();
    let err = env
        .engine
        .store
        .tx(|tx| tasks::update_status(tx, Utc::now(), &task.id, "dispatched", None, false))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::TaskGuardRefused);
    let attempt = dispatch(&env, &task.id, 0);
    env.engine
        .store
        .tx(|tx| {
            let now = env.clock.now().to_rfc3339();
            tx.0.execute(
                "INSERT INTO workers (dispatch_id, state, owns_pane, terminal_state, created_at, updated_at) VALUES (?1, 'ready', 1, 'active', ?2, ?2)",
                rusqlite::params![attempt.as_str(), now],
            )?;
            tx.0.execute(
                "INSERT INTO observations (dispatch_id, liveness, activity, presence, needs_input, evidence_source, observed_at) VALUES (?1, 'live', 'working', 'present', '{\"count\":0,\"reasons\":[]}', 'herdr_state', ?2)",
                rusqlite::params![attempt.as_str(), now],
            )?;
            Ok(())
        })
        .unwrap();
    let err = env
        .engine
        .store
        .tx(|tx| tasks::update_status(tx, Utc::now(), &task.id, "completed", None, true))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::TaskGuardRefused);
    env.engine
        .store
        .tx(|tx| {
            tasks::settle_dispatch(
                tx,
                Utc::now(),
                &attempt,
                tasks::Outcome::Succeeded,
                "no_follow_on",
                None,
            )
        })
        .unwrap();
    let still_live = env
        .engine
        .store
        .read(|tx| tasks::has_live_worker(tx, &task.id))
        .unwrap();
    assert!(still_live);
    let err = env
        .engine
        .store
        .tx(|tx| tasks::update_status(tx, Utc::now(), &task.id, "failed", None, still_live))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::TaskGuardRefused);
}

#[test]
fn circuit_broken_dispatch_can_be_manually_retried() {
    let mut env = TestEnv::new();
    let run = run(&mut env);
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
                "term-coordinator",
            )
        })
        .unwrap();
    let attempt = DispatchId::new();
    env.engine.store.tx(|tx| {
        tx.0.execute(
            "INSERT INTO dispatches (id, run_id, task_id, status, failure_count, depth, created_at) VALUES (?1, ?2, ?3, 'circuit_broken', 3, 0, ?4)",
            rusqlite::params![attempt.as_str(), run.id.as_str(), task.id.as_str(), env.clock.now().to_rfc3339()],
        )?;
        tx.0.execute("UPDATE tasks SET status='failed' WHERE id=?1", [task.id.as_str()])?;
        Ok(())
    }).unwrap();
    env.engine
        .store
        .read(|tx| tasks::startable(tx, &task.id, Some(&attempt)))
        .unwrap();
}

#[test]
fn startable_refusal_carries_status_and_unmet_dependencies() {
    let mut env = TestEnv::new();
    let run = run(&mut env);
    let first = env
        .engine
        .store
        .tx(|tx| {
            tasks::create(
                tx,
                env.clock.now(),
                &run.id,
                "first",
                None,
                None,
                &[],
                None,
                "term-coordinator",
            )
        })
        .unwrap();
    let second = env
        .engine
        .store
        .tx(|tx| {
            tasks::create(
                tx,
                env.clock.now(),
                &run.id,
                "second",
                None,
                None,
                std::slice::from_ref(&first.id),
                None,
                "term-coordinator",
            )
        })
        .unwrap();
    let err = env
        .engine
        .store
        .read(|tx| tasks::startable(tx, &second.id, None))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::TaskNotStartable);
    assert_eq!(err.data["status"], json!("pending"));
    assert_eq!(err.data["unmetDependencies"], json!([first.id]));
}

#[test]
fn task_update_guard_is_named_and_result_survives_a_status_change() {
    let mut env = TestEnv::new();
    let run = run(&mut env);
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
                "term-coordinator",
            )
        })
        .unwrap();
    let err = env
        .engine
        .store
        .tx(|tx| tasks::update_status(tx, env.clock.now(), &task.id, "dispatched", None, false))
        .unwrap_err();
    assert_eq!(err.data["guard"], json!("active_dispatch_required"));
    env.engine
        .store
        .tx(|tx| {
            tasks::update_status(
                tx,
                env.clock.now(),
                &task.id,
                "blocked",
                Some(json!({"note": "kept"})),
                false,
            )
        })
        .unwrap();
    let row = env
        .engine
        .store
        .tx(|tx| tasks::update_status(tx, env.clock.now(), &task.id, "ready", None, false))
        .unwrap();
    assert_eq!(row.result, Some(json!({"note": "kept"})));
    let stored = env
        .engine
        .store
        .read(|tx| tasks::get(tx, &task.id))
        .unwrap()
        .unwrap();
    assert_eq!(stored.result, Some(json!({"note": "kept"})));
    dispatch(&env, &task.id, 0);
    let err = env
        .engine
        .store
        .tx(|tx| tasks::update_status(tx, env.clock.now(), &task.id, "ready", None, false))
        .unwrap_err();
    assert_eq!(err.data["guard"], json!("dispatch_active"));
}
