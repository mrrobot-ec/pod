use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;

use serde::Serialize;

use crate::config::Config;
use crate::herdr::Herdr;
use crate::paths::Paths;
use crate::store::Store;

/// herdr-dagr's install command; the same text `pod board` prints.
/// ASSUMPTION: the command text (commands.md).
const DAGR_INSTALL: &str = "herdr plugin install aemrebarut/herdr-dagr";

#[derive(Debug, Serialize)]
pub struct Check {
    pub name: String,
    pub status: String,
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub struct DoctorReport {
    pub checks: Vec<Check>,
}

impl DoctorReport {
    pub fn failed(&self) -> bool {
        self.checks.iter().any(|check| check.status == "fail")
    }
}

/// What doctor learns from outside pod: the output of
/// `herdr integration status` and whether herdr-dagr is installed. Injected so
/// tests need no herdr.
#[derive(Debug)]
pub struct Probes {
    pub integration_status: std::result::Result<String, String>,
    pub dagr_present: bool,
}

impl Probes {
    /// Runs the real `herdr integration status` (pod executes only herdr;
    /// herdr 0.9.3 has no `integration list`) and looks for dagr on `PATH`, as
    /// `pod board` does, or among herdr's plugins.
    pub fn real() -> Probes {
        let integration_status = Command::new("herdr")
            .args(["integration", "status"])
            .output()
            .map_err(|error| error.to_string())
            .and_then(|out| {
                if out.status.success() {
                    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
                } else {
                    Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
                }
            });
        let dagr_present = find_on_path("dagr").is_some()
            || find_on_path("herdr-dagr").is_some()
            || Command::new("herdr")
                .args(["plugin", "list"])
                .output()
                .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains("herdr-dagr"));
        Probes {
            integration_status,
            dagr_present,
        }
    }
}

/// Checks with the real probes and the config at `paths.config_path`. A config
/// that cannot be loaded is reported as a failed `config` check, and the agent
/// checks fall back to the default kinds (claude and codex).
pub fn run(paths: &Paths, herdr: &dyn Herdr) -> DoctorReport {
    let (config, config_error) = match Config::load(&paths.config_path) {
        Ok(config) => (config, None),
        Err(error) => (Config::default(), Some(error)),
    };
    let mut report = run_with(paths, herdr, &config, &Probes::real());
    if let Some(error) = config_error {
        report.checks.push(Check {
            name: "config".into(),
            status: "fail".into(),
            detail: format!(
                "{}; the agent checks above used the default kinds (claude, codex)",
                error.message
            ),
        });
    }
    report
}

/// Spec section 18 checks: herdr reachable at >= 0.9.3 and protocol 22, store
/// writable and passing `integrity_check`, `pod` on `PATH`, a herdr agent
/// integration installed and a writable state directory for each configured
/// agent kind, and herdr-dagr (optional). A failing check is reported, never
/// raised, so every check always runs.
pub fn run_with(
    paths: &Paths,
    herdr: &dyn Herdr,
    config: &Config,
    probes: &Probes,
) -> DoctorReport {
    let herdr_check = match herdr.ping() {
        Ok(info) if version_at_least(&info.version, (0, 9, 3)) && info.protocol == 22 => Check {
            name: "herdr".into(),
            status: "ok".into(),
            detail: format!("{} (protocol {})", info.version, info.protocol),
        },
        Ok(info) => Check {
            name: "herdr".into(),
            status: "fail".into(),
            detail: format!(
                "{} (protocol {}; need >= 0.9.3, protocol 22)",
                info.version, info.protocol
            ),
        },
        Err(error) => Check {
            name: "herdr".into(),
            status: "fail".into(),
            detail: error.to_string(),
        },
    };
    let store_check = store_check(paths);
    let path_check = find_on_path("pod").map_or_else(
        || Check {
            name: "path".into(),
            status: "fail".into(),
            detail: "pod not found on PATH".into(),
        },
        |path| Check {
            name: "path".into(),
            status: "ok".into(),
            detail: path.display().to_string(),
        },
    );
    let mut checks = vec![herdr_check, store_check, path_check];
    let kinds = configured_kinds(config);
    for kind in &kinds {
        checks.push(integration_check(kind, &probes.integration_status));
    }
    let probe = probe_state_dir(paths);
    for kind in &kinds {
        checks.push(sandbox_check(kind, paths, &probe));
    }
    checks.push(if probes.dagr_present {
        Check {
            name: "herdr-dagr".into(),
            status: "ok".into(),
            detail: "present".into(),
        }
    } else {
        Check {
            name: "herdr-dagr".into(),
            status: "warn".into(),
            detail: format!(
                "not found (optional; `pod board` needs it); install with `{DAGR_INSTALL}`"
            ),
        }
    });
    checks.push(usage_check(paths));
    checks.push(launch_mapping_check(config));
    DoctorReport { checks }
}

/// `usage`: whether Claude Code's status line and herdr's tab bar run
/// `pod usage`. A warning, never a failure: nothing else depends on it.
fn usage_check(paths: &Paths) -> Check {
    let statusline = crate::usage::claude_statusline_installed(&paths.claude_settings);
    let bar = crate::usage::herdr_bar_installed(&paths.herdr_config);
    let detail = match (statusline, bar) {
        (true, true) => "status line and tab bar wired".to_owned(),
        (false, true) => format!(
            "Claude status line not wired ({}); run `pod usage --install`",
            paths.claude_settings.display()
        ),
        (true, false) => format!(
            "herdr tab bar not wired ({}); run `pod usage --install`",
            paths.herdr_config.display()
        ),
        (false, false) => {
            "Claude status line and herdr tab bar not wired; run `pod usage --install`".to_owned()
        }
    };
    Check {
        name: "usage".into(),
        status: if statusline && bar { "ok" } else { "warn" }.into(),
        detail,
    }
}

/// `launch-mapping`: the agent kinds whose launch table (the built-in with
/// any `[agents.<kind>]` `launch` keys over it) maps at least one of
/// `--model`, `--effort`, `--fast`, `--yolo`. Informational, always `ok`.
fn launch_mapping_check(config: &Config) -> Check {
    let kinds: BTreeSet<&str> = ["claude", "codex"]
        .into_iter()
        .chain(config.agents.keys().map(String::as_str))
        .filter(|kind| {
            config
                .launch_table(kind)
                .is_some_and(|table| table.maps_any_flag())
        })
        .collect();
    Check {
        name: "launch-mapping".into(),
        status: "ok".into(),
        detail: kinds.into_iter().collect::<Vec<_>>().join(", "),
    }
}

/// Agent kinds named by presets and aliases; claude and codex when none are.
fn configured_kinds(config: &Config) -> Vec<String> {
    let mut kinds: BTreeSet<String> = config.presets.values().map(|p| p.agent.clone()).collect();
    kinds.extend(config.agents.keys().cloned());
    if kinds.is_empty() {
        kinds.extend(["claude".to_owned(), "codex".to_owned()]);
    }
    kinds.into_iter().collect()
}

fn herdr_kind_name(kind: &str) -> String {
    kind.replace('_', "-")
}

fn integration_check(kind: &str, status: &std::result::Result<String, String>) -> Check {
    let name = format!("agent-integration:{kind}");
    let install = format!("herdr integration install {}", herdr_kind_name(kind));
    let (status, detail) = match status {
        Err(error) => (
            "warn",
            format!("could not run `herdr integration status` ({error}); run `{install}`"),
        ),
        Ok(output) => match integration_state(output, kind) {
            None => (
                "warn",
                format!(
                    "`herdr integration status` does not list {kind}; herdr has no integration for it"
                ),
            ),
            Some(state) if state.starts_with("not installed") => {
                ("warn", format!("not installed; run `{install}`"))
            }
            Some(state) if state.starts_with("outdated") => {
                ("warn", format!("{state}; run `{install}` to update it"))
            }
            Some(state) => ("ok", state.to_owned()),
        },
    };
    Check {
        name,
        status: status.into(),
        detail,
    }
}

/// The state `herdr integration status` reports for `kind`. herdr 0.9.3
/// prints one line per kind, `<kind>[ (experimental)]: <state> (<hook path>)`,
/// for example `claude: current (v10) (...)` or `pi: not installed (...)`.
fn integration_state<'a>(output: &'a str, kind: &str) -> Option<&'a str> {
    let herdr_name = herdr_kind_name(kind);
    output.lines().find_map(|line| {
        let (name, state) = line.split_once(": ")?;
        let name = name.split_whitespace().next()?;
        (name == kind || name == herdr_name).then(|| state.trim())
    })
}

fn sandbox_hint(kind: &str) -> &'static str {
    match kind {
        "claude" => {
            "Allow writes to the pod state directory in Claude Code's sandbox settings (the filesystem write allowlist), then restart the session."
        }
        "codex" => {
            "Add the state directory to the Codex sandbox's writable roots, or run Codex with a sandbox mode that allows it, then restart the agent."
        }
        _ => "Allow the agent's sandbox to write the pod state directory, then restart the agent.",
    }
}

/// One write probe in the state directory, shared by every agent kind. It
/// never creates the directory (the store check does, with mode 0700), and the
/// probe name carries the pid so concurrent doctors do not race.
fn probe_state_dir(paths: &Paths) -> std::io::Result<()> {
    let probe = paths
        .state_dir
        .join(format!(".doctor-write-probe-{}", std::process::id()));
    fs::write(&probe, b"ok")?;
    fs::remove_file(&probe)
}

/// ASSUMPTION: pod cannot enter another process's sandbox, so this reports the
/// write probe from doctor's own process and attaches the per-kind fix an
/// agent sandbox would need.
fn sandbox_check(kind: &str, paths: &Paths, probe: &std::io::Result<()>) -> Check {
    let (status, detail) = match probe {
        Ok(()) => (
            "ok",
            format!(
                "{} writable (probed from doctor's process)",
                paths.state_dir.display()
            ),
        ),
        Err(error) => (
            "fail",
            format!(
                "{} not writable: {error}. {}",
                paths.state_dir.display(),
                sandbox_hint(kind)
            ),
        ),
    };
    Check {
        name: format!("sandbox:{kind}"),
        status: status.into(),
        detail,
    }
}

/// The store's health and schema version against this binary's. An
/// existing store is probed, never migrated: a store a newer build migrated
/// is refused by this one, and one this build would migrate is then refused
/// by every older build on the machine (seen live 2026-10-14).
fn store_check(paths: &Paths) -> Check {
    let latest = crate::store::schema::LATEST_VERSION;
    let db = paths.db_path.display();
    let (status, detail) = if !paths.db_path.exists() {
        match Store::open(paths).and_then(|store| store.integrity_ok()) {
            Ok(true) => (
                "ok",
                format!("{db} created at schema {latest}; writable; integrity ok"),
            ),
            Ok(false) => ("fail", format!("{db} failed integrity_check")),
            Err(error) => (
                "fail",
                format!("{}: {}", error.message, error.next_steps.join(" ")),
            ),
        }
    } else {
        match Store::probe(paths) {
            Ok(probe) if !probe.integrity_ok => ("fail", format!("{db} failed integrity_check")),
            Ok(probe) if !probe.writable => ("fail", format!("{db} is not writable")),
            Ok(probe) if probe.version > latest => (
                "fail",
                format!(
                    "{db} is at schema {}, newer than this pod's {latest}: a newer pod build migrated it, so this build refuses it; run that build or upgrade",
                    probe.version
                ),
            ),
            Ok(probe) if probe.version < latest => (
                "warn",
                format!(
                    "{db} is at schema {}, this pod's is {latest}: its next command migrates the store, and every older pod build then refuses it",
                    probe.version
                ),
            ),
            Ok(probe) => (
                "ok",
                format!(
                    "{db} at schema {}, this pod's too; writable; integrity ok",
                    probe.version
                ),
            ),
            Err(error) => (
                "fail",
                format!("{}: {}", error.message, error.next_steps.join(" ")),
            ),
        }
    };
    Check {
        name: "store".into(),
        status: status.into(),
        detail,
    }
}

fn version_at_least(version: &str, minimum: (u64, u64, u64)) -> bool {
    let mut parts = version.split('.').map(|part| {
        part.split(|ch: char| !ch.is_ascii_digit())
            .next()
            .unwrap_or("")
            .parse::<u64>()
            .unwrap_or(0)
    });
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    ) >= minimum
}

fn find_on_path(name: &str) -> Option<std::path::PathBuf> {
    env::var_os("PATH").and_then(|path| {
        env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|candidate| is_executable(candidate))
    })
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}
