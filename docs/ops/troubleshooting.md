# Troubleshooting

One entry per row of the failure table in spec section 13, then entries for specific symptoms. The rule behind every entry: act on proof, never on absence. First step for any problem: `pod doctor`.

## Failure table

### Agent exits without reporting

**Symptom:** `worker-list` shows `presence: exited` or `absent`, `liveness: exited`, attention `failure`, `requiresAction: true`, and the projected outcome is `outcome_unknown`.

**Cause:** The agent process is gone from the pane, or the pane exited or closed, and its attempt has no recorded ending. A recorded success, failure, stop, or abandonment takes precedence.

**Fix:** Inspect first: `pod worker-read --dispatch <id>` (the transcript persists after exit). Then decide on proof: `pod worker-stop --dispatch <id>` to fence it and close the exact pane, or `pod worker-abandon --dispatch <id>` to fence it and touch nothing. Then start the Task again: with `pod worker-start --task <id> --agent ...` if it is back to `ready`, or with `--retry-of <dispatch_id>` added if it is `failed` or `blocked` (`--retry-of` is refused for any other status). Finally account for the old worker with `worker-release`.

### Agent stuck at an approval or question prompt

**Symptom:** Attention `approval` or `input`, `requiresAction: true`, liveness `live`, `needsInput.count` above 0.

**Cause:** The agent's own prompt is waiting for a person. pod never answers it.

**Fix:** Look with `pod worker-read --dispatch <id> --source terminal`. Resolve the prompt in the pane yourself or ask the user. If you type into the pane, pod sees the text on its next screen read, leaves the pane alone, and shows attention `interruption` until the input is bare again; typing alone does not make the pane user-owned.

### Worker blocked on a missing fact or ambiguity

**Symptom:** A worker stops making progress and its last message is an `escalation` or a routed `question`, or nothing at all while it waits.

**Cause:** It needs a fact it does not have. The worker should ask the specialist that owns the knowledge (`ask --to dispatch:<id>`) or escalate with evidence, and you route it. This is guide policy, not an automatic action by pod.

**Fix:** Name the blocker first: `pod worker-read --dispatch <id>`. A permission or approval prompt is your own unblock task. A missing fact goes to its owner: reply with the answer or a pointer, or tell the asker to use `pod ask --to dispatch:<specialist>`. You are copied on routed questions; the first `reply` wins and a later one returns `question_answered`. If `ask --to` returns `recipient_not_live`, the target is gone or is the asker: ask the coordinator instead. An `escalation` refused with `escalation_evidence_required` needs `--evidence` and `--recommendation`; the worker resends it. Never report a blocked worker as in progress.

### Coordinator cannot decide

**Symptom:** You need a product decision that is not yours to make.

**Fix:** Create one user gate with the evidence and a default: `pod gate-create --for user --question "<text>" --options '["a","b"]' --evidence "<ref>" --recommendation "a"`. `gate_evidence_required` means evidence or recommendation is missing, or the recommendation is not one of the options. A gate linked to a Task is exported to the board as a decision card. A gate without `--task` has no board card or sidebar status in version 1 (deferred), so find it with `pod gate-list`; `pod check --peek` shows its resolution notification only while unread. Wait with `pod check --wait --types decision_gate`. When the person runs `pod gate-resolve --id <gate_id> --resolution "<text>"`, a `decision_gate` message wakes the wait. Do not create a gate for a step that completed cleanly, and do not ask the user twice.

### Wake ladder exhausted

**Symptom:** `worker-list` shows attention `unreachable`, `requiresAction: true`, and `nextAction.argv` is `pod worker-show --dispatch <id>`.

**Cause:** Five recorded nudge attempts for that mailbox (the first at commit, each later one only on an idle event hook, a mutating command, or a tick of a waiting coordinator or the tab bar, and at least 60 seconds after the previous recorded attempt) ended with no consuming `check` by the recipient. Finding the recipient working, its pane not yet quiet, or a wake in flight records no attempt, so a long turn or a person typing in the pane never makes a mailbox unreachable. The message is still stored and unread. pod never retypes the message body.

**Fix:** Run `pod worker-show --dispatch <id>`, then `pod worker-read --dispatch <id>`, to see whether the agent is stuck, at a prompt, or gone. Act on proof: `presence: exited` or `absent` is proof, `unreachable` alone is not. For the Run mailbox, the status is written to the board: read it with `pod check`. A consuming `check` by the recipient clears the ladder. The ladder is computed from the store, so a herdr restart does not reset it. Read-only inspection does not advance it. Outside a waiting coordinator and the tab bar tick there is no timer; without a later eligible trigger, it stays at its current attempt count.

### Pasted text not confirmed on screen

**Symptom:** A `wake_attempts` row (or the worker's `preamble_input`) has outcome `rendered_unconfirmed`, or text sits in a worker's prompt unsent.

**Cause:** pod pastes text without submitting it, preserving embedded preamble newlines, re-reads the pane, and sends Enter only after it sees the text. When confirmation was unavailable it withheld Enter, so nothing was submitted by mistake. Later nudge attempts skip until the input is proven cleared. pod does not append, submit, or delete uncertain text.

**Fix:** Inspect with `pod worker-read --dispatch <id> --source terminal`. The worker can read mail at its next checkpoint; the next attempt will not append to uncertain input. Do not press Enter in the pane for it: text pod did not type shows as attention `interruption`, and pod sends nothing more until the input is bare. If herdr cannot paste without submitting or confirm the input, automatic input is withheld (spec section 19). A skipped paste records `skipped`; a paste without confirmation records `rendered_unconfirmed`. An unperformed initial injection fails, or returns `start_unknown` if effects are uncertain, with recovery instructions; mail remains available through `check`.

### Agent present but activity unknown

**Symptom:** `liveness: live`, `activity: unknown`.

**Fix:** Keep waiting. Only `presence: exited` or `absent` is proof of exit.

### Worker silent

**Symptom:** Attention `stale`, a warning only.

**Cause:** No heartbeat for 10 minutes. The worker may be in a long task, blocked in `ask`, or hung.

**Fix:** Read `pod worker-read --dispatch <id>`. Send a message if you need an update: `pod send --to dispatch:<id> --subject "status?"`. Never fail the Task on silence alone. pod does not auto-fail a worker.

### herdr unreachable or state unknown

**Symptom:** Liveness `unverifiable` with a `reason`; attention `unverifiable`.

**Cause:** Process presence cannot be established: `presence` is null. Unknown activity alone still permits `live` when presence is proven.

**Fix:** Do nothing to the worker. `unverifiable` never authorizes stop, abandon, retry, or release. Restore herdr (see the restart entry below) and re-run `pod worker-list`.

### `worker-start` fails partway

**Symptom:** Exit 1 with a receipt holding `failedStage`, `setup`, `effects`, `residualResources`, and recovery commands in `nextCommands`. If the start created a pane, the worker is `reclaimable`; for `start_unknown` the Task is `blocked`.

**Fix:** Read `failedStage` (`preflight`, `placement`, `setup`, `pane`, `agent`, `inject`, `record`). Run the recovery commands in the receipt's `nextCommands`, usually `pod worker-release --dispatch <id>` for the leftover pane. Fix the cause (for `setup`, the repo's setup script) and start again, with `--retry-of` if the Task is `failed` or `blocked`. After 3 cumulative failures the Task is `failed` and the Dispatch `circuit_broken`.

### Prompt typed but turn not observed

**Symptom:** `worker-start` or `dispatch --inject` reports `dispatched_unobserved`.

**Cause:** herdr reported the prompt stalled.

**Fix:** Do not resend; the preamble is never sent twice. Check the pane with `pod worker-read --dispatch <id> --source terminal`. If the agent is working, wait. If it never received the brief, stop it on proof and retry. See [dispatched_unobserved](../reference/errors.md).

### Lost mutation reply

**Symptom:** A mutating command timed out or its output was lost, and you do not know whether it applied.

**Fix:** Ask pod. `pod request-show --request <id>` returns one of:

- `completed`: the mutation applied; read the receipt in the output.
- `pending`: it did not finish; replay the same command with `--retry-request <id>`.
- `absent`: pod never saw it; inspect current state (`task-list`, `worker-list`) before retrying.

Pass `--retry-request <id>` on every mutating command whose reply may not arrive, so replay is safe. Receipts are pruned beyond 10,000 rows or 30 days.

### Coordinator crash or context reset

**Symptom:** A new coordinator session has no memory of the Run.

**Fix:** `pod run-current` or `pod run-list`, then `pod run-use --id <run_id>`. This bumps the consumer generation; a Delivery that was outstanding is fenced and its unacknowledged mail replays in a new one. Then `pod worker-list` to see where every worker stands, and `pod check --peek` to see unread mail.

### herdr restart or update

**Symptom:** Panes are gone or renumbered after herdr restarts.

**Cause and behaviour:** The plugin's startup hook runs `pod hook startup`, which reconciles every active Dispatch against the panes that still exist. Missing panes become `exited`, never `failed`. Workers whose panes survive keep working.

**Fix:** Run `pod worker-list`. For each `exited` worker, inspect with `worker-read`, then stop or abandon on proof and retry. If the hook did not run (for example herdr was down), run `pod hook startup` by hand once herdr is reachable.

### Concurrent writers

**Symptom:** A command is slow for a moment under load.

**Behaviour:** Each writer takes one transaction with a busy timeout and bounded retry. No action is needed. If a command still fails with a busy error after the retry, run it again; the data is consistent.

### Store not writable (sandbox)

See [`store_access_denied`](#store_access_denied) below.

### User takes over or closes a worker pane

**Behaviour:** When the user types into a worker pane, pod sees the text through the styled screen read before its next prompt or paste, leaves the pane alone, and shows attention `interruption` (`requiresAction: false`); the category clears and a pending nudge goes through once the input is bare. Typing alone does not make the pane user-owned. Closing the pane, or `worker-retain`, does: `worker-release` leaves a user-owned pane alone, and a closed pane is `exited`.

**Fix:** Nothing is required. Account for the worker as usual: `worker-release` archives what it can and marks it `released` without closing a pane it does not own.

### pod updated during a run

**Behaviour:** Migrations only add tables and columns, so running workers keep working. But the first command of a newer pod migrates the session's store, and from then on every older build refuses it with `store user_version N is newer than supported version M`, including the `pod` that workers already running call for heartbeats (seen 2026-10-14).

**Fix:** Upgrade the installed `pod` (`~/.local/bin/pod`) and the binary every worker calls at the same time. `pod doctor` names the store's schema and its own without migrating anything. Never run a build from a worktree or `target/` (`cargo run`, `./target/release/pod`) inside a herdr pane of the live session: the `HERDR_` variables point it at the live store, which it migrates. Unset them first (`unset $(env | grep '^HERDR_' | cut -d= -f1)`), as `scripts/e2e.sh` and `scripts/live.sh` do, or use `POD_STATE_DIR` for a scratch store.

## Specific symptoms

### `store_access_denied`

**Cause:** The agent's sandbox cannot write pod's state directory (`~/.local/state/pod/<session>/`, or `POD_STATE_DIR`). The error prints the exact fix for the detected agent kind. Find the directory with `pod status`, then grant write access to it.

**Claude Code:** Allow writes to the pod state directory in Claude Code's sandbox settings (the filesystem write allowlist), then restart the session. Alternatively set `POD_STATE_DIR` to a directory inside the project that the sandbox already permits. The exact settings key depends on the installed Claude Code version, so pod does not name it.

**Codex:** Add the state directory to the Codex sandbox's writable roots (for the `workspace-write` sandbox, an additional writable root), or run Codex with a sandbox mode that allows it. Restart the agent after changing it. The exact flag or config key depends on the installed Codex version, so pod does not name it.

**Verify:** run `pod doctor` from inside the agent's sandbox (in the worker's pane). Its `sandbox:<kind>` check probes a write from doctor's own process, so it is `ok` only when that sandbox can write the state directory; on `fail` its `detail` repeats the fix above.

### `waiter_exists`

**Cause:** A `pod check --wait` is already running for this Run. Only one `check --wait` waiter per Run is allowed. `ask` and `ask --resume` wait independently on their question and do not occupy that slot.

**Fix:** Do not start a second. Find the first (another shell or an earlier session of the coordinator) and let it return or end it, then re-run. After a context reset, an old waiter may still be blocked in its pane; wait for its `--timeout-ms` to expire or close that pane.

### `consumer_fenced` on a worker

**Cause:** The worker's Dispatch is no longer its own: it was stopped, abandoned, or replaced.

**Fix:** The worker stops and idles. The coordinator should check `pod dispatch-show --task <id>`.

### herdr-dagr missing

**Symptom:** `pod board` reports `"dagr": "absent"` with an install command, and `pod doctor` warns that herdr-dagr is not found.

**Fix:** Install herdr-dagr with the command `pod board` prints, or ignore it. The board is optional; every fact is available through `worker-list`, `task-list`, `inbox`, and `log`. The board files in `<state dir>/<session>/runs/<run_id>/run.json` are written whether or not dagr is installed. When dagr is present but `pod board` reports `opened: false`, its `warnings` say why; run `DAGR_RUN=<boardFile> dagr` in any pane yourself.

### A nudge never arrives

**Cause:** pod types a nudge only when the recipient's `presence` is `present` and `activity` is `idle_at_prompt` on evidence under 15 seconds old, its pane has been quiet for 3 seconds, the mailbox has no outstanding unacknowledged Delivery, and no nudge is in flight. An absent, stale, or mid-delivery recipient is recorded `skipped` with the reason. A working recipient, a quiet window not yet proven (the mail arrived within 3 seconds of the recipient going idle, or its screen changed since, as when a person is typing), or a wake in flight records nothing, and the next idle hook, tick, or mutating `pod` command retries at once, without waiting. A nudge is attention only and proves nothing about reading.

**Fix:** Do not wait on a nudge. The worker runs `pod check --terminal <handle>` at its checkpoints, and mail is re-offered when herdr reports that agent going idle, up to five attempts. See [Wake ladder exhausted](#wake-ladder-exhausted).

### A `worker_done` was rejected

**Cause:** `worker_identity_changed` or `caller_fenced`. (A `worker_done` refused for a missing closure reason or target gets `closure_reason_required` or `closure_target_required` instead; the rejected report is stored as `status` but does not settle the Dispatch, so correct the flags and resend. A target pod cannot find is not refused: the report is accepted with a `warnings` entry.) The message is still stored, rewritten to a `status` message with priority `high` and a `rejected_reason`, and delivered to you.

**Fix:** Read the rejected message in `pod check`. Inspect the worker with `worker-show` and `worker-read`. If the work is real, accept it by hand-checking and use `task-update` after the worker is stopped (guards apply), or retry the Task.

### Running the live suite

`scripts/live.sh` starts real `claude` and `codex` workers on an isolated herdr session named `pod-live` and checks seven cases: a start in the current folder (brief by `agent.prompt`, `worker_done`, release), a new child worktree with folder trust, a reused pane with a relabelled tab, a nudge to a worker mid-turn, `interruption` after a keystroke into the pane, a user gate on the board with its notification, and the waiting coordinator's clock. It needs herdr 0.9.3 or newer, `claude` and `codex` signed in, and `pod` on `PATH`, spends model tokens (about 20 minutes for the full run), and is not part of `cargo test`: the tests in `tests/live.rs` are ignored unless `POD_LIVE=1`.

The run is isolated except for `HOME`, which the agents need to sign in. herdr gets its own config and state directories under `/tmp/pod-live-*`, so your sessions, plugins, and pod store are untouched; pod's store derives from that session's socket path, as it does for any session. A `--yolo` start adds trust entries to `~/.claude.json` and `~/.codex/config.toml`; the harness removes the entries it caused, and the worktree folders it created, when it ends. Codex workers launch with pod's built-in `--no-daemon` (see [config](../reference/config.md)), because a Codex desktop app running its shared background server with other feature settings makes Codex stop at a "Background server has incompatible feature settings" dialog.

Each case appends the command, a trimmed receipt, and pass or the failure reason to `docs/reviews/live/<date>.md`. Both agent kinds run even when one fails; the test fails at the end. Settings: `POD_LIVE_CLAUDE_MODEL` (default `claude-sonnet-5-5`), `POD_LIVE_CODEX_MODEL` (default `gpt-5.6-terra`), `POD_LIVE_KINDS` (`claude` or `codex`, to rerun one), `POD_LIVE_RESULTS` (another results path), and test names after the script, for example `scripts/live.sh live_worktree`. To watch a stuck start, attach with the harness's directories: `XDG_CONFIG_HOME=/tmp/pod-live-<id>/xdg-config herdr session attach pod-live`, and read a worker with `pod worker-read --dispatch <id>`.
