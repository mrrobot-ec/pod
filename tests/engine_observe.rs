mod support;

use std::str::FromStr;
use std::time::Duration;

use pod::Clock;
use pod::engine::messages::{self, SendInput};
use pod::engine::observe::{self, Projection};
use pod::engine::questions;
use pod::herdr::AgentStatus;
use pod::ids::{DispatchId, RunId, WakeId};
use pod::store::rows::wake_attempts;
use support::TestEnv;

const RUN: &str = "run_000000000811";
const TASK: &str = "task_000000000811";
const DISPATCH: &str = "ctx_000000000811";

fn seed(env: &TestEnv) -> DispatchId {
    let now = Clock::now(env.clock.as_ref());
    env.engine.store.tx(|tx| {
        tx.0.execute("INSERT INTO runs (id, objective, coordinator_terminal_id, coordinator_handle, consumer_generation, home_cwd, depth, created_at, updated_at) VALUES (?1, 'observe', 'term-c', 'c', 1, '/tmp', 0, ?2, ?2)", (RUN, now.to_rfc3339()))?;
        tx.0.execute("INSERT INTO tasks (id, run_id, spec, status, deps, created_by_terminal_id, created_at) VALUES (?1, ?2, 'spec', 'dispatched', '[]', 'term-c', ?3)", (TASK, RUN, now.to_rfc3339()))?;
        tx.0.execute("INSERT INTO dispatches (id, run_id, task_id, status, failure_count, depth, assignee_handle, created_at) VALUES (?1, ?2, ?3, 'dispatched', 0, 1, 'worker', ?4)", (DISPATCH, RUN, TASK, now.to_rfc3339()))?;
        tx.0.execute("INSERT INTO workers (dispatch_id, state, terminal_state, owns_pane, created_at, updated_at) VALUES (?1, 'ready', 'active', 1, ?2, ?2)", (DISPATCH, now.to_rfc3339()))?;
        Ok(())
    }).unwrap();
    DispatchId::from_str(DISPATCH).unwrap()
}

fn project(env: &TestEnv, dispatch: &DispatchId) -> Projection {
    env.engine
        .store
        .read(|tx| observe::project(tx, Clock::now(env.clock.as_ref()), dispatch))
        .unwrap()
}

#[test]
fn projection_table_7_6_every_row() {
    let env = TestEnv::new();
    let dispatch = seed(&env);
    env.engine
        .store
        .tx(|tx| {
            observe::record_herdr_state(
                tx,
                Clock::now(env.clock.as_ref()),
                &dispatch,
                AgentStatus::Idle,
                &[],
                Some("present"),
                None,
            )
        })
        .unwrap();
    let live = project(&env, &dispatch);
    assert_eq!(live.liveness, "live");
    assert!(live.attention.categories.is_empty());

    env.engine
        .store
        .tx(|tx| {
            observe::record_herdr_state(
                tx,
                Clock::now(env.clock.as_ref()),
                &dispatch,
                AgentStatus::Blocked,
                &["approval".into()],
                Some("present"),
                None,
            )
        })
        .unwrap();
    let approval = project(&env, &dispatch);
    assert_eq!(approval.liveness, "live");
    assert_eq!(approval.attention.categories, vec!["approval"]);
    assert!(approval.attention.requires_action);

    env.engine
        .store
        .tx(|tx| {
            observe::record_herdr_state(
                tx,
                Clock::now(env.clock.as_ref()),
                &dispatch,
                AgentStatus::Unknown,
                &[],
                Some("exited"),
                None,
            )
        })
        .unwrap();
    let exited = project(&env, &dispatch);
    assert_eq!(exited.liveness, "exited");
    assert!(exited.attention.categories.contains(&"failure".into()));

    env.engine
        .store
        .tx(|tx| {
            observe::record_herdr_state(
                tx,
                Clock::now(env.clock.as_ref()),
                &dispatch,
                AgentStatus::Unknown,
                &[],
                None,
                None,
            )
        })
        .unwrap();
    let unknown = project(&env, &dispatch);
    assert_eq!(unknown.liveness, "unverifiable");
    assert!(
        unknown
            .attention
            .categories
            .contains(&"unverifiable".into())
    );

    // Row 2, `absent`: the pane is confirmed gone.
    record(&env, &dispatch, Some("absent"));
    let absent = project(&env, &dispatch);
    assert_eq!(absent.liveness, "exited");
    assert!(absent.attention.categories.contains(&"failure".into()));
    assert!(absent.attention.requires_action);

    // Row 4: no heartbeat for 10 minutes leaves liveness unchanged.
    record(&env, &dispatch, Some("present"));
    env.clock.advance(Duration::from_secs(601));
    let stale = project(&env, &dispatch);
    assert_eq!(stale.liveness, "live");
    assert_eq!(stale.attention.categories, vec!["stale"]);
    assert!(!stale.attention.requires_action);

    // Row 5: an exhausted wake ladder.
    let now = Clock::now(env.clock.as_ref());
    env.engine
        .store
        .tx(|tx| {
            for attempt in 1..=5 {
                wake_attempts::insert(
                    tx,
                    &wake_attempts::WakeAttemptRow {
                        id: WakeId::new(),
                        run_id: RunId::from_str(RUN)?,
                        mailbox: format!("dispatch:{DISPATCH}"),
                        delivery_id: None,
                        attempt,
                        outcome: "skipped".into(),
                        detail: Some("test".into()),
                        created_at: now,
                    },
                )?;
            }
            Ok(())
        })
        .unwrap();
    let unreachable = project(&env, &dispatch);
    assert_eq!(unreachable.liveness, "live");
    assert!(
        unreachable
            .attention
            .categories
            .contains(&"unreachable".into())
    );
    assert!(unreachable.attention.requires_action);
    assert_eq!(
        unreachable.next_action.argv,
        ["pod", "worker-show", "--dispatch", DISPATCH]
    );

    // Row 2, settled: an exit after a recorded ending is not a failure.
    env.engine
        .store
        .tx(|tx| {
            tx.0.execute(
                "UPDATE dispatches SET status = 'completed' WHERE id = ?1",
                [DISPATCH],
            )?;
            Ok(())
        })
        .unwrap();
    record(&env, &dispatch, Some("exited"));
    let settled = project(&env, &dispatch);
    assert_eq!(settled.liveness, "exited");
    assert_eq!(settled.outcome, "succeeded");
    assert!(!settled.attention.categories.contains(&"failure".into()));
    assert!(!settled.attention.categories.contains(&"stale".into()));
}

fn record(env: &TestEnv, dispatch: &DispatchId, presence: Option<&str>) {
    env.engine
        .store
        .tx(|tx| {
            observe::record_herdr_state(
                tx,
                Clock::now(env.clock.as_ref()),
                dispatch,
                AgentStatus::Idle,
                &[],
                presence,
                None,
            )
        })
        .unwrap();
}

#[test]
fn self_report_proves_activity_never_presence() {
    let env = TestEnv::new();
    let dispatch = seed(&env);
    env.engine
        .store
        .tx(|tx| {
            observe::record_self_report(tx, Clock::now(env.clock.as_ref()), &dispatch, "working")
        })
        .unwrap();
    let value = project(&env, &dispatch);
    assert_eq!(value.activity, "working");
    assert_eq!(value.presence, None);
    assert_eq!(value.liveness, "unverifiable");
}

#[test]
fn herdr_state_proves_presence() {
    let env = TestEnv::new();
    let dispatch = seed(&env);
    env.engine
        .store
        .tx(|tx| {
            observe::record_herdr_state(
                tx,
                Clock::now(env.clock.as_ref()),
                &dispatch,
                AgentStatus::Unknown,
                &[],
                Some("present"),
                None,
            )
        })
        .unwrap();
    let value = project(&env, &dispatch);
    assert_eq!(value.presence.as_deref(), Some("present"));
    assert_eq!(value.liveness, "live");
    assert_eq!(value.activity, "unknown");
}

#[test]
fn recorded_ending_beats_exit_observation() {
    let env = TestEnv::new();
    let dispatch = seed(&env);
    env.engine
        .store
        .tx(|tx| {
            pod::store::rows::workers::update_state(tx, &dispatch, "succeeded")?;
            pod::store::rows::dispatches::update_status(tx, &dispatch, "completed")?;
            observe::record_herdr_state(
                tx,
                Clock::now(env.clock.as_ref()),
                &dispatch,
                AgentStatus::Done,
                &[],
                Some("exited"),
                None,
            )
        })
        .unwrap();
    assert_eq!(project(&env, &dispatch).outcome, "succeeded");
}

#[test]
fn outcome_unknown_when_exited_without_ending() {
    let env = TestEnv::new();
    let dispatch = seed(&env);
    env.engine
        .store
        .tx(|tx| {
            observe::record_herdr_state(
                tx,
                Clock::now(env.clock.as_ref()),
                &dispatch,
                AgentStatus::Done,
                &[],
                Some("exited"),
                None,
            )
        })
        .unwrap();
    assert_eq!(project(&env, &dispatch).outcome, "outcome_unknown");
}

#[test]
fn unverifiable_never_authorizes_stop_abandon_retry_release() {
    let env = TestEnv::new();
    let dispatch = seed(&env);
    env.engine
        .store
        .tx(|tx| {
            observe::record_self_report(tx, Clock::now(env.clock.as_ref()), &dispatch, "unknown")
        })
        .unwrap();
    let action = project(&env, &dispatch).next_action.argv.join(" ");
    for forbidden in ["worker-stop", "worker-abandon", "retry", "worker-release"] {
        assert!(
            !action.contains(forbidden),
            "unexpected destructive action: {action}"
        );
    }
}

#[test]
fn stale_after_10_minutes_is_warning_only() {
    let env = TestEnv::new();
    let dispatch = seed(&env);
    env.engine
        .store
        .tx(|tx| {
            observe::record_herdr_state(
                tx,
                Clock::now(env.clock.as_ref()),
                &dispatch,
                AgentStatus::Idle,
                &[],
                Some("present"),
                None,
            )
        })
        .unwrap();
    env.clock.advance(Duration::from_secs(601));
    let value = project(&env, &dispatch);
    assert_eq!(value.liveness, "live");
    assert!(value.attention.categories.contains(&"stale".into()));
    assert!(!value.attention.requires_action);
}

const GUIDANCE_AFTER: Duration = Duration::from_secs(5 * 60 + 1);

fn seed_dispatched_worker(env: &mut TestEnv) -> DispatchId {
    let dispatch = seed(env);
    env.engine
        .store
        .tx(|tx| {
            tx.0.execute(
                "UPDATE dispatches SET assignee_terminal_id = 'term-w', assignee_pid = ?1 WHERE id = ?2",
                (std::process::id(), DISPATCH),
            )?;
            Ok(())
        })
        .unwrap();
    observe_present(env, &dispatch);
    dispatch
}

fn observe_present(env: &TestEnv, dispatch: &DispatchId) {
    env.engine
        .store
        .tx(|tx| {
            observe::record_herdr_state(
                tx,
                Clock::now(env.clock.as_ref()),
                dispatch,
                AgentStatus::Idle,
                &[],
                Some("present"),
                None,
            )
        })
        .unwrap();
}

fn mail(kind: &str, to: Option<&str>, dispatch: Option<&DispatchId>) -> SendInput {
    SendInput {
        to: to.map(str::to_owned),
        subject: "subject".into(),
        body: Some("body".into()),
        r#type: kind.into(),
        priority: "normal".into(),
        thread_id: None,
        payload: None,
        task_id: None,
        dispatch_id: dispatch.cloned(),
        outcome: None,
        reason: None,
        reason_target: None,
        files_modified: Vec::new(),
        report_path: None,
        phase: None,
        evidence: (kind == "escalation").then(|| "the logs".into()),
        recommendation: (kind == "escalation").then(|| "retry".into()),
        from: None,
    }
}

#[test]
fn an_unanswered_worker_question_becomes_guidance_after_five_minutes() {
    let mut env = TestEnv::new();
    let dispatch = seed_dispatched_worker(&mut env);
    env.as_caller("term-w", std::process::id());
    let (question, _) =
        questions::ask_store(&env.engine, "Rate limit by key or by IP?", &[], None).unwrap();
    let fresh = project(&env, &dispatch);
    assert!(!fresh.attention.categories.contains(&"guidance".to_string()));
    env.clock.advance(GUIDANCE_AFTER);
    observe_present(&env, &dispatch);
    let late = project(&env, &dispatch);
    assert!(late.attention.categories.contains(&"guidance".to_string()));
    assert!(late.attention.requires_action);
    assert_eq!(late.next_action.kind, "reply");
    assert_eq!(
        late.next_action.argv,
        vec!["pod", "reply", "--id", question.as_str()]
    );

    env.as_caller("term-c", std::process::id());
    questions::reply(&env.engine, &question, "by key").unwrap();
    assert!(
        !project(&env, &dispatch)
            .attention
            .categories
            .contains(&"guidance".to_string())
    );
}

#[test]
fn an_unanswered_escalation_becomes_guidance_until_the_coordinator_sends_to_the_dispatch() {
    let mut env = TestEnv::new();
    let dispatch = seed_dispatched_worker(&mut env);
    env.as_caller("term-w", std::process::id());
    messages::send(&env.engine, mail("escalation", None, Some(&dispatch))).unwrap();
    env.clock.advance(Duration::from_secs(4 * 60));
    observe_present(&env, &dispatch);
    assert!(
        !project(&env, &dispatch)
            .attention
            .categories
            .contains(&"guidance".to_string())
    );
    env.clock.advance(Duration::from_secs(61));
    observe_present(&env, &dispatch);
    let late = project(&env, &dispatch);
    assert!(late.attention.categories.contains(&"guidance".to_string()));
    assert!(late.attention.requires_action);
    assert_eq!(late.next_action.kind, "worker-read");

    env.as_caller("term-c", std::process::id());
    let to = format!("dispatch:{dispatch}");
    messages::send(&env.engine, mail("status", Some(&to), None)).unwrap();
    assert!(
        !project(&env, &dispatch)
            .attention
            .categories
            .contains(&"guidance".to_string())
    );
}

#[test]
fn a_settled_worker_with_an_old_question_has_no_guidance() {
    let mut env = TestEnv::new();
    let dispatch = seed_dispatched_worker(&mut env);
    env.as_caller("term-w", std::process::id());
    questions::ask_store(&env.engine, "Which branch?", &[], None).unwrap();
    env.clock.advance(GUIDANCE_AFTER);
    env.engine
        .store
        .tx(|tx| {
            tx.0.execute(
                "UPDATE workers SET state = 'succeeded' WHERE dispatch_id = ?1",
                [DISPATCH],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(
        !project(&env, &dispatch)
            .attention
            .categories
            .contains(&"guidance".to_string())
    );
}

/// Spec A4.2: a `send` to the Dispatch after the question answers it as a
/// `reply` would, even while the question row stays pending.
#[test]
fn a_send_to_the_dispatch_clears_guidance_for_a_pending_question() {
    let mut env = TestEnv::new();
    let dispatch = seed_dispatched_worker(&mut env);
    env.as_caller("term-w", std::process::id());
    questions::ask_store(&env.engine, "Which branch?", &[], None).unwrap();
    env.clock.advance(GUIDANCE_AFTER + Duration::from_secs(1));
    observe_present(&env, &dispatch);
    assert!(
        project(&env, &dispatch)
            .attention
            .categories
            .contains(&"guidance".to_string())
    );
    env.as_caller("term-c", std::process::id());
    let to = format!("dispatch:{dispatch}");
    messages::send(&env.engine, mail("status", Some(&to), None)).unwrap();
    assert!(
        !project(&env, &dispatch)
            .attention
            .categories
            .contains(&"guidance".to_string())
    );
}

const PROMPT_SCREEN: &str = "Allow edits?\nDo you want to proceed?\n 1. Yes\n 2. No\n\n\n";

fn record_state(
    env: &TestEnv,
    dispatch: &DispatchId,
    status: AgentStatus,
    needs_input: &[&str],
    presence: Option<&str>,
    screen: Option<&str>,
) -> Option<observe::PromptEscalation> {
    let needs_input: Vec<String> = needs_input.iter().map(|reason| (*reason).into()).collect();
    env.engine
        .store
        .tx(|tx| {
            observe::record_herdr_state(
                tx,
                Clock::now(env.clock.as_ref()),
                dispatch,
                status,
                &needs_input,
                presence,
                screen,
            )
        })
        .unwrap()
}

fn run_mail(env: &TestEnv) -> Vec<pod::store::rows::messages::MessageRow> {
    env.engine
        .store
        .read(|tx| {
            pod::store::rows::messages::select(tx, &format!("run:{RUN}"), &[], false, 100, false)
        })
        .unwrap()
}

fn acknowledge_run_mail(env: &TestEnv) {
    env.engine
        .store
        .tx(|tx| {
            for message in pod::store::rows::messages::select(
                tx,
                &format!("run:{RUN}"),
                &[],
                true,
                100,
                false,
            )? {
                pod::store::rows::messages::mark_read(
                    tx,
                    &message.id,
                    Clock::now(env.clock.as_ref()),
                )?;
            }
            Ok(())
        })
        .unwrap();
}

fn give_pane(env: &TestEnv, pane: &str) {
    env.engine
        .store
        .tx(|tx| {
            tx.0.execute(
                "UPDATE workers SET pane_id = ?1 WHERE dispatch_id = ?2",
                (pane, DISPATCH),
            )?;
            Ok(())
        })
        .unwrap();
}

/// Seen live 2026-10-16: a worker sat on a model-switch selector for ten
/// minutes while its coordinator's wait loop discarded the empty return. A
/// blocked prompt is mail from pod, once per blocked episode (spec 7.6).
#[test]
fn a_worker_that_blocks_on_a_prompt_is_escalated_once_per_episode() {
    let mut env = TestEnv::new();
    let dispatch = seed_dispatched_worker(&mut env);
    give_pane(&env, "pane-7");

    let working = record_state(
        &env,
        &dispatch,
        AgentStatus::Working,
        &[],
        Some("present"),
        None,
    );
    assert!(working.is_none());
    assert!(run_mail(&env).is_empty());

    // herdr's bare `blocked` status counts as input; the screen rides along.
    let report = record_state(
        &env,
        &dispatch,
        AgentStatus::Blocked,
        &[],
        Some("present"),
        Some(PROMPT_SCREEN),
    )
    .expect("the first blocked observation stores an escalation");
    let mail = run_mail(&env);
    assert_eq!(mail.len(), 1);
    assert_eq!(report.message_id, mail[0].id);
    assert_eq!(report.run_id.as_str(), RUN);
    let message = &mail[0];
    assert_eq!(message.r#type, "escalation");
    assert_eq!(message.from_handle, "pod");
    assert_eq!(message.priority, "high");
    assert_eq!(message.sender_terminal_id, None);
    assert!(!message.read);
    assert_eq!(
        message.subject,
        "Blocked on a prompt: worker (input)".to_owned()
    );
    let payload = message.payload.as_ref().unwrap();
    assert_eq!(payload["dispatch_id"], DISPATCH);
    assert_eq!(payload["task_id"], TASK);
    assert_eq!(payload["reason"], "input");
    assert_eq!(payload["source"], "pod");
    assert_eq!(payload["needs_input"], serde_json::json!(["blocked"]));
    assert_eq!(message.evidence.as_deref(), Some("pane:pane-7"));
    let recommendation = message.recommendation.as_deref().unwrap();
    assert!(recommendation.contains("worker-read"), "{recommendation}");
    assert!(recommendation.contains("worker-stop"), "{recommendation}");
    let body = message.body.as_deref().unwrap();
    assert!(
        body.starts_with("Worker worker is waiting on a prompt"),
        "{body}"
    );
    assert!(body.contains("Do you want to proceed?"), "{body}");
    assert!(
        body.ends_with(" 2. No"),
        "trailing blank lines are trimmed: {body:?}"
    );

    // The same blocked episode, seen again, stores nothing more.
    for _ in 0..2 {
        let again = record_state(
            &env,
            &dispatch,
            AgentStatus::Blocked,
            &["blocked"],
            Some("present"),
            Some(PROMPT_SCREEN),
        );
        assert!(again.is_none());
    }
    assert_eq!(run_mail(&env).len(), 1);

    // A later block while the first escalation is still unread adds nothing.
    record_state(
        &env,
        &dispatch,
        AgentStatus::Working,
        &[],
        Some("present"),
        None,
    );
    let unread = record_state(
        &env,
        &dispatch,
        AgentStatus::Blocked,
        &[],
        Some("present"),
        None,
    );
    assert!(unread.is_none());
    assert_eq!(run_mail(&env).len(), 1);

    // Once the coordinator has read it, the next episode is a new escalation.
    acknowledge_run_mail(&env);
    record_state(
        &env,
        &dispatch,
        AgentStatus::Working,
        &[],
        Some("present"),
        None,
    );
    let second = record_state(
        &env,
        &dispatch,
        AgentStatus::Blocked,
        &[],
        Some("present"),
        None,
    );
    assert!(second.is_some());
    let mail = run_mail(&env);
    assert_eq!(mail.len(), 2);
    // Without a screen read the body is the first line alone.
    assert_eq!(mail[1].body.as_deref().unwrap().lines().count(), 1);

    // An approval prompt names its reason.
    acknowledge_run_mail(&env);
    record_state(
        &env,
        &dispatch,
        AgentStatus::Working,
        &[],
        Some("present"),
        None,
    );
    record_state(
        &env,
        &dispatch,
        AgentStatus::Blocked,
        &["approval"],
        Some("present"),
        None,
    )
    .expect("an approval prompt is escalated");
    let mail = run_mail(&env);
    assert_eq!(mail.len(), 3);
    assert_eq!(mail[2].subject, "Blocked on a prompt: worker (approval)");
    assert_eq!(mail[2].payload.as_ref().unwrap()["reason"], "approval");
    assert_eq!(
        mail[2].payload.as_ref().unwrap()["needs_input"],
        serde_json::json!(["approval"])
    );

    // A settled Dispatch has nothing left to escalate.
    let mut env = TestEnv::new();
    let dispatch = seed_dispatched_worker(&mut env);
    env.engine
        .store
        .tx(|tx| {
            tx.0.execute(
                "UPDATE dispatches SET status = 'completed' WHERE id = ?1",
                [DISPATCH],
            )?;
            Ok(())
        })
        .unwrap();
    let settled = record_state(
        &env,
        &dispatch,
        AgentStatus::Blocked,
        &["approval"],
        Some("present"),
        None,
    );
    assert!(settled.is_none());
    assert!(run_mail(&env).is_empty());

    // Unproven presence is no evidence that anyone is being asked.
    let mut env = TestEnv::new();
    let dispatch = seed_dispatched_worker(&mut env);
    let unproven = record_state(&env, &dispatch, AgentStatus::Blocked, &[], None, None);
    assert!(unproven.is_none());
    assert!(run_mail(&env).is_empty());
}

#[test]
fn a_long_screen_is_cut_to_forty_lines_of_two_hundred_characters() {
    let mut env = TestEnv::new();
    let dispatch = seed_dispatched_worker(&mut env);
    let screen: String = (1..=60)
        .map(|n| format!("line {n} {}\n", "x".repeat(300)))
        .collect();
    record_state(
        &env,
        &dispatch,
        AgentStatus::Blocked,
        &[],
        Some("present"),
        Some(&screen),
    )
    .unwrap();
    let body = run_mail(&env)[0].body.clone().unwrap();
    let lines: Vec<&str> = body.lines().skip(1).collect();
    assert_eq!(lines.len(), 40);
    assert!(lines[0].starts_with("line 21 "), "{}", lines[0]);
    assert!(lines[39].starts_with("line 60 "), "{}", lines[39]);
    assert!(lines.iter().all(|line| line.chars().count() == 200));
}

/// The live `input` category already covers the stuck state and clears when
/// the worker moves on, so pod's own escalation must not also go stale into
/// `guidance` after five minutes.
#[test]
fn pod_escalations_never_become_guidance() {
    let mut env = TestEnv::new();
    let dispatch = seed_dispatched_worker(&mut env);
    record_state(
        &env,
        &dispatch,
        AgentStatus::Blocked,
        &[],
        Some("present"),
        Some(PROMPT_SCREEN),
    )
    .unwrap();
    env.clock.advance(Duration::from_secs(6 * 60));
    let value = project(&env, &dispatch);
    assert!(
        !value.attention.categories.contains(&"guidance".to_string()),
        "{:?}",
        value.attention.categories
    );
    assert!(value.attention.categories.contains(&"input".to_string()));
    assert!(value.attention.requires_action);
    assert_eq!(value.next_action.kind, "worker-read");
}

#[test]
fn a_stalled_escalation_adds_the_stalled_category_until_the_worker_makes_progress() {
    let env = TestEnv::new();
    let dispatch = seed(&env);
    record(&env, &dispatch, Some("present"));
    env.clock.advance(Duration::from_secs(9 * 60));
    let at = Clock::now(env.clock.as_ref()).to_rfc3339();
    env.engine.store.tx(|tx| {
        tx.0.execute("INSERT INTO messages (id, run_id, mailbox, from_handle, subject, type, priority, payload, read, created_at) VALUES ('msg_000000000812', ?1, ?2, 'pod', 'Stalled: worker (9 min without progress)', 'escalation', 'high', ?3, 0, ?4)", (RUN, format!("run:{RUN}"), serde_json::json!({"dispatch_id": DISPATCH, "task_id": TASK, "source": "pod", "reason": "stalled"}).to_string(), &at))?;
        Ok(())
    }).unwrap();
    let stalled = project(&env, &dispatch);
    assert_eq!(stalled.attention.categories, vec!["stalled"]);
    assert!(stalled.attention.requires_action);
    assert_eq!(stalled.next_action.kind, "worker-read");

    // A working observation after the escalation is progress.
    env.clock.advance(Duration::from_secs(60));
    env.engine
        .store
        .tx(|tx| {
            observe::record_herdr_state(
                tx,
                Clock::now(env.clock.as_ref()),
                &dispatch,
                AgentStatus::Working,
                &[],
                Some("present"),
                None,
            )
        })
        .unwrap();
    let after = project(&env, &dispatch);
    assert!(after.attention.categories.is_empty(), "{after:?}");
    assert!(!after.attention.requires_action);
    assert_eq!(after.next_action.kind, "none");
}

/// A brief herdr accepted but the transcript never showed (seen live
/// 2026-10-21) needs the coordinator to read the worker.
#[test]
fn an_unconfirmed_brief_requires_a_read_while_the_dispatch_is_active() {
    let mut env = TestEnv::new();
    let dispatch = seed_dispatched_worker(&mut env);
    assert!(
        !project(&env, &dispatch)
            .attention
            .categories
            .contains(&"brief_unconfirmed".into())
    );
    env.engine
        .store
        .tx(|tx| {
            tx.0.execute(
                "UPDATE workers SET preamble_input = 'rendered_unconfirmed' WHERE dispatch_id = ?1",
                [DISPATCH],
            )?;
            Ok(())
        })
        .unwrap();
    let value = project(&env, &dispatch);
    assert!(
        value
            .attention
            .categories
            .contains(&"brief_unconfirmed".into()),
        "{:?}",
        value.attention
    );
    assert!(value.attention.requires_action);
    let next = serde_json::to_value(&value.next_action).unwrap();
    assert_eq!(next["kind"], "worker-read", "{next}");

    env.engine
        .store
        .tx(|tx| {
            tx.0.execute(
                "UPDATE dispatches SET status = 'completed' WHERE id = ?1",
                [DISPATCH],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(
        !project(&env, &dispatch)
            .attention
            .categories
            .contains(&"brief_unconfirmed".into())
    );
}
