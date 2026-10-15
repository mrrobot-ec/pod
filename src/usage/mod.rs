//! Account usage for the agents pod runs: Claude Code's rate-limit windows
//! and session cost, received through its status-line hook, and Codex's
//! rate-limit windows and credits, read live from `codex app-server`.
//! `pod usage` folds both into one line for herdr's tab bar.

mod codex;
mod install;

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{PodError, Result};

pub use codex::{codex_from_rate_limits, read_codex};
pub use install::{
    InstallOutcome, claude_statusline_installed, herdr_bar_installed, herdr_bar_without_tick,
    install_claude_statusline, install_herdr_bar,
};

/// Data older than this shows its age on the bar.
const FRESH_SECS: i64 = 60;

/// One rate-limit window: `5h`, `7d`, or `wk`.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Window {
    pub label: String,
    pub used_percent: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<i64>,
}

impl Window {
    fn from_value(label: &str, value: &Value, percent_key: &str) -> Option<Self> {
        let used_percent = value.get(percent_key)?.as_f64()?;
        Some(Self {
            label: label.to_owned(),
            used_percent,
            resets_at: value
                .get("resets_at")
                .or_else(|| value.get("resetsAt"))
                .and_then(Value::as_i64),
        })
    }

    fn render(&self) -> String {
        format!("{} {}%", self.label, self.used_percent.round() as i64)
    }
}

/// The newest Claude Code status-line payload that carried rate limits.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ClaudeUsage {
    pub recorded_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_used_percent: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub five_hour: Option<Window>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seven_day: Option<Window>,
}

/// Codex's account rate limits, as `account/rateLimits/read` reports them.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CodexUsage {
    pub recorded_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// The long window, weekly on current plans.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary: Option<Window>,
    /// The short window, five hours on current plans.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secondary: Option<Window>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credits: Option<f64>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Snapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude: Option<ClaudeUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex: Option<CodexUsage>,
}

/// Parses a Claude Code status-line payload. Payloads without `rate_limits`
/// (the first ticks of a session) are rejected so the last good record stays.
pub fn claude_from_statusline(payload: &str, now: DateTime<Utc>) -> Result<ClaudeUsage> {
    let value = statusline_value(payload)?;
    if value.get("rate_limits").is_none() {
        return Err(PodError::usage(
            "status-line payload carries no rate_limits; nothing recorded",
        ));
    }
    Ok(statusline_record(&value, now))
}

/// Parses a status-line payload for its session cost alone: `rate_limits` is
/// optional (API-key sessions never carry it), a `session_id` is required.
pub fn claude_session_from_statusline(payload: &str, now: DateTime<Utc>) -> Result<ClaudeUsage> {
    let value = statusline_value(payload)?;
    let record = statusline_record(&value, now);
    if record.session_id.is_none() {
        return Err(PodError::usage(
            "status-line payload carries no session_id; nothing recorded",
        ));
    }
    Ok(record)
}

fn statusline_value(payload: &str) -> Result<Value> {
    serde_json::from_str(payload)
        .map_err(|error| PodError::usage(format!("status-line payload is not JSON: {error}")))
}

fn statusline_record(value: &Value, now: DateTime<Utc>) -> ClaudeUsage {
    let string = |pointer: &str| {
        value
            .pointer(pointer)
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    let number = |pointer: &str| value.pointer(pointer).and_then(Value::as_f64);
    let window = |key: &str, label: &str| {
        value
            .pointer(&format!("/rate_limits/{key}"))
            .and_then(|window| Window::from_value(label, window, "used_percentage"))
    };
    ClaudeUsage {
        recorded_at: now,
        session_id: string("/session_id"),
        model: string("/model/display_name"),
        context_used_percent: number("/context_window/used_percentage"),
        cost_usd: number("/cost/total_cost_usd"),
        five_hour: window("five_hour", "5h"),
        seven_day: window("seven_day", "7d"),
    }
}

/// The tab bar entry, chevron at the right edge like herdr's sidebar: the
/// line then `»` while expanded (collapses rightwards), `«` while collapsed.
pub fn bar_entry(snapshot: &Snapshot, now: DateTime<Utc>, collapsed: bool) -> String {
    if collapsed {
        "«".to_owned()
    } else {
        format!("{} »", bar_line(snapshot, now))
    }
}

/// The one-line summary for herdr's tab bar.
pub fn bar_line(snapshot: &Snapshot, now: DateTime<Utc>) -> String {
    let claude = match &snapshot.claude {
        None => "claude: no data (pod usage --install)".to_owned(),
        Some(claude) => {
            let mut parts: Vec<String> = claude
                .five_hour
                .iter()
                .chain(claude.seven_day.iter())
                .map(Window::render)
                .collect();
            if let Some(cost) = claude.cost_usd {
                parts.push(format!("${cost:.2}"));
            }
            format!(
                "claude {}{}",
                parts.join(" · "),
                age(claude.recorded_at, now)
            )
        }
    };
    let codex = match &snapshot.codex {
        None => "codex: no data".to_owned(),
        Some(codex) => {
            let mut parts: Vec<String> = codex
                .secondary
                .iter()
                .chain(codex.primary.iter())
                .map(Window::render)
                .collect();
            if let Some(credits) = codex.credits {
                parts.push(format!("{} credits", compact(credits)));
            }
            format!("codex {}{}", parts.join(" · "), age(codex.recorded_at, now))
        }
    };
    format!("{claude} | {codex}")
}

fn age(recorded_at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let seconds = (now - recorded_at).num_seconds();
    if seconds <= FRESH_SECS {
        String::new()
    } else if seconds < 3600 {
        format!(" ({}m ago)", seconds / 60)
    } else {
        format!(" ({}h ago)", seconds / 3600)
    }
}

fn compact(value: f64) -> String {
    if value >= 1000.0 {
        format!("{:.1}k", value / 1000.0)
    } else {
        format!("{value:.0}")
    }
}

/// `<state dir>/usage`: `claude.json`, `codex.json`, and `sessions/<id>.json`.
pub struct UsageStore {
    dir: PathBuf,
}

impl UsageStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn load_claude(&self) -> Result<Option<ClaudeUsage>> {
        self.load("claude.json")
    }

    /// Keeps the newest record: status lines from several panes race.
    pub fn save_claude(&self, claude: &ClaudeUsage) -> Result<()> {
        if let Some(current) = self.load_claude()?
            && current.recorded_at > claude.recorded_at
        {
            return Ok(());
        }
        self.save("claude.json", claude)
    }

    /// Stores a status-line record by its `session_id`, keeping the newest.
    /// A record without a usable id is skipped: the id becomes a file name.
    pub fn save_session(&self, claude: &ClaudeUsage) -> Result<()> {
        let Some(id) = claude.session_id.as_deref().filter(|id| session_id_ok(id)) else {
            return Ok(());
        };
        let sessions = self.sessions();
        let name = format!("{id}.json");
        if let Some(current) = sessions.load::<ClaudeUsage>(&name)?
            && current.recorded_at > claude.recorded_at
        {
            return Ok(());
        }
        sessions.save(&name, claude)
    }

    /// The newest status-line record of one Claude session, if any. An
    /// unreadable record is the same as none: cost is informational.
    pub fn load_session(&self, session_id: &str) -> Option<ClaudeUsage> {
        if !session_id_ok(session_id) {
            return None;
        }
        self.sessions()
            .load(&format!("{session_id}.json"))
            .ok()
            .flatten()
    }

    fn sessions(&self) -> UsageStore {
        UsageStore::new(self.dir.join("sessions"))
    }

    /// Whether the bar entry is collapsed (`collapsed` marker file present).
    pub fn collapsed(&self) -> Result<bool> {
        Ok(self.dir.join("collapsed").exists())
    }

    /// Flips the collapsed marker and returns the new state.
    pub fn toggle_collapsed(&self) -> Result<bool> {
        let marker = self.dir.join("collapsed");
        if marker.exists() {
            std::fs::remove_file(&marker).map_err(|error| {
                PodError::store(format!("could not remove {}: {error}", marker.display()))
            })?;
            Ok(false)
        } else {
            create_private_dir(&self.dir)?;
            std::fs::write(&marker, b"").map_err(|error| {
                PodError::store(format!("could not write {}: {error}", marker.display()))
            })?;
            Ok(true)
        }
    }

    pub fn load_codex(&self) -> Result<Option<CodexUsage>> {
        self.load("codex.json")
    }

    pub fn save_codex(&self, codex: &CodexUsage) -> Result<()> {
        self.save("codex.json", codex)
    }

    fn load<T: for<'de> Deserialize<'de>>(&self, name: &str) -> Result<Option<T>> {
        let path = self.dir.join(name);
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).map(Some).map_err(|error| {
                PodError::store(format!("{} is not a usage record: {error}", path.display()))
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(PodError::store(format!(
                "could not read {}: {error}",
                path.display()
            ))),
        }
    }

    fn save<T: Serialize>(&self, name: &str, value: &T) -> Result<()> {
        create_private_dir(&self.dir)?;
        let path = self.dir.join(name);
        let tmp = self.dir.join(format!("{name}.{}.tmp", std::process::id()));
        let text = serde_json::to_string(value)
            .map_err(|error| PodError::store(format!("could not encode {name}: {error}")))?;
        std::fs::write(&tmp, text)
            .and_then(|()| std::fs::rename(&tmp, &path))
            .map_err(|error| {
                PodError::store(format!("could not write {}: {error}", path.display()))
            })
    }
}

/// Session ids are UUIDs; anything else could name a path outside `sessions/`.
fn session_id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn create_private_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)
        .map_err(|error| PodError::store(format!("could not create {}: {error}", dir.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}
