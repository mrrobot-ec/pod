# Command reference

Commands follow spec section 6. This page covers conventions, Environment, Runs, Tasks, Gates, and Recovery. The rest are split out:

- [Workers and low-level dispatch](commands-workers.md): `worker-start`, `worker-show`, `worker-read`, `worker-list`, `worker-stop`, `worker-abandon`, `worker-retain`, `worker-release`, `worker-gc`, `dispatch`, `dispatch-show`
- [Messages](commands-messages.md): `send`, `check`, `ask`, `reply`, `inbox`

The spec fixes command names, flags, and the field names it mentions, but not full receipt layouts. Receipts use spec-named fields exactly and take the rest of their shape from the data model in spec section 5. The JSON examples on these pages match pod's output. The contract tests (spec section 17) compare the receipts with Orca's, and each difference from Orca is listed, with its reason, in `tests/contract/allowed-differences.toml`.

## Conventions

- Every command accepts `--json`. Without it, a success prints the same JSON pretty-printed, and an error prints `error: <message>` and one `next: <step>` line per next step on stderr.
- `pod --version` prints the version; `--help` works on every command.
- Every mutating command accepts `--retry-request <id>`. Replaying the same ID returns the stored receipt instead of repeating the mutation.
- Every mutating receipt ends with `"mutation": {"requestId": <the --retry-request ID or null>, "replayed": <bool>}`, as Orca's do; `replayed` is true when the stored receipt was returned. The examples below omit it.
- List receipts carry `count`, and Run-scoped lists (`task-list`, `gate-list`) also carry `runId`.
- Errors go to stderr as `{"error": {"code", "message", "data": {"nextSteps"}}}`.
- Exit codes: `0` success, `1` refused or failed operation, `2` usage error (unknown flag, missing required flag, mutually exclusive flags). The error codes in [errors.md](errors.md) all exit 1 unless that page says otherwise.
- Any command that touches the store can return `store_access_denied` (exit 1) when the store is not writable, for example under an agent sandbox.
- Outside herdr (`HERDR_ENV` unset), read-only commands work with an explicit `--run` (without it they are a usage error, exit 2); mutating commands that need a caller identity return `not_in_herdr`.
- `--run <id>` selects a Run explicitly. Without it, the Run bound to the calling terminal is used.

## Error code index

Every code in [errors.md](errors.md), with the commands on these pages that can return it.

| Code | Returned by |
|---|---|
| `task_not_found` | `worker-start`, `dispatch`, `dispatch-show`, `task-update`, `gate-create` |
| `task_not_startable` | `worker-start` |
| `nested_worker_depth_exceeded` | `worker-start` |
| `worker_identity_changed` | `send` (lifecycle types) |
| `caller_fenced` | `send` (lifecycle types) |
| `not_in_herdr` | every mutating command that needs a caller identity |
| `remote_unsupported` | `worker-start --on` |
| `consumer_fenced` | `check --terminal` |
| `waiter_exists` | `check --wait` |
| `store_access_denied` | every command that opens the store |
| `source_changed` | `worker-read --cursor` |
| `inactive_dispatch` | `send --type worker_done` (conflicting repeat) |
| `closure_reason_required` | `send --type worker_done --outcome failed` |
| `closure_target_required` | `send --type worker_done` with `--reason handed_off`, `blocked_on`, or `superseded` |
| `question_answered` | `reply` (a later reply to a routed or answered question) |
| `recipient_not_live` | `ask --to dispatch:<id>` |
| `escalation_evidence_required` | `send --type escalation` |
| `gate_evidence_required` | `gate-create --for user` |
| `herdr_unreachable` | every command that talks to herdr (`worker-*`, `dispatch`, `send` and `check` nudges, `hook`) |
| `herdr_error` | every command that talks to herdr |
| `terminal_busy` | `worker-start --terminal`, `dispatch` |
| `unknown_agent` | `worker-start --agent` |
| `launch_mapping_unsupported` | `worker-start --model`/`--effort`/`--fast`/`--yolo` |
| `caller_mismatch` | `send`, `ask` (with `--from`) |
| `invalid_recipient` | `send --to` |
| `question_closed` | `reply` |
| `task_guard_refused` | `task-update`, `gate-create` |
| `reset_requires_force` | `reset` |
| `limit_reached` | `worker-start`, `dispatch` (a `[limits]` cap or usage pause) |
| `budget_exceeded` | `worker-start`, `dispatch` (a Run with `--budget-usd`) |

A `--reason-target` that pod cannot find in the Run is not an error: `send --type worker_done` is accepted with a `warnings` entry in the receipt.

Two values on [errors.md](errors.md) are reported in a receipt, not in the error envelope:

| Value | Returned by |
|---|---|
| `settled` (with `duplicate: true`) | `send --type worker_done` (repeat with the same outcome), exit 0 |
| `dispatched_unobserved` | `worker-start`, `dispatch`, exit 1 |

## Environment

### `pod doctor`

Checks the environment and reports each result. Flags: none beyond `--json` (pretty-printed without it, compact with it). Changes no Run state; it creates an empty store when none exists and writes and removes a probe file in the state directory.

The checks, by `name`: `herdr` (reachable and 0.9.3 or newer), `store` (writable, passes an integrity check, at this pod's schema version), `path` (`pod` on `PATH`), `usage` (Claude Code's status line runs `pod usage --record` and herdr's tab bar runs `pod usage --tick`; a warning until `pod usage --install` has run, and a warning naming `--tick` when the bar entry predates the flag and so runs no wake ladder pass), `agent-integration:<kind>` and `sandbox:<kind>` (one each per configured agent kind, `claude` and `codex` when the config names none; the integration check reads `herdr integration status`, the sandbox check probes a write to the state directory), `herdr-dagr` (optional; a warning, not a failure), and `launch-mapping` (the kinds whose launch table maps a flag; informational). A config that cannot be loaded adds a failed `config` check. Each entry has `status` (`ok`, `warn`, `fail`) and `detail`; a check that is not `ok` carries the fix in its `detail`, for example ``not installed; run `herdr integration install claude` ``. pod exits 1 when any check is `fail`; warnings alone exit 0.

Receipt (from a machine without herdr-dagr):

```json
{"ok": true, "checks": [
  {"name": "herdr", "status": "ok", "detail": "0.9.3 (protocol 22)"},
  {"name": "store", "status": "ok", "detail": "/Users/me/.local/state/pod/default/pod.db at schema 3, this pod's too; writable; integrity ok"},
  {"name": "path", "status": "ok", "detail": "/Users/me/.local/bin/pod"},
  {"name": "agent-integration:claude", "status": "ok", "detail": "current (v10) (/Users/me/.claude/hooks/herdr-agent-state.sh)"},
  {"name": "agent-integration:codex", "status": "warn", "detail": "not installed; run `herdr integration install codex`"},
  {"name": "sandbox:claude", "status": "ok", "detail": "/Users/me/.local/state/pod/default writable (probed from doctor's process)"},
  {"name": "sandbox:codex", "status": "ok", "detail": "/Users/me/.local/state/pod/default writable (probed from doctor's process)"},
  {"name": "herdr-dagr", "status": "warn", "detail": "not found (optional; `pod board` needs it); install with `herdr plugin install aemrebarut/herdr-dagr`"}
]}
```

The `store` check names the store's schema version and this binary's, and never migrates an existing store: a store at an older schema is a `warn` (this pod's next command migrates it, and every older build then refuses it), and one at a newer schema is a `fail` (a newer build migrated it). A failing check is reported in the body, never raised: an unwritable store is a `fail` on `store`, not `store_access_denied`. `ok` is the envelope's and stays `true`; the exit code carries the verdict. Exit 0 when no check is `fail` (warnings are allowed); exit 1 when any check is `fail`.

### `pod status`

Reports pod's own state: version, resolved paths, store health, herdr reachability, and the bound Run (spec section 6). The `session` field is added here because paths are per session. Read-only.

```json
{"version": "0.1.0", "session": "default", "stateDir": "/Users/me/.local/state/pod/default",
 "store": {"path": "/Users/me/.local/state/pod/default/pod.db", "userVersion": 1, "ok": true},
 "herdr": {"reachable": true, "version": "0.9.3"},
 "run": {"id": "run_7f3a2c", "source": "bound"}}
```

Errors: `store_access_denied`. Exit 0 (a degraded component is reported in the body, not the exit code).

### `pod skills get <coordinator|worker> [--reference <name>] [--full]`

Prints a guide: `coordinator` serves [coordinator.md](../guides/coordinator.md), `worker` serves [worker.md](../guides/worker.md). `--reference <name>` prints one named reference page instead; `--full` prints the guide plus all its references. Reference names are the file names under `docs/reference` without extension. Read-only, needs no store.

Errors: usage error (exit 2) for any name other than `coordinator` or `worker`. Exit 0.

### `pod log --run <id> [--follow [--timeout-ms <n>]]`

Prints the Run's `events` rows in `sequence` order, each with the columns of the `events` table: `{sequence, at, type, task_id, dispatch_id, actor, detail}`. Read-only. `--run` is required. With `--follow` it is a feed instead of a receipt: the events so far, one per line, then each new event as it is appended (within a second of its commit), until `--timeout-ms` (default ten minutes) elapses; exit 0 on the timeout. With `--json` each line is one event object, otherwise `at type actor task dispatch detail`. pod writes these types, one module per action: `run.created`, `run.used`, `task.created`, `task.ready`, `task.updated`, `gate.created`, `gate.resolved`, `dispatch.completed` and `dispatch.failed` (from settling a `worker_done`; `dispatch.failed` also from a `worker-stop` or `worker-abandon` fence, with `termination_reason: canceled`, same detail `{termination_reason, termination_target, dispatch_status, task_status}`), `mailbox.consumed` (every consuming `check`; detail `{mailbox, delivery_id, consumer}`; it clears the mailbox's wake ladder), and `mailbox.unreachable` (the fifth unconsumed nudge attempt on the Run mailbox; detail `{mailbox, attempts, nextAction}`; the board exports it so the person sees that the coordinator is not reading its mail). The worker lifecycle adds `dispatch.dispatched` (a start or `dispatch` reached Record), `dispatch.start_failed` (a start or `dispatch` failed; detail `{stage, failure_count, counted, dispatch_status, error}`, where `counted` is false for a `preflight` or `placement` failure), `worker.user_owned` (retain, or a pane closed outside pod; detail `{reason}`), and `worker.released` (a `worker-release` or `worker-gc` that changed the worker's state; detail `{terminal_state, closed_tabs, closed_panes, left_alone}`; a repeated release writes none). Each action writes its events once, from one module.

```json
{"events": [
  {"sequence": 41, "at": "2026-10-05T14:02:11Z", "type": "task.created", "task_id": "task_a1b2c3", "dispatch_id": null, "actor": "term_0f69", "detail": {}},
  {"sequence": 42, "at": "2026-10-05T14:02:19Z", "type": "dispatch.dispatched", "task_id": "task_a1b2c3", "dispatch_id": "ctx_9d41e7", "actor": "term_0f69", "detail": {}},
  {"sequence": 57, "at": "2026-10-05T14:11:02Z", "type": "dispatch.completed", "task_id": "task_a1b2c3", "dispatch_id": "ctx_9d41e7", "actor": "terra-k3x9", "detail": {"termination_reason": "no_follow_on", "termination_target": null}}
]}
```

Each Dispatch ending shows the `termination_reason` and `termination_target` that were stored on it (spec section 8.1), in `detail` beside `dispatch_status` and `task_status`.

Errors: `store_access_denied`; a missing Run is a usage error (exit 2); there is no dedicated run-not-found code. Exit 0.

### `pod board [--run <id>]`

Rewrites the Run's `run.json` and reports its path. When a `dagr` executable is on `PATH`, it opens herdr-dagr: a new, focused tab named `board` in the caller's workspace (the focused pane's, for the plugin's `open-board` action) whose shell has `DAGR_RUN` set to that file, with the `dagr` path typed into it through safe input (paste, confirm, Enter); herdr's shell runs dagr, pod does not. `opened` is `true` with the board `pane` when that worked; otherwise `opened` is `false` and `warnings` says why (herdr unreachable, the typed command not confirmed). Without dagr, `install` gives the install command. The board is optional. Flag: `--run` (default: the bound Run, or for a caller that is not a pane the Run bound to herdr's focused pane; outside herdr `--run` is required). Every committing mutation already rewrites `run.json` (`Engine::after_commit`), and `reset` removes a deleted Run's board directory; `pod board` rewrites it once more and reports.

```json
{"run": "run_7f3a2c", "boardFile": "/Users/me/.local/state/pod/default/runs/run_7f3a2c/run.json", "opened": false, "dagr": "absent", "install": "herdr plugin install aemrebarut/herdr-dagr"}
{"run": "run_7f3a2c", "boardFile": "/Users/me/.local/state/pod/default/runs/run_7f3a2c/run.json", "opened": true, "pane": "p_31", "dagr": "present"}
```

Every commit also writes each worker workspace's herdr sidebar status (`<task> · <phase>`, cleared when its workers end); a herdr that refuses it gets one stderr warning and the mutation still succeeds. Errors: `store_access_denied`; a missing or unknown Run is a usage error (exit 2). Exit 0 whether or not dagr is present.

### `pod usage [--json] [--max-age-ms <n>]`, `pod usage --tick`, `pod usage --toggle`, `pod usage --record`, `pod usage --install [--no-herdr]`

Account usage of both agents in one line, meant for herdr's tab bar: `claude 5h 12% · 7d 30% · $3.10 | codex 5h 61% · wk 1% · 49.6k credits »`. `--toggle` collapses the entry to `«` and expands it again (a marker file in `<state dir>/usage/`); the bar follows on its next one-second poll. Rate-limit windows are per account, so one line covers every pane. Data older than a minute shows its age (`(12m ago)`); a missing account says `no data`. `--json` adds both records (`claude`, `codex`) and the `line`.

- Codex is read live from `codex app-server` (`account/rateLimits/read`), cached in `<state dir>/usage/codex.json` and reused while younger than `--max-age-ms` (default 60000). `POD_CODEX_BIN` overrides the executable. A failed read keeps the cached record.
- Claude cannot be polled, only received: `pod usage --record` is Claude Code's status-line command. It reads the payload from stdin, keeps the newest one that carries `rate_limits` in `<state dir>/usage/claude.json`, and prints nothing, so the pane itself stays clean. A payload without rate limits is skipped.
- `--tick` is what `--install` puts in the tab bar. It prints the same line and, at most every 30 seconds (a `tick.at` stamp in the state directory), runs one wake ladder pass for every Run with an active Dispatch, so a nudge due for a mailbox goes out when nobody is waiting. The pass never waits for the quiet window, asks herdr for no turn wait on a nudge, never records an attempt against a working recipient or an unproven quiet window, and never runs `reconcile`, so it returns inside the bar's 5-second timeout. `--json` adds `tick`: `{ran, runs, attempts}`, or `{ran: false, reason}` when it was skipped (`ticked less than 30 s ago`, `herdr unreachable`).
- `--install` sets `statusLine` in Claude Code's `settings.json` (`$CLAUDE_CONFIG_DIR`, default `~/.claude`), backing up a file that pointed elsewhere, and adds a `command` entry for `pod usage --tick` to `[ui] tab_bar_right` (an entry from 0.1 that runs `pod usage` is rewritten in place) plus a `prefix+u` shell binding for `pod usage --toggle` under `[[keys.command]]` in herdr's `config.toml` (`$HERDR_CONFIG_PATH`, default `~/.config/herdr/config.toml`) leaving the bar where it is (herdr has one bar for tabs and status, top or bottom), then runs `herdr server reload-config`. Idempotent; `--no-herdr` leaves herdr's file alone. `pod doctor` reports the wiring as the `usage` check (a warning until both are set).

### `pod hook <startup|event>`

Run by herdr, not by you. `startup` reconciles every active Dispatch against the panes that still exist (missing panes become `exited`, never `failed`). `event` handles `pane.exited`, `pane.closed`, and `pane.agent_status_changed`: it updates observations and re-offers pending mail to an agent that just went idle. That re-offer can make the next nudge attempt of the bounded wake policy (at most 5 per mailbox), at most once per invocation and at least 60 seconds after the previous recorded attempt (a recipient found working or a pane not yet quiet records nothing, so an idle hook inside that minute still nudges mail that landed during the turn); each recorded attempt is stored in `wake_attempts` (see [state machines](state-machines.md#wake-ladder-per-mailbox)). Produces no output on success. Errors: `store_access_denied`. Exit 0, or 1 on failure. Do not call it by hand.

## Runs

### `pod run-create --objective <text> [--budget-usd <n>]`

Creates a Run and binds it to the calling terminal as coordinator. `--budget-usd` gives the Run a soft budget in dollars (a positive number, else exit 2): `worker-start` and `dispatch` are refused with `budget_exceeded` once its workers have reported spending it, and a start receipt carries a `warnings` entry above 80 percent. Cost is what the agents report and may differ from the bill. A Run with a budget stores it as `run.budget_usd` and repeats it as `budgetUsd` beside `run`.

```json
{"run": {"id": "run_7f3a2c", "objective": "Add rate limiting to the API", "coordinator_terminal_id": "term_0f69c67b", "coordinator_handle": "term_0f69c67b", "consumer_generation": 1, "home_cwd": "/Users/me/api", "depth": 0, "created_at": "2026-10-05T14:00:00.000000Z", "updated_at": "2026-10-05T14:00:00.000000Z"},
 "mutation": {"requestId": null, "replayed": false}}
```

Errors: `not_in_herdr`, `store_access_denied`. Exit 0.

### `pod run-use --id <run_id>`

Binds the calling terminal to an existing Run as its coordinator and bumps `runs.consumer_generation`. Any outstanding Delivery from an older generation becomes `fenced` and its mail is offered again. Use after a context reset.

```json
{"run": {"id": "run_7f3a2c", "consumer_generation": 2}, "fencedDeliveries": 1}
```

Errors: `not_in_herdr`, `store_access_denied`. A missing Run is a usage error (exit 2); there is no dedicated code. Exit 0.

### `pod run-current`

Shows the Run bound to the calling terminal, with `source` set to `bound`. Read-only. With no binding, `run` is `null` and it exits 0.

```json
{"run": {"id": "run_7f3a2c", "objective": "Add rate limiting to the API", "consumer_generation": 2}, "source": "bound"}
```

Errors: `store_access_denied`.

### `pod run-list`

Lists the Runs in the current session's store, most recently bound first: each is the full Run row (as `run-create` prints it) plus `tasks` (count) and `activeWorkers` (active Dispatches). There is no paging, so `nextCursor` is always null. Read-only.

```json
{"runs": [{"id": "run_7f3a2c", "objective": "Add rate limiting to the API", "coordinator_handle": "term_0f69c67b", "consumer_generation": 2, "updated_at": "2026-10-05T14:20:00.000000Z", "tasks": 2, "activeWorkers": 0}], "nextCursor": null}
```

Errors: `store_access_denied`. Exit 0.

### `pod run-show --id <run_id>`

Shows one Run with its task and worker counts (tasks by `status`, workers by `terminal_state`). A Run with a budget also reports `budgetUsd` and `spentUsd`, the sum of the cost its workers' status lines reported (zero when none matched). Read-only.

```json
{"run": {"id": "run_7f3a2c", "objective": "Add rate limiting to the API", "depth": 0},
 "tasks": {"completed": 2}, "workers": {"released": 2}}
```

Errors: `store_access_denied`. A missing Run is a usage error (exit 2). Exit 0.

## Tasks

### `pod task-create --spec <text> [--task-title <t>] [--display-name <n>] [--deps <json>] [--parent <task_id>]`

Creates a Task, `ready` when every dependency is already `completed`, otherwise `pending`. `--spec` is required. `--deps` is a JSON array of task IDs. A dependency must exist in the Run (`task_not_found` otherwise). Cycles cannot form because dependencies are fixed at creation. A dependency that is already `failed` leaves the new Task `pending` under attention `waiting_on_failed_dependency`. `--parent` groups Tasks for display; it must be in the same Run and gates nothing. Spec text must name target, change, constraints, ownership, and observable acceptance; pod does not parse it.

```json
{"task": {"id": "task_a1b2c3", "run_id": "run_7f3a2c", "title": "Implement limiter", "status": "ready", "deps": []}}
```

Errors: `task_not_found` (a dependency or parent is not in this Run), `store_access_denied`. Exit 0.

### `pod task-list [--status <s>] [--ready] [--brief] [--run <id>]`

Lists the Run's Tasks. `--status` filters by one of `pending|ready|dispatched|completed|failed|blocked`. `--ready` lists only `ready` Tasks. `--brief` drops the spec. Tasks whose dependency failed carry `attention: waiting_on_failed_dependency` and a `nextAction`. Each Task that has an ended Dispatch also shows that Dispatch's closure: `termination_reason` and `termination_target` (spec section 8.1). The two fields sit on the Task row and come from its latest ended Dispatch. Read-only.

```json
{"runId": "run_7f3a2c", "tasks": [
  {"id": "task_d4e5f6", "title": "Review limiter", "status": "ready", "deps": ["task_a1b2c3"]},
  {"id": "task_a1b2c3", "title": "Implement limiter", "status": "completed", "deps": [], "termination_reason": "no_follow_on", "termination_target": null}
], "count": 2}
```

Tasks are newest first.

Errors: `store_access_denied`. Exit 0.

### `pod task-update --id <task_id> --status <s> [--result <json>]`

Sets a Task's status, with an optional JSON `result`. Guards: `dispatched` requires an active Dispatch; other statuses are refused while a Dispatch is active; `completed` and `failed` are refused while a supervised worker is live.

```json
{"task": {"id": "task_a1b2c3", "status": "completed", "result": {"note": "verified by hand"}}, "promoted": ["task_d4e5f6"]}
```

Errors: `task_not_found`, `task_guard_refused` (exit 1, `nextSteps` explains the unmet guard), `store_access_denied`. Inspect first; a live worker is not permission to stop it. Exit 0 on success.

## Gates

### `pod gate-create [--for coordinator|user] [--task <task_id>] --question <text> [--options <json>] [--evidence <ref>] [--recommendation <text>]`

Two audiences. `--for coordinator` (the default) is a coordinator-owned DAG decision: it is refused while a supervised worker is live on the Task, otherwise it completes the Task's active Dispatch and sets the Task `blocked`. For the coordinator audience `--task` is required (spec section 9). `--question` is required. `--options` is a JSON array.

`--for user` is for the coordinator's own blocker, a decision only the person can make. `--task` is optional; with it, the Task is blocked exactly as for a coordinator gate, including the refusal while a worker is live. `--evidence` (a file path, a message ID, or a `worker-read` cursor), `--recommendation`, and a nonempty `--options` list are required, and the recommendation must be one of the `--options`; otherwise the command returns `gate_evidence_required`. With `--task`, pod exports the gate to the board as a decision card (question, options, recommendation, evidence) with `unblock: user`. Without `--task`, `gates.task_id` is null, the gate belongs to its Run, and it creates no pod Task and changes no Task or Dispatch state. Its board card is a blocked `question` task with id `gate:<gate id>`, `unblock: user`, and no attempts, and while any user gate is open every worker workspace's sidebar status starts with `DECISION · <question, 40 characters>`, cleared on `gate-resolve`. A new user gate (tasked or not) also fires a herdr notification titled with the Run objective and a body of the question, and runs the `[notify] command` (a failure of either is an entry in the receipt's `warnings`, never a failed command); a replayed command does not notify again, and neither does a coordinator gate. A `check --wait` that returns `wokeOn: attention` notifies the same way, once per worker that newly requires action, with the worker's next action as the body; an unchanged attention set never repeats it. Inspect the gate with `gate-list`; `check --peek` shows its resolution notification only while unread. Then the coordinator waits with `pod check --wait --types decision_gate`. Gates do not time out.

```json
{"gate": {"id": "gate_55aa10", "task_id": "task_d4e5f6", "audience": "coordinator", "question": "Ship v1 schema or v2?", "options": ["v1", "v2"], "status": "pending"}, "task": {"status": "blocked"}}
```

```json
{"gate": {"id": "gate_71c3d2", "task_id": null, "audience": "user", "question": "Rate limit by API key or client IP?", "options": ["api_key", "client_ip"], "evidence": "msg_31ac42", "recommendation": "api_key", "status": "pending"}}
```

Errors: `task_not_found`, `gate_evidence_required` (user audience), `store_access_denied`. `task_guard_refused` (exit 1) while a worker is live.

### `pod gate-resolve --id <gate_id> --resolution <text>`

Records the resolution. When the gate has a Task, the Task returns to `ready` only when every dependency is `completed`, otherwise to `pending`; a gate never bypasses the dependency rule. A coordinator gate sends no message. The next preamble for that Task includes the question and resolution. For a user gate, `gate-resolve` also stores a `decision_gate` message in the Run mailbox, which wakes a `check --wait --types decision_gate`. The person answers a user gate with this command. Its notification is engine-generated and requires no worker Dispatch identity.

```json
{"gate": {"id": "gate_55aa10", "status": "resolved", "resolution": "v2"}, "task": {"id": "task_d4e5f6", "status": "ready"}}
```

Resolving an already resolved gate changes nothing, stores no second message, and adds `"duplicate": true` (the receipt shows the first resolution).

The same call on a Task with an unfinished dependency returns it to `pending`:

```json
{"gate": {"id": "gate_55aa11", "status": "resolved", "resolution": "v2"}, "task": {"id": "task_g7h8i9", "status": "pending"}}
```

```json
{"gate": {"id": "gate_71c3d2", "audience": "user", "status": "resolved", "resolution": "api_key"}, "message": {"id": "msg_31ae10", "mailbox": "run:run_7f3a2c", "type": "decision_gate"}}
```

Errors: `store_access_denied`. Exit 0.

### `pod gate-list [--task <task_id>] [--run <id>]`

Lists gates, optionally for one Task. Each row carries `audience`, and for user gates `evidence` and `recommendation`. A user gate without a Task is listed here and also has its own board card. `check --peek` shows its resolution notification only while unread; it does not list pending gate rows. Read-only.

```json
{"runId": "run_7f3a2c", "gates": [{"id": "gate_55aa10", "task_id": "task_d4e5f6", "audience": "coordinator", "status": "resolved", "resolution": "v2"}], "count": 1}
```

Errors: `task_not_found` (with an unknown `--task`), `store_access_denied`. Exit 0.

## Recovery

### `pod request-show --request <request_id>`

Shows the state of a mutation receipt by request ID, for recovering a lost reply. Receipts are keyed by `caller_fingerprint` and `request_id`, so a caller sees only its own. `state` is `completed` (the `receipt` field holds the original receipt), `pending` (replay the command with `--retry-request`), or `absent` (inspect state before retrying); `interpretation` says the same in a sentence. Field names follow Orca's. Read-only; needs the caller that issued the request (`not_in_herdr` outside herdr).

```json
{"requestId": "req_c0ffee", "state": "completed", "method": "worker-start", "createdAt": "2026-10-05T14:02:19.000000Z", "updatedAt": "2026-10-05T14:02:21.000000Z",
 "receipt": {"dispatch": {"id": "ctx_9d41e7", "status": "dispatched"}}, "interpretation": "The mutation committed; receipt is its original result. Do not retry."}
```

An unknown ID prints `{"requestId": ..., "state": "absent", "interpretation": ...}`.

Errors: `store_access_denied`. Exit 0 for all three states.

### `pod reset --scope <run:id|all> [--force]`

Deletes one Run's state (`run:<id>`) or all state (`all`, which also deletes every receipt), and each deleted Run's board directory. Refused while the Run has live workers unless `--force`. `--scope all` requires `--force`. This is the only command that prunes messages and events.

```json
{"reset": {"scope": "run:run_7f3a2c", "deleted": true}}
```

Errors: `reset_requires_force` (exit 1, `nextSteps` names the live workers), `store_access_denied`. Exit 0 on success.
