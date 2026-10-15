# Lane A6: Live suite with real agents

Spec: `docs/superpowers/specs/2026-10-14-hardening-design.md` section A6. Depends on lanes A1 to A5 merged into the branch you work on. Read the Global Constraints in `00-index.md` first.

**Goal:** `scripts/live.sh` runs seven cases against real `claude` and `codex` on an isolated herdr session and records the receipts, so "thinly proven" becomes verified or a known bug.

**Files:**
- Create: `scripts/live.sh`, `tests/live.rs`, `tests/live/mod.rs` (harness), `docs/reviews/live/2026-10-14.md` (results)
- Modify: `docs/ops/troubleshooting.md` or `README.md` (how to run it), `CHANGELOG.md`

**Interfaces:**
- Consumes the existing e2e harness in `tests/e2e/session.rs`: copy its `Session` (server start on `herdr --session <name> server`, `link_plugin`, `pod_command` with `HERDR_SOCKET_PATH`, `create_workspace`, `check_wait`, `wait_until`, `stop_server`) into `tests/live/mod.rs` and remove the fake-agent parts. Do not set `POD_STATE_DIR`: the state directory must derive from the socket path (`paths::session_from_env`), which is what makes a worker started in a new worktree find the same store.
- Real agents need `--yolo` and a model each accepts; take them from `POD_LIVE_CLAUDE_MODEL` (default `claude-sonnet-5-5`) and `POD_LIVE_CODEX_MODEL` (default `gpt-5.6-terra`).
- Every case appends a Markdown section to the results file: the command, the receipt JSON (compact), and pass or fail with the reason.

### Task 1: Harness and case 1 (current folder, prompt path, worker_done, release)

- [ ] **Step 1: Write the test** in `tests/live.rs`:

```rust
mod live;
#[test]
#[ignore = "needs POD_LIVE=1, herdr, claude and codex on this machine"]
fn live_current_folder_claude_and_codex() {
    if std::env::var("POD_LIVE").is_err() { return; }
    let session = live::Session::new("pod-live");
    for kind in ["claude", "codex"] {
        let worker = session.start_worker(kind, "current", &format!("Target: this folder. Change: create a file named live-{kind}.txt containing the word done, then report worker_done with --outcome succeeded. Constraints: no other changes. Ownership: only that file. Acceptance: the file exists."));
        assert_eq!(worker.receipt["state"], "ready", "{}", worker.receipt);
        assert_eq!(worker.receipt["launch"]["inject"], "prompt");
        let done = session.check_wait("worker_done", None);
        assert_eq!(done["messages"][0]["payload"]["outcome"], "succeeded");
        session.pod(&["check", "--ack", done["delivery"]["id"].as_str().unwrap()]);
        let released = session.pod(&["worker-release", "--dispatch", &worker.dispatch]);
        assert_eq!(released["closedTabs"].as_array().map(Vec::len), Some(1));
        session.record("case 1", kind, &worker.receipt, &done, true);
    }
}
```

- [ ] **Step 2: Write `scripts/live.sh`:** `#!/usr/bin/env bash`, `set -euo pipefail`, checks `herdr`, `claude`, `codex` and `pod` on PATH, builds the release binary, then `POD_LIVE=1 cargo test --release --test live -- --ignored --test-threads=1`, and prints the results file path. `chmod +x`. Run `shellcheck scripts/live.sh` if shellcheck is installed.
- [ ] **Step 3: Run** `sh scripts/live.sh` and fix the harness until case 1 passes for both kinds. Watch the panes in herdr (`herdr --session pod-live`) when something stalls; `pod worker-read --dispatch <id>` shows the transcript.
- [ ] **Step 4: Commit:** `git commit -am "test(live): harness and case 1 against real agents"`.

### Task 2: Cases 2 and 3 (worktree with trust, reuse with relabel)

- [ ] **Step 1: Write the tests:** case 2 starts each kind with `--worktree new-child --name live-<kind> --repo path:<a fresh git repo the harness inits under its temp dir>` and the same spec; asserts `state == ready`, that `~/.codex/config.toml` (Codex) or `~/.claude.json` (Claude) now holds the worktree's trust entry (read the file; see `src/engine/trust.rs` for the keys), and that `worker_done` arrives from the worktree. Case 3 reuses the first case's pane for a second Task (`--terminal <handle>`) and asserts a `tab_rename` happened: `herdr tab list` shows the new task title in the label.
- [ ] **Step 2: Run** `sh scripts/live.sh`, fix, record, commit: `git commit -am "test(live): worktree trust and reuse"`.

### Task 3: Cases 4 to 7 (mid-turn nudge, interruption, gate notification, the clock)

- [ ] **Step 1: Write the tests:** case 4 sends `pod send --to dispatch:<id>` while the worker is `working` (start a long task first) and asserts the wake attempt row is `delivered` or `skipped` with a reason, never missing. Case 5 types `herdr pane send-text <pane> "typing here"` into an idle worker's pane, sends it a message, and asserts the attempt is `skipped` with `person is typing` and `worker-show` carries `interruption`; then clears the line with `herdr pane send-keys <pane> ctrl-u` (or the key herdr names for line kill) and asserts the next tick delivers. Case 6 creates `gate-create --for user --question ... --options ... --evidence ... --recommendation ...` and asserts the board file has a `gate:` card and `herdr notification` was shown (herdr has no notification list API; assert pod's receipt `warnings` is empty and the call did not fail). Case 7 sends a message to a worker that is `working`, then runs `pod check --wait --timeout-ms 90000` and asserts a later attempt row appeared without any `pane.agent_status_changed` hook (compare attempt timestamps with the worker's status events in `pod log`).
- [ ] **Step 2: Run, fix, record, commit:** `git commit -am "test(live): nudges, interruption, gates, and the clock"`.

### Task 4: Results and docs

- [ ] **Step 1:** `docs/reviews/live/2026-10-14.md`: the seven cases with their recorded receipts, each pass or fail, and every bug found with the fix commit or an open item. Keep it at or under 400 lines; trim receipts to the fields that matter.
- [ ] **Step 2:** README "What pod doctor checks" or `docs/ops/troubleshooting.md`: one paragraph on running `scripts/live.sh`. `CHANGELOG.md` 0.2.0 line.
- [ ] **Step 3:** stop the live session (`herdr --session pod-live server stop`), remove its worktrees under `~/.herdr/worktrees` that the harness created, restore the two trust files to their state before the run (the harness should back them up first and restore in `Drop`), run the full checks, commit: `git commit -am "docs(live): results of the first live suite"`.
