use std::collections::BTreeMap;
use std::path::Path;
use std::str::FromStr;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, TimeZone, Utc};
use pod::ids::{DispatchId, RunId, TaskId};
use pod::paths;
use pod::store::Store;
use pod::store::rows::{
    deliveries, dispatches, events, gates, messages, observations, questions, receipts, runs,
    tasks, wake_attempts, worker_archives, workers,
};
use serde_json::json;
use tempfile::tempdir;

fn at(second: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 12, 12, 0, second)
        .single()
        .unwrap()
}

fn id<T: FromStr>(value: &str) -> T
where
    T::Err: std::fmt::Debug,
{
    value.parse().unwrap()
}

fn test_paths(root: &Path) -> pod::Paths {
    let values = BTreeMap::from([
        ("HOME", root.display().to_string()),
        ("POD_STATE_DIR", root.join("state").display().to_string()),
        ("POD_CONFIG", root.join("config.toml").display().to_string()),
        ("HERDR_SOCKET_PATH", "/tmp/herdr/store.sock".to_owned()),
    ]);
    paths::resolve(&move |name| values.get(name).cloned()).unwrap()
}

fn run_row(id_value: &str) -> runs::RunRow {
    runs::RunRow {
        id: id(id_value),
        objective: "ship pod".into(),
        coordinator_terminal_id: "term-coordinator".into(),
        coordinator_handle: "coordinator".into(),
        consumer_generation: 1,
        home_cwd: "/tmp/repo".into(),
        depth: 0,
        budget_usd: None,
        created_at: at(0),
        updated_at: at(0),
    }
}

fn task_row(id_value: &str, run_id: &RunId) -> tasks::TaskRow {
    tasks::TaskRow {
        id: id(id_value),
        run_id: run_id.clone(),
        parent_id: None,
        title: Some("store rows".into()),
        display_name: None,
        spec: "implement typed rows".into(),
        status: "ready".into(),
        deps: Vec::new(),
        result: None,
        created_by_terminal_id: "term-coordinator".into(),
        created_at: at(1),
        completed_at: None,
    }
}

fn dispatch_row(id_value: &str, run_id: &RunId, task_id: &TaskId) -> dispatches::DispatchRow {
    dispatches::DispatchRow {
        id: id(id_value),
        run_id: run_id.clone(),
        task_id: task_id.clone(),
        status: "dispatched".into(),
        retry_of_dispatch_id: None,
        failure_count: 0,
        last_failure: None,
        termination_reason: None,
        termination_target: None,
        depth: 0,
        assignee_handle: Some("worker-a".into()),
        assignee_terminal_id: Some("term-worker-a".into()),
        assignee_pid: Some(42),
        agent_kind: Some("codex".into()),
        launch_requested: Some(json!({"agent": "sol"})),
        launch_effective: Some(json!({"kind": "codex"})),
        dispatched_at: Some(at(2)),
        last_heartbeat_at: None,
        last_phase: None,
        completed_at: None,
        created_at: at(2),
    }
}

fn message_row(id_value: &str, run_id: &RunId) -> messages::MessageRow {
    messages::MessageRow {
        sequence: None,
        id: id(id_value),
        run_id: run_id.clone(),
        mailbox: format!("run:{run_id}"),
        from_handle: "worker-a".into(),
        subject: "done".into(),
        body: Some("finished".into()),
        r#type: "status".into(),
        priority: "normal".into(),
        thread_id: None,
        payload: Some(json!({"ok": true})),
        read: false,
        created_at: at(3),
        delivered_at: None,
        sender_terminal_id: Some("term-worker-a".into()),
        rejected_reason: None,
        evidence: None,
        recommendation: None,
    }
}

fn seed_dispatch(store: &Store) -> (RunId, TaskId, DispatchId) {
    let run = run_row("run_000000000001");
    let task = task_row("task_000000000001", &run.id);
    let dispatch = dispatch_row("ctx_000000000001", &run.id, &task.id);
    let ids = (run.id.clone(), task.id.clone(), dispatch.id.clone());
    store
        .tx(|tx| {
            runs::insert(tx, &run)?;
            tasks::insert(tx, &task)?;
            dispatches::insert(tx, &dispatch)
        })
        .unwrap();
    ids
}

#[test]
fn runs_round_trip_insert_get_list() {
    let tmp = tempdir().unwrap();
    let store = Store::open(&test_paths(tmp.path())).unwrap();
    let row = run_row("run_000000000001");
    store.tx(|tx| runs::insert(tx, &row)).unwrap();
    store
        .read(|tx| {
            assert_eq!(runs::get(tx, &row.id)?, Some(row.clone()));
            assert_eq!(runs::list(tx, 10)?, vec![row.clone()]);
            Ok(())
        })
        .unwrap();
}

#[test]
fn tasks_round_trip_insert_get_list() {
    let tmp = tempdir().unwrap();
    let store = Store::open(&test_paths(tmp.path())).unwrap();
    let run = run_row("run_000000000001");
    let mut dependency = task_row("task_000000000001", &run.id);
    dependency.status = "completed".into();
    dependency.completed_at = Some(at(2));
    let mut row = task_row("task_000000000002", &run.id);
    row.status = "pending".into();
    row.deps = vec![dependency.id.clone()];
    store
        .tx(|tx| {
            runs::insert(tx, &run)?;
            tasks::insert(tx, &dependency)?;
            tasks::insert(tx, &row)
        })
        .unwrap();
    store
        .read(|tx| {
            assert_eq!(tasks::get(tx, &row.id)?, Some(row.clone()));
            assert_eq!(tasks::list_by_run(tx, &run.id, None)?.len(), 2);
            assert_eq!(
                tasks::ready_dependants(tx, &dependency.id)?,
                vec![row.clone()]
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn dispatches_round_trip_insert_get_list() {
    let tmp = tempdir().unwrap();
    let store = Store::open(&test_paths(tmp.path())).unwrap();
    let (run, task, dispatch) = seed_dispatch(&store);
    store
        .read(|tx| {
            let row = dispatches::get(tx, &dispatch)?.unwrap();
            assert_eq!(row.run_id, run);
            assert_eq!(dispatches::list(tx, 10)?, vec![row.clone()]);
            assert_eq!(
                dispatches::active_for_terminal(tx, "term-worker-a")?,
                Some(row.clone())
            );
            assert_eq!(dispatches::active_for_task(tx, &task)?, Some(row));
            Ok(())
        })
        .unwrap();
}

#[test]
fn workers_round_trip_insert_get_list() {
    let tmp = tempdir().unwrap();
    let store = Store::open(&test_paths(tmp.path())).unwrap();
    let (_, _, dispatch) = seed_dispatch(&store);
    let row = workers::WorkerRow {
        dispatch_id: dispatch,
        state: "ready".into(),
        stage: Some("record".into()),
        placement: Some(json!({"kind": "current"})),
        worktree_path: Some("/tmp/repo".into()),
        workspace_id: Some("workspace-1".into()),
        tab_id: Some("tab-1".into()),
        pane_id: Some("pane-1".into()),
        terminal_id: Some("term-worker-a".into()),
        owns_pane: true,
        user_owned: false,
        user_owned_reason: None,
        setup_state: Some("none".into()),
        setup_pane_id: None,
        preamble_input: Some("delivered".into()),
        effects: Some(json!(["pane_created"])),
        residual_resources: Some(json!([])),
        terminal_state: "active".into(),
        retained_reason: None,
        session_id: Some("session-1".into()),
        transcript_path: None,
        transcript_proof: None,
        last_error: None,
        created_at: at(3),
        updated_at: at(3),
    };
    store.tx(|tx| workers::insert(tx, &row)).unwrap();
    store
        .read(|tx| {
            assert_eq!(workers::get(tx, &row.dispatch_id)?, Some(row.clone()));
            assert_eq!(workers::list(tx, 10)?, vec![row.clone()]);
            Ok(())
        })
        .unwrap();
}

#[test]
fn worker_archives_round_trip_insert_get_list() {
    let tmp = tempdir().unwrap();
    let store = Store::open(&test_paths(tmp.path())).unwrap();
    let (_, _, dispatch) = seed_dispatch(&store);
    let row = worker_archives::WorkerArchiveRow {
        dispatch_id: dispatch,
        kind: "final_screen".into(),
        content: "all tests pass".into(),
        created_at: at(4),
    };
    store.tx(|tx| worker_archives::insert(tx, &row)).unwrap();
    store
        .read(|tx| {
            assert_eq!(
                worker_archives::get(tx, &row.dispatch_id, &row.kind)?,
                Some(row.clone())
            );
            assert_eq!(worker_archives::list(tx, 10)?, vec![row.clone()]);
            Ok(())
        })
        .unwrap();
}

#[test]
fn observations_round_trip_insert_get_list() {
    let tmp = tempdir().unwrap();
    let store = Store::open(&test_paths(tmp.path())).unwrap();
    let (_, _, dispatch) = seed_dispatch(&store);
    let row = observations::ObservationRow {
        dispatch_id: dispatch,
        liveness: "live".into(),
        activity: "working".into(),
        presence: Some("present".into()),
        needs_input: json!({"count": 0, "reasons": []}),
        evidence_source: "herdr_state".into(),
        reason: None,
        herdr_status: Some("working".into()),
        attention: Some(json!({"categories": []})),
        observed_at: at(4),
    };
    store.tx(|tx| observations::insert(tx, &row)).unwrap();
    store
        .read(|tx| {
            assert_eq!(observations::get(tx, &row.dispatch_id)?, Some(row.clone()));
            assert_eq!(observations::list(tx, 10)?, vec![row.clone()]);
            Ok(())
        })
        .unwrap();
}

#[test]
fn messages_round_trip_insert_get_list() {
    let tmp = tempdir().unwrap();
    let store = Store::open(&test_paths(tmp.path())).unwrap();
    let run = run_row("run_000000000001");
    let row = message_row("msg_000000000001", &run.id);
    store
        .tx(|tx| {
            runs::insert(tx, &run)?;
            messages::insert(tx, &row)
        })
        .unwrap();
    store
        .read(|tx| {
            let got = messages::get(tx, &row.id)?.unwrap();
            assert_eq!(got.sequence, Some(1));
            assert_eq!(
                messages::unread_for_mailbox(tx, &row.mailbox, 10)?,
                vec![got.clone()]
            );
            assert_eq!(messages::history(tx, &row.mailbox, 10)?, vec![got]);
            Ok(())
        })
        .unwrap();
}

#[test]
fn deliveries_round_trip_insert_get_list() {
    let tmp = tempdir().unwrap();
    let store = Store::open(&test_paths(tmp.path())).unwrap();
    let run = run_row("run_000000000001");
    let row = deliveries::DeliveryRow {
        id: id("dlv_000000000001"),
        run_id: run.id.clone(),
        mailbox: format!("run:{}", run.id),
        consumer_generation: 1,
        message_ids: vec!["msg_000000000001".parse().expect("message ID")],
        status: "outstanding".into(),
        created_at: at(5),
        acknowledged_at: None,
    };
    store
        .tx(|tx| {
            runs::insert(tx, &run)?;
            deliveries::insert(tx, &row)
        })
        .unwrap();
    store
        .read(|tx| {
            assert_eq!(deliveries::get(tx, &row.id)?, Some(row.clone()));
            assert_eq!(deliveries::list(tx, 10)?, vec![row.clone()]);
            assert_eq!(
                deliveries::outstanding(tx, &row.mailbox)?,
                Some(row.clone())
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn wake_attempts_round_trip_insert_get_list() {
    let tmp = tempdir().unwrap();
    let store = Store::open(&test_paths(tmp.path())).unwrap();
    let run = run_row("run_000000000001");
    let row = wake_attempts::WakeAttemptRow {
        id: id("wake_000000000001"),
        run_id: run.id.clone(),
        mailbox: format!("run:{}", run.id),
        delivery_id: None,
        attempt: 1,
        outcome: "delivered".into(),
        detail: None,
        created_at: at(6),
    };
    store
        .tx(|tx| {
            runs::insert(tx, &run)?;
            wake_attempts::insert(tx, &row)
        })
        .unwrap();
    store
        .read(|tx| {
            assert_eq!(wake_attempts::get(tx, &row.id)?, Some(row.clone()));
            assert_eq!(wake_attempts::list(tx, 10)?, vec![row.clone()]);
            assert_eq!(
                wake_attempts::count_since_last_consume(tx, &row.mailbox)?,
                1
            );
            Ok(())
        })
        .unwrap();
    store
        .tx(|tx| {
            events::append(
                tx,
                &events::EventRow {
                    sequence: None,
                    run_id: run.id.clone(),
                    at: at(7),
                    r#type: "mailbox.consumed".into(),
                    task_id: None,
                    dispatch_id: None,
                    actor: "term-coordinator".into(),
                    detail: Some(json!({
                        "mailbox": row.mailbox,
                        "delivery_id": null,
                        "consumer": "term-coordinator"
                    })),
                },
            )
        })
        .unwrap();
    assert_eq!(
        store
            .read(|tx| wake_attempts::count_since_last_consume(tx, &row.mailbox))
            .unwrap(),
        0
    );
}

#[test]
fn questions_round_trip_insert_get_list() {
    let tmp = tempdir().unwrap();
    let store = Store::open(&test_paths(tmp.path())).unwrap();
    let (run, _, dispatch) = seed_dispatch(&store);
    let message = message_row("msg_000000000001", &run);
    let row = questions::QuestionRow {
        message_id: message.id.clone(),
        run_id: run,
        dispatch_id: Some(dispatch),
        asker_handle: "worker-a".into(),
        status: "pending".into(),
        answer_message_id: None,
        answer_body: None,
        created_at: at(7),
        answered_at: None,
        closed_at: None,
    };
    store
        .tx(|tx| {
            messages::insert(tx, &message)?;
            questions::insert(tx, &row)
        })
        .unwrap();
    store
        .read(|tx| {
            assert_eq!(questions::get(tx, &row.message_id)?, Some(row.clone()));
            assert_eq!(questions::list(tx, 10)?, vec![row.clone()]);
            Ok(())
        })
        .unwrap();
}

#[test]
fn gates_round_trip_insert_get_list() {
    let tmp = tempdir().unwrap();
    let store = Store::open(&test_paths(tmp.path())).unwrap();
    let run = run_row("run_000000000001");
    let row = gates::GateRow {
        id: id("gate_000000000001"),
        run_id: run.id.clone(),
        task_id: None,
        audience: "user".into(),
        question: "ship?".into(),
        options: vec!["yes".into(), "no".into()],
        evidence: Some("msg_000000000001".into()),
        recommendation: Some("yes".into()),
        status: "pending".into(),
        resolution: None,
        created_at: at(8),
        resolved_at: None,
    };
    store
        .tx(|tx| {
            runs::insert(tx, &run)?;
            gates::insert(tx, &row)
        })
        .unwrap();
    store
        .read(|tx| {
            assert_eq!(gates::get(tx, &row.id)?, Some(row.clone()));
            assert_eq!(gates::list(tx, 10)?, vec![row.clone()]);
            Ok(())
        })
        .unwrap();
}

#[test]
fn receipts_round_trip_insert_get_list() {
    let tmp = tempdir().unwrap();
    let store = Store::open(&test_paths(tmp.path())).unwrap();
    let row = receipts::ReceiptRow {
        caller_fingerprint: "abc123".into(),
        request_id: id("req_000000000001"),
        method: "task-create".into(),
        payload_hash: "def456".into(),
        state: "completed".into(),
        receipt: Some(json!({"ok": true})),
        created_at: at(9),
        updated_at: at(9),
    };
    store.tx(|tx| receipts::insert(tx, &row)).unwrap();
    store
        .read(|tx| {
            assert_eq!(
                receipts::get(tx, &row.caller_fingerprint, &row.request_id)?,
                Some(row.clone())
            );
            assert_eq!(receipts::list(tx, 10)?, vec![row.clone()]);
            Ok(())
        })
        .unwrap();
}

#[test]
fn events_round_trip_insert_get_list() {
    let tmp = tempdir().unwrap();
    let store = Store::open(&test_paths(tmp.path())).unwrap();
    let run = run_row("run_000000000001");
    let row = events::EventRow {
        sequence: None,
        run_id: run.id.clone(),
        at: at(10),
        r#type: "run.created".into(),
        task_id: None,
        dispatch_id: None,
        actor: "coordinator".into(),
        detail: Some(json!({})),
    };
    store
        .tx(|tx| {
            runs::insert(tx, &run)?;
            events::insert(tx, &row)
        })
        .unwrap();
    store
        .read(|tx| {
            let got = events::get(tx, 1)?.unwrap();
            assert_eq!(got.sequence, Some(1));
            assert_eq!(events::list(tx, &run.id)?, vec![got]);
            Ok(())
        })
        .unwrap();
}

#[test]
fn events_sequence_is_monotonic() {
    let tmp = tempdir().unwrap();
    let store = Store::open(&test_paths(tmp.path())).unwrap();
    let run = run_row("run_000000000001");
    store
        .tx(|tx| {
            runs::insert(tx, &run)?;
            for index in 0..100 {
                events::append(
                    tx,
                    &events::EventRow {
                        sequence: None,
                        run_id: run.id.clone(),
                        at: at(10),
                        r#type: format!("test.{index}"),
                        task_id: None,
                        dispatch_id: None,
                        actor: "test".into(),
                        detail: None,
                    },
                )?;
            }
            Ok(())
        })
        .unwrap();
    let sequences = store
        .read(|tx| {
            Ok(events::list(tx, &run.id)?
                .into_iter()
                .map(|row| row.sequence.unwrap())
                .collect::<Vec<_>>())
        })
        .unwrap();
    assert_eq!(sequences, (1..=100).collect::<Vec<_>>());
}

#[test]
fn receipts_prune_by_count_and_age() {
    let tmp = tempdir().unwrap();
    let store = Store::open(&test_paths(tmp.path())).unwrap();
    let now = Utc
        .with_ymd_and_hms(2026, 10, 12, 12, 0, 0)
        .single()
        .unwrap();
    store
        .tx(|tx| {
            receipts::insert(
                tx,
                &receipts::ReceiptRow {
                    caller_fingerprint: "stale".into(),
                    request_id: id("req_ffffffffffff"),
                    method: "send".into(),
                    payload_hash: "stale".into(),
                    state: "completed".into(),
                    receipt: None,
                    created_at: now - chrono::Duration::days(31),
                    updated_at: now - chrono::Duration::days(31),
                },
            )?;
            for index in 0..10_002_u64 {
                receipts::insert(
                    tx,
                    &receipts::ReceiptRow {
                        caller_fingerprint: "active".into(),
                        request_id: id(&format!("req_{index:012x}")),
                        method: "send".into(),
                        payload_hash: format!("hash-{index}"),
                        state: "completed".into(),
                        receipt: None,
                        created_at: now + chrono::Duration::microseconds(index as i64),
                        updated_at: now + chrono::Duration::microseconds(index as i64),
                    },
                )?;
            }
            receipts::prune(tx, now)
        })
        .unwrap();
    store
        .read(|tx| {
            assert_eq!(receipts::list(tx, 20_000)?.len(), 10_000);
            assert!(receipts::get(tx, "stale", &id("req_ffffffffffff"))?.is_none());
            assert!(receipts::get(tx, "active", &id("req_000000000000"))?.is_none());
            assert!(receipts::get(tx, "active", &id("req_000000002711"))?.is_some());
            Ok(())
        })
        .unwrap();
}

#[test]
fn store_concurrent_writers_both_commit() {
    let tmp = tempdir().unwrap();
    let paths = test_paths(tmp.path());
    let barrier = Arc::new(Barrier::new(2));
    let handles = (0..2)
        .map(|writer| {
            let barrier = barrier.clone();
            let paths = paths.clone();
            thread::spawn(move || {
                // Both open the fresh file at once: the WAL switch and the first
                // migration race too, as they do for two `pod` processes.
                barrier.wait();
                let store = Store::open(&paths).unwrap();
                for index in 0..200_u64 {
                    let suffix = writer as u64 * 200 + index + 1;
                    let row = run_row(&format!("run_{suffix:012x}"));
                    store.tx(|tx| runs::insert(tx, &row)).unwrap();
                }
            })
        })
        .collect::<Vec<_>>();
    for handle in handles {
        handle.join().unwrap();
    }
    let store = Store::open(&paths).unwrap();
    assert_eq!(store.read(|tx| runs::list(tx, 500)).unwrap().len(), 400);
}

#[test]
#[cfg_attr(debug_assertions, ignore = "timing test: run with --release")]
fn signal_wait_wakes_on_touch_within_100ms() {
    let tmp = tempdir().unwrap();
    let path = tmp.path().join("pod.signal");
    let touched = path.clone();
    let worker = thread::spawn(move || {
        thread::sleep(Duration::from_millis(20));
        pod::store::signal::touch(&touched).unwrap();
    });
    let started = Instant::now();
    assert!(pod::store::signal::wait(&path, started + Duration::from_secs(1)).unwrap());
    assert!(
        started.elapsed() < Duration::from_millis(100),
        "{:?}",
        started.elapsed()
    );
    worker.join().unwrap();
}

#[test]
fn signal_wait_times_out_false() {
    let tmp = tempdir().unwrap();
    let path = tmp.path().join("pod.signal");
    assert!(!pod::store::signal::wait(&path, Instant::now() + Duration::from_millis(30)).unwrap());
}

#[test]
fn signal_wait_wakes_on_commit_without_touch() {
    let tmp = tempdir().unwrap();
    let paths = test_paths(tmp.path());
    drop(Store::open(&paths).unwrap());
    let db_path = paths.db_path.clone();
    let writer = thread::spawn(move || {
        thread::sleep(Duration::from_millis(20));
        let connection = rusqlite::Connection::open(db_path).unwrap();
        connection
            .execute_batch("CREATE TABLE committed_without_touch (id INTEGER)")
            .unwrap();
    });
    let started = Instant::now();
    assert!(
        pod::store::signal::wait(&paths.signal_path, started + Duration::from_secs(3)).unwrap()
    );
    assert!(started.elapsed() < Duration::from_millis(1_500));
    writer.join().unwrap();
}

#[test]
fn signal_snapshot_counts_commit_before_wait() {
    let tmp = tempdir().unwrap();
    let paths = test_paths(tmp.path());
    let store = Store::open(&paths).unwrap();
    // A waiter snapshots, finds nothing, and a commit lands before it waits.
    let snapshot = pod::store::signal::snapshot(&paths.signal_path);
    store
        .tx(|tx| runs::insert(tx, &run_row("run_000000000001")))
        .unwrap();
    let started = Instant::now();
    assert!(snapshot.wait(started + Duration::from_secs(2)).unwrap());
    // The wake itself is the rule; its latency is a release-only timing check.
    if !cfg!(debug_assertions) {
        assert!(started.elapsed() < Duration::from_millis(100));
    }
}

#[cfg(unix)]
#[test]
fn signal_touch_creates_private_file_and_dir() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempdir().unwrap();
    let dir = tmp.path().join("fresh");
    let path = dir.join("pod.signal");
    pod::store::signal::touch(&path).unwrap();
    let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&path), 0o600);
}

#[test]
fn dispatches_fence_ends_active_once_and_keeps_accepted_ending() {
    let tmp = tempdir().unwrap();
    let store = Store::open(&test_paths(tmp.path())).unwrap();
    let (_, task, dispatch) = seed_dispatch(&store);
    store
        .tx(|tx| {
            assert!(dispatches::fence(
                tx,
                &dispatch,
                "failed",
                "canceled",
                at(5)
            )?);
            // A late second fence (or a stop after worker_done) changes nothing.
            assert!(!dispatches::fence(
                tx,
                &dispatch,
                "failed",
                "superseded",
                at(9)
            )?);
            let row = dispatches::get(tx, &dispatch)?.unwrap();
            assert_eq!(row.status, "failed");
            assert_eq!(row.termination_reason.as_deref(), Some("canceled"));
            assert_eq!(row.completed_at, Some(at(5)));
            assert_eq!(dispatches::active_for_task(tx, &task)?, None);
            assert_eq!(dispatches::active_for_terminal(tx, "term-worker-a")?, None);
            Ok(())
        })
        .unwrap();
}

#[test]
fn a_version_2_store_migrates_to_3_with_a_budget_column() {
    let tmp = tempdir().unwrap();
    let paths = test_paths(tmp.path());
    drop(Store::open(&paths).unwrap());
    // Rewind the store to what 0.1.0 wrote: no budget column, version 2.
    let connection = rusqlite::Connection::open(&paths.db_path).unwrap();
    connection
        .execute_batch("ALTER TABLE runs DROP COLUMN budget_usd; PRAGMA user_version = 2;")
        .unwrap();
    // A Run 0.1.0 wrote, before the migration, must read back after it.
    connection
        .execute(
            "INSERT INTO runs (id, objective, coordinator_terminal_id, coordinator_handle, \
             consumer_generation, home_cwd, depth, created_at, updated_at) \
             VALUES ('run_000000000009', 'from 0.1.0', 'term-a', 'coord', 1, '/tmp', 0, \
             '2026-10-13T00:00:00Z', '2026-10-13T00:00:00Z')",
            [],
        )
        .unwrap();
    drop(connection);
    let store = Store::open(&paths).unwrap();
    assert_eq!(store.user_version().unwrap(), 3);
    let columns: Vec<String> = store
        .read(|tx| {
            let mut statement = tx.0.prepare("PRAGMA table_info(runs)")?;
            Ok(statement
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<rusqlite::Result<_>>()?)
        })
        .unwrap();
    assert!(columns.contains(&"budget_usd".to_owned()), "{columns:?}");
    // An older Run reads back without a budget, a new one keeps its budget.
    let mut budgeted = run_row("run_000000000002");
    budgeted.budget_usd = Some(2.5);
    store
        .tx(|tx| {
            runs::insert(tx, &run_row("run_000000000001"))?;
            runs::insert(tx, &budgeted)
        })
        .unwrap();
    let read = |value: &str| {
        store
            .read(|tx| runs::get(tx, &id(value)))
            .unwrap()
            .unwrap()
            .budget_usd
    };
    assert_eq!(read("run_000000000001"), None);
    assert_eq!(read("run_000000000009"), None);
    let old = store
        .read(|tx| runs::get(tx, &id("run_000000000009")))
        .unwrap()
        .unwrap();
    assert_eq!(old.objective, "from 0.1.0");
    assert_eq!(read("run_000000000002"), Some(2.5));
}
