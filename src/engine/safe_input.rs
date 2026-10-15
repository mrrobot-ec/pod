use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::engine::waiting::StateLock;
use crate::engine::{Engine, observe};
use crate::error::{ErrorCode, Result};
use crate::herdr::{AgentInfo, AgentStatus, Herdr, PromptOutcome};
use crate::ids::DispatchId;
use crate::store::rows::workers;

const FRESH_SECONDS: i64 = 15;
/// How long the paste fallback waits for a detected agent to draw an empty
/// input line. A Codex that answers a stray startup prompt first takes about
/// 8 seconds to become idle (seen live 2026-10-13).
pub const READY_WAIT_MS: u64 = 20_000;
const PROMPT_WAIT_MS: u64 = 5_000;
const MAX_CONFIRMATION_LINES: usize = 2_000;

/// Result of the paste-confirm-submit safe-input procedure.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InputOutcome {
    Delivered,
    RenderedUnconfirmed,
    Skipped(String),
    Failed(String),
}

impl InputOutcome {
    pub(crate) fn stored_name(&self) -> &'static str {
        match self {
            Self::Delivered => "delivered",
            Self::RenderedUnconfirmed => "rendered_unconfirmed",
            Self::Skipped(_) => "skipped",
            Self::Failed(_) => "failed",
        }
    }

    pub(crate) fn detail(&self) -> Option<&str> {
        match self {
            Self::Skipped(detail) | Self::Failed(detail) => Some(detail),
            Self::Delivered | Self::RenderedUnconfirmed => None,
        }
    }
}

/// Input-line prompt markers per agent kind (coordinator-confirmed). The
/// input line is the lowest screen line that starts with one of them; TUI
/// borders and footers below it are ignored.
const PROMPTS: &[(&str, &[&str])] = &[
    ("codex", &["›"]),
    ("claude", &["❯", ">"]),
    ("grok", &[">", "❯"]),
    ("cursor", &[">", "❯"]),
    ("pi", &[">", "❯"]),
];

/// Characters that frame an input box rather than hold input.
const FRAME: &[char] = &[
    '│', '┃', '|', '─', '━', '═', '╭', '╮', '╰', '╯', '┌', '┐', '└', '┘',
];

/// Whether `rest` (the input line after its marker) is the agent's own hint
/// text for an empty input. herdr's plain-text read loses the dim styling
/// that marks it, so it is recognised by shape. Observed on herdr 0.9.3 in
/// the T8 review: Claude Code `❯ Try "write a test for <filepath>"`, Codex
/// `› Ask Codex to do anything`.
fn is_placeholder(kind: &str, rest: &str) -> bool {
    match kind {
        "claude" => rest.starts_with("Try \"") && rest.ends_with('"') && rest.len() > 6,
        "codex" => rest == "Ask Codex to do anything",
        _ => false,
    }
}

fn markers(kind: &str) -> &'static [&'static str] {
    PROMPTS
        .iter()
        .find(|(name, _)| *name == kind)
        .map_or(&[], |(_, markers)| markers)
}

/// The pane's screen as text, with an agent's prompt suggestion removed:
/// Claude Code draws it dim (`ESC[2m ... ESC[0m`, seen live 2026-10-14), so
/// dim runs are dropped before every other escape sequence is stripped.
pub fn screen(herdr: &dyn Herdr, pane_id: &str, lines: u32) -> Result<String> {
    herdr
        .pane_read_styled(pane_id, lines)
        .map(|styled| strip_ansi(&drop_dim_runs(&styled)))
}

/// Removes every `ESC[2m` run up to its `ESC[0m` or `ESC[22m` end.
fn drop_dim_runs(styled: &str) -> String {
    let mut out = String::with_capacity(styled.len());
    let mut rest = styled;
    while let Some(start) = rest.find("\x1b[2m") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 4..];
        let end = ["\x1b[0m", "\x1b[22m"]
            .iter()
            .filter_map(|marker| after.find(marker).map(|at| (at, marker.len())))
            .min()
            .map(|(at, len)| at + len);
        rest = match end {
            Some(end) => &after[end..],
            None => "",
        };
    }
    out.push_str(rest);
    out
}

/// Strips CSI (`ESC [ ... final`) and OSC (`ESC ] ... BEL | ESC \`) sequences.
fn strip_ansi(styled: &str) -> String {
    let mut out = String::with_capacity(styled.len());
    let mut chars = styled.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for next in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&next) {
                        break;
                    }
                }
            }
            Some(']') => {
                let mut previous = '\0';
                for next in chars.by_ref() {
                    if next == '\u{7}' || (previous == '\x1b' && next == '\\') {
                        break;
                    }
                    previous = next;
                }
            }
            _ => {}
        }
    }
    out
}

/// A screen line without surrounding whitespace and input-box frame.
fn content(line: &str) -> &str {
    line.trim().trim_matches(FRAME).trim()
}

/// The input line: its index in `lines` and the text after the marker.
fn input_line<'a>(kind: &str, lines: &[&'a str]) -> Option<(usize, &'a str)> {
    let markers = markers(kind);
    lines.iter().enumerate().rev().find_map(|(index, line)| {
        let line = content(line);
        markers.iter().find_map(|marker| {
            let rest = line.strip_prefix(marker)?;
            (rest.is_empty() || rest.starts_with(char::is_whitespace)).then(|| (index, rest.trim()))
        })
    })
}

/// Text on the input line that pod did not put there: neither empty nor the
/// agent's placeholder. `screen` must come from [`screen`], which drops the
/// dim prompt suggestion an agent draws in an empty composer.
pub fn foreign_input(agent: &AgentInfo, screen: &str) -> Option<String> {
    input_text(agent, screen).flatten()
}

/// The input line's text: `None` when no input line is on the screen,
/// `Some(None)` when it is bare (empty or the placeholder).
fn input_text(agent: &AgentInfo, screen: &str) -> Option<Option<String>> {
    let lines = screen.lines().collect::<Vec<_>>();
    let (_, rest) = input_line(&agent.kind, &lines)?;
    Some((!rest.is_empty() && !is_placeholder(&agent.kind, rest)).then(|| rest.to_owned()))
}

/// A paste whose rendering could not be confirmed may not be followed by
/// another paste in the same process. Wake attempts also persist this guard
/// through their stored `rendered_unconfirmed` outcome.
fn uncertain_inputs() -> &'static Mutex<HashSet<(u64, String)>> {
    static UNCERTAIN: OnceLock<Mutex<HashSet<(u64, String)>>> = OnceLock::new();
    UNCERTAIN.get_or_init(|| Mutex::new(HashSet::new()))
}

fn pane_key(herdr: &dyn Herdr, pane_id: &str) -> (u64, String) {
    (herdr.instance_id(), pane_id.to_owned())
}

/// Which route delivered a prompt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InjectPath {
    Prompt,
    Paste,
}

impl InjectPath {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::Paste => "paste",
        }
    }
}

/// How long a submission may hold its caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Patience {
    /// A brief for an agent pod just started: herdr may still call it
    /// inactive for a moment, and a fallback paste waits for its prompt
    /// (`ready_wait`, the settle window).
    Launch,
    /// A nudge: herdr waits up to 5 seconds for the turn, pod itself never
    /// sleeps, and a fallback paste needs the prompt ready at once.
    Nudge,
    /// A nudge from the tab bar, whose command has a 5-second timeout: herdr
    /// returns at submission without waiting for the turn.
    Bar,
}

/// herdr's own submission first (text and Enter as one ordered write, even
/// while the agent works); hand paste only when herdr refused the prompt.
/// One screen read before either: text a person typed into the input is left
/// alone and recorded as `user_typing` for `dispatch` (spec 7.4). An
/// unreachable herdr fails the input without a paste: the prompt may have
/// gone in before the connection broke, and a paste would type it twice.
pub fn prompt_or_paste(
    engine: &Engine,
    handle: &str,
    pane_id: &str,
    text: &str,
    dispatch: Option<&DispatchId>,
    patience: Patience,
) -> Result<(InputOutcome, InjectPath)> {
    prompt_or_paste_with_reason(engine, handle, pane_id, text, dispatch, patience)
        .map(|(outcome, path, _)| (outcome, path))
}

/// `prompt_or_paste`, plus herdr's error message when the prompt path
/// failed and the paste fallback ran, so a receipt can say why.
pub fn prompt_or_paste_with_reason(
    engine: &Engine,
    handle: &str,
    pane_id: &str,
    text: &str,
    dispatch: Option<&DispatchId>,
    patience: Patience,
) -> Result<(InputOutcome, InjectPath, Option<String>)> {
    let seen = person_typing(engine.herdr.as_ref(), pane_id);
    if let Some(dispatch) = dispatch {
        record_typing(engine, dispatch, seen.as_ref().map(Option::as_deref))?;
    }
    if let Some(Some(typed)) = seen {
        let shown = typed.chars().take(80).collect::<String>();
        return Ok((
            InputOutcome::Skipped(format!("person is typing: {shown}")),
            InjectPath::Prompt,
            None,
        ));
    }
    let wait_ms = (patience != Patience::Bar).then_some(PROMPT_WAIT_MS);
    // Right after `agent.start` herdr can still answer "not an active named
    // agent" for a moment (seen live 2026-10-14 on every fresh Claude
    // worker): a race at launch, retried within `ready_wait`, not a refusal.
    let retry_until = (patience == Patience::Launch).then(|| Instant::now() + engine.ready_wait);
    let prompted = loop {
        match engine.herdr.agent_prompt(handle, text, wait_ms) {
            Err(error)
                if error.message.contains("not an active named agent")
                    && retry_until.is_some_and(|deadline| Instant::now() < deadline) =>
            {
                let left = retry_until
                    .map(|deadline| deadline.saturating_duration_since(Instant::now()))
                    .unwrap_or_default();
                std::thread::sleep(Duration::from_millis(500).min(left));
            }
            other => break other,
        }
    };
    match prompted {
        Ok(PromptOutcome::Submitted) => Ok((InputOutcome::Delivered, InjectPath::Prompt, None)),
        Ok(PromptOutcome::Blocked) => Ok((
            InputOutcome::Skipped("agent is blocked at a prompt (agent_blocked)".into()),
            InjectPath::Prompt,
            None,
        )),
        // The text and Enter went in; no turn was observed. Never type again.
        Ok(PromptOutcome::Stalled) => {
            Ok((InputOutcome::RenderedUnconfirmed, InjectPath::Prompt, None))
        }
        Err(error) if error.code == ErrorCode::HerdrUnreachable => Ok((
            InputOutcome::Failed(error.message),
            InjectPath::Prompt,
            None,
        )),
        Err(error) => {
            if patience == Patience::Launch {
                wait_until_ready(
                    engine.herdr.as_ref(),
                    pane_id,
                    engine.ready_wait,
                    engine.input_settle,
                );
            }
            Ok((
                type_and_submit(engine.herdr.as_ref(), pane_id, text)?,
                InjectPath::Paste,
                Some(error.message),
            ))
        }
    }
}

/// What the pane's agent input holds, when it can be read: `Some(Some)` is
/// text a person typed, `Some(None)` a bare input. A pane or agent that
/// cannot be read, or a screen without an input line, proves nothing.
fn person_typing(herdr: &dyn Herdr, pane_id: &str) -> Option<Option<String>> {
    let agent = herdr
        .agent_list()
        .ok()?
        .into_iter()
        .find(|agent| agent.pane_id == pane_id)?;
    input_text(&agent, &screen(herdr, pane_id, 20).ok()?)
}

/// Stores what the pre-prompt read saw. A clear is written only after a bare
/// read and only while a person's typing is the latest input seen.
fn record_typing(engine: &Engine, dispatch: &DispatchId, seen: Option<Option<&str>>) -> Result<()> {
    let Some(typed) = seen else {
        return Ok(());
    };
    let now = engine.clock.now();
    engine.store.tx(|tx| {
        if typed.is_some() || observe::user_typing(tx, dispatch)?.is_some() {
            observe::record_user_typing(tx, now, dispatch, typed)?;
        }
        Ok(())
    })
}

/// Pastes `text`, confirms it is visible, and only then sends Enter.
pub fn type_and_submit(herdr: &dyn Herdr, pane_id: &str, text: &str) -> Result<InputOutcome> {
    if text.is_empty() {
        return Ok(InputOutcome::Skipped(
            "text is empty; Enter withheld".into(),
        ));
    }
    let key = pane_key(herdr, pane_id);
    if uncertain_inputs()
        .lock()
        .expect("uncertain input mutex poisoned")
        .contains(&key)
    {
        return Ok(InputOutcome::Skipped(
            "an earlier paste is unconfirmed; input has not been proven cleared".into(),
        ));
    }

    let (agent, before) = match readiness(herdr, pane_id) {
        Ok(ready) => ready,
        Err(outcome) => return Ok(outcome),
    };
    if let Err(error) = herdr.pane_send_text(pane_id, text) {
        uncertain_inputs()
            .lock()
            .expect("uncertain input mutex poisoned")
            .insert(key);
        return Ok(InputOutcome::Failed(error.message));
    }
    // Keep the first pasted line and its prompt marker inside the bounded
    // snapshot. The base worker preamble is already longer than 40 logical
    // lines, while a nudge should retain the small read it has always used.
    let confirmation_lines = text
        .lines()
        .count()
        .saturating_add(20)
        .min(MAX_CONFIRMATION_LINES) as u32;
    // Herdr acknowledges the PTY write before an application necessarily
    // redraws its input box. Allow that redraw one short bounded window; the
    // text is never sent again and Enter is still withheld unless a snapshot
    // proves this exact paste.
    // Measured on Codex 0.160.1 under herdr 0.9.3: a 4.6 KB paste is fully
    // drawn within 100 ms on an idle machine; ten panes starting at once
    // need more, so the window is 600 ms (still under the 1 s bound).
    let deadline = Instant::now() + Duration::from_millis(600);
    let confirmed = loop {
        match screen(herdr, pane_id, confirmation_lines) {
            Ok(after) if after != before && rendered(&agent, &after, text) => break true,
            Ok(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(_) | Err(_) => break false,
        }
    };
    if !confirmed {
        uncertain_inputs()
            .lock()
            .expect("uncertain input mutex poisoned")
            .insert(key);
        return Ok(InputOutcome::RenderedUnconfirmed);
    }
    // The confirmed text is in the input; if Enter cannot be confirmed the
    // text is still there, which is `rendered_unconfirmed`, never `failed`.
    if herdr.pane_send_keys(pane_id, &["Enter"]).is_err() {
        uncertain_inputs()
            .lock()
            .expect("uncertain input mutex poisoned")
            .insert(key);
        return Ok(InputOutcome::RenderedUnconfirmed);
    }
    Ok(InputOutcome::Delivered)
}

/// Types one command line into a shell pane pod just created (the repo
/// setup argv, spec 7.3): paste, confirm it is visible, then Enter. There is
/// no agent to prove idle, so the agent preconditions of `type_and_submit`
/// do not apply; the pane is new and holds nothing else. Visibility ignores
/// whitespace, because a narrow pane soft-wraps the line.
pub fn type_command(herdr: &dyn Herdr, pane_id: &str, command: &str) -> Result<InputOutcome> {
    if let Err(error) = herdr.pane_send_text(pane_id, command) {
        return Ok(InputOutcome::Failed(error.message));
    }
    match screen(herdr, pane_id, 2000) {
        Ok(screen) if squash(&screen).contains(&squash(command)) => {}
        _ => return Ok(InputOutcome::RenderedUnconfirmed),
    }
    if herdr.pane_send_keys(pane_id, &["Enter"]).is_err() {
        return Ok(InputOutcome::RenderedUnconfirmed);
    }
    Ok(InputOutcome::Delivered)
}

/// The proof that `pane_id` may be pasted into: a detected agent, idle on
/// evidence no older than 15 seconds, whose input line is empty. `Err` is the
/// outcome to report instead; nothing has been typed.
fn readiness(
    herdr: &dyn Herdr,
    pane_id: &str,
) -> std::result::Result<(AgentInfo, String), InputOutcome> {
    let agent = match herdr.agent_list() {
        Ok(agents) => agents.into_iter().find(|agent| agent.pane_id == pane_id),
        Err(error) => return Err(InputOutcome::Failed(error.message)),
    };
    let Some(agent) = agent else {
        return Err(InputOutcome::Skipped(
            "no detected agent owns the target pane".into(),
        ));
    };
    // herdr reports an agent that finished its turn as `done` until someone
    // looks at the pane; it is at its prompt like `idle`. `blocked` waits on
    // a person and is never typed into.
    if !matches!(agent.status, AgentStatus::Idle | AgentStatus::Done) {
        return Err(InputOutcome::Skipped("agent is not idle".into()));
    }
    if Utc::now()
        .signed_duration_since(agent.observed_at)
        .num_seconds()
        > FRESH_SECONDS
    {
        return Err(InputOutcome::Skipped("agent idle evidence is stale".into()));
    }
    let before = match screen(herdr, pane_id, 20) {
        Ok(screen) => screen,
        Err(error) => return Err(InputOutcome::Failed(error.message)),
    };
    if !bare_prompt(&agent, &before) {
        return Err(InputOutcome::Skipped(
            "pane input is not proven empty and ready".into(),
        ));
    }
    Ok((agent, before))
}

/// Waits up to `timeout` for `pane_id` to be ready for safe input (see
/// `readiness`) and to stay that way, screen unchanged, for `settle`. A
/// freshly started agent can be detected before its input line is drawn,
/// and herdr's launch can leave a stray prompt (the command name) that the
/// agent answers a moment later: a paste typed into that gap loses its head
/// (seen live 2026-10-14 with Claude Code). This reads the pane and types
/// nothing.
pub fn wait_until_ready(
    herdr: &dyn Herdr,
    pane_id: &str,
    timeout: Duration,
    settle: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    let mut stable: Option<(Instant, String)> = None;
    loop {
        match readiness(herdr, pane_id) {
            Ok((_, screen)) => match &stable {
                Some((since, previous)) if *previous == screen => {
                    if since.elapsed() >= settle {
                        return true;
                    }
                }
                _ => stable = Some((Instant::now(), screen)),
            },
            Err(_) => stable = None,
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The input is proven empty and ready: an input line holding nothing or
/// only the agent's placeholder, with no continuation line under it.
fn bare_prompt(agent: &AgentInfo, screen: &str) -> bool {
    let lines = screen.lines().collect::<Vec<_>>();
    let Some((index, rest)) = input_line(&agent.kind, &lines) else {
        return false;
    };
    if !(rest.is_empty() || is_placeholder(&agent.kind, rest)) {
        return false;
    }
    // A composer drawn as a box ends with a frame line that may carry a
    // label (Grok: `╰──── Grok 4`); it is frame, not a continuation.
    lines
        .get(index + 1)
        .is_none_or(|next| content(next).is_empty() || is_frame_line(next))
}

/// A line that is part of an input box's frame: it starts with a frame
/// character after any indentation.
fn is_frame_line(line: &str) -> bool {
    line.trim_start().starts_with(FRAME)
}

/// The pasted `text` is visible in the input: line by line from the input
/// line down (indentation and frame ignored), or as the agent's collapsed
/// paste summary with the matching size (Claude Code
/// `[Pasted text #1 +29 lines]` counts newlines; Codex
/// `[Pasted Content 1949 chars]`). A wrapped line does not match, which only
/// withholds Enter.
///
/// A composer taller than its box scrolls to the cursor, so the marker line
/// then shows a later line of the paste and the box ends at the paste's last
/// line (Codex 0.160.1 on herdr 0.9.3 draws a 60-line paste as lines 25 to
/// 60 after the marker). That contiguous tail is accepted when at least
/// `SCROLLED_MIN_LINES` of it are visible; the input was proven empty before
/// the paste, so nothing else can be in the box.
fn rendered(agent: &AgentInfo, screen: &str, text: &str) -> bool {
    let lines = screen.lines().collect::<Vec<_>>();
    let Some((index, rest)) = input_line(&agent.kind, &lines) else {
        return false;
    };
    let newlines = text.matches('\n').count();
    let collapsed = match agent.kind.as_str() {
        "claude" => rest
            .strip_prefix("[Pasted text #")
            .and_then(|tail| tail.split_once(' '))
            .is_some_and(|(number, tail)| {
                number.chars().all(|c| c.is_ascii_digit())
                    && (tail == format!("+{newlines} lines]")
                        || tail == format!("+{} lines]", text.lines().count().saturating_sub(1)))
            }),
        "codex" | "pi" => {
            rest == format!("[Pasted Content {} chars]", text.chars().count())
                || rest == format!("[Pasted Content {} chars]", text.len())
        }
        _ => false,
    };
    if collapsed {
        return true;
    }
    // Compare without whitespace: Codex 0.160.1 soft-wraps a long pasted
    // line inside its composer and scrolls the box to the cursor (seen live
    // 2026-10-13 with the worker preamble), so neither line breaks nor the
    // first line are reliable. The input was proven empty before the paste,
    // so the box holds nothing but this paste: the screen from the marker
    // down (the agent's footer included) must contain the whole paste, or,
    // when the box scrolled, at least its last `SCROLLED_MIN_CHARS`.
    let wanted = squash(text);
    let shown = squash(
        &std::iter::once(rest)
            .chain(lines[index + 1..].iter().map(|line| content(line)))
            .collect::<Vec<_>>()
            .join(" "),
    );
    if wanted.is_empty() {
        return false;
    }
    if shown.contains(&wanted) {
        return true;
    }
    // Only a composer that scrolls can legitimately show a tail. Claude Code
    // collapses multi-line pastes instead, so a visible tail there is a
    // truncated paste and must not be confirmed.
    if !matches!(agent.kind.as_str(), "codex" | "pi") {
        return false;
    }
    let visible = wanted.len().min(SCROLLED_MIN_CHARS);
    let tail = &wanted[wanted.len() - visible..];
    wanted.len() > visible && shown.starts_with(&squash(rest)) && shown.contains(tail)
}

/// Text without any whitespace, the form screen and paste are compared in.
fn squash(value: &str) -> String {
    value.split_whitespace().collect()
}

/// The fewest characters of a scrolled paste that must be visible to count
/// as it (about three lines of a worker preamble).
const SCROLLED_MIN_CHARS: usize = 200;

/// Injects a worker preamble exactly once and records its safe-input outcome.
pub fn inject_preamble(
    engine: &Engine,
    dispatch: &DispatchId,
    handle: &str,
    text: &str,
) -> Result<InputOutcome> {
    // Two processes injecting at once could both see no record and both
    // type; the per-Dispatch lock makes the read-type-record one step.
    let path = engine
        .paths
        .state_dir
        .join(format!("preamble-{dispatch}.lock"));
    let _lock = match StateLock::try_acquire(&path) {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            return Ok(InputOutcome::Skipped(
                "another preamble injection for this Dispatch is in progress".into(),
            ));
        }
        Err(error) => {
            return Err(crate::error::PodError::store(format!(
                "could not lock {}: {error}",
                path.display()
            )));
        }
    };
    let worker = engine.store.read(|tx| workers::get(tx, dispatch))?;
    let Some(worker) = worker else {
        return Ok(InputOutcome::Skipped("worker does not exist".into()));
    };
    if worker.preamble_input.is_some() {
        return Ok(InputOutcome::Skipped(
            "preamble input was already attempted".into(),
        ));
    }
    let (outcome, path, prompt_error) = match worker.pane_id.as_deref() {
        Some(pane) => {
            let (outcome, path, prompt_error) = prompt_or_paste_with_reason(
                engine,
                handle,
                pane,
                text,
                Some(dispatch),
                Patience::Launch,
            )?;
            (outcome, Some(path), prompt_error)
        }
        None => (
            InputOutcome::Skipped("worker has no pane".into()),
            None,
            None,
        ),
    };
    let now = engine.clock.now();
    engine.store.tx(|tx| {
        if let Some(row) = workers::get(tx, dispatch)?
            && row.preamble_input.is_none()
        {
            if let Some(path) = path {
                let mut effects = row
                    .effects
                    .and_then(|value| serde_json::from_value::<Vec<String>>(value).ok())
                    .unwrap_or_default();
                effects.push(format!("inject:{}", path.as_str()));
                if let Some(reason) = prompt_error.as_deref() {
                    effects.push(format!("prompt_error:{reason}"));
                }
                workers::update_effects(tx, dispatch, Some(&serde_json::json!(effects)))?;
            }
            workers::update_preamble_input(tx, dispatch, Some(outcome.stored_name()))?;
            if let Some(detail) = outcome.detail() {
                workers::update_last_error(tx, dispatch, Some(detail))?;
            }
            workers::update_updated_at(tx, dispatch, now)?;
        }
        Ok(())
    })?;
    Ok(outcome)
}
