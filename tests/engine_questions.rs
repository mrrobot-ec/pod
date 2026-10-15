mod support;

use std::str::FromStr;

use pod::ErrorCode;
use pod::engine::questions;
use pod::ids::DispatchId;
use support::TestEnv;

const RUN: &str = "run_000000000001";
const ASK_TASK: &str = "task_000000000001";
const TARGET_TASK: &str = "task_000000000002";
const ASK_DISPATCH: &str = "ctx_000000000001";
const TARGET_DISPATCH: &str = "ctx_000000000002";

fn seed(env: &TestEnv) {
    let now = env.clock.0.lock().unwrap().to_rfc3339();
    env.engine.store.tx(|tx| {
        tx.0.execute("INSERT INTO runs (id, objective, coordinator_terminal_id, coordinator_handle, consumer_generation, home_cwd, depth, created_at, updated_at) VALUES (?1, 'test', 'term-coordinator', 'coordinator', 1, '/tmp', 0, ?2, ?2)", (RUN, &now))?;
        for (task, dispatch, handle, terminal) in [
            (ASK_TASK, ASK_DISPATCH, "asker", "term-asker"),
            (TARGET_TASK, TARGET_DISPATCH, "specialist", "term-specialist"),
        ] {
            tx.0.execute("INSERT INTO tasks (id, run_id, spec, status, deps, created_by_terminal_id, created_at) VALUES (?1, ?2, 'spec', 'dispatched', '[]', 'term-coordinator', ?3)", (task, RUN, &now))?;
            tx.0.execute("INSERT INTO dispatches (id, run_id, task_id, status, failure_count, depth, assignee_handle, assignee_terminal_id, assignee_pid, agent_kind, created_at) VALUES (?1, ?2, ?3, 'dispatched', 0, 1, ?4, ?5, ?6, 'codex', ?7)", (dispatch, RUN, task, handle, terminal, std::process::id(), &now))?;
            tx.0.execute("INSERT INTO workers (dispatch_id, state, terminal_id, terminal_state, owns_pane, created_at, updated_at) VALUES (?1, 'ready', ?2, 'active', 1, ?3, ?3)", (dispatch, terminal, &now))?;
        }
        Ok(())
    }).unwrap();
}

#[test]
fn ask_to_specialist_copies_run_mailbox_same_thread_one_question_row() {
    let mut env = TestEnv::new();
    seed(&env);
    env.as_caller("term-asker", std::process::id());
    let (run_copy, specialist_copy) = questions::ask_store(
        &env.engine,
        "question?",
        &["a".into(), "b".into()],
        Some(TARGET_DISPATCH),
    )
    .unwrap();
    assert!(specialist_copy.is_some());
    let (count, distinct_threads): (i64, i64) = env
        .engine
        .store
        .read(|tx| {
            Ok(tx.0.query_row(
                "SELECT COUNT(*), COUNT(DISTINCT thread_id) FROM messages WHERE id IN (?1, ?2)",
                (run_copy.as_str(), specialist_copy.unwrap().as_str()),
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?)
        })
        .unwrap();
    assert_eq!((count, distinct_threads), (2, 1));
    assert_eq!(
        env.engine
            .store
            .read(|tx| Ok(tx
                .0
                .query_row("SELECT COUNT(*) FROM questions", [], |row| row
                    .get::<_, i64>(0))?))
            .unwrap(),
        1
    );
}

#[test]
fn ask_to_requires_live_other_worker_same_run_not_self() {
    let mut env = TestEnv::new();
    seed(&env);
    env.as_caller("term-asker", std::process::id());
    let error =
        questions::ask_store(&env.engine, "question?", &[], Some(ASK_DISPATCH)).unwrap_err();
    assert_eq!(error.code, ErrorCode::RecipientNotLive);
    env.engine
        .store
        .tx(|tx| {
            tx.0.execute(
                "UPDATE dispatches SET status = 'failed' WHERE id = ?1",
                [TARGET_DISPATCH],
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        questions::ask_store(&env.engine, "question?", &[], Some(TARGET_DISPATCH))
            .unwrap_err()
            .code,
        ErrorCode::RecipientNotLive
    );
}

#[test]
fn first_reply_wins_either_id() {
    for reply_to_specialist in [false, true] {
        let mut env = TestEnv::new();
        seed(&env);
        env.as_caller("term-asker", std::process::id());
        let (run_copy, specialist_copy) =
            questions::ask_store(&env.engine, "question?", &[], Some(TARGET_DISPATCH)).unwrap();
        let message = if reply_to_specialist {
            specialist_copy.unwrap()
        } else {
            run_copy
        };
        let answered = questions::reply(&env.engine, &message, "answer").unwrap();
        assert_eq!(answered.status, "answered");
        assert_eq!(answered.answer_body.as_deref(), Some("answer"));
    }
}

#[test]
fn second_reply_question_answered() {
    let mut env = TestEnv::new();
    seed(&env);
    env.as_caller("term-asker", std::process::id());
    let (run_copy, specialist_copy) =
        questions::ask_store(&env.engine, "question?", &[], Some(TARGET_DISPATCH)).unwrap();
    questions::reply(&env.engine, &specialist_copy.unwrap(), "answer").unwrap();
    assert_eq!(
        questions::reply(&env.engine, &run_copy, "later")
            .unwrap_err()
            .code,
        ErrorCode::QuestionAnswered
    );
}

#[test]
fn questions_closed_when_dispatch_fenced() {
    let mut env = TestEnv::new();
    seed(&env);
    env.as_caller("term-asker", std::process::id());
    questions::ask_store(&env.engine, "question?", &[], None).unwrap();
    let closed = env
        .engine
        .store
        .tx(|tx| {
            questions::close_questions_for_dispatch(
                tx,
                env.engine.clock.now(),
                &DispatchId::from_str(ASK_DISPATCH).unwrap(),
            )
        })
        .unwrap();
    assert_eq!(closed, 1);
}

#[test]
fn reply_outside_herdr_is_refused_and_unrouted_question_has_no_routing_fields() {
    let mut env = TestEnv::new();
    seed(&env);
    env.as_caller("term-asker", std::process::id());
    let (question, specialist) = questions::ask_store(&env.engine, "question?", &[], None).unwrap();
    assert!(specialist.is_none());
    let payload: String = env
        .engine
        .store
        .read(|tx| {
            Ok(tx.0.query_row(
                "SELECT payload FROM messages WHERE id = ?1",
                [question.as_str()],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert!(!payload.contains("routed_to") && !payload.contains("copy_of"));
    env.engine.caller = None;
    assert_eq!(
        questions::reply(&env.engine, &question, "answer")
            .unwrap_err()
            .code,
        ErrorCode::NotInHerdr
    );
}
