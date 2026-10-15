//! Claude Code: `~/.claude/projects/<cwd slug>/<session id>.jsonl`.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::screen::{home_dir, json_lines, proven_path, valid_id};
use super::{Page, Reader, Row};
use crate::error::Result;

#[derive(Clone, Debug)]
pub struct ClaudeReader {
    home: PathBuf,
}

impl ClaudeReader {
    pub fn new() -> Self {
        Self::with_home(home_dir())
    }

    pub fn with_home(home: impl Into<PathBuf>) -> Self {
        Self { home: home.into() }
    }
}

impl Default for ClaudeReader {
    fn default() -> Self {
        Self::new()
    }
}

/// Claude names the project directory after the cwd with every character
/// outside `[A-Za-z0-9]` replaced by `-` (`/.worktrees` becomes `--worktrees`).
fn slug(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
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

/// `tool_result.content` is a string or a list of `{type:"text"}` blocks.
fn result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn parse(line: &Value) -> Vec<Row> {
    let role = match line.get("type").and_then(Value::as_str) {
        Some(r @ ("user" | "assistant")) => r,
        _ => return Vec::new(),
    };
    if line.get("isMeta").and_then(Value::as_bool) == Some(true) {
        return Vec::new();
    }
    let at = line.get("timestamp").and_then(Value::as_str);
    let content = line.get("message").and_then(|m| m.get("content"));
    let blocks = match content {
        Some(Value::String(text)) => return vec![row(role, "message", text.clone(), None, at)],
        Some(Value::Array(blocks)) => blocks,
        _ => return Vec::new(),
    };
    let mut rows = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                let text = block
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !text.is_empty() {
                    rows.push(row(role, "message", text.to_owned(), None, at));
                }
            }
            Some("tool_use") => {
                let input = block.get("input").map(Value::to_string).unwrap_or_default();
                let name = block.get("name").and_then(Value::as_str);
                rows.push(row("assistant", "tool_call", input, name, at));
            }
            Some("tool_result") => {
                let text = block.get("content").map(result_text).unwrap_or_default();
                rows.push(row("tool", "tool_result", text, None, at));
            }
            _ => {} // thinking and other blocks carry no transcript text
        }
    }
    rows
}

impl Reader for ClaudeReader {
    fn kind(&self) -> &'static str {
        "claude"
    }

    fn locate(
        &self,
        session_id: &str,
        session_path: Option<&Path>,
        cwd: &Path,
    ) -> Result<Option<PathBuf>> {
        if let Some(path) = proven_path(session_path) {
            return Ok(Some(path));
        }
        if !valid_id(session_id) {
            return Ok(None);
        }
        let mut slugs = vec![slug(cwd)];
        if let Ok(real) = cwd.canonicalize() {
            slugs.push(slug(&real));
        }
        Ok(slugs
            .into_iter()
            .map(|s| {
                self.home
                    .join(".claude/projects")
                    .join(s)
                    .join(format!("{session_id}.jsonl"))
            })
            .find(|p| p.is_file()))
    }

    fn read(&self, source: &Path, cursor: Option<&str>, limit: u32) -> Result<Page> {
        json_lines(source, cursor, limit, parse)
    }
}
