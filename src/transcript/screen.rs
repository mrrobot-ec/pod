//! Plain-text screen reader, plus the paging engine every reader shares.
//!
//! The shared helpers live here because `mod.rs` is limited to the registry
//! body; each agent reader only supplies a line parser.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{Page, Reader, Row};
use crate::error::{ErrorCode, PodError, Result};

/// Tool output longer than this many characters is trimmed.
pub const TOOL_OUTPUT_LIMIT: usize = 4000;

pub(crate) fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// Session ids are joined into paths, so only plain id characters pass.
pub(crate) fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A herdr-reported session path is proof by itself, if the file exists.
pub(crate) fn proven_path(session_path: Option<&Path>) -> Option<PathBuf> {
    session_path.filter(|p| p.is_file()).map(Path::to_path_buf)
}

fn source_hash(source: &Path) -> String {
    let digest = Sha256::digest(source.to_string_lossy().as_bytes());
    hex::encode(&digest[..8])
}

fn source_changed(detail: &str) -> PodError {
    PodError::new(
        ErrorCode::SourceChanged,
        format!("the cursor does not belong to this transcript source: {detail}"),
        &["Restart reading without --cursor, or force the original source with --source."],
    )
}

fn clip(text: &mut String) -> bool {
    match text.char_indices().nth(TOOL_OUTPUT_LIMIT) {
        Some((byte, _)) => {
            text.truncate(byte);
            true
        }
        None => false,
    }
}

/// A cursor offset is valid only on a line boundary: just after a newline,
/// or at the end of an unterminated last line that was already consumed
/// (the next record then starts with a newline, or none has been written).
/// Anything else means the file was rewritten under the cursor.
fn at_line_boundary(file: &mut File, start: u64, len: u64) -> std::io::Result<bool> {
    if start == 0 {
        return Ok(true);
    }
    let mut byte = [0u8; 1];
    file.seek(SeekFrom::Start(start - 1))?;
    file.read_exact(&mut byte)?;
    if byte[0] == b'\n' || start == len {
        return Ok(true);
    }
    file.read_exact(&mut byte)?;
    Ok(byte[0] == b'\n')
}

/// Pages lines from `source`. `parse` returns the rows for one line (empty to
/// skip it) or `None` when the line is malformed. `limit` counts rows and is
/// checked only between lines, so a line is never split. A last line without
/// a newline is consumed only if `parse` accepts it, because some writers
/// (Cursor) never end the file with a newline; otherwise it is left for the
/// next read as a write in progress. The file is streamed, never loaded whole.
pub(crate) fn read_lines(
    source: &Path,
    cursor: Option<&str>,
    limit: u32,
    parse: impl Fn(&str) -> Option<Vec<Row>>,
) -> Result<Page> {
    let hash = source_hash(source);
    let start = match cursor {
        None => 0,
        Some(raw) => {
            let (h, off) = raw.split_once(':').ok_or_else(|| {
                PodError::usage("cursor must look like <source-hash>:<byte offset>")
            })?;
            if h != hash {
                return Err(source_changed("it was issued for another file"));
            }
            off.parse::<u64>()
                .map_err(|_| PodError::usage("cursor offset is not a number"))?
        }
    };
    let io = |e: std::io::Error| source_changed(&e.to_string());
    let mut file = File::open(source).map_err(|e| source_changed(&format!("cannot open: {e}")))?;
    let len = file.metadata().map_err(io)?.len();
    if start > len {
        return Err(source_changed("the file is shorter than the cursor"));
    }
    if !at_line_boundary(&mut file, start, len).map_err(io)? {
        return Err(source_changed("the cursor no longer falls between lines"));
    }
    file.seek(SeekFrom::Start(start)).map_err(io)?;
    let mut input = BufReader::new(file);

    let mut rows: Vec<Row> = Vec::new();
    let mut warnings = Vec::new();
    let mut clipping = false;
    let mut consumed = 0u64;
    let mut malformed = 0u32;
    let mut line = Vec::new();
    let content_complete = loop {
        if rows.len() as u32 >= limit {
            break input.fill_buf().map_err(io)?.is_empty();
        }
        line.clear();
        let n = input.read_until(b'\n', &mut line).map_err(io)?;
        if n == 0 {
            break true;
        }
        let terminated = line.last() == Some(&b'\n');
        let text = String::from_utf8_lossy(&line);
        let text = text.trim_end_matches('\n');
        let blank = text.trim().is_empty();
        let parsed = if blank { Some(Vec::new()) } else { parse(text) };
        if !terminated && (blank || parsed.is_none()) {
            break false; // a write in progress
        }
        match parsed {
            Some(parsed) => rows.extend(parsed),
            None => malformed += 1,
        }
        consumed += n as u64;
    };
    for row in rows.iter_mut().filter(|r| r.kind == "tool_result") {
        clipping |= clip(&mut row.text);
    }
    if malformed > 0 {
        warnings.push(format!("skipped {malformed} unreadable line(s)"));
    }
    Ok(Page {
        rows,
        cursor: Some(format!("{hash}:{}", start + consumed)),
        content_complete,
        clipping,
        warnings,
    })
}

pub(crate) fn json_lines(
    source: &Path,
    cursor: Option<&str>,
    limit: u32,
    parse: impl Fn(&Value) -> Vec<Row>,
) -> Result<Page> {
    read_lines(source, cursor, limit, |line| {
        serde_json::from_str::<Value>(line).ok().map(|v| parse(&v))
    })
}

/// Reads archived or captured screen text, one row per non-empty line.
/// It cannot find anything on its own: the caller supplies the path.
#[derive(Clone, Debug, Default)]
pub struct ScreenReader;

impl Reader for ScreenReader {
    fn kind(&self) -> &'static str {
        "screen"
    }

    fn locate(
        &self,
        _session_id: &str,
        session_path: Option<&Path>,
        _cwd: &Path,
    ) -> Result<Option<PathBuf>> {
        Ok(proven_path(session_path))
    }

    fn read(&self, source: &Path, cursor: Option<&str>, limit: u32) -> Result<Page> {
        read_lines(source, cursor, limit, |line| {
            Some(vec![Row {
                role: "screen".into(),
                kind: "screen".into(),
                text: line.trim_end().to_owned(),
                tool: None,
                at: None,
            }])
        })
    }
}
