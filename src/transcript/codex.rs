//! Codex: `~/.codex/sessions/YYYY/MM/DD/rollout-<timestamp>-<session id>.jsonl`.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::screen::{home_dir, json_lines, proven_path, valid_id};
use super::{Page, Reader, Row};
use crate::error::Result;

#[derive(Clone, Debug)]
pub struct CodexReader {
    home: PathBuf,
}

impl CodexReader {
    pub fn new() -> Self {
        Self::with_home(home_dir())
    }

    pub fn with_home(home: impl Into<PathBuf>) -> Self {
        Self { home: home.into() }
    }
}

impl Default for CodexReader {
    fn default() -> Self {
        Self::new()
    }
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect()
}

/// The session id in `rollout-YYYY-MM-DDThh-mm-ss-<id>.jsonl`. The id is
/// everything after the fixed-width timestamp, so a tail of a UUID (whose
/// groups are also `-`-joined) never matches.
fn rollout_id(name: &str) -> Option<&str> {
    let rest = name.strip_prefix("rollout-")?.strip_suffix(".jsonl")?;
    let (stamp, id) = (rest.get(..19)?, rest.get(19..)?.strip_prefix('-')?);
    stamp.contains('T').then_some(id)
}

fn row(role: &str, kind: &str, text: String, tool: Option<&str>, at: Option<&str>) -> Row {
    Row {
        role: role.into(),
        kind: kind.into(),
        text,
        tool: tool.map(str::to_owned),
        at: at.map(str::to_owned),
    }
}

/// Text of a `message` content list or a tool `output` (string or blocks).
fn blocks_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn parse(line: &Value) -> Vec<Row> {
    if line.get("type").and_then(Value::as_str) != Some("response_item") {
        return Vec::new();
    }
    let Some(payload) = line.get("payload") else {
        return Vec::new();
    };
    let at = line.get("timestamp").and_then(Value::as_str);
    let name = payload.get("name").and_then(Value::as_str);
    match payload.get("type").and_then(Value::as_str) {
        Some("message") => {
            let role = match payload.get("role").and_then(Value::as_str) {
                Some(r @ ("user" | "assistant")) => r,
                _ => return Vec::new(),
            };
            let text = blocks_text(payload.get("content"));
            // Codex injects its environment block and the repo's AGENTS.md
            // as user messages; neither is something the user said.
            if text.is_empty()
                || text.starts_with("<environment_context>")
                || text.starts_with("# AGENTS.md instructions for ")
            {
                return Vec::new();
            }
            vec![row(role, "message", text, None, at)]
        }
        Some("custom_tool_call") => {
            let input = payload
                .get("input")
                .and_then(Value::as_str)
                .unwrap_or_default();
            vec![row("assistant", "tool_call", input.to_owned(), name, at)]
        }
        Some("function_call") => {
            let args = payload
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or_default();
            vec![row("assistant", "tool_call", args.to_owned(), name, at)]
        }
        Some("custom_tool_call_output" | "function_call_output") => {
            vec![row(
                "tool",
                "tool_result",
                blocks_text(payload.get("output")),
                None,
                at,
            )]
        }
        _ => Vec::new(),
    }
}

impl Reader for CodexReader {
    fn kind(&self) -> &'static str {
        "codex"
    }

    fn locate(
        &self,
        session_id: &str,
        session_path: Option<&Path>,
        _cwd: &Path,
    ) -> Result<Option<PathBuf>> {
        if let Some(path) = proven_path(session_path) {
            return Ok(Some(path));
        }
        if !valid_id(session_id) {
            return Ok(None);
        }
        let root = self.home.join(".codex/sessions");
        for year in subdirs(&root) {
            for month in subdirs(&year) {
                for day in subdirs(&month) {
                    let hit = fs::read_dir(&day)
                        .into_iter()
                        .flatten()
                        .flatten()
                        .find(|e| rollout_id(&e.file_name().to_string_lossy()) == Some(session_id));
                    if let Some(entry) = hit {
                        return Ok(Some(entry.path()));
                    }
                }
            }
        }
        Ok(None)
    }

    fn read(&self, source: &Path, cursor: Option<&str>, limit: u32) -> Result<Page> {
        json_lines(source, cursor, limit, parse)
    }
}
