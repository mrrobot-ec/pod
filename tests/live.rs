//! The live suite: real `claude` and `codex` workers on an isolated herdr
//! server. Run it through `scripts/live.sh`; every test is ignored unless
//! `POD_LIVE=1`.
#[path = "live/mod.rs"]
mod live;

fn enabled() -> bool {
    std::env::var("POD_LIVE").is_ok()
}

/// The brief the file-writing cases give a worker.
fn file_spec(title: &str, file: &str) -> String {
    format!(
        "{title}\nTarget: this folder. Change: create a file named {file} containing the word done, then report worker_done with --outcome succeeded. Constraints: no other changes. Ownership: only that file. Acceptance: the file exists."
    )
}

/// Waits for the one `worker_done` a case expects, acknowledges it, and
/// returns the message.
fn worker_done(session: &live::Session, outcome: &str) -> serde_json::Value {
    let done = session.check_wait("worker_done", None);
    let message = done["messages"][0].clone();
    assert_eq!(message["payload"]["outcome"], outcome, "{done}");
    session.pod(&["check", "--ack", done["delivery"]["id"].as_str().unwrap()]);
    message
}

fn worktree_of(receipt: &serde_json::Value) -> std::path::PathBuf {
    let path = receipt["effects"]
        .as_array()
        .and_then(|effects| effects.iter().find(|effect| effect["kind"] == "worktree"))
        .and_then(|effect| effect["id"].as_str())
        .expect("the receipt names the worktree");
    std::fs::canonicalize(path).unwrap_or_else(|_| path.into())
}

#[test]
#[ignore = "needs POD_LIVE=1, herdr, claude and codex on this machine"]
fn live_current_folder_claude_and_codex() {
    if !enabled() {
        return;
    }
    let session = live::Session::new("pod-live");
    for kind in live::kinds() {
        let spec = file_spec("Case 1 current folder", &format!("live-{kind}.txt"));
        session.case(
            "case 1",
            kind,
            &format!("pod worker-start --agent {kind} --worktree current --yolo"),
            || {
                let worker = session.start_worker(kind, "current", &spec);
                assert_eq!(worker.receipt["state"], "ready", "{}", worker.receipt);
                session.soft(
                    worker.receipt["launch"]["inject"] == "prompt",
                    format!(
                        "brief went in by {}, not agent.prompt (herdr: {})",
                        worker.receipt["launch"]["inject"],
                        worker.receipt["effects"]
                            .as_array()
                            .and_then(|effects| effects
                                .iter()
                                .find(|effect| effect["kind"] == "prompt_error"))
                            .map_or("no prompt error", |effect| effect["id"]
                                .as_str()
                                .unwrap_or("?"))
                    ),
                );
                worker_done(&session, "succeeded");
                let released = session.pod(&["worker-release", "--dispatch", &worker.dispatch]);
                assert_eq!(released["closedTabs"].as_array().map(Vec::len), Some(1));
                worker.receipt
            },
        );
    }
    session.finish();
}

#[test]
#[ignore = "needs POD_LIVE=1, herdr, claude and codex on this machine"]
fn live_worktree_trust_and_reuse() {
    if !enabled() {
        return;
    }
    let session = live::Session::new("pod-live");
    for kind in live::kinds() {
        // Case 2: a new child worktree; trust is written, worker_done arrives.
        let file = format!("live-{kind}-wt.txt");
        let spec = file_spec("Case 2 child worktree", &file);
        session.case(
            "case 2",
            kind,
            &format!("pod worker-start --agent {kind} --worktree new-child --name live-{kind} --repo path:<project> --yolo"),
            || {
                let worker = session.start_worker(kind, "new-child", &spec);
                assert_eq!(worker.receipt["state"], "ready", "{}", worker.receipt);
                let worktree = worktree_of(&worker.receipt);
                assert_eq!(worker.receipt["setup"]["source"], "new_worktree");
                let trusted = format!("folder_trusted:{}", worktree.display());
                assert!(
                    worker.receipt["worker"]["effects"]
                        .as_array()
                        .is_some_and(|effects| effects.iter().any(|e| e == trusted.as_str())),
                    "pod did not report trusting the worktree: {}",
                    worker.receipt["worker"]["effects"]
                );
                match kind {
                    "claude" => assert!(session.claude_trusted(&worktree), "{}", worktree.display()),
                    _ => assert!(
                        session.codex_trusted(&worktree) && session.codex_trusted(&session.project_root()),
                        "codex config lacks the worktree or its repository"
                    ),
                }
                let done = worker_done(&session, "succeeded");
                assert!(worktree.join(&file).exists(), "{file} missing in {}", worktree.display());
                session.pod(&["worker-release", "--dispatch", &worker.dispatch]);
                assert_eq!(done["payload"]["dispatch_id"], worker.dispatch.as_str());
                worker.receipt
            },
        );
        // Case 3: the same pane serves a second Task and its tab is relabelled.
        let first = file_spec("Case 3 first task", &format!("live-{kind}-one.txt"));
        session.case(
            "case 3",
            kind,
            &format!("pod worker-start --terminal <handle> --spec <second task> ({kind})"),
            || {
                let worker = session.start_worker(kind, "current", &first);
                assert_eq!(worker.receipt["state"], "ready", "{}", worker.receipt);
                worker_done(&session, "succeeded");
                let tab = worker.receipt["worker"]["tabId"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                let before = session.herdr_text(&["tab", "get", &tab]);
                assert!(before.contains("Case 3 first task"), "{before}");
                let second = file_spec("Case 3 second task", &format!("live-{kind}-two.txt"));
                let reused =
                    session.start_worker_with(kind, &["--terminal", &worker.handle], &second);
                assert_eq!(reused.receipt["state"], "ready", "{}", reused.receipt);
                assert_eq!(reused.receipt["setup"]["source"], "reused_pane");
                assert_eq!(reused.pane, worker.pane, "reuse must keep the pane");
                let after = session.herdr_text(&["tab", "get", &tab]);
                assert!(
                    after.contains("Case 3 second task"),
                    "tab not relabelled: {after}"
                );
                worker_done(&session, "succeeded");
                assert!(
                    session
                        .project
                        .join(format!("live-{kind}-two.txt"))
                        .exists()
                );
                session.pod(&["worker-release", "--dispatch", &reused.dispatch]);
                reused.receipt
            },
        );
    }
    session.finish();
}

fn busy_spec(title: &str) -> String {
    format!(
        "{title}\nTarget: this folder. Change: run exactly one shell command, `python3 -c 'import time; time.sleep(110)'`, in the foreground and wait for it to finish (do not shorten it or run it in the background), then report worker_done with --outcome succeeded. Constraints: no file changes. Ownership: none. Acceptance: the sleep ran to the end."
    )
}

fn idle_spec(title: &str) -> String {
    format!(
        "{title}\nTarget: this folder. Change: none yet. Reply with the single word ready, then stop and wait; run no command and report no worker_done until a coordinator message tells you to. When a line says you have orchestration messages, run the pod check command it names, then report worker_done with --outcome succeeded. Constraints: no file changes. Ownership: none. Acceptance: you waited, then reported."
    )
}

fn wait_for_activity(session: &live::Session, dispatch: &str, activity: &str) {
    session.wait_until(
        || session.worker_show(dispatch)["projection"]["activity"] == activity,
        120,
        &format!("worker activity {activity}"),
    );
}

#[test]
#[ignore = "needs POD_LIVE=1, herdr, claude and codex on this machine"]
fn live_nudges_interruption_gate_and_clock() {
    if !enabled() {
        return;
    }
    let session = live::Session::new("pod-live");
    for kind in live::kinds() {
        // Cases 4 and 7 share one worker that stays `working` for about two minutes.
        let busy = std::cell::RefCell::new(None);
        session.case(
            "case 4",
            kind,
            &format!("pod send --to dispatch:<id> (a {kind} worker mid-turn)"),
            || {
                let worker = session.start_worker(kind, "current", &busy_spec("Case 4 busy worker"));
                assert_eq!(worker.receipt["state"], "ready", "{}", worker.receipt);
                // Past the brief and into the long command, where the worker
                // cannot run its own `pod check` and consume the message.
                session.wait_until(
                    || session.process_running(r"time\.sleep\(110\)"),
                    120,
                    "the worker to start its long command",
                );
                wait_for_activity(&session, &worker.dispatch, "working");
                let sent = session.pod(&[
                    "send",
                    "--to",
                    &format!("dispatch:{}", worker.dispatch),
                    "--type",
                    "status",
                    "--subject",
                    "case 4 nudge",
                    "--body",
                    "sent while the worker is mid-turn",
                ]);
                let attempts = session.wake_attempts(&worker.dispatch);
                assert!(!attempts.is_empty(), "no wake attempt was recorded: {sent}");
                let (outcome, detail, _) = &attempts[0];
                assert!(
                    matches!(outcome.as_str(), "delivered" | "skipped" | "rendered_unconfirmed"),
                    "unexpected outcome {outcome}: {detail}"
                );
                assert!(outcome == "delivered" || !detail.is_empty(), "skipped without a reason");
                assert!(
                    outcome == "delivered" || session.unread_messages(&worker.dispatch) == 1,
                    "the message was lost"
                );
                let receipt = serde_json::json!({"attempts": attempts, "unread": session.unread_messages(&worker.dispatch)});
                *busy.borrow_mut() = Some(worker);
                receipt
            },
        );
        session.case_with(
            "case 7",
            kind,
            "pod check --wait --timeout-ms 75000 (the worker stays working)",
            false,
            || {
                let worker = busy.borrow_mut().take().expect("case 4 started no worker");
                let before = session.wake_attempts(&worker.dispatch);
                let status_before = session.worker_show(&worker.dispatch)["observation"]["herdrStatus"].clone();
                let started = chrono::Utc::now();
                let waited = session.check_wait_for("worker_done", None, 75_000);
                let after = session.wake_attempts(&worker.dispatch);
                let show = session.worker_show(&worker.dispatch);
                let status_after = show["observation"]["herdrStatus"].clone();
                assert_eq!(
                    (&status_before, &status_after),
                    (&serde_json::json!("working"), &serde_json::json!("working")),
                    "the worker changed status, so the tick did not meet a busy worker"
                );
                // At least one tick ran inside the 75 seconds. A tick that
                // finds the worker working is not an attempt (8.2), so a long
                // turn cannot exhaust the ladder. The wait's closing consume
                // is a mutating command, which still counts: at most one
                // attempt, stamped at the timeout, not at the minute's tick.
                let added = &after[before.len().min(after.len())..];
                assert!(after.starts_with(&before) && added.len() <= 1, "before {before:?}, after {after:?}");
                for (_, _, at) in added {
                    let at = chrono::DateTime::parse_from_rfc3339(at).expect("attempt timestamp");
                    assert!(
                        at.signed_duration_since(started).num_seconds() >= 70,
                        "a tick recorded an attempt against a working worker at {at} (wait from {started}: {waited})"
                    );
                }
                let categories = show["projection"]["attention"]["categories"].to_string();
                assert!(!categories.contains("unreachable"), "attention: {categories}");
                assert_eq!(session.unread_messages(&worker.dispatch), 1, "the message was lost");
                let receipt = serde_json::json!({"before": before, "after": after, "wokeOn": waited["wokeOn"], "timedOut": waited["timedOut"]});
                // Let the worker finish so the tab can be released.
                worker_done(&session, "succeeded");
                session.pod(&["worker-release", "--dispatch", &worker.dispatch]);
                receipt
            },
        );

        // Case 5: a person types into an idle worker's pane.
        session.case(
            "case 5",
            kind,
            &format!("herdr pane send-text <pane> 'typing here' (a {kind} worker), then pod send"),
            || {
                let worker = session.start_worker(kind, "current", &idle_spec("Case 5 idle worker"));
                assert_eq!(worker.receipt["state"], "ready", "{}", worker.receipt);
                wait_for_activity(&session, &worker.dispatch, "idle_at_prompt");
                std::thread::sleep(std::time::Duration::from_secs(5));
                session.herdr_text(&["pane", "send-text", &worker.pane, "typing here"]);
                session.pod(&[
                    "send",
                    "--to",
                    &format!("dispatch:{}", worker.dispatch),
                    "--type",
                    "status",
                    "--subject",
                    "case 5 nudge",
                    "--body",
                    "sent while a person is typing",
                ]);
                // The commit-time attempt cannot prove the pane quiet; the
                // waiting coordinator's tick, a minute on, does and sees the text.
                session.check_wait_for("worker_done", None, 80_000);
                let attempts = session.wake_attempts(&worker.dispatch);
                assert!(
                    attempts.iter().any(|(outcome, detail, _)| outcome == "skipped" && detail.contains("person is typing")),
                    "no attempt was skipped for a person typing: {attempts:?}"
                );
                let show = session.worker_show(&worker.dispatch);
                let categories = show["projection"]["attention"]["categories"].to_string();
                assert!(categories.contains("interruption"), "attention: {categories}");
                // The person deletes the text; the next tick delivers.
                session.herdr_text(&["pane", "send-keys", &worker.pane, "ctrl+u"]);
                let done = session.check_wait_for("worker_done", None, 150_000);
                let attempts = session.wake_attempts(&worker.dispatch);
                assert!(
                    attempts.iter().any(|(outcome, _, _)| outcome == "delivered"),
                    "the nudge was never delivered after the line was cleared: {attempts:?}, wait {done}"
                );
                session.pod(&["check", "--ack", done["delivery"]["id"].as_str().unwrap_or_default()]);
                session.pod(&["worker-release", "--dispatch", &worker.dispatch]);
                serde_json::json!({"attempts": attempts})
            },
        );
    }

    // Case 6 needs no agent: a user gate reaches the board and herdr.
    session.case(
        "case 6",
        "-",
        "pod gate-create --for user --question ... --options ... --evidence ... --recommendation ...",
        || {
            let created = session.pod(&[
                "gate-create",
                "--for",
                "user",
                "--question",
                "ship it?",
                "--options",
                "[\"yes\",\"no\"]",
                "--evidence",
                "live suite",
                "--recommendation",
                "yes",
            ]);
            let gate = live::string_at(&created, &["gate", "id"]);
            assert!(
                created["warnings"].as_array().is_none_or(Vec::is_empty),
                "gate-create warned: {created}"
            );
            let board = session.board().to_string();
            assert!(board.contains(&format!("gate:{gate}")), "no gate card for {gate} on the board");
            assert!(
                session.herdr_log().contains("notification.show"),
                "herdr was never asked to show a notification"
            );
            created
        },
    );
    session.finish();
}
