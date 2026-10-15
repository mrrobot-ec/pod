//! Folder trust for an unattended (`--yolo`) worker. An agent launched in a
//! folder it has not seen stops at its own trust prompt before it ever
//! reaches its input line, which keeps the preamble from being typed (seen
//! live on Codex 0.160.1 in a new worktree, 2026-10-13; Claude Code's trust
//! dialog behaves the same). pod never answers that prompt. Instead, before
//! the launch, it records the trust in the agent's own config, exactly as
//! the agent itself would after a person chose "trust":
//!
//! - Codex: `[projects."<path>"] trust_level = "trusted"` in
//!   `$CODEX_HOME/config.toml` (Codex keys a linked worktree on the
//!   repository it came from, so that root is trusted as well).
//! - Claude Code: `projects["<path>"].hasTrustDialogAccepted = true` in
//!   `~/.claude.json`.
//!
//! Only `--yolo` launches do this: trusting a folder is part of running a
//! worker unattended. Files are rewritten atomically, with their mode kept,
//! and an existing entry is left alone. A file pod cannot read or parse is
//! not touched; the launch proceeds and may still stop at the prompt.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::{PodError, Result};

/// Records trust for `folders` in the config of agent `kind`. Returns the
/// folders newly added (already-trusted ones are skipped).
pub fn ensure(kind: &str, config: &Path, folders: &[PathBuf]) -> Result<Vec<PathBuf>> {
    match kind {
        "codex" => ensure_codex(config, folders),
        "claude" => ensure_claude(config, folders),
        _ => Ok(Vec::new()),
    }
}

fn ensure_codex(config: &Path, folders: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let existing = match fs::read_to_string(config) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(unreadable(config, &error.to_string())),
    };
    let table: toml::Table = toml::from_str(&existing)
        .map_err(|error| unreadable(config, &format!("not valid TOML: {error}")))?;
    let trusted = |path: &str| {
        table
            .get("projects")
            .and_then(toml::Value::as_table)
            .and_then(|projects| projects.get(path))
            .and_then(toml::Value::as_table)
            .and_then(|project| project.get("trust_level"))
            .and_then(toml::Value::as_str)
            == Some("trusted")
    };
    let mut added = Vec::new();
    let mut text = existing.clone();
    for folder in folders {
        let key = folder.display().to_string();
        if trusted(&key) || added.contains(folder) {
            continue;
        }
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&format!(
            "\n[projects.{}]\ntrust_level = \"trusted\"\n",
            toml_quoted_key(&key)
        ));
        added.push(folder.clone());
    }
    if !added.is_empty() {
        write_atomically(config, text.as_bytes())?;
    }
    Ok(added)
}

fn ensure_claude(config: &Path, folders: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let existing = match fs::read_to_string(config) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "{}".into(),
        Err(error) => return Err(unreadable(config, &error.to_string())),
    };
    let mut root: serde_json::Value = serde_json::from_str(&existing)
        .map_err(|error| unreadable(config, &format!("not valid JSON: {error}")))?;
    let Some(object) = root.as_object_mut() else {
        return Err(unreadable(config, "the top level is not an object"));
    };
    let projects = object
        .entry("projects")
        .or_insert_with(|| serde_json::Value::Object(Default::default()));
    let Some(projects) = projects.as_object_mut() else {
        return Err(unreadable(config, "`projects` is not an object"));
    };
    let mut added = Vec::new();
    for folder in folders {
        let key = folder.display().to_string();
        let project = projects
            .entry(key)
            .or_insert_with(|| serde_json::Value::Object(Default::default()));
        let Some(project) = project.as_object_mut() else {
            continue;
        };
        if project.get("hasTrustDialogAccepted") == Some(&serde_json::Value::Bool(true)) {
            continue;
        }
        project.insert(
            "hasTrustDialogAccepted".into(),
            serde_json::Value::Bool(true),
        );
        added.push(folder.clone());
    }
    if !added.is_empty() {
        let text = serde_json::to_string_pretty(&root).map_err(|error| {
            PodError::store(format!("could not serialize {}: {error}", config.display()))
        })?;
        write_atomically(config, text.as_bytes())?;
    }
    Ok(added)
}

/// A TOML quoted key for a path (`"..."`, with backslashes and quotes escaped).
pub fn toml_quoted_key(path: &str) -> String {
    let escaped = path.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn unreadable(config: &Path, detail: &str) -> PodError {
    PodError::usage(format!(
        "agent config {} could not be updated for folder trust: {detail}",
        config.display()
    ))
}

/// Temporary file beside `path`, then rename; the existing mode is kept
/// (a new file is 0600).
fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    let io = |error: std::io::Error| {
        PodError::store(format!("could not write {}: {error}", path.display()))
    };
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(io)?;
    let mode = fs::metadata(path).ok().map(|meta| meta.permissions());
    let temp = parent.join(format!(
        ".{}.pod-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("config"),
        std::process::id()
    ));
    {
        let mut file = fs::File::create(&temp).map_err(io)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let permissions = mode
                .clone()
                .unwrap_or_else(|| fs::Permissions::from_mode(0o600));
            fs::set_permissions(&temp, permissions).map_err(io)?;
        }
        file.write_all(bytes).map_err(io)?;
        file.sync_all().map_err(io)?;
    }
    fs::rename(&temp, path).map_err(io)?;
    Ok(())
}
