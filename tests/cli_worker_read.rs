mod support;

use std::path::PathBuf;
use std::str::FromStr;

use pod::cli::commands::workers::read::{Source, worker_read};
use pod::engine::lifecycle::archive_final_screen;
use pod::herdr::{AgentInfo, AgentStatus};
use pod::ids::DispatchId;
use pod::store::rows::worker_archives;
use pod::{ErrorCode, PodError};
use support::TestEnv;

const RUN: &str = "run_000000000001";
const TASK: &str = "task_000000000001";
const DISPATCH: &str = "ctx_000000000001";
const PANE: &str = "pane-worker";

fn sample() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/transcript/samples/claude/7563e397-f26f-4e62-9345-99e7637e8deb.jsonl")
}

fn dispatch_id() -> DispatchId {
    DispatchId::from_str(DISPATCH).unwrap()
}

/// A dispatch and worker row. `worker_set` is extra `SET` clauses for the worker.
fn seed(env: &TestEnv, kind: &str, worker_set: &str) {
    let now = "2026-10-12T12:00:00.000000Z";
    env.engine
        .store
        .tx(|tx| {
            tx.0.execute(
                "INSERT INTO runs (id, objective, coordinator_terminal_id, coordinator_handle, consumer_generation, home_cwd, depth, created_at, updated_at) VALUES (?1, 'test', 'term-coordinator', 'coordinator', 1, '/tmp', 0, ?2, ?2)",
                (RUN, now),
            )?;
            tx.0.execute(
                "INSERT INTO tasks (id, run_id, spec, status, deps, created_by_terminal_id, created_at) VALUES (?1, ?2, 'spec', 'dispatched', '[]', 'term-coordinator', ?3)",
                (TASK, RUN, now),
            )?;
            tx.0.execute(
                "INSERT INTO dispatches (id, run_id, task_id, status, failure_count, depth, assignee_handle, assignee_terminal_id, agent_kind, created_at) VALUES (?1, ?2, ?3, 'dispatched', 0, 1, 'worker-a', 'term-worker', ?4, ?5)",
                (DISPATCH, RUN, TASK, kind, now),
            )?;
            tx.0.execute(
                "INSERT INTO workers (dispatch_id, state, terminal_id, terminal_state, owns_pane, created_at, updated_at) VALUES (?1, 'ready', 'term-worker', 'active', 1, ?2, ?2)",
                (DISPATCH, now),
            )?;
            if !worker_set.is_empty() {
                tx.0.execute(
                    &format!("UPDATE workers SET {worker_set} WHERE dispatch_id = ?1"),
                    [DISPATCH],
                )?;
            }
            Ok(())
        })
        .unwrap();
}

fn herdr_agent(env: &TestEnv, kind: &str, session_id: Option<&str>, session_path: Option<PathBuf>) {
    env.herdr.add_pane(PANE, "term-worker", "ws_fake");
    env.herdr.set_agent(
        "worker-a",
        AgentInfo {
            name: "worker-a".into(),
            kind: kind.into(),
            pane_id: PANE.into(),
            pid: 1,
            status: AgentStatus::Idle,
            session_id: session_id.map(Into::into),
            session_path,
            needs_input: vec![],
            observed_at: env.clock.0.lock().unwrap().to_owned(),
        },
    );
}

fn read(
    env: &TestEnv,
    source: Source,
    limit: u32,
    cursor: Option<&str>,
) -> pod::error::Result<pod::cli::commands::workers::read::ReadReceipt> {
    worker_read(&env.engine, &dispatch_id(), source, limit, cursor)
}

fn code(error: PodError) -> ErrorCode {
    error.code
}

#[test]
fn auto_prefers_proven_transcript_then_pod_assigned_session_then_screen_with_fallback_reason() {
    // 1. herdr proves the session: transcript, no fallback.
    let env = TestEnv::new();
    seed(&env, "claude", "pane_id = 'pane-worker'");
    herdr_agent(&env, "claude", Some("s1"), Some(sample()));
    env.herdr.set_screen(PANE, "screen line\n");
    let receipt = read(&env, Source::Auto, 50, None).unwrap();
    assert_eq!(receipt.source, "transcript");
    assert_eq!(receipt.fallback_reason, None);
    assert!(!receipt.rows.is_empty());
    assert!(receipt.rows.iter().all(|row| row.role != "screen"));

    // 2. no herdr proof, but pod assigned the session id at launch.
    let env = TestEnv::new();
    let path = sample().display().to_string();
    seed(
        &env,
        "claude",
        &format!(
            "pane_id = 'pane-worker', session_id = 'abc', transcript_proof = 'pod_assigned', transcript_path = '{path}'"
        ),
    );
    herdr_agent(&env, "claude", None, None);
    env.herdr.set_screen(PANE, "screen line\n");
    let receipt = read(&env, Source::Auto, 50, None).unwrap();
    assert_eq!(receipt.source, "transcript");
    assert_eq!(receipt.fallback_reason, None);

    // 3. a session id alone is not proof: screen, session_unproven.
    let env = TestEnv::new();
    seed(
        &env,
        "claude",
        "pane_id = 'pane-worker', session_id = 'abc'",
    );
    herdr_agent(&env, "claude", None, None);
    env.herdr.set_screen(PANE, "screen line\n\nsecond\n");
    let receipt = read(&env, Source::Auto, 50, None).unwrap();
    assert_eq!(receipt.source, "terminal");
    assert_eq!(receipt.fallback_reason.as_deref(), Some("session_unproven"));
    let texts: Vec<_> = receipt.rows.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(texts, ["screen line", "second"]);
    assert!(receipt.content_complete);

    // 4. proven session but no reader for the kind: reader_unavailable.
    let env = TestEnv::new();
    seed(&env, "opencode", "pane_id = 'pane-worker'");
    herdr_agent(&env, "opencode", Some("s1"), Some(sample()));
    env.herdr.set_screen(PANE, "screen line\n");
    let receipt = read(&env, Source::Auto, 50, None).unwrap();
    assert_eq!(receipt.source, "terminal");
    assert_eq!(
        receipt.fallback_reason.as_deref(),
        Some("reader_unavailable")
    );

    // 5. unproven and no pane and no archive: no_terminal, nothing to read.
    let env = TestEnv::new();
    seed(&env, "claude", "");
    let receipt = read(&env, Source::Auto, 50, None).unwrap();
    assert_eq!(receipt.fallback_reason.as_deref(), Some("no_terminal"));
    assert!(receipt.rows.is_empty());

    // an explicit transcript source never falls back.
    let env = TestEnv::new();
    seed(&env, "claude", "pane_id = 'pane-worker'");
    herdr_agent(&env, "claude", None, None);
    let error = read(&env, Source::Transcript, 50, None).unwrap_err();
    assert_eq!(code(error), ErrorCode::Usage);
}

#[test]
fn source_terminal_refused_when_no_pane() {
    let env = TestEnv::new();
    seed(&env, "claude", "");
    let error = read(&env, Source::Terminal, 50, None).unwrap_err();
    assert_eq!(code(error), ErrorCode::Usage);

    // a pane id that herdr no longer knows is the same refusal.
    let env = TestEnv::new();
    seed(&env, "claude", "pane_id = 'pane-gone'");
    let error = read(&env, Source::Terminal, 50, None).unwrap_err();
    assert_eq!(code(error), ErrorCode::Usage);
}

#[test]
fn cursor_pinned_to_source_source_changed() {
    let env = TestEnv::new();
    seed(&env, "claude", "pane_id = 'pane-worker'");
    herdr_agent(&env, "claude", Some("s1"), Some(sample()));
    env.herdr.set_screen(PANE, "a\nb\nc\n");

    // a terminal cursor, then the transcript becomes the auto source.
    let screen = read(&env, Source::Terminal, 2, None).unwrap();
    assert_eq!(screen.rows.len(), 2);
    assert!(!screen.content_complete);
    let screen_cursor = screen.cursor.clone().unwrap();
    let error = read(&env, Source::Auto, 2, Some(&screen_cursor)).unwrap_err();
    assert_eq!(code(error), ErrorCode::SourceChanged);

    // forcing the original source continues from the cursor.
    let rest = read(&env, Source::Terminal, 2, Some(&screen_cursor)).unwrap();
    let texts: Vec<_> = rest.rows.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(texts, ["c"]);
    assert!(rest.content_complete);

    // a transcript cursor is refused by the terminal source, and the reverse.
    let transcript = read(&env, Source::Transcript, 1, None).unwrap();
    let transcript_cursor = transcript.cursor.clone().unwrap();
    let error = read(&env, Source::Terminal, 2, Some(&transcript_cursor)).unwrap_err();
    assert_eq!(code(error), ErrorCode::SourceChanged);
    let next = read(&env, Source::Auto, 1, Some(&transcript_cursor)).unwrap();
    assert_eq!(next.source, "transcript");

    // a terminal cursor for another pane is a different source.
    env.herdr.add_pane("pane-other", "term-other", "ws_fake");
    env.engine
        .store
        .tx(|tx| {
            tx.0.execute("UPDATE workers SET pane_id = 'pane-other'", [])?;
            Ok(())
        })
        .unwrap();
    let error = read(&env, Source::Terminal, 2, Some(&screen_cursor)).unwrap_err();
    assert_eq!(code(error), ErrorCode::SourceChanged);
}

#[test]
fn archive_written_once_for_every_kind_and_readable_after_release() {
    for kind in ["claude", "codex", "grok", "cursor", "opencode"] {
        let env = TestEnv::new();
        seed(&env, kind, "pane_id = 'pane-worker'");
        herdr_agent(&env, kind, None, None);
        env.herdr
            .set_screen(PANE, "final line one\nfinal line two\n");
        archive_final_screen(&env.engine, &dispatch_id()).unwrap();

        // the screen changes and the pane closes; a second call keeps the first archive.
        env.herdr.set_screen(PANE, "something else\n");
        archive_final_screen(&env.engine, &dispatch_id()).unwrap();
        let rows = env
            .engine
            .store
            .read(|tx| worker_archives::list(tx, 10))
            .unwrap();
        assert_eq!(rows.len(), 1, "{kind}");
        assert_eq!(rows[0].kind, "final_screen");
        assert_eq!(rows[0].content, "final line one\nfinal line two\n");
        assert_eq!(
            env.herdr
                .calls()
                .iter()
                .filter(|c| c.starts_with("pane_read"))
                .count(),
            1,
            "{kind}: the screen is read once"
        );

        // release closed the pane: terminal reads come from the archive.
        env.herdr.set_screen(PANE, "");
        env.engine
            .store
            .tx(|tx| {
                tx.0.execute(
                    "UPDATE workers SET pane_id = NULL, terminal_state = 'released'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        let receipt = read(&env, Source::Terminal, 50, None).unwrap();
        assert_eq!(receipt.source, "terminal");
        let texts: Vec<_> = receipt.rows.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(texts, ["final line one", "final line two"], "{kind}");
        let auto = read(&env, Source::Auto, 50, None).unwrap();
        assert_eq!(auto.fallback_reason.as_deref(), Some("session_unproven"));
        assert_eq!(auto.rows.len(), 2);
    }
}

#[test]
fn archive_without_a_pane_writes_nothing() {
    let env = TestEnv::new();
    seed(&env, "claude", "");
    archive_final_screen(&env.engine, &dispatch_id()).unwrap();
    let rows = env
        .engine
        .store
        .read(|tx| worker_archives::list(tx, 10))
        .unwrap();
    assert!(rows.is_empty());
}

#[test]
fn cli_receipt_shape() {
    let env = TestEnv::new();
    seed(&env, "claude", "pane_id = 'pane-worker'");
    env.herdr.add_pane(PANE, "term-worker", "ws_fake");
    let mut command = env.pod(None);
    command.env("POD_FAKE_PANES", format!("{PANE}=term-worker"));
    // the fake herdr of a subprocess has no screen text; the shape still holds.
    let output = command
        .args([
            "worker-read",
            "--dispatch",
            DISPATCH,
            "--limit",
            "3",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(value["ok"], true);
    assert_eq!(value["source"], "terminal");
    assert_eq!(value["fallbackReason"], "session_unproven");
    assert!(value["rows"].is_array());
    assert!(value.get("cursor").is_some());
    assert_eq!(value["contentComplete"], true);
    assert_eq!(value["clipping"], serde_json::json!({"rowsTrimmed": 0}));
    assert_eq!(value["warnings"], serde_json::json!([]));

    // an unknown dispatch is a usage error; a bad source is too.
    env.pod(None)
        .args(["worker-read", "--dispatch", "ctx_00000000ffff", "--json"])
        .assert()
        .code(2);
    env.pod(None)
        .args(["worker-read", "--dispatch", DISPATCH, "--source", "bogus"])
        .assert()
        .code(2);
}

fn codex_sample() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
        "tests/transcript/samples/codex/rollout-2026-10-12T19-19-28-01a13269-f92a-7643-adc6-d672d6d6a4ff.jsonl",
    )
}

#[test]
fn released_worker_reads_its_archive_not_the_pane_left_open() {
    let env = TestEnv::new();
    seed(&env, "claude", "pane_id = 'pane-worker'");
    herdr_agent(&env, "claude", None, None);
    env.herdr.set_screen(PANE, "final line\n");
    archive_final_screen(&env.engine, &dispatch_id()).unwrap();

    // release left the pane open (user-taken-over); someone else now works in it.
    env.herdr.set_screen(PANE, "the user's own work\n");
    herdr_agent(&env, "claude", Some("someone-else"), Some(sample()));
    let receipt = read(&env, Source::Terminal, 50, None).unwrap();
    let texts: Vec<_> = receipt.rows.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(texts, ["final line"]);
    let auto = read(&env, Source::Auto, 50, None).unwrap();
    assert_eq!(auto.source, "terminal");
    assert_eq!(auto.fallback_reason.as_deref(), Some("session_unproven"));
}

#[test]
fn release_keeps_herdr_session_proof_so_the_transcript_stays_readable() {
    let env = TestEnv::new();
    seed(&env, "codex", "pane_id = 'pane-worker'");
    herdr_agent(&env, "codex", Some("01a13269"), Some(codex_sample()));
    env.herdr.set_screen(PANE, "codex screen\n");
    archive_final_screen(&env.engine, &dispatch_id()).unwrap();
    let worker = env
        .engine
        .store
        .read(|tx| pod::store::rows::workers::get(tx, &dispatch_id()))
        .unwrap()
        .unwrap();
    assert_eq!(worker.transcript_proof.as_deref(), Some("herdr"));
    assert_eq!(worker.session_id.as_deref(), Some("01a13269"));

    // the pane is gone and herdr no longer reports the session.
    env.herdr.set_agent(
        "worker-a",
        AgentInfo {
            name: "worker-a".into(),
            kind: "codex".into(),
            pane_id: "pane-elsewhere".into(),
            pid: 1,
            status: AgentStatus::Idle,
            session_id: None,
            session_path: None,
            needs_input: vec![],
            observed_at: env.clock.0.lock().unwrap().to_owned(),
        },
    );
    let receipt = read(&env, Source::Auto, 50, None).unwrap();
    assert_eq!(receipt.source, "transcript");
    assert_eq!(receipt.fallback_reason, None);
    assert!(!receipt.rows.is_empty());

    // a stored pod-assigned proof is never replaced.
    let env = TestEnv::new();
    seed(
        &env,
        "claude",
        "pane_id = 'pane-worker', session_id = 'mine', transcript_proof = 'pod_assigned'",
    );
    herdr_agent(&env, "claude", Some("theirs"), Some(sample()));
    archive_final_screen(&env.engine, &dispatch_id()).unwrap();
    let worker = env
        .engine
        .store
        .read(|tx| pod::store::rows::workers::get(tx, &dispatch_id()))
        .unwrap()
        .unwrap();
    assert_eq!(worker.session_id.as_deref(), Some("mine"));
    assert_eq!(worker.transcript_proof.as_deref(), Some("pod_assigned"));
}

#[test]
fn herdr_agent_kind_is_used_when_the_dispatch_has_none() {
    let env = TestEnv::new();
    seed(&env, "claude", "pane_id = 'pane-worker'");
    env.engine
        .store
        .tx(|tx| {
            tx.0.execute("UPDATE dispatches SET agent_kind = NULL", [])?;
            Ok(())
        })
        .unwrap();
    herdr_agent(&env, "claude", Some("s1"), Some(sample()));
    let receipt = read(&env, Source::Auto, 50, None).unwrap();
    assert_eq!(receipt.source, "transcript");
}

#[test]
fn bad_limits_and_cursors_are_refused() {
    let env = TestEnv::new();
    seed(&env, "claude", "pane_id = 'pane-worker'");
    herdr_agent(&env, "claude", None, None);
    env.herdr.set_screen(PANE, "a\nb\n");
    assert_eq!(
        code(read(&env, Source::Auto, 0, None).unwrap_err()),
        ErrorCode::Usage
    );
    assert_eq!(
        code(read(&env, Source::Terminal, 5, Some("screen-nonsense")).unwrap_err()),
        ErrorCode::Usage
    );
    let page = read(&env, Source::Terminal, 1, None).unwrap();
    let cursor = page.cursor.unwrap();
    let (head, _) = cursor.split_once(':').unwrap();
    assert_eq!(
        code(read(&env, Source::Terminal, 5, Some(&format!("{head}:x"))).unwrap_err()),
        ErrorCode::Usage
    );
    // the screen shrank under the cursor.
    assert_eq!(
        code(read(&env, Source::Terminal, 5, Some(&format!("{head}:9"))).unwrap_err()),
        ErrorCode::SourceChanged
    );

    // the pane is gone with no archive: a cursor has no source left.
    let env = TestEnv::new();
    seed(&env, "claude", "pane_id = 'pane-gone'");
    assert_eq!(
        code(read(&env, Source::Auto, 5, Some(&cursor)).unwrap_err()),
        ErrorCode::SourceChanged
    );
    let empty = read(&env, Source::Auto, 5, None).unwrap();
    assert_eq!(empty.fallback_reason.as_deref(), Some("no_terminal"));
}
