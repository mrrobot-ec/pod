//! T5: transcript readers. Samples live in tests/transcript/samples; each
//! test builds a throwaway home directory so nothing reads the real one.

use std::fs;
use std::path::{Path, PathBuf};

use pod::error::ErrorCode;
use pod::transcript::claude::ClaudeReader;
use pod::transcript::codex::CodexReader;
use pod::transcript::cursor::CursorReader;
use pod::transcript::grok::GrokReader;
use pod::transcript::{Reader, reader_for};

const CLAUDE_ID: &str = "7563e397-f26f-4e62-9345-99e7637e8deb";
const CLAUDE_CWD: &str = "/private/tmp/claude-501/disc";
const CODEX_ID: &str = "01a13269-f92a-7643-adc6-d672d6d6a4ff";
const CODEX_FILE: &str = "rollout-2026-10-12T19-19-28-01a13269-f92a-7643-adc6-d672d6d6a4ff.jsonl";
const GROK_ID: &str = "01a0d4d6-9f6d-7f42-8114-585e3a1d5294";
const CURSOR_ID: &str = "00000000-0000-4000-8000-00000000c0de";

fn sample(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/transcript/samples")
        .join(rel)
}

fn put(home: &Path, rel: &str, from: &Path) -> PathBuf {
    let dest = home.join(rel);
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    fs::copy(from, &dest).unwrap();
    dest
}

fn claude_home() -> (tempfile::TempDir, PathBuf) {
    let home = tempfile::tempdir().unwrap();
    let file = put(
        home.path(),
        &format!(".claude/projects/-private-tmp-claude-501-disc/{CLAUDE_ID}.jsonl"),
        &sample(&format!("claude/{CLAUDE_ID}.jsonl")),
    );
    (home, file)
}

fn codex_home() -> (tempfile::TempDir, PathBuf) {
    let home = tempfile::tempdir().unwrap();
    let file = put(
        home.path(),
        &format!(".codex/sessions/2026/10/12/{CODEX_FILE}"),
        &sample(&format!("codex/{CODEX_FILE}")),
    );
    (home, file)
}

#[test]
fn claude_locate_by_session_id_in_cwd_slug() {
    let (home, file) = claude_home();
    let reader = ClaudeReader::with_home(home.path());
    let found = reader
        .locate(CLAUDE_ID, None, Path::new(CLAUDE_CWD))
        .unwrap();
    assert_eq!(found, Some(file));
    // Another cwd has another slug, so the same id is not found there.
    let other = reader
        .locate(CLAUDE_ID, None, Path::new("/somewhere/else"))
        .unwrap();
    assert_eq!(other, None);
    // Ids that could escape the directory are never joined into a path.
    assert_eq!(
        reader
            .locate("../../etc/passwd", None, Path::new(CLAUDE_CWD))
            .unwrap(),
        None
    );
}

#[test]
fn claude_read_rows_roles_kinds_tools() {
    let (home, file) = claude_home();
    let reader = ClaudeReader::with_home(home.path());
    let page = reader.read(&file, None, 100).unwrap();
    let shape: Vec<(&str, &str, Option<&str>)> = page
        .rows
        .iter()
        .map(|r| (r.role.as_str(), r.kind.as_str(), r.tool.as_deref()))
        .collect();
    assert_eq!(
        shape,
        vec![
            ("user", "message", None),
            ("assistant", "tool_call", Some("Bash")),
            ("tool", "tool_result", None),
            ("assistant", "message", None),
        ]
    );
    assert!(page.rows[0].text.contains("echo hello-pod"));
    assert!(page.rows[1].text.contains("echo hello-pod"));
    assert_eq!(page.rows[2].text, "hello-pod");
    assert_eq!(page.rows[3].text, "done");
    assert!(page.rows[3].at.is_some());
    assert!(page.content_complete);
    assert!(!page.clipping);
}

#[test]
fn codex_locate_rollout_by_id() {
    let (home, file) = codex_home();
    let reader = CodexReader::with_home(home.path());
    let found = reader
        .locate(CODEX_ID, None, Path::new("/ignored"))
        .unwrap();
    assert_eq!(found, Some(file));
    assert_eq!(
        reader
            .locate("nope-id", None, Path::new("/ignored"))
            .unwrap(),
        None
    );
    // A partial id must not match by substring, including a tail of the
    // UUID, whose groups are joined by the same `-` as the timestamp.
    for partial in ["01a13269", "d672d6d6a4ff", "adc6-d672d6d6a4ff"] {
        assert_eq!(
            reader.locate(partial, None, Path::new("/ignored")).unwrap(),
            None,
            "{partial} must not match"
        );
    }
}

#[test]
fn codex_read_rows() {
    let (home, file) = codex_home();
    let reader = CodexReader::with_home(home.path());
    let page = reader.read(&file, None, 100).unwrap();
    let shape: Vec<(&str, &str, Option<&str>)> = page
        .rows
        .iter()
        .map(|r| (r.role.as_str(), r.kind.as_str(), r.tool.as_deref()))
        .collect();
    assert_eq!(
        shape,
        vec![
            ("user", "message", None),
            ("assistant", "message", None),
            ("assistant", "tool_call", Some("exec")),
            ("tool", "tool_result", None),
            ("assistant", "message", None),
        ]
    );
    assert!(page.rows[2].text.contains("echo hello-pod"));
    assert!(page.rows[3].text.contains("hello-pod"));
    assert_eq!(page.rows[4].text, "done");
    assert!(page.content_complete);
}

#[test]
fn read_pages_with_cursor_and_limit() {
    let (home, file) = claude_home();
    let reader = ClaudeReader::with_home(home.path());
    let first = reader.read(&file, None, 2).unwrap();
    assert_eq!(first.rows.len(), 2);
    assert!(!first.content_complete);
    let cursor = first.cursor.clone().expect("cursor after a page");
    let (hash, offset) = cursor.split_once(':').expect("<hash>:<offset>");
    assert!(!hash.is_empty());
    offset.parse::<u64>().expect("numeric offset");

    let second = reader.read(&file, Some(&cursor), 2).unwrap();
    assert_eq!(second.rows.len(), 2);
    assert!(second.content_complete);

    let all = reader.read(&file, None, 100).unwrap();
    let mut joined = first.rows.clone();
    joined.extend(second.rows.clone());
    assert_eq!(joined, all.rows);

    // Polling at the end returns nothing new and a cursor that still works.
    let tail = second.cursor.clone().unwrap();
    let again = reader.read(&file, Some(&tail), 10).unwrap();
    assert!(again.rows.is_empty());
    assert!(again.content_complete);
    assert_eq!(again.cursor, Some(tail));
}

#[test]
fn cursor_from_other_source_is_source_changed() {
    let (home, file) = claude_home();
    let other = put(
        home.path(),
        ".claude/projects/-private-tmp-claude-501-other/other.jsonl",
        &file,
    );
    let reader = ClaudeReader::with_home(home.path());
    let cursor = reader.read(&file, None, 2).unwrap().cursor.unwrap();
    let err = reader.read(&other, Some(&cursor), 2).unwrap_err();
    assert_eq!(err.code, ErrorCode::SourceChanged);
    // A truncated source (offset beyond the end) is also a changed source.
    let short = format!("{}:{}", cursor.split_once(':').unwrap().0, 9_999_999);
    let err = reader.read(&file, Some(&short), 2).unwrap_err();
    assert_eq!(err.code, ErrorCode::SourceChanged);
    // A file rewritten in place so the offset lands mid-line is changed too.
    let offset: u64 = cursor.split_once(':').unwrap().1.parse().unwrap();
    let hash = cursor.split_once(':').unwrap().0;
    fs::write(&file, "x".repeat(offset as usize + 10) + "\n").unwrap();
    let err = reader.read(&file, Some(&cursor), 2).unwrap_err();
    assert_eq!(err.code, ErrorCode::SourceChanged);
    // A malformed cursor is a usage error, not a changed source.
    let err = reader
        .read(&file, Some(&format!("{hash}:x")), 2)
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Usage);
}

#[test]
fn unterminated_last_line_is_read_once_complete() {
    let home = tempfile::tempdir().unwrap();
    let file = home.path().join("t.jsonl");
    let line = |text: &str| {
        serde_json::json!({"role":"assistant","message":{"content":[{"type":"text","text":text}]}})
            .to_string()
    };
    // Cursor ends its transcripts without a final newline.
    fs::write(&file, format!("{}\n{}", line("one"), line("two"))).unwrap();
    let reader = CursorReader::with_home(home.path());
    let page = reader.read(&file, None, 10).unwrap();
    let texts: Vec<&str> = page.rows.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(texts, ["one", "two"]);
    assert!(page.content_complete);
    // The writer appends the next record after a newline; nothing repeats.
    let mut f = fs::OpenOptions::new().append(true).open(&file).unwrap();
    std::io::Write::write_all(&mut f, format!("\n{}", line("three")).as_bytes()).unwrap();
    let next = reader.read(&file, page.cursor.as_deref(), 10).unwrap();
    let texts: Vec<&str> = next.rows.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(texts, ["three"]);
    // A half-written tail is left for the next read and is not a warning.
    std::io::Write::write_all(&mut f, b"\n{\"role\":\"assi").unwrap();
    let partial = reader.read(&file, next.cursor.as_deref(), 10).unwrap();
    assert!(partial.rows.is_empty());
    assert!(!partial.content_complete);
    assert!(partial.warnings.is_empty());
    // Once finished, a last line that yields no rows is still consumed.
    std::io::Write::write_all(&mut f, b"stant\"}\n{\"type\":\"turn_ended\"}").unwrap();
    let done = reader.read(&file, partial.cursor.as_deref(), 10).unwrap();
    assert!(done.rows.is_empty());
    assert!(done.content_complete);
    assert!(done.warnings.is_empty());
}

#[test]
fn large_tool_output_is_trimmed_with_clipping() {
    let home = tempfile::tempdir().unwrap();
    let big = "x".repeat(10_000);
    let lines = [
        serde_json::json!({"type":"user","timestamp":"2026-10-12T00:00:00Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":big}]}}),
        serde_json::json!({"type":"assistant","timestamp":"2026-10-12T00:00:01Z","message":{"role":"assistant","content":[{"type":"text","text":"y".repeat(10_000)}]}}),
    ];
    let file = home.path().join("big.jsonl");
    let body: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    fs::write(&file, body.join("\n") + "\n").unwrap();
    let page = ClaudeReader::with_home(home.path())
        .read(&file, None, 10)
        .unwrap();
    assert!(page.clipping);
    assert_eq!(page.rows[0].kind, "tool_result");
    assert_eq!(page.rows[0].text.chars().count(), 4000);
    // Only tool output is trimmed; assistant prose is returned whole.
    assert_eq!(page.rows[1].text.chars().count(), 10_000);
}

#[test]
fn unproven_session_refuses_read() {
    let (chome, _) = claude_home();
    let (xhome, _) = codex_home();
    // No id and no path: never pick a file by recency, for every kind.
    let readers: Vec<Box<dyn Reader>> = vec![
        Box::new(ClaudeReader::with_home(chome.path())),
        Box::new(CodexReader::with_home(xhome.path())),
        Box::new(GrokReader::with_home(chome.path())),
        Box::new(CursorReader::with_home(chome.path())),
    ];
    for reader in readers {
        assert_eq!(
            reader.locate("", None, Path::new(CLAUDE_CWD)).unwrap(),
            None,
            "{} must not guess",
            reader.kind()
        );
    }
    // The registry knows exactly the four kinds.
    for kind in ["claude", "codex", "grok", "cursor"] {
        assert_eq!(reader_for(kind).unwrap().kind(), kind);
    }
    assert!(reader_for("pi").is_none());
}

#[test]
fn grok_and_cursor_tests_skip_when_no_sample() {
    let grok = sample(&format!("grok/{GROK_ID}/chat_history.jsonl"));
    if grok.exists() {
        let home = tempfile::tempdir().unwrap();
        let file = put(
            home.path(),
            &format!(".grok/sessions/%2Fprivate%2Ftmp%2Fclaude-501/{GROK_ID}/chat_history.jsonl"),
            &grok,
        );
        let reader = GrokReader::with_home(home.path());
        assert_eq!(
            reader.locate(GROK_ID, None, Path::new("/x")).unwrap(),
            Some(file.clone())
        );
        let page = reader.read(&file, None, 100).unwrap();
        let shape: Vec<(&str, &str, Option<&str>)> = page
            .rows
            .iter()
            .map(|r| (r.role.as_str(), r.kind.as_str(), r.tool.as_deref()))
            .collect();
        assert_eq!(
            shape,
            vec![
                ("user", "message", None),
                ("assistant", "message", None),
                ("assistant", "tool_call", Some("read_file")),
                ("tool", "tool_result", None),
                ("assistant", "message", None),
            ]
        );
        assert_eq!(page.rows[0].text, "Reply with exactly: OK");
    } else {
        eprintln!("skipped: no grok sample");
    }
    let cursor = sample(&format!("cursor/{CURSOR_ID}/{CURSOR_ID}.jsonl"));
    if cursor.exists() {
        let home = tempfile::tempdir().unwrap();
        let file = put(
            home.path(),
            &format!(".cursor/projects/some-slug/agent-transcripts/{CURSOR_ID}/{CURSOR_ID}.jsonl"),
            &cursor,
        );
        let reader = CursorReader::with_home(home.path());
        assert_eq!(
            reader.locate(CURSOR_ID, None, Path::new("/x")).unwrap(),
            Some(file.clone())
        );
        let page = reader.read(&file, None, 100).unwrap();
        let texts: Vec<&str> = page.rows.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "Reply with exactly: OK",
                "OK",
                "Now say done.",
                "Checking first.",
                r#"{"path":"/tmp/example.txt"}"#,
                "done"
            ]
        );
        assert_eq!(page.rows[0].role, "user");
        assert_eq!(page.rows[1].role, "assistant");
        assert_eq!(page.rows[4].kind, "tool_call");
        assert_eq!(page.rows[4].tool.as_deref(), Some("Read"));
        assert!(page.content_complete);
    } else {
        eprintln!("skipped: no cursor sample");
    }
}
