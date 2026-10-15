//! Spec rules (sections 3 to 13) that no other test pinned, found by the
//! final review's rule-by-rule pass. Each test names its spec section.

mod support;

use std::str::FromStr;
use std::time::Duration;

use pod::ErrorCode;
use pod::engine::lifecycle::{self, Setup, StartInput, Worktree};
use pod::engine::messages::{self, SendInput};
use pod::engine::{deliveries, gates, questions, reconcile, runs, tasks, waiting, wake};
use pod::herdr::{AgentStatus, HerdrEvent};
use pod::ids::{DispatchId, RunId, TaskId};
use pod::store::rows::{dispatches, events, wake_attempts, workers};
use serde_json::Value;
use support::TestEnv;

const COORDINATOR: &str = "term-coord";

fn seeded() -> (TestEnv, RunId) {
    let mut env = TestEnv::new();
    env.as_caller(COORDINATOR, std::process::id());
    let caller = env.engine.caller.clone().unwrap();
    let run = env
        .engine
        .store
        .tx(|tx| runs::create(tx, env.engine.clock.now(), "run", &caller, env.tmp.path()))
        .unwrap();
    (env, run.id)
}

fn new_task(env: &TestEnv, run: &RunId, spec: &str, deps: &[TaskId]) -> TaskId {
    env.engine
        .store
        .tx(|tx| {
            tasks::create(
                tx,
                env.engine.clock.now(),
                run,
                spec,
                Some(spec),
                None,
                deps,
                None,
                COORDINATOR,
            )
        })
        .unwrap()
        .id
}

fn input(task: &TaskId, run: &RunId) -> StartInput {
    StartInput {
        task: Some(task.clone()),
        spec: None,
        agent: Some("codex".into()),
        terminal: None,
        worktree: Worktree::Current,
        name: None,
        repo: None,
        base_branch: None,
        display_name: None,
        comment: None,
        setup: Setup::Run,
        model: None,
        effort: None,
        fast: false,
        yolo: false,
        task_title: None,
        deps: Vec::new(),
        parent: None,
        retry_of: None,
        timeout_ms: Some(2_000),
        run: Some(run.clone()),
        handle: None,
    }
}

/// Starts a worker for a new Task as the coordinator.
fn start(env: &mut TestEnv, run: &RunId, spec: &str) -> lifecycle::StartReceipt {
    env.as_caller(COORDINATOR, std::process::id());
    let task = new_task(env, run, spec, &[]);
    let receipt = lifecycle::worker_start(&env.engine, input(&task, run)).unwrap();
    assert_eq!(receipt.state, "ready", "{receipt:?}");
    receipt
}

fn worker(env: &TestEnv, dispatch: &DispatchId) -> workers::WorkerRow {
    env.engine
        .store
        .read(|tx| workers::get(tx, dispatch))
        .unwrap()
        .unwrap()
}

fn dispatch_row(env: &TestEnv, dispatch: &DispatchId) -> dispatches::DispatchRow {
    env.engine
        .store
        .read(|tx| dispatches::get(tx, dispatch))
        .unwrap()
        .unwrap()
}

/// Acts as the worker of `dispatch`: its terminal, and a process the
/// recorded agent (the fake's PID 1) is an ancestor of.
fn as_worker(env: &mut TestEnv, dispatch: &DispatchId) {
    let terminal = worker(env, dispatch).terminal_id.unwrap();
    env.as_caller(&terminal, std::process::id());
}

fn send(kind: &str, receipt: &lifecycle::StartReceipt) -> SendInput {
    SendInput {
        to: None,
        subject: "subject".into(),
        body: Some("body".into()),
        r#type: kind.into(),
        priority: "normal".into(),
        thread_id: None,
        payload: None,
        task_id: Some(receipt.task_id.clone()),
        dispatch_id: Some(receipt.dispatch_id.clone()),
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

fn event_types(env: &TestEnv, run: &RunId) -> Vec<String> {
    env.engine
        .store
        .read(|tx| events::list(tx, run))
        .unwrap()
        .into_iter()
        .map(|event| event.r#type)
        .collect()
}

fn idle(pane: &str) -> HerdrEvent {
    HerdrEvent::AgentStatusChanged {
        pane_id: pane.into(),
        status: AgentStatus::Idle,
        needs_input: Vec::new(),
    }
}

#[test]
fn spec_3_2_a_waiter_subscribes_to_its_runs_worker_panes() {
    let (mut env, run) = seeded();
    let started = start(&mut env, &run, "work");
    env.as_caller(COORDINATOR, std::process::id());
    let pane = worker(&env, &started.dispatch_id).pane_id.unwrap();
    waiting::check_wait(&env.engine, &["status".into()], 50).unwrap();
    assert!(
        env.herdr
            .calls()
            .iter()
            .any(|call| call.starts_with("events_subscribe:") && call.contains(&pane)),
        "{:?}",
        env.herdr.calls()
    );
}

#[test]
fn spec_3_3_the_plugin_state_dir_is_never_used() {
    let resolve = |plugin_dir: Option<&str>| {
        let env = |name: &str| match name {
            "HOME" => Some("/Users/me".to_owned()),
            "HERDR_PLUGIN_STATE_DIR" => plugin_dir.map(str::to_owned),
            _ => None,
        };
        pod::paths::resolve(&env).unwrap()
    };
    assert_eq!(resolve(Some("/plugins/pod/state")), resolve(None));
}

#[test]
fn spec_4_ask_timeout_is_capped_at_1800000() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    std::fs::write(&path, "[defaults]\nask_timeout_ms = 9000000\n").unwrap();
    assert_eq!(
        pod::Config::load(&path).unwrap().defaults.ask_timeout_ms,
        1_800_000
    );
}

#[test]
fn spec_5_one_outstanding_delivery_per_mailbox_and_a_fenced_one_replays() {
    let (mut env, run) = seeded();
    let started = start(&mut env, &run, "work");
    as_worker(&mut env, &started.dispatch_id);
    messages::send(&env.engine, send("status", &started)).unwrap();
    env.as_caller(COORDINATOR, std::process::id());
    let mailbox = format!("run:{run}");
    let first = deliveries::check_consume(&env.engine, &mailbox).unwrap();
    let again = deliveries::check_consume(&env.engine, &mailbox).unwrap();
    assert_eq!(first.id, again.id);
    assert_eq!(first.message_ids.len(), 1);

    // A new coordinator session runs run-use: the outstanding Delivery is
    // fenced and its messages are offered again in a new one (spec 5, 13).
    let caller = env.engine.caller.clone().unwrap();
    env.engine
        .store
        .tx(|tx| runs::use_run(tx, env.engine.clock.now(), &run, &caller))
        .unwrap();
    let replay = deliveries::check_consume(&env.engine, &mailbox).unwrap();
    assert_ne!(replay.id, first.id);
    assert_eq!(replay.message_ids, first.message_ids);
}

#[test]
fn spec_6_outcome_and_reason_flags_need_worker_done() {
    let (mut env, run) = seeded();
    let started = start(&mut env, &run, "work");
    as_worker(&mut env, &started.dispatch_id);
    for (outcome, reason) in [(Some("succeeded"), None), (None, Some("no_follow_on"))] {
        let mut status = send("status", &started);
        status.outcome = outcome.map(str::to_owned);
        status.reason = reason.map(str::to_owned);
        assert_eq!(
            messages::send(&env.engine, status).unwrap_err().code,
            ErrorCode::Usage
        );
    }
}

#[test]
fn spec_7_1_the_worker_tab_is_created_without_focus() {
    let (mut env, run) = seeded();
    start(&mut env, &run, "work");
    let calls = env.herdr.calls();
    assert!(calls.iter().any(|call| call.starts_with("tab_create:")));
    assert!(!calls.iter().any(|call| call.starts_with("tab_focus:")));
}

#[test]
fn spec_7_1_a_new_worktree_worker_uses_the_worktrees_first_tab() {
    let (env, run) = seeded();
    let task = new_task(&env, &run, "work", &[]);
    let mut start = input(&task, &run);
    start.worktree = Worktree::NewChild;
    start.name = Some("child".into());
    start.repo = Some(format!("path:{}", env.tmp.path().display()));
    let receipt = lifecycle::worker_start(&env.engine, start).unwrap();
    assert_eq!(receipt.state, "ready", "{receipt:?}");
    let calls = env.herdr.calls();
    assert!(
        calls
            .iter()
            .any(|call| call.starts_with("worktree_create:child:"))
    );
    assert!(!calls.iter().any(|call| call.starts_with("tab_create:")));
    assert!(worker(&env, &receipt.dispatch_id).owns_pane);
}

#[test]
fn spec_7_3_setup_skip_runs_no_setup_in_a_new_worktree() {
    let (mut env, run) = seeded();
    env.engine.config.repos.insert(
        env.tmp.path().to_path_buf(),
        pod::config::RepoConfig {
            setup: vec!["sh".into(), "setup.sh".into()],
            ..pod::config::RepoConfig::default()
        },
    );
    let task = new_task(&env, &run, "work", &[]);
    let mut start = input(&task, &run);
    start.worktree = Worktree::NewChild;
    start.name = Some("skipped".into());
    start.repo = Some(format!("path:{}", env.tmp.path().display()));
    start.setup = Setup::Skip;
    let receipt = lifecycle::worker_start(&env.engine, start).unwrap();
    assert_eq!(receipt.state, "ready", "{receipt:?}");
    assert_eq!(receipt.setup.requested, "skip");
    assert_eq!(receipt.setup.effective, "skip");
    assert!(worker(&env, &receipt.dispatch_id).setup_pane_id.is_none());
    assert!(
        env.herdr
            .submissions()
            .iter()
            .all(|(_, text)| !text.contains("setup.sh"))
    );
}

#[test]
fn spec_7_4_looking_idle_is_never_completion() {
    let (mut env, run) = seeded();
    let started = start(&mut env, &run, "work");
    let pane = worker(&env, &started.dispatch_id).pane_id.unwrap();
    for status in [AgentStatus::Idle, AgentStatus::Done] {
        reconcile::event(
            &env.engine,
            HerdrEvent::AgentStatusChanged {
                pane_id: pane.clone(),
                status,
                needs_input: Vec::new(),
            },
        )
        .unwrap();
    }
    assert_eq!(
        dispatch_row(&env, &started.dispatch_id).status,
        "dispatched"
    );
    let task = env
        .engine
        .store
        .read(|tx| tasks::get(tx, &started.task_id))
        .unwrap()
        .unwrap();
    assert_eq!(task.status, "dispatched");
    assert!(
        !event_types(&env, &run)
            .iter()
            .any(|t| t == "dispatch.completed")
    );
}

#[test]
fn spec_7_4_retain_keeps_the_pane_with_its_reason() {
    let (mut env, run) = seeded();
    let started = start(&mut env, &run, "work");
    as_worker(&mut env, &started.dispatch_id);
    let mut done = send("worker_done", &started);
    done.outcome = Some("succeeded".into());
    messages::send(&env.engine, done).unwrap();
    env.as_caller(COORDINATOR, std::process::id());
    let receipt =
        lifecycle::retain(&env.engine, &started.dispatch_id, "person reviews the diff").unwrap();
    assert_eq!(receipt.state, "retained");
    assert_eq!(
        receipt.worker.retained_reason.as_deref(),
        Some("person reviews the diff")
    );
    assert!(
        !env.herdr
            .calls()
            .iter()
            .any(|call| call.starts_with("pane_close:") || call.starts_with("tab_close:"))
    );
}

#[test]
fn spec_7_5_retry_of_requires_task() {
    let (env, run) = seeded();
    let task = new_task(&env, &run, "work", &[]);
    let mut start = input(&task, &run);
    start.task = None;
    start.spec = Some("fresh work".into());
    start.retry_of = Some(DispatchId::new());
    assert_eq!(
        lifecycle::worker_start(&env.engine, start)
            .unwrap_err()
            .code,
        ErrorCode::Usage
    );
}

#[test]
fn spec_8_1_worker_done_and_heartbeat_always_go_to_the_run_mailbox() {
    let (mut env, run) = seeded();
    let other = start(&mut env, &run, "other");
    let started = start(&mut env, &run, "work");
    as_worker(&mut env, &started.dispatch_id);
    let mut heartbeat = send("heartbeat", &started);
    heartbeat.to = Some(format!("dispatch:{}", other.dispatch_id));
    heartbeat.phase = Some("implementing".into());
    assert_eq!(
        messages::send(&env.engine, heartbeat).unwrap().mailbox,
        format!("run:{run}")
    );
    let mut done = send("worker_done", &started);
    done.to = Some(format!("dispatch:{}", other.dispatch_id));
    done.outcome = Some("succeeded".into());
    assert_eq!(
        messages::send(&env.engine, done).unwrap().mailbox,
        format!("run:{run}")
    );
}

#[test]
fn spec_8_1_closure_reason_and_target_are_shown_by_show_task_list_and_log() {
    let (mut env, run) = seeded();
    let blocker = new_task(&env, &run, "blocker", &[]);
    let started = start(&mut env, &run, "work");
    as_worker(&mut env, &started.dispatch_id);
    let mut done = send("worker_done", &started);
    done.outcome = Some("failed".into());
    done.reason = Some("blocked_on".into());
    done.reason_target = Some(blocker.to_string());
    messages::send(&env.engine, done).unwrap();

    let view = lifecycle::show(&env.engine, &started.dispatch_id).unwrap();
    assert_eq!(
        view.dispatch.termination_reason.as_deref(),
        Some("blocked_on")
    );
    assert_eq!(
        view.dispatch.termination_target.as_deref(),
        Some(blocker.as_str())
    );
    let closure = env
        .engine
        .store
        .read(|tx| tasks::latest_closure(tx, &started.task_id))
        .unwrap()
        .unwrap();
    assert_eq!(closure.termination_reason.as_deref(), Some("blocked_on"));
    let logged = env
        .engine
        .store
        .read(|tx| events::list(tx, &run))
        .unwrap()
        .into_iter()
        .find(|event| event.r#type == "dispatch.failed")
        .unwrap();
    let detail = logged.detail.unwrap();
    assert_eq!(detail["termination_reason"], "blocked_on");
    assert_eq!(detail["termination_target"], blocker.as_str());
}

#[test]
fn spec_8_1_all_seven_reasons_are_valid_with_either_outcome() {
    for outcome in ["succeeded", "failed"] {
        for reason in [
            "no_follow_on",
            "handed_off",
            "blocked_on",
            "denied",
            "canceled",
            "escalated",
            "superseded",
        ] {
            let (mut env, run) = seeded();
            let started = start(&mut env, &run, "work");
            as_worker(&mut env, &started.dispatch_id);
            let mut done = send("worker_done", &started);
            done.outcome = Some(outcome.into());
            done.reason = Some(reason.into());
            if matches!(reason, "handed_off" | "blocked_on" | "superseded") {
                done.reason_target = Some(started.task_id.to_string());
            }
            let receipt = messages::send(&env.engine, done)
                .unwrap_or_else(|error| panic!("{outcome}/{reason}: {error:?}"));
            assert!(receipt.settled.is_some(), "{outcome}/{reason}");
            assert_eq!(
                dispatch_row(&env, &started.dispatch_id)
                    .termination_reason
                    .as_deref(),
                Some(reason)
            );
        }
    }
}

#[test]
fn spec_8_2_a_failed_nudge_is_recorded_and_counts_toward_the_ladder() {
    let (mut env, run) = seeded();
    let started = start(&mut env, &run, "work");
    let pane = worker(&env, &started.dispatch_id).pane_id.unwrap();
    reconcile::event(&env.engine, idle(&pane)).unwrap();
    env.clock.advance(Duration::from_secs(3));
    // herdr cannot prompt, so the nudge falls back to the paste, which fails.
    env.herdr.fail_agent_prompt("agent.prompt unsupported");
    env.herdr.fail_pane_send_text("pane is gone");
    let mut mail = send("status", &started);
    mail.to = Some(format!("dispatch:{}", started.dispatch_id));
    mail.task_id = None;
    mail.dispatch_id = None;
    messages::send(&env.engine, mail).unwrap();
    let mailbox = format!("dispatch:{}", started.dispatch_id);
    let attempts = env
        .engine
        .store
        .read(|tx| wake_attempts::list_since_last_consume(tx, &mailbox))
        .unwrap();
    assert_eq!(attempts.len(), 1, "{attempts:?}");
    assert_eq!(attempts[0].outcome, "failed");
    let ladder = env
        .engine
        .store
        .read(|tx| wake::ladder_position(tx, &mailbox))
        .unwrap();
    assert_eq!(ladder.attempts, 1);
}

/// Spec 8.2: an exhausted Run mailbox is written to the board for the user;
/// spec 13: the message stays stored and unread and is never retyped.
#[test]
fn spec_8_2_an_exhausted_run_mailbox_reaches_the_board_and_mail_stays_unread() {
    let (mut env, run) = seeded();
    let started = start(&mut env, &run, "work");
    as_worker(&mut env, &started.dispatch_id);
    let mut mail = send("status", &started);
    mail.body = Some("unique body text".into());
    messages::send(&env.engine, mail).unwrap();
    let mailbox = format!("run:{run}");
    // The coordinator is not a detected agent, so every attempt is skipped.
    for _ in 0..5 {
        env.clock.advance(Duration::from_secs(61));
        wake::on_trigger(&env.engine, &mailbox, wake::Trigger::MutatingCommand).unwrap();
    }
    let ladder = env
        .engine
        .store
        .read(|tx| wake::ladder_position(tx, &mailbox))
        .unwrap();
    assert!(ladder.exhausted, "{ladder:?}");
    let board = env
        .engine
        .store
        .read(|tx| pod::board::export(tx, &run, env.engine.clock.now()))
        .unwrap();
    let unreachable = board["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| {
            event["type"] == "note"
                && event["detail"]
                    .as_str()
                    .is_some_and(|detail| detail.starts_with("mailbox.unreachable: "))
        })
        .collect::<Vec<_>>();
    assert_eq!(unreachable.len(), 1, "{board}");
    assert!(
        unreachable[0]["detail"]
            .as_str()
            .unwrap()
            .contains(&format!("\"mailbox\":\"{mailbox}\"")),
        "{}",
        unreachable[0]["detail"]
    );
    let unread = deliveries::check_peek(&env.engine, &mailbox, &[]).unwrap();
    assert_eq!(unread.len(), 1);
    assert!(
        env.herdr
            .submissions()
            .iter()
            .all(|(_, text)| !text.contains("unique body text"))
    );
}

#[test]
fn spec_8_5_decision_gate_needs_the_owner_and_changes_no_state() {
    let (mut env, run) = seeded();
    let started = start(&mut env, &run, "work");
    env.as_caller("term-stranger", std::process::id());
    let error = messages::send(&env.engine, send("decision_gate", &started)).unwrap_err();
    assert_eq!(error.code, ErrorCode::CallerFenced);

    as_worker(&mut env, &started.dispatch_id);
    let receipt = messages::send(&env.engine, send("decision_gate", &started)).unwrap();
    let stored = env
        .engine
        .store
        .read(|tx| pod::store::rows::messages::get(tx, &receipt.message_ids[0]))
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.payload.unwrap()["dispatch_id"],
        started.dispatch_id.as_str()
    );
    assert_eq!(
        dispatch_row(&env, &started.dispatch_id).status,
        "dispatched"
    );
}

#[test]
fn spec_8_5_priority_adds_a_tag_when_rendered() {
    let (mut env, run) = seeded();
    let started = start(&mut env, &run, "work");
    as_worker(&mut env, &started.dispatch_id);
    for priority in ["high", "urgent"] {
        let mut mail = send("status", &started);
        mail.priority = priority.into();
        mail.subject = format!("{priority} subject");
        messages::send(&env.engine, mail).unwrap();
    }
    let output = env
        .pod(None)
        .args([
            "check",
            "--peek",
            "--run",
            run.as_str(),
            "--format",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("[HIGH] [status]"), "{text}");
    assert!(text.contains("[URGENT] [status]"), "{text}");
}

#[test]
fn spec_9_parent_must_be_in_the_same_run() {
    let (env, run) = seeded();
    let caller = env.engine.caller.clone().unwrap();
    let other = env
        .engine
        .store
        .tx(|tx| runs::create(tx, env.engine.clock.now(), "other", &caller, env.tmp.path()))
        .unwrap();
    let parent = new_task(&env, &other.id, "elsewhere", &[]);
    let refused = env.engine.store.tx(|tx| {
        tasks::create(
            tx,
            env.engine.clock.now(),
            &run,
            "child",
            None,
            None,
            &[],
            Some(&parent),
            COORDINATOR,
        )
    });
    let error = refused.unwrap_err();
    assert_eq!(error.code, ErrorCode::TaskNotFound, "{error:?}");
}

#[test]
fn spec_9_coordinator_gate_resolve_sends_no_message_and_the_next_preamble_has_the_decision() {
    let (env, run) = seeded();
    let task = new_task(&env, &run, "work", &[]);
    let gate = env
        .engine
        .store
        .tx(|tx| {
            gates::create(
                tx,
                env.engine.clock.now(),
                &run,
                "coordinator",
                Some(&task),
                "Which schema?",
                &[],
                None,
                None,
                false,
            )
        })
        .unwrap();
    let (_, message) = env
        .engine
        .store
        .tx(|tx| gates::resolve(tx, env.engine.clock.now(), &gate.id, "schema v2"))
        .unwrap();
    assert!(message.is_none());
    let start = lifecycle::worker_start(&env.engine, input(&task, &run)).unwrap();
    assert_eq!(start.state, "ready", "{start:?}");
    let preamble = &support::brief_of(&env.herdr.submissions()[0].1);
    assert!(preamble.contains("=== DECISION ==="), "{preamble}");
    assert!(preamble.contains("Which schema?") && preamble.contains("schema v2"));
}

#[test]
fn spec_9_worker_list_reports_tasks_waiting_on_a_failed_dependency() {
    let (env, run) = seeded();
    let failed = new_task(&env, &run, "fails", &[]);
    let waiting = new_task(&env, &run, "waits", std::slice::from_ref(&failed));
    env.engine
        .store
        .tx(|tx| pod::store::rows::tasks::update_status(tx, &failed, "failed"))
        .unwrap();
    let output = env
        .pod(None)
        .args(["worker-list", "--run", run.as_str(), "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let body: Value = serde_json::from_slice(&output.stdout).unwrap();
    let entry = &body["waitingOnFailedDependency"][0];
    assert_eq!(entry["taskId"], waiting.as_str());
    assert_eq!(entry["failedDependency"], failed.as_str());
    assert_eq!(entry["nextAction"]["argv"][1], "dispatch-show");
}

#[test]
fn spec_9_1_a_question_survives_the_loss_of_its_specialist() {
    let (mut env, run) = seeded();
    let specialist = start(&mut env, &run, "specialist");
    let asker = start(&mut env, &run, "asker");
    as_worker(&mut env, &asker.dispatch_id);
    let to = format!("dispatch:{}", specialist.dispatch_id);
    let (question, _) = questions::ask_store(&env.engine, "Which port?", &[], Some(&to)).unwrap();
    env.as_caller(COORDINATOR, std::process::id());
    lifecycle::stop(&env.engine, &specialist.dispatch_id).unwrap();
    let row = env
        .engine
        .store
        .read(|tx| pod::store::rows::questions::get(tx, &question))
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "pending");
    assert_eq!(row.dispatch_id, Some(asker.dispatch_id));
    let answered = questions::reply(&env.engine, &question, "8080").unwrap();
    assert_eq!(answered.status, "answered");
}

#[test]
fn spec_9_1_ask_to_an_unknown_or_other_run_dispatch_is_recipient_not_live() {
    let (mut env, run) = seeded();
    let asker = start(&mut env, &run, "asker");
    let caller = env.engine.caller.clone().unwrap();
    let other_run = env
        .engine
        .store
        .tx(|tx| runs::create(tx, env.engine.clock.now(), "other", &caller, env.tmp.path()))
        .unwrap();
    let elsewhere = start(&mut env, &other_run.id, "elsewhere");
    as_worker(&mut env, &asker.dispatch_id);
    for target in [DispatchId::new(), elsewhere.dispatch_id] {
        let to = format!("dispatch:{target}");
        let error = questions::ask_store(&env.engine, "?", &[], Some(&to)).unwrap_err();
        assert_eq!(error.code, ErrorCode::RecipientNotLive, "{target}");
    }
}

#[test]
fn spec_9_1_the_preamble_lists_live_specialists_and_not_the_worker_itself() {
    let (mut env, run) = seeded();
    let specialist = start(&mut env, &run, "specialist");
    let second = start(&mut env, &run, "second");
    let preamble = env
        .herdr
        .submissions()
        .into_iter()
        .find(|(_, text)| text.contains(second.dispatch_id.as_str()))
        .map(|(_, text)| support::brief_of(&text))
        .unwrap();
    let section = preamble
        .split("=== SPECIALISTS ===")
        .nth(1)
        .unwrap_or_else(|| panic!("{preamble}"))
        .split("===")
        .next()
        .unwrap();
    assert!(
        section.contains(specialist.dispatch_id.as_str()),
        "{section}"
    );
    assert!(section.contains(&specialist.handle));
    assert!(!section.contains(second.dispatch_id.as_str()));
}

#[test]
fn spec_9_1_a_user_gate_with_a_task_blocks_it_and_peek_shows_the_resolution_while_unread() {
    let (env, run) = seeded();
    let task = new_task(&env, &run, "work", &[]);
    let gate = env
        .engine
        .store
        .tx(|tx| {
            gates::create(
                tx,
                env.engine.clock.now(),
                &run,
                "user",
                Some(&task),
                "Ship now?",
                &["yes".into(), "no".into()],
                Some("notes/review.md"),
                Some("yes"),
                false,
            )
        })
        .unwrap();
    let status = |env: &TestEnv| {
        env.engine
            .store
            .read(|tx| tasks::get(tx, &task))
            .unwrap()
            .unwrap()
            .status
    };
    assert_eq!(status(&env), "blocked");
    env.engine
        .store
        .tx(|tx| gates::resolve(tx, env.engine.clock.now(), &gate.id, "yes"))
        .unwrap();
    assert_eq!(status(&env), "ready");
    let mailbox = format!("run:{run}");
    let types = ["decision_gate".to_owned()];
    assert_eq!(
        deliveries::check_peek(&env.engine, &mailbox, &types)
            .unwrap()
            .len(),
        1
    );
    let delivery = deliveries::check_consume(&env.engine, &mailbox).unwrap();
    deliveries::check_ack(&env.engine, &delivery.id).unwrap();
    assert!(
        deliveries::check_peek(&env.engine, &mailbox, &types)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn spec_10_sub_dispatch_appears_only_below_the_depth_limit() {
    for (max, expected) in [(1, false), (2, true)] {
        let (mut env, run) = seeded();
        env.engine.config.defaults.nested_depth_max = max;
        start(&mut env, &run, "work");
        let preamble = &support::brief_of(&env.herdr.submissions()[0].1);
        assert_eq!(
            preamble.contains("=== SUB-DISPATCH ==="),
            expected,
            "max {max}"
        );
    }
}

#[test]
fn spec_12_a_coordinator_gate_is_a_gate_card_with_its_question() {
    let (env, run) = seeded();
    let task = new_task(&env, &run, "work", &[]);
    env.engine
        .store
        .tx(|tx| {
            gates::create(
                tx,
                env.engine.clock.now(),
                &run,
                "coordinator",
                Some(&task),
                "Split the task?",
                &[],
                None,
                None,
                false,
            )
        })
        .unwrap();
    let board = env
        .engine
        .store
        .read(|tx| pod::board::export(tx, &run, env.engine.clock.now()))
        .unwrap();
    let card = board["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|card| card["id"] == task.as_str())
        .unwrap();
    // dagr's `inputs` are task ids and its `gate` is a fan-in; a decision a
    // person must make is a `question` task whose `criteria` is the question.
    assert_eq!(card["kind"], "question");
    assert_eq!(card["state"], "blocked");
    assert_eq!(card["criteria"], "Split the task?");
}

#[test]
fn spec_13_pod_never_answers_an_approval_prompt() {
    let (mut env, run) = seeded();
    let started = start(&mut env, &run, "work");
    let pane = worker(&env, &started.dispatch_id).pane_id.unwrap();
    let typed_before = env.herdr.submissions().len();
    reconcile::event(
        &env.engine,
        HerdrEvent::AgentStatusChanged {
            pane_id: pane.clone(),
            status: AgentStatus::Blocked,
            needs_input: vec!["approval".into()],
        },
    )
    .unwrap();
    env.as_caller(COORDINATOR, std::process::id());
    env.engine.after_commit(&run, &[]).unwrap();
    let view = lifecycle::show(&env.engine, &started.dispatch_id).unwrap();
    assert!(
        view.projection
            .attention
            .categories
            .contains(&"approval".to_owned()),
        "{:?}",
        view.projection
    );
    assert_eq!(env.herdr.submissions().len(), typed_before);
    assert!(
        !env.herdr
            .calls()
            .iter()
            .any(|call| call.starts_with(&format!("pane_send_keys:{pane}:"))
                && !call.ends_with(":Enter"))
    );
}

#[test]
fn spec_13_a_silent_worker_is_stale_but_never_auto_failed() {
    let (mut env, run) = seeded();
    let started = start(&mut env, &run, "work");
    env.clock.advance(Duration::from_secs(11 * 60));
    reconcile::startup(&env.engine).unwrap();
    env.engine.after_commit(&run, &[]).unwrap();
    let view = lifecycle::show(&env.engine, &started.dispatch_id).unwrap();
    assert!(
        view.projection
            .attention
            .categories
            .contains(&"stale".to_owned()),
        "{:?}",
        view.projection
    );
    assert_eq!(view.dispatch.status, "dispatched");
    let task = env
        .engine
        .store
        .read(|tx| tasks::get(tx, &TaskId::from_str(started.task_id.as_str()).unwrap()))
        .unwrap()
        .unwrap();
    assert_eq!(task.status, "dispatched");
}

/// Spec 3.4: `--dispatch-capability` is accepted for Orca compatibility and
/// ignored; a send with any value stores the same kind of row as one without.
#[test]
fn spec_3_4_dispatch_capability_is_accepted_and_ignored() {
    let (mut env, run) = seeded();
    let started = start(&mut env, &run, "work");
    let to = format!("dispatch:{}", started.dispatch_id);
    for extra in [&[][..], &["--dispatch-capability", "dcap_anything"][..]] {
        let output = env
            .pod(Some(COORDINATOR))
            .args(["send", "--to", &to, "--subject", "follow-up", "--json"])
            .args(extra)
            .output()
            .unwrap();
        assert!(output.status.success(), "{extra:?}: {output:?}");
        let body: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(body["message"]["mailbox"], to, "{body}");
    }
}
