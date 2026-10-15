# Command reference: workers and low-level dispatch

Part of the [command reference](commands.md); conventions (exit codes, `--json`, `--retry-request`) are there. Receipt layouts beyond spec-named fields are explained on that page.

## `pod worker-start`

Starts one worker for one Dispatch, or reuses an existing pane.

Flags:

| Flag | Meaning |
|---|---|
| `--task <id>` | Start on an existing Task. One of `--task` or `--spec` is required. |
| `--spec <text>` | Create a Task from this spec and start on it. |
| `--agent <name>` | Agent to launch. Resolves in order: preset name, command alias, herdr agent kind (`claude`, `codex`, `grok`, or any other kind herdr supports). |
| `--terminal <handle>` | Reuse this existing pane and agent instead of launching. One of `--agent` or `--terminal` is required. |
| `--worktree <mode>` | `current` (default), `new-child`, `new-top-level`, `path:<path>`, or `branch:<name>`. `path:` to a folder herdr does not track as a worktree (another project) joins the workspace whose panes already sit in that folder, else creates a workspace named after the folder, so each project is its own space in the sidebar. |
| `--name <x>` | Name for a new worktree (`new-child`, `new-top-level`). |
| `--handle <x>` | The worker's handle (herdr agent name). Default: `<agent>-<worktree name>` for a worktree placement, else `<agent>`, with `-2`, `-3`, ... appended only while a live agent holds the name. A `--handle` that a live agent already holds is a usage error. |
| `--repo <path:<path> \| name:<name>>` | Repository for a new worktree; required when the coordinator's folder is not in a Git repo. |
| `--base-branch <ref>` | Base for `new-top-level`: this, else config `base_branch`, else the repo default. |
| `--display-name <n>` | Label of the new worktree's space in the sidebar. Default `<repo folder>/<name>`, so a worktree made for another project says which repository it belongs to. |
| `--comment <text>` | Comment for a new worktree. |
| `--setup <run \| skip \| inherit>` | Setup policy for new worktrees; default `run`. |
| `--model <m>`, `--effort <e>` | Override model and effort. Accepted for `codex` and `claude`, and for any kind whose `[agents.<kind>]` `launch` table sets them; otherwise rejected with `launch_mapping_unsupported`. |
| `--fast` | Launch in fast mode (`codex`: `-c service_tier="fast"`). Rejected for a kind whose launch table has no `fast` entry (`claude` has none). |
| `--yolo` | Launch without approval prompts (`codex`: `--dangerously-bypass-approvals-and-sandbox`; `claude`: `--dangerously-skip-permissions`). Rejected for a kind whose launch table has no `yolo` entry. Without it the agent asks a person to approve every shell command, including its `pod` commands. |
| `--task-title <t>`, `--deps <json>`, `--parent <id>` | Used when `--spec` creates the Task. |
| `--retry-of <dispatch_id>` | Retry a failed Dispatch. Requires `--task`; valid only when the Task is `failed` or `blocked`; never inherits placement. |
| `--timeout-ms <n>` | Bound for the start stages. |
| `--run <id>` | Select the Run. |
| `--on <target>` | Recognized and refused with `remote_unsupported`. |

Creation flags (`--name`, `--repo`, `--base-branch`, `--display-name`, `--comment`, `--setup`) are rejected for `current` and existing worktrees (usage error, exit 2).

Every worker's brief is written to `<state dir>/runs/<run>/preambles/<dispatch>.txt` (mode `0600`, effect `preamble_file`), whatever the kind or size, and a one-line pointer to it is submitted through herdr's `agent.prompt`, which types the text and Enter as one ordered submission. `launch.inject` on the receipt is `prompt` or `paste`, so a person can see which path ran. `agent_prompt_stalled` means Enter was already pressed but no turn was seen: it is reported as `dispatched_unobserved` and the pointer is never typed again. Hand paste, with its idle-and-unchanged settle window (`POD_INPUT_SETTLE_MS` overrides the three seconds), the Claude Code tail rule and the Codex scrolled rule, runs only when herdr cannot prompt at all.

The worker's tab is labelled `<handle> · <task title>` (the spec's first line when the Task has no title, 40 characters at most). Reuse (`--terminal`) relabels the tab for the new Task; a failed relabel does not fail the start.

Stages, in order: preflight, placement, setup, pane, agent, inject, record. Exit 0 only when the worker is `ready`. herdr starts an agent only in a shell at its prompt, so the pane stage waits (at most 10 s, within `--timeout-ms`) for the new tab's shell to be in the foreground, and the agent stage retries herdr's "not an available shell" refusal within the same bound; a shell that never gets there fails the start at `pane`. The Dispatch's `agentKind` is the kind pod launched (a command alias keeps its kind, even when herdr first detects it as `unknown`). The inject stage submits the brief pointer through `agent.prompt`, falling back to the safe-input procedure (paste without submitting, confirm the text is visible on screen, then Enter); see [`send`](commands-messages.md#nudges-safe-input-and-the-wake-ladder).

Receipts use the names of Orca's contract samples (`tests/contract`). Success:

```json
{"runId": "run_7f3a2c", "taskId": "task_a1b2c3", "dispatchId": "ctx_9d41e7", "handle": "terra-limiter",
 "state": "ready", "stage": "record", "timeoutMs": 30000,
 "setup": {"requested": "not_applicable", "effective": "not_applicable", "source": "current_folder", "hookFound": false, "startupPolicy": "wait-for-setup", "state": "none"},
 "launch": {"requested": {"agent": "terra", "model": null, "effort": null, "fast": false, "yolo": true}, "effective": {"kind": "codex", "command": "codex", "model": "gpt-5.6-sol", "effort": null, "fast": false, "yolo": true, "args": []}},
 "effects": [{"kind": "worktree", "action": "reused", "id": "/Users/me/api"}, {"kind": "setup", "action": "not_applicable", "state": "none"},
             {"kind": "tab", "action": "created", "id": "t_7"}, {"kind": "pane", "action": "created", "id": "p_12"}],
 "residualResources": [],
 "dispatch": {"id": "ctx_9d41e7", "status": "dispatched", "...": "the Dispatch row"},
 "worker": {"state": "ready", "terminalState": "active", "paneId": "p_12", "...": "the worker row"}}
```

`setup.source` is `current_folder`, `existing_worktree`, `new_worktree`, or `reused_pane`; `requested` and `effective` are `run`, `skip`, `inherit`, or `not_applicable` (current and existing worktrees never run setup). Failure (exit 1; `state` is `failed` or `start_unknown`), which also carries `lastError`:

```json
{"dispatchId": "ctx_9d41e7", "state": "failed", "stage": "agent", "failedStage": "agent", "lastError": "timeout",
 "effects": [{"kind": "tab", "action": "created", "id": "t_7"}, {"kind": "pane", "action": "created", "id": "p_12"}],
 "residualResources": [{"kind": "pane", "role": "agent", "action": "created", "id": "p_12", "surface": "background"}],
 "nextCommands": ["pod worker-release --dispatch ctx_9d41e7", "pod worker-show --dispatch ctx_9d41e7"]}
```

`residualResources[].role` is `agent`, `setup` (the setup pane, never closed), or `workspace` (a worktree pod created, never removed); the worker row keeps the same list. `nextCommands` is omitted when there is nothing to do. A start that fails after creating a pane leaves that worker `reclaimable`. A `start_unknown` start sets the Task `blocked`. The `dispatched_unobserved` outcome means the preamble was pasted but not confirmed on screen, so Enter was withheld; the preamble is never sent twice:

```json
{"dispatchId": "ctx_9d41e7", "state": "ready", "outcome": "dispatched_unobserved", "lastError": "the preamble was pasted but ...",
 "nextCommands": ["pod worker-show --dispatch ctx_9d41e7", "pod worker-read --dispatch ctx_9d41e7"]}
```

A Claude worker is launched with `--session-id <uuid>` (unless its preset's arguments already pick a session); the worker row records it with `transcript_proof: "pod_assigned"`, or herdr's own session report with `"herdr"`, for `worker-read` (spec 11).

`dispatched_unobserved` exits 1, because exit 0 is only for `ready`; the Dispatch stays recorded. It is a receipt outcome, not an error code.

Errors: `task_not_found`, `task_not_startable` (data has `status` and `unmetDependencies`), `nested_worker_depth_exceeded`, `remote_unsupported`, `not_in_herdr`, `store_access_denied`. `limit_reached` and `budget_exceeded` refuse the start before any Dispatch is created (see below). `unknown_agent` (an `--agent` name that does not resolve), `launch_mapping_unsupported` (a `--model`, `--effort`, `--fast`, or `--yolo` flag the kind's launch table leaves empty; see config `[agents.<kind>]`), `terminal_busy` (a target terminal that already owns an active Dispatch), `herdr_unreachable`, `herdr_error`; all exit 1. A usage error (exit 2) for conflicting flags.

### Limits and budget

Preflight refuses a start with `limit_reached` when `[limits]` `max_active_workers` or the worker kind's `max_active_per_kind` is already reached, or when `pause_above_percent` is set and the latest `pod usage` record for the worker's vendor (Claude or Codex, younger than 10 minutes) shows a window at or above it. It refuses with `budget_exceeded` when the Run has a `--budget-usd` and the cost its workers reported is at or above it. Stale or missing usage never blocks a start. A start of a new agent checks the limits; `--terminal`, which reuses a live pane, does not. `dispatch` checks them for the target's kind. Once a Run's spend passes 80 percent of its budget, a successful start receipt carries a `warnings` array naming the spend. See [config](config.md#limits) and [errors](errors.md).

## `pod worker-show --dispatch <id>`

Shows one worker: its Dispatch, worker row, latest observation, projection, and `briefPath`, the file holding its brief (the same file the preamble names and every nudge points at). The Dispatch carries the closure recorded when it ended, `termination_reason` and `termination_target` (spec section 8.1; both are `null` while the Dispatch is active). The latest observation carries the three axes and the evidence behind them: `activity` (`working`, `idle_at_prompt`, `unknown`), `presence` (`present`, `exited`, `absent`, or null when unproven), `needs_input` (`{count, reasons[]}`), and `evidence_source` (`self_report` or `herdr_state`: worker reports prove activity, never presence, and herdr state is authoritative for presence; unproven activity is `unknown` and unproven presence is null). The worker row carries `preamble_input`, the outcome of the single preamble injection (`delivered`, `rendered_unconfirmed`, `skipped`, or `failed`; null until attempted). The projection carries `liveness`, `activity`, `presence`, `needsInput`, `attention`, `nextAction`, and the projected attempt outcome: `active`, `succeeded`, `failed`, `stopped`, `abandoned`, or `outcome_unknown`. `outcome_unknown` is computed, not stored: the agent is proven `exited` and its attempt has no recorded ending; a recorded success, failure, stop, or abandonment takes precedence. Read-only.

The Dispatch, worker, and observation rows use camelCase names (Orca's `worker-show`), and the Dispatch also carries `task_id`.

A Claude worker whose session pod has seen (the status line runs `pod usage --record`, which stores each payload by `session_id`; the worker row's `session_id` matches it) reports `costUsd`, the session cost Claude Code last reported. It is `null` when no cost is known, never `0`. `worker-list` rows carry it the same way, and the board appends it to the task note as `· $<cost> reported`. The figure is the agent's own report and may differ from the bill. Codex has no equivalent: its `app-server` (0.160.1) offers account-level usage and a token-count notification pushed to the connected client, but no per-thread query, and neither gives a dollar cost, so Codex workers show `null`.

```json
{"dispatch": {"id": "ctx_9d41e7", "taskId": "task_a1b2c3", "task_id": "task_a1b2c3", "status": "dispatched", "lastHeartbeatAt": "2026-10-05T14:09:00Z", "lastPhase": "implementing", "terminationReason": null, "terminationTarget": null},
 "worker": {"state": "ready", "terminalState": "active", "preambleInput": "delivered", "userOwned": false, "transcriptProof": "pod_assigned"},
 "observation": {"liveness": "live", "activity": "working", "presence": "present", "needsInput": {"count": 0, "reasons": []}, "evidenceSource": "herdr_state", "herdrStatus": "working", "observedAt": "2026-10-05T14:09:30Z"},
 "projection": {"liveness": "live", "activity": "working", "presence": "present", "needsInput": {"count": 0, "reasons": []},
   "attention": {"categories": [], "requiresAction": false}, "nextAction": {"kind": "none", "argv": []}, "outcome": "active"},
 "wakeLadder": {"exhausted": false, "attempts": []},
 "briefPath": "/home/user/.local/state/pod/default/runs/run_a1b2c3/preambles/ctx_9d41e7.txt"}
```

`nextAction` is `{kind, argv}`: `argv` is the literal command and `kind` names it, or `kind: "none"` with an empty `argv` when the right move is to wait. pod suggests `worker-show` for an `exited` attempt with no recorded ending or an exhausted ladder, `worker-read` when a prompt waits on a person (`approval`, `input`), and `worker-release` for a settled worker still in `reclaimable`, `release_pending`, or `release_unknown`. `unverifiable` alone never suggests stop, abandon, retry, or release. A worker herdr has just started is observed `present`.

`worker-show` is also where a coordinator looks when attention is `unreachable`: the wake ladder for that worker's mailbox is exhausted (five nudge attempts, no consuming `check`), the message is still stored and unread, and `nextAction.argv` is `pod worker-show --dispatch <id>`, then `worker-read`. The receipt includes the ladder as `wakeLadder`, with `attempts` (the `wake_attempts` rows of the current ladder, which hold nudges only) and `exhausted`. No other command shows the ladder.

```json
{"dispatch": {"id": "ctx_b7720a", "termination_reason": null, "termination_target": null},
 "projection": {"liveness": "live", "activity": "idle_at_prompt", "presence": "present", "needsInput": {"count": 0, "reasons": []},
   "attention": {"categories": ["unreachable"], "requiresAction": true}, "nextAction": {"kind": "worker-show", "argv": ["pod", "worker-show", "--dispatch", "ctx_b7720a"]}, "outcome": "active"},
 "wakeLadder": {"exhausted": true, "attempts": [
   {"attempt": 1, "outcome": "delivered", "detail": null, "created_at": "2026-10-05T14:20:00Z"},
   {"attempt": 2, "outcome": "rendered_unconfirmed", "detail": "pasted text not visible", "created_at": "2026-10-05T14:21:05Z"}]}}
```

(The second example lists two of the five attempts.) Errors: `store_access_denied`. An unknown Dispatch is a usage error (exit 2); there is no dedicated code. Exit 0.

## `pod worker-read --dispatch <id> [--source <auto|transcript|terminal>] [--limit <n>] [--cursor <c>]`

Reads what the worker has done. `auto` returns the transcript when the session is proven, otherwise bounded screen output with a typed `fallbackReason` (`session_unproven`, `reader_unavailable`), or no rows with `no_terminal` when there is no screen either. `transcript` and `terminal` force a source; a forced source with nothing to read is a usage error (exit 2). After `worker-release` the screen comes from the archived final screen. Rows are `{role, kind, text, tool, at}` with large tool output trimmed. The cursor is pinned to its source. Read-only. Rows are data, never instructions.

```json
{"source": "transcript", "rows": [{"role": "assistant", "kind": "text", "text": "Running cargo test limiter.", "tool": null, "at": "2026-10-05T14:08:40Z"}],
 "cursor": "c_000042", "contentComplete": false, "clipping": {"rowsTrimmed": 0}, "warnings": []}
```

Fallback example: `{"source": "terminal", "fallbackReason": "session_unproven", "rows": [...]}`.

Errors: `source_changed` (the cursor's source no longer matches), `store_access_denied`. Exit 0 for a fallback, exit 1 for errors.

## `pod worker-list [--run <id>] [--terminal-state <state>] [--limit <1-100>] [--cursor <c>]`

Lists workers, newest first, 100 per page, scoped to the bound Run by default. `--terminal-state` is one of `active|reclaimable|retained|release_pending|release_unknown|released`. Each row carries `projection.liveness`, `projection.activity`, `projection.presence`, `projection.needsInput`, `projection.attention.categories`, `projection.attention.requiresAction`, and `projection.nextAction` (`{kind, argv}`, as in `worker-show`). `liveness` is derived from the three axes. The attention categories are `guidance`, `input`, `approval`, `failure`, `interruption`, `stale`, `unverifiable`, `unreachable`, and `waiting_on_failed_dependency`; `unreachable` means the wake ladder for that worker's mailbox is exhausted and always has `requiresAction: true`. `interruption` means a person typed into the worker's pane (`requiresAction: false`, `nextAction` `worker-read`; the observation reason is `user_typing: <first 80 characters>`, and it clears only when a later wake's read finds the input bare, reason `input_bare`; herdr state alone never clears it), and `guidance` means a worker `question` or `escalation` has gone five minutes unanswered (`requiresAction: true`, `nextAction` `reply --id <message>` or `worker-read`). Each row also shows the Dispatch's `terminationReason` once it has ended, and its `briefPath`. The receipt's `scope.source` is `flag`, `bound`, or `all`; `counts` counts the scope's workers by terminal state (before `--terminal-state`), and `page` carries `limit`, `total` (after the filter), `hasMore`, and `nextCursor`. Read-only.

```json
{"scope": {"source": "bound", "run": "run_7f3a2c"},
 "workers": [{"dispatchId": "ctx_9d41e7", "taskId": "task_a1b2c3", "runId": "run_7f3a2c", "workerState": "succeeded", "dispatchStatus": "completed",
   "agentTerminalHandle": "terra-k3x9", "terminalState": "reclaimable", "terminationReason": "no_follow_on", "terminationTarget": null,
   "projection": {"liveness": "live", "activity": "idle_at_prompt", "presence": "present", "needsInput": {"count": 0, "reasons": []},
     "attention": {"categories": [], "requiresAction": false}, "nextAction": {"kind": "worker-release", "argv": ["pod", "worker-release", "--dispatch", "ctx_9d41e7"]}, "outcome": "succeeded"}}],
 "counts": {"active": 0, "reclaimable": 1, "retained": 0, "release_pending": 0, "release_unknown": 0, "released": 0},
 "page": {"limit": 100, "total": 1, "hasMore": false, "nextCursor": null},
 "waitingOnFailedDependency": [],
 "warnings": []}
```

`waitingOnFailedDependency` lists the Run's `pending` Tasks whose dependency failed (spec 9), each `{taskId, failedDependency, attention: "waiting_on_failed_dependency", nextAction}` with the same `pod dispatch-show --task <failed dependency>` next action as `task-list`, plus one line in `warnings`. It is filled when the list is scoped to one Run.

The reclaimable example selects release as its next accounting action; reuse or retention is also valid. pod suggests release because the spec names the three choices but not their precedence. An empty `workers` array from `--terminal-state reclaimable` means no reclaimable workers remain; also finish any `release_pending` or `release_unknown` cleanup. Errors: `store_access_denied`. Exit 0.

## `pod worker-stop --dispatch <id>`

Fences the Dispatch and closes only the exact supervised agent pane, and only when pod created it, after archiving its final screen (as release would) so the worker stays readable. A `dispatch --to` pane pod did not create stays open and is listed in `leftAlone` as `pane_not_created_by_pod`. Pending questions on the Dispatch are closed. An active Dispatch is recorded with `termination_reason: canceled` when fenced; an accepted ending is never overwritten. Use on proof (an `exited` verdict, or the worker's own report), never on `unverifiable`.

```json
{"dispatchId": "ctx_9d41e7", "state": "reclaimable", "outcome": "stopped", "closedPanes": ["p_12"], "closedTabs": [], "leftAlone": [],
 "archive": {"source": "final_screen", "status": "preserved", "bytes": 4120},
 "dispatch": {"id": "ctx_9d41e7", "status": "failed", "terminationReason": "canceled", "terminationTarget": null}, "worker": {"state": "stopped", "terminalState": "reclaimable"}}
```

On a Dispatch that already ended, `worker-stop` and `worker-abandon` change nothing (receipt `outcome: "unchanged"`), except that a `stop_unknown` stop retries its close. The fenced Dispatch is recorded `failed` (counting toward the circuit breaker, see [state machines](state-machines.md) D8) and the worker ends `reclaimable` so that `worker-release` can archive and account for it. Errors: `not_in_herdr`, `store_access_denied`. A `stop_unknown` outcome (pane close could not be confirmed) exits 1. Exit 0 otherwise.

## `pod worker-abandon --dispatch <id>`

Fences the Dispatch and touches no process or pane. Pending questions are closed. An active Dispatch is recorded with `termination_reason: canceled` when fenced; an accepted ending is never overwritten.

```json
{"dispatchId": "ctx_9d41e7", "state": "reclaimable", "outcome": "abandoned", "dispatch": {"status": "failed", "terminationReason": "canceled"}, "worker": {"state": "abandoned", "terminalState": "reclaimable"}}
```

The resulting `terminal_state` and Dispatch status are as for `worker-stop`. The worker ends `reclaimable`, not `retained`, so the coordinator still makes one of the three completion choices (spec section 7.4) and the reclaimable check catches it. Retaining needs a recorded reason, which abandon does not supply. A later `worker-release` leaves an active or unproven pane alone, so nothing is closed that pod cannot prove it may close. Errors: `store_access_denied`. Exit 0.

## `pod worker-retain --dispatch <id>`

Keeps the pane and records a reason in `workers.retained_reason`. One of the three completion choices.

```json
{"dispatchId": "ctx_9d41e7", "state": "retained", "outcome": "retained", "worker": {"terminalState": "retained", "retainedReason": "user will review the diff in this pane", "userOwned": true}}
```

The reason is supplied with a `--reason <text>` flag; without it, pod records `coordinator retained`. Errors: `store_access_denied`. Exit 0.

## `pod worker-release --dispatch <id>`

Archives the final screen text for every agent kind, whether or not a transcript reader exists, then closes the worker's tab only if pod created it. When a person has split another pane into that tab (herdr's `tab.get` reports more than one pane), only the worker's pane is closed and it is listed in `closedPanes`. Reused, pre-existing, setup, coordinator, active, user-taken-over, and unproven panes are left alone. The setup pane is never closed. Safe to call again; a worker in `release_pending` or `release_unknown` is retried.

```json
{"dispatchId": "ctx_9d41e7", "state": "released", "outcome": "released", "archive": {"source": "final_screen", "status": "preserved", "bytes": 4120},
 "closedPanes": [], "closedTabs": ["t_7"], "leftAlone": [], "dispatch": {"...": "the Dispatch row"}, "worker": {"terminalState": "released"}}
```

Every worker receipt (stop, abandon, retain, release) has this layout, and like `worker-start` and `worker-show` it spells the `dispatch` and `worker` rows in camelCase (`terminalState`, plus Orca's `task_id` on the Dispatch): `state` is the worker's `terminal_state` afterwards and `archive.status` is `preserved` or `absent`. When a pane is left alone, `leftAlone` gives the reason (`pane_not_created_by_pod`, `user_taken_over`, `no_worker_pane`, `setup_pane`, `coordinator_pane`, `active`, `pane_already_closed`), and `terminal_state` becomes `released`. herdr reports no typing into a pane (protocol 22), so a pane that closed while pod was neither stopping nor releasing it is what marks a worker user-taken-over ([herdr adapter](herdr-adapter.md) item 10); retain marks it too. Leaving a pane alone still counts as released, because pod's obligation to account for the worker is met. A worker that is still `active` (its Dispatch has not ended) is not released at all: the receipt lists `active` in `leftAlone`, nothing is archived or closed, and `terminal_state` stays `active`, because release starts only from `reclaimable` or `retained` ([state machines](state-machines.md) R7).

Errors: `store_access_denied`. A close that cannot be confirmed leaves `release_unknown` and exits 1. Exit 0 otherwise.

## `pod worker-gc [--run <id>]`

Releases every worker of the Run that is `reclaimable`, `release_pending`, or `release_unknown`, each exactly as `worker-release` would: the final screen is archived, then the tab closes only if pod created it. Retained workers are kept, because retain is a choice already made, and active workers are still working; both are listed in `kept` with their reason. Without `--run` the Run bound to the caller's terminal is swept. Run it after a batch of `worker_done` reports instead of one `worker-release` per worker, and whenever `worker-list --terminal-state reclaimable` still returns rows.

```json
{"runId": "run_8f2c1a", "released": [{"dispatchId": "ctx_9d41e7", "state": "released", "outcome": "released", "closedTabs": ["t_7"], "closedPanes": [], "leftAlone": [], "...": "a worker-release receipt"}],
 "kept": [{"dispatchId": "ctx_4b0c22", "terminalState": "retained", "reason": "retained"}]}
```

Errors: `store_access_denied`; a usage error when no Run is bound and `--run` is absent. Exit 1 when any release could not be confirmed (`release_unknown`), 0 otherwise.

## `pod dispatch --task <id> --to <handle> [--no-inject] [--dry-run] [--return-preamble]`

Low level: assigns a Task to an existing agent handle without launching anything, and types the preamble and spec into its pane the way `worker-start` does. This is how a retained worker takes its next Task. The pane's cleanup moves to the new Dispatch as on reuse: a `reclaimable` or `retained` worker that last held the pane is recorded `released`, and pod's ownership of the pane, its tab, and its worktree carry over, so releasing the new Dispatch closes the tab if pod created it.

| Flag | Meaning |
|---|---|
| `--task <id>` | Task to dispatch. |
| `--to <handle>` | Target agent handle or terminal. |
| `--no-inject` | Record the Dispatch and type nothing; the caller pastes the preamble itself, usually with `--return-preamble`. |
| `--inject` | Accepted for Orca's contract; injection is the default. A failed injection counts toward the circuit breaker. |
| `--dry-run` | Validate and show what would happen; change nothing. |
| `--return-preamble` | Include the rendered preamble in the receipt. |

```json
{"dispatch": {"id": "ctx_a01b22", "task_id": "task_a1b2c3", "status": "dispatched", "assignee_handle": "codex"}, "injected": true}
```

With `--inject`, an injection that typed nothing records the Dispatch `failed` (counting toward the circuit breaker) and the receipt's `outcome` is `failed`, exit 1.

Errors: `task_not_found`, `task_not_startable`, `nested_worker_depth_exceeded`, `limit_reached`, `budget_exceeded`, `not_in_herdr`, `store_access_denied`. With `--inject`, `dispatched_unobserved` is a receipt outcome, not an error code (same exit-code assumption as `worker-start`). A target terminal that already owns an active Dispatch is refused with exit 1. Exit 0 on success.

## `pod dispatch-show --task <id>`

Shows the Dispatches of a Task, with the active one marked, so a coordinator can see `failure_count`, `last_failure`, and the retry chain. `dispatch` is the current one (Orca's single row): the active Dispatch, else the newest. Read-only. `dispatches` lists them oldest first.

```json
{"task_id": "task_a1b2c3", "dispatch": {"id": "ctx_9d41e7", "status": "completed", "...": "the Dispatch row"}, "dispatches": [
  {"id": "ctx_9d41e7", "status": "completed", "retry_of_dispatch_id": null, "failure_count": 0, "active": false}]}
```

Errors: `task_not_found`, `store_access_denied`. Exit 0.
