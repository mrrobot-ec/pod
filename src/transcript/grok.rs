//! Grok (best-effort): `~/.grok/sessions/<url-encoded cwd>/<session id>/chat_history.jsonl`.
//! Returns `Ok(None)` from `locate` when no such store exists (`reader_unavailable`).

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::screen::{home_dir, json_lines, proven_path, valid_id};
use super::{Page, Reader, Row};
use crate::error::Result;

#[derive(Clone, Debug)]
pub struct GrokReader {
    home: PathBuf,
}

impl GrokReader {
    pub fn new() -> Self {
        Self::with_home(home_dir())
    }

    pub fn with_home(home: impl Into<PathBuf>) -> Self {
        Self { home: home.into() }
    }
}

impl Default for GrokReader {
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
        at: None, // chat_history.jsonl lines carry no timestamp
    }
}

fn user_text(content: &Value) -> String {
    let joined = content
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();
    joined
        .trim()
        .trim_start_matches("<user_query>")
        .trim_end_matches("</user_query>")
        .trim()
        .to_owned()
}

fn parse(line: &Value) -> Vec<Row> {
    match line.get("type").and_then(Value::as_str) {
        // Harness-injected context (system reminders, user_info) has a
        // synthetic_reason or a plain-string body; real prompts have a prompt_index.
        Some("user") if line.get("prompt_index").is_some() => {
            let text = user_text(&line["content"]);
            if text.is_empty() {
                Vec::new()
            } else {
                vec![row("user", "message", text, None)]
            }
        }
        Some("assistant") => {
            let mut rows = Vec::new();
            if let Some(text) = line
                .get("content")
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty())
            {
                rows.push(row("assistant", "message", text.to_owned(), None));
            }
            for call in line
                .get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let args = call
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let name = call.get("name").and_then(Value::as_str);
                rows.push(row("assistant", "tool_call", args.to_owned(), name));
            }
            rows
        }
        Some("tool_result") => {
            let text = line
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default();
            vec![row("tool", "tool_result", text.to_owned(), None)]
        }
        _ => Vec::new(), // system, reasoning
    }
}

impl Reader for GrokReader {
    fn kind(&self) -> &'static str {
        "grok"
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
        let root = self.home.join(".grok/sessions");
        Ok(fs::read_dir(root)
            .into_iter()
            .flatten()
            .flatten()
            .map(|cwd_dir| cwd_dir.path().join(session_id).join("chat_history.jsonl"))
            .find(|p| p.is_file()))
    }

    fn read(&self, source: &Path, cursor: Option<&str>, limit: u32) -> Result<Page> {
        json_lines(source, cursor, limit, parse)
    }
}
