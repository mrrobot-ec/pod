# Lane A1: Injection through `herdr agent prompt`

Spec: `docs/superpowers/specs/2026-10-14-hardening-design.md` section A1. Read the Global Constraints in `00-index.md` first.

**Goal:** every worker gets its brief as a file and a one-line pointer typed through herdr's `agent.prompt`; hand paste becomes the fallback; the start receipt says which path ran.

**Files:**
- Modify: `src/engine/safe_input.rs` (new `prompt_or_paste`, `inject_preamble` takes the handle)
- Modify: `src/engine/lifecycle.rs` (`inject`, three call sites, `LaunchReport.inject`, `brief_path`)
- Modify: `src/engine/wake.rs` (nudges call `prompt_or_paste`)
- Modify: `src/herdr/fake.rs` (`agent_prompt` records text, `fail_agent_prompt`)
- Modify: `docs/reference/commands-workers.md`, `docs/reference/herdr-adapter.md`, spec 7.1 step 6, 8.2, 19 row; `CHANGELOG.md`
- Test: `tests/engine_preamble.rs`, `tests/engine_wake.rs`, `tests/engine_lifecycle.rs`

**Interfaces:**
- Produces `pub fn brief_path(paths: &Paths, run: &RunId, dispatch: &DispatchId) -> PathBuf` in `src/engine/lifecycle.rs` (A3 uses it).
- Produces `pub enum InjectPath { Prompt, Paste }` and `pub fn prompt_or_paste(engine: &Engine, handle: &str, pane_id: &str, text: &str) -> Result<(InputOutcome, InjectPath)>` in `src/engine/safe_input.rs` (A4 hooks its screen read into it).
- One important correction to the spec text: herdr submits the text before it waits, so `PromptOutcome::Stalled` means Enter was already pressed. It maps to `InputOutcome::RenderedUnconfirmed` (start state `dispatched_unobserved`), never to a second paste. Only a transport error from `agent_prompt` falls back to paste.

### Task 1: The fake records what was prompted, and can fail

- [ ] **Step 1: Write the failing test** in `tests/engine_wake.rs` (bottom):

```rust
#[test]
fn fake_agent_prompt_records_text_as_a_submission_and_can_fail() {
    let herdr = pod::herdr::fake::FakeHerdr::new();
    herdr.add_pane("p1", "t1", "ws");
    herdr.set_agent("w1", pod::herdr::AgentInfo {
        name: "w1".into(), kind: "claude".into(), pane_id: "p1".into(), pid: 7,
        status: pod::herdr::AgentStatus::Idle, session_id: None, session_path: None,
        needs_input: Vec::new(), observed_at: chrono::Utc::now(),
    });
    assert_eq!(herdr.agent_prompt("w1", "hello", Some(5000)).unwrap(), pod::herdr::PromptOutcome::Submitted);
    assert!(herdr.calls().iter().any(|c| c == "agent_prompt:w1:hello"));
    assert_eq!(herdr.submissions(), vec![("p1".to_string(), "hello".to_string())]);
    herdr.fail_agent_prompt("agent.prompt is not available");
    assert!(herdr.agent_prompt("w1", "again", Some(5000)).is_err());
}
```

- [ ] **Step 2: Run it:** `cargo test --release --test engine_wake fake_agent_prompt` → fails (`fail_agent_prompt` missing, call string differs).
- [ ] **Step 3: Implement** in `src/herdr/fake.rs`: add `agent_prompt_error: Option<String>` to `State` (init `None`), `pub fn fail_agent_prompt(&self, message: &str)` that sets it; in `agent_prompt` record `format!("agent_prompt:{name}:{text}")`, return `Err(PodError::new(ErrorCode::HerdrError, message, &[]))` when the error is set (consume it once), else push `(pane_id of the agent, text)` onto `submissions` and pop the scripted outcome as today. Keep the pane lookup through `state.agents.get(name)`; an unknown agent returns `Err(... UnknownAgent)`.
- [ ] **Step 4: Run the whole wake suite:** `cargo test --release --test engine_wake` → passes (existing tests use `starts_with("agent_prompt:")`; fix any exact-match assertion to `starts_with`).
- [ ] **Step 5: Commit:** `git commit -am "test(fake): agent_prompt records the text and can be scripted to fail"`.

### Task 2: `prompt_or_paste` and the brief file for every kind

- [ ] **Step 1: Write the failing tests** in `tests/engine_preamble.rs`:

```rust
#[test]
fn every_kind_gets_a_brief_file_and_the_pointer_goes_through_agent_prompt() {
    // seed as in `claude_workers_always_get_the_brief_as_a_file`, agent "codex"
    let receipt = worker_start(&env.engine, start(task.id, run.id)).unwrap();
    assert_eq!(receipt.state, "ready");
    assert_eq!(receipt.launch.inject.as_deref(), Some("prompt"));
    let effect = receipt.effects.iter().find(|e| e.kind == "preamble_file").unwrap();
    assert_eq!(
        effect.id.as_deref().map(PathBuf::from),
        Some(pod::engine::lifecycle::brief_path(&env.engine.paths, &receipt.run_id, &receipt.dispatch_id))
    );
    let calls = env.herdr.calls();
    assert!(calls.iter().any(|c| c.starts_with(&format!("agent_prompt:{}:The full worker brief is stored at", receipt.handle))));
    assert!(!calls.iter().any(|c| c.starts_with("pane_send_text")), "no hand paste on the prompt path");
}

#[test]
fn a_stalled_prompt_is_dispatched_unobserved_and_never_pasted_twice() {
    env.herdr.script_prompt_outcome(pod::herdr::PromptOutcome::Stalled);
    let receipt = worker_start(&env.engine, start(task.id, run.id)).unwrap();
    assert_eq!(receipt.outcome.as_deref(), Some("dispatched_unobserved"));
    assert_eq!(env.herdr.submissions().len(), 1);
    assert!(!env.herdr.calls().iter().any(|c| c.starts_with("pane_send_text")));
}

#[test]
fn a_prompt_transport_error_falls_back_to_safe_paste() {
    env.herdr.fail_agent_prompt("agent.prompt unsupported");
    let receipt = worker_start(&env.engine, start(task.id, run.id)).unwrap();
    assert_eq!(receipt.state, "ready");
    assert_eq!(receipt.launch.inject.as_deref(), Some("paste"));
    assert!(env.herdr.calls().iter().any(|c| c.starts_with("pane_send_text")));
}
```

Note: a paste that was rendered but not confirmed already yields `receipt.outcome == Some("dispatched_unobserved")` (`lifecycle.rs` around the comment "not confirmed is `dispatched_unobserved`"); a stalled prompt reuses that exact mapping, so look there before writing new state names.

- [ ] **Step 2: Run:** `cargo test --release --test engine_preamble` → the three fail (`launch.inject` missing, `brief_path` missing, prompt not called).
- [ ] **Step 3: Implement.**
  1. `lifecycle.rs`: `pub fn brief_path(paths: &Paths, run: &RunId, dispatch: &DispatchId) -> PathBuf { paths.board_dir.join(run.as_str()).join("preambles").join(format!("{}.txt", dispatch.as_str())) }`; make `write_preamble_pointer` use it. Add `#[serde(default, skip_serializing_if = "Option::is_none")] pub inject: Option<String>` to `LaunchReport`; fill it from the worker's effects (`inject:prompt` / `inject:paste`) where the receipt is built.
  2. `inject(...)` gains `handle: &str`, always writes the brief file (drop the `PREAMBLE_LIMIT`/`kind == "claude"` condition; keep the constant only if still referenced), records the `preamble_file` effect, then calls `safe_input::inject_preamble(engine, dispatch, handle, &pointer_text)`.
  3. `safe_input.rs`:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InjectPath { Prompt, Paste }

const PROMPT_WAIT_MS: u64 = 5_000;

/// herdr's own submission first (text and Enter as one ordered write, even
/// while the agent works); hand paste only when herdr cannot prompt at all.
pub fn prompt_or_paste(engine: &Engine, handle: &str, pane_id: &str, text: &str) -> Result<(InputOutcome, InjectPath)> {
    match engine.herdr.agent_prompt(handle, text, Some(PROMPT_WAIT_MS)) {
        Ok(PromptOutcome::Submitted) => Ok((InputOutcome::Delivered, InjectPath::Prompt)),
        Ok(PromptOutcome::Blocked) => Ok((InputOutcome::Skipped("agent is blocked at a prompt (agent_blocked)".into()), InjectPath::Prompt)),
        // The text and Enter went in; no turn was observed. Never type again.
        Ok(PromptOutcome::Stalled) => Ok((InputOutcome::RenderedUnconfirmed, InjectPath::Prompt)),
        Err(_) => {
            wait_until_ready(engine.herdr.as_ref(), pane_id, Duration::from_millis(READY_WAIT_MS), engine.input_settle);
            Ok((type_and_submit(engine.herdr.as_ref(), pane_id, text)?, InjectPath::Paste))
        }
    }
}
```

  Move `READY_WAIT_MS` from `lifecycle.rs` to `safe_input.rs` (pub const) if `inject` no longer needs it. `inject_preamble(engine, dispatch, handle, text)` keeps its lock and "already attempted" check, calls `prompt_or_paste`, records `preamble_input` as today, and appends `inject:prompt` or `inject:paste` to the worker's effects in the same transaction. Update the three `inject(` call sites (`ctx.handle`, `handle`, `agent.name`).
- [ ] **Step 4: Run** `cargo test --release` (whole suite, herdr env unset). Existing tests that asserted `pane_send_text` for a preamble now see `agent_prompt`; update them to assert the prompt call, not the paste. Tests that exercise the paste rules keep using `type_and_submit` directly.
- [ ] **Step 5: Commit:** `git commit -am "feat(launch): briefs always as a file, pointer through herdr agent prompt"`.

### Task 3: Nudges go through the same path

- [ ] **Step 1: Write the failing test** in `tests/engine_wake.rs`, next to the existing nudge tests (reuse their setup helper that seeds a worker and sends it a message):

```rust
#[test]
fn a_nudge_is_prompted_not_pasted_and_a_stalled_nudge_counts_as_an_attempt() {
    // seed: worker "w1" idle_at_prompt, present, fresh; one unread message in dispatch:<id>
    let row = wake::on_trigger(&env.engine, &mailbox, Trigger::IdleHook).unwrap().unwrap();
    assert_eq!(row.outcome, "delivered");
    assert!(env.herdr.calls().iter().any(|c| c.starts_with("agent_prompt:w1:You have 1 orchestration message")));
    env.herdr.script_prompt_outcome(pod::herdr::PromptOutcome::Stalled);
    env.clock.advance(std::time::Duration::from_secs(61));
    let row = wake::on_trigger(&env.engine, &mailbox, Trigger::IdleHook).unwrap().unwrap();
    assert_eq!((row.attempt, row.outcome.as_str()), (2, "rendered_unconfirmed"));
}
```

- [ ] **Step 2: Run:** fails (nudge still pastes).
- [ ] **Step 3: Implement:** in `wake::preconditions_and_input`, replace the final `safe_input::type_and_submit(...)` with `safe_input::prompt_or_paste(engine, &recipient.handle, &recipient.pane_id, &text)` mapped to its outcome (discard the path); `Recipient` gains `handle: String` filled from the agent name in `recipient()`. Keep every precondition above it unchanged.
- [ ] **Step 4: Run** `cargo test --release --test engine_wake` and the whole suite → green.
- [ ] **Step 5: Commit:** `git commit -am "feat(wake): nudges go through herdr agent prompt"`.

### Task 4: Docs

- [ ] **Step 1:** `docs/reference/commands-workers.md`: replace the injection paragraph (the one starting "Before the preamble is pasted") with: every worker's brief is written to `<state dir>/runs/<run>/preambles/<dispatch>.txt` (effect `preamble_file`), the one-line pointer is submitted through herdr `agent.prompt`, which types text and Enter as one submission; `launch.inject` on the receipt is `prompt` or `paste`; `agent_prompt_stalled` means Enter was pressed but no turn was seen, reported as `dispatched_unobserved`; hand paste with the settle window and the Claude tail rule runs only when herdr cannot prompt.
- [ ] **Step 2:** spec `docs/specs/2026-10-05-pod-design.md`: 7.1 step 6 and the 8.2 paragraph that describes injection now say the same; the section 19 row for `herdr agent prompt` becomes "it is the primary path; paste is the fallback". `docs/reference/herdr-adapter.md` row for `agent_prompt`: remove "this is not the 8.2 safe-input procedure" and state the mapping above.
- [ ] **Step 3:** `CHANGELOG.md` under `## [0.2.0] - Unreleased`: one line for this lane.
- [ ] **Step 4:** run `cargo test --release --test docs_sync --test cli_skills`, the docs line-count check (`wc -l docs/**/*.md`), and commit: `git commit -am "docs: injection through herdr agent prompt"`.
