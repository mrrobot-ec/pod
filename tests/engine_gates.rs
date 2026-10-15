mod support;

use std::path::Path;

use pod::engine::{gates, runs, tasks};
use pod::{Clock, ErrorCode};

use support::TestEnv;

fn setup() -> (TestEnv, runs::RunRow, tasks::TaskRow) {
    let mut env = TestEnv::new();
    env.as_caller("term-coordinator", 10);
    let caller = env.engine.caller.as_ref().unwrap().clone();
    let run = env
        .engine
        .store
        .tx(|tx| {
            runs::create(
                tx,
                env.clock.now(),
                "gates",
                &caller,
                Path::new("/tmp/repo"),
            )
        })
        .unwrap();
    let task = env
        .engine
        .store
        .tx(|tx| {
            tasks::create(
                tx,
                env.clock.now(),
                &run.id,
                "decide",
                None,
                None,
                &[],
                None,
                "term-coordinator",
            )
        })
        .unwrap();
    (env, run, task)
}

fn status(env: &TestEnv, task: &pod::ids::TaskId) -> String {
    env.engine
        .store
        .read(|tx| {
            Ok(tx.0.query_row(
                "SELECT status FROM tasks WHERE id=?1",
                [task.as_str()],
                |r| r.get(0),
            )?)
        })
        .unwrap()
}

#[test]
fn gate_create_coordinator_requires_task() {
    let (env, run, _) = setup();
    let err = env
        .engine
        .store
        .tx(|tx| {
            gates::create(
                tx,
                env.clock.now(),
                &run.id,
                "coordinator",
                None,
                "question",
                &[],
                None,
                None,
                false,
            )
        })
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Usage);
}

#[test]
fn gate_refused_while_worker_live() {
    let (env, run, task) = setup();
    let err = env
        .engine
        .store
        .tx(|tx| {
            gates::create(
                tx,
                env.clock.now(),
                &run.id,
                "coordinator",
                Some(&task.id),
                "question",
                &[],
                None,
                None,
                true,
            )
        })
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::TaskGuardRefused);
}

#[test]
fn gate_create_user_requires_evidence_recommendation_options_and_recommendation_in_options() {
    let (env, run, _) = setup();
    for (options, evidence, recommendation) in [
        (vec![], Some("msg_1"), Some("a")),
        (vec!["a".into()], None, Some("a")),
        (vec!["a".into()], Some("msg_1"), None),
        (vec!["a".into()], Some("msg_1"), Some("b")),
    ] {
        let err = env
            .engine
            .store
            .tx(|tx| {
                gates::create(
                    tx,
                    env.clock.now(),
                    &run.id,
                    "user",
                    None,
                    "question",
                    &options,
                    evidence,
                    recommendation,
                    false,
                )
            })
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::GateEvidenceRequired);
    }
}

#[test]
fn gate_resolve_ready_only_when_deps_completed_else_pending() {
    let (env, run, dependency) = setup();
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
    let gate = env
        .engine
        .store
        .tx(|tx| {
            gates::create(
                tx,
                env.clock.now(),
                &run.id,
                "coordinator",
                Some(&dependant.id),
                "question",
                &[],
                None,
                None,
                false,
            )
        })
        .unwrap();
    env.engine
        .store
        .tx(|tx| gates::resolve(tx, env.clock.now(), &gate.id, "later"))
        .unwrap();
    assert_eq!(status(&env, &dependant.id), "pending");
    env.engine
        .store
        .tx(|tx| {
            tasks::update_status(
                tx,
                env.clock.now(),
                &dependency.id,
                "completed",
                None,
                false,
            )
        })
        .unwrap();
    let second = env
        .engine
        .store
        .tx(|tx| {
            gates::create(
                tx,
                env.clock.now(),
                &run.id,
                "coordinator",
                Some(&dependant.id),
                "again",
                &[],
                None,
                None,
                false,
            )
        })
        .unwrap();
    env.engine
        .store
        .tx(|tx| gates::resolve(tx, env.clock.now(), &second.id, "go"))
        .unwrap();
    assert_eq!(status(&env, &dependant.id), "ready");
}

#[test]
fn gate_resolve_user_gate_stores_decision_gate_message() {
    let (env, run, _) = setup();
    let options = vec!["a".to_owned(), "b".to_owned()];
    let gate = env
        .engine
        .store
        .tx(|tx| {
            gates::create(
                tx,
                env.clock.now(),
                &run.id,
                "user",
                None,
                "question",
                &options,
                Some("msg_1"),
                Some("a"),
                false,
            )
        })
        .unwrap();
    let (_, message) = env
        .engine
        .store
        .tx(|tx| gates::resolve(tx, env.clock.now(), &gate.id, "a"))
        .unwrap();
    let message = message.unwrap();
    assert_eq!(message.r#type, "decision_gate");
    assert_eq!(message.mailbox, format!("run:{}", run.id));
}

#[test]
fn untasked_gate_changes_no_task() {
    let (env, run, task) = setup();
    let before = status(&env, &task.id);
    let options = vec!["a".to_owned()];
    let gate = env
        .engine
        .store
        .tx(|tx| {
            gates::create(
                tx,
                env.clock.now(),
                &run.id,
                "user",
                None,
                "question",
                &options,
                Some("evidence"),
                Some("a"),
                false,
            )
        })
        .unwrap();
    env.engine
        .store
        .tx(|tx| gates::resolve(tx, env.clock.now(), &gate.id, "a"))
        .unwrap();
    assert_eq!(status(&env, &task.id), before);
}

#[test]
fn gate_resolve_duplicate_is_idempotent() {
    let (env, run, _) = setup();
    let options = vec!["a".to_owned()];
    let gate = env
        .engine
        .store
        .tx(|tx| {
            gates::create(
                tx,
                env.clock.now(),
                &run.id,
                "user",
                None,
                "question",
                &options,
                Some("evidence"),
                Some("a"),
                false,
            )
        })
        .unwrap();
    env.engine
        .store
        .tx(|tx| gates::resolve(tx, env.clock.now(), &gate.id, "a"))
        .unwrap();
    let (_, duplicate_message) = env
        .engine
        .store
        .tx(|tx| gates::resolve(tx, env.clock.now(), &gate.id, "a"))
        .unwrap();
    assert!(duplicate_message.is_none());
    let count: u32 = env
        .engine
        .store
        .read(|tx| {
            Ok(tx.0.query_row(
                "SELECT COUNT(*) FROM messages WHERE type='decision_gate'",
                [],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(count, 1);
}

fn coordinator_gate(env: &TestEnv, run: &runs::RunRow, task: &pod::ids::TaskId) -> gates::GateRow {
    env.engine
        .store
        .tx(|tx| {
            gates::create(
                tx,
                env.clock.now(),
                &run.id,
                "coordinator",
                Some(task),
                "question",
                &[],
                None,
                None,
                false,
            )
        })
        .unwrap()
}

#[test]
fn gate_on_completed_task_never_reopens_it() {
    let (env, run, task) = setup();
    env.engine
        .store
        .tx(|tx| tasks::update_status(tx, env.clock.now(), &task.id, "completed", None, false))
        .unwrap();
    let gate = coordinator_gate(&env, &run, &task.id);
    assert_eq!(status(&env, &task.id), "completed");
    env.engine
        .store
        .tx(|tx| gates::resolve(tx, env.clock.now(), &gate.id, "redo"))
        .unwrap();
    assert_eq!(status(&env, &task.id), "completed");
}

#[test]
fn task_stays_blocked_until_its_last_pending_gate_resolves() {
    let (env, run, task) = setup();
    let first = coordinator_gate(&env, &run, &task.id);
    let second = coordinator_gate(&env, &run, &task.id);
    assert_eq!(status(&env, &task.id), "blocked");
    env.engine
        .store
        .tx(|tx| gates::resolve(tx, env.clock.now(), &first.id, "a"))
        .unwrap();
    assert_eq!(status(&env, &task.id), "blocked");
    env.engine
        .store
        .tx(|tx| gates::resolve(tx, env.clock.now(), &second.id, "b"))
        .unwrap();
    assert_eq!(status(&env, &task.id), "ready");
}

#[test]
fn gate_completes_active_dispatch_and_leaves_its_worker_reclaimable() {
    let (env, run, task) = setup();
    env.engine.store.tx(|tx| {
        let now = env.clock.now().to_rfc3339();
        tx.0.execute("INSERT INTO dispatches (id, run_id, task_id, status, failure_count, depth, created_at) VALUES ('ctx_000000000001', ?1, ?2, 'dispatched', 0, 0, ?3)", rusqlite::params![run.id.as_str(), task.id.as_str(), now])?;
        tx.0.execute("INSERT INTO workers (dispatch_id, state, owns_pane, terminal_state, created_at, updated_at) VALUES ('ctx_000000000001', 'ready', 1, 'active', ?1, ?1)", [&now])?;
        tx.0.execute("UPDATE tasks SET status='dispatched' WHERE id=?1", [task.id.as_str()])?;
        Ok(())
    }).unwrap();
    coordinator_gate(&env, &run, &task.id);
    let (dispatch, worker): (String, String) = env
        .engine
        .store
        .read(|tx| {
            Ok(tx.0.query_row(
                "SELECT d.status, w.terminal_state FROM dispatches d JOIN workers w ON w.dispatch_id=d.id WHERE d.id='ctx_000000000001'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .unwrap();
    assert_eq!(dispatch, "completed");
    assert_eq!(worker, "reclaimable");
    assert_eq!(status(&env, &task.id), "blocked");
}

fn user_gate(env: &TestEnv, run: &runs::RunRow) -> gates::GateRow {
    env.engine
        .store
        .tx(|tx| {
            gates::create(
                tx,
                env.clock.now(),
                &run.id,
                "user",
                None,
                "Ship or wait?",
                &["ship".to_owned(), "wait".to_owned()],
                Some("evidence"),
                Some("ship"),
                false,
            )
        })
        .unwrap()
}

#[test]
fn a_new_user_gate_notifies_through_herdr_and_a_coordinator_gate_does_not() {
    let (env, run, task) = setup();
    let gate = coordinator_gate(&env, &run, &task.id);
    gates::notify_created(&env.engine, &gate);
    assert!(
        env.herdr
            .calls()
            .iter()
            .all(|call| !call.starts_with("notification_show:")),
        "{:?}",
        env.herdr.calls()
    );
    let gate = user_gate(&env, &run);
    gates::notify_created(&env.engine, &gate);
    let shown: Vec<String> = env
        .herdr
        .calls()
        .into_iter()
        .filter(|call| call.starts_with("notification_show:"))
        .collect();
    assert_eq!(shown, vec!["notification_show:gates:Ship or wait?:request"]);
}

#[test]
fn notify_runs_the_configured_command_with_title_and_body_in_its_environment() {
    let (mut env, _, _) = setup();
    let out = env.tmp.path().join("notify.out");
    env.engine.config.notify.command = vec![
        "sh".into(),
        "-c".into(),
        format!(
            "printf '%s|%s' \"$NOTIFY_TITLE\" \"$NOTIFY_BODY\" > {}",
            out.display()
        ),
    ];
    env.engine.notify("title", "body");
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "title|body");
    assert!(
        env.herdr
            .calls()
            .contains(&"notification_show:title:body:request".to_owned())
    );
}

#[test]
fn a_failing_or_missing_notify_command_is_only_a_warning() {
    let (mut env, _, _) = setup();
    env.engine.config.notify.command = vec!["/nonexistent/pod-notify".into()];
    env.engine.notify("title", "body");
    env.engine.config.notify.command = vec!["sh".into(), "-c".into(), "exit 3".into()];
    env.engine.notify("title", "body");
    // Spec A5.4: each failure is a warning the receipt carries, once.
    let warnings = env.engine.take_warnings();
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(warnings[0].starts_with("notify command failed: /nonexistent/pod-notify"));
    assert!(warnings[1].contains("exited with"), "{warnings:?}");
    assert!(env.engine.take_warnings().is_empty());
    env.engine.config.notify.command = vec!["true".into()];
    env.engine.notify("title", "body");
    assert!(env.engine.take_warnings().is_empty());
}
