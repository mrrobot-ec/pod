//! `pod log --follow`: a feed of events, one line each, until the timeout.

mod support;

use serde_json::Value;
use support::TestEnv;

#[test]
fn log_follow_prints_existing_events_then_new_ones_and_exits_on_timeout() {
    let env = TestEnv::new();
    let created = env
        .pod(Some("term-coord"))
        .args(["run-create", "--objective", "feed", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let created: Value = serde_json::from_slice(&created).unwrap();
    let run = created["run"]["id"].as_str().unwrap().to_owned();

    let started = std::time::Instant::now();
    let output = env
        .pod(Some("term-coord"))
        .args([
            "log",
            "--run",
            &run,
            "--follow",
            "--timeout-ms",
            "400",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    assert!(started.elapsed() >= std::time::Duration::from_millis(400));
    let lines: Vec<Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).expect("one JSON event per line"))
        .collect();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["type"], "run.created");

    // Plain text: one line per event with the event's columns.
    let text = env
        .pod(Some("term-coord"))
        .args(["log", "--run", &run, "--follow", "--timeout-ms", "50"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&text);
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(text.contains(" run.created term-coord - - "), "{text}");
}
