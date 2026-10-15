use std::path::PathBuf;

use chrono::{DateTime, Duration, Utc};
use pod::usage::{self, Snapshot, UsageStore};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn at(rfc3339: &str) -> DateTime<Utc> {
    rfc3339.parse::<DateTime<Utc>>().unwrap()
}

#[test]
fn claude_statusline_payload_becomes_a_usage_record() {
    let payload = std::fs::read_to_string(fixture("claude-statusline.json")).unwrap();
    let now = at("2026-10-13T20:00:00Z");
    let claude = usage::claude_from_statusline(&payload, now).unwrap();
    assert_eq!(
        claude.session_id.as_deref(),
        Some("7988217c-5f46-4d1e-8b3f-b8a10647ac7e")
    );
    assert_eq!(claude.model.as_deref(), Some("Opus 5.5"));
    assert_eq!(claude.context_used_percent, Some(42.3));
    assert_eq!(claude.cost_usd, Some(3.1046));
    let five = claude.five_hour.as_ref().unwrap();
    assert_eq!((five.label.as_str(), five.used_percent), ("5h", 12.0));
    assert_eq!(five.resets_at, Some(1791960000));
    assert_eq!(claude.seven_day.as_ref().unwrap().used_percent, 30.4);
    assert_eq!(claude.recorded_at, now);
}

#[test]
fn claude_payload_without_rate_limits_is_rejected_so_the_last_good_record_stays() {
    let now = at("2026-10-13T20:00:00Z");
    let error =
        usage::claude_from_statusline(r#"{"session_id":"x","model":{"display_name":"Opus"}}"#, now)
            .unwrap_err();
    assert!(error.message.contains("rate_limits"), "{error:?}");
}

#[test]
fn codex_rate_limits_reply_becomes_a_usage_record() {
    let reply: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(fixture("codex-rate-limits.json")).unwrap())
            .unwrap();
    let now = at("2026-10-13T20:00:00Z");
    let codex = usage::codex_from_rate_limits(&reply, now).unwrap();
    assert_eq!(codex.plan.as_deref(), Some("pro"));
    let weekly = codex.primary.as_ref().unwrap();
    assert_eq!((weekly.label.as_str(), weekly.used_percent), ("wk", 1.0));
    let five = codex.secondary.as_ref().unwrap();
    assert_eq!((five.label.as_str(), five.used_percent), ("5h", 61.0));
    assert_eq!(codex.credits, Some(49558.637202));
}

#[test]
fn codex_is_read_live_through_the_app_server() {
    let now = at("2026-10-13T20:00:00Z");
    let codex = usage::read_codex(fixture("fake-codex.sh").as_os_str(), now, 5_000).unwrap();
    assert_eq!(codex.secondary.as_ref().unwrap().used_percent, 61.0);
    let missing = usage::read_codex(fixture("no-such-codex").as_os_str(), now, 5_000).unwrap_err();
    assert!(missing.message.contains("codex"), "{missing:?}");
}

#[test]
fn bar_line_shows_both_accounts_marks_stale_data_and_says_what_is_missing() {
    let now = at("2026-10-13T20:00:00Z");
    let payload = std::fs::read_to_string(fixture("claude-statusline.json")).unwrap();
    let claude = usage::claude_from_statusline(&payload, now - Duration::minutes(12)).unwrap();
    let reply: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(fixture("codex-rate-limits.json")).unwrap())
            .unwrap();
    let codex = usage::codex_from_rate_limits(&reply, now).unwrap();
    let both = Snapshot {
        claude: Some(claude.clone()),
        codex: Some(codex),
    };
    assert_eq!(
        usage::bar_line(&both, now),
        "claude 5h 12% · 7d 30% · $3.10 (12m ago) | codex 5h 61% · wk 1% · 49.6k credits"
    );
    let fresh = Snapshot {
        claude: Some(usage::claude_from_statusline(&payload, now).unwrap()),
        codex: None,
    };
    assert_eq!(
        usage::bar_line(&fresh, now),
        "claude 5h 12% · 7d 30% · $3.10 | codex: no data"
    );
    let none = Snapshot {
        claude: None,
        codex: None,
    };
    assert_eq!(
        usage::bar_line(&none, now),
        "claude: no data (pod usage --install) | codex: no data"
    );
}

#[test]
fn collapsed_bar_shows_only_the_expander_and_expanded_bar_has_the_collapser() {
    let now = at("2026-10-13T20:00:00Z");
    let payload = std::fs::read_to_string(fixture("claude-statusline.json")).unwrap();
    let snapshot = Snapshot {
        claude: Some(usage::claude_from_statusline(&payload, now).unwrap()),
        codex: None,
    };
    assert_eq!(
        usage::bar_entry(&snapshot, now, false),
        "claude 5h 12% · 7d 30% · $3.10 | codex: no data »"
    );
    assert_eq!(usage::bar_entry(&snapshot, now, true), "«");

    let tmp = tempfile::tempdir().unwrap();
    let store = UsageStore::new(tmp.path().join("usage"));
    assert!(!store.collapsed().unwrap());
    assert!(store.toggle_collapsed().unwrap());
    assert!(store.collapsed().unwrap());
    assert!(!store.toggle_collapsed().unwrap());
    assert!(!store.collapsed().unwrap());
}

#[test]
fn usage_store_round_trips_and_keeps_the_newest_claude_rate_limits() {
    let tmp = tempfile::tempdir().unwrap();
    let store = UsageStore::new(tmp.path().join("usage"));
    assert!(store.load_claude().unwrap().is_none());
    let payload = std::fs::read_to_string(fixture("claude-statusline.json")).unwrap();
    let older = usage::claude_from_statusline(&payload, at("2026-10-13T19:00:00Z")).unwrap();
    let newer = usage::claude_from_statusline(&payload, at("2026-10-13T20:00:00Z")).unwrap();
    store.save_claude(&newer).unwrap();
    store.save_claude(&older).unwrap();
    assert_eq!(
        store.load_claude().unwrap().unwrap().recorded_at,
        newer.recorded_at
    );
    let reply: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(fixture("codex-rate-limits.json")).unwrap())
            .unwrap();
    let codex = usage::codex_from_rate_limits(&reply, at("2026-10-13T20:00:00Z")).unwrap();
    store.save_codex(&codex).unwrap();
    assert_eq!(store.load_codex().unwrap().unwrap().credits, codex.credits);
}

#[test]
fn install_wires_claude_statusline_and_herdr_bar_once_with_a_backup() {
    let tmp = tempfile::tempdir().unwrap();
    let settings = tmp.path().join("settings.json");
    std::fs::write(
        &settings,
        r#"{"model":"opus","statusLine":{"type":"command","command":"~/.orca/agent-hooks/claude-statusline.sh"}}"#,
    )
    .unwrap();
    let first = usage::install_claude_statusline(&settings, "pod usage --record").unwrap();
    assert!(first.changed);
    let backup = first
        .backup
        .clone()
        .expect("backup of the replaced settings");
    assert!(backup.exists() && backup.starts_with(tmp.path()));
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(written["model"], "opus");
    assert_eq!(written["statusLine"]["command"], "pod usage --record");
    assert!(usage::claude_statusline_installed(&settings));
    let second = usage::install_claude_statusline(&settings, "pod usage --record").unwrap();
    assert!(!second.changed && second.backup.is_none());

    let config = tmp.path().join("config.toml");
    std::fs::write(
        &config,
        "[theme]\nname = \"latte\"\n\n[ui]\nwindow_title = \"x\"\n",
    )
    .unwrap();
    assert!(!usage::herdr_bar_installed(&config));
    let first = usage::install_herdr_bar(&config, "pod usage --tick").unwrap();
    assert!(first.changed);
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(text.contains("name = \"latte\""), "{text}");
    assert!(!text.contains("tab_bar_position"), "{text}");
    assert!(text.contains("command = \"pod usage --tick\""), "{text}");
    assert!(text.contains("interval_seconds = 1"), "{text}");
    assert!(text.contains("[[keys.command]]"), "{text}");
    assert!(text.contains("key = \"prefix+u\""), "{text}");
    assert!(text.contains("command = \"pod usage --toggle\""), "{text}");
    assert!(usage::herdr_bar_installed(&config));
    let second = usage::install_herdr_bar(&config, "pod usage --tick").unwrap();
    assert!(!second.changed);
    assert_eq!(std::fs::read_to_string(&config).unwrap(), text);
    // An entry installed before 0.2 (`pod usage`) is upgraded in place, not
    // duplicated.
    let old = tmp.path().join("old.toml");
    std::fs::write(
        &old,
        "[ui]\ntab_bar_right = [{ type = \"command\", command = \"pod usage\", interval_seconds = 1, timeout_seconds = 5 }]\n",
    )
    .unwrap();
    assert!(
        usage::install_herdr_bar(&old, "pod usage --tick")
            .unwrap()
            .changed
    );
    let upgraded = std::fs::read_to_string(&old).unwrap();
    assert_eq!(
        upgraded.matches("command = \"pod usage --tick\"").count(),
        1,
        "{upgraded}"
    );
    assert!(!upgraded.contains("command = \"pod usage\""), "{upgraded}");
    assert!(
        upgraded.contains("command = \"pod usage --toggle\""),
        "{upgraded}"
    );
    // A missing config file is created rather than refused.
    let fresh = tmp.path().join("fresh.toml");
    assert!(
        usage::install_herdr_bar(&fresh, "pod usage --tick")
            .unwrap()
            .changed
    );
    assert!(usage::herdr_bar_installed(&fresh));
}

#[test]
fn a_session_record_round_trips_by_session_id() {
    let tmp = tempfile::tempdir().unwrap();
    let store = UsageStore::new(tmp.path().join("usage"));
    let payload = std::fs::read_to_string(fixture("claude-statusline.json")).unwrap();
    let claude = usage::claude_from_statusline(&payload, at("2026-10-13T20:00:00Z")).unwrap();
    store.save_session(&claude).unwrap();
    let id = "7988217c-5f46-4d1e-8b3f-b8a10647ac7e";
    assert_eq!(store.load_session(id), Some(claude.clone()));
    assert!(
        tmp.path()
            .join("usage/sessions")
            .join(format!("{id}.json"))
            .exists()
    );
    assert_eq!(store.load_session("another-session"), None);
    // A session id is a file name: anything that could leave the directory is refused.
    assert_eq!(store.load_session("../claude"), None);
    let mut hostile = claude;
    hostile.session_id = Some("../escape".into());
    store.save_session(&hostile).unwrap();
    assert!(!tmp.path().join("usage/escape.json").exists());
}

#[test]
fn a_payload_without_rate_limits_still_yields_a_session_record_for_its_cost() {
    let payload = r#"{"session_id":"abc","cost":{"total_cost_usd":1.5}}"#;
    let now = at("2026-10-13T20:00:00Z");
    let session = usage::claude_session_from_statusline(payload, now).unwrap();
    assert_eq!(session.cost_usd, Some(1.5));
    assert!(session.five_hour.is_none());
    assert!(usage::claude_session_from_statusline(r#"{"cost":{}}"#, now).is_err());
}
