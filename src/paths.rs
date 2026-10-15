use std::path::{Path, PathBuf};

use crate::error::{PodError, Result};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Paths {
    pub session: String,
    pub state_dir: PathBuf,
    pub config_path: PathBuf,
    pub db_path: PathBuf,
    pub board_dir: PathBuf,
    pub signal_path: PathBuf,
    /// Codex's own config file (`$CODEX_HOME/config.toml`, default
    /// `~/.codex/config.toml`), where folder trust lives.
    pub codex_config: PathBuf,
    /// Claude Code's own state file (`~/.claude.json`), where folder trust lives.
    pub claude_config: PathBuf,
    /// Claude Code's `settings.json` (`$CLAUDE_CONFIG_DIR` or `~/.claude`),
    /// where the status-line command lives.
    pub claude_settings: PathBuf,
    /// herdr's `config.toml` (`$HERDR_CONFIG_PATH`, else
    /// `$XDG_CONFIG_HOME/herdr/config.toml`, else `~/.config/herdr/config.toml`).
    pub herdr_config: PathBuf,
}

pub fn resolve(env: &dyn Fn(&str) -> Option<String>) -> Result<Paths> {
    let env = |name: &str| env(name).filter(|value| !value.is_empty());
    let home = env("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| PodError::usage("HOME is not set"))?;
    let session = session_from_env(&env);
    let state_root = env("POD_STATE_DIR")
        .or_else(|| env("XDG_STATE_HOME").map(|path| format!("{path}/pod")))
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/state/pod"));
    let state_dir = state_root.join(&session);
    let config_path = env("POD_CONFIG")
        .map(PathBuf::from)
        .or_else(|| env("XDG_CONFIG_HOME").map(|path| PathBuf::from(path).join("pod/config.toml")))
        .unwrap_or_else(|| home.join(".config/pod/config.toml"));

    let codex_config = env("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"))
        .join("config.toml");
    let claude_config = home.join(".claude.json");
    let claude_settings = env("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".claude"))
        .join("settings.json");
    let herdr_config = env("HERDR_CONFIG_PATH")
        .map(PathBuf::from)
        .or_else(|| {
            env("XDG_CONFIG_HOME").map(|path| PathBuf::from(path).join("herdr/config.toml"))
        })
        .unwrap_or_else(|| home.join(".config/herdr/config.toml"));

    Ok(Paths {
        session,
        db_path: state_dir.join("pod.db"),
        board_dir: state_dir.join("runs"),
        signal_path: state_dir.join("pod.signal"),
        state_dir,
        config_path,
        codex_config,
        claude_config,
        claude_settings,
        herdr_config,
    })
}

/// The herdr session this process belongs to, derived from `HERDR_SOCKET_PATH`
/// (spec section 3.3). herdr names every session's socket `herdr.sock` inside
/// that session's directory: the default session lives in herdr's config
/// directory, a named session in a directory named after it. So the default
/// socket (or no socket variable at all) is `default`, a `herdr.sock` elsewhere
/// is its parent directory's name, and any other socket file is its file stem.
pub fn session_from_env(env: &dyn Fn(&str) -> Option<String>) -> String {
    let Some(socket) = env("HERDR_SOCKET_PATH").filter(|socket| !socket.is_empty()) else {
        return DEFAULT_SESSION.to_owned();
    };
    let socket = PathBuf::from(socket);
    if default_herdr_sockets(env).contains(&socket) {
        return DEFAULT_SESSION.to_owned();
    }
    let name = if socket
        .file_name()
        .is_some_and(|name| name == HERDR_SOCKET_NAME)
    {
        socket.parent().and_then(Path::file_name)
    } else {
        socket.file_stem()
    };
    name.and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && !name.starts_with('.'))
        .unwrap_or(DEFAULT_SESSION)
        .to_owned()
}

/// herdr's default-session socket: `$XDG_CONFIG_HOME/herdr/herdr.sock`, else
/// `~/.config/herdr/herdr.sock` (what `herdr session list` reports for
/// `default`). Both candidates are returned when `XDG_CONFIG_HOME` is set.
pub fn default_herdr_sockets(env: &dyn Fn(&str) -> Option<String>) -> Vec<PathBuf> {
    let mut sockets = Vec::new();
    if let Some(config) = env("XDG_CONFIG_HOME").filter(|path| !path.is_empty()) {
        sockets.push(PathBuf::from(config).join("herdr").join(HERDR_SOCKET_NAME));
    }
    if let Some(home) = env("HOME").filter(|path| !path.is_empty()) {
        sockets.push(
            PathBuf::from(home)
                .join(".config/herdr")
                .join(HERDR_SOCKET_NAME),
        );
    }
    sockets
}

const DEFAULT_SESSION: &str = "default";
const HERDR_SOCKET_NAME: &str = "herdr.sock";
