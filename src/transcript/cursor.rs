//! Cursor Agent (best-effort):
//! `~/.cursor/projects/<slug>/agent-transcripts/<session id>/<session id>.jsonl`.
//! Returns `Ok(None)` from `locate` when no such store exists (`reader_unavailable`).

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::screen::{home_dir, json_lines, proven_path, valid_id};
use super::{Page, Reader, Row};
use crate::error::Result;

#[derive(Clone, Debug)]
pub struct CursorReader {
    home: PathBuf,
}

impl CursorReader {
    pub fn new() -> Self {
        Self::with_home(home_dir())
    }

    pub fn with_home(home: impl Into<PathBuf>) -> Self {
        Self { home: home.into() }
    }
}

impl Default for CursorReader {
    fn default() -> Self {
        Self::new()
    }
}

fn row(role: &str, kind: &str, text: String, tool: Option<&str>) -> Row {
    Row {
        role: role.into(),
        kind: kind.into(),
        text,
        tool: tool.map(str::to_owned),
        at: None, // transcript lines carry no timestamp
    }
}

/// User text is `[<timestamp>…</timestamp>\n]<user_query>…</user_query>`;
/// only the query is what the user typed.
fn user_query(text: &str) -> &str {
    let query = text
        .split_once("<user_query>")
        .and_then(|(_, rest)| rest.rsplit_once("</user_query>"))
        .map_or(text, |(query, _)| query);
    query.trim()
}

fn parse(line: &Value) -> Vec<Row> {
    let role = match line.get("role").and_then(Value::as_str) {
        Some(r @ ("user" | "assistant")) => r,
        _ => return Vec::new(), // also `{"type":"turn_ended"}`
    };
    let blocks = line
        .pointer("/message/content")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut rows = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                let text = block
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let text = if role == "user" {
                    user_query(text)
                } else {
                    text.trim()
                };
                if !text.is_empty() {
                    rows.push(row(role, "message", text.to_owned(), None));
                }
            }
            // Cursor records the call but not its output.
            Some("tool_use") => {
                let input = block.get("input").map(Value::to_string).unwrap_or_default();
                let name = block.get("name").and_then(Value::as_str);
                rows.push(row("assistant", "tool_call", input, name));
            }
            _ => {}
        }
    }
    rows
}

impl Reader for CursorReader {
    fn kind(&self) -> &'static str {
        "cursor"
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
        let root = self.home.join(".cursor/projects");
        Ok(fs::read_dir(root)
            .into_iter()
            .flatten()
            .flatten()
            .map(|project| {
                project
                    .path()
                    .join("agent-transcripts")
                    .join(session_id)
                    .join(format!("{session_id}.jsonl"))
            })
            .find(|p| p.is_file()))
    }

    fn read(&self, source: &Path, cursor: Option<&str>, limit: u32) -> Result<Page> {
        json_lines(source, cursor, limit, parse)
    }
}
