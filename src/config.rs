use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{ErrorCode, PodError, Result};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub defaults: Defaults,
    pub agents: BTreeMap<String, AgentAlias>,
    pub presets: BTreeMap<String, Preset>,
    pub repos: BTreeMap<PathBuf, RepoConfig>,
    pub notify: NotifyConfig,
    pub limits: Limits,
}

/// `[notify]`: an argv pod runs for every decision and every newly stuck
/// worker, with `NOTIFY_TITLE` and `NOTIFY_BODY` in its environment, for
/// push plugins. Empty (the default) runs nothing.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct NotifyConfig {
    pub command: Vec<String>,
}

/// `[limits]`: fleet caps and the usage threshold that pause `worker-start`.
/// Every field is off by default.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Limits {
    pub max_active_workers: Option<u32>,
    pub max_active_per_kind: BTreeMap<String, u32>,
    pub pause_above_percent: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Defaults {
    pub nested_depth_max: u32,
    pub heartbeat_minutes: u64,
    pub ask_timeout_ms: u64,
    /// Acknowledging a `worker_done` releases its worker, and `check --wait`
    /// first releases every acknowledged one (spec 7.4), closing the tab pod
    /// created. `release_on_wait` is the 0.2.7 name of the same switch.
    #[serde(alias = "release_on_wait")]
    pub auto_release: bool,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            nested_depth_max: 1,
            heartbeat_minutes: 5,
            ask_timeout_ms: 600_000,
            auto_release: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentAlias {
    /// The executable; empty means the kind's own name.
    #[serde(default)]
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch: Option<LaunchOverrides>,
}

/// A config `launch` table: each key it gives replaces that key of the
/// kind's built-in table, and every key it leaves out keeps the built-in's.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct LaunchOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub always: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fast: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub yolo: Option<Vec<String>>,
}

impl LaunchOverrides {
    /// `base` with every key this table gives replaced.
    pub fn over(&self, base: LaunchTable) -> LaunchTable {
        let pick = |given: &Option<Vec<String>>, base: Vec<String>| given.clone().unwrap_or(base);
        LaunchTable {
            always: pick(&self.always, base.always),
            model: pick(&self.model, base.model),
            effort: pick(&self.effort, base.effort),
            fast: pick(&self.fast, base.fast),
            yolo: pick(&self.yolo, base.yolo),
        }
    }
}

/// Launch flags for one agent kind: each entry is an argv fragment with
/// `{model}` or `{effort}` substituted; an empty vector means unsupported.
/// `always` goes on every launch of the kind, right after the command.
/// `model` and `effort` count as supported only when a part holds the
/// placeholder, so a value is never dropped.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct LaunchTable {
    pub always: Vec<String>,
    pub model: Vec<String>,
    pub effort: Vec<String>,
    pub fast: Vec<String>,
    pub yolo: Vec<String>,
}

impl LaunchTable {
    pub fn maps_model(&self) -> bool {
        self.model.iter().any(|part| part.contains("{model}"))
    }

    pub fn maps_effort(&self) -> bool {
        self.effort.iter().any(|part| part.contains("{effort}"))
    }

    /// Whether any of `--model`, `--effort`, `--fast`, `--yolo` has a mapping.
    pub fn maps_any_flag(&self) -> bool {
        self.maps_model() || self.maps_effort() || !self.fast.is_empty() || !self.yolo.is_empty()
    }
}

fn fragment(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_owned()).collect()
}

/// The built-in tables, in the same shape a config `launch` table has.
pub fn builtin_launch_table(kind: &str) -> Option<LaunchTable> {
    match kind {
        // With the Codex desktop app's shared background server running, a
        // plain `codex` stops at a dialog asking whether to restart that
        // server (seen live 2026-10-14) and pod's brief is never typed.
        // `--no-daemon` runs the session in its own process instead; it only
        // leaves the session out of `codex agents`, which pod does not use.
        "codex" => Some(LaunchTable {
            always: fragment(&["--no-daemon"]),
            model: fragment(&["-m", "{model}"]),
            effort: fragment(&["-c", "model_reasoning_effort=\"{effort}\""]),
            fast: fragment(&["-c", "service_tier=\"fast\""]),
            yolo: fragment(&["--dangerously-bypass-approvals-and-sandbox"]),
        }),
        "claude" => Some(LaunchTable {
            always: Vec::new(),
            model: fragment(&["--model", "{model}"]),
            effort: fragment(&["--effort", "{effort}"]),
            fast: Vec::new(),
            yolo: fragment(&["--dangerously-skip-permissions"]),
        }),
        _ => None,
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Preset {
    pub agent: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub fast: bool,
    pub yolo: bool,
    pub args: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct RepoConfig {
    pub base_branch: Option<String>,
    pub setup: Vec<String>,
    pub setup_policy: String,
}

impl Default for RepoConfig {
    fn default() -> Self {
        Self {
            base_branch: None,
            setup: Vec::new(),
            setup_policy: "wait-for-setup".into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Launch {
    pub kind: String,
    pub command: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub fast: bool,
    pub yolo: bool,
    pub args: Vec<String>,
    /// The table the flags came from; `None` means the built-in for `kind`.
    #[serde(skip)]
    pub table: Option<LaunchTable>,
}

pub type AgentResolution = Launch;

impl Config {
    pub fn load(path: &Path) -> Result<Config> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let source = fs::read_to_string(path).map_err(|error| {
            PodError::usage(format!("could not read {}: {error}", path.display()))
        })?;
        let mut config: Config = toml::from_str(&source).map_err(|error| {
            PodError::usage(format!("invalid config {}: {error}", path.display()))
        })?;
        config.validate()?;
        config.defaults.ask_timeout_ms = config.defaults.ask_timeout_ms.min(1_800_000);
        Ok(config)
    }

    pub fn resolve_agent(
        &self,
        name: &str,
        model: Option<&str>,
        effort: Option<&str>,
        fast: bool,
        yolo: bool,
    ) -> Result<Launch> {
        let (kind, preset_model, preset_effort, preset_fast, preset_yolo, args) =
            if let Some(preset) = self.presets.get(name) {
                (
                    preset.agent.as_str(),
                    preset.model.clone(),
                    preset.effort.clone(),
                    preset.fast,
                    preset.yolo,
                    preset.args.clone(),
                )
            } else {
                (name, None, None, false, false, Vec::new())
            };
        if !self.is_known_kind(kind) {
            return Err(PodError::new(
                ErrorCode::UnknownAgent,
                format!("{name} is not a preset, command alias, or herdr agent kind"),
                &[
                    "Use a preset or alias from config.toml, or a herdr agent kind such as codex or claude.",
                ],
            ));
        }
        let command = self
            .agents
            .get(kind)
            .map(|alias| alias.command.clone())
            .filter(|command| !command.is_empty())
            .unwrap_or_else(|| kind.to_owned());
        let table = self.launch_table(kind);
        let mapped = table.clone().unwrap_or_default();
        // (flag, requested, came from the preset, mapped, snippet to add)
        let wanted = [
            (
                "model",
                model.is_some() || preset_model.is_some(),
                model.is_none() && preset_model.is_some(),
                mapped.maps_model(),
                r#"model = ["--model", "{model}"]"#,
            ),
            (
                "effort",
                effort.is_some() || preset_effort.is_some(),
                effort.is_none() && preset_effort.is_some(),
                mapped.maps_effort(),
                r#"effort = ["--effort", "{effort}"]"#,
            ),
            (
                "fast",
                fast || preset_fast,
                !fast && preset_fast,
                !mapped.fast.is_empty(),
                r#"fast = ["--fast"]"#,
            ),
            (
                "yolo",
                yolo || preset_yolo,
                !yolo && preset_yolo,
                !mapped.yolo.is_empty(),
                r#"yolo = ["--yolo"]"#,
            ),
        ];
        if let Some((what, _, from_preset, _, snippet)) = wanted
            .iter()
            .find(|(_, requested, _, mapped, _)| *requested && !mapped)
        {
            let remove = if *from_preset {
                format!("Remove {what} from preset {name} in config.toml.")
            } else {
                format!("Remove --{what}.")
            };
            return Err(PodError::new(
                ErrorCode::LaunchMappingUnsupported,
                format!("{kind} has no launch mapping for {what}"),
                &[
                    &remove,
                    &format!(
                        "Or add `launch = {{ {snippet} }}` under [agents.{kind}] in config.toml, with the agent's own flag in place of --{what}."
                    ),
                    "Or pass the agent's own flags through the preset's args.",
                ],
            ));
        }
        Ok(Launch {
            kind: kind.to_owned(),
            command,
            model: model.map(str::to_owned).or(preset_model),
            effort: effort.map(str::to_owned).or(preset_effort),
            fast: fast || preset_fast,
            yolo: yolo || preset_yolo,
            args,
            table,
        })
    }

    /// The launch table for `kind`: the built-in for `codex` and `claude`
    /// (empty for any other kind) with the config's `launch` keys over it.
    pub fn launch_table(&self, kind: &str) -> Option<LaunchTable> {
        let builtin = builtin_launch_table(kind);
        match self
            .agents
            .get(kind)
            .and_then(|alias| alias.launch.as_ref())
        {
            Some(overrides) => Some(overrides.over(builtin.unwrap_or_default())),
            None => builtin,
        }
    }

    /// A herdr kind, or a kind with an `[agents.<kind>]` entry.
    fn is_known_kind(&self, kind: &str) -> bool {
        is_builtin_kind(kind) || self.agents.contains_key(kind)
    }

    fn validate(&self) -> Result<()> {
        if self.defaults.heartbeat_minutes == 0 || self.defaults.ask_timeout_ms == 0 {
            return Err(PodError::usage(
                "heartbeat_minutes and ask_timeout_ms must be at least 1",
            ));
        }
        for (kind, alias) in &self.agents {
            // An empty command launches the kind's own name, which the
            // wrapper ban covers too: `[agents.sh]` is `sh`.
            let command = if alias.command.is_empty() {
                kind
            } else {
                &alias.command
            };
            validate_command(kind, command)?;
            let table = self.launch_table(kind).unwrap_or_default();
            for (what, given, mapped) in [
                ("model", &table.model, table.maps_model()),
                ("effort", &table.effort, table.maps_effort()),
            ] {
                if !given.is_empty() && !mapped {
                    return Err(PodError::usage(format!(
                        "launch.{what} for agent {kind} must use {{{what}}}, or be [] for unsupported"
                    )));
                }
            }
        }
        for (name, preset) in &self.presets {
            if !self.is_known_kind(&preset.agent) {
                return Err(PodError::usage(format!(
                    "preset {name} names an unknown agent kind"
                )));
            }
        }
        for (path, repo) in &self.repos {
            if !path.is_absolute() {
                return Err(PodError::usage(format!(
                    "repository config key must be absolute: {}",
                    path.display()
                )));
            }
            if !matches!(
                repo.setup_policy.as_str(),
                "wait-for-setup" | "start-immediately"
            ) {
                return Err(PodError::usage(format!(
                    "invalid setup_policy for {}",
                    path.display()
                )));
            }
            if repo.setup.first().is_some_and(|program| program.is_empty()) {
                return Err(PodError::usage(format!(
                    "setup executable is empty for {}",
                    path.display()
                )));
            }
        }
        Ok(())
    }
}

/// herdr's agent kinds: the `IntegrationTarget` enumeration of
/// `vendor/herdr/schema-22.json`. Only `codex` and `claude` have built-in launch tables.
pub const HERDR_AGENT_KINDS: &[&str] = &[
    "pi",
    "omp",
    "claude",
    "codex",
    "copilot",
    "devin",
    "droid",
    "kimi",
    "opencode",
    "kilo",
    "hermes",
    "qodercli",
    "qwen",
    "cursor",
    "mastracode",
    "antigravity_cli",
    "grok",
];

fn is_builtin_kind(kind: &str) -> bool {
    HERDR_AGENT_KINDS.contains(&kind)
}

/// Orca's alias rule (spec section 4): one executable token or an absolute
/// path, with no interpreter arguments, environment assignments, or shell
/// wrappers. Whitespace and `=` are refused in every form, so an absolute
/// `/usr/bin/env FOO=1 codex` cannot pass as a path.
fn validate_command(kind: &str, command: &str) -> Result<()> {
    let basename = Path::new(command)
        .file_name()
        .and_then(|part| part.to_str())
        .unwrap_or(command);
    let wrapper = matches!(
        basename,
        "sh" | "bash" | "zsh" | "fish" | "dash" | "ksh" | "csh" | "tcsh" | "env" | "sudo"
    );
    let not_one_token = command.chars().any(char::is_whitespace) || command.contains('=');
    if command.is_empty() || not_one_token || wrapper {
        return Err(PodError::usage(format!(
            "agent alias {kind} must be one executable token or absolute path"
        )));
    }
    Ok(())
}

pub fn launch_argv(launch: &Launch) -> Vec<String> {
    let table = launch
        .table
        .clone()
        .or_else(|| builtin_launch_table(&launch.kind))
        .unwrap_or_default();
    let mut argv = vec![launch.command.clone()];
    // A preset that already passes one of them in `args` keeps it there
    // once; a repeated flag is a usage error to the agent.
    argv.extend(
        table
            .always
            .iter()
            .filter(|part| !launch.args.contains(part))
            .cloned(),
    );
    if let Some(model) = &launch.model {
        argv.extend(
            table
                .model
                .iter()
                .map(|part| part.replace("{model}", model)),
        );
    }
    if let Some(effort) = &launch.effort {
        argv.extend(
            table
                .effort
                .iter()
                .map(|part| part.replace("{effort}", effort)),
        );
    }
    if launch.fast {
        argv.extend(table.fast);
    }
    if launch.yolo {
        argv.extend(table.yolo);
    }
    argv.extend(launch.args.clone());
    argv
}
