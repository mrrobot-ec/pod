//! Codex rate limits through `codex app-server` (JSON-RPC over stdio):
//! `initialize`, `initialized`, then `account/rateLimits/read`.

use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use super::{CodexUsage, Window};
use crate::error::{ErrorCode, PodError, Result};

const READ_ID: u64 = 2;

/// Spawns `<binary> app-server`, asks for the account's rate limits, and
/// stops it. `timeout_ms` bounds the whole exchange.
pub fn read_codex(binary: &OsStr, now: DateTime<Utc>, timeout_ms: u64) -> Result<CodexUsage> {
    let mut child = Command::new(binary)
        .arg("app-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            PodError::new(
                ErrorCode::Usage,
                format!(
                    "could not start codex app-server ({}): {error}",
                    binary.to_string_lossy()
                ),
                &["Install Codex, or point POD_CODEX_BIN at the codex executable."],
            )
        })?;
    let result = exchange(&mut child, timeout_ms);
    let _ = child.kill();
    let _ = child.wait();
    let reply = result?;
    codex_from_rate_limits(&reply, now)
}

fn exchange(child: &mut std::process::Child, timeout_ms: u64) -> Result<Value> {
    let stdout = child.stdout.take().expect("piped stdout");
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if value.get("id").and_then(Value::as_u64) == Some(READ_ID) {
                let _ = sender.send(value);
                break;
            }
        }
    });
    let mut stdin = child.stdin.take().expect("piped stdin");
    let messages = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"clientInfo": {"name": "pod", "title": "pod", "version": env!("CARGO_PKG_VERSION")}}}),
        json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}),
        json!({"jsonrpc": "2.0", "id": READ_ID, "method": "account/rateLimits/read", "params": {}}),
    ];
    for message in &messages {
        writeln!(stdin, "{message}")
            .and_then(|()| stdin.flush())
            .map_err(|error| {
                PodError::new(
                    ErrorCode::Usage,
                    format!("codex app-server closed its input: {error}"),
                    &[],
                )
            })?;
    }
    let reply = receiver
        .recv_timeout(Duration::from_millis(timeout_ms))
        .map_err(|_| {
            PodError::new(
                ErrorCode::Usage,
                "codex app-server did not answer account/rateLimits/read in time",
                &["Check `codex` logs in and works, then retry."],
            )
        })?;
    if let Some(error) = reply.get("error") {
        return Err(PodError::new(
            ErrorCode::Usage,
            format!("codex refused account/rateLimits/read: {error}"),
            &["Sign in to codex and retry."],
        ));
    }
    reply
        .get("result")
        .cloned()
        .ok_or_else(|| PodError::new(ErrorCode::Usage, "codex reply has no result", &[]))
}

/// Shapes an `account/rateLimits/read` result into a usage record.
pub fn codex_from_rate_limits(result: &Value, now: DateTime<Utc>) -> Result<CodexUsage> {
    let limits = result
        .get("rateLimits")
        .ok_or_else(|| PodError::new(ErrorCode::Usage, "codex reply has no rateLimits", &[]))?;
    let window = |key: &str| {
        let value = limits.get(key)?;
        let minutes = value.get("windowDurationMins").and_then(Value::as_i64);
        Window::from_value(&window_label(minutes), value, "usedPercent")
    };
    Ok(CodexUsage {
        recorded_at: now,
        plan: limits
            .get("planType")
            .and_then(Value::as_str)
            .map(str::to_owned),
        primary: window("primary"),
        secondary: window("secondary"),
        credits: limits
            .pointer("/credits/balance")
            .and_then(|balance| match balance {
                Value::String(text) => text.parse::<f64>().ok(),
                other => other.as_f64(),
            }),
    })
}

fn window_label(minutes: Option<i64>) -> String {
    match minutes {
        Some(300) => "5h".into(),
        Some(10080) => "wk".into(),
        Some(minutes) if minutes % 1440 == 0 => format!("{}d", minutes / 1440),
        Some(minutes) if minutes % 60 == 0 => format!("{}h", minutes / 60),
        Some(minutes) => format!("{minutes}m"),
        None => "window".into(),
    }
}
