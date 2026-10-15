# Lane A4: `interruption` and `guidance`

Spec: `docs/superpowers/specs/2026-10-14-hardening-design.md` section A4. Depends on lane A1 (`safe_input::prompt_or_paste`). Read the Global Constraints in `00-index.md` first.

**Goal:** a person typing into a worker pane is seen and left alone (`interruption`), and a worker whose question or escalation sits unanswered for five minutes shows `guidance` for the coordinator.

**Files:**
- Modify: `src/engine/safe_input.rs` (screen read before a prompt), `src/engine/observe.rs` (`record_user_typing`, the two categories in `project`), `src/engine/wake.rs` (skip reason), `src/store/rows/observations.rs` only if a helper is missing
- Modify: spec 7.4 and 7.6, `docs/guides/coordinator.md` (attention table), `docs/reference/commands-workers.md`, `docs/reference/state-machines.md` if it lists categories, `CHANGELOG.md`
- Test: `tests/engine_observe.rs`, `tests/engine_wake.rs`

**Interfaces:**
- Produces `pub fn record_user_typing(tx: &Tx<'_>, now: DateTime<Utc>, dispatch: &DispatchId, text: Option<&str>) -> Result<()>` in `observe.rs`: `Some(text)` records an observation whose `reason` is `user_typing: <first 80 chars>`; `None` records the clearing observation (reason `None`), both with `evidence_source = "herdr_state"` and the latest known liveness/activity/presence copied from the previous observation.
- Produces `pub fn foreign_input(agent: &AgentInfo, screen: &str) -> Option<String>` in `safe_input.rs`: the input line's text when it is neither empty nor the agent's placeholder (reuses `input_line` and `is_placeholder`). The screen it inspects must come from `safe_input::screen(herdr, pane, lines)`, which reads the pane with styling kept and drops dim runs: Claude Code draws its prompt suggestion ("yes, execute the plan") dim, and that is never a person typing (hotfix of 2026-10-14, test `safe_input_ignores_a_dim_composer_suggestion`). Add a test that a dim suggestion gives `None` and typed text gives `Some`.
- `project()` adds categories: `interruption` (latest observation reason starts with `user_typing:`; `requires_action` unchanged; `next_action` `worker-read`), `guidance` (an unanswered `question` or `escalation` from this Dispatch older than 5 minutes; `requires_action = true`; `next_action` `reply --id <message_id>` for a question, `worker-read` for an escalation).

### Task 1: `foreign_input` and the pre-prompt screen read

- [ ] **Step 1: Write the failing tests** in `tests/engine_wake.rs` (the `MinimalHerdr`, `claude_screen`, `codex_screen` helpers are there):

```rust
#[test]
fn foreign_input_is_the_text_a_person_typed() {
    use pod::engine::safe_input::foreign_input;
    let agent = |kind: &str| pod::herdr::AgentInfo { name: "w".into(), kind: kind.into(), pane_id: "p".into(), pid: 1,
        status: pod::herdr::AgentStatus::Idle, session_id: None, session_path: None, needs_input: Vec::new(), observed_at: chrono::Utc::now() };
    assert_eq!(foreign_input(&agent("claude"), &claude_screen("")), None);
    assert_eq!(foreign_input(&agent("codex"), &codex_screen("Ask Codex to do anything")), None);
    assert_eq!(foreign_input(&agent("claude"), &claude_screen("fix the tests first")).as_deref(), Some("fix the tests first"));
}

#[test]
fn a_prompt_is_withheld_while_a_person_is_typing_and_recorded_as_interruption() {
    let (env, run, mailbox) = seeded_worker_with_unread_mail();
    env.herdr.set_screen("pane_w1", &claude_screen("let me finish this thought"));
    let row = wake::on_trigger(&env.engine, &mailbox, Trigger::IdleHook).unwrap().unwrap();
    assert_eq!(row.outcome, "skipped");
    assert!(row.detail.as_deref().unwrap().contains("person is typing"));
    assert!(!env.herdr.calls().iter().any(|c| c.starts_with("agent_prompt:")));
    let view = pod::engine::lifecycle::worker_show(&env.engine, &dispatch_of(&mailbox)).unwrap();
    assert!(view.projection.attention.categories.contains(&"interruption".to_string()));
    assert_eq!(view.projection.next_action.kind, "worker-read");
    // The person deleted the text: the next read clears it and the nudge goes through.
    env.herdr.set_screen("pane_w1", &claude_screen(""));
    env.clock.advance(std::time::Duration::from_secs(61));
    let row = wake::on_trigger(&env.engine, &mailbox, Trigger::IdleHook).unwrap().unwrap();
    assert_eq!(row.outcome, "delivered");
    let view = pod::engine::lifecycle::worker_show(&env.engine, &dispatch_of(&mailbox)).unwrap();
    assert!(!view.projection.attention.categories.contains(&"interruption".to_string()));
}
```

Use the real pane id the seeding helper creates and the real `worker_show` engine function name (`grep -n "pub fn worker_show" src/engine/lifecycle.rs`); `NextAction` field names come from `Projection` in `observe.rs`.
- [ ] **Step 2: Run:** fails.
- [ ] **Step 3: Implement:** in `safe_input.rs`:

```rust
/// Text on the input line that pod did not put there.
pub fn foreign_input(agent: &AgentInfo, screen: &str) -> Option<String> {
    let lines = screen.lines().collect::<Vec<_>>();
    let (_, rest) = input_line(&agent.kind, &lines)?;
    (!rest.is_empty() && !is_placeholder(&agent.kind, rest)).then(|| rest.to_owned())
}
```

`prompt_or_paste` gains a leading step: read the agent (`agent_list` find by pane) and the pane (`pane_read(pane, 20)`); when `foreign_input` is `Some(text)`, return `(InputOutcome::Skipped(format!("person is typing: {}", text.chars().take(80).collect::<String>())), InjectPath::Prompt)` without prompting. It also calls `observe::record_user_typing` through a new callback: give `prompt_or_paste` an extra parameter `dispatch: Option<&DispatchId>` and record `Some(text)` or `None` (clearing) inside `engine.store.tx` when a dispatch is given; A1's two callers pass the Dispatch. `record_user_typing` in `observe.rs` inserts an `ObservationRow` as described in Interfaces (copy liveness, activity, presence, needs_input, herdr_status from `observations::latest(tx, dispatch)` or whatever the latest-row reader is called; default `unverifiable`/`unknown`/`None`/`[]`).
- [ ] **Step 4: `project()`:** after the evidence rows are read, if the latest observation's `reason` starts with `user_typing:` push `"interruption"` (no `requires_action` change) and set `next_action` to `worker-read` unless a stronger category already set one. Run the suite.
- [ ] **Step 5: Commit:** `git commit -am "feat(observe): interruption when a person types into a worker pane"`.

### Task 2: `guidance`

- [ ] **Step 1: Write the failing test** in `tests/engine_observe.rs` (its helpers seed a run, a task, a dispatched worker; `messages::send` or the engine's `send` is how a worker question is stored; look at `tests/engine_questions.rs` for a worker `ask`):

```rust
#[test]
fn an_unanswered_worker_question_becomes_guidance_after_five_minutes() {
    let (env, run, dispatch) = seeded_dispatched_worker();
    let question = ask_as_worker(&env, &dispatch, "Rate limit by key or by IP?"); // returns MessageId
    let fresh = project_now(&env, &dispatch);
    assert!(!fresh.attention.categories.contains(&"guidance".to_string()));
    env.clock.advance(std::time::Duration::from_secs(5 * 60 + 1));
    let late = project_now(&env, &dispatch);
    assert!(late.attention.categories.contains(&"guidance".to_string()));
    assert!(late.attention.requires_action);
    assert_eq!(late.next_action.argv, vec!["reply", "--id", question.as_str()]);
    reply_as_coordinator(&env, &question, "by key");
    assert!(!project_now(&env, &dispatch).attention.categories.contains(&"guidance".to_string()));
}
```

Write `ask_as_worker`, `reply_as_coordinator`, and `project_now` as test helpers over the existing engine functions (`engine::questions::ask`, `engine::questions::reply`, `observe::project` inside `store.read`), matching their real signatures.
- [ ] **Step 2: Run:** fails.
- [ ] **Step 3: Implement** in `project()`: query the Run mailbox for the oldest message with `type IN ('question','escalation')` whose `payload.dispatch_id` equals this Dispatch, `created_at` older than 5 minutes, and no answer: for a question, `questions.status != 'answered'` (table `questions`, column names in `schema.rs`); for an escalation, no later message whose `mailbox = 'dispatch:<id>'` was sent after it. Push `guidance`, set `requires_action`, and `next_action` as in Interfaces. One SQL per kind is fine; keep it under the 50 ms budget (indexes exist on mailbox and created_at; check `schema.rs`).
- [ ] **Step 4: Run** the suite and clippy.
- [ ] **Step 5: Commit:** `git commit -am "feat(observe): guidance for a worker question left unanswered"`.

### Task 3: Docs

- [ ] **Step 1:** spec 7.6: define both categories with their evidence and next actions as in Interfaces; 7.4: a pane a person types into is left alone until bare, and is not user-owned by that alone. Coordinator guide attention table: two rows. `docs/reference/commands-workers.md` projection section: the two categories. Implementation notes: strike deferral items 2 and 4 with the date. `CHANGELOG.md` 0.2.0 line.
- [ ] **Step 2:** `cargo test --release --test docs_sync --test cli_skills`, line counts, commit: `git commit -am "docs: interruption and guidance"`.
