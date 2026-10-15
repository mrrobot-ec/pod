use std::path::PathBuf;

use assert_cmd::Command;
use serde_json::Value;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn pod(home: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("pod").unwrap();
    cmd.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("POD_STATE_DIR", home.join("state"))
        .env("POD_CODEX_BIN", fixture("fake-codex.sh"));
    cmd
}

#[test]
fn usage_record_stores_the_claude_payload_silently() {
    let tmp = tempfile::tempdir().unwrap();
    let payload = std::fs::read_to_string(fixture("claude-statusline.json")).unwrap();
    let out = pod(tmp.path())
        .args(["usage", "--record"])
        .write_stdin(payload)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(out.is_empty(), "record prints nothing: {out:?}");
    let line = pod(tmp.path())
        .args(["usage"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let line = String::from_utf8(line).unwrap();
    assert!(line.starts_with("claude 5h 12% · 7d 30% · $3.10"), "{line}");
    assert!(
        line.trim_end()
            .ends_with("| codex 5h 61% · wk 1% · 49.6k credits »"),
        "{line}"
    );
    let json = pod(tmp.path())
        .args(["usage", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: Value = serde_json::from_slice(&json).unwrap();
    assert_eq!(value["claude"]["five_hour"]["used_percent"], 12.0);
    assert_eq!(value["codex"]["plan"], "pro");
    assert_eq!(value["line"].as_str().unwrap(), line.trim_end());
    pod(tmp.path())
        .args(["usage", "--toggle"])
        .assert()
        .success();
    let collapsed = pod(tmp.path())
        .args(["usage"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(String::from_utf8(collapsed).unwrap().trim_end(), "«");
    pod(tmp.path())
        .args(["usage", "--toggle"])
        .assert()
        .success();
    let back = pod(tmp.path())
        .args(["usage"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(String::from_utf8(back).unwrap().trim_end(), line.trim_end());
}

#[test]
fn usage_without_data_still_exits_zero_with_a_line_for_the_bar() {
    let tmp = tempfile::tempdir().unwrap();
    let out = pod(tmp.path())
        .env("POD_CODEX_BIN", fixture("no-such-codex"))
        .args(["usage"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        String::from_utf8(out).unwrap().trim_end(),
        "claude: no data (pod usage --install) | codex: no data »"
    );
}

#[test]
fn usage_install_wires_both_files_under_home() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".claude")).unwrap();
    std::fs::write(tmp.path().join(".claude/settings.json"), "{}").unwrap();
    let herdr_config = tmp.path().join("herdr.toml");
    let out = pod(tmp.path())
        .env("HERDR_CONFIG_PATH", &herdr_config)
        .args(["usage", "--install", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(value["claude_statusline"]["changed"], true);
    assert_eq!(value["herdr_bar"]["changed"], true);
    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(tmp.path().join(".claude/settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(settings["statusLine"]["command"], "pod usage --record");
    assert!(
        std::fs::read_to_string(&herdr_config)
            .unwrap()
            .contains("pod usage")
    );
}

#[test]
fn usage_tick_prints_the_line_and_runs_at_most_every_thirty_seconds() {
    let tmp = tempfile::tempdir().unwrap();
    let tick = |dir: &std::path::Path| -> Value {
        let out = pod(dir)
            .args(["usage", "--tick", "--json"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        serde_json::from_slice(&out).unwrap()
    };
    let first = tick(tmp.path());
    assert!(first["line"].as_str().unwrap().contains("codex"));
    // CI has no herdr: the tick then reports why it did not run.
    if first["tick"]["ran"] == true {
        let again = tick(tmp.path());
        assert_eq!(
            again["tick"]["ran"], false,
            "a second tick inside 30 s is skipped"
        );
    } else {
        assert_eq!(first["tick"]["reason"], "herdr unreachable");
    }
    // Plain output stays the bar line only.
    let plain = pod(tmp.path())
        .args(["usage", "--tick"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(String::from_utf8(plain).unwrap().lines().count(), 1);
}

#[test]
fn usage_record_also_stores_the_payload_by_session_id() {
    let tmp = tempfile::tempdir().unwrap();
    let payload = std::fs::read_to_string(fixture("claude-statusline.json")).unwrap();
    pod(tmp.path())
        .args(["usage", "--record"])
        .write_stdin(payload)
        .assert()
        .success();
    let path = tmp
        .path()
        .join("state/default/usage/sessions/7988217c-5f46-4d1e-8b3f-b8a10647ac7e.json");
    let record: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(record["cost_usd"], 3.1046);
}

/// Spec A2.2 with a herdr: the bar ticks, and a second invocation inside
/// 30 seconds returns without a pass.
#[test]
fn usage_tick_runs_once_per_thirty_seconds_with_a_herdr() {
    let tmp = tempfile::tempdir().unwrap();
    let tick = || -> Value {
        let out = pod(tmp.path())
            .env("HERDR_SOCKET_PATH", "/tmp/herdr/usage-tick.sock")
            .env("POD_FAKE_HERDR", "1")
            .args(["usage", "--tick", "--json"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        serde_json::from_slice(&out).unwrap()
    };
    let first = tick();
    assert_eq!(first["tick"]["ran"], true, "{first}");
    assert_eq!(first["tick"]["attempts"], 0);
    let again = tick();
    assert_eq!(again["tick"]["ran"], false, "{again}");
    assert_eq!(again["tick"]["reason"], "ticked less than 30 s ago");
}
