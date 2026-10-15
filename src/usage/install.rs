//! Wiring: Claude Code's `statusLine` in `settings.json` and herdr's
//! `[ui] tab_bar_right` command entry in `config.toml`. Both are idempotent.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Map, Value, json};
use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, value};

use crate::error::{PodError, Result};

#[derive(Clone, Debug, Default, Serialize)]
pub struct InstallOutcome {
    pub changed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup: Option<PathBuf>,
}

pub fn claude_statusline_installed(settings: &Path) -> bool {
    read_settings(settings).is_ok_and(|settings| {
        settings
            .pointer("/statusLine/command")
            .and_then(Value::as_str)
            .is_some_and(|command| command.contains("pod usage"))
    })
}

/// Sets `statusLine` to `command`, backing up a settings file whose status
/// line pointed elsewhere.
pub fn install_claude_statusline(settings: &Path, command: &str) -> Result<InstallOutcome> {
    let mut current = read_settings(settings)?;
    let wanted = json!({"type": "command", "command": command});
    if current.get("statusLine") == Some(&wanted) {
        return Ok(InstallOutcome::default());
    }
    let backup = if settings.exists() {
        let backup = settings.with_file_name(format!(
            "{}.pod-backup-{}",
            settings
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("settings.json"),
            chrono::Utc::now().format("%Y%m%d%H%M%S")
        ));
        std::fs::copy(settings, &backup).map_err(|error| io_error(&backup, error))?;
        Some(backup)
    } else {
        None
    };
    current
        .as_object_mut()
        .expect("settings object")
        .insert("statusLine".into(), wanted);
    let text = serde_json::to_string_pretty(&current)
        .map_err(|error| PodError::store(format!("could not encode settings: {error}")))?;
    if let Some(parent) = settings.parent() {
        std::fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;
    }
    std::fs::write(settings, text + "\n").map_err(|error| io_error(settings, error))?;
    Ok(InstallOutcome {
        changed: true,
        backup,
    })
}

fn read_settings(settings: &Path) -> Result<Value> {
    match std::fs::read_to_string(settings) {
        Ok(text) if text.trim().is_empty() => Ok(Value::Object(Map::new())),
        Ok(text) => {
            let value: Value = serde_json::from_str(&text).map_err(|error| {
                PodError::usage(format!("{} is not JSON: {error}", settings.display()))
            })?;
            if value.is_object() {
                Ok(value)
            } else {
                Err(PodError::usage(format!(
                    "{} is not a JSON object",
                    settings.display()
                )))
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Value::Object(Map::new())),
        Err(error) => Err(io_error(settings, error)),
    }
}

pub fn herdr_bar_installed(config: &Path) -> bool {
    read_config(config).is_ok_and(|document| bar_entry_present(&document, "pod usage"))
}

/// Adds a `pod usage` command entry to the tab bar's right side and a
/// `prefix+u` binding that toggles it. The bar's position is the person's
/// choice and is left alone.
pub fn install_herdr_bar(config: &Path, command: &str) -> Result<InstallOutcome> {
    let mut document = read_config(config)?;
    let mut changed = false;
    if !document.contains_table("ui") {
        document["ui"] = Item::Table(Table::new());
        changed = true;
    }
    let ui = document["ui"]
        .as_table_mut()
        .ok_or_else(|| PodError::usage(format!("[ui] in {} is not a table", config.display())))?;
    if !ui.contains_key("tab_bar_right") {
        ui["tab_bar_right"] = value(Array::new());
    }
    let entries = ui["tab_bar_right"].as_array_mut().ok_or_else(|| {
        PodError::usage(format!(
            "ui.tab_bar_right in {} is not an array",
            config.display()
        ))
    })?;
    // `command` may carry flags (`pod usage --tick`); the toggle binding and
    // an entry from before the flag are named by the bare command.
    let bare = command.strip_suffix(" --tick").unwrap_or(command);
    let mut present = false;
    for entry in entries.iter_mut() {
        let Some(table) = entry.as_inline_table_mut() else {
            continue;
        };
        match table.get("command").and_then(|item| item.as_str()) {
            Some(existing) if existing.contains(command) => present = true,
            Some(existing) if existing == bare => {
                table.insert("command", command.into());
                present = true;
                changed = true;
            }
            _ => {}
        }
    }
    if !present {
        let mut entry = InlineTable::new();
        entry.insert("type", "command".into());
        entry.insert("command", command.into());
        entry.insert("interval_seconds", 1.into());
        entry.insert("timeout_seconds", 5.into());
        entries.push(entry);
        changed = true;
    }
    let toggle = format!("{bare} --toggle");
    if !toggle_binding_present(&document, &toggle) {
        if !document.contains_table("keys") {
            document["keys"] = Item::Table(Table::new());
        }
        let keys = document["keys"].as_table_mut().ok_or_else(|| {
            PodError::usage(format!("[keys] in {} is not a table", config.display()))
        })?;
        if !keys.contains_key("command") {
            keys["command"] = Item::ArrayOfTables(ArrayOfTables::new());
        }
        let bindings = keys["command"].as_array_of_tables_mut().ok_or_else(|| {
            PodError::usage(format!(
                "keys.command in {} is not an array of tables",
                config.display()
            ))
        })?;
        let mut binding = Table::new();
        binding["key"] = value("prefix+u");
        binding["type"] = value("shell");
        binding["command"] = value(toggle.as_str());
        binding["description"] = value("toggle usage on the tab bar");
        bindings.push(binding);
        changed = true;
    }
    if changed {
        if let Some(parent) = config.parent() {
            std::fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;
        }
        std::fs::write(config, document.to_string()).map_err(|error| io_error(config, error))?;
    }
    Ok(InstallOutcome {
        changed,
        backup: None,
    })
}

fn toggle_binding_present(document: &DocumentMut, toggle: &str) -> bool {
    document
        .get("keys")
        .and_then(Item::as_table)
        .and_then(|keys| keys.get("command"))
        .and_then(Item::as_array_of_tables)
        .is_some_and(|bindings| {
            bindings.iter().any(|binding| {
                binding
                    .get("command")
                    .and_then(Item::as_str)
                    .is_some_and(|command| command == toggle)
            })
        })
}

fn bar_entry_present(document: &DocumentMut, command: &str) -> bool {
    document
        .get("ui")
        .and_then(Item::as_table)
        .and_then(|ui| ui.get("tab_bar_right"))
        .and_then(Item::as_array)
        .is_some_and(|entries| {
            entries.iter().any(|entry| {
                entry
                    .as_inline_table()
                    .and_then(|table| table.get("command"))
                    .and_then(|item| item.as_str())
                    .is_some_and(|existing| existing.contains(command))
            })
        })
}

fn read_config(config: &Path) -> Result<DocumentMut> {
    match std::fs::read_to_string(config) {
        Ok(text) => text.parse::<DocumentMut>().map_err(|error| {
            PodError::usage(format!("{} is not valid TOML: {error}", config.display()))
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(DocumentMut::new()),
        Err(error) => Err(io_error(config, error)),
    }
}

fn io_error(path: &Path, error: std::io::Error) -> PodError {
    PodError::store(format!("{}: {error}", path.display()))
}
