mod support;

use std::fs;
use std::path::PathBuf;

use pod::engine::lifecycle::{Setup, StartInput, Worktree, handle_for, worker_start};
use pod::engine::preamble::{Decision, PreambleContext, Specialist, render_preamble};
use pod::engine::{runs, tasks};
use pod::ids::{DispatchId, RunId, TaskId};

use support::TestEnv;

#[test]
fn preamble_matches_section_10_golden() {
    let task = TaskId::new();
    let dispatch = DispatchId::new();
    let specialist = DispatchId::new();
    let rendered = render_preamble(&PreambleContext {
        pod: "/opt/pod".into(),
        coordinator_handle: "coord-1".into(),
        task_id: task.clone(),
        dispatch_id: dispatch.clone(),
        worker_handle: "codex-ab12".into(),
        heartbeat_minutes: 5,
        specialists: vec![Specialist {
            dispatch_id: specialist.clone(),
            handle: "claude-cd34".into(),
            task_title: "Review change".into(),
        }],
        decision: Some(Decision {
            question: "Which schema?".into(),
            resolution: "v2".into(),
        }),
        sub_dispatch: Some("You may dispatch one bounded subtask.".into()),
        spec: "Implement the lifecycle.".into(),
        brief_path: format!("/s/runs/r/preambles/{}.txt", dispatch.as_str()).into(),
    });
    let expected = include_str!("engine_preamble_golden.txt")
        .replace("<TASK>", task.as_str())
        .replace(
            "<BRIEF>",
            &format!("/s/runs/r/preambles/{}.txt", dispatch.as_str()),
        )
        .replace("<DISPATCH>", dispatch.as_str())
        .replace("<SPECIALIST>", specialist.as_str());
    assert_eq!(rendered, expected);
}

/// Claude Code splits a long paste into several `[Pasted text #N]` chunks
/// and loses the early ones when it is busy (seen live 2026-10-14), so a
/// Claude worker always gets a one-line pointer to the brief file, whatever
/// the brief's size.
#[test]
fn claude_workers_always_get_the_brief_as_a_file() {
    let mut env = TestEnv::new();
    env.as_caller("term-coord", 42);
    let caller = env.engine.caller.clone().unwrap();
    let run = env
        .engine
        .store
        .tx(|tx| runs::create(tx, env.engine.clock.now(), "test", &caller, env.tmp.path()))
        .unwrap();
    let task = env
        .engine
        .store
        .tx(|tx| {
            tasks::create(
                tx,
                env.engine.clock.now(),
                &run.id,
                "Target: do work",
                None,
                None,
                &[],
                None,
                "term-coord",
            )
        })
        .unwrap();
    let mut input = start(task.id, run.id);
    input.agent = Some("claude".into());
    let receipt = worker_start(&env.engine, input).unwrap();
    assert_eq!(receipt.state, "ready");
    let submissions = env.herdr.submissions();
    assert_eq!(submissions.len(), 1);
    let typed = &submissions[0].1;
    assert!(typed.contains("full worker brief is stored at"), "{typed}");
    assert!(!typed.contains('\n'), "a single line: {typed}");
    let effect = receipt
        .effects
        .iter()
        .find(|e| e.kind == "preamble_file")
        .expect("preamble_file effect");
    let stored = fs::read_to_string(effect.id.as_deref().unwrap()).unwrap();
    assert!(
        stored.contains("Your worker handle is:"),
        "the file holds the whole preamble"
    );
    assert!(
        stored.contains("do work") || stored.contains("Target"),
        "{stored}"
    );
}

#[test]
fn handles_are_mnemonic_and_number_only_on_collision() {
    assert_eq!(handle_for("codex", None, &[]), "codex");
    assert_eq!(handle_for("codex", None, &["codex".into()]), "codex-2");
    assert_eq!(
        handle_for("codex", None, &["codex".into(), "codex-2".into()]),
        "codex-3"
    );
    assert_eq!(handle_for("sol", Some("limiter"), &[]), "sol-limiter");
    assert_eq!(
        handle_for("sol", Some("limiter"), &["sol-limiter".into()]),
        "sol-limiter-2"
    );
}

#[test]
fn handle_is_sanitized_to_the_agent_name_grammar() {
    let handle = handle_for(
        "9 Bad/Agent.Name That Is Much Too Long",
        Some("Rate Limiter! Lane Of Great Length"),
        &[],
    );
    assert!(handle.len() <= 32 && handle.as_bytes()[0].is_ascii_lowercase());
    assert!(
        handle
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
    );
    assert!(handle.starts_with("bad-agent-name"));
    assert!(!handle.ends_with('-'));
    assert_eq!(handle_for("", None, &[]), "worker");
}

#[test]
fn preamble_oversize_uses_pointer_and_records_once() {
    let mut env = TestEnv::new();
    env.as_caller("term-coord", 42);
    let caller = env.engine.caller.clone().unwrap();
    let run = env
        .engine
        .store
        .tx(|tx| runs::create(tx, env.engine.clock.now(), "test", &caller, env.tmp.path()))
        .unwrap();
    let task = env
        .engine
        .store
        .tx(|tx| {
            tasks::create(
                tx,
                env.engine.clock.now(),
                &run.id,
                &"x".repeat(9_000),
                None,
                None,
                &[],
                None,
                "term-coord",
            )
        })
        .unwrap();
    let receipt = worker_start(&env.engine, start(task.id, run.id)).unwrap();
    assert_eq!(receipt.state, "ready");
    let submissions = env.herdr.submissions();
    assert_eq!(submissions.len(), 1);
    assert!(submissions[0].1.contains("full worker brief is stored at"));
    assert!(submissions[0].1.len() < 8 * 1024);
    let worker = env
        .engine
        .store
        .read(|tx| pod::store::rows::workers::get(tx, &receipt.dispatch_id))
        .unwrap()
        .unwrap();
    assert_eq!(worker.preamble_input.as_deref(), Some("delivered"));
    let effects = worker.effects.unwrap();
    let effect = effects
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .find(|v| v.starts_with("preamble_file:"))
        .unwrap();
    let path = effect.trim_start_matches("preamble_file:");
    assert!(
        receipt
            .effects
            .iter()
            .any(|e| e.kind == "preamble_file" && e.id.as_deref() == Some(path))
    );
    let metadata = fs::metadata(path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    }
    assert!(metadata.len() > 9_000);
}

/// A caller, a Run, and one Task whose spec is small.
fn seeded() -> (TestEnv, RunId, TaskId) {
    let mut env = TestEnv::new();
    env.as_caller("term-coord", 42);
    let caller = env.engine.caller.clone().unwrap();
    let run = env
        .engine
        .store
        .tx(|tx| runs::create(tx, env.engine.clock.now(), "test", &caller, env.tmp.path()))
        .unwrap();
    let task = env
        .engine
        .store
        .tx(|tx| {
            tasks::create(
                tx,
                env.engine.clock.now(),
                &run.id,
                "Target: do work",
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

#[test]
fn every_kind_gets_a_brief_file_and_the_pointer_goes_through_agent_prompt() {
    let (env, run, task) = seeded();
    let receipt = worker_start(&env.engine, start(task, run)).unwrap();
    assert_eq!(receipt.state, "ready");
    assert_eq!(receipt.launch.inject.as_deref(), Some("prompt"));
    let effect = receipt
        .effects
        .iter()
        .find(|e| e.kind == "preamble_file")
        .unwrap();
    assert_eq!(
        effect.id.as_deref().map(PathBuf::from),
        Some(pod::engine::lifecycle::brief_path(
            &env.engine.paths,
            &receipt.run_id,
            &receipt.dispatch_id
        ))
    );
    let calls = env.herdr.calls();
    assert!(
        calls.iter().any(|c| c.starts_with(&format!(
            "agent_prompt:{}:The full worker brief is stored at",
            receipt.handle
        ))),
        "{calls:?}"
    );
    assert!(
        !calls.iter().any(|c| c.starts_with("pane_send_text")),
        "no hand paste on the prompt path"
    );
}

#[test]
fn a_stalled_prompt_is_dispatched_unobserved_and_never_pasted_twice() {
    let (env, run, task) = seeded();
    env.herdr
        .script_prompt_outcome(pod::herdr::PromptOutcome::Stalled);
    let receipt = worker_start(&env.engine, start(task, run)).unwrap();
    assert_eq!(receipt.outcome.as_deref(), Some("dispatched_unobserved"));
    assert_eq!(env.herdr.submissions().len(), 1);
    assert!(
        !env.herdr
            .calls()
            .iter()
            .any(|c| c.starts_with("pane_send_text"))
    );
}

/// Right after `agent.start`, herdr can answer `agent.prompt` with "agent
/// <name> is not an active named agent" for a moment (seen live
/// 2026-10-14 on every fresh Claude worker). That is a race, not a refusal:
/// pod retries the prompt until the name resolves, bounded by `ready_wait`,
/// and only then pastes.
#[test]
fn a_prompt_whose_agent_is_not_yet_active_is_retried_not_pasted() {
    let (env, run, task) = seeded();
    env.herdr
        .fail_agent_prompt("agent codex is not an active named agent");
    let receipt = worker_start(&env.engine, start(task, run)).unwrap();
    assert_eq!(receipt.state, "ready");
    assert_eq!(receipt.launch.inject.as_deref(), Some("prompt"));
    assert!(
        receipt.effects.iter().all(|e| e.kind != "prompt_error"),
        "{:?}",
        receipt.effects
    );
    let prompts = env
        .herdr
        .calls()
        .iter()
        .filter(|c| c.starts_with("agent_prompt:"))
        .count();
    assert_eq!(prompts, 2, "one failed attempt, then the retry");
    assert!(
        !env.herdr
            .calls()
            .iter()
            .any(|c| c.starts_with("pane_send_text"))
    );
}

#[test]
fn a_prompt_transport_error_falls_back_to_safe_paste() {
    let (env, run, task) = seeded();
    env.herdr.fail_agent_prompt("agent.prompt unsupported");
    let receipt = worker_start(&env.engine, start(task, run)).unwrap();
    assert_eq!(receipt.state, "ready");
    assert_eq!(receipt.launch.inject.as_deref(), Some("paste"));
    let reason = receipt
        .effects
        .iter()
        .find(|e| e.kind == "prompt_error")
        .expect("the prompt error is kept");
    assert_eq!(reason.id.as_deref(), Some("agent.prompt unsupported"));
    assert!(
        env.herdr
            .calls()
            .iter()
            .any(|c| c.starts_with("pane_send_text"))
    );
}

fn start(task: TaskId, run: RunId) -> StartInput {
    StartInput {
        task: Some(task),
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
        timeout_ms: Some(100),
        run: Some(run),
        handle: None,
    }
}
