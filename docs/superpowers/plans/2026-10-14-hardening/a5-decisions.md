# Lane A5: Decisions visible away from the terminal

Spec: `docs/superpowers/specs/2026-10-14-hardening-design.md` section A5. No dependency on other lanes. Read the Global Constraints in `00-index.md` first.

**Goal:** an untasked user gate reaches the board and the sidebar, and every new decision or newly stuck worker fires a herdr notification, with an optional push command from config.

**Files:**
- Modify: `src/board/mod.rs` (untasked gates as `question` cards; `sidebar_statuses` DECISION prefix)
- Modify: `src/herdr/mod.rs`, `src/herdr/socket.rs`, `src/herdr/fake.rs`, `tests/engine_wake.rs` `MinimalHerdr` (new `notification_show`)
- Modify: `src/config.rs` (`[notify] command`), `src/engine/mod.rs` (`notify`), `src/engine/gates.rs` or `src/cli/commands/gates.rs` (gate creation notifies), `src/engine/waiting.rs` (newly stuck workers notify when the wait returns `wokeOn: attention`)
- Modify: `docs/reference/config.md`, `docs/reference/commands.md` (`gate-create`, `check --wait` notifications), spec 9.1 and 12, implementation notes deferral 5, `CHANGELOG.md`
- Test: `tests/board_export.rs`, `tests/engine_gates.rs`, `tests/engine_waiting.rs`, `tests/herdr_socket.rs`

**Interfaces:**
- Produces `fn notification_show(&self, title: &str, body: Option<&str>, sound: &str) -> Result<()>` on the `Herdr` trait. Socket: `notification.show {title, body, sound}`; the reply type is not in the schema's constants, so run `herdr notification show "pod probe" --body probe` once on this machine and use the `type` it prints (expected `ok`). Fake records `notification_show:<title>:<body>:<sound>`.
- Produces `pub struct NotifyConfig { pub command: Vec<String> }` under `Config.notify` (default empty) and `Engine::notify(&self, title: &str, body: &str)`: herdr first, then the command with `NOTIFY_TITLE` and `NOTIFY_BODY` in its environment, 5-second timeout, every failure a warning on stderr.
- Board card for an untasked gate: `id: "gate:<gate_id>"`, `kind: "question"`, `state: "blocked"`, `unblock: "user"`, `owner: "user"`, `title: <question, 80 chars>`, `criteria: <question>`, `note: <evidence and recommendation as for tasked gates>`, no parent, attached to the coordinator's synthetic attempt like tasked gates (read how `coordinator` attempts are emitted in `export`). It must pass `dagr check --strict`; the board tests already run it when `dagr` is on PATH.

### Task 1: Untasked user gates on the board and in the sidebar

- [ ] **Step 1: Write the failing tests** in `tests/board_export.rs` (its helpers create a run, tasks, and call `board::export`):

```rust
#[test]
fn an_untasked_user_gate_is_a_question_card_and_a_decision_sidebar_status() {
    let (env, run) = seeded_run_with_one_active_worker();
    let gate = env.engine.store.tx(|tx| gates::create(tx, env.engine.clock.now(), &run, "user", None,
        "Ship with the old limiter or wait for the new one?", &["ship".into(), "wait".into()],
        Some("tests pass on both"), Some("ship"), true)).unwrap();
    let value = env.engine.store.read(|tx| board::export(tx, &run, env.engine.clock.now())).unwrap();
    let card = value["tasks"].as_array().unwrap().iter()
        .find(|t| t["id"] == format!("gate:{}", gate.id)).expect("gate card");
    assert_eq!((card["kind"].as_str(), card["state"].as_str(), card["unblock"].as_str()), (Some("question"), Some("blocked"), Some("user")));
    assert_eq!(card["criteria"], "Ship with the old limiter or wait for the new one?");
    let statuses = env.engine.store.read(|tx| board::sidebar_statuses(tx, &run)).unwrap();
    assert!(statuses.iter().all(|(_, status)| status.as_deref().is_some_and(|s| s.starts_with("DECISION · Ship with the old limiter"))), "{statuses:?}");
    env.engine.store.tx(|tx| gates::resolve(tx, env.engine.clock.now(), &gate.id, "ship", /* caller args as the real signature needs */)).unwrap();
    let value = env.engine.store.read(|tx| board::export(tx, &run, env.engine.clock.now())).unwrap();
    assert!(value["tasks"].as_array().unwrap().iter().all(|t| t["id"] != format!("gate:{}", gate.id)));
    let statuses = env.engine.store.read(|tx| board::sidebar_statuses(tx, &run)).unwrap();
    assert!(statuses.iter().all(|(_, status)| !status.as_deref().unwrap_or("").starts_with("DECISION")));
}
```

Match `gates::create` and `gates::resolve` to their real signatures (`grep -n "pub fn create\|pub fn resolve" -A12 src/engine/gates.rs`).
- [ ] **Step 2: Run:** `cargo test --release --test board_export` → fails.
- [ ] **Step 3: Implement** in `board/mod.rs`: a second query for pending gates `WHERE run_id = ?1 AND status = 'pending' AND task_id IS NULL AND audience = 'user'`, each becoming the card described in Interfaces (build `note` with the same helper the tasked gates use), pushed after the task cards. In `sidebar_statuses`, when any pending user gate exists (tasked or not), prefix every workspace status with `DECISION · <question truncated to 40 chars> · ` (keep the existing `<task> · <phase>` after it) and emit a status for the Run's workspaces even when no worker is active: when the statuses list is empty, return one entry per workspace of the Run's live workers; if there are none, return nothing (the notification covers that case).
- [ ] **Step 4: Run** `cargo test --release --test board_export` (with `dagr` on PATH the strict check runs) and the whole suite.
- [ ] **Step 5: Commit:** `git commit -am "feat(board): untasked user gates reach the board and the sidebar"`.

### Task 2: `notification_show` on the adapter

- [ ] **Step 1: Write the failing test** in `tests/herdr_socket.rs` (copy the `MockServer::one` pattern used for `workspace_report_metadata`): the request method is `notification.show`, params `{"title": "pod: decision", "body": "Ship or wait?", "sound": "request"}`, reply `{"type": "<the type you probed>"}`, and the call returns `Ok(())`.
- [ ] **Step 2: Run:** fails (method missing).
- [ ] **Step 3: Implement** the trait method, the socket call through `self.expected(...)`, the fake recording, and the `MinimalHerdr` stub in `tests/engine_wake.rs` (`self.unsupported()`).
- [ ] **Step 4: Run** `cargo test --release --test herdr_socket --test engine_wake`.
- [ ] **Step 5: Commit:** `git commit -am "feat(herdr): notification_show"`.

### Task 3: Notify on new gates and newly stuck workers

- [ ] **Step 1: Write the failing tests.** In `tests/engine_gates.rs`: creating a user gate through the engine path the CLI uses (`gate-create`) records `notification_show:<run objective>:<question>:request` in `env.herdr.calls()`; a coordinator gate records nothing. In `tests/engine_waiting.rs`: a `check_wait` that returns `WokeOn::Attention` because a worker newly carries `requiresAction: true` (the existing attention tests show how to script a `blocked` status) records one `notification_show:<objective>:<nextAction text>:request`, and a second wait for the same, unchanged attention records none. In a config test (`tests/engine_store.rs` or wherever `Config::load` is tested): `[notify]\ncommand = ["sh", "-c", "echo \"$NOTIFY_TITLE|$NOTIFY_BODY\" >> /tmp/x"]` parses into `config.notify.command`; and an engine test that sets `config.notify.command` to a script writing a temp file asserts the file holds `title|body` after `engine.notify("title", "body")`.
- [ ] **Step 2: Run:** fails.
- [ ] **Step 3: Implement:** `NotifyConfig` in `config.rs` (`#[serde(default)]`); `Engine::notify` as in Interfaces (`std::process::Command` with `.env`, `wait_timeout` through a thread or the `wait` with a `std::thread::spawn` and `recv_timeout` as `usage/codex.rs` does); call it where a `user` gate is created (after the commit, in the same place the CLI calls `after_commit`), and in `check_wait`/`ask_wait` where the attention difference is detected, once per newly stuck Dispatch with the title from the Run's objective and the body from the projection's `nextAction` argv joined by spaces. Ticks (lane A2) do not notify; only the waiter does.
- [ ] **Step 4: Run** the whole suite and clippy.
- [ ] **Step 5: Commit:** `git commit -am "feat(notify): herdr notifications for decisions and stuck workers"`.

### Task 4: Docs

- [ ] **Step 1:** `docs/reference/config.md`: the `[notify]` table with the two environment variables. `docs/reference/commands.md`: `gate-create --for user` notifies and shows on the board without a Task; `check --wait` notifies on `wokeOn: attention`. Spec 9.1 and 12: the untasked gate card and `DECISION` status are no longer deferred; implementation notes: strike deferral 5 with the date; README's feature bullet about gates: remove "deferred past version 1". `CHANGELOG.md` 0.2.0 line.
- [ ] **Step 2:** `cargo test --release --test docs_sync`, line counts, commit: `git commit -am "docs: decisions away from the terminal"`.
