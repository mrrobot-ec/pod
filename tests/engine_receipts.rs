mod support;

use std::path::Path;
use std::str::FromStr;

use pod::engine::{receipts, runs};
use pod::ids::RequestId;
use pod::{Caller, Clock, ErrorCode};
use serde::{Deserialize, Serialize};

use support::TestEnv;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct Receipt {
    value: u32,
}

#[test]
fn receipt_replay_returns_stored_receipt() {
    let mut env = TestEnv::new();
    env.as_caller("term-a", 1);
    let request = RequestId::from_str("req_000000000001").unwrap();
    let mut calls = 0;
    let first = receipts::with_receipt(&env.engine, Some(&request), "test", "hash", |_| {
        calls += 1;
        Ok(Receipt { value: 7 })
    })
    .unwrap();
    let replay = receipts::with_receipt(&env.engine, Some(&request), "test", "hash", |_| {
        calls += 1;
        Ok(Receipt { value: 9 })
    })
    .unwrap();
    assert_eq!(first, replay);
    assert_eq!(calls, 1);
}

#[test]
fn receipt_replay_is_scoped_to_caller_fingerprint() {
    let mut env = TestEnv::new();
    let request = RequestId::from_str("req_000000000001").unwrap();
    env.as_caller("term-a", 1);
    let one = receipts::with_receipt(&env.engine, Some(&request), "test", "hash", |_| {
        Ok(Receipt { value: 1 })
    })
    .unwrap();
    env.engine.caller = Some(Caller {
        terminal_id: "term-b".into(),
        pane_id: None,
        pid: 2,
        session: env.engine.paths.session.clone(),
    });
    let two = receipts::with_receipt(&env.engine, Some(&request), "test", "hash", |_| {
        Ok(Receipt { value: 2 })
    })
    .unwrap();
    assert_eq!(one.value, 1);
    assert_eq!(two.value, 2);
}

#[test]
fn request_show_requires_caller_identity() {
    let env = TestEnv::new();
    let request = RequestId::from_str("req_000000000003").unwrap();
    let error = receipts::show(&env.engine, &request).unwrap_err();
    assert_eq!(error.code, ErrorCode::NotInHerdr);
}

#[test]
fn run_use_bumps_generation_and_fences_deliveries() {
    let mut env = TestEnv::new();
    env.as_caller("term-a", 1);
    let caller = env.engine.caller.as_ref().unwrap().clone();
    let run = env
        .engine
        .store
        .tx(|tx| runs::create(tx, env.clock.now(), "run", &caller, Path::new("/tmp/repo")))
        .unwrap();
    env.engine.store.tx(|tx| {
        tx.0.execute("INSERT INTO deliveries (id, run_id, mailbox, consumer_generation, message_ids, status, created_at) VALUES ('dlv_000000000001', ?1, ?2, 1, '[]', 'outstanding', ?3)", rusqlite::params![run.id.as_str(), format!("run:{}", run.id), env.clock.now().to_rfc3339()])?;
        Ok(())
    }).unwrap();
    let updated = env
        .engine
        .store
        .tx(|tx| runs::use_run(tx, env.clock.now(), &run.id, &caller))
        .unwrap();
    assert_eq!(updated.consumer_generation, 2);
    let status: String = env
        .engine
        .store
        .read(|tx| {
            Ok(tx.0.query_row(
                "SELECT status FROM deliveries WHERE id='dlv_000000000001'",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(status, "fenced");
}

#[test]
fn most_recent_run_binding_wins() {
    let mut env = TestEnv::new();
    env.as_caller("term-a", 1);
    let caller = env.engine.caller.as_ref().unwrap().clone();
    let first = env
        .engine
        .store
        .tx(|tx| runs::create(tx, env.clock.now(), "first", &caller, Path::new("/tmp/one")))
        .unwrap();
    env.clock.advance(std::time::Duration::from_secs(1));
    let second = env
        .engine
        .store
        .tx(|tx| {
            runs::create(
                tx,
                env.clock.now(),
                "second",
                &caller,
                Path::new("/tmp/two"),
            )
        })
        .unwrap();
    let bound = env
        .engine
        .store
        .read(|tx| runs::bound_run(tx, &caller))
        .unwrap()
        .unwrap();
    assert_eq!(bound.id, second.id);
    env.clock.advance(std::time::Duration::from_secs(1));
    env.engine
        .store
        .tx(|tx| runs::use_run(tx, env.clock.now(), &first.id, &caller))
        .unwrap();
    let rebound = env
        .engine
        .store
        .read(|tx| runs::bound_run(tx, &caller))
        .unwrap()
        .unwrap();
    assert_eq!(rebound.id, first.id);
}

#[test]
fn receipt_reuse_for_different_mutation_is_usage() {
    let mut env = TestEnv::new();
    env.as_caller("term-a", 1);
    let request = RequestId::from_str("req_000000000002").unwrap();
    receipts::with_receipt(&env.engine, Some(&request), "task-create", "one", |_| {
        Ok(Receipt { value: 1 })
    })
    .unwrap();
    let error = receipts::with_receipt(&env.engine, Some(&request), "gate-create", "two", |_| {
        Ok(Receipt { value: 2 })
    })
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::Usage);
    assert!(error.message.contains("task-create"));
    assert!(error.message.contains("gate-create"));
}
