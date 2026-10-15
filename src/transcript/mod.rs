use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Result;

pub mod claude;
pub mod codex;
pub mod cursor;
pub mod grok;
pub mod screen;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub role: String,
    pub kind: String,
    pub text: String,
    pub tool: Option<String>,
    pub at: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Page {
    pub rows: Vec<Row>,
    pub cursor: Option<String>,
    pub content_complete: bool,
    pub clipping: bool,
    pub warnings: Vec<String>,
}

pub trait Reader {
    fn kind(&self) -> &'static str;
    fn locate(
        &self,
        session_id: &str,
        session_path: Option<&Path>,
        cwd: &Path,
    ) -> Result<Option<PathBuf>>;
    fn read(&self, source: &Path, cursor: Option<&str>, limit: u32) -> Result<Page>;
}

pub fn reader_for(kind: &str) -> Option<Box<dyn Reader>> {
    match kind {
        "claude" => Some(Box::new(claude::ClaudeReader::new())),
        "codex" => Some(Box::new(codex::CodexReader::new())),
        "grok" => Some(Box::new(grok::GrokReader::new())),
        "cursor" => Some(Box::new(cursor::CursorReader::new())),
        _ => None,
    }
}
