mod support;

use pod::config::Config;
use pod::doctor::{self, Probes};
use support::TestEnv;

fn config(source: &str) -> Config {
    toml_config(source)
}

fn toml_config(source: &str) -> Config {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    std::fs::write(&path, source).unwrap();
    Config::load(&path).unwrap()
}

fn probes(integrations: Result<&str, &str>, dagr: bool) -> Probes {
    Probes {
        integration_status: integrations.map(str::to_owned).map_err(str::to_owned),
        dagr_present: dagr,
    }
}

/// `herdr integration status` output as herdr 0.9.3 prints it.
const HERDR_STATUS_BOTH: &str =
    "pi: not installed (/home/me/.pi/agent/extensions/herdr-agent-state.ts)
claude: current (v10) (/home/me/.claude/hooks/herdr-agent-state.sh)
codex: current (v8) (/home/me/.codex/herdr-agent-state.sh)
letta (experimental): not installed (/home/me/.letta/hooks/herdr-agent-session.sh)
";

const HERDR_STATUS_CLAUDE_ONLY: &str =
    "claude: current (v10) (/home/me/.claude/hooks/herdr-agent-state.sh)
codex: not installed (/home/me/.codex/herdr-agent-state.sh)
";

fn names(report: &doctor::DoctorReport) -> Vec<&str> {
    report.checks.iter().map(|c| c.name.as_str()).collect()
}

#[test]
fn doctor_reports_every_check_in_section_18() {
    let env = TestEnv::new();
    let config = config("[presets.sol]\nagent = \"codex\"\n[presets.terra]\nagent = \"claude\"\n");
    let report = doctor::run_with(
        &env.engine.paths,
        &*env.herdr,
        &config,
        &probes(Ok(HERDR_STATUS_BOTH), true),
    );
    let names = names(&report);
    for expected in [
        "herdr",
        "store",
        "path",
        "agent-integration:claude",
        "agent-integration:codex",
        "sandbox:claude",
        "sandbox:codex",
        "herdr-dagr",
    ] {
        assert!(names.contains(&expected), "missing {expected}: {names:?}");
    }
    let by = |name: &str| report.checks.iter().find(|c| c.name == name).unwrap();
    assert_eq!(by("agent-integration:claude").status, "ok");
    assert_eq!(by("sandbox:codex").status, "ok");
    assert_eq!(by("agent-integration:codex").status, "ok");
    assert_eq!(by("herdr-dagr").status, "ok");
}

#[test]
fn doctor_reads_outdated_and_unlisted_integrations() {
    let env = TestEnv::new();
    let config = config(
        "[presets.sol]\nagent = \"codex\"\n[presets.luna]\nagent = \"antigravity_cli\"\n[presets.terra]\nagent = \"claude\"\n",
    );
    let report = doctor::run_with(
        &env.engine.paths,
        &*env.herdr,
        &config,
        &probes(
            Ok(
                "claude: outdated (v9) (/home/me/.claude/hooks/herdr-agent-state.sh)\nantigravity-cli: current (v1) (/x)\n",
            ),
            true,
        ),
    );
    let by = |name: &str| report.checks.iter().find(|c| c.name == name).unwrap();
    let claude = by("agent-integration:claude");
    assert_eq!(claude.status, "warn");
    assert!(claude.detail.contains("outdated"), "{}", claude.detail);
    assert!(claude.detail.contains("herdr integration install claude"));
    assert_eq!(by("agent-integration:antigravity_cli").status, "ok");
    let codex = by("agent-integration:codex");
    assert_eq!(codex.status, "warn");
    assert!(
        codex.detail.contains("does not list codex"),
        "{}",
        codex.detail
    );
}

#[test]
fn doctor_reports_an_invalid_config_as_a_failed_check() {
    let env = TestEnv::new();
    std::fs::write(&env.engine.paths.config_path, "presets = 3\n").unwrap();
    let report = doctor::run(&env.engine.paths, &*env.herdr);
    let config = report.checks.iter().find(|c| c.name == "config").unwrap();
    assert_eq!(config.status, "fail");
    assert!(report.failed());
    // The agent checks still run, against the default kinds.
    assert!(
        report
            .checks
            .iter()
            .any(|c| c.name == "agent-integration:claude")
    );
    assert!(report.checks.iter().any(|c| c.name == "sandbox:codex"));
}

#[test]
fn doctor_fix_hints_per_agent_kind() {
    let env = TestEnv::new();
    let config = config("[presets.sol]\nagent = \"codex\"\n[presets.terra]\nagent = \"claude\"\n");
    let report = doctor::run_with(
        &env.engine.paths,
        &*env.herdr,
        &config,
        &probes(Ok(HERDR_STATUS_CLAUDE_ONLY), false),
    );
    let by = |name: &str| report.checks.iter().find(|c| c.name == name).unwrap();
    assert_eq!(by("agent-integration:claude").status, "ok");
    let codex = by("agent-integration:codex");
    assert_eq!(codex.status, "warn");
    assert!(codex.detail.contains("herdr integration install codex"));
    let dagr = by("herdr-dagr");
    assert_eq!(dagr.status, "warn");
    assert!(dagr.detail.contains("not found"));
    assert!(dagr.detail.contains("herdr plugin install"));

    // An unreachable `herdr integration status` is a warning that names the fix.
    let report = doctor::run_with(
        &env.engine.paths,
        &*env.herdr,
        &config,
        &probes(Err("herdr: not found"), true),
    );
    let claude = report
        .checks
        .iter()
        .find(|c| c.name == "agent-integration:claude")
        .unwrap();
    assert_eq!(claude.status, "warn");
    assert!(claude.detail.contains("herdr integration install claude"));

    // Sandbox hints are per kind when the state directory is not writable.
    let state = &env.engine.paths.state_dir;
    let mut perms = std::fs::metadata(state).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o500);
    std::fs::set_permissions(state, perms.clone()).unwrap();
    let report = doctor::run_with(
        &env.engine.paths,
        &*env.herdr,
        &config,
        &probes(Ok(""), true),
    );
    perms.set_mode(0o700);
    std::fs::set_permissions(state, perms).unwrap();
    let get = |n: &str| {
        report
            .checks
            .iter()
            .find(|c| c.name == n)
            .unwrap()
            .clone_detail()
    };
    let (claude_status, claude_detail) = get("sandbox:claude");
    let (codex_status, codex_detail) = get("sandbox:codex");
    if claude_status == "fail" {
        assert!(claude_detail.contains("Claude Code"));
        assert!(codex_detail.contains("writable root"));
        assert_eq!(codex_status, "fail");
    }
}

trait CloneDetail {
    fn clone_detail(&self) -> (String, String);
}
impl CloneDetail for doctor::Check {
    fn clone_detail(&self) -> (String, String) {
        (self.status.clone(), self.detail.clone())
    }
}

#[test]
fn usage_check_warns_until_the_status_line_and_bar_are_wired() {
    let env = TestEnv::new();
    let config = config("");
    let report = doctor::run_with(
        &env.engine.paths,
        &*env.herdr,
        &config,
        &probes(Ok(HERDR_STATUS_BOTH), true),
    );
    let check = report.checks.iter().find(|c| c.name == "usage").unwrap();
    assert_eq!(check.status, "warn");
    assert!(
        check.detail.contains("pod usage --install"),
        "{}",
        check.detail
    );

    pod::usage::install_claude_statusline(&env.engine.paths.claude_settings, "pod usage --record")
        .unwrap();
    pod::usage::install_herdr_bar(&env.engine.paths.herdr_config, "pod usage").unwrap();
    let report = doctor::run_with(
        &env.engine.paths,
        &*env.herdr,
        &config,
        &probes(Ok(HERDR_STATUS_BOTH), true),
    );
    let check = report.checks.iter().find(|c| c.name == "usage").unwrap();
    assert_eq!(
        (check.status.as_str(), check.detail.as_str()),
        ("ok", "status line and tab bar wired")
    );
}

#[test]
fn doctor_lists_the_kinds_with_a_launch_table() {
    let env = TestEnv::new();
    let run = |config: &Config| {
        let report = doctor::run_with(
            &env.engine.paths,
            &*env.herdr,
            config,
            &probes(Ok(HERDR_STATUS_BOTH), true),
        );
        report
            .checks
            .into_iter()
            .find(|c| c.name == "launch-mapping")
            .expect("a launch-mapping check")
    };
    let default = run(&Config::default());
    assert_eq!(default.status, "ok");
    assert!(
        default.detail.contains("claude, codex"),
        "{}",
        default.detail
    );
    assert!(!default.detail.contains("gemini"), "{}", default.detail);

    let with_gemini = run(&config(
        "[agents.gemini]\ncommand = \"gemini\"\nlaunch = { model = [\"--model\", \"{model}\"], yolo = [\"--yolo\"] }\n",
    ));
    assert_eq!(with_gemini.status, "ok");
    assert!(
        with_gemini.detail.contains("claude, codex, gemini"),
        "{}",
        with_gemini.detail
    );
    // A kind is listed for what its table maps, not for having an entry: a
    // gemini entry that maps nothing is left out, and claude with every flag
    // emptied drops off.
    let unmapped = run(&config(
        "[agents.gemini]\nlaunch = { model = [] }\n[agents.claude]\nlaunch = { model = [], effort = [], yolo = [] }\n",
    ));
    assert_eq!(unmapped.detail, "codex");
}

/// Live finding 2026-10-14: a newer pod migrated the shared store and every
/// older build refused it. `doctor` names both schema versions and never
/// migrates the store itself.
#[test]
fn doctor_names_the_store_and_binary_schema_versions_without_migrating() {
    let env = TestEnv::new();
    let latest = pod::store::schema::LATEST_VERSION;
    // 0.2.0's store is version 3 (B1's `runs.budget_usd`).
    assert_eq!(latest, 3);
    let store = |version: u32| {
        let connection = rusqlite::Connection::open(&env.engine.paths.db_path).unwrap();
        connection
            .pragma_update(None, "user_version", version)
            .unwrap();
    };
    let check = || {
        let report = doctor::run_with(
            &env.engine.paths,
            &*env.herdr,
            &Config::default(),
            &probes(Ok(HERDR_STATUS_BOTH), true),
        );
        report
            .checks
            .into_iter()
            .find(|check| check.name == "store")
            .unwrap()
    };
    let current = check();
    assert_eq!(current.status, "ok", "{current:?}");
    assert!(
        current.detail.contains(&format!("at schema {latest}")),
        "{current:?}"
    );

    store(latest - 1);
    let older = check();
    assert_eq!(older.status, "warn", "{older:?}");
    assert!(
        older
            .detail
            .contains(&format!("schema {}, this pod's is {latest}", latest - 1)),
        "{older:?}"
    );
    let version: u32 = rusqlite::Connection::open(&env.engine.paths.db_path)
        .unwrap()
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, latest - 1, "doctor migrated the store");

    store(latest + 1);
    let newer = check();
    assert_eq!(newer.status, "fail", "{newer:?}");
    assert!(newer.detail.contains("newer than this pod's"), "{newer:?}");
}
