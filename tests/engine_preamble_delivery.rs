//! The brief at start: herdr gets only the agent's arguments, the brief waits
//! for an idle agent, and "delivered" means the agent's transcript shows it.
mod support;

use std::time::Duration;

use pod::engine::lifecycle::{self, Setup, StartInput, StartReceipt, Worktree};
use pod::engine::{runs, tasks};
use pod::herdr::AgentStatus;
use pod::ids::{DispatchId, RunId, TaskId};
use pod::store::rows::workers;

use support::TestEnv;

fn seeded() -> (TestEnv, RunId, TaskId) {
    let mut env = TestEnv::new();
    env.as_caller("term-coord", 42);
    let caller = env.engine.caller.clone().unwrap();
    let run = env
        .engine
        .store
        .tx(|tx| runs::create(tx, env.engine.clock.now(), "run", &caller, env.tmp.path()))
        .unwrap();
    let task = env
        .engine
        .store
        .tx(|tx| {
            tasks::create(
                tx,
                env.engine.clock.now(),
                &run.id,
                "do work",
                None,
                None,
                &[],
                None,
                "term-coord",
            )
        })
        .unwrap();
    (env, run.id, task.id)
}

fn start(env: &TestEnv, run: RunId, task: TaskId, agent: &str) -> StartReceipt {
    lifecycle::worker_start(
        &env.engine,
        StartInput {
            task: Some(task),
            spec: None,
            agent: Some(agent.into()),
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
            timeout_ms: Some(100),
            run: Some(run),
            handle: None,
        },
    )
    .unwrap()
}

fn worker_row(env: &TestEnv, dispatch: &DispatchId) -> workers::WorkerRow {
    env.engine
        .store
        .read(|tx| workers::get(tx, dispatch))
        .unwrap()
        .unwrap()
}

fn effects(row: &workers::WorkerRow) -> Vec<String> {
    row.effects
        .clone()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

/// herdr runs the kind's own executable; the command name pod resolved must
/// not follow it as a stray first prompt (seen live 2026-10-21).
#[test]
fn herdr_gets_the_arguments_without_the_command_name() {
    let (env, run, task) = seeded();
    let receipt = start(&env, run.clone(), task, "claude");
    let argv = env.herdr.agent_start_argv().pop().unwrap();
    assert_ne!(argv.first().map(String::as_str), Some("claude"), "{argv:?}");
    assert_eq!(argv.first().map(String::as_str), Some("--session-id"));
    assert_eq!(
        receipt.launch.effective.as_ref().unwrap()["command"],
        "claude"
    );

    let second = {
        let t = env
            .engine
            .store
            .tx(|tx| {
                tasks::create(
                    tx,
                    env.engine.clock.now(),
                    &run,
                    "second",
                    None,
                    None,
                    &[],
                    None,
                    "term-coord",
                )
            })
            .unwrap();
        t.id
    };
    start(&env, run, second, "codex");
    let argv = env.herdr.agent_start_argv().pop().unwrap();
    assert_eq!(argv, ["--no-daemon"]);
}

/// The brief waits for the agent to leave `working`.
#[test]
fn the_brief_waits_for_an_idle_agent() {
    let (mut env, run, task) = seeded();
    env.engine.ready_wait = Duration::from_secs(5);
    for status in [
        AgentStatus::Working,
        AgentStatus::Working,
        AgentStatus::Idle,
    ] {
        env.herdr.script_agent_status("claude", status);
    }
    let receipt = start(&env, run, task, "claude");
    let calls = env.herdr.calls();
    let prompt = calls
        .iter()
        .position(|c| c.starts_with("agent_prompt:"))
        .expect("the brief was prompted");
    let gets = calls[..prompt]
        .iter()
        .filter(|c| *c == "agent_get:claude")
        .count();
    assert!(
        gets >= 3,
        "three statuses were read before the prompt: {calls:?}"
    );
    let waited = effects(&worker_row(&env, &receipt.dispatch_id))
        .into_iter()
        .find_map(|e| e.strip_prefix("preamble_waited_ms:").map(str::to_owned))
        .expect("the wait is recorded");
    assert!(waited.parse::<u64>().unwrap() >= 400, "{waited}");
}

/// An agent that never goes idle does not hold the brief past `ready_wait`.
#[test]
fn an_agent_that_never_idles_gets_the_brief_after_the_deadline() {
    let (mut env, run, task) = seeded();
    env.engine.ready_wait = Duration::from_millis(300);
    for _ in 0..50 {
        env.herdr
            .script_agent_status("claude", AgentStatus::Working);
    }
    let receipt = start(&env, run, task, "claude");
    assert_eq!(receipt.state, "ready");
    assert!(
        env.herdr
            .calls()
            .iter()
            .any(|c| c.starts_with("agent_prompt:claude:")),
    );
    let waited = effects(&worker_row(&env, &receipt.dispatch_id))
        .into_iter()
        .find_map(|e| e.strip_prefix("preamble_waited_ms:").map(str::to_owned))
        .unwrap();
    assert!(waited.parse::<u64>().unwrap() >= 300, "{waited}");
}

fn with_transcript(env: &TestEnv) -> std::path::PathBuf {
    let path = env.tmp.path().join("session.jsonl");
    std::fs::write(&path, "").unwrap();
    env.herdr
        .report_agent_session(Some("session-1"), Some(&path));
    path
}

#[test]
fn a_brief_seen_in_the_transcript_is_delivered() {
    let (env, run, task) = seeded();
    with_transcript(&env);
    env.herdr.set_transcript_echo(true);
    let receipt = start(&env, run, task, "claude");
    assert_eq!(receipt.outcome, None, "{receipt:?}");
    let row = worker_row(&env, &receipt.dispatch_id);
    assert_eq!(row.preamble_input.as_deref(), Some("delivered"));
    assert_eq!(row.last_error, None);
}

#[test]
fn a_brief_missing_from_the_transcript_is_unconfirmed_and_never_retyped() {
    let (env, run, task) = seeded();
    with_transcript(&env);
    env.herdr.set_transcript_echo(false);
    let receipt = start(&env, run, task, "claude");
    assert_eq!(receipt.outcome.as_deref(), Some("dispatched_unobserved"));
    let row = worker_row(&env, &receipt.dispatch_id);
    assert_eq!(row.preamble_input.as_deref(), Some("rendered_unconfirmed"));
    let error = receipt.last_error.as_deref().unwrap();
    assert!(
        error.contains("transcript shows no user turn containing it"),
        "{error}"
    );
    assert!(error.contains("never typed again"), "{error}");
    assert!(
        receipt
            .next_commands
            .iter()
            .any(|c| c.contains("worker-read")),
        "{:?}",
        receipt.next_commands
    );
    let prompts = env
        .herdr
        .calls()
        .iter()
        .filter(|c| c.starts_with("agent_prompt:"))
        .count();
    assert_eq!(prompts, 1);
    assert!(
        !env.herdr
            .calls()
            .iter()
            .any(|c| c.starts_with("pane_send_text"))
    );
}

#[test]
fn a_kind_without_a_transcript_reader_keeps_herdrs_word_with_a_warning() {
    let (env, run, task) = seeded();
    let receipt = start(&env, run, task, "pi");
    assert_eq!(receipt.outcome, None);
    assert_eq!(
        worker_row(&env, &receipt.dispatch_id)
            .preamble_input
            .as_deref(),
        Some("delivered")
    );
    assert!(
        receipt
            .warnings
            .iter()
            .any(|w| w == "brief delivery unproven: no transcript for pi"),
        "{:?}",
        receipt.warnings
    );
}
