# Lane A2: A clock without a daemon

Spec: `docs/superpowers/specs/2026-10-14-hardening-design.md` section A2. Read the Global Constraints in `00-index.md` first.

**Goal:** the wake ladder and the stale check run on a clock inside `check --wait` and `ask`, and from the tab bar entry (`pod usage --tick`) when nobody waits, with no new process.

**Files:**
- Modify: `src/engine/wake.rs` (`Trigger::WaitTick`, `Trigger::BarTick`, `pub fn tick`)
- Modify: `src/engine/waiting.rs` (`check_wait` and `ask_wait` loops)
- Modify: `src/engine/mod.rs` (`Engine.tick_interval`), `tests/support/mod.rs` (set it to 200 ms)
- Modify: `src/cli/commands/env.rs` (`pod usage --tick`), `src/usage/install.rs` (install writes `pod usage --tick`), `tests/usage.rs`, `tests/cli_usage.rs`
- Modify: spec sections 3.2 and 8.2, `docs/reference/commands.md` (`usage --tick`), `docs/reference/commands-messages.md` (`check --wait` ticks), `CHANGELOG.md`
- Test: `tests/engine_wake.rs`, `tests/engine_waiting.rs`

**Interfaces:**
- Produces `pub fn tick(engine: &Engine, run: &RunId, trigger: Trigger) -> Result<TickReport>` in `wake.rs` with `pub struct TickReport { pub mailboxes: usize, pub attempts: Vec<WakeAttemptRow> }`.
- Produces `pub fn tick_all(engine: &Engine, trigger: Trigger) -> Result<Vec<(RunId, TickReport)>>`: every Run that has a Dispatch with status `dispatched`.
- `Trigger::WaitTick` may wait (quiet check sleeps, nobody waits on the waiter's own tick); `Trigger::BarTick` may not (the bar command has a 5-second timeout).
- Consumes nothing from other lanes; A1's `prompt_or_paste` lands under the same `on_trigger`, so the tick needs no change when A1 merges.

### Task 1: `wake::tick` runs one ladder pass for every mailbox with unread mail

- [ ] **Step 1: Write the failing test** in `tests/engine_wake.rs` (reuse the helper that seeds a run, a worker `w1` present and idle, and sends it one message; look at the existing `on_trigger` tests for the seeding):

```rust
#[test]
fn tick_runs_one_attempt_per_mailbox_and_honours_the_sixty_second_rule() {
    let (env, run, mailbox) = seeded_worker_with_unread_mail();
    let first = wake::tick(&env.engine, &run, Trigger::WaitTick).unwrap();
    assert_eq!(first.mailboxes, 1);
    assert_eq!(first.attempts.len(), 1);
    // Inside the minute nothing happens; after it, attempt 2.
    env.clock.advance(std::time::Duration::from_secs(30));
    assert!(wake::tick(&env.engine, &run, Trigger::WaitTick).unwrap().attempts.is_empty());
    env.clock.advance(std::time::Duration::from_secs(31));
    let third = wake::tick(&env.engine, &run, Trigger::WaitTick).unwrap();
    assert_eq!(third.attempts[0].attempt, 2);
    assert_eq!(third.attempts[0].mailbox, mailbox);
    // A read-only trigger never ticks.
    assert!(wake::tick(&env.engine, &run, Trigger::ReadOnlyCommand).unwrap().attempts.is_empty());
}
```

- [ ] **Step 2: Run:** `cargo test --release --test engine_wake tick_runs` → fails (`tick` missing).
- [ ] **Step 3: Implement** in `wake.rs`:

```rust
pub enum Trigger { IdleHook, MutatingCommand, ReadOnlyCommand, WaitTick, BarTick }
// can_wait: IdleHook | WaitTick => true, others false.

pub struct TickReport { pub mailboxes: usize, pub attempts: Vec<WakeAttemptRow> }

/// One ladder pass for every mailbox of `run` that holds unread mail: the
/// Run mailbox and each `dispatch:<id>` with an unread row. Each goes through
/// `on_trigger`, so the 60-second rule, the lock, and the ladder count hold.
pub fn tick(engine: &Engine, run: &RunId, trigger: Trigger) -> Result<TickReport> {
    if trigger == Trigger::ReadOnlyCommand { return Ok(TickReport { mailboxes: 0, attempts: Vec::new() }); }
    let mailboxes: Vec<String> = engine.store.read(|tx| {
        let mut statement = tx.0.prepare(
            "SELECT DISTINCT mailbox FROM messages WHERE run_id = ?1 AND read = 0 ORDER BY mailbox")?;
        Ok(statement.query_map([run.as_str()], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    })?;
    let mut attempts = Vec::new();
    for mailbox in &mailboxes {
        if let Some(row) = on_trigger(engine, mailbox, trigger)? { attempts.push(row); }
    }
    Ok(TickReport { mailboxes: mailboxes.len(), attempts })
}

pub fn tick_all(engine: &Engine, trigger: Trigger) -> Result<Vec<(RunId, TickReport)>> {
    let runs: Vec<RunId> = engine.store.read(|tx| { /* SELECT DISTINCT run_id FROM dispatches WHERE status = 'dispatched' */ })?;
    runs.into_iter().map(|run| tick(engine, &run, trigger).map(|report| (run, report))).collect()
}
```

Check the real column names in `src/store/schema.rs` (`messages.read`, `messages.run_id`) before writing the SQL. Update every `match` on `Trigger` (clippy will list them).
- [ ] **Step 4: Run** `cargo test --release --test engine_wake` → green.
- [ ] **Step 5: Commit:** `git commit -am "feat(wake): tick runs one ladder pass per mailbox with unread mail"`.

### Task 2: The waiting coordinator ticks

- [ ] **Step 1: Write the failing test** in `tests/engine_waiting.rs` (its helpers seed a run and a worker; `check_wait` runs with real time, so set the interval short):

```rust
#[test]
fn check_wait_ticks_the_ladder_while_it_waits() {
    let (mut env, run) = seeded(); // run bound to the coordinator terminal
    env.engine.tick_interval = std::time::Duration::from_millis(200);
    // a message for a worker mailbox whose first nudge was skipped (worker busy)
    let mailbox = seed_busy_worker_with_unread_mail(&env, &run);
    env.herdr.script_agent_status("w1", AgentStatus::Idle); // now idle for the tick
    let (_delivery, woke) = waiting::check_wait(&env.engine, &format!("run:{}", run.id), &["worker_done".into()], 1_500).unwrap();
    assert_eq!(woke, WokeOn::Timeout);
    let attempts = env.engine.store.read(|tx| wake_attempts::for_mailbox(tx, &mailbox)).unwrap();
    assert!(attempts.len() >= 2, "the waiter ticked: {attempts:?}");
}
```

Use the existing row reader for `wake_attempts` (look in `src/store/rows/wake_attempts.rs` for a list function and use its real name). The 60-second ladder rule still applies between attempts, so for this test advance `env.clock` by 61 seconds from a thread, or set the rule's clock through the fake clock before the wait: the simplest is to make the first attempt at commit time (as `after_commit_nudge` does) `skipped`, advance the fake clock by 61 s, then wait; the tick then produces attempt 2.
- [ ] **Step 2: Run:** fails (no tick during the wait).
- [ ] **Step 3: Implement:** `Engine` gains `pub tick_interval: std::time::Duration` (default 60 s; `tests/support/mod.rs` sets 200 ms). In `check_wait` and `ask_wait` add `let mut next_tick = Instant::now() + engine.tick_interval;`, include it in the `until` minimum, and when `now >= next_tick`: `wake::tick(engine, &run, Trigger::WaitTick)?; store_changed = true; next_tick = now + engine.tick_interval;`. `store_changed = true` makes the attention set recompute, which is what lets `stale` (a time-based category) wake the waiter (spec A2.3): a worker whose attempt is `active` with no heartbeat for 10 minutes now carries `stale`, which is a new category, so the existing difference check returns `WokeOn::Attention`.
- [ ] **Step 4: Add the stale test** in `tests/engine_waiting.rs`: seed an active worker, advance the fake clock 11 minutes before the wait, set `tick_interval` 200 ms, wait 1500 ms, assert `WokeOn::Attention` and that the attention list names category `stale`. Run both tests and the whole suite.
- [ ] **Step 5: Commit:** `git commit -am "feat(waiting): the waiting coordinator ticks the ladder and wakes on stale"`.

### Task 3: `pod usage --tick`

- [ ] **Step 1: Write the failing tests.** In `tests/usage.rs` extend `install_wires_claude_statusline_and_herdr_bar_once_with_a_backup`: the installed entry's command is `pod usage --tick` (change the assertion from `command = "pod usage"` to `command = "pod usage --tick"`; `herdr_bar_installed` must still return true for an entry whose command contains `pod usage`). In `tests/cli_usage.rs` add:

```rust
#[test]
fn usage_tick_prints_the_line_and_runs_at_most_every_thirty_seconds() {
    let tmp = tempfile::tempdir().unwrap();
    let out = pod(tmp.path()).args(["usage", "--tick", "--json"]).assert().success().get_output().stdout.clone();
    let value: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(value["tick"]["ran"], true);
    let again = pod(tmp.path()).args(["usage", "--tick", "--json"]).assert().success().get_output().stdout.clone();
    let value: Value = serde_json::from_slice(&again).unwrap();
    assert_eq!(value["tick"]["ran"], false, "a second tick inside 30 s is skipped");
    assert!(value["line"].as_str().unwrap().contains("codex"));
}
```

The CLI test has no herdr: `--tick` must print the line and report `ran: false` with `reason: "herdr unreachable"` when the engine cannot open; make the first assertion `value["tick"]["ran"] == true` only when a herdr is present, else assert `value["tick"]["reason"]` is `"herdr unreachable"`. Write the test with that branch, since CI has no herdr.
- [ ] **Step 2: Run:** fails (`--tick` unknown).
- [ ] **Step 3: Implement** in `env.rs` `usage()`: add `#[arg(long)] pub tick: bool`. After computing `snapshot` and before printing, when `args.tick`:

```rust
let tick = match ctx.engine() {
    Err(_) => json!({"ran": false, "reason": "herdr unreachable"}),
    Ok(engine) => {
        let stamp = paths.state_dir.join("tick.at");
        let due = std::fs::read_to_string(&stamp).ok()
            .and_then(|text| text.trim().parse::<DateTime<Utc>>().ok())
            .is_none_or(|last| now.signed_duration_since(last).num_seconds() >= 30);
        if !due { json!({"ran": false, "reason": "ticked less than 30 s ago"}) } else {
            let _ = std::fs::write(&stamp, now.to_rfc3339());
            match wake::tick_all(&engine, wake::Trigger::BarTick) {
                Ok(reports) => json!({"ran": true, "runs": reports.len(), "attempts": reports.iter().map(|(_, r)| r.attempts.len()).sum::<usize>()}),
                Err(error) => json!({"ran": false, "reason": error.message}),
            }
        }
    }
};
```

Plain output stays the bar line only; `--json` adds `tick`. In `install.rs` the entry command becomes `pod usage --tick` (the `herdr_bar_installed` check already uses `contains("pod usage")`). The tick must never run `reconcile` and must return inside the bar's 5-second timeout: `BarTick` cannot wait, so every quiet check returns `NotYetProven` at once.
- [ ] **Step 4: Run** the suite; then on this machine run `pod usage --install` once so the live config entry becomes `pod usage --tick` (it is idempotent and reloads herdr).
- [ ] **Step 5: Commit:** `git commit -am "feat(usage): --tick runs the ladder from the tab bar when nobody waits"`.

### Task 4: Docs and spec

- [ ] **Step 1:** spec 3.2: add a bullet "**Ticking**: while a coordinator waits, its process runs one ladder pass per minute; the tab bar entry (`pod usage --tick`) runs one every 30 seconds when nobody waits." Spec 8.2: replace "There is no timer: without another eligible trigger it stays where it is, so exhaustion is a count, not an elapsed time." with "There is no timer outside a bounded wait and the tab bar tick, which are eligible triggers; exhaustion is still a count, not an elapsed time." Add `stale` wakes the waiter to 8.3 where `wokeOn: attention` is described.
- [ ] **Step 2:** `docs/reference/commands.md` (`pod usage` section: `--tick`), `docs/reference/commands-messages.md` (`check --wait` ticks every 60 s and wakes on `stale`), `CHANGELOG.md` 0.2.0 line.
- [ ] **Step 3:** `cargo test --release --test docs_sync`, line counts, commit: `git commit -am "docs: the clock inside the wait and the tab bar tick"`.
