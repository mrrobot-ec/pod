use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use pod::config::{AgentAlias, Config, Defaults, Launch, Preset, launch_argv};
use pod::herdr::fake::FakeHerdr;
use pod::identity::Caller;
use pod::paths;
use pod::store::Store;
use predicates::prelude::*;
use serde_json::Value;
use tempfile::tempdir;

pub mod support;

fn env_from(values: BTreeMap<&str, String>) -> impl Fn(&str) -> Option<String> {
    move |name| values.get(name).cloned()
}

#[test]
fn paths_default_and_overrides() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().join("home");
    let mut values = BTreeMap::from([
        ("HOME", home.display().to_string()),
        ("HERDR_SOCKET_PATH", "/tmp/herdr/team-blue.sock".to_owned()),
    ]);

    let default = paths::resolve(&env_from(values.clone())).unwrap();
    assert_eq!(default.session, "team-blue");
    assert_eq!(default.state_dir, home.join(".local/state/pod/team-blue"));
    assert_eq!(default.config_path, home.join(".config/pod/config.toml"));
    assert_eq!(default.db_path, default.state_dir.join("pod.db"));
    assert_eq!(default.board_dir, default.state_dir.join("runs"));

    values.insert(
        "XDG_STATE_HOME",
        tmp.path().join("state").display().to_string(),
    );
    values.insert(
        "XDG_CONFIG_HOME",
        tmp.path().join("config").display().to_string(),
    );
    values.insert(
        "POD_STATE_DIR",
        tmp.path().join("pod-state").display().to_string(),
    );
    values.insert(
        "POD_CONFIG",
        tmp.path().join("pod.toml").display().to_string(),
    );
    let overridden = paths::resolve(&env_from(values)).unwrap();
    assert_eq!(overridden.state_dir, tmp.path().join("pod-state/team-blue"));
    assert_eq!(overridden.config_path, tmp.path().join("pod.toml"));
}

fn test_paths(root: &Path) -> pod::Paths {
    let values = BTreeMap::from([
        ("HOME", root.display().to_string()),
        ("POD_STATE_DIR", root.join("state").display().to_string()),
        ("POD_CONFIG", root.join("config.toml").display().to_string()),
        ("HERDR_SOCKET_PATH", "/tmp/herdr/t0.sock".to_owned()),
    ]);
    paths::resolve(&env_from(values)).unwrap()
}

#[test]
fn store_open_creates_0600_file_and_0700_dir_and_wal() {
    let tmp = tempdir().unwrap();
    let paths = test_paths(tmp.path());
    let store = Store::open(&paths).unwrap();

    assert_eq!(
        fs::metadata(&paths.db_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(&paths.state_dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let mode = store
        .read(|tx| {
            Ok(tx
                .0
                .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))?)
        })
        .unwrap();
    assert_eq!(mode.to_ascii_lowercase(), "wal");
}

#[test]
fn store_migrates_to_user_version_3_and_is_idempotent() {
    let tmp = tempdir().unwrap();
    let paths = test_paths(tmp.path());
    assert_eq!(Store::open(&paths).unwrap().user_version().unwrap(), 3);
    assert_eq!(Store::open(&paths).unwrap().user_version().unwrap(), 3);
}

#[test]
fn schema_rejects_invalid_enum() {
    let tmp = tempdir().unwrap();
    let store = Store::open(&test_paths(tmp.path())).unwrap();
    store
        .tx(|tx| {
            tx.0.execute(
                "INSERT INTO runs (id, objective, coordinator_terminal_id, coordinator_handle, consumer_generation, home_cwd, depth, created_at, updated_at) VALUES ('run_000000000001', 'x', 'term', 'term', 1, '/tmp', 0, 'now', 'now')",
                [],
            )?;
            let invalid = tx.0.execute(
                "INSERT INTO tasks (id, run_id, spec, status, deps, created_by_terminal_id, created_at) VALUES ('task_000000000001', 'run_000000000001', 'x', 'bogus', '[]', 'term', 'now')",
                [],
            );
            assert!(invalid.is_err());
            Ok(())
        })
        .unwrap();
}

#[test]
fn schema_one_active_dispatch_per_task_and_per_terminal() {
    let tmp = tempdir().unwrap();
    let store = Store::open(&test_paths(tmp.path())).unwrap();
    store
        .tx(|tx| {
            tx.0.execute_batch(
                "INSERT INTO runs (id, objective, coordinator_terminal_id, coordinator_handle, consumer_generation, home_cwd, depth, created_at, updated_at) VALUES ('run_000000000001', 'x', 'term', 'term', 1, '/tmp', 0, 'now', 'now');
                 INSERT INTO tasks (id, run_id, spec, status, deps, created_by_terminal_id, created_at) VALUES ('task_000000000001', 'run_000000000001', 'x', 'dispatched', '[]', 'term', 'now');
                 INSERT INTO tasks (id, run_id, spec, status, deps, created_by_terminal_id, created_at) VALUES ('task_000000000002', 'run_000000000001', 'y', 'dispatched', '[]', 'term', 'now');
                 INSERT INTO dispatches (id, run_id, task_id, status, failure_count, depth, assignee_handle, assignee_terminal_id, created_at) VALUES ('ctx_000000000001', 'run_000000000001', 'task_000000000001', 'dispatched', 0, 0, 'worker-a', 'terminal-a', 'now');",
            )?;
            assert!(tx.0.execute(
                "INSERT INTO dispatches (id, run_id, task_id, status, failure_count, depth, assignee_handle, assignee_terminal_id, created_at) VALUES ('ctx_000000000002', 'run_000000000001', 'task_000000000001', 'pending', 0, 0, 'worker-b', 'terminal-b', 'now')",
                [],
            ).is_err());
            assert!(tx.0.execute(
                "INSERT INTO dispatches (id, run_id, task_id, status, failure_count, depth, assignee_handle, assignee_terminal_id, created_at) VALUES ('ctx_000000000003', 'run_000000000001', 'task_000000000002', 'pending', 0, 0, 'worker-c', 'terminal-a', 'now')",
                [],
            ).is_err());
            Ok(())
        })
        .unwrap();
}

#[test]
fn config_resolve_agent_order_preset_alias_kind() {
    let mut config = Config::default();
    config.agents.insert(
        "codex".into(),
        AgentAlias {
            command: "/opt/codex-real".into(),
            launch: None,
        },
    );
    config.presets.insert(
        "reviewer".into(),
        Preset {
            agent: "codex".into(),
            model: Some("gpt-review".into()),
            effort: Some("high".into()),
            fast: true,
            yolo: false,
            args: vec!["--quiet".into()],
        },
    );

    let preset = config
        .resolve_agent("reviewer", Some("gpt-override"), None, false, false)
        .unwrap();
    assert_eq!(preset.kind, "codex");
    assert_eq!(preset.command, "/opt/codex-real");
    assert_eq!(preset.model.as_deref(), Some("gpt-override"));
    assert_eq!(preset.effort.as_deref(), Some("high"));
    assert!(
        preset.fast,
        "the preset's fast = true carries into the launch"
    );

    let alias = config
        .resolve_agent("codex", None, None, false, false)
        .unwrap();
    assert_eq!(alias.command, "/opt/codex-real");
    assert!(!alias.fast);
    let kind = Config::default()
        .resolve_agent("claude", None, None, false, false)
        .unwrap();
    assert_eq!(
        (kind.kind.as_str(), kind.command.as_str()),
        ("claude", "claude")
    );
}

fn toml_config(source: &str) -> Config {
    let tmp = tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    fs::write(&path, source).unwrap();
    Config::load(&path).unwrap()
}

#[test]
fn a_config_launch_table_gives_any_kind_model_effort_and_yolo_flags() {
    let config = toml_config(
        r#"
[agents.gemini]
command = "gemini"
launch = { model = ["--model", "{model}"], effort = [], fast = [], yolo = ["--yolo"] }
"#,
    );
    let launch = config
        .resolve_agent("gemini", Some("gemini-3-pro"), None, false, true)
        .unwrap();
    assert_eq!(
        launch_argv(&launch),
        vec!["gemini", "--model", "gemini-3-pro", "--yolo"]
    );
    let error = config
        .resolve_agent("gemini", None, Some("high"), false, false)
        .unwrap_err();
    assert_eq!(error.code, pod::ErrorCode::LaunchMappingUnsupported);
    assert!(
        error
            .next_steps
            .iter()
            .any(|s| s.contains("[agents.gemini]")),
        "{error:?}"
    );
    // Built-ins are unchanged.
    let codex = config
        .resolve_agent("codex", Some("gpt-5.6-terra"), Some("high"), true, true)
        .unwrap();
    assert_eq!(
        launch_argv(&codex),
        vec![
            "codex",
            "--no-daemon",
            "-m",
            "gpt-5.6-terra",
            "-c",
            "model_reasoning_effort=\"high\"",
            "-c",
            "service_tier=\"fast\"",
            "--dangerously-bypass-approvals-and-sandbox"
        ]
    );
}

#[test]
fn config_alias_rejects_shell_wrappers() {
    let tmp = tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    fs::write(&path, "[agents.codex]\ncommand = 'bash -lc codex'\n").unwrap();
    assert!(Config::load(&path).is_err());
}

#[test]
fn launch_argv_codex_and_claude() {
    let codex = Launch {
        kind: "codex".into(),
        command: "codex-bin".into(),
        model: Some("gpt-x".into()),
        effort: Some("high".into()),
        fast: true,
        yolo: true,
        args: vec!["--foo".into()],
        table: None,
    };
    assert_eq!(
        launch_argv(&codex),
        [
            "codex-bin",
            "--no-daemon",
            "-m",
            "gpt-x",
            "-c",
            "model_reasoning_effort=\"high\"",
            "-c",
            "service_tier=\"fast\"",
            "--dangerously-bypass-approvals-and-sandbox",
            "--foo"
        ]
    );

    let claude = Launch {
        kind: "claude".into(),
        command: "claude-bin".into(),
        model: Some("opus".into()),
        effort: Some("high".into()),
        fast: false,
        yolo: true,
        args: vec!["--verbose".into()],
        table: None,
    };
    assert_eq!(
        launch_argv(&claude),
        [
            "claude-bin",
            "--model",
            "opus",
            "--effort",
            "high",
            "--dangerously-skip-permissions",
            "--verbose"
        ]
    );
}

#[test]
fn config_fast_is_codex_only_and_parses_from_preset() {
    let config = Config::default();
    let codex = config
        .resolve_agent("codex", None, None, true, false)
        .unwrap();
    assert!(codex.fast);
    assert_eq!(
        launch_argv(&codex),
        ["codex", "--no-daemon", "-c", "service_tier=\"fast\""]
    );
    let plain = config
        .resolve_agent("codex", None, None, false, false)
        .unwrap();
    assert_eq!(launch_argv(&plain), ["codex", "--no-daemon"]);
    for kind in ["claude", "opencode"] {
        let error = config
            .resolve_agent(kind, None, None, true, false)
            .unwrap_err();
        assert_eq!(
            error.code,
            pod::ErrorCode::LaunchMappingUnsupported,
            "{kind}"
        );
    }

    let tmp = tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    fs::write(
        &path,
        "[presets.terra]\nagent = 'codex'\nmodel = 'gpt-5.6-sol'\nfast = true\n[presets.calm]\nagent = 'codex'\n",
    )
    .unwrap();
    let config = Config::load(&path).unwrap();
    assert!(config.presets["terra"].fast);
    assert!(!config.presets["calm"].fast);
    let terra = config
        .resolve_agent("terra", None, None, false, false)
        .unwrap();
    assert_eq!(
        launch_argv(&terra),
        [
            "codex",
            "--no-daemon",
            "-m",
            "gpt-5.6-sol",
            "-c",
            "service_tier=\"fast\""
        ]
    );
}

#[test]
fn config_yolo_maps_to_each_agents_bypass_flag_and_parses_from_preset() {
    let config = Config::default();
    let codex = config
        .resolve_agent("codex", None, None, false, true)
        .unwrap();
    assert!(codex.yolo);
    assert_eq!(
        launch_argv(&codex),
        [
            "codex",
            "--no-daemon",
            "--dangerously-bypass-approvals-and-sandbox"
        ]
    );
    let claude = config
        .resolve_agent("claude", None, None, false, true)
        .unwrap();
    assert_eq!(
        launch_argv(&claude),
        ["claude", "--dangerously-skip-permissions"]
    );
    let plain = config
        .resolve_agent("codex", None, None, false, false)
        .unwrap();
    assert_eq!(launch_argv(&plain), ["codex", "--no-daemon"]);
    let error = config
        .resolve_agent("opencode", None, None, false, true)
        .unwrap_err();
    assert_eq!(error.code, pod::ErrorCode::LaunchMappingUnsupported);

    let tmp = tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    fs::write(&path, "[presets.worker]\nagent = 'codex'\nyolo = true\n").unwrap();
    let config = Config::load(&path).unwrap();
    assert!(config.presets["worker"].yolo);
    let worker = config
        .resolve_agent("worker", None, None, false, false)
        .unwrap();
    assert_eq!(
        launch_argv(&worker),
        [
            "codex",
            "--no-daemon",
            "--dangerously-bypass-approvals-and-sandbox"
        ]
    );
}

#[test]
fn identity_none_outside_herdr() {
    let fake = FakeHerdr::new();
    let env = |_name: &str| None;
    assert!(pod::identity::resolve(&env, &fake).unwrap().is_none());
    assert!(fake.calls().is_empty());
}

#[test]
fn identity_fingerprint_stable() {
    let caller = Caller {
        terminal_id: "term_123".into(),
        pane_id: Some("pane_7".into()),
        pid: 42,
        session: "team-blue".into(),
    };
    assert_eq!(
        caller.fingerprint(),
        "1b3638d0727f62dc6e5316a7e2d8cb5e466bfc40729113a05422682e6fccc478"
    );
}

#[test]
fn cli_every_command_parses_documented_flags() {
    let commands: &[&[&str]] = &[
        &["doctor"],
        &["status"],
        &["skills", "get"],
        &["log"],
        &["board"],
        &["hook"],
        &["run-create"],
        &["run-use"],
        &["run-current"],
        &["run-list"],
        &["run-show"],
        &["task-create"],
        &["task-list"],
        &["task-update"],
        &["worker-start"],
        &["worker-show"],
        &["worker-read"],
        &["worker-list"],
        &["worker-stop"],
        &["worker-abandon"],
        &["worker-retain"],
        &["worker-release"],
        &["dispatch"],
        &["dispatch-show"],
        &["send"],
        &["check"],
        &["ask"],
        &["reply"],
        &["inbox"],
        &["gate-create"],
        &["gate-resolve"],
        &["gate-list"],
        &["request-show"],
        &["reset"],
    ];
    for command in commands {
        let mut cmd = Command::cargo_bin("pod").unwrap();
        cmd.args(*command).arg("--help").assert().success();
    }
}

#[test]
fn cli_error_envelope_and_exit_codes() {
    Command::cargo_bin("pod")
        .unwrap()
        .args(["run-create", "--json"])
        .assert()
        .code(2)
        .stderr(
            predicate::str::contains("\"error\"")
                .and(predicate::str::contains("\"code\":\"usage\"")),
        );
}

#[test]
fn doctor_reports_store_and_herdr() {
    let tmp = tempdir().unwrap();
    let bin = assert_cmd::cargo::cargo_bin("pod");
    let bin_dir = bin.parent().unwrap();
    let old_path = std::env::var_os("PATH").unwrap_or_default();
    let mut path_parts = vec![bin_dir.to_path_buf()];
    path_parts.extend(std::env::split_paths(&old_path));
    let path = std::env::join_paths(path_parts).unwrap();
    let output = Command::cargo_bin("pod")
        .unwrap()
        .args(["doctor", "--json"])
        .env("HOME", tmp.path())
        .env("POD_STATE_DIR", tmp.path().join("state"))
        .env("POD_CONFIG", tmp.path().join("config.toml"))
        .env("POD_FAKE_HERDR", "1")
        .env("PATH", path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let body: Value = serde_json::from_slice(&output.stdout).unwrap();
    let checks = body["checks"].as_array().unwrap();
    for name in ["herdr", "store", "path"] {
        assert!(
            checks
                .iter()
                .any(|check| check["name"] == name && check["status"] == "ok")
        );
    }
}

#[test]
fn defaults_are_contract_values() {
    assert_eq!(Defaults::default().nested_depth_max, 1);
    assert_eq!(Defaults::default().heartbeat_minutes, 5);
    assert_eq!(Defaults::default().ask_timeout_ms, 600_000);
    let _ = PathBuf::new();
}

#[test]
fn paths_session_from_herdr_socket() {
    let session = |socket: Option<&str>| {
        let mut values = BTreeMap::from([("HOME", "/Users/me".to_owned())]);
        if let Some(socket) = socket {
            values.insert("HERDR_SOCKET_PATH", socket.to_owned());
        }
        paths::session_from_env(&env_from(values))
    };
    assert_eq!(session(None), "default");
    assert_eq!(
        session(Some("/Users/me/.config/herdr/herdr.sock")),
        "default"
    );
    assert_eq!(
        session(Some("/Users/me/.config/herdr/sessions/pod-e2e/herdr.sock")),
        "pod-e2e"
    );
    assert_eq!(session(Some("/tmp/herdr/team-blue.sock")), "team-blue");
}

#[test]
fn store_creates_missing_state_root_0700() {
    let tmp = tempdir().unwrap();
    let paths = test_paths(tmp.path());
    Store::open(&paths).unwrap();
    for dir in [tmp.path().join("state"), paths.board_dir.clone()] {
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}

#[test]
fn store_tightens_existing_state_root_to_0700() {
    let tmp = tempdir().unwrap();
    let paths = test_paths(tmp.path());
    let root = tmp.path().join("state");
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
    Store::open(&paths).unwrap();
    assert_eq!(
        fs::metadata(&root).unwrap().permissions().mode() & 0o777,
        0o700
    );
}

#[test]
fn identity_none_without_pane_id() {
    let fake = FakeHerdr::new();
    fake.add_pane("pane_focused", "term_focused", "ws_1");
    let env = |name: &str| (name == "HERDR_ENV").then(|| "1".to_owned());
    assert!(pod::identity::resolve(&env, &fake).unwrap().is_none());
}

#[test]
fn identity_resolves_terminal_from_pane() {
    let fake = FakeHerdr::new();
    fake.add_pane("pane_7", "term_7", "ws_1");
    let env = |name: &str| match name {
        "HERDR_ENV" => Some("1".to_owned()),
        "HERDR_PANE_ID" => Some("pane_7".to_owned()),
        _ => None,
    };
    let caller = pod::identity::resolve(&env, &fake).unwrap().unwrap();
    assert_eq!(caller.terminal_id, "term_7");
    assert_eq!(caller.session, "default");
    let missing = |name: &str| match name {
        "HERDR_ENV" => Some("1".to_owned()),
        "HERDR_PANE_ID" => Some("pane_gone".to_owned()),
        _ => None,
    };
    let error = pod::identity::resolve(&missing, &fake).unwrap_err();
    assert_eq!(error.code, pod::ErrorCode::NotInHerdr);
}

#[test]
fn error_codes_match_errors_md() {
    let doc = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/docs/reference/errors.md"
    ))
    .unwrap();
    let documented: std::collections::BTreeSet<String> = doc
        .lines()
        .filter_map(|line| line.strip_prefix("| `"))
        .filter_map(|rest| rest.split('`').next())
        .map(str::to_owned)
        .collect();
    let implemented: std::collections::BTreeSet<String> = pod::ErrorCode::ALL
        .iter()
        .map(|code| {
            serde_json::to_value(code)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(implemented, documented);
    for code in pod::ErrorCode::ALL {
        let error = pod::PodError::new(*code, "x", &[]);
        let expected = if *code == pod::ErrorCode::Usage { 2 } else { 1 };
        assert_eq!(error.exit_code(), expected, "{code:?}");
    }
}

#[test]
fn config_unknown_agent_and_missing_mapping_exit_1() {
    let config = Config::default();
    let unknown = config
        .resolve_agent("my-agent", None, None, false, false)
        .unwrap_err();
    assert_eq!(unknown.code, pod::ErrorCode::UnknownAgent);
    assert_eq!(unknown.exit_code(), 1);
    let opencode = config
        .resolve_agent("opencode", None, None, false, false)
        .unwrap();
    assert_eq!(opencode.command, "opencode");
    let unmapped = config
        .resolve_agent("opencode", Some("m"), None, false, false)
        .unwrap_err();
    assert_eq!(unmapped.code, pod::ErrorCode::LaunchMappingUnsupported);
}

#[test]
fn config_alias_rules() {
    let tmp = tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    for bad in [
        "[agents.codex]\ncommand = '/usr/bin/env FOO=1 codex'\n",
        "[agents.codex]\ncommand = '/bin/sh'\n",
        "[presets.x]\nagent = 'mycodex'\n",
    ] {
        fs::write(&path, bad).unwrap();
        assert!(Config::load(&path).is_err(), "{bad}");
    }
    fs::write(&path, "[agents.codex]\ncommand = '/opt/codex/bin/codex'\n").unwrap();
    assert!(Config::load(&path).is_ok());
    // An `[agents.<kind>]` entry makes any kind known, to presets too.
    fs::write(
        &path,
        "[agents.mycodex]\ncommand = 'codex'\n[presets.x]\nagent = 'mycodex'\n",
    )
    .unwrap();
    let config = Config::load(&path).unwrap();
    assert_eq!(
        config
            .resolve_agent("x", None, None, false, false)
            .unwrap()
            .command,
        "codex"
    );
}

#[test]
fn cli_usage_errors_exit_2() {
    let commands: &[&[&str]] = &[
        &["check", "--peek", "--all"],
        &["ask", "--question", "q", "--resume", "msg_000000000001"],
        &["ask"],
        &["worker-start", "--task", "t", "--spec", "s", "--agent", "a"],
        &[
            "worker-start",
            "--task",
            "t",
            "--terminal",
            "term_1",
            "--fast",
        ],
        &[
            "worker-start",
            "--task",
            "t",
            "--terminal",
            "term_1",
            "--yolo",
        ],
        &["worker-start", "--task", "t"],
        &["worker-list", "--limit", "101"],
        &["skills", "get", "reviewer"],
        &["hook", "later"],
        &["log"],
        &["bogus-command"],
    ];
    // These commands open the store before they refuse, so they get a throwaway
    // one: running them against the real state directory would migrate it.
    let tmp = tempdir().unwrap();
    for command in commands {
        Command::cargo_bin("pod")
            .unwrap()
            .args(*command)
            .arg("--json")
            .env("HOME", tmp.path())
            .env("POD_STATE_DIR", tmp.path().join("state"))
            .assert()
            .code(2)
            .stderr(predicate::str::contains("\"code\":\"usage\""));
    }
}

#[test]
fn doctor_reports_unwritable_store_as_failed_check() {
    let tmp = tempdir().unwrap();
    let blocker = tmp.path().join("not-a-dir");
    fs::write(&blocker, "").unwrap();
    let output = Command::cargo_bin("pod")
        .unwrap()
        .args(["doctor", "--json"])
        .env("HOME", tmp.path())
        .env("POD_STATE_DIR", blocker.join("state"))
        .env("POD_FAKE_HERDR", "1")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let body: Value = serde_json::from_slice(&output.stdout).unwrap();
    let checks = body["checks"].as_array().unwrap();
    assert!(
        checks
            .iter()
            .any(|check| check["name"] == "store" && check["status"] == "fail")
    );
    assert!(
        checks
            .iter()
            .any(|check| check["name"] == "herdr" && check["status"] == "ok")
    );
}

#[test]
fn status_json_prints_paths() {
    let tmp = tempdir().unwrap();
    let output = Command::cargo_bin("pod")
        .unwrap()
        .args(["status", "--json"])
        .env("HOME", tmp.path())
        .env("POD_STATE_DIR", tmp.path().join("state"))
        .env("POD_FAKE_HERDR", "1")
        .env_remove("HERDR_SOCKET_PATH")
        .output()
        .unwrap();
    assert!(output.status.success());
    let body: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["ok"], true);
    assert_eq!(body["session"], "default");
    assert_eq!(body["store"]["userVersion"], 3);
    assert_eq!(body["herdr"]["reachable"], true);
    assert!(
        body["paths"]["dbPath"]
            .as_str()
            .unwrap()
            .ends_with("state/default/pod.db")
    );
}

#[test]
fn config_notify_command_parses_and_defaults_to_empty() {
    let tmp = tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    assert!(Config::load(&path).unwrap().notify.command.is_empty());
    fs::write(
        &path,
        "[notify]\ncommand = [\"sh\", \"-c\", \"echo \\\"$NOTIFY_TITLE|$NOTIFY_BODY\\\" >> /tmp/x\"]\n",
    )
    .unwrap();
    let config = Config::load(&path).unwrap();
    assert_eq!(
        config.notify.command,
        ["sh", "-c", "echo \"$NOTIFY_TITLE|$NOTIFY_BODY\" >> /tmp/x"]
    );
}

/// Live finding 2026-10-14: with the Codex desktop app's shared server
/// running, a plain `codex` waits at a restart dialog. The built-in table
/// adds `--no-daemon` to every Codex launch, once even when a preset already
/// passes it, and only a config `always = []` takes it off.
#[test]
fn every_codex_launch_runs_without_the_shared_daemon() {
    let codex = Config::default()
        .resolve_agent("codex", None, None, false, false)
        .unwrap();
    assert_eq!(launch_argv(&codex), ["codex", "--no-daemon"]);
    let config = toml_config(
        r#"
[presets.live]
agent = "codex"
args = ["--no-daemon"]
"#,
    );
    let preset = config
        .resolve_agent("live", None, None, false, false)
        .unwrap();
    assert_eq!(launch_argv(&preset), ["codex", "--no-daemon"]);
    let config = toml_config(
        r#"
[agents.codex]
launch = { always = [], yolo = ["--dangerously-bypass-approvals-and-sandbox"] }
"#,
    );
    let own = config
        .resolve_agent("codex", None, None, false, true)
        .unwrap();
    assert_eq!(
        launch_argv(&own),
        ["codex", "--dangerously-bypass-approvals-and-sandbox"]
    );
    let claude = Config::default()
        .resolve_agent("claude", None, None, false, false)
        .unwrap();
    assert_eq!(launch_argv(&claude), ["claude"]);
}

fn load_error(source: &str) -> String {
    let tmp = tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    fs::write(&path, source).unwrap();
    Config::load(&path).unwrap_err().message
}

/// Grok review of B2, finding 1: an entry with no `command` launches the
/// kind's own name, so the shell-wrapper ban applies to that name too.
#[test]
fn an_agent_entry_without_a_command_still_passes_the_wrapper_ban() {
    for kind in ["sh", "env", "sudo"] {
        let message = load_error(&format!("[agents.{kind}]\n"));
        assert!(
            message.contains("one executable token"),
            "{kind}: {message}"
        );
    }
    assert!(
        toml_config("[agents.gemini]\n")
            .resolve_agent("gemini", None, None, false, false)
            .is_ok()
    );
}

/// Grok review of B2, finding 2: a `launch` table gives only the keys it
/// names, over the built-in, and a misspelled or misplaced key is a config
/// error instead of a silently dropped flag.
#[test]
fn a_launch_table_merges_over_the_builtin_and_refuses_unknown_keys() {
    let config = toml_config(
        r#"
[agents.codex]
launch = { model = ["--model", "{model}"] }
"#,
    );
    let codex = config
        .resolve_agent("codex", Some("gpt-5.6"), Some("high"), true, true)
        .unwrap();
    assert_eq!(
        launch_argv(&codex),
        vec![
            "codex",
            "--no-daemon",
            "--model",
            "gpt-5.6",
            "-c",
            "model_reasoning_effort=\"high\"",
            "-c",
            "service_tier=\"fast\"",
            "--dangerously-bypass-approvals-and-sandbox"
        ]
    );
    // `launch = {}` changes nothing.
    let empty = toml_config("[agents.claude]\nlaunch = {}\n");
    let claude = empty
        .resolve_agent("claude", Some("opus"), None, false, true)
        .unwrap();
    assert_eq!(
        launch_argv(&claude),
        [
            "claude",
            "--model",
            "opus",
            "--dangerously-skip-permissions"
        ]
    );
    // The old flat shape, and a typo inside `launch`, are refused at load.
    for bad in [
        "[agents.gemini]\nmodel = [\"--model\", \"{model}\"]\n",
        "[agents.gemini]\nlaunch = { yollo = [\"--yolo\"] }\n",
    ] {
        assert!(load_error(bad).contains("unknown field"), "{bad}");
    }
}

/// Grok review of B2, finding 4: a model or effort fragment without its
/// placeholder would drop the value, so the config refuses it at load and
/// `resolve_agent` treats it as no mapping.
#[test]
fn a_model_or_effort_fragment_must_carry_its_placeholder() {
    for bad in [
        "[agents.gemini]\nlaunch = { model = [\"--model\"] }\n",
        "[agents.gemini]\nlaunch = { effort = [\"--effort\", \"{model}\"] }\n",
    ] {
        let message = load_error(bad);
        assert!(message.contains("must use {"), "{bad}: {message}");
    }
    let mut config = Config::default();
    config.agents.insert(
        "gemini".into(),
        AgentAlias {
            command: String::new(),
            launch: Some(pod::config::LaunchOverrides {
                model: Some(vec!["--model".into()]),
                ..Default::default()
            }),
        },
    );
    let error = config
        .resolve_agent("gemini", Some("gemini-3-pro"), None, false, false)
        .unwrap_err();
    assert_eq!(error.code, pod::ErrorCode::LaunchMappingUnsupported);
}

/// Grok review of B2, finding 6: the next step is a snippet a person can
/// paste, and a value that came from a preset names the preset.
#[test]
fn an_unmapped_flag_names_a_pasteable_snippet_and_its_preset() {
    let config = toml_config(
        r#"
[agents.gemini]
launch = { yolo = ["--yolo"] }

[presets.deep]
agent = "gemini"
effort = "high"
"#,
    );
    let direct = config
        .resolve_agent("gemini", Some("gemini-3-pro"), None, false, false)
        .unwrap_err();
    assert_eq!(direct.next_steps[0], "Remove --model.");
    assert!(
        direct.next_steps[1]
            .contains(r#"`launch = { model = ["--model", "{model}"] }` under [agents.gemini]"#),
        "{:?}",
        direct.next_steps
    );
    let preset = config
        .resolve_agent("deep", None, None, false, false)
        .unwrap_err();
    assert_eq!(
        preset.next_steps[0],
        "Remove effort from preset deep in config.toml."
    );
    assert!(
        preset.next_steps[1].contains(r#"effort = ["--effort", "{effort}"]"#),
        "{:?}",
        preset.next_steps
    );
    // The snippet is valid config once pasted.
    let pasted = toml_config(
        r#"
[agents.gemini]
launch = { model = ["--model", "{model}"] }
"#,
    );
    assert!(
        pasted
            .resolve_agent("gemini", Some("gemini-3-pro"), None, false, false)
            .is_ok()
    );
}

/// The launch tables `docs/reference/config.md` writes out load as they
/// stand: the two built-ins equal the code's, and the grok example gives
/// grok its flags.
#[test]
fn the_launch_tables_in_config_md_are_the_builtins_and_a_working_grok_example() {
    let doc = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/docs/reference/config.md"
    ))
    .unwrap();
    let block = doc
        .split("```toml\n")
        .find(|block| block.contains("[agents.grok]"))
        .and_then(|block| block.split("```").next())
        .expect("the launch-table block in config.md");
    let config = toml_config(block);
    for kind in ["codex", "claude"] {
        assert_eq!(
            config.launch_table(kind),
            pod::config::builtin_launch_table(kind),
            "{kind}"
        );
    }
    let grok = config
        .resolve_agent("grok", Some("grok-5"), Some("high"), false, true)
        .unwrap();
    assert_eq!(
        launch_argv(&grok),
        [
            "grok",
            "-m",
            "grok-5",
            "--reasoning-effort",
            "high",
            "--permission-mode",
            "bypassPermissions"
        ]
    );
}
