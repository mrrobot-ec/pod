mod support;

use std::path::PathBuf;

use pod::ErrorCode;
use pod::config::RepoConfig;
use pod::engine::lifecycle::{self, Setup, StartInput, Worktree};
use pod::engine::placement::{self, CreationOptions};
use pod::engine::{runs, tasks};
use pod::herdr::Herdr;
use pod::ids::{DispatchId, RunId, TaskId};
use pod::store::rows::{dispatches, workers};

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

fn input(task: TaskId, run: RunId) -> StartInput {
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

/// A worker's workspace closes with its last tab; clearing its sidebar
/// status afterwards is not an error worth a warning on every command.
#[test]
fn clearing_the_sidebar_of_a_closed_workspace_is_skipped() {
    let (env, run, task) = seeded();
    let receipt = lifecycle::worker_start(&env.engine, input(task, run)).unwrap();
    settle(&env, &receipt.dispatch_id);
    lifecycle::release(&env.engine, &receipt.dispatch_id).unwrap();
    env.herdr.remove_workspace("ws_fake");
    let before = env.herdr.calls().len();
    env.engine.after_commit(&receipt.run_id, &[]).unwrap();
    let calls = env.herdr.calls()[before..].to_vec();
    assert!(
        calls.iter().any(|c| c == "workspace_get:ws_fake"),
        "{calls:?}"
    );
    assert!(
        !calls
            .iter()
            .any(|c| c.starts_with("workspace_report_metadata:ws_fake")),
        "{calls:?}"
    );
}

#[test]
fn tab_label_is_handle_and_task_title_and_handles_count_up() {
    let (env, run, task) = seeded();
    let first = lifecycle::worker_start(&env.engine, input(task, run.clone())).unwrap();
    assert_eq!(first.handle, "codex");
    let second_task = new_task(&env, &run, "second");
    let second = lifecycle::worker_start(&env.engine, input(second_task, run)).unwrap();
    assert_eq!(second.handle, "codex-2");
    let calls = env.herdr.calls();
    assert!(calls.contains(&"tab_create:ws_fake:codex · do work".to_string()));
    assert!(calls.contains(&"tab_create:ws_fake:codex-2 · second".to_string()));
}

#[test]
fn explicit_handle_wins_and_a_live_collision_is_a_usage_error() {
    let (env, run, task) = seeded();
    target_agent(&env);
    let mut taken = input(task.clone(), run.clone());
    taken.handle = Some("target".into());
    let error = lifecycle::worker_start(&env.engine, taken).unwrap_err();
    assert_eq!(error.code, ErrorCode::Usage);
    assert!(error.message.contains("target"));
    let mut named = input(task, run);
    named.handle = Some("Sol Advisor".into());
    let receipt = lifecycle::worker_start(&env.engine, named).unwrap();
    assert_eq!(receipt.handle, "sol-advisor");
}

#[test]
fn folder_placement_joins_the_workspace_whose_panes_sit_in_that_folder() {
    let (env, run, task) = seeded();
    let monaco = env.tmp.path().join("monaco");
    env.herdr.add_folder_workspace(&monaco, "ws_monaco");
    let mut start = input(task, run);
    start.worktree = Worktree::Path(monaco.clone());
    let receipt = lifecycle::worker_start(&env.engine, start).unwrap();
    assert_eq!(receipt.state, "ready");
    assert_eq!(receipt.handle, "codex-monaco");
    let calls = env.herdr.calls();
    assert!(calls.contains(&"tab_create:ws_monaco:codex-monaco · do work".to_string()));
    assert!(!calls.iter().any(|call| call.starts_with("worktree_open:")));
    assert!(
        !calls
            .iter()
            .any(|call| call.starts_with("workspace_create:"))
    );
}

#[test]
fn folder_placement_creates_a_workspace_named_after_the_folder() {
    let (env, run, task) = seeded();
    let monaco = env.tmp.path().join("monaco");
    let mut start = input(task, run);
    start.worktree = Worktree::Path(monaco.clone());
    let receipt = lifecycle::worker_start(&env.engine, start).unwrap();
    assert_eq!(receipt.state, "ready");
    let worker = worker_row(&env, &receipt.dispatch_id);
    let calls = env.herdr.calls();
    assert!(calls.contains(&format!("workspace_create:{}:monaco", monaco.display())));
    // The new workspace's own tab is the worker's tab, relabelled.
    let tab = worker.tab_id.clone().unwrap();
    assert!(calls.contains(&format!("tab_rename:{tab}:codex-monaco · do work")));
    assert!(!calls.iter().any(|call| call.starts_with("tab_create:")));
    settle(&env, &receipt.dispatch_id);
    let released = lifecycle::release(&env.engine, &receipt.dispatch_id).unwrap();
    assert_eq!(released.closed_tabs, [tab]);
}

/// Seen live 2026-10-15: herdr's `worktree.list` answers for its focused
/// workspace unless given a folder, and four starts failed with git's "not a
/// git repository" for the folder the person happened to be looking at while
/// every target was a valid worktree. Placement asks for the target's own
/// repository; a target that is no repository is a plain folder; any other
/// herdr failure names the step and the folder.
#[test]
fn path_placement_lists_the_targets_repository_not_the_focused_workspace() {
    let (env, run, task) = seeded();
    let monaco = env.tmp.path().join("monaco");
    let mut start = input(task, run.clone());
    start.worktree = Worktree::Path(monaco.clone());
    lifecycle::worker_start(&env.engine, start).unwrap();
    let calls = env.herdr.calls();
    assert!(
        calls.contains(&format!("worktree_list:{}", monaco.display())),
        "{calls:?}"
    );
    assert!(!calls.iter().any(|call| call == "worktree_list"));

    let plain = env.tmp.path().join("plain");
    env.herdr
        .fail_worktree_list("fatal: not a git repository (or any of the parent directories): .git");
    let mut start = input(new_task(&env, &run, "plain"), run.clone());
    start.worktree = Worktree::Path(plain.clone());
    let receipt = lifecycle::worker_start(&env.engine, start).unwrap();
    assert_eq!(
        (receipt.state.as_str(), receipt.failed_stage),
        ("ready", None)
    );
    assert!(
        env.herdr
            .calls()
            .contains(&format!("workspace_create:{}:plain", plain.display()))
    );

    env.herdr.fail_worktree_list("herdr did not answer");
    let mut start = input(new_task(&env, &run, "third"), run);
    start.worktree = Worktree::Path(plain.clone());
    let receipt = lifecycle::worker_start(&env.engine, start).unwrap();
    assert_eq!(receipt.failed_stage.as_deref(), Some("placement"));
    assert_eq!(
        receipt.last_error.as_deref(),
        Some(
            format!(
                "herdr worktree.list in {}: herdr did not answer",
                plain.display()
            )
            .as_str()
        )
    );
}

/// herdr refuses a linked worktree as the start of a worktree action
/// (`linked_worktree_source`, seen live 2026-10-15), so a new worktree is
/// created from, and an existing one opened from, the source checkout its
/// listing names, wherever the coordinator or the target sits.
#[test]
fn worktree_actions_start_from_the_source_checkout_herdr_names() {
    let (env, _run, _task) = seeded();
    let main = env.tmp.path().join("api-service");
    let linked = env.tmp.path().join("api-service-wt");
    std::fs::create_dir_all(&main).unwrap();
    std::fs::create_dir_all(linked.join(".git")).unwrap();
    env.herdr.set_worktree_source(&main);
    env.herdr.add_worktree(&linked, "refs/heads/feature");

    // A new worktree, created by a coordinator sitting in the linked worktree.
    let options = CreationOptions {
        name: Some("child"),
        ..CreationOptions::default()
    };
    let placed =
        placement::place(&env.engine, &Worktree::NewChild, &options, "ws", &linked).unwrap();
    assert_eq!(placed.repo.as_deref(), Some(main.as_path()));
    let calls = env.herdr.calls();
    assert!(
        calls.contains(&format!(
            "worktree_create:child:api-service/child:{}",
            main.display()
        )),
        "{calls:?}"
    );

    // An existing worktree, by path and by branch, opens from the same checkout.
    let none = CreationOptions::default();
    placement::place(
        &env.engine,
        &Worktree::Path(linked.clone()),
        &none,
        "ws",
        &main,
    )
    .unwrap();
    placement::place(
        &env.engine,
        &Worktree::Branch("feature".into()),
        &none,
        "ws",
        &linked,
    )
    .unwrap();
    let open = format!("worktree_open:{}:{}", linked.display(), main.display());
    let calls = env.herdr.calls();
    assert_eq!(
        calls.iter().filter(|call| **call == open).count(),
        2,
        "{calls:?}"
    );
}

#[test]
fn new_worktree_space_is_labelled_repo_slash_name_unless_display_name_is_given() {
    let (env, run, task) = seeded();
    let repo = env.tmp.path().join("api-service");
    std::fs::create_dir_all(&repo).unwrap();
    let mut start = new_child(&env, task.clone(), run.clone());
    start.repo = Some(format!("path:{}", repo.display()));
    start.name = Some("feature-x".into());
    lifecycle::worker_start(&env.engine, start).unwrap();
    let calls = env.herdr.calls();
    assert!(
        calls
            .iter()
            .any(|call| call.starts_with("worktree_create:feature-x:api-service/feature-x:"))
    );

    let other = new_task(&env, &run, "other");
    let mut named = new_child(&env, other, run);
    named.name = Some("feature-y".into());
    named.display_name = Some("Agent svc feature y".into());
    lifecycle::worker_start(&env.engine, named).unwrap();
    let calls = env.herdr.calls();
    assert!(
        calls
            .iter()
            .any(|call| call.starts_with("worktree_create:feature-y:Agent svc feature y:"))
    );
}

#[test]
fn reuse_renames_the_tab_to_the_new_task() {
    let (env, run, task) = seeded();
    let first = lifecycle::worker_start(&env.engine, input(task, run.clone())).unwrap();
    let tab = worker_row(&env, &first.dispatch_id).tab_id.unwrap();
    settle(&env, &first.dispatch_id);
    let next = new_task(&env, &run, "review limiter");
    let mut reuse = input(next, run);
    reuse.agent = None;
    reuse.terminal = Some(first.handle.clone());
    lifecycle::worker_start(&env.engine, reuse).unwrap();
    assert!(
        env.herdr
            .calls()
            .contains(&format!("tab_rename:{tab}:codex · review limiter"))
    );
}

#[test]
fn new_worktree_names_the_handle_after_the_place_and_relabels_its_tab() {
    let (env, run, task) = seeded();
    let mut start = new_child(&env, task, run);
    start.name = Some("limiter".into());
    let receipt = lifecycle::worker_start(&env.engine, start).unwrap();
    assert_eq!(receipt.state, "ready");
    assert_eq!(receipt.handle, "codex-limiter");
    let tab = worker_row(&env, &receipt.dispatch_id).tab_id.unwrap();
    assert!(
        env.herdr
            .calls()
            .contains(&format!("tab_rename:{tab}:codex-limiter · do work"))
    );
}

#[test]
fn preflight_errors_task_not_found_not_startable_with_unmet_deps_depth_exceeded_terminal_busy() {
    let (env, run, task) = seeded();
    let missing =
        lifecycle::worker_start(&env.engine, input(TaskId::new(), run.clone())).unwrap_err();
    assert_eq!(missing.code, ErrorCode::TaskNotFound);

    let pending = env
        .engine
        .store
        .tx(|tx| {
            tasks::create(
                tx,
                env.engine.clock.now(),
                &run,
                "blocked by dependency",
                None,
                None,
                std::slice::from_ref(&task),
                None,
                "term-coord",
            )
        })
        .unwrap();
    let unmet = lifecycle::worker_start(&env.engine, input(pending.id, run.clone())).unwrap_err();
    assert_eq!(unmet.code, ErrorCode::TaskNotStartable);

    let first = lifecycle::worker_start(&env.engine, input(task, run.clone())).unwrap();
    let next = env
        .engine
        .store
        .tx(|tx| {
            tasks::create(
                tx,
                env.engine.clock.now(),
                &run,
                "reuse busy terminal",
                None,
                None,
                &[],
                None,
                "term-coord",
            )
        })
        .unwrap();
    let mut busy_input = input(next.id, run);
    busy_input.agent = None;
    busy_input.terminal = Some(first.handle);
    let busy = lifecycle::worker_start(&env.engine, busy_input).unwrap_err();
    assert_eq!(busy.code, ErrorCode::TerminalBusy);

    let (mut depth_env, depth_run, depth_task) = seeded();
    depth_env.engine.config.defaults.nested_depth_max = 0;
    let depth =
        lifecycle::worker_start(&depth_env.engine, input(depth_task, depth_run)).unwrap_err();
    assert_eq!(depth.code, ErrorCode::NestedWorkerDepthExceeded);
}

#[test]
fn placement_current_new_child_new_top_level_path_branch() {
    let (env, _, _) = seeded();
    let path = env.tmp.path();
    let none = CreationOptions::default();
    placement::place(&env.engine, &Worktree::Current, &none, "ws", path).unwrap();
    let create = CreationOptions {
        name: Some("child"),
        repo: Some(path.to_str().unwrap()),
        ..CreationOptions::default()
    };
    placement::place(&env.engine, &Worktree::NewChild, &create, "ws", path).unwrap();
    placement::place(&env.engine, &Worktree::NewTopLevel, &create, "ws", path).unwrap();
    placement::place(&env.engine, &Worktree::Path(path.into()), &none, "ws", path).unwrap();
    env.herdr
        .add_worktree(&path.join("branch-worktree"), "refs/heads/branch");
    env.herdr
        .add_worktree(&path.join("short-worktree"), "short");
    placement::place(
        &env.engine,
        &Worktree::Branch("branch".into()),
        &none,
        "ws",
        path,
    )
    .unwrap();
    placement::place(
        &env.engine,
        &Worktree::Branch("short".into()),
        &none,
        "ws",
        path,
    )
    .unwrap();
    assert!(
        placement::place(
            &env.engine,
            &Worktree::Branch("missing".into()),
            &none,
            "ws",
            path,
        )
        .is_err()
    );
    // `name:` selects a configured repository by its folder name.
    let (mut named_env, _, _) = seeded();
    let repo = named_env.tmp.path().join("api");
    named_env
        .engine
        .config
        .repos
        .insert(repo.clone(), RepoConfig::default());
    let by_name = CreationOptions {
        name: Some("named"),
        repo: Some("name:api"),
        ..CreationOptions::default()
    };
    let named =
        placement::place(&named_env.engine, &Worktree::NewChild, &by_name, "ws", path).unwrap();
    assert_eq!(named.repo.as_deref(), Some(repo.as_path()));
    let unknown = CreationOptions {
        repo: Some("name:nope"),
        ..by_name
    };
    assert_eq!(
        placement::place(&named_env.engine, &Worktree::NewChild, &unknown, "ws", path)
            .unwrap_err()
            .code,
        ErrorCode::Usage
    );

    let calls = env.herdr.calls().join("\n");
    assert!(calls.contains("worktree_create:child"));
    assert!(calls.contains("worktree_list"));
    assert!(calls.contains(&format!(
        "worktree_open:{}",
        path.join("branch-worktree").display()
    )));
    assert!(calls.contains(&format!(
        "worktree_open:{}",
        path.join("short-worktree").display()
    )));
}

#[test]
fn creation_flags_rejected_for_current_and_existing() {
    let options = CreationOptions {
        name: Some("x"),
        ..CreationOptions::default()
    };
    assert!(placement::validate_creation_flags(&Worktree::Current, &options).is_err());
    assert!(
        placement::validate_creation_flags(&Worktree::Path(PathBuf::from("/tmp/x")), &options)
            .is_err()
    );
}

#[test]
fn repo_required_when_not_in_git() {
    let (env, _, _) = seeded();
    let options = CreationOptions {
        name: Some("x"),
        ..CreationOptions::default()
    };
    let error = placement::place(
        &env.engine,
        &Worktree::NewChild,
        &options,
        "ws",
        env.tmp.path(),
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::Usage);
}

#[test]
fn setup_wait_for_setup_blocks_prompt_until_exit_0_and_failure_fails_at_setup() {
    assert_eq!(
        lifecycle::parse_setup_exit("noise\n__DONE__0\n", "__DONE__"),
        Some(0)
    );
    assert_eq!(
        lifecycle::parse_setup_exit("__DONE__7\n", "__DONE__"),
        Some(7)
    );

    let (mut env, run, task) = seeded();
    configure_setup(&mut env, "wait-for-setup");
    env.herdr.script_setup_exit(0);
    let receipt = lifecycle::worker_start(&env.engine, new_child(&env, task, run)).unwrap();
    assert_eq!(receipt.state, "ready");
    assert_eq!(receipt.setup.state, "succeeded");
    let worker = worker_row(&env, &receipt.dispatch_id);
    let setup_pane = worker.setup_pane_id.clone().unwrap();
    assert_ne!(worker.pane_id.as_deref(), Some(setup_pane.as_str()));
    let calls = env.herdr.calls();
    let setup_enter = position(&calls, &format!("pane_send_keys:{setup_pane}:Enter"));
    let agent = position(&calls, "agent_start:");
    assert!(
        setup_enter < agent,
        "setup finished before the agent started"
    );

    let (mut env, run, task) = seeded();
    configure_setup(&mut env, "wait-for-setup");
    env.herdr.script_setup_exit(7);
    let receipt = lifecycle::worker_start(&env.engine, new_child(&env, task.clone(), run)).unwrap();
    assert_eq!(receipt.state, "failed");
    assert_eq!(receipt.failed_stage.as_deref(), Some("setup"));
    assert_eq!(receipt.setup.state, "failed");
    assert!(
        !env.herdr
            .calls()
            .iter()
            .any(|c| c.starts_with("agent_start:"))
    );
    assert!(
        receipt
            .residual_resources
            .iter()
            .any(|r| r.kind == "worktree" && r.role == "workspace")
    );
    assert!(
        receipt
            .residual_resources
            .iter()
            .any(|r| r.kind == "pane" && r.role == "setup")
    );
    assert_eq!(
        worker_row(&env, &receipt.dispatch_id).terminal_state,
        "reclaimable"
    );
    let task = env
        .engine
        .store
        .read(|tx| tasks::get(tx, &task))
        .unwrap()
        .unwrap();
    assert_eq!(task.status, "ready");
    // The setup pane is never closed by release.
    let released = lifecycle::release(&env.engine, &receipt.dispatch_id).unwrap();
    assert!(released.closed_tabs.is_empty());
    assert!(
        !env.herdr
            .calls()
            .iter()
            .any(|c| c.starts_with("pane_close") || c.starts_with("tab_close"))
    );
}

#[test]
fn setup_start_immediately_reports_running() {
    let (mut env, run, task) = seeded();
    configure_setup(&mut env, "start-immediately");
    let receipt = lifecycle::worker_start(&env.engine, new_child(&env, task, run)).unwrap();
    assert_eq!(
        receipt.state,
        "ready",
        "{:?}",
        worker_row(&env, &receipt.dispatch_id).last_error
    );
    assert_eq!(receipt.setup.state, "running");
    assert!(
        receipt
            .effects
            .iter()
            .any(|e| e.kind == "setup" && e.action == "started")
    );
    assert_eq!(
        (
            receipt.setup.requested.as_str(),
            receipt.setup.effective.as_str()
        ),
        ("run", "run")
    );
    assert!(receipt.setup.hook_found);
    assert_eq!(receipt.setup.startup_policy, "start-immediately");
}

#[test]
fn agent_start_failure_leaves_reclaimable_pane_with_recovery() {
    let (env, run, task) = seeded();
    env.herdr.fail_agent_start("scripted agent start failure");
    let receipt = lifecycle::worker_start(&env.engine, input(task, run)).unwrap();
    assert_eq!(receipt.state, "failed");
    assert_eq!(receipt.failed_stage.as_deref(), Some("agent"));
    assert_eq!(
        receipt.next_commands[0],
        format!("pod worker-release --dispatch {}", receipt.dispatch_id)
    );
    assert!(
        receipt
            .residual_resources
            .iter()
            .any(|r| r.kind == "pane" && r.role == "agent")
    );
    let worker = env
        .engine
        .store
        .read(|tx| workers::get(tx, &receipt.dispatch_id))
        .unwrap()
        .unwrap();
    assert_eq!(worker.terminal_state, "reclaimable");
}

#[test]
fn prompt_stalled_is_dispatched_unobserved_never_resent() {
    let (env, run, task) = seeded();
    env.herdr.set_render_sent_text(false);
    let receipt = lifecycle::worker_start(&env.engine, input(task.clone(), run)).unwrap();
    // The receipt agrees with the stored rows: the Dispatch stays recorded.
    assert_eq!(receipt.state, "ready");
    assert_eq!(receipt.outcome.as_deref(), Some("dispatched_unobserved"));
    let worker = worker_row(&env, &receipt.dispatch_id);
    assert_eq!(
        worker.preamble_input.as_deref(),
        Some("rendered_unconfirmed")
    );
    let task = env
        .engine
        .store
        .read(|tx| tasks::get(tx, &task))
        .unwrap()
        .unwrap();
    assert_eq!(task.status, "dispatched");
    let calls = env.herdr.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.starts_with("agent_prompt:"))
            .count(),
        1
    );
    assert!(!calls.iter().any(|call| call.starts_with("pane_send_text:")));
    assert!(!calls.iter().any(|call| call.starts_with("pane_send_keys:")));
}

#[test]
fn an_unconfirmed_paste_fallback_is_dispatched_unobserved_never_resent() {
    let (env, run, task) = seeded();
    env.herdr.set_render_sent_text(false);
    env.herdr.fail_agent_prompt("agent.prompt unsupported");
    let receipt = lifecycle::worker_start(&env.engine, input(task.clone(), run)).unwrap();
    assert_eq!(receipt.state, "ready");
    assert_eq!(receipt.outcome.as_deref(), Some("dispatched_unobserved"));
    let worker = worker_row(&env, &receipt.dispatch_id);
    assert_eq!(
        worker.preamble_input.as_deref(),
        Some("rendered_unconfirmed")
    );
    let calls = env.herdr.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.starts_with("pane_send_text:"))
            .count(),
        1
    );
    assert!(!calls.iter().any(|call| call.starts_with("pane_send_keys:")));
}

#[test]
fn reuse_terminal_moves_cleanup_ownership() {
    let (env, run, task) = seeded();
    let first = lifecycle::worker_start(&env.engine, input(task, run.clone())).unwrap();
    env.engine
        .store
        .tx(|tx| {
            tasks::settle_dispatch(
                tx,
                env.engine.clock.now(),
                &first.dispatch_id,
                tasks::Outcome::Succeeded,
                "no_follow_on",
                None,
            )
        })
        .unwrap();
    let next = env
        .engine
        .store
        .tx(|tx| {
            tasks::create(
                tx,
                env.engine.clock.now(),
                &run,
                "next",
                None,
                None,
                &[],
                None,
                "term-coord",
            )
        })
        .unwrap();
    let mut reuse = input(next.id, run);
    reuse.agent = None;
    reuse.terminal = Some(first.handle.clone());
    let second = lifecycle::worker_start(&env.engine, reuse).unwrap();
    let (old, new) = env
        .engine
        .store
        .read(|tx| {
            Ok((
                workers::get(tx, &first.dispatch_id)?.unwrap(),
                workers::get(tx, &second.dispatch_id)?.unwrap(),
            ))
        })
        .unwrap();
    assert_eq!(old.terminal_state, "released");
    assert!(new.owns_pane);
}

#[test]
fn release_archives_screen_closes_only_pod_created_tab_and_is_idempotent() {
    let (env, run, task) = seeded();
    let start = lifecycle::worker_start(&env.engine, input(task, run)).unwrap();
    env.engine
        .store
        .tx(|tx| {
            tasks::settle_dispatch(
                tx,
                env.engine.clock.now(),
                &start.dispatch_id,
                tasks::Outcome::Succeeded,
                "no_follow_on",
                None,
            )
        })
        .unwrap();
    let first = lifecycle::release(&env.engine, &start.dispatch_id).unwrap();
    let second = lifecycle::release(&env.engine, &start.dispatch_id).unwrap();
    assert_eq!(first.closed_tabs.len(), 1);
    assert!(second.closed_tabs.is_empty() && second.archive.bytes.is_some());
}

#[test]
fn release_leaves_user_taken_over_setup_and_coordinator_panes() {
    // Not created by pod.
    let (env, run, task) = seeded();
    let start = lifecycle::worker_start(&env.engine, input(task, run)).unwrap();
    env.engine
        .store
        .tx(|tx| {
            workers::update_owns_pane(tx, &start.dispatch_id, false)?;
            workers::update_terminal_state(tx, &start.dispatch_id, "reclaimable")
        })
        .unwrap();
    let receipt = lifecycle::release(&env.engine, &start.dispatch_id).unwrap();
    assert!(receipt.closed_tabs.is_empty());
    assert_eq!(receipt.left_alone, ["pane_not_created_by_pod"]);

    // User-taken-over, and closed by the user.
    let (env, run, task) = seeded();
    let start = lifecycle::worker_start(&env.engine, input(task, run)).unwrap();
    settle(&env, &start.dispatch_id);
    let pane = worker_row(&env, &start.dispatch_id).pane_id.unwrap();
    env.herdr.pane_close(&pane).unwrap();
    let receipt = lifecycle::release(&env.engine, &start.dispatch_id).unwrap();
    assert!(receipt.closed_tabs.is_empty());
    assert_eq!(receipt.left_alone, ["user_taken_over"]);
    assert_eq!(
        receipt.worker.user_owned_reason.as_deref(),
        Some("pane_closed_outside_pod")
    );

    // The coordinator's own pane.
    let (env, run, task) = seeded();
    let start = lifecycle::worker_start(&env.engine, input(task, run)).unwrap();
    settle(&env, &start.dispatch_id);
    let coordinator = env.engine.caller.clone().unwrap();
    env.engine
        .store
        .tx(|tx| {
            workers::update_pane_id(tx, &start.dispatch_id, coordinator.pane_id.as_deref())?;
            workers::update_terminal_id(tx, &start.dispatch_id, Some(&coordinator.terminal_id))
        })
        .unwrap();
    let receipt = lifecycle::release(&env.engine, &start.dispatch_id).unwrap();
    assert!(receipt.closed_tabs.is_empty());
    assert_eq!(receipt.left_alone, ["coordinator_pane"]);

    // An active worker is not released at all.
    let (env, run, task) = seeded();
    let start = lifecycle::worker_start(&env.engine, input(task, run)).unwrap();
    let receipt = lifecycle::release(&env.engine, &start.dispatch_id).unwrap();
    assert!(receipt.closed_tabs.is_empty());
    assert_eq!(receipt.worker.terminal_state, "active");
}

#[test]
fn release_after_stop_does_not_mark_the_pane_user_owned() {
    let (env, run, task) = seeded();
    let start = lifecycle::worker_start(&env.engine, input(task, run)).unwrap();
    lifecycle::stop(&env.engine, &start.dispatch_id).unwrap();
    let receipt = lifecycle::release(&env.engine, &start.dispatch_id).unwrap();
    assert_eq!(receipt.worker.terminal_state, "released");
    assert!(!receipt.worker.user_owned);
    assert_eq!(receipt.left_alone, ["pane_already_closed"]);
}

/// `dispatch --to` types the brief by default and takes over the pane's
/// cleanup like reuse (spec 7.4): the worker that last held the pane is
/// released, ownership carries, and releasing the new Dispatch closes the
/// tab pod created. Seen live 2026-10-15: four dispatches to retained
/// workers recorded `dispatched` while nothing reached the terminal, and
/// every finished pane had to be closed by hand.
#[test]
fn dispatch_to_a_handle_injects_and_takes_over_the_pane() {
    let (env, run, task) = seeded();
    let start = lifecycle::worker_start(&env.engine, input(task, run.clone())).unwrap();
    settle(&env, &start.dispatch_id);
    lifecycle::retain(&env.engine, &start.dispatch_id, "keep for the next task").unwrap();
    let typed = |calls: &[String]| {
        calls
            .iter()
            .filter(|call| call.starts_with("agent_prompt:") || call.starts_with("pane_send_text:"))
            .count()
    };
    let before = typed(&env.herdr.calls());
    let next = new_task(&env, &run, "follow-up");
    let receipt =
        lifecycle::dispatch(&env.engine, &next, &start.handle, true, false, false).unwrap();
    assert!(receipt.injected, "{receipt:?}");
    assert!(
        typed(&env.herdr.calls()) > before,
        "{:?}",
        env.herdr.calls()
    );
    let id = receipt.dispatch.as_ref().unwrap().id.clone();

    let (old, new) = env
        .engine
        .store
        .read(|tx| {
            Ok((
                workers::get(tx, &start.dispatch_id)?.unwrap(),
                workers::get(tx, &id)?.unwrap(),
            ))
        })
        .unwrap();
    assert_eq!(old.terminal_state, "released");
    assert!(new.owns_pane);
    assert_eq!(new.tab_id, old.tab_id);
    assert!(!new.user_owned);

    // Releasing the old worker again changes nothing; releasing the new one,
    // once it has finished, closes the tab.
    let again = lifecycle::release(&env.engine, &start.dispatch_id).unwrap();
    assert!(again.closed_tabs.is_empty());
    settle(&env, &id);
    let released = lifecycle::release(&env.engine, &id).unwrap();
    assert_eq!(released.closed_tabs, vec![old.tab_id.clone().unwrap()]);
    assert!(released.left_alone.is_empty(), "{released:?}");
}

/// `worker-gc` releases every reclaimable worker of the Run under the
/// release rules and keeps retained and active ones, listing why.
#[test]
fn worker_gc_releases_every_reclaimable_worker_and_keeps_the_rest() {
    let (env, run, task) = seeded();
    let done = lifecycle::worker_start(&env.engine, input(task, run.clone())).unwrap();
    settle(&env, &done.dispatch_id);
    let kept = lifecycle::worker_start(
        &env.engine,
        input(new_task(&env, &run, "kept"), run.clone()),
    )
    .unwrap();
    settle(&env, &kept.dispatch_id);
    lifecycle::retain(&env.engine, &kept.dispatch_id, "user reads it").unwrap();
    let busy = lifecycle::worker_start(
        &env.engine,
        input(new_task(&env, &run, "busy"), run.clone()),
    )
    .unwrap();

    let receipt = lifecycle::gc(&env.engine, Some(&run)).unwrap();
    assert_eq!(receipt.run_id, run);
    assert_eq!(
        receipt
            .released
            .iter()
            .map(|worker| worker.dispatch_id.clone())
            .collect::<Vec<_>>(),
        vec![done.dispatch_id.clone()]
    );
    assert_eq!(
        receipt.released[0].closed_tabs.len(),
        1,
        "{:?}",
        receipt.released[0]
    );
    let mut kept_ids: Vec<(String, String)> = receipt
        .kept
        .iter()
        .map(|row| (row.dispatch_id.to_string(), row.reason.clone()))
        .collect();
    kept_ids.sort();
    let mut expected = vec![
        (kept.dispatch_id.to_string(), "retained".to_owned()),
        (busy.dispatch_id.to_string(), "active".to_owned()),
    ];
    expected.sort();
    assert_eq!(kept_ids, expected);

    // A second sweep, on the bound Run by default, finds nothing to release.
    let again = lifecycle::gc(&env.engine, None).unwrap();
    assert_eq!(again.run_id, run);
    assert!(again.released.is_empty(), "{again:?}");
    assert_eq!(again.kept.len(), 2);
}

#[test]
fn worker_user_owned_migration_setter_and_release_skip() {
    let (env, run, task) = seeded();
    assert_eq!(env.engine.store.user_version().unwrap(), 3);
    let start = lifecycle::worker_start(&env.engine, input(task, run)).unwrap();
    env.engine
        .store
        .tx(|tx| {
            tasks::settle_dispatch(
                tx,
                env.engine.clock.now(),
                &start.dispatch_id,
                tasks::Outcome::Succeeded,
                "no_follow_on",
                None,
            )?;
            lifecycle::mark_user_owned(tx, env.engine.clock.now(), &start.dispatch_id, "pane_input")
        })
        .unwrap();
    let receipt = lifecycle::release(&env.engine, &start.dispatch_id).unwrap();
    assert!(receipt.closed_tabs.is_empty());
    assert_eq!(
        receipt.worker.user_owned_reason.as_deref(),
        Some("pane_input")
    );
    let event = env
        .engine
        .store
        .read(|tx| pod::store::rows::events::list(tx, &receipt.dispatch.run_id))
        .unwrap()
        .into_iter()
        .find(|event| event.r#type == "worker.user_owned")
        .unwrap();
    assert_eq!(event.dispatch_id.as_ref(), Some(&start.dispatch_id));
}

#[test]
fn stop_records_canceled_closes_only_supervised_pane_and_does_not_overwrite_accepted_ending() {
    let (env, run, task) = seeded();
    let start = lifecycle::worker_start(&env.engine, input(task.clone(), run.clone())).unwrap();
    let pane = worker_row(&env, &start.dispatch_id).pane_id.unwrap();
    let stopped = lifecycle::stop(&env.engine, &start.dispatch_id).unwrap();
    assert_eq!(
        stopped.dispatch.termination_reason.as_deref(),
        Some("canceled")
    );
    assert_eq!(stopped.dispatch.status, "failed");
    assert_eq!(stopped.dispatch.failure_count, 1);
    assert_eq!(stopped.closed_panes, std::slice::from_ref(&pane));
    assert_eq!(stopped.worker.state, "stopped");
    assert_eq!(stopped.worker.terminal_state, "reclaimable");
    let closes = |env: &TestEnv| {
        env.herdr
            .calls()
            .iter()
            .filter(|call| call.starts_with("pane_close:") || call.starts_with("tab_close:"))
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(closes(&env), [format!("pane_close:{pane}")]);
    let task_row = env
        .engine
        .store
        .read(|tx| tasks::get(tx, &task))
        .unwrap()
        .unwrap();
    assert_eq!(task_row.status, "ready");
    let again = lifecycle::stop(&env.engine, &start.dispatch_id).unwrap();
    assert_eq!(again.dispatch.failure_count, 1);
    assert_eq!(closes(&env).len(), 1);

    // An accepted ending stands: neither the Dispatch nor the worker changes.
    let (env, run, task) = seeded();
    let start = lifecycle::worker_start(&env.engine, input(task, run)).unwrap();
    settle(&env, &start.dispatch_id);
    let receipt = lifecycle::stop(&env.engine, &start.dispatch_id).unwrap();
    assert_eq!(receipt.dispatch.status, "completed");
    assert_eq!(
        receipt.dispatch.termination_reason.as_deref(),
        Some("no_follow_on")
    );
    assert_eq!(receipt.worker.state, "succeeded");
    assert_eq!(receipt.worker.terminal_state, "reclaimable");
    assert!(receipt.closed_panes.is_empty());
    assert!(closes(&env).is_empty());
}

#[test]
fn abandon_touches_no_process() {
    let (env, run, task) = seeded();
    let start = lifecycle::worker_start(&env.engine, input(task, run)).unwrap();
    let before = env.herdr.calls().len();
    let receipt = lifecycle::abandon(&env.engine, &start.dispatch_id).unwrap();
    assert_eq!(receipt.worker.state, "abandoned");
    // Only the sidebar status is written (spec 12); no pane is touched.
    let after = env.herdr.calls();
    assert!(
        after[before..].iter().all(|call| {
            call.starts_with("workspace_report_metadata:") || call.starts_with("workspace_get:")
        }),
        "{:?}",
        &after[before..]
    );
}

#[test]
fn retry_of_requires_failed_or_blocked_and_never_inherits_placement() {
    let (env, run, task) = seeded();
    let mut start = input(task.clone(), run.clone());
    start.retry_of = Some(DispatchId::new());
    assert_eq!(
        lifecycle::worker_start(&env.engine, start)
            .unwrap_err()
            .code,
        ErrorCode::TaskNotStartable
    );

    // Three failed starts at placement break the circuit and fail the Task.
    let mut last = None;
    for _ in 0..3 {
        let mut attempt = input(task.clone(), run.clone());
        attempt.worktree = Worktree::Branch("missing".into());
        let receipt = lifecycle::worker_start(&env.engine, attempt).unwrap();
        assert_eq!(receipt.failed_stage.as_deref(), Some("placement"));
        last = Some(receipt.dispatch_id);
    }
    let last = last.unwrap();
    let broken = env
        .engine
        .store
        .read(|tx| dispatches::get(tx, &last))
        .unwrap()
        .unwrap();
    assert_eq!(broken.status, "circuit_broken");
    assert_eq!(worker_row(&env, &last).terminal_state, "released");
    assert_eq!(
        env.engine
            .store
            .read(|tx| tasks::get(tx, &task))
            .unwrap()
            .unwrap()
            .status,
        "failed"
    );

    let mut retry = input(task.clone(), run);
    retry.retry_of = Some(last.clone());
    let receipt = lifecycle::worker_start(&env.engine, retry).unwrap();
    assert_eq!(receipt.state, "ready");
    let row = env
        .engine
        .store
        .read(|tx| dispatches::get(tx, &receipt.dispatch_id))
        .unwrap()
        .unwrap();
    assert_eq!(row.retry_of_dispatch_id.as_ref(), Some(&last));
    assert_eq!(row.failure_count, 3);
    assert_eq!(
        worker_row(&env, &receipt.dispatch_id).placement,
        Some(serde_json::json!("current"))
    );
}

#[test]
fn spec_with_unmet_dependencies_creates_no_task() {
    let (env, run, task) = seeded();
    let mut start = input(task.clone(), run.clone());
    start.task = None;
    start.spec = Some("waits on the first task".into());
    start.deps = vec![task];
    let error = lifecycle::worker_start(&env.engine, start).unwrap_err();
    assert_eq!(error.code, ErrorCode::TaskNotStartable);
    let tasks = env
        .engine
        .store
        .read(|tx| tasks::list_by_run(tx, &run, None))
        .unwrap();
    assert_eq!(tasks.len(), 1);
}

#[test]
fn a_worker_cannot_start_a_worker_by_naming_the_run() {
    let (mut env, run, task) = seeded();
    let start = lifecycle::worker_start(&env.engine, input(task, run.clone())).unwrap();
    let worker_terminal = worker_row(&env, &start.dispatch_id).terminal_id.unwrap();
    let next = new_task(&env, &run, "nested");
    env.as_caller(&worker_terminal, 77);
    let error = lifecycle::worker_start(&env.engine, input(next.clone(), run)).unwrap_err();
    assert_eq!(error.code, ErrorCode::NestedWorkerDepthExceeded);
    let error =
        lifecycle::dispatch(&env.engine, &next, &start.handle, false, true, false).unwrap_err();
    assert_eq!(error.code, ErrorCode::NestedWorkerDepthExceeded);
}

#[test]
fn reuse_rejects_worktree_and_existing_worktrees_get_a_new_tab() {
    let (env, run, task) = seeded();
    let mut reuse = input(task.clone(), run.clone());
    reuse.agent = None;
    reuse.terminal = Some("codex-0000".into());
    reuse.worktree = Worktree::Path(env.tmp.path().into());
    assert_eq!(
        lifecycle::worker_start(&env.engine, reuse)
            .unwrap_err()
            .code,
        ErrorCode::Usage
    );

    let mut existing = input(task, run);
    env.herdr
        .add_worktree(&env.tmp.path().join("checkout"), "checkout");
    existing.worktree = Worktree::Path(env.tmp.path().join("checkout"));
    let receipt = lifecycle::worker_start(&env.engine, existing).unwrap();
    assert_eq!(receipt.state, "ready");
    let worker = worker_row(&env, &receipt.dispatch_id);
    // worktree.open's own tab may belong to the user; the worker gets its own.
    assert!(
        env.herdr
            .calls()
            .iter()
            .any(|c| c.starts_with("tab_create:workspace_1:"))
    );
    settle(&env, &receipt.dispatch_id);
    let released = lifecycle::release(&env.engine, &receipt.dispatch_id).unwrap();
    assert_eq!(released.closed_tabs, [worker.tab_id.unwrap()]);
}

#[test]
fn dispatch_inject_failure_counts_toward_breaker() {
    let (env, _run, task) = seeded();
    target_agent(&env);
    // herdr cannot prompt, so the paste fallback runs and fails.
    env.herdr.fail_agent_prompt("agent.prompt unsupported");
    env.herdr.fail_pane_send_text("scripted injection failure");
    let receipt = lifecycle::dispatch(&env.engine, &task, "target", true, false, false).unwrap();
    assert_eq!(receipt.outcome.as_deref(), Some("failed"));
    assert!(!receipt.injected);
    let dispatch = receipt.dispatch.unwrap();
    let row = env
        .engine
        .store
        .read(|tx| dispatches::get(tx, &dispatch.id))
        .unwrap()
        .unwrap();
    assert_eq!(row.failure_count, 1);
    assert_eq!(row.status, "failed");
    // pod created no pane, so nothing is left to reclaim.
    assert_eq!(worker_row(&env, &dispatch.id).terminal_state, "released");
    let task = env
        .engine
        .store
        .read(|tx| tasks::get(tx, &task))
        .unwrap()
        .unwrap();
    assert_eq!(task.status, "ready");
}

#[test]
fn dispatch_unconfirmed_injection_stays_dispatched() {
    let (env, _run, task) = seeded();
    target_agent(&env);
    env.herdr.set_render_sent_text(false);
    let receipt = lifecycle::dispatch(&env.engine, &task, "target", true, false, false).unwrap();
    assert_eq!(receipt.outcome.as_deref(), Some("dispatched_unobserved"));
    assert_eq!(receipt.dispatch.unwrap().status, "dispatched");
}

#[test]
fn dispatch_dry_run_returns_preamble() {
    let (env, _run, task) = seeded();
    env.herdr.add_pane("target-pane", "target-term", "ws");
    env.herdr.set_agent(
        "target",
        pod::herdr::AgentInfo {
            name: "target".into(),
            kind: "codex".into(),
            pane_id: "target-pane".into(),
            pid: 9,
            status: pod::herdr::AgentStatus::Idle,
            session_id: None,
            session_path: None,
            needs_input: vec![],
            observed_at: env.engine.clock.now(),
        },
    );
    let receipt = lifecycle::dispatch(&env.engine, &task, "target", false, true, true).unwrap();
    assert!(
        receipt.dry_run
            && receipt.dispatch.is_none()
            && receipt.preamble.unwrap().contains("=== TASK ===")
    );
}

#[test]
fn worker_list_pagination_scope_source_and_reclaimable_filter() {
    let (mut env, run, task) = seeded();
    let start = lifecycle::worker_start(&env.engine, input(task, run.clone())).unwrap();
    settle(&env, &start.dispatch_id);
    let list = lifecycle::list(&env.engine, Some(&run), Some("reclaimable"), 1, None).unwrap();
    assert_eq!(list.scope.source, "flag");
    assert_eq!(list.workers.len(), 1);

    let mut started = vec![start.dispatch_id];
    for title in ["second", "third"] {
        let next = new_task(&env, &run, title);
        started.push(
            lifecycle::worker_start(&env.engine, input(next, run.clone()))
                .unwrap()
                .dispatch_id,
        );
    }
    let page = lifecycle::list(&env.engine, None, None, 2, None).unwrap();
    assert_eq!(page.scope.source, "bound");
    assert_eq!(page.scope.run.as_ref(), Some(&run));
    let ids = page
        .workers
        .iter()
        .map(|w| w.dispatch.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(ids, [started[2].clone(), started[1].clone()]);
    let cursor = page.next_cursor.unwrap();
    let rest = lifecycle::list(&env.engine, None, None, 2, Some(&cursor)).unwrap();
    assert_eq!(rest.workers.len(), 1);
    assert_eq!(rest.workers[0].dispatch.id, started[0]);
    assert!(rest.next_cursor.is_none());
    assert!(lifecycle::list(&env.engine, None, None, 0, None).is_err());
    assert!(lifecycle::list(&env.engine, None, None, 101, None).is_err());

    // A caller bound to no Run lists every Run, and says so.
    env.as_caller("term-unbound", 7);
    assert_eq!(
        lifecycle::list(&env.engine, None, None, 100, None)
            .unwrap()
            .scope
            .source,
        "all"
    );
}

#[test]
fn setup_command_quotes_tokens_and_exit_sentinel_parses() {
    let command =
        lifecycle::setup_command(&["printf".into(), "a'b".into(), "$(no)".into()], "__S__");
    assert!(command.contains("'a'\\''b'") && command.contains("'$(no)'"));
    assert_eq!(
        lifecycle::parse_setup_exit("\n__S__12\n", "__S__"),
        Some(12)
    );
}

fn worker_row(env: &TestEnv, dispatch: &DispatchId) -> workers::WorkerRow {
    env.engine
        .store
        .read(|tx| workers::get(tx, dispatch))
        .unwrap()
        .unwrap()
}

fn settle(env: &TestEnv, dispatch: &DispatchId) {
    env.engine
        .store
        .tx(|tx| {
            tasks::settle_dispatch(
                tx,
                env.engine.clock.now(),
                dispatch,
                tasks::Outcome::Succeeded,
                "no_follow_on",
                None,
            )
        })
        .unwrap();
}

fn new_task(env: &TestEnv, run: &RunId, spec: &str) -> TaskId {
    env.engine
        .store
        .tx(|tx| {
            tasks::create(
                tx,
                env.engine.clock.now(),
                run,
                spec,
                None,
                None,
                &[],
                None,
                "term-coord",
            )
        })
        .unwrap()
        .id
}

fn configure_setup(env: &mut TestEnv, policy: &str) {
    env.engine.config.repos.insert(
        env.tmp.path().to_path_buf(),
        RepoConfig {
            base_branch: None,
            setup: vec!["bash".into(), "scripts/setup.sh".into()],
            setup_policy: policy.into(),
        },
    );
}

fn new_child(env: &TestEnv, task: TaskId, run: RunId) -> StartInput {
    StartInput {
        worktree: Worktree::NewChild,
        name: Some("child".into()),
        repo: Some(format!("path:{}", env.tmp.path().display())),
        ..input(task, run)
    }
}

fn position(calls: &[String], prefix: &str) -> usize {
    calls
        .iter()
        .position(|call| call.starts_with(prefix))
        .unwrap_or_else(|| panic!("no call starting with {prefix}: {calls:?}"))
}

fn target_agent(env: &TestEnv) {
    env.herdr.add_pane("target-pane", "target-term", "ws");
    env.herdr.set_agent(
        "target",
        pod::herdr::AgentInfo {
            name: "target".into(),
            kind: "codex".into(),
            pane_id: "target-pane".into(),
            pid: 9,
            status: pod::herdr::AgentStatus::Idle,
            session_id: None,
            session_path: None,
            needs_input: Vec::new(),
            // herdr stamps its agent list with the real time of the query.
            observed_at: chrono::Utc::now(),
        },
    );
}

/// T9's proof order end to end on FakeHerdr: a Claude start passes
/// `--session-id` and records `pod_assigned`, `worker-read` finds that
/// transcript under `HOME`, and a session herdr reports wins over pod's.
#[test]
fn claude_start_assigns_session_and_worker_read_follows_the_proof_order() {
    let (env, run, task) = seeded();
    let mut claude = input(task, run.clone());
    claude.agent = Some("claude".into());
    let receipt = lifecycle::worker_start(&env.engine, claude).unwrap();
    assert_eq!(receipt.state, "ready");
    let argv = env.herdr.agent_start_argv().pop().unwrap();
    let flag = argv.iter().position(|arg| arg == "--session-id").unwrap();
    let session = argv[flag + 1].clone();
    let worker = worker_row(&env, &receipt.dispatch_id);
    assert_eq!(worker.session_id.as_deref(), Some(session.as_str()));
    assert_eq!(worker.transcript_proof.as_deref(), Some("pod_assigned"));
    assert_eq!(worker.transcript_path, None);

    let cwd = worker.worktree_path.clone().unwrap();
    let slug: String = cwd
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let project = env.tmp.path().join(".claude/projects").join(slug);
    std::fs::create_dir_all(&project).unwrap();
    std::fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/transcript/samples/claude/7563e397-f26f-4e62-9345-99e7637e8deb.jsonl"),
        project.join(format!("{session}.jsonl")),
    )
    .unwrap();
    let output = env
        .pod(None)
        .args([
            "worker-read",
            "--dispatch",
            receipt.dispatch_id.as_str(),
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(value["source"], "transcript", "{value}");
    assert!(value.get("fallbackReason").is_none(), "{value}");

    // herdr's report is the first proof: it replaces the assigned ID.
    let reported = env.tmp.path().join("reported.jsonl");
    env.herdr
        .report_agent_session(Some("herdr-session"), Some(&reported));
    let second = new_task(&env, &run, "second");
    let mut claude = input(second, run);
    claude.agent = Some("claude".into());
    let receipt = lifecycle::worker_start(&env.engine, claude).unwrap();
    let worker = worker_row(&env, &receipt.dispatch_id);
    assert_eq!(worker.session_id.as_deref(), Some("herdr-session"));
    assert_eq!(worker.transcript_proof.as_deref(), Some("herdr"));
    assert_eq!(
        worker.transcript_path.as_deref(),
        Some(reported.display().to_string().as_str())
    );

    // A kind with no launch flag for sessions gets none and stays unproven.
    let (env, run, task) = seeded();
    let receipt = lifecycle::worker_start(&env.engine, input(task, run)).unwrap();
    assert!(!env.herdr.agent_start_argv()[0].contains(&"--session-id".to_owned()));
    assert_eq!(
        worker_row(&env, &receipt.dispatch_id).transcript_proof,
        None
    );
}

/// Every row of the 7.6 table, observed the way production observes it
/// (herdr events through `reconcile::event`), comes out of `worker-show`
/// and `worker-list`, with a `nextAction` naming a real pod command.
#[test]
fn show_and_list_project_every_7_6_row_from_herdr_events() {
    use pod::engine::reconcile;
    use pod::herdr::{AgentStatus, HerdrEvent};
    let (env, run, task) = seeded();
    let receipt = lifecycle::worker_start(&env.engine, input(task, run.clone())).unwrap();
    let dispatch = receipt.dispatch_id.clone();
    let pane = worker_row(&env, &dispatch).pane_id.unwrap();
    let show = |env: &TestEnv| lifecycle::show(&env.engine, &dispatch).unwrap().projection;

    // herdr detected the agent it started: present, and nothing to do.
    let started = show(&env);
    assert_eq!(started.liveness, "live");
    assert_eq!(started.next_action.kind, "none");

    // herdr no longer detects an agent in the pane: presence is unproven,
    // and nothing destructive is suggested.
    env.herdr.remove_agent(&receipt.handle);
    reconcile::startup(&env.engine).unwrap();
    let unproven = show(&env);
    assert_eq!(unproven.liveness, "unverifiable");
    assert_eq!(unproven.attention.categories, ["unverifiable"]);
    assert_eq!(unproven.next_action.kind, "none");

    let status = |status, needs: &[&str]| HerdrEvent::AgentStatusChanged {
        pane_id: pane.clone(),
        status,
        needs_input: needs.iter().map(|s| s.to_string()).collect(),
    };
    reconcile::event(&env.engine, status(AgentStatus::Working, &[])).unwrap();
    let live = show(&env);
    assert_eq!(
        (live.liveness.as_str(), live.activity.as_str()),
        ("live", "working")
    );
    assert!(live.attention.categories.is_empty());

    reconcile::event(&env.engine, status(AgentStatus::Blocked, &["approval"])).unwrap();
    let approval = show(&env);
    assert_eq!(approval.attention.categories, ["approval"]);
    assert!(approval.attention.requires_action);
    assert_eq!(
        approval.next_action.argv,
        ["pod", "worker-read", "--dispatch", dispatch.as_str()]
    );

    env.clock.advance(std::time::Duration::from_secs(601));
    reconcile::event(&env.engine, status(AgentStatus::Working, &[])).unwrap();
    assert_eq!(show(&env).attention.categories, ["stale"]);

    reconcile::event(
        &env.engine,
        HerdrEvent::PaneExited {
            pane_id: pane.clone(),
        },
    )
    .unwrap();
    let exited = show(&env);
    assert_eq!(exited.liveness, "exited");
    assert_eq!(exited.outcome, "outcome_unknown");
    assert!(exited.attention.categories.contains(&"failure".to_owned()));
    assert_eq!(exited.next_action.kind, "worker-show");

    // A recorded ending beats the exit; the settled worker is reclaimable.
    settle(&env, &dispatch);
    let settled = show(&env);
    assert_eq!(settled.outcome, "succeeded");
    assert!(!settled.attention.categories.contains(&"failure".to_owned()));
    assert_eq!(
        settled.next_action.argv,
        ["pod", "worker-release", "--dispatch", dispatch.as_str()]
    );
    let listed = lifecycle::list(&env.engine, Some(&run), None, 10, None).unwrap();
    assert_eq!(listed.workers[0].projection, settled);
    assert_eq!(listed.counts.get("reclaimable"), Some(&1));
    lifecycle::release(&env.engine, &dispatch).unwrap();
    assert_eq!(show(&env).next_action.kind, "none");

    // A stop is projected as `stopped`, not as the fenced `failed`.
    let second = new_task(&env, &run, "second");
    let started = lifecycle::worker_start(&env.engine, input(second, run.clone())).unwrap();
    // Row 5: five nudge attempts with no consuming check.
    let now = env.engine.clock.now();
    env.engine
        .store
        .tx(|tx| {
            for attempt in 1..=5 {
                pod::store::rows::wake_attempts::insert(
                    tx,
                    &pod::store::rows::wake_attempts::WakeAttemptRow {
                        id: pod::ids::WakeId::new(),
                        run_id: run.clone(),
                        mailbox: format!("dispatch:{}", started.dispatch_id),
                        delivery_id: None,
                        attempt,
                        outcome: "skipped".into(),
                        detail: None,
                        created_at: now,
                    },
                )?;
            }
            Ok(())
        })
        .unwrap();
    let unreachable = lifecycle::show(&env.engine, &started.dispatch_id).unwrap();
    assert!(unreachable.wake_ladder.exhausted);
    assert_eq!(unreachable.wake_ladder.attempts.len(), 5);
    assert!(
        unreachable
            .projection
            .attention
            .categories
            .contains(&"unreachable".to_owned())
    );
    assert_eq!(unreachable.projection.next_action.kind, "worker-show");
    lifecycle::stop(&env.engine, &started.dispatch_id).unwrap();
    let stopped = lifecycle::show(&env.engine, &started.dispatch_id).unwrap();
    assert_eq!(stopped.projection.outcome, "stopped");
}

/// FALLBACK for herdr's missing pane-input event: a pane close pod did not
/// make marks the worker user-owned through the hook and the startup
/// reconcile, a close by pod's own stop does not, and a user-owned worker
/// stays reclaimable until released without pod closing anything.
#[test]
fn pane_closed_outside_pod_is_user_owned_and_still_accounted() {
    use pod::engine::reconcile;
    use pod::herdr::HerdrEvent;
    let (env, run, task) = seeded();
    let first = lifecycle::worker_start(&env.engine, input(task, run.clone())).unwrap();
    let pane = worker_row(&env, &first.dispatch_id).pane_id.unwrap();
    env.herdr.pane_close(&pane).unwrap();
    reconcile::event(
        &env.engine,
        HerdrEvent::PaneClosed {
            pane_id: pane.clone(),
        },
    )
    .unwrap();
    let worker = worker_row(&env, &first.dispatch_id);
    assert!(worker.user_owned);
    assert_eq!(
        worker.user_owned_reason.as_deref(),
        Some("pane_closed_outside_pod")
    );
    settle(&env, &first.dispatch_id);
    let reclaimable =
        lifecycle::list(&env.engine, Some(&run), Some("reclaimable"), 10, None).unwrap();
    assert_eq!(reclaimable.workers.len(), 1);
    let stop = lifecycle::stop(&env.engine, &first.dispatch_id).unwrap();
    assert!(stop.closed_panes.is_empty());
    let released = lifecycle::release(&env.engine, &first.dispatch_id).unwrap();
    assert!(released.closed_tabs.is_empty());
    assert_eq!(released.left_alone, ["user_taken_over"]);
    assert_eq!(released.worker.terminal_state, "released");

    // pod's own stop closes the pane; the hook that follows marks nothing.
    let second = new_task(&env, &run, "second");
    let stopped = lifecycle::worker_start(&env.engine, input(second, run.clone())).unwrap();
    let pane = worker_row(&env, &stopped.dispatch_id).pane_id.unwrap();
    lifecycle::stop(&env.engine, &stopped.dispatch_id).unwrap();
    reconcile::event(&env.engine, HerdrEvent::PaneClosed { pane_id: pane }).unwrap();
    assert!(!worker_row(&env, &stopped.dispatch_id).user_owned);

    // W10: a stop whose close was unconfirmed is `stopped` once herdr
    // reports the pane closed, and that close is not a takeover.
    let fourth = new_task(&env, &run, "fourth");
    let unconfirmed = lifecycle::worker_start(&env.engine, input(fourth, run.clone())).unwrap();
    let pane = worker_row(&env, &unconfirmed.dispatch_id).pane_id.unwrap();
    lifecycle::stop(&env.engine, &unconfirmed.dispatch_id).unwrap();
    env.engine
        .store
        .tx(|tx| workers::update_state(tx, &unconfirmed.dispatch_id, "stop_unknown"))
        .unwrap();
    reconcile::event(&env.engine, HerdrEvent::PaneClosed { pane_id: pane }).unwrap();
    let worker = worker_row(&env, &unconfirmed.dispatch_id);
    assert_eq!(worker.state, "stopped");
    assert!(!worker.user_owned);

    // A pane gone while pod was down is found by the startup reconcile.
    let third = new_task(&env, &run, "third");
    let missing = lifecycle::worker_start(&env.engine, input(third, run)).unwrap();
    let pane = worker_row(&env, &missing.dispatch_id).pane_id.unwrap();
    env.herdr.pane_close(&pane).unwrap();
    reconcile::startup(&env.engine).unwrap();
    assert!(worker_row(&env, &missing.dispatch_id).user_owned);
}

/// `worker-stop` closes the pane, so it keeps the final screen first; the
/// release that follows reports it and `worker-read` can still read it.
#[test]
fn stop_archives_the_final_screen_before_closing_the_pane() {
    let (env, run, task) = seeded();
    let started = lifecycle::worker_start(&env.engine, input(task, run)).unwrap();
    let pane = worker_row(&env, &started.dispatch_id).pane_id.unwrap();
    env.herdr.set_screen(&pane, "last words\n");
    let stopped = lifecycle::stop(&env.engine, &started.dispatch_id).unwrap();
    assert_eq!(stopped.closed_panes, [pane]);
    assert_eq!(stopped.archive.status, "preserved");
    let released = lifecycle::release(&env.engine, &started.dispatch_id).unwrap();
    assert_eq!(released.archive.bytes, Some("last words\n".len()));
    let read = pod::cli::commands::workers::read::worker_read(
        &env.engine,
        &started.dispatch_id,
        pod::cli::commands::workers::read::Source::Terminal,
        10,
        None,
    )
    .unwrap();
    assert_eq!(read.rows[0].text, "last words");
}

/// Each lifecycle mutation writes its events exactly once, from one module,
/// so `log` and the board never see an action twice.
#[test]
fn lifecycle_mutations_write_each_event_once() {
    use pod::store::rows::events;
    let (env, run, task) = seeded();
    let types = |env: &TestEnv| {
        env.engine
            .store
            .read(|tx| events::list(tx, &run))
            .unwrap()
            .into_iter()
            .map(|row| row.r#type)
            .collect::<Vec<_>>()
    };
    let count = |env: &TestEnv, kind: &str| types(env).iter().filter(|t| *t == kind).count();

    let started = lifecycle::worker_start(&env.engine, input(task, run.clone())).unwrap();
    assert_eq!(count(&env, "dispatch.dispatched"), 1);
    lifecycle::stop(&env.engine, &started.dispatch_id).unwrap();
    lifecycle::stop(&env.engine, &started.dispatch_id).unwrap();
    lifecycle::abandon(&env.engine, &started.dispatch_id).unwrap();
    assert_eq!(count(&env, "dispatch.failed"), 1, "{:?}", types(&env));
    lifecycle::retain(&env.engine, &started.dispatch_id, "keep").unwrap();
    lifecycle::retain(&env.engine, &started.dispatch_id, "keep").unwrap();
    lifecycle::release(&env.engine, &started.dispatch_id).unwrap();
    lifecycle::release(&env.engine, &started.dispatch_id).unwrap();
    // A repeated retain with the same reason marks nothing new.
    assert_eq!(count(&env, "worker.user_owned"), 1, "{:?}", types(&env));

    let failing = new_task(&env, &run, "fails");
    env.herdr.fail_agent_start("no agent");
    lifecycle::worker_start(&env.engine, input(failing, run.clone())).unwrap();
    assert_eq!(count(&env, "dispatch.start_failed"), 1);
    assert_eq!(count(&env, "dispatch.dispatched"), 1);

    target_agent(&env);
    let dispatched = new_task(&env, &run, "dispatched");
    lifecycle::dispatch(&env.engine, &dispatched, "target", true, false, false).unwrap();
    assert_eq!(count(&env, "dispatch.dispatched"), 2, "{:?}", types(&env));
}

/// Global Constraint: `worker-list` for 100 workers in under 100 ms, now
/// that every row is projected (observations, ladder) on one read.
#[test]
#[cfg_attr(debug_assertions, ignore = "timing test: run with --release")]
fn worker_list_100_workers_under_100ms() {
    let (env, run, _) = seeded();
    let now = env.engine.clock.now();
    env.engine
        .store
        .tx(|tx| {
            for n in 0..100 {
                let task = new_task_in(tx, &env, &run, n)?;
                let id = DispatchId::new();
                dispatches::insert(
                    tx,
                    &dispatches::DispatchRow {
                        id: id.clone(),
                        run_id: run.clone(),
                        task_id: task,
                        status: "dispatched".into(),
                        retry_of_dispatch_id: None,
                        failure_count: 0,
                        last_failure: None,
                        termination_reason: None,
                        termination_target: None,
                        depth: 1,
                        assignee_handle: Some(format!("w-{n}")),
                        assignee_terminal_id: Some(format!("term-{n}")),
                        assignee_pid: Some(1),
                        agent_kind: Some("codex".into()),
                        launch_requested: None,
                        launch_effective: None,
                        dispatched_at: Some(now),
                        last_heartbeat_at: None,
                        last_phase: None,
                        completed_at: None,
                        created_at: now,
                    },
                )?;
                let mut worker = worker_template(&id, now);
                worker.pane_id = Some(format!("pane-{n}"));
                workers::insert(tx, &worker)?;
                for _ in 0..3 {
                    pod::engine::observe::record_herdr_state(
                        tx,
                        now,
                        &id,
                        pod::herdr::AgentStatus::Working,
                        &[],
                        Some("present"),
                    )?;
                }
            }
            Ok(())
        })
        .unwrap();
    let started = std::time::Instant::now();
    let list = lifecycle::list(&env.engine, Some(&run), None, 100, None).unwrap();
    let elapsed = started.elapsed();
    assert_eq!(list.workers.len(), 100);
    assert!(
        elapsed < std::time::Duration::from_millis(100),
        "worker-list took {elapsed:?}"
    );
}

fn new_task_in(
    tx: &pod::store::Tx<'_>,
    env: &TestEnv,
    run: &RunId,
    n: usize,
) -> pod::error::Result<TaskId> {
    tasks::create(
        tx,
        env.engine.clock.now(),
        run,
        &format!("task {n}"),
        None,
        None,
        &[],
        None,
        "term-coord",
    )
    .map(|task| task.id)
}

fn worker_template(id: &DispatchId, now: chrono::DateTime<chrono::Utc>) -> workers::WorkerRow {
    workers::WorkerRow {
        dispatch_id: id.clone(),
        state: "ready".into(),
        stage: None,
        placement: None,
        worktree_path: None,
        workspace_id: None,
        tab_id: None,
        pane_id: None,
        terminal_id: None,
        owns_pane: true,
        user_owned: false,
        user_owned_reason: None,
        setup_state: None,
        setup_pane_id: None,
        preamble_input: None,
        effects: None,
        residual_resources: None,
        terminal_state: "active".into(),
        retained_reason: None,
        session_id: None,
        transcript_path: None,
        transcript_proof: None,
        last_error: None,
        created_at: now,
        updated_at: now,
    }
}

/// A just-created tab's shell can still run a startup command; herdr refuses
/// `agent_start` there ("not an available shell"). The start waits for the
/// prompt, retries that refusal, and records the kind it launched.
#[test]
fn worker_start_waits_for_the_new_shell_and_records_the_configured_kind() {
    let (env, run, task) = seeded();
    env.herdr.script_shell_busy("*", 3);
    env.herdr
        .fail_agent_start_once("pane pane_1 is not an available shell");
    env.herdr.report_agent_kind("unknown");
    let mut start = input(task, run);
    start.timeout_ms = Some(2_000);
    let receipt = lifecycle::worker_start(&env.engine, start).unwrap();
    assert_eq!(receipt.state, "ready", "{receipt:?}");
    assert_eq!(
        env.herdr
            .calls()
            .iter()
            .filter(|call| call.starts_with("agent_start:"))
            .count(),
        2
    );
    let dispatch = env
        .engine
        .store
        .read(|tx| dispatches::get(tx, &receipt.dispatch_id))
        .unwrap()
        .unwrap();
    assert_eq!(dispatch.agent_kind.as_deref(), Some("codex"));
}

#[test]
fn worker_start_fails_at_pane_when_the_shell_never_reaches_its_prompt() {
    let (env, run, task) = seeded();
    env.herdr.script_shell_busy("*", u32::MAX);
    let receipt = lifecycle::worker_start(&env.engine, input(task, run)).unwrap();
    assert_eq!(receipt.state, "failed");
    assert_eq!(receipt.failed_stage.as_deref(), Some("pane"));
    assert!(
        receipt
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("did not reach its prompt")),
        "{receipt:?}"
    );
    assert!(
        !env.herdr
            .calls()
            .iter()
            .any(|call| call.starts_with("agent_start:"))
    );
}

/// Spec 7.4: stop closes only the exact supervised pane, and never a pane
/// pod did not create (`dispatch --to` injects into someone else's pane).
#[test]
fn stop_leaves_a_dispatch_to_pane_pod_did_not_create() {
    let (env, _run, task) = seeded();
    target_agent(&env);
    let receipt = lifecycle::dispatch(&env.engine, &task, "target", true, false, false).unwrap();
    let dispatch = receipt.dispatch.unwrap().id;
    let stopped = lifecycle::stop(&env.engine, &dispatch).unwrap();
    assert_eq!(
        stopped.dispatch.termination_reason.as_deref(),
        Some("canceled")
    );
    assert!(stopped.closed_panes.is_empty());
    assert_eq!(stopped.left_alone, ["pane_not_created_by_pod"]);
    assert!(
        !env.herdr
            .calls()
            .iter()
            .any(|call| call.starts_with("pane_close:") || call.starts_with("tab_close:"))
    );
    assert!(env.herdr.pane_get("target-pane").unwrap().is_some());
}

/// Spec 7.4: release closes the worker's tab only while the worker's pane is
/// all it holds; with a pane a person split in, only pod's pane closes.
#[test]
fn release_closes_only_the_worker_pane_when_a_person_split_into_its_tab() {
    let (env, run, task) = seeded();
    let start = lifecycle::worker_start(&env.engine, input(task, run)).unwrap();
    settle(&env, &start.dispatch_id);
    let worker = worker_row(&env, &start.dispatch_id);
    let (tab, pane) = (worker.tab_id.unwrap(), worker.pane_id.unwrap());
    env.herdr.split_into_tab(&tab);
    let released = lifecycle::release(&env.engine, &start.dispatch_id).unwrap();
    assert_eq!(released.state, "released");
    assert!(released.closed_tabs.is_empty());
    assert_eq!(released.closed_panes, std::slice::from_ref(&pane));
    let calls = env.herdr.calls();
    assert!(calls.contains(&format!("pane_close:{pane}")));
    assert!(!calls.iter().any(|call| call.starts_with("tab_close:")));
}

/// Spec 12: every commit writes each worker workspace's sidebar status
/// (`<task> · <phase>`), clears it once its workers have ended, and treats a
/// herdr refusal as a warning (spec 19 fallback), never a failed mutation.
#[test]
fn commits_write_the_worker_workspace_sidebar_status_and_survive_a_refusal() {
    let (env, run, task) = seeded();
    let start = lifecycle::worker_start(&env.engine, input(task.clone(), run.clone())).unwrap();
    let workspace = worker_row(&env, &start.dispatch_id).workspace_id.unwrap();
    let statuses = |env: &TestEnv| {
        let prefix = format!("workspace_report_metadata:{workspace}:");
        env.herdr
            .calls()
            .iter()
            .filter_map(|call| call.strip_prefix(&prefix).map(str::to_owned))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        statuses(&env).last().map(String::as_str),
        Some(format!("{task} · working").as_str())
    );
    env.engine
        .store
        .tx(|tx| dispatches::update_last_phase(tx, &start.dispatch_id, Some("implementing")))
        .unwrap();
    env.engine.after_commit(&run, &[]).unwrap();
    assert_eq!(
        statuses(&env).last().map(String::as_str),
        Some(format!("{task} · implementing").as_str())
    );
    settle(&env, &start.dispatch_id);
    env.engine.after_commit(&run, &[]).unwrap();
    assert_eq!(statuses(&env).last().map(String::as_str), Some("<clear>"));

    env.herdr
        .refuse_metadata("metadata source pod is not allowed");
    let next = new_task(&env, &run, "after refusal");
    let started = lifecycle::worker_start(&env.engine, input(next, run)).unwrap();
    assert_eq!(started.state, "ready");
}

#[test]
fn worker_start_refuses_above_the_active_caps_with_a_literal_limit() {
    let (mut env, run, task) = seeded();
    env.engine.config.limits.max_active_workers = Some(1);
    env.engine
        .config
        .limits
        .max_active_per_kind
        .insert("claude".into(), 0);
    let first = lifecycle::worker_start(&env.engine, input(task, run.clone())).unwrap();
    assert_eq!(first.state, "ready");
    let second_task = new_task(&env, &run, "second");
    let error =
        lifecycle::worker_start(&env.engine, input(second_task.clone(), run.clone())).unwrap_err();
    assert_eq!(error.code, ErrorCode::LimitReached);
    assert_eq!(error.data["limit"], "max_active_workers");
    assert_eq!(
        (error.data["current"].as_u64(), error.data["max"].as_u64()),
        (Some(1), Some(1))
    );
    // No Dispatch was created by the refused start.
    let attempts = env
        .engine
        .store
        .read(|tx| dispatches::list_by_task(tx, &second_task))
        .unwrap();
    assert!(attempts.is_empty());
    env.engine.config.limits.max_active_workers = None;
    let mut claude = input(second_task, run);
    claude.agent = Some("claude".into());
    let error = lifecycle::worker_start(&env.engine, claude).unwrap_err();
    assert_eq!(error.data["limit"], "max_active_per_kind:claude");
}

fn claude_usage(
    used_percent: f64,
    recorded_at: chrono::DateTime<chrono::Utc>,
) -> pod::usage::ClaudeUsage {
    pod::usage::ClaudeUsage {
        recorded_at,
        session_id: None,
        model: None,
        context_used_percent: None,
        cost_usd: None,
        five_hour: Some(pod::usage::Window {
            label: "5h".into(),
            used_percent,
            resets_at: None,
        }),
        seven_day: None,
    }
}

#[test]
fn worker_start_pauses_above_a_fresh_usage_window_and_ignores_a_stale_one() {
    let (mut env, run, task) = seeded();
    env.engine.config.limits.pause_above_percent = Some(95.0);
    let usage = pod::usage::UsageStore::new(env.engine.paths.state_dir.join("usage"));
    let now = env.engine.clock.now();
    usage.save_claude(&claude_usage(97.0, now)).unwrap();
    let mut claude = input(task, run.clone());
    claude.agent = Some("claude".into());
    let error = lifecycle::worker_start(&env.engine, claude.clone()).unwrap_err();
    assert_eq!(error.code, ErrorCode::LimitReached);
    assert_eq!(error.data["limit"], "pause_above_percent");
    assert_eq!(error.data["window"], "5h");
    assert_eq!(error.data["usedPercent"], 97.0);
    // A Codex start is judged by the Codex record only, and there is none.
    let other = new_task(&env, &run, "codex work");
    let codex = lifecycle::worker_start(&env.engine, input(other, run.clone())).unwrap();
    assert_eq!(codex.state, "ready");
    // Usage older than ten minutes proves nothing.
    // `save_claude` keeps the newest record, so clear the fresh one first.
    std::fs::remove_file(env.engine.paths.state_dir.join("usage/claude.json")).unwrap();
    let stale = now - chrono::Duration::minutes(11);
    usage.save_claude(&claude_usage(97.0, stale)).unwrap();
    let started = lifecycle::worker_start(&env.engine, claude).unwrap();
    assert_eq!(started.state, "ready");
}

#[test]
fn a_claude_worker_shows_the_cost_its_status_line_reported() {
    let (env, run, task) = seeded();
    let started = lifecycle::worker_start(&env.engine, input(task, run.clone())).unwrap();
    let dispatch = started.dispatch_id;
    let payload = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/claude-statusline.json"
    ))
    .unwrap();
    let usage = pod::usage::claude_from_statusline(&payload, env.engine.clock.now()).unwrap();
    let store = pod::usage::UsageStore::new(env.engine.paths.state_dir.join("usage"));
    store.save_session(&usage).unwrap();
    // No matched session yet: the cost is unknown, not zero.
    assert_eq!(
        lifecycle::show(&env.engine, &dispatch).unwrap().cost_usd,
        None
    );
    env.engine
        .store
        .tx(|tx| workers::update_session_id(tx, &dispatch, usage.session_id.as_deref()))
        .unwrap();
    assert_eq!(
        lifecycle::show(&env.engine, &dispatch).unwrap().cost_usd,
        Some(3.1046)
    );
    let list = lifecycle::list(&env.engine, Some(&run), None, 100, None).unwrap();
    assert_eq!(list.workers[0].cost_usd, Some(3.1046));
}

/// A Dispatch whose session reported `cost`, in the usage store the engine reads.
fn spend(env: &TestEnv, dispatch: &DispatchId, cost: f64) {
    let payload = format!(r#"{{"session_id":"sess-spend","cost":{{"total_cost_usd":{cost}}}}}"#);
    let record =
        pod::usage::claude_session_from_statusline(&payload, env.engine.clock.now()).unwrap();
    pod::usage::UsageStore::new(env.engine.paths.state_dir.join("usage"))
        .save_session(&record)
        .unwrap();
    env.engine
        .store
        .tx(|tx| workers::update_session_id(tx, dispatch, Some("sess-spend")))
        .unwrap();
}

fn set_budget(env: &TestEnv, run: &RunId, budget: f64) {
    env.engine
        .store
        .tx(|tx| {
            tx.0.execute(
                "UPDATE runs SET budget_usd = ?1 WHERE id = ?2",
                rusqlite::params![budget, run.as_str()],
            )?;
            Ok(())
        })
        .unwrap();
}

#[test]
fn worker_start_refuses_a_run_whose_budget_is_spent_and_warns_near_it() {
    let (env, run, task) = seeded();
    let first = lifecycle::worker_start(&env.engine, input(task, run.clone())).unwrap();
    spend(&env, &first.dispatch_id, 3.1046);
    assert!(first.warnings.is_empty());

    set_budget(&env, &run, 1.0);
    let second = new_task(&env, &run, "second");
    let error =
        lifecycle::worker_start(&env.engine, input(second.clone(), run.clone())).unwrap_err();
    assert_eq!(error.code, ErrorCode::BudgetExceeded);
    assert_eq!(error.data["spentUsd"], 3.1046);
    assert_eq!(error.data["budgetUsd"], 1.0);
    let attempts = env
        .engine
        .store
        .read(|tx| dispatches::list_by_task(tx, &second))
        .unwrap();
    assert!(attempts.is_empty());

    // Spent is 3.1046: a budget of 3.5 is past 80 percent, 10.0 is not.
    set_budget(&env, &run, 3.5);
    let warned = lifecycle::worker_start(&env.engine, input(second, run.clone())).unwrap();
    assert_eq!(warned.state, "ready");
    assert_eq!(warned.warnings.len(), 1, "{:?}", warned.warnings);
    assert!(
        warned.warnings[0].contains("$3.10"),
        "{:?}",
        warned.warnings
    );
    set_budget(&env, &run, 10.0);
    let third = new_task(&env, &run, "third");
    let quiet = lifecycle::worker_start(&env.engine, input(third, run)).unwrap();
    assert!(quiet.warnings.is_empty());
}

#[test]
fn a_budget_with_no_matched_session_costs_nothing() {
    let (env, run, task) = seeded();
    set_budget(&env, &run, 0.01);
    let started = lifecycle::worker_start(&env.engine, input(task, run)).unwrap();
    assert_eq!(started.state, "ready");
    assert!(started.warnings.is_empty());
}

/// Review Focus 5: with `pause_above_percent` set, a missing or unreadable
/// usage record proves nothing, so the start proceeds with no error.
#[test]
fn a_missing_or_unreadable_usage_record_never_blocks_a_start() {
    let (mut env, run, task) = seeded();
    env.engine.config.limits.pause_above_percent = Some(0.0);
    let mut claude = input(task, run.clone());
    claude.agent = Some("claude".into());
    let started = lifecycle::worker_start(&env.engine, claude).unwrap();
    assert_eq!(started.state, "ready");
    assert!(started.warnings.is_empty(), "{:?}", started.warnings);
    let usage = env.engine.paths.state_dir.join("usage");
    std::fs::create_dir_all(&usage).unwrap();
    std::fs::write(usage.join("claude.json"), "{not json").unwrap();
    std::fs::write(usage.join("codex.json"), "{not json").unwrap();
    let mut again = input(new_task(&env, &run, "after a bad record"), run.clone());
    again.agent = Some("claude".into());
    assert_eq!(
        lifecycle::worker_start(&env.engine, again).unwrap().state,
        "ready"
    );
    let codex = input(new_task(&env, &run, "codex too"), run);
    assert_eq!(
        lifecycle::worker_start(&env.engine, codex).unwrap().state,
        "ready"
    );
}

/// Phase B review item 4: limits never lock a person out. With every cap
/// hit, reusing a pod pane (`--terminal`) still starts; with the Run's
/// budget spent, release, check, and gate-resolve still work.
#[test]
fn caps_and_a_spent_budget_never_block_reuse_release_check_or_a_gate() {
    use pod::engine::{deliveries, gates};

    let (mut env, run, task) = seeded();
    let first = lifecycle::worker_start(&env.engine, input(task, run.clone())).unwrap();
    let busy = lifecycle::worker_start(
        &env.engine,
        input(new_task(&env, &run, "keeps the cap full"), run.clone()),
    )
    .unwrap();
    settle(&env, &first.dispatch_id);
    env.engine.config.limits.max_active_workers = Some(1);
    env.engine
        .config
        .limits
        .max_active_per_kind
        .insert("codex".into(), 0);
    let blocked = lifecycle::worker_start(
        &env.engine,
        input(new_task(&env, &run, "refused"), run.clone()),
    )
    .unwrap_err();
    assert_eq!(blocked.code, ErrorCode::LimitReached);
    let mut reuse = input(new_task(&env, &run, "reuse"), run.clone());
    reuse.agent = None;
    reuse.terminal = Some(first.handle.clone());
    let reused = lifecycle::worker_start(&env.engine, reuse).unwrap();
    assert_eq!(reused.state, "ready");

    env.engine.config.limits = Default::default();
    spend(&env, &busy.dispatch_id, 3.0);
    set_budget(&env, &run, 1.0);
    let refused = lifecycle::worker_start(
        &env.engine,
        input(new_task(&env, &run, "over budget"), run.clone()),
    )
    .unwrap_err();
    assert_eq!(refused.code, ErrorCode::BudgetExceeded);
    let gate = env
        .engine
        .store
        .tx(|tx| {
            gates::create(
                tx,
                env.engine.clock.now(),
                &run,
                "user",
                None,
                "Ship or wait?",
                &["ship".to_owned(), "wait".to_owned()],
                Some("evidence"),
                Some("ship"),
                false,
            )
        })
        .unwrap();
    let (resolved, _) = env
        .engine
        .store
        .tx(|tx| gates::resolve(tx, env.engine.clock.now(), &gate.id, "ship"))
        .unwrap();
    assert_eq!(resolved.status, "resolved");
    deliveries::check_consume(&env.engine, &format!("run:{run}")).unwrap();
    settle(&env, &busy.dispatch_id);
    assert_eq!(
        lifecycle::release(&env.engine, &busy.dispatch_id)
            .unwrap()
            .state,
        "released"
    );
}

/// Spec B1.1 and B1.3 for `dispatch`: a cap or a spent budget refuses it
/// before a Dispatch row exists; a dry run still shows the preamble.
#[test]
fn dispatch_refuses_at_a_cap_or_a_spent_budget_before_any_dispatch() {
    let (mut env, run, task) = seeded();
    target_agent(&env);
    env.engine
        .config
        .limits
        .max_active_per_kind
        .insert("codex".into(), 0);
    let error = lifecycle::dispatch(&env.engine, &task, "target", true, false, false).unwrap_err();
    assert_eq!(error.code, ErrorCode::LimitReached);
    assert_eq!(error.data["limit"], "max_active_per_kind:codex");
    let dry = lifecycle::dispatch(&env.engine, &task, "target", false, true, true).unwrap();
    assert!(dry.dry_run && dry.preamble.is_some());

    env.engine.config.limits = Default::default();
    let other = lifecycle::worker_start(
        &env.engine,
        input(new_task(&env, &run, "spends"), run.clone()),
    )
    .unwrap();
    spend(&env, &other.dispatch_id, 2.0);
    set_budget(&env, &run, 2.0);
    let error = lifecycle::dispatch(&env.engine, &task, "target", true, false, false).unwrap_err();
    assert_eq!(error.code, ErrorCode::BudgetExceeded);
    let attempts = env
        .engine
        .store
        .read(|tx| dispatches::list_by_task(tx, &task))
        .unwrap();
    assert!(attempts.is_empty());
}

/// Spec 12.4: a reused pane carries its Claude session to the next
/// Dispatch, and `spentUsd` counts that session once.
#[test]
fn a_session_shared_by_two_dispatches_is_spent_once() {
    let (env, run, task) = seeded();
    let first = lifecycle::worker_start(&env.engine, input(task, run.clone())).unwrap();
    let second = lifecycle::worker_start(
        &env.engine,
        input(new_task(&env, &run, "same session"), run.clone()),
    )
    .unwrap();
    spend(&env, &first.dispatch_id, 3.1046);
    spend(&env, &second.dispatch_id, 3.1046);
    let usage = pod::usage::UsageStore::new(env.engine.paths.state_dir.join("usage"));
    let spent = env
        .engine
        .store
        .read(|tx| runs::spent_usd(tx, &usage, &run))
        .unwrap();
    assert_eq!(spent, 3.1046);
}
