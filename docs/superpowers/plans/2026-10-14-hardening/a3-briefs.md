# Lane A3: Briefs that survive compaction

Spec: `docs/superpowers/specs/2026-10-14-hardening-design.md` section A3. Depends on lane A1 (every worker has a brief file at `lifecycle::brief_path`). Read the Global Constraints in `00-index.md` first.

**Goal:** a worker that lost its context can find its brief: every nudge names the file, `worker-show` prints `briefPath`, the preamble says where it is stored, and the worker guide says what to do after compaction.

**Files:**
- Modify: `src/engine/wake.rs` (nudge text), `src/engine/preamble.rs` (first paragraph), `src/engine/lifecycle.rs` (`WorkerView.brief_path`), `src/cli/commands/workers.rs` (`briefPath` in `worker-show` and `worker-list` rows)
- Modify: `docs/guides/worker.md`, `docs/reference/commands-workers.md`, `docs/reference/commands-messages.md` (nudge text), spec section 10 preamble template, `tests/engine_preamble_golden.txt`, `CHANGELOG.md`
- Test: `tests/engine_wake.rs`, `tests/engine_preamble.rs`, `tests/cli_workers.rs`

**Interfaces:**
- Consumes `lifecycle::brief_path(&Paths, &RunId, &DispatchId) -> PathBuf` from A1.
- Produces `pub fn nudge_text(count: usize, handle: Option<&str>, brief: Option<&Path>) -> String` in `wake.rs`.

### Task 1: Nudge text names the handle and the brief

- [ ] **Step 1: Write the failing test** in `tests/engine_wake.rs`:

```rust
#[test]
fn nudge_text_names_the_handle_and_the_brief_only_when_they_exist() {
    use std::path::Path;
    assert_eq!(
        wake::nudge_text(2, Some("codex-limiter"), Some(Path::new("/s/runs/r/preambles/ctx_1.txt"))),
        "You have 2 orchestration message(s). Run `pod check --terminal codex-limiter`. Your brief: /s/runs/r/preambles/ctx_1.txt."
    );
    // The Run mailbox (the coordinator) has neither a worker handle nor a brief.
    assert_eq!(wake::nudge_text(1, None, None), "You have 1 orchestration message(s). Run `pod check`.");
}

#[test]
fn a_worker_nudge_carries_its_brief_path() {
    let (env, run, mailbox) = seeded_worker_with_unread_mail(); // from the existing tests
    let dispatch = mailbox.trim_start_matches("dispatch:");
    wake::on_trigger(&env.engine, &mailbox, Trigger::IdleHook).unwrap();
    let expected = pod::engine::lifecycle::brief_path(&env.engine.paths, &run, &DispatchId::from_str(dispatch).unwrap());
    assert!(env.herdr.calls().iter().any(|c| c.starts_with("agent_prompt:") && c.contains(&format!("Your brief: {}", expected.display()))));
}
```

- [ ] **Step 2: Run:** fails (`nudge_text` missing).
- [ ] **Step 3: Implement** in `wake.rs`:

```rust
pub fn nudge_text(count: usize, handle: Option<&str>, brief: Option<&Path>) -> String {
    let mut text = match handle {
        Some(handle) => format!("You have {count} orchestration message(s). Run `pod check --terminal {handle}`."),
        None => format!("You have {count} orchestration message(s). Run `pod check`."),
    };
    if let Some(brief) = brief { text.push_str(&format!(" Your brief: {}.", brief.display())); }
    text
}
```

In `preconditions_and_input`, build the text with the recipient's handle and, for a `dispatch:<id>` mailbox, `Some(brief_path(&engine.paths, run, &dispatch))` only when that file exists (`Path::exists`); the Run mailbox passes `None, None`. The `Recipient` struct already carries `handle` after A1.
- [ ] **Step 4: Run** `cargo test --release --test engine_wake`; fix any test that asserted the old nudge text literally.
- [ ] **Step 5: Commit:** `git commit -am "feat(wake): nudges name the worker handle and its brief file"`.

### Task 2: `briefPath` on `worker-show` and `worker-list`, and in the preamble

- [ ] **Step 1: Write the failing tests.** In `tests/cli_workers.rs` find the test that reads `worker-show --json` output and add `assert!(value["briefPath"].as_str().unwrap().ends_with(&format!("preambles/{}.txt", dispatch)))`; for `worker-list` rows assert the same key. In `tests/engine_preamble.rs` the golden test: update `tests/engine_preamble_golden.txt` so the first paragraph reads `Please carry out this task from my pod coordinator by following the brief below. A copy is stored at <BRIEF>; read it again after a context reset or compaction.` and replace `<BRIEF>` in the test with `brief_path(...)` the same way `<TASK>` is replaced.
- [ ] **Step 2: Run:** fails.
- [ ] **Step 3: Implement:** `WorkerView` gains `pub brief_path: PathBuf` filled by `brief_path(&engine.paths, &dispatch.run_id, &dispatch.id)` wherever views are built (`worker_show`, `worker_list`); the CLI JSON adds `"briefPath": view.brief_path`. `PreambleContext` gains `brief_path: PathBuf`, `render_preamble` prints it in the first paragraph; `preamble_for` fills it.
- [ ] **Step 4: Run** the whole suite (`cargo test --release`) and clippy.
- [ ] **Step 5: Commit:** `git commit -am "feat(workers): briefPath on worker-show and in the preamble"`.

### Task 3: Guides and docs

- [ ] **Step 1:** `docs/guides/worker.md`: under the rules, add "**After a context reset or compaction**, read your brief again: the file named in the last nudge (`Your brief: <path>`) or in `pod worker-show --dispatch <id>` as `briefPath`. Your handle, Run, Task, and Dispatch IDs are in it." Keep the guide at or under 400 lines; the `cli_skills` test compares the served skill with this file.
- [ ] **Step 2:** `docs/reference/commands-messages.md`: the nudge text paragraph shows the new text with its two optional parts. `docs/reference/commands-workers.md`: `briefPath` in the `worker-show` and `worker-list` field lists. Spec section 10: the template's first paragraph as above. `CHANGELOG.md` 0.2.0 line.
- [ ] **Step 3:** `cargo test --release --test docs_sync --test cli_skills`, line counts, commit: `git commit -am "docs: briefs after compaction"`.
