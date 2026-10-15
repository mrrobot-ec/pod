//! Preflight limits for `worker-start` and `dispatch` (spec 7.1, 12.4): fleet
//! caps and the usage pause from `[limits]`, and a Run's soft budget. Runs before any Dispatch row
//! exists, so a refusal leaves nothing to settle.

use rusqlite::params;

use crate::engine::{Engine, runs};
use crate::error::{ErrorCode, PodError, Result};
use crate::ids::RunId;
use crate::usage::{UsageStore, Window};

/// Usage records older than this prove nothing about the windows now.
const USAGE_MAX_AGE_MINUTES: i64 = 10;

/// A Run's receipts warn once its spend passes this share of its budget.
const BUDGET_WARN_FRACTION: f64 = 0.8;

/// Refuses with `limit_reached` when a fleet cap is hit or the vendor's usage
/// is at the pause threshold.
pub fn check(engine: &Engine, kind: &str, run: &RunId) -> Result<()> {
    let limits = &engine.config.limits;
    if let Some(max) = limits.max_active_workers {
        let current = engine.store.read(|tx| active_workers(tx, None))?;
        if current >= max {
            return Err(limit_reached("max_active_workers", current, max));
        }
    }
    if let Some(max) = limits.max_active_per_kind.get(kind).copied() {
        let current = engine.store.read(|tx| active_workers(tx, Some(kind)))?;
        if current >= max {
            return Err(limit_reached(
                &format!("max_active_per_kind:{kind}"),
                current,
                max,
            ));
        }
    }
    if let Some(threshold) = limits.pause_above_percent {
        check_usage(engine, kind, threshold)?;
    }
    if let Some((spent, budget)) = budget_state(engine, run)?
        && spent >= budget
    {
        return Err(PodError::new(
            ErrorCode::BudgetExceeded,
            format!("Run {run} has spent ${spent:.2} of its ${budget:.2} budget"),
            &[
                "Finish the remaining work without a new worker, or start a new Run with a larger --budget-usd.",
                "Cost is what the agents report and may differ from the bill.",
            ],
        )
        .with_data("spentUsd", spent)
        .with_data("budgetUsd", budget));
    }
    Ok(())
}

/// A warning for a receipt once a Run's spend passes 80 percent of its budget.
pub fn budget_warning(engine: &Engine, run: &RunId) -> Result<Option<String>> {
    Ok(budget_state(engine, run)?
        .filter(|(spent, budget)| *spent > BUDGET_WARN_FRACTION * *budget)
        .map(|(spent, budget)| {
            format!("Run {run} has spent ${spent:.2} of its ${budget:.2} budget; new workers are refused at 100 percent.")
        }))
}

/// `(spent, budget)` in USD for a Run that has a budget.
fn budget_state(engine: &Engine, run: &RunId) -> Result<Option<(f64, f64)>> {
    let usage = UsageStore::new(engine.paths.state_dir.join("usage"));
    engine.store.read(|tx| {
        let Some(budget) = runs::get(tx, run)?.and_then(|row| row.budget_usd) else {
            return Ok(None);
        };
        Ok(Some((runs::spent_usd(tx, &usage, run)?, budget)))
    })
}

/// Refuses when a fresh usage window for `kind`'s vendor is at `threshold`.
/// Stale, missing, or unreadable records, and kinds with no record, never
/// block a start.
fn check_usage(engine: &Engine, kind: &str, threshold: f64) -> Result<()> {
    let store = UsageStore::new(engine.paths.state_dir.join("usage"));
    let (recorded_at, windows) = match kind {
        "claude" => match store.load_claude() {
            Ok(Some(usage)) => (usage.recorded_at, [usage.five_hour, usage.seven_day]),
            _ => return Ok(()),
        },
        "codex" => match store.load_codex() {
            Ok(Some(usage)) => (usage.recorded_at, [usage.secondary, usage.primary]),
            _ => return Ok(()),
        },
        _ => return Ok(()),
    };
    if engine.clock.now() - recorded_at > chrono::Duration::minutes(USAGE_MAX_AGE_MINUTES) {
        return Ok(());
    }
    match windows
        .into_iter()
        .flatten()
        .find(|window| window.used_percent >= threshold)
    {
        Some(window) => Err(usage_pause(kind, &window, threshold)),
        None => Ok(()),
    }
}

fn usage_pause(kind: &str, window: &Window, threshold: f64) -> PodError {
    PodError::new(
        ErrorCode::LimitReached,
        format!(
            "the {kind} {} usage window is at {}%, at or above pause_above_percent {threshold}",
            window.label, window.used_percent
        ),
        &[
            &format!(
                "The {kind} {} window is at {}% and pause_above_percent is {threshold}.",
                window.label, window.used_percent
            ),
            "Wait for the window to reset, then retry.",
            "Raise pause_above_percent under [limits] in the pod config, or remove it.",
        ],
    )
    .with_data("limit", "pause_above_percent")
    .with_data("window", window.label.as_str())
    .with_data("usedPercent", window.used_percent)
    .with_data("max", threshold)
}

/// Active workers in this session's store, optionally of one agent kind.
fn active_workers(tx: &crate::store::Tx<'_>, kind: Option<&str>) -> Result<u32> {
    Ok(tx.0.query_row(
        "SELECT COUNT(*) FROM workers JOIN dispatches ON dispatches.id = workers.dispatch_id \
         WHERE workers.terminal_state = 'active' AND dispatches.status = 'dispatched' \
         AND (?1 IS NULL OR dispatches.agent_kind = ?1)",
        params![kind],
        |row| row.get(0),
    )?)
}

fn limit_reached(limit: &str, current: u32, max: u32) -> PodError {
    let state = format!("{limit} allows {max} active workers and {current} are active.");
    PodError::new(
        ErrorCode::LimitReached,
        format!("{limit} is {max} and {current} workers are active"),
        &[
            &state,
            "Wait for a worker to finish, or stop one with worker-stop.",
            "Raise the limit under [limits] in the pod config.",
        ],
    )
    .with_data("limit", limit)
    .with_data("current", current)
    .with_data("max", max)
}
