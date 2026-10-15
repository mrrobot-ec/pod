# Lane B1: Limits, cost, and budget

Spec: `docs/superpowers/specs/2026-10-14-hardening-design.md` section B1. Depends on lane A2 merged (`pod usage --tick` keeps the usage records fresh). Read the Global Constraints in `00-index.md` first.

**Goal:** `worker-start` refuses when a fleet cap or a usage-window threshold is hit, each Claude worker's cost is known, and a Run can carry a soft budget.

**Files:**
- Modify: `src/config.rs` (`[limits]`), `src/error.rs` (`LimitReached`, `BudgetExceeded`), `src/engine/lifecycle.rs` (preflight `limits::check`), new `src/engine/limits.rs`
- Modify: `src/usage/mod.rs` (`save_session`, `load_session`), `src/cli/commands/env.rs` (`--record` stores per session), `src/engine/lifecycle.rs` (`WorkerView.cost_usd`), `src/cli/commands/workers.rs` (`costUsd`), `src/board/mod.rs` (cost in the task note)
- Modify: `src/store/schema.rs` (migration 3: `runs.budget_usd REAL`), `src/store/rows/runs.rs` (`budget_usd`), `src/engine/runs.rs`, `src/cli/commands/runs.rs` (`--budget-usd`, `spentUsd`, `budgetUsd`)
- Modify: `docs/reference/config.md`, `docs/reference/errors.md`, `docs/reference/commands.md`, `docs/reference/commands-workers.md`, spec 7.1 and a new 12.4, `CHANGELOG.md`
- Test: `tests/engine_lifecycle.rs`, `tests/usage.rs`, `tests/cli_usage.rs`, `tests/engine_store.rs` (migration), `tests/cli_runs_tasks.rs`

**Interfaces:**
- `pub struct Limits { pub max_active_workers: Option<u32>, pub max_active_per_kind: BTreeMap<String, u32>, pub pause_above_percent: Option<f64> }` under `Config.limits` (`#[serde(default)]`).
- `pub fn check(engine: &Engine, kind: &str, run: &RunId) -> Result<()>` in `src/engine/limits.rs`: returns `Err(limit_reached)` or `Err(budget_exceeded)` with `data` `{limit, current, max}` or `{spentUsd, budgetUsd}`; `Ok(())` otherwise. Called in `worker_start` and `dispatch` before any Dispatch is inserted.
- Active workers: `workers.terminal_state = 'active'` joined to dispatches with `status = 'dispatched'` in this session's store; per kind through `dispatches.agent_kind`.
- `UsageStore::save_session(&ClaudeUsage)` writes `usage/sessions/<session_id>.json`; `load_session(session_id) -> Option<ClaudeUsage>`. Cost of a Dispatch: `load_session(workers.session_id).cost_usd`.
- `runs.budget_usd` nullable; `spent_usd(tx, run) -> f64` sums the matched session costs of the Run's Dispatches (0 when none match).

### Task 1: Caps on active workers

- [ ] **Step 1: Write the failing test** in `tests/engine_lifecycle.rs`:

```rust
#[test]
fn worker_start_refuses_above_the_active_caps_with_a_literal_limit() {
    let (mut env, run, task) = seeded();
    env.engine.config.limits.max_active_workers = Some(1);
    env.engine.config.limits.max_active_per_kind.insert("claude".into(), 0);
    let first = lifecycle::worker_start(&env.engine, input(task, run.clone())).unwrap();
    assert_eq!(first.state, "ready");
    let second_task = new_task(&env, &run, "second");
    let error = lifecycle::worker_start(&env.engine, input(second_task.clone(), run.clone())).unwrap_err();
    assert_eq!(error.code, ErrorCode::LimitReached);
    assert_eq!(error.data["limit"], "max_active_workers");
    assert_eq!((error.data["current"].as_u64(), error.data["max"].as_u64()), (Some(1), Some(1)));
    // No Dispatch was created by the refused start.
    let dispatches = env.engine.store.read(|tx| dispatches::for_task(tx, &second_task)).unwrap();
    assert!(dispatches.is_empty());
    env.engine.config.limits.max_active_workers = None;
    let mut claude = input(second_task, run);
    claude.agent = Some("claude".into());
    let error = lifecycle::worker_start(&env.engine, claude).unwrap_err();
    assert_eq!(error.data["limit"], "max_active_per_kind:claude");
}
```

Use the real dispatch-listing reader (`grep -n "pub fn " src/store/rows/dispatches.rs`).
- [ ] **Step 2: Run:** fails (`limits` missing).
- [ ] **Step 3: Implement:** `Limits` in `config.rs`; `ErrorCode::LimitReached` (`limit_reached`) and `BudgetExceeded` (`budget_exceeded`) in `error.rs` including `ErrorCode::ALL` and the snake_case names; `src/engine/limits.rs` with `check` as in Interfaces (`PodError::new(...).with_data(json!({...}))` or however `data` is attached today; `grep -n "data" src/error.rs`); call it in `worker_start` right after `launch` is resolved and before `insert_starting`, and in `dispatch` before its Dispatch row.
- [ ] **Step 4: Run** the suite. **Step 5: Commit:** `git commit -am "feat(limits): caps on active workers"`.

### Task 2: Pause on a usage window

- [ ] **Step 1: Write the failing test** in `tests/engine_lifecycle.rs`: write a `ClaudeUsage` record with `five_hour.used_percent = 97` and `recorded_at = now` into `UsageStore::new(env.engine.paths.state_dir.join("usage"))`, set `pause_above_percent = Some(95.0)`, start a `claude` worker → `LimitReached` with `data.limit == "pause_above_percent"`, `data.window == "5h"`, `data.usedPercent == 97.0`. Then overwrite the record with `recorded_at = now - 11 minutes` → the start succeeds (stale usage proves nothing). A `codex` start is judged by the Codex record only.
- [ ] **Step 2: Run:** fails. **Step 3: Implement** in `limits::check`: load the vendor's record (`claude` → `load_claude`, `codex` → `load_codex`), ignore it when older than 10 minutes, and refuse when any window's `used_percent >= pause_above_percent`. **Step 4: Run** the suite. **Step 5: Commit:** `git commit -am "feat(limits): pause starts above a usage window threshold"`.

### Task 3: Per-worker cost

- [ ] **Step 1: Write the failing tests.** `tests/usage.rs`: `save_session`/`load_session` round trip keyed by `session_id`. `tests/cli_usage.rs`: `pod usage --record` with the fixture payload writes `usage/sessions/7988217c-5f46-4d1e-8b3f-b8a10647ac7e.json`. `tests/engine_lifecycle.rs`: a worker whose `workers.session_id` equals that id (set it through `workers::update_session_id` in a test transaction) shows `costUsd == 3.1046` in `worker_show`'s view, and `worker-list` rows carry `costUsd`.
- [ ] **Step 2: Run:** fails. **Step 3: Implement:** the two store methods, the `--record` branch saving both the account record and the session record, `WorkerView.cost_usd: Option<f64>` and the JSON keys; in `board/mod.rs` append ` · $<cost>` to the task `note` when known (dagr's contract has no cost field; the note is free text). **Step 4: Run** the suite. **Step 5: Commit:** `git commit -am "feat(usage): per-worker cost from the Claude status line"`.

### Task 4: Soft budget per Run

- [ ] **Step 1: Write the failing tests.** `tests/engine_store.rs`: opening a version-2 store migrates to 3 and `runs.budget_usd` exists (`PRAGMA table_info(runs)`). `tests/cli_runs_tasks.rs`: `run-create --budget-usd 5 --json` returns `budgetUsd: 5.0`; `run-show` returns `spentUsd` and `budgetUsd`. `tests/engine_lifecycle.rs`: with a Run budget of 1.0 and one Dispatch whose session cost is 3.1046, a start fails with `BudgetExceeded` and `data.spentUsd == 3.1046`; with a budget of 10.0 it succeeds and the receipt carries a `warnings` entry when spent is above 80 percent of the budget.
- [ ] **Step 2: Run:** fails. **Step 3: Implement:** `MIGRATION_3 = "ALTER TABLE runs ADD COLUMN budget_usd REAL;"`, `LATEST_VERSION = 3`, `RunRow.budget_usd: Option<f64>` read and written everywhere `RunRow` is built (grep `RunRow {`), `runs::create` gains the budget, the CLI flag, `spent_usd` in `engine/runs.rs`, the check in `limits::check` (budget after caps), the warning in `worker_start`'s receipt. **Step 4: Run** the suite. **Step 5: Commit:** `git commit -am "feat(runs): soft budget per Run"`.

### Task 5: Docs

- [ ] **Step 1:** `config.md` `[limits]` table; `errors.md` rows for `limit_reached` and `budget_exceeded`; `commands.md` (`run-create --budget-usd`, `run-show` fields); `commands-workers.md` (`costUsd`, the refusals); spec 7.1 preflight list and a new 12.4 "Limits and cost" paragraph; `CHANGELOG.md` 0.2.0 lines.
- [ ] **Step 2:** `cargo test --release --test docs_sync`, line counts, commit: `git commit -am "docs: limits, cost, and budget"`.
