# Changelog

All notable changes to this project are documented in this file. The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.6] - 2026-10-15

### Added

- `pod log` records `worker.released` for every `worker-release` or `worker-gc` that changed a worker's state, with `{terminal_state, closed_tabs, closed_panes, left_alone}`.

### Changed

- A start that fails at `preflight` or `placement`, before any pane exists, no longer counts toward the circuit breaker: nothing ran, so the failure is pod's or herdr's. The Dispatch is `failed`, the Task returns to `ready`, `failure_count` is unchanged, and the `dispatch.start_failed` event carries `counted: false`. Two placement failures in a row had spent two of a Task's three attempts on 2026-10-15.
- The `task_not_startable` refusal names the one cause that applies instead of printing `has unmet dependencies:` with an empty list for a bad `--retry-of`.
- A `worker_done` with the same outcome as the report that settled its Dispatch is `duplicate` only when its subject and body match too, and the receipt then carries a `warnings` entry naming the re-sent message. One that reports different work is refused with `inactive_dispatch`, stored for the coordinator as a refused status, and told why: the worker was briefed outside pod. A verdict typed into a hand-briefed retained pane had vanished as `duplicate` on 2026-10-15.

## [0.2.5] - 2026-10-15

### Added

- `pod worker-gc [--run <id>]` releases every `reclaimable` worker of the Run in one call (and retries `release_pending` and `release_unknown`), under the same rules as `worker-release`: the final screen is archived and the tab closes only if pod created it. Retained and active workers are kept and listed with their reason. Eight finished panes piling up in one afternoon, each closed by hand, asked for it.

### Changed

- `pod dispatch --task <id> --to <handle>` types the preamble into the pane by default; `--no-inject` records the Dispatch and types nothing, and `--inject` is accepted for Orca's contract. Four dispatches to retained workers in one afternoon returned `dispatched` while nothing reached the terminal, because the flag was missing. The pane's cleanup now moves to the new Dispatch as it does on reuse: the worker that last held the pane is recorded `released`, and pod's ownership of the pane, tab, and worktree carries over, so releasing the new Dispatch closes the tab pod created instead of leaving it alone as `pane_not_created_by_pod`.

### Fixed

- `worker-start --worktree path:<folder>` failed in 0.2.4 with herdr's `New and open worktree actions start from the repo parent workspace` for a linked worktree of another repository (two starts, 2026-10-15), and a new worktree created by a coordinator sitting in a linked worktree would have failed the same way: every open and create now starts from the repository's source checkout, as herdr's own listing names it, instead of the worktree's folder.

## [0.2.4] - 2026-10-15

### Fixed

- `worker-start --worktree path:<folder>` still failed at `placement` in 0.2.3 for a worktree of another repository, now with `worktree path not found`, or with `fatal: not a git repository` while the focused folder was no repository: the listing named the target's repository, but the `worktree.open` that followed did not, so herdr searched its focused workspace's repository for the path (seen live 2026-10-15, two starts in a row). The open now names the target's own folder too.

## [0.2.3] - 2026-10-15

### Changed

- `pod doctor`'s `usage` check warns when herdr's tab bar runs `pod usage` without `--tick`: an entry from before 0.2.0 shows usage but never runs the ladder pass that retries a nudge when nobody waits. `pod usage --install` rewrites it in place.

### Fixed

- `worker-start --worktree path:<folder>` could fail at `placement` with git's `fatal: not a git repository` while the target was a valid worktree: pod asked herdr for its worktree list without a folder, so herdr answered for its focused workspace, whatever the person was looking at (seen live 2026-10-15, four starts in a row while a folder that was no repository had focus). Placement now lists the target's own repository, places a target that is no repository as a plain folder, and names the step and folder in any other failure.
- A worker's `nohup` heartbeat loop was refused with `worker_identity_changed` once its shell exited and the system reparented it, since the agent was no longer an ancestor of the `pod` process. A process on the agent's controlling terminal now passes the identity check with a warning.
- A refused lifecycle message, stored for the coordinator as a high-priority `status`, ran no commit hook, so neither the board nor a nudge followed it and the coordinator never learned of it. It now runs the commit hook like any other mail, and the error's `data.stored` names the stored message and mailbox.
- Mail that landed while the coordinator was busy, or while a person was typing in its pane, could sit unread for good: the commit's attempt was recorded `skipped`, so the coordinator's next idle hook, inside the ladder's minute, was refused, and with no tick or later command nothing retried it (a `worker_done` seen live 2026-10-15). A trigger that finds the recipient working, its pane not yet proven quiet, or a wake in flight now records nothing, whatever the trigger: the ladder keeps its position and the next eligible trigger nudges at once. Only typed nudges, failed inputs, and skips for a recipient pod could not reach count toward `unreachable`, and every mutating command retries every mailbox of the Run with unread mail not already in an outstanding Delivery.

## [0.2.2] - 2026-10-15

### Changed

- `scripts/install.sh` installs the latest release when no release matches the installed commit, so `herdr plugin install` keeps downloading a prebuilt binary after commits that follow a tag; building from source is now only for a repository with no release or a failed download. `POD_RELEASE_TAG` pins a tag.

## [0.2.1] - 2026-10-14

### Fixed

- The two skill stubs under `skills/` had an unquoted colon in their `description`, which strict YAML reads as a nested mapping, so `npx skills add mrrobot-ec/pod` skipped both and installed nothing. The descriptions are quoted.

## [0.2.0] - 2026-10-14

One line per lane of the hardening plan (`docs/superpowers/plans/2026-10-14-hardening/`), in lane order.

### Added

- A1, prompt injection: every worker's brief is a file and its one-line pointer, like every nudge, goes through herdr `agent.prompt`; hand paste is the fallback, and `launch.inject` on the start receipt says which path ran.
- A2, a clock without a daemon: `check --wait` and `ask` run one wake ladder pass per minute while they block, and `pod usage --tick` (what `pod usage --install` now writes to the tab bar) runs one every 30 seconds when nobody waits. A worker that goes `stale` now ends `check --wait` with `wokeOn: attention`.
- A3, briefs survive compaction: every nudge to a worker names its handle and its brief file (`Your brief: <path>`), `worker-show` and `worker-list` print `briefPath`, and the preamble says where a copy of the brief is stored.
- A4, attention: `interruption` when a person has typed into a worker pane (the screen read before every prompt or paste leaves the pane alone, records `user_typing`, and the category clears when the input is bare again) and `guidance` (`requiresAction: true`) when a worker's `question` or `escalation` has gone five minutes unanswered, with `reply --id` or `worker-read` as `nextAction`.
- A5, decisions away from the terminal: an untasked user gate is a blocked `question` card (`gate:<id>`) on the board, every worker workspace's sidebar status leads with `DECISION · <question>` while a user gate is open, and herdr notifications (`notification.show`, sound `request`) fire for every new user gate and every worker a `check --wait` newly reports as requiring action, plus an optional `[notify] command` run with `NOTIFY_TITLE` and `NOTIFY_BODY`.
- A6, a live suite: `scripts/live.sh` runs seven cases against real `claude` and `codex` workers on an isolated herdr session (current-folder start, worktree trust, pane reuse, mid-turn nudge, interruption, user gate, the clock) and records receipts in `docs/reviews/live/<date>.md`. See `docs/ops/troubleshooting.md`.
- B1, limits, cost, and budget: `[limits]` in the config (`max_active_workers`, `max_active_per_kind`, `pause_above_percent`) makes `worker-start` and `dispatch` refuse with the new `limit_reached` code, and stale or missing usage never blocks; `pod usage --record` stores every status-line payload so `worker-show`, `worker-list`, and the board show `costUsd` for a Claude worker (`null` when no agent reported one, never `0`); `run-create --budget-usd <n>` gives a Run a soft budget (`budgetUsd`, `spentUsd`, a warning above 80 percent, and the new `budget_exceeded` code once spent), and the store migrates to version 3.
- B2, agent table: launch flags are a table per agent kind (`[agents.<kind>]` takes `launch = { always, model, effort, fast, yolo }` argv fragments with `{model}` and `{effort}` placeholders, merged key by key over the built-in), so any kind can get `--model`, `--effort`, `--fast`, and `--yolo` without a code change; only `codex` and `claude` have built-in tables (Claude's output is unchanged, Codex adds `--no-daemon`), `config.md` gives `[agents.grok]` as the worked example, `launch_mapping_unsupported` shows a pasteable snippet and names the preset that asked, `pod doctor` has a `launch-mapping` check, and an `[agents.<kind>]` entry no longer has to name a herdr agent kind or carry a `command`.

### Changed

- B3, spec hygiene and positioning: the `BASE DRIFT` preamble section is dropped from the spec and the renderer (counting commits needs git, which pod does not run), spec section 14 matches Orca v1.4.219 to v1.4.224, and the README says why pod exists, when to use native subagents instead, and how pod compares.

### Fixed

- A tick (`check --wait`, `ask`, the tab bar) no longer records a wake attempt against a recipient that is working or a mailbox whose wake another process holds, so a long turn cannot exhaust a healthy worker's ladder. Nudges never sleep in pod (the launch retry and readiness wait stay at launch), the tab bar's nudge asks herdr for no turn wait, and an unreachable herdr fails an input instead of pasting it a second time.
- `interruption` clears only when a read finds the input bare, not on any later herdr state; a `send` to the Dispatch answers a worker's question for `guidance`; a failing `[notify] command` is a `warnings` entry on the receipt; a worker that only went `stale` is not announced.
- Config refuses what it would silently drop: an unknown key in `[agents.<kind>]` or its `launch` table, a `model` or `effort` fragment without its placeholder, and an entry with no `command` whose kind name is a shell wrapper (`[agents.sh]`).
- A board task's cost counts each Claude session once when two attempts share a pane.

## [0.1.0] - 2026-10-13

Version 1 scope, as defined in `docs/specs/2026-10-05-pod-design.md`.

### Decisions 2026-10-13

11. `--fast` on `worker-start` and `fast = true` in a preset launch Codex in fast mode (`-c service_tier="fast"`); the flag is refused for other kinds with `launch_mapping_unsupported`. `claude` effort maps to `--effort`, confirmed on the installed Claude Code (4, 19).
12. `--yolo` on `worker-start` and `yolo = true` in a preset launch without approval prompts (`codex`: `--dangerously-bypass-approvals-and-sandbox`; `claude`: `--dangerously-skip-permissions`), and first record trust for the worker's folder in the agent's own config (Codex `config.toml`, Claude `~/.claude.json`), so a new worktree does not stop at the folder-trust prompt (4, 19).
13. Safe input confirms a paste by whitespace-free comparison from the input marker down, so a composer that scrolls and soft-wraps (Codex 0.160.1) is confirmed; the ten-researcher pilot had withheld Enter on every start. The preamble waits up to 20 s for an idle agent (8.2, 19).
14. `check --wait` also returns when a worker newly requires action (`wokeOn: attention`, with the workers listed), and herdr's bare `blocked` status counts as `needs_input` (7.6, 13, 14).
15. The board export follows `CONTRACT.md` of herdr-dagr 0.3.1 and passes `dagr check --strict`; `pod board` runs `dagr view <file>` (12).
16. Board: every attempt after the first names its predecessor even without `--retry-of`; the model column is a `family·version·effort` chip; a task's `note` is its worker's latest message subject, and the last message counts toward the attempt's `last_output_at` (12).
17. `pod log --follow [--timeout-ms]` streams events as they are appended, one line each (3).
18. The coordinator guide asks for `--deps` and `--parent` so the board shows the graph, and for `--yolo` on every unattended worker.

### Decisions 2026-10-12

1. A failed `worker_done` counts toward the circuit breaker like any failed Dispatch, except `--reason denied` or `canceled`, which fail the Task directly (7.5).
2. `gate-resolve` returns the Task to `ready` only when every dependency is completed, otherwise to `pending`; gates never bypass the dependency rule (9).
3. The final screen text is archived at release for every agent kind (11).
4. The `sol` reviewer runs at `xhigh` effort (1).
5. `--reason-target` is required for `handed_off`, `blocked_on`, and `superseded` (`closure_target_required`), but a target pod cannot find is accepted with a `warnings` entry; the unknown-target error code no longer exists (8.1).
6. The wake ladder is one bounded policy: at most 5 nudge attempts, the first at commit, later ones only on an eligible trigger at least 60 seconds apart, then attention `unreachable` (8.2).
7. `wake_attempts` records nudges only; the preamble injection is recorded once in the new `workers.preamble_input` column with the same outcome values (8.2, 5).
8. No screen sampling and no ranked evidence: `evidence_source` is `self_report | herdr_state`, worker reports prove activity never presence, and herdr state is authoritative for presence (7.6, 5).
9. Specialist routing is unchanged (9.1).
10. The sidebar `DECISION` status and the board card for a user gate without a Task are deferred past version 1; such a gate is visible through `gate-list` (`check --peek` shows its resolution notification while unread), and a gate linked to a Task is exported as before (9.1, 12).
19. Worker handles are mnemonic: `--handle` when given, else `<agent>-<worktree name>` or `<agent>`, numbered from `-2` only while the name is live; the random suffix is gone. Worker tabs are labelled `<handle> · <task title>` and relabelled on reuse (7.1).
20. `--worktree path:<folder>` on a folder herdr does not track as a worktree joins the workspace whose panes sit there, else creates a workspace named after the folder, so every project is its own space in the sidebar; a new worktree's space is labelled `<repo folder>/<name>` unless `--display-name` says otherwise (7.2).
21. `pod usage` shows both accounts' rate-limit windows, Claude's session cost, and Codex's credits in one line for herdr's tab bar; `--record` is Claude Code's status-line command, Codex is read through `codex app-server`, `--toggle` collapses the entry to `«` (bound to `prefix+u`, the bar polls every second), and `--install` wires both files. `pod doctor` gains the `usage` check (12.3).
22. Preamble injection waits for the worker's prompt to be idle and unchanged for three seconds (`POD_INPUT_SETTLE_MS`), because a paste typed while the agent answers herdr's stray startup prompt loses its head (seen 2026-10-14 with five Claude workers), a Claude composer showing only a paste's tail no longer counts as confirmed, and a `claude` worker always gets its brief as a file plus a one-line pointer, since Claude Code chunks long pastes and drops the early chunks while busy (7.3).

### Added

- `pod doctor` checks herdr, the store, `pod` on `PATH`, a herdr agent integration and a sandbox write per configured agent kind, and herdr-dagr.
- Criterion benchmarks asserting the performance targets for non-waiting commands.
- Single Rust binary `pod`, shipped as a herdr plugin (herdr 0.9.3 or newer; macOS and Linux), with no daemon.
- SQLite store (WAL mode, owner-only files) with additive migrations.
- Orca-compatible orchestration contract: Runs, Tasks with dependencies, Dispatches, durable mailboxes, FIFO Deliveries with acknowledgement and replay, questions, and decision gates.
- Environment commands: `doctor`, `status`, `skills get`, `log`, `board`, `hook`.
- Run commands: `run-create`, `run-use`, `run-current`, `run-list`, `run-show`.
- Task commands: `task-create`, `task-list`, `task-update`.
- Worker commands: `worker-start`, `worker-show`, `worker-read`, `worker-list`, `worker-stop`, `worker-abandon`, `worker-retain`, `worker-release`.
- Low-level commands: `dispatch`, `dispatch-show`.
- Message commands: `send`, `check` (including `--wait`), `ask`, `reply`, `inbox`.
- Gate commands: `gate-create`, `gate-resolve`, `gate-list`.
- Recovery commands: `request-show`, `reset`, and `--retry-request` on every mutating command.
- Worker placement: current folder, new child worktree, new top-level worktree, existing worktree; repo setup argv with `wait-for-setup` and `start-immediately`.
- Named presets (agent kind, model, effort, extra args) and command aliases in `config.toml`; `terra` and `sol` example presets.
- Circuit breaker (3 cumulative failures, including a failed `worker_done` unless its reason is `denied` or `canceled`), retry with `--retry-of`, and completion accounting (reuse, retain, release).
- Liveness and attention projection with literal `nextAction` argv per worker.
- Three-axis worker observation (`activity`, `presence`, `needs_input`) with `evidence_source` of `self_report` or `herdr_state`; `liveness` is derived from the axes.
- Closure reasons on `worker_done` (`--reason`, `--reason-target`), required for a failed outcome; a target pod cannot find is accepted with a `warnings` entry.
- Safe pane input (paste, confirm rendered, then Enter) for nudges and the preamble, with each nudge in `wake_attempts` and the preamble injection once in `workers.preamble_input`.
- Wake ladder: one bounded policy of at most 5 store-derived nudge attempts per mailbox, the first at commit and later ones only on an idle hook or mutating command at least 60 seconds after the previous attempt; then attention `unreachable`. No timer or daemon.
- Escalation routing: `ask --to dispatch:<id>` to a specialist worker, required `--evidence` and `--recommendation` on escalations, and user gates (`gate-create --for user`) that record a decision with evidence and a default, with a board card only when linked to a Task.
- Caller identity from terminal ID plus agent-process ancestry.
- Nudges and herdr event hooks (`pane.exited`, `pane.closed`, `pane.agent_status_changed`) and startup reconciliation.
- Transcript readers for Claude Code and Codex, plus best-effort Grok and Cursor Agent readers, with refusal when the session is unproven.
- Board export to herdr-dagr (`run.json`, contract v3). `pod board` opens dagr in a new tab with `DAGR_RUN` set when `dagr` is on `PATH`, and every commit writes each worker workspace's herdr sidebar status (`<task> · <phase>`); a herdr refusal is a warning (spec section 12).
- Documentation: README, coordinator and worker guides, command, error, state-machine and config references, troubleshooting, and a worked example.
- MIT license and a NOTICE crediting Orca (MIT) for the orchestration contract and preamble wording, and OpenRig (Apache-2.0) for the closure-reason, safe-input, wake-ladder, activity-axis, and escalation-routing ideas.

### Changed in the final review (2026-10-13)

- `worker-stop` closes only a pane pod created; a `dispatch --to` pane stays open and is listed in `leftAlone` as `pane_not_created_by_pod` (7.4).
- `worker-release` closes only the worker's pane, not its tab, when a person has split another pane into that tab (7.4).
- The 3-second quiet check of a nudge no longer blocks the sending command: it is proven from a screen snapshot recorded when herdr reports the recipient idle, and only herdr's hook process may still wait it out. A command that cannot prove it records the attempt `skipped` with the reason (8.2, 16).
- `worker-start` waits, within `--timeout-ms` and at most 10 s, for a new tab's shell to reach its prompt before `agent_start`, and retries herdr's "not an available shell" refusal; a shell that never gets there fails at stage `pane` (7.1).
- The Dispatch's `agentKind` is the kind pod launched, so an agent run under a command alias no longer records `unknown`.
- Worker receipts (`worker-start`, stop, abandon, retain, release) spell their `dispatch` and `worker` rows in camelCase like `worker-show` (`terminalState`, never `terminal_state`).
- `worker-list` reports Tasks waiting on a failed dependency (`waitingOnFailedDependency`, with a next action) (9).
- The fifth unconsumed nudge on a Run mailbox writes a `mailbox.unreachable` event, which the board exports for the user (8.2).
- An existing state root (`~/.local/state/pod` or `POD_STATE_DIR`) is tightened to `0700` (15).
- Timing tests and the bench assert spec 16 targets in release builds only, so `cargo test` is deterministic on slow machines.

### Not in version 1

- Remote or SSH placement (`--on`, refused with `remote_unsupported`).
- Scheduled jobs.
- Orca's browser, computer-use, artifacts, and account features.
- Migration from Orca's legacy contract.
- Windows.
- A built-in UI (herdr-dagr is the board).
- A cap on concurrent workers.
- The sidebar `DECISION` status and a board card for a user gate without a Task. Inspect such a gate with `gate-list`; `check --peek` shows its resolution notification while unread.
