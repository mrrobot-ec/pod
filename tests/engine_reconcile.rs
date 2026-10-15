mod support;

use std::str::FromStr;

use pod::Clock;
use pod::engine::{reconcile, wake};
use pod::herdr::{AgentInfo, AgentStatus, HerdrEvent};
use pod::ids::{DispatchId, MessageId, RunId};
use pod::store::rows::messages;
use support::TestEnv;

const RUN: &str = "run_000000000831";
const TASK: &str = "task_000000000831";
const DISPATCH: &str = "ctx_000000000831";
const PANE: &str = "pane-reconcile";

fn seed(env: &TestEnv) {
    let now = Clock::now(env.clock.as_ref());
    env.engine.store.tx(|tx| {
        tx.0.execute("INSERT INTO runs (id, objective, coordinator_terminal_id, coordinator_handle, consumer_generation, home_cwd, depth, created_at, updated_at) VALUES (?1, 'reconcile', 'term-c', 'c', 1, '/tmp', 0, ?2, ?2)", (RUN, now.to_rfc3339()))?;
        tx.0.execute("INSERT INTO tasks (id, run_id, spec, status, deps, created_by_terminal_id, created_at) VALUES (?1, ?2, 'spec', 'dispatched', '[]', 'term-c', ?3)", (TASK, RUN, now.to_rfc3339()))?;
        tx.0.execute("INSERT INTO dispatches (id, run_id, task_id, status, failure_count, depth, assignee_handle, assignee_terminal_id, agent_kind, created_at) VALUES (?1, ?2, ?3, 'dispatched', 0, 1, 'worker', 'term-worker', 'codex', ?4)", (DISPATCH, RUN, TASK, now.to_rfc3339()))?;
        tx.0.execute("INSERT INTO workers (dispatch_id, state, pane_id, terminal_id, terminal_state, owns_pane, created_at, updated_at) VALUES (?1, 'ready', ?2, 'term-worker', 'active', 1, ?3, ?3)", (DISPATCH, PANE, now.to_rfc3339()))?;
        Ok(())
    }).unwrap();
}

#[test]
fn startup_reconcile_marks_missing_panes_exited_never_failed() {
    let env = TestEnv::new();
    seed(&env);
    let report = reconcile::startup(&env.engine).unwrap();
    assert_eq!(report.missing, 1);
    let dispatch = DispatchId::from_str(DISPATCH).unwrap();
    let (status, state, projection) = env
        .engine
        .store
        .read(|tx| {
            let status = pod::store::rows::dispatches::get(tx, &dispatch)?
                .unwrap()
                .status;
            let state = pod::store::rows::workers::get(tx, &dispatch)?
                .unwrap()
                .state;
            let projection =
                pod::engine::observe::project(tx, Clock::now(env.clock.as_ref()), &dispatch)?;
            Ok((status, state, projection))
        })
        .unwrap();
    assert_eq!(status, "dispatched");
    assert_eq!(state, "ready");
    assert_eq!(projection.liveness, "exited");
    assert_eq!(projection.presence.as_deref(), Some("absent"));
}

#[test]
fn event_idle_reoffers_pending_mail() {
    let env = TestEnv::new();
    seed(&env);
    env.herdr.add_pane(PANE, "term-worker", "ws");
    env.herdr.set_screen(PANE, "occupied");
    env.herdr.set_agent(
        "worker",
        AgentInfo {
            name: "worker".into(),
            kind: "codex".into(),
            pane_id: PANE.into(),
            pid: 1,
            status: AgentStatus::Idle,
            session_id: None,
            session_path: None,
            needs_input: vec![],
            observed_at: chrono::Utc::now(),
        },
    );
    let now = Clock::now(env.clock.as_ref());
    env.engine
        .store
        .tx(|tx| {
            messages::insert(
                tx,
                &messages::MessageRow {
                    sequence: None,
                    id: MessageId::from_str("msg_000000000831")?,
                    run_id: RunId::from_str(RUN)?,
                    mailbox: format!("dispatch:{DISPATCH}"),
                    from_handle: "c".into(),
                    subject: "mail".into(),
                    body: None,
                    r#type: "status".into(),
                    priority: "normal".into(),
                    thread_id: None,
                    payload: None,
                    read: false,
                    created_at: now,
                    delivered_at: None,
                    sender_terminal_id: Some("term-c".into()),
                    rejected_reason: None,
                    evidence: None,
                    recommendation: None,
                },
            )?;
            Ok(())
        })
        .unwrap();
    reconcile::event(
        &env.engine,
        HerdrEvent::AgentStatusChanged {
            pane_id: PANE.into(),
            status: AgentStatus::Idle,
            needs_input: vec![],
        },
    )
    .unwrap();
    let ladder = env
        .engine
        .store
        .read(|tx| wake::ladder_position(tx, &format!("dispatch:{DISPATCH}")))
        .unwrap();
    assert_eq!(ladder.attempts, 1);
}

#[test]
fn store_access_denied_has_agent_specific_fix() {
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = ENV_LOCK.lock().unwrap();
    let old_codex = std::env::var_os("CODEX_SESSION_ID");
    let old_claude = std::env::var_os("CLAUDECODE");
    unsafe { std::env::set_var("CODEX_SESSION_ID", "test") };
    unsafe { std::env::remove_var("CLAUDECODE") };
    let codex = pod::PodError::store("denied");
    assert_eq!(
        codex.next_steps,
        vec![
            "Add the state directory to the Codex sandbox's writable roots (for the workspace-write sandbox, an additional writable root), or run Codex with a sandbox mode that allows it. Restart the agent after changing it."
        ]
    );
    unsafe { std::env::remove_var("CODEX_SESSION_ID") };
    unsafe { std::env::set_var("CLAUDECODE", "1") };
    let claude = pod::PodError::store("denied");
    assert_eq!(
        claude.next_steps,
        vec![
            "Allow writes to the pod state directory in Claude Code's sandbox settings (the filesystem write allowlist), then restart the session. Alternatively set POD_STATE_DIR to a directory inside the project that the sandbox already permits."
        ]
    );
    match old_codex {
        Some(value) => unsafe { std::env::set_var("CODEX_SESSION_ID", value) },
        None => unsafe { std::env::remove_var("CODEX_SESSION_ID") },
    }
    match old_claude {
        Some(value) => unsafe { std::env::set_var("CLAUDECODE", value) },
        None => unsafe { std::env::remove_var("CLAUDECODE") },
    }
}

#[test]
fn closed_pane_is_absent_and_unrecorded_pane_is_not() {
    let env = TestEnv::new();
    seed(&env);
    env.herdr.add_pane(PANE, "term-worker", "ws");
    reconcile::event(
        &env.engine,
        HerdrEvent::PaneClosed {
            pane_id: PANE.into(),
        },
    )
    .unwrap();
    let dispatch = DispatchId::from_str(DISPATCH).unwrap();
    let projection = env
        .engine
        .store
        .read(|tx| pod::engine::observe::project(tx, Clock::now(env.clock.as_ref()), &dispatch))
        .unwrap();
    assert_eq!(projection.presence.as_deref(), Some("absent"));
    assert_eq!(projection.liveness, "exited");

    // A worker whose pane was never recorded proves nothing either way.
    let env = TestEnv::new();
    seed(&env);
    env.engine
        .store
        .tx(|tx| {
            tx.0.execute("UPDATE workers SET pane_id = NULL", [])?;
            Ok(())
        })
        .unwrap();
    let report = reconcile::startup(&env.engine).unwrap();
    assert_eq!((report.missing, report.unproven), (0, 1));
    let projection = env
        .engine
        .store
        .read(|tx| pod::engine::observe::project(tx, Clock::now(env.clock.as_ref()), &dispatch))
        .unwrap();
    assert_eq!(projection.liveness, "unverifiable");
}

#[test]
fn hook_reads_protocol_22_event_data() {
    // herdr's EventData for an agent status change (schema-22.json).
    let event = pod::cli::commands::env::hook_event_from(
        Some("pane_agent_status_changed"),
        r#"{"type":"pane_agent_status_changed","pane_id":"w1:p1","workspace_id":"w1","agent":"codex","agent_status":"done","state_labels":{"approval":"approval"}}"#,
    );
    assert_eq!(
        event,
        Some(HerdrEvent::AgentStatusChanged {
            pane_id: "w1:p1".into(),
            status: AgentStatus::Done,
            needs_input: vec!["approval".into()],
        })
    );
    assert_eq!(
        pod::cli::commands::env::hook_event_from(
            None,
            r#"{"type":"pane_closed","pane_id":"w1:p1","workspace_id":"w1"}"#
        ),
        Some(HerdrEvent::PaneClosed {
            pane_id: "w1:p1".into()
        })
    );
    // Malformed or unrecognized payloads fall back to reconciliation.
    assert_eq!(
        pod::cli::commands::env::hook_event_from(None, "not json"),
        None
    );
    assert_eq!(
        pod::cli::commands::env::hook_event_from(None, r#"{"type":"tab_created"}"#),
        None
    );
}

#[test]
fn idle_coordinator_pane_reoffers_run_mail() {
    let env = TestEnv::new();
    seed(&env);
    env.herdr.add_pane("pane-c", "term-c", "ws");
    env.herdr.set_screen("pane-c", "› ");
    env.herdr.set_agent(
        "coordinator",
        AgentInfo {
            name: "coordinator".into(),
            kind: "codex".into(),
            pane_id: "pane-c".into(),
            pid: 2,
            status: AgentStatus::Done,
            session_id: None,
            session_path: None,
            needs_input: vec![],
            observed_at: chrono::Utc::now(),
        },
    );
    let now = Clock::now(env.clock.as_ref());
    env.engine
        .store
        .tx(|tx| {
            messages::insert(
                tx,
                &messages::MessageRow {
                    sequence: None,
                    id: MessageId::from_str("msg_000000000832")?,
                    run_id: RunId::from_str(RUN)?,
                    mailbox: format!("run:{RUN}"),
                    from_handle: "worker".into(),
                    subject: "done".into(),
                    body: None,
                    r#type: "status".into(),
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
    reconcile::event(
        &env.engine,
        HerdrEvent::AgentStatusChanged {
            pane_id: "pane-c".into(),
            status: AgentStatus::Done,
            needs_input: vec![],
        },
    )
    .unwrap();
    let ladder = env
        .engine
        .store
        .read(|tx| wake::ladder_position(tx, &format!("run:{RUN}")))
        .unwrap();
    assert_eq!(ladder.attempts, 1);
    assert_eq!(env.herdr.submissions().len(), 1);
}
