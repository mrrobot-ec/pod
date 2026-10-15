# pod - Orca-style orchestration for herdr

**Status:** design, reviewed 2026-10-12; owner decisions applied; final consistency checked · **Date:** 2026-10-05 · **Target:** herdr ≥ 0.9.3, macOS and Linux

## 1. Summary

`pod` is a single Rust binary, shipped as a herdr plugin, that gives herdr the
supervised multi-agent workflow Orca provides: a coordinator agent creates a
Run, defines Tasks with dependencies, starts workers in panes or new worktrees,
exchanges durable messages with them, and accounts for every worker until it is
reused, retained, or released.

The goal is to stop using Orca entirely while keeping the same workflow. Command
names, flags, JSON shapes, and rules follow Orca's orchestration contract
(github.com/stablyai/orca, MIT) so existing habits and skills carry over with
`pod` in place of `orca orchestration`.

### Why

Cross-vendor supervision (a Claude coordinator over Codex workers) with proof-based completion is the reason pod exists: every vendor-native multi-agent feature coordinates only that vendor's own agents (`docs/reviews/2026-10-14-research/3-vendor-native.md`, `5-pod-distinctions.md`).

### Success criteria

1. A Claude session in a herdr pane can coordinate a build the size of the
   Connected Accounts run (about 100 tasks, 5 worktrees, several repositories)
   with Orca uninstalled.
2. The terra → sol loop works: a codex worker implements, a second codex worker
   at `xhigh` effort reviews, findings go back to the first worker in the same pane.
3. A colleague installs it with one command and `pod doctor` tells them
   everything else they need.
4. Non-waiting commands finish in under 50 ms; a waiting coordinator wakes
   within 100 ms of a message being stored.

### Not in version 1

Remote/SSH placement (`--on`), scheduled jobs, Orca's browser / computer-use /
artifacts / account features, Orca's legacy-contract migration, Windows, a
built-in UI (herdr-dagr is the board), a cap on concurrent workers.

## 2. Glossary

| Term | Meaning |
|---|---|
| Run | A durable namespace and the coordinator's inbox. It never schedules or places workers. |
| Task | A unit of work with a spec and dependencies. |
| Dispatch | One authoritative attempt at a Task. Lifecycle authority comes only from the active Dispatch. |
| Coordinator | The agent that owns a Run. |
| Worker | An agent started by `worker-start` for one Dispatch. |
| Handle | A worker's or coordinator's address: the herdr agent name (workers) or terminal ID (coordinator). |
| Mailbox | `run:<id>` (coordinator) or `dispatch:<id>` (one worker attempt). |
| Delivery | One FIFO batch of messages handed to a mailbox's consumer, replayed until acknowledged. |
| Preset | A named worker configuration in pod's config (agent kind + model + effort + extra args). |
| Closure reason | Why a Dispatch ended. Required on a failed `worker_done`: `no_follow_on`, `handed_off`, `blocked_on`, `denied`, `canceled`, `escalated`, `superseded`. |
| Activity axes | The three independent observations of a worker: `activity`, `presence`, `needs_input`. `liveness` is derived from them. |
| Wake ladder | The store-derived sequence of at most five nudge attempts for one mailbox; the fifth unconsumed attempt yields attention `unreachable`. |
| Specialist | Another live worker with an active Dispatch in the same Run that a worker may question directly with `ask --to dispatch:<id>`. |
| User gate | A gate with audience `user`: a recorded decision with evidence and a recommended default, answered through `gate-resolve`; only Task-linked gates have a board card in v1. |
| `terra` | The example preset for the implementing worker: a codex agent at default effort. |
| `sol` | The example preset for the reviewing worker: a codex agent at `xhigh` effort. |
| `herdr` | The terminal workspace manager that hosts every pane, agent, and worktree pod uses. pod is a plugin for it. |
| `herdr-dagr` | The optional board pane that renders a Run's `run.json`. Also called dagr. |
| Orca | The multi-agent IDE (github.com/stablyai/orca, MIT) whose orchestration contract pod follows. |

## 3. Architecture

```
 coordinator agent (herdr pane)                 worker agents (herdr panes)
        │ pod run-create / worker-start / check        │ pod ask / check / send worker_done
        ▼                                              ▼
 ┌──────────────────────── pod (one Rust binary, no daemon) ────────────────────────┐
 │ cli ── engine ── store (SQLite, WAL)                                             │
 │           │  ├── herdr adapter ──────────► herdr server (socket API)             │
 │           │  ├── transcript readers (claude, codex)                              │
 │           │  └── board exporter ─────────► run.json ─► herdr-dagr pane           │
 └───────────────────────────────────────────────────────────────────────────────────┘
        ▲ herdr plugin hooks: startup, pane.exited, pane.closed, pane.agent_status_changed
```

### 3.1 Components

| Component | Responsibility | Depends on |
|---|---|---|
| `cli` | Argument parsing, JSON/text output, exit codes. Orca's command names and flags. | engine |
| `engine` | All rules: task state machine, dispatch authority, mailboxes, deliveries, questions, gates, circuit breaker, mutation receipts. Pure logic over the store and two traits (`Herdr`, `Clock`). | store |
| `store` | SQLite access, schema migrations, transactions, wake signal. | - |
| `herdr adapter` | The only code that talks to herdr: panes, tabs, worktrees, agent start/prompt/read, agent state, event subscription, workspace metadata. Implements the `Herdr` trait. | herdr socket |
| `transcript` | Per-agent-kind readers that turn a transcript file into structured rows. | - |
| `board` | Translates store state into herdr-dagr's `run.json` (contract v3). | store |
| `doctor` | Environment checks and fixes. | all |

The engine never calls herdr directly, so it is testable with a fake `Herdr`.

### 3.2 No daemon

Every `pod` invocation starts, runs one short transaction (or one bounded
wait), and exits. The herdr server is the only long-lived process.

- **Waiting** (`check --wait`, `ask`): the process blocks on a per-run signal
  file that every committing writer touches, using the OS file-watch API, with a
  1-second fallback poll of SQLite's `data_version`. While waiting it also holds
  a herdr `events.subscribe` stream for the run's worker panes.
- **Nobody waiting**: herdr itself runs `pod hook …` on `pane.exited`,
  `pane.closed`, and `pane.agent_status_changed` (plugin event hooks, confirmed
  available in herdr 0.9.3). The hook updates worker observations in the store
  and re-offers pending mail to an agent that just went idle.
- **Ticking**: while a coordinator waits, its process runs one ladder pass per
  minute; the tab bar entry (`pod usage --tick`) runs one every 30 seconds
  when nobody waits.
- **After a herdr restart**: the plugin's startup hook runs `pod hook startup`,
  which reconciles every active Dispatch against the panes that still exist.

### 3.3 Paths

| What | Location | Override |
|---|---|---|
| Store | `$XDG_STATE_HOME/pod/<session>/pod.db` (default `~/.local/state/pod/…`) | `POD_STATE_DIR` |
| Config | `$XDG_CONFIG_HOME/pod/config.toml` (default `~/.config/pod/…`) | `POD_CONFIG` |
| Board files | `<state dir>/<session>/runs/<run_id>/run.json` | - |
| Binary link | `~/.local/bin/pod` → the plugin's installed binary | - |

`<session>` is the herdr session the caller belongs to, derived from the socket
path (`HERDR_SOCKET_PATH`, or herdr's default). Each herdr session is a separate
server with its own pane IDs, so each gets its own store.

pod resolves these paths itself and never relies on `HERDR_PLUGIN_STATE_DIR`,
which herdr sets only for commands herdr launches.

### 3.4 Caller identity

- Every pane has `HERDR_PANE_ID` in its environment. pod resolves it through
  herdr to the pane's **terminal ID**, which is stable when a pane moves between
  workspaces (pane IDs are not).
- A Run is bound to its coordinator's terminal ID by `run-create` / `run-use`.
- A Dispatch records the worker's terminal ID and the agent's process ID.
- A lifecycle message (`worker_done`, `heartbeat`, `escalation`,
  `decision_gate`) is accepted only when the caller's terminal ID equals the
  Dispatch's and the recorded agent process is an ancestor of the calling `pod`
  process, or shares its controlling terminal (a background job the agent
  started, such as a heartbeat loop, that the system reparented when its shell
  exited; accepted with a warning). Otherwise: `worker_identity_changed` or
  `caller_fenced`. A refused lifecycle message is still stored for the
  coordinator as a high-priority `status` with the reason, and runs the
  commit hook like any other mail, so the coordinator is nudged about it.
- `--from` and `--dispatch-capability` are accepted for Orca compatibility.
  `--from` must match the resolved caller when both are known.
  `--dispatch-capability` is ignored (as in Orca's local mode).
- Outside herdr (`HERDR_ENV` unset), read-only commands work with an explicit
  `--run`; mutating commands that need a caller identity are refused with
  `not_in_herdr`.

### 3.5 Plugin manifest

- `[[build]]`: `scripts/install.sh` - downloads the checksum-verified release
  binary matching the installed revision, else the latest release, or builds
  from source when there is no release; links `~/.local/bin/pod`.
- `[[startup]]`: `pod hook startup`.
- `[[events]]`: `pane.exited`, `pane.closed`, `pane.agent_status_changed` →
  `pod hook event`.
- `[[actions]]`: `open-board`, `worker-list` (opens a popup pane), `doctor`.
- `min_herdr_version = "0.9.3"`, `platforms = ["macos", "linux"]`.

## 4. Configuration

`config.toml`, all sections optional:

```toml
[defaults]
nested_depth_max = 1          # workers may not start workers
heartbeat_minutes = 5
ask_timeout_ms = 600000       # capped at 1800000

[agents.codex]                # command alias: swap the executable for a built-in kind
command = "codex"             # single token or absolute path only

[presets.terra]               # pod addition: named worker configurations
agent = "codex"               # no model: the agent runs its own default
args = []                     # extra argv passed after the launch flags

[presets.sol]
agent = "codex"
model = "gpt-5.6-sol"         # optional: a model name the agent accepts
effort = "xhigh"
fast = true                   # Codex fast mode (service tier "fast")

[repos."/Users/me/code/api"]
base_branch = "dev"
setup = ["bash", "scripts/worktree-setup.sh"]   # argv, run in the new worktree
setup_policy = "wait-for-setup"                  # or "start-immediately"
```

- `--agent <name>` resolves in order: preset name, command alias, herdr agent kind.
- A command alias follows Orca's rule: one executable token, no interpreter
  arguments, environment assignments, or shell wrappers. An unconfigured name is
  refused even if it is on `PATH`.
- Launch flags per kind (built in): `codex` → `-m <model>`,
  `-c model_reasoning_effort="<effort>"`, and `-c service_tier="fast"` for
  fast mode, `--dangerously-bypass-approvals-and-sandbox` for `--yolo`;
  `claude` → `--model <model>`, `--effort <effort>`,
  `--dangerously-skip-permissions` for `--yolo`. A `--yolo` launch also
  records trust for the worker's folder (and, for a new worktree, the
  repository it came from) in the agent's own config before the launch,
  exactly as the agent would after a person chose "trust": Codex
  `[projects."<path>"] trust_level = "trusted"` in `$CODEX_HOME/config.toml`,
  Claude Code `projects["<path>"].hasTrustDialogAccepted` in `~/.claude.json`.
  Otherwise the agent stops at its folder-trust prompt before its input line
  and the preamble is never typed. pod never answers the prompt itself. Kinds without a built-in mapping reject `--model`, `--effort`, and
  `--yolo`; only `codex` accepts `--fast` (preset `fast = true`), because
  Claude Code has no command-line flag for its fast mode. Without `--yolo`
  (preset `yolo = true`) an agent asks a person to approve every shell
  command, including the `pod` commands the preamble tells it to run.
- The start receipt reports `launch.requested` and `launch.effective`.

## 5. Data model

SQLite, WAL mode, file mode `0600`, `user_version` for migrations. Migrations
only add tables and columns.

```
runs(id, objective, coordinator_terminal_id, coordinator_handle, consumer_generation,
     home_cwd, depth, created_at, updated_at)
tasks(id, run_id, parent_id, title, display_name, spec, status, deps JSON, result JSON,
      created_by_terminal_id, created_at, completed_at)
dispatches(id, run_id, task_id, status, retry_of_dispatch_id, failure_count, last_failure,
           termination_reason, termination_target, depth, assignee_handle, assignee_terminal_id, assignee_pid,
           agent_kind, launch_requested JSON, launch_effective JSON,
           dispatched_at, last_heartbeat_at, last_phase, completed_at, created_at)
workers(dispatch_id, state, stage, placement JSON, worktree_path, workspace_id, tab_id,
        pane_id, terminal_id, owns_pane, setup_state, setup_pane_id, preamble_input, effects JSON,
        residual_resources JSON, terminal_state, retained_reason, session_id,
        transcript_path, transcript_proof, last_error, created_at, updated_at)
worker_archives(dispatch_id, kind, content, created_at)
observations(dispatch_id, liveness, activity, presence, needs_input JSON, evidence_source,
             reason, herdr_status, attention JSON, observed_at)
messages(sequence INTEGER PRIMARY KEY AUTOINCREMENT, id UNIQUE, run_id, mailbox,
         from_handle, subject, body, type, priority, thread_id, payload JSON,
         read, created_at, delivered_at, sender_terminal_id, rejected_reason,
         evidence, recommendation)
deliveries(id, run_id, mailbox, consumer_generation, message_ids JSON,
           status, created_at, acknowledged_at)
wake_attempts(id, run_id, mailbox, delivery_id, attempt, outcome, detail, created_at)
questions(message_id, run_id, dispatch_id, asker_handle, status, answer_message_id,
          answer_body, created_at, answered_at, closed_at)
gates(id, run_id, task_id, audience, question, options JSON, evidence, recommendation,
      status, resolution, created_at, resolved_at)
receipts(caller_fingerprint, request_id, method, payload_hash, state, receipt JSON,
         created_at, updated_at)
events(sequence INTEGER PRIMARY KEY AUTOINCREMENT, run_id, at, type, task_id,
       dispatch_id, actor, detail JSON)
```

Enumerations:

- `tasks.status`: `pending | ready | dispatched | completed | failed | blocked`
- `dispatches.status`: `pending | dispatched | completed | failed | circuit_broken`
- `workers.state`: `starting | ready | start_unknown | failed | succeeded | stopping | stop_unknown | stopped | abandoned`
- `workers.terminal_state`: `active | reclaimable | retained | release_pending | release_unknown | released`
- `observations.liveness`: `live | unverifiable | exited`
- `observations.activity`: `working | idle_at_prompt | unknown`
- `observations.presence`: `present | exited | absent` (nullable when unproven)
- `observations.evidence_source`: `self_report | herdr_state`
- `dispatches.termination_reason`: `no_follow_on | handed_off | blocked_on | denied | canceled | escalated | superseded`
- `wake_attempts.outcome`: `delivered | rendered_unconfirmed | skipped | failed`
- `workers.preamble_input`: `delivered | rendered_unconfirmed | skipped | failed` (null until injection is attempted)
- `deliveries.status`: `outstanding | acknowledged | fenced`
- `questions.status`: `pending | answered | closed`
- `gates.status`: `pending | resolved`
- `gates.audience`: `coordinator | user`
- `messages.priority`: `normal | high | urgent`

Column definitions that are not obvious from the names:

- `runs.consumer_generation`: an integer bumped by every `run-use`. A Delivery
  records the generation current when it was created; once the Run's generation
  is higher, that Delivery becomes `fenced` and its messages are offered again in
  a new Delivery to the new consumer.
- `receipts.caller_fingerprint`: a stable hash of the resolved caller (terminal
  ID plus session). With `request_id` it keys a mutation receipt, so one caller's
  `--retry-request` can never replay another caller's receipt.
- `worker_archives.kind`: what `content` holds. `final_screen` is the screen text
  archived at release; further kinds may be added by migration.
- `gates.task_id`: nullable for audience `user`; a gate without a Task belongs
  to its `run_id` and changes no Task or Dispatch state.

Invariants enforced in the store: one active Dispatch per Task; one active
Dispatch per terminal; one outstanding Delivery per mailbox.

Retention: messages and events are never pruned except by `pod reset`. Receipts
are pruned beyond 10,000 rows or 30 days.

## 6. Commands

All commands accept `--json`. Mutating commands accept `--retry-request <id>`.
Errors are `{"error": {"code", "message", "data": {"nextSteps"}}}` on stderr;
exit 1 for a refused or failed operation, 2 for a usage error.

| Group | Commands |
|---|---|
| Environment | `doctor`, `status`, `skills get <coordinator\|worker> [--reference <name>] [--full]`, `log --run <id>`, `board [--run <id>]`, `hook <startup\|event>` |
| Runs | `run-create --objective`, `run-use --id`, `run-current`, `run-list`, `run-show --id` |
| Tasks | `task-create --spec [--task-title] [--display-name] [--deps <json>] [--parent]`, `task-list [--status] [--ready] [--brief]`, `task-update --id --status [--result <json>]` |
| Workers | `worker-start`, `worker-show --dispatch`, `worker-read --dispatch`, `worker-list`, `worker-stop --dispatch`, `worker-abandon --dispatch`, `worker-retain --dispatch`, `worker-release --dispatch`, `worker-gc [--run]` |
| Low level | `dispatch --task --to <handle> [--no-inject] [--dry-run] [--return-preamble]`, `dispatch-show --task` |
| Messages | `send`, `check`, `ask [--to dispatch:<id>]`, `reply --id --body`, `inbox [--limit] [--terminal] [--full]` |
| Gates | `gate-create [--for coordinator\|user] [--task] --question [--options <json>] [--evidence] [--recommendation]`, `gate-resolve --id --resolution`, `gate-list [--task]` |
| Recovery | `request-show --request <id>`, `reset --scope <run:id\|all> [--force]` |

`worker-start` flags: `(--task <id> | --spec <text>)`, `(--agent <name> |
--terminal <handle>)`, `--worktree <current | new-child | new-top-level |
path:<path> | branch:<name>>`, `--name`, `--repo <path:<path> | name:<name>>`,
`--base-branch <ref>`, `--display-name`, `--comment`, `--setup <run | skip |
inherit>`, `--model`, `--effort`, `--fast`, `--yolo`, `--task-title`, `--deps`, `--parent`,
`--retry-of <dispatch_id>`, `--timeout-ms`, `--run`. `--on` is recognized and
refused with `remote_unsupported`.

`send` flags: `--subject` (required), `--to`, `--body`, `--type`, `--priority`,
`--thread-id`, `--payload <json>`, `--task-id`, `--dispatch-id`, `--outcome`, `--reason`, `--reason-target`,
`--files-modified <csv>`, `--report-path`, `--phase`, `--evidence`, `--recommendation`, `--from`,
`--dispatch-capability`. `--payload` and the typed flags are mutually exclusive.
`--outcome`, `--reason`, and `--reason-target` are valid only with `--type worker_done`.

`check` flags: `--terminal <handle>`, `--run`, `--ack <delivery_id>`,
`--unread | --peek | --all`, `--types <csv>`, `--format`, `--wait`,
`--timeout-ms`.

`worker-read` flags: `--dispatch`, `--source <auto | transcript | terminal>`,
`--limit`, `--cursor`.

`worker-list` flags: `--run`, `--terminal-state <state>`, `--limit <1-100>`,
`--cursor`. Newest first, 100 per page, scoped to the bound Run by default; the
receipt's `scope.source` is `flag`, `bound`, or `all`.

One-line descriptions of the commands this section only names:

- `inbox`: lists messages in the Run mailbox (or the mailbox of `--terminal`), newest first, read-only.
- `status`: reports pod's own state: version, paths, store health, herdr reachability, and the bound Run.
- `log`: prints the Run's `events` rows in sequence order, read-only;
  `--follow` streams new events as they are appended until `--timeout-ms`.
- `run-show`: shows one Run with its task and worker counts.
- `run-list`: lists the Runs in the current session's store.
- `run-current`: shows the Run bound to the calling terminal.
- `dispatch-show`: shows the Dispatches of a Task, with the active one marked.
- `request-show`: shows the state of a mutation receipt by request ID, for recovering a lost reply.

## 7. Worker lifecycle

### 7.1 `worker-start` stages

1. **Preflight.** Task exists in the Run (`task_not_found`); Task is `ready`, or
   `failed`/`blocked` with a valid `--retry-of` (`task_not_startable`, with
   `status` and `unmetDependencies`); depth within limit
   (`nested_worker_depth_exceeded`); agent name resolves; target terminal owns
   no active Dispatch; no `[limits]` cap or usage pause applies
   (`limit_reached`) and the Run's budget is not spent (`budget_exceeded`),
   see 12.4.
2. **Placement.** See 7.2.
3. **Setup.** New worktrees only, see 7.3.
4. **Pane.** One new tab per worker, named after the worker, created without
   focus. A new worktree's workspace uses its first tab.
5. **Agent.** `herdr agent start <handle> --kind <kind> --pane <id> -- <argv>`.
   Success means herdr detected the agent and it is ready for input. The
   model, effort, fast, and yolo flags come from a launch table per kind
   (`[agents.<kind>]` `launch` in config, with built-in tables for `codex` and
   `claude`); a flag the table leaves empty is `launch_mapping_unsupported`.
6. **Inject.** The preamble plus task spec is written to a file for every
   agent kind, and a one-line pointer to it goes in through `herdr agent
   prompt`. If herdr reports the prompt stalled, Enter was already pressed:
   the outcome is `dispatched_unobserved` and the pointer is never sent twice.
   Only when herdr cannot prompt does the safe-input procedure in 8.2 run:
   paste, confirm it rendered, then Enter. `launch.inject` is `prompt` or
   `paste`.
7. **Record.** Dispatch `dispatched`, worker `ready`, Task `dispatched`.

Exit 0 only for `ready`. On `failed` or `start_unknown`, exit 1 with
`failedStage`, `setup`, `effects`, `residualResources`, and recovery commands.
A start that fails after creating a pane leaves that worker `reclaimable`; a
`start_unknown` start sets the Task `blocked`.

Worker handles are herdr agent names matching `[a-z][a-z0-9_-]{0,31}`, unique
among live agents: `--handle` when given (a live collision is a usage error),
else `<preset-or-kind>-<worktree name>` for a worktree placement, else
`<preset-or-kind>`, with `-2`, `-3`, ... appended only while the name is live.
The worker's tab is labelled `<handle> · <task title>` and relabelled on reuse.

### 7.2 Placement

| `--worktree` | Behaviour |
|---|---|
| `current` (default) | New tab in the coordinator's folder. No Git required; plain folders are valid. |
| `new-child --name X` | New worktree branched from the current branch (stacked). |
| `new-top-level --name X` | New worktree branched from the repo's base: `--base-branch`, else config `base_branch`, else the repo default. |
| `path:<path>` / `branch:<name>` | An existing worktree, opened through herdr; a new tab in it. A `path:` folder herdr does not track as a worktree joins the workspace whose panes sit in it, else gets a new workspace named after the folder whose first tab is the worker's. A new worktree's space is labelled `--display-name`, default `<repo folder>/<name>`. |

- `--repo` selects the repository for a new worktree and is required when the
  coordinator's folder is not inside a Git repository.
- Creation flags (`--name`, `--repo`, `--base-branch`, `--display-name`,
  `--comment`, `--setup`) are rejected for `current` and existing worktrees.
- Worktrees are created through `herdr worktree create`. **pod never removes,
  cleans, or commits in a worktree.**
- Every worktree action starts from the repository's source checkout, the
  folder herdr's own listing names for any folder inside the repository.
  herdr refuses a linked worktree as a starting point, so a coordinator that
  sits in one still creates and opens from the parent, and a `path:` target
  in another repository opens from that repository's checkout.
- A fresh worker means a fresh agent pane, not a new worktree. Create a worktree
  only when asked or when a concrete filesystem conflict makes sharing unsafe.

### 7.3 Setup

`--setup run | skip | inherit` (default `run` for new worktrees; current and
existing worktrees never rerun setup). The repo's `setup` argv runs in its own
pane in the new worktree. `wait-for-setup` holds the prompt until setup exits 0;
a setup failure fails the start at stage `setup`. `start-immediately` runs setup
beside the agent and reports `setup: running`. The setup pane is never closed by
release.

### 7.4 Settlement and cleanup

- A worker-reported outcome is accepted only through a valid `worker_done`
  from its active Dispatch. It ends the attempt and updates the Task under 7.5;
  a retryable failure can return the Task to `ready`. Looking idle is never completion.
- After an accepted `worker_done`, the coordinator does exactly one of:
  - **Reuse:** `worker-start --task <next> --terminal <handle>`, or
    `dispatch --task <next> --to <handle>` for a retained worker - same pane,
    same agent context, the brief typed in; cleanup ownership moves to the
    new Dispatch and the worker that last held the pane is released.
  - **Retain:** `worker-retain` - keep the pane, with a recorded reason.
  - **Release:** `worker-release` - archive the final screen text, then close
    the worker's tab only if pod created it. Reused, pre-existing, setup,
    coordinator, active, user-taken-over, and unproven panes are left alone.
    Safe to call again.
- `worker-stop` fences the Dispatch and closes only the exact supervised agent
  pane. `worker-abandon` fences the Dispatch and touches no process.
- `worker-gc` releases every `reclaimable` worker of the Run (and retries
  `release_pending` and `release_unknown`) under the release rules in one
  call; retained and active workers are kept and listed with their reason.
- If the user closes a worker pane, pod treats the pane as user-owned from
  then on. A pane a person types into is left alone, not taken over: the
  screen read before a prompt or paste (8.2) finds text on the input line that
  pod did not put there, no nudge or paste touches the pane until a later read
  finds the input bare, and typing alone does not make the pane user-owned.
  Closing or retaining it still does.
- The coordinator's turn is not finished while `worker-list --terminal-state
  reclaimable` returns rows.

### 7.5 Retry and the circuit breaker

- `--retry-of <dispatch_id>` requires `--task`, is valid only when the Task is
  `failed` or `blocked`, and never inherits placement.
- `failure_count` is cumulative per Task and carried across retries. A failed
  Dispatch below 3 returns the Task to `ready`. At 3 the Dispatch is
  `circuit_broken` and the Task is `failed`.
- `worker_done --outcome failed` counts toward the breaker like any other
  failed Dispatch, except with `--reason denied` or `canceled`, which fail the
  Task directly because a retry is pointless; these two reports do not increment
  `failure_count` and leave the Dispatch `failed`, not `circuit_broken`.
- A failed `dispatch` injection counts toward the breaker.
- A start that fails at `preflight` or `placement`, before any pane exists,
  does not count: nothing ran, so the failure is pod's or herdr's. The
  Dispatch is `failed`, the Task returns to `ready`, and `failure_count` is
  unchanged.

### 7.6 Liveness and attention

pod observes a worker on three separate axes and never collapses them into one
state (after OpenRig's activity taxonomy):

| Axis | Values | Meaning |
|---|---|---|
| `activity` | `working \| idle_at_prompt \| unknown` | What the agent is doing. |
| `presence` | `present \| exited \| absent` (nullable) | Whether the agent process still exists in its pane. `absent` means the pane is confirmed gone; null means presence is unproven. |
| `needs_input` | `{count, reasons[]}` | How many prompts wait on a human and why (`approval`, `input`). A count, never a state. |

Evidence comes from two sources, and each observation row records which
(`evidence_source`): the worker's own reports (`heartbeat`, `worker_done`,
`ask`), which prove activity but never presence, and herdr's agent state with
the plugin event hooks, which is authoritative for presence. pod does not read
screen text to infer liveness in version 1; its one screen read, before a prompt
or paste, only looks for a person typing (7.4). Unproven activity is `unknown`;
unproven presence is null, as in the axis table. Neither is guessed around.

`liveness` is the fleet verdict derived from the axes, and is what every rule
in this document means by `live`, `exited`, and `unverifiable`:

| Evidence | `liveness` | Attention |
|---|---|---|
| `presence: present` (including unknown activity) | `live` | `approval` or `input`, `requiresAction: true`, when `needs_input.count > 0`; otherwise none |
| `presence: exited` or `absent` (agent gone, pane exited or confirmed closed) | `exited` | `failure`, `requiresAction: true`, only if the attempt is still unsettled |
| `presence: null`: process presence cannot be established (herdr unreachable, unknown presence, no observation) | `unverifiable` (with `reason`) | `unverifiable` |
| no heartbeat for 10 minutes | unchanged | `stale` (warning only) |
| wake ladder exhausted (8.2) | unchanged | `unreachable`, `requiresAction: true` |
| the screen read before a prompt or paste found text a person typed (7.4) | unchanged | `interruption`, `requiresAction: false`; `nextAction` is `worker-read`; clears when a later read finds the input bare |
| a `question` or `escalation` from this Dispatch has gone 5 minutes without a `reply` or a `send` to the Dispatch | unchanged | `guidance`, `requiresAction: true`; `nextAction` is `reply --id <message>` for a question and `worker-read` for an escalation; clears on the reply |

Each `worker-list` row carries `projection.liveness`, `projection.activity`,
`projection.presence`, `projection.needsInput`,
`projection.attention.categories`, `projection.attention.requiresAction`, and a
literal `projection.nextAction` argv (`none` when the right move is to keep
waiting). Attention categories: `guidance`, `input`, `approval`, `failure`,
`interruption`, `stale`, `unverifiable`, `unreachable`,
`waiting_on_failed_dependency`.

`worker-show` and `worker-list` also report a projected attempt outcome:
`active`, `succeeded`, `failed`, `stopped`, `abandoned`, or `outcome_unknown`
(the agent is proven `exited` and its attempt has no recorded ending).
`outcome_unknown` is computed from the worker and observation rows, not stored.
Recorded success, failure, stop, or abandonment takes precedence over exit
observations. Worker reports and screen text do not establish process presence;
herdr state does. Unknown activity does not erase proven presence.

`unverifiable` never authorizes stop, abandon, retry, or release. Only a
positive `exited` verdict, or the worker's own report, does. A live pane can
still hold a dead or stuck agent, which is why `presence` and `activity` are
reported separately.

## 8. Messaging

### 8.1 Send

- Success means the message is durably stored. Nothing more.
- Default recipient for a worker with an active Dispatch: its Run mailbox.
  `worker_done` and `heartbeat` always go to `run:<id>` regardless of `--to`.
- `task:` recipients are rejected. A sender bound to no Run must address
  `run:<id>` or `dispatch:<id>`.
- A rejected lifecycle message is still stored, rewritten to type `status` with
  priority `high` and a `rejected_reason`, and delivered to the coordinator
  like any other mail (the commit hook runs: board, sidebar, nudge); the
  command exits 1 and its error names the stored message in `data.stored`.
- A repeated `worker_done` with the same outcome returns `settled` with
  `duplicate: true`. A conflicting one returns `inactive_dispatch`.
- **Closure reasons** (after OpenRig). `worker_done` carries
  `--reason <no_follow_on | handed_off | blocked_on | denied | canceled |
  escalated | superseded>`. `--outcome succeeded` defaults to `no_follow_on`.
  `--outcome failed` requires an explicit reason (`closure_reason_required`).
  `handed_off`, `blocked_on`, and `superseded` also require
  `--reason-target <task or dispatch id>` (`closure_target_required`). A
  target pod cannot find in the Run is accepted with a `warnings` entry, not
  refused. The reason and target
  are stored on the Dispatch as `termination_reason` and `termination_target`,
  enforced in the engine at the store boundary, and shown by `worker-show`,
  `task-list`, and `log`. `worker-stop` and `worker-abandon` record `canceled`.
  A reason explains where the work went. It does not change the reported
  outcome, but `denied` and `canceled` on failed reports select the direct Task
  failure exception in 7.5.
  All seven reasons are valid with either outcome. Stop and abandon record
  `canceled` only when fencing an active Dispatch; they do not overwrite an
  accepted ending or turn fencing into a `worker_done` settlement (7.4).
  Failed starts and gate completion have no worker-reported closure reason.

### 8.2 Nudge

The stored message row is the obligation. A nudge only points at it, so a
nudge can be repeated within the bounded policy without duplicating the message
(after OpenRig's wake ladder).

After commit, in the sender's process:

1. If a `check --wait` waiter on that mailbox matches the type, it wakes.
   Nothing is typed.
2. Otherwise pod types a nudge only when all hold: the recipient's
   `presence` is `present` and `activity` is `idle_at_prompt` on evidence no
   older than 15 seconds; the pane has been quiet for 3 seconds; the mailbox
   has no outstanding unacknowledged Delivery; no wake for that mailbox is in
   flight. An absent, unproven, or stale recipient and an outstanding
   Delivery are recorded `skipped`; a working recipient, an unproven quiet
   window, and a wake in flight are not now and record nothing (below).
3. Text: for a worker, `You have N orchestration message(s). Run \`pod check --terminal <handle>\`. Your brief: <path>.`
   where `<handle>` is its worker handle and `<path>` its brief file, so a worker
   that lost its context to compaction finds the brief again; the brief sentence
   is left out when the file does not exist. A nudge to the coordinator (the Run
   mailbox) has neither: `You have N orchestration message(s). Run \`pod check\`.`

**Safe input.** Every prompt pod sends to an agent pane, the nudge and the
preamble pointer alike, goes through `herdr agent prompt` first, which types
the text and Enter as one ordered submission. `Blocked` is `skipped` and
`Stalled` is `rendered_unconfirmed`; neither is retyped. An unreachable herdr
fails the input with no paste, since the text may already have gone in. Only
at launch does pod retry while herdr still calls a new agent inactive and wait
for its prompt before a fallback paste; a nudge never sleeps in pod, and the
tab bar's nudge asks herdr for no turn wait. When herdr refuses the prompt,
the fallback is one procedure: paste the text without submitting it
(preserving embedded newlines in a multi-line preamble);
re-read the pane and confirm the pasted text is visible; only then send Enter.
pod never sends a bare Enter it cannot attribute to text it just confirmed.
The input must be proven empty and ready before pasting. After an unconfirmed
paste, later attempts skip until that input is proven cleared; pod neither
appends another nudge nor submits or deletes uncertain text.
Each nudge attempt is stored in `wake_attempts`; the preamble injection is
recorded once in `workers.preamble_input`. Both use the outcomes `delivered` (text
confirmed, Enter sent), `rendered_unconfirmed` (text pasted, confirmation
unavailable, Enter withheld), `skipped` (a wake or safe-input precondition failed, with the
reason in `wake_attempts.detail` for nudges or `workers.last_error` for injection), or `failed`.

**Wake ladder.** Nudges for one mailbox form a bounded, store-derived retry
policy: at most 5 recorded attempts, the first at commit in the sender's
process, each later one only on an eligible trigger (an idle event hook for
the recipient, or a mutating `pod` command for that Run, which retries every
mailbox of the Run with unread mail not already in its consumer's hands, at
most one attempt per mailbox per invocation) and no sooner than 60 seconds
after the previous recorded attempt. After the fifth attempt without a
consuming `check` by the recipient, the mailbox carries attention
`unreachable`, `requiresAction: true`; for a worker mailbox the coordinator's
`nextAction` is `worker-show`, and for the Run mailbox the status is written to
the board for the user.

The position is computed from `wake_attempts` and `events`, never held in
memory, so a herdr restart or a new `pod` process neither loses nor resets it.
A consuming `check` by the recipient clears it. Only recorded attempts count:
a nudge pod typed (`delivered`, `rendered_unconfirmed`), an input that
`failed`, and a `skipped` attempt against a recipient it could not reach
(presence unproven or not `present`, evidence stale, an outstanding Delivery,
an earlier paste unconfirmed, or safe input refused). A trigger that finds the
recipient present and working, its pane not yet proven quiet for 3 seconds
(the screen changed, or the trigger cannot wait out the window), or another
process's wake for the mailbox in flight records nothing, whatever the
trigger: that is not now, not evidence about the mailbox, so the ladder keeps
its position and the next eligible trigger tries again at once. Recorded,
each of those spent a rung and started the minute, and a completion that
landed during a long turn or while a person typed in the pane was refused at
the recipient's next idle hook with nothing left to retry it (seen live
2026-10-14 and 2026-10-15). New mail does not restart a pending policy.
Read-only commands never type or advance it. There is no timer outside a
bounded wait and the tab bar tick, which are eligible triggers; exhaustion is
still a count, not an elapsed time.

A nudge is attention only. It proves nothing about reading.

### 8.3 Check

| Form | Behaviour |
|---|---|
| `check` (coordinator) | Returns the Run mailbox's oldest unacknowledged FIFO batch, up to 50, and replays it until acknowledged. |
| `--ack <delivery_id>` | Marks that batch read, then checks again. |
| `--unread` | The explicit consuming form of the default. |
| `--peek` | Read-only: unread messages, newest 100. |
| `--all` | Read-only: history, newest 100. |
| `--wait --types … --timeout-ms N` | Blocks until a matching message exists. `--types` decides only when to wake; the Delivery is always the whole batch. Only `--peek` and `--all` filter rows by type. |
| `--format` | Also renders rows as agent-readable text. |
| `check --terminal <handle>` (worker) | Returns unread messages in the worker's Dispatch mailbox and marks them read. Returns `consumer_fenced` if the Dispatch is no longer the worker's. |

- One `check --wait` waiter per Run; a second is refused with `waiter_exists`.
  `ask` and `ask --resume` wait on their own question, do not consume mailbox
  Deliveries, and do not occupy that waiter slot.
- While waiting, pod prints `{"_keepalive": true}` to stderr every 15 seconds.
- While waiting, pod also runs one wake ladder pass per minute (8.2) and
  recomputes attention, so a worker that goes `stale` (no activity, heartbeat,
  or message for 10 minutes) ends the wait with `wokeOn: attention`, the same
  as a worker that newly requires action. `stale` itself stays
  `requiresAction: false`.
- A timeout or empty result is a checkpoint, not a failure.

### 8.4 Ask and reply

- `ask --question … [--options a,b] [--timeout-ms N]` stores a `question`
  message and blocks until answered. Default 600,000 ms, maximum 1,800,000 ms.
- On timeout the question stays `pending`; the worker continues with
  `ask --resume <message_id>` and never creates a duplicate.
- `reply --id <message_id> --body …` answers it and wakes the asker.
- `ask --to dispatch:<id>` routes the question to a specialist worker instead
  of the coordinator; see 9.1.
- Pending questions are closed when their Dispatch fails or is fenced.

### 8.5 Types, priority, groups

| Type | Effect |
|---|---|
| `worker_done` | Ends the Dispatch and updates the Task under 7.5; records the closure reason; promotes dependants on success. |
| `heartbeat` | Updates `last_heartbeat_at` and `last_phase` for the active Dispatch only. |
| `escalation`, `decision_gate` | Must come from the Dispatch's owner; the dispatch ID is stamped into the payload. No automatic state change. `escalation` must carry `--evidence` and `--recommendation` (9.1). |
| `question` | Created by `ask`. |
| `status`, `merge_ready`, `handoff`, `dispatch` | Labels only. |

Priority adds a `[HIGH]` or `[URGENT]` tag. It never reorders or interrupts.

Groups: `@all`, `@idle`, `@<agent kind>`, `@worktree:<path>`. A group resolves
to the live Dispatches of the sender's Run, excluding the coordinator; one
message row per recipient, sharing a generated thread ID. Zero recipients is an
error. Lifecycle types cannot target groups.

## 9. Tasks and gates

- A Task is created `ready` when every dependency is already `completed`,
  otherwise `pending`. Promotion to `ready` happens when a dependency completes.
- `--parent` groups Tasks for display and must be in the same Run. It gates nothing.
- When a dependency fails, dependants stay `pending` (Orca's behaviour). pod
  additionally reports them under `waiting_on_failed_dependency` in `task-list`
  and `worker-list`, with a next action.
- `task-update` guards: `dispatched` requires an active Dispatch; other statuses
  are refused while a Dispatch is active; `completed`/`failed` are refused while
  a supervised worker is live.
- Task specs must name: target, change, constraints, ownership, observable
  acceptance. The coordinator guide states this; pod does not parse specs.
- `gate-create` defaults to audience `coordinator`, which requires `--task`.
  For either audience, a gate with `--task` is refused while a supervised worker is live on the Task.
  Otherwise it completes the Task's active Dispatch and sets the Task `blocked`.
- `gate-resolve` records the resolution and returns the Task to `ready` when
  every dependency is `completed`, otherwise to `pending`; a gate never
  bypasses the dependency rule. It sends no message for a coordinator gate;
  user gates send the message in 9.1.
  With no Task, it only resolves the gate and sends that message. The next
  preamble for a linked Task includes the question and resolution.
- Gates do not time out. Gates are for coordinator-owned DAG decisions, not for
  answering a worker's `ask`.

### 9.1 Escalation routing

A blocked worker is never left to spin, and a decision reaches a person once,
with its evidence attached (after OpenRig's human-in-the-loop rule).

- **Ask a specialist.** `ask --to dispatch:<id>` sends the question to another
  live worker in the same Run instead of the coordinator. The Run mailbox
  receives a copy in the same thread, so the coordinator sees it and may answer
  first. The first `reply` wins; a later one is refused with
  `question_answered`. Both message IDs resolve to one `questions` row through
  a fresh shared thread for that question; `reply --id` accepts either ID. The row's `dispatch_id`
  is the asker's, so loss of the specialist does not close the question; the
  coordinator can still answer. Omitting `--to` retains the Run-mailbox default.
  The target must own an active Dispatch, be live in the same Run, and not be
  the asker (`recipient_not_live`, also for unknown or other-Run IDs).
  The preamble lists such eligible workers under
  `=== SPECIALISTS ===`.
- **Escalate with evidence.** An `escalation` must carry `--evidence <ref>` (a
  file path, a message ID, or a `worker-read` cursor) and
  `--recommendation <text>` (what the sender would do by default). Otherwise it
  is refused with `escalation_evidence_required`. pod stores both and
  validates neither.
- **User gate.** `gate-create --for user --question … --options … --evidence …
  --recommendation …` is for the coordinator's own blocker. `--task` is
  optional; with it the Task is blocked as for a coordinator gate. Evidence and
  recommendation and a nonempty options list are required, and the
  recommendation must be one of the options (all refused with
  `gate_evidence_required`). Every user gate is exported to the board as in
  section 12, with or without a Task, and while one is open the worker
  workspaces' sidebar status leads with `DECISION · <question>`. A new user
  gate fires a herdr notification and the optional `[notify] command`. It
  is also visible through `gate-list`. `check --peek` shows its resolution notification
  only while that message is unread. The coordinator waits with
  `check --wait --types decision_gate`. `gate-resolve` on a user gate also
  stores a `decision_gate` message in the Run mailbox, which wakes that wait.
  This is an engine-generated gate notification, not a worker lifecycle send;
  it requires no worker Dispatch identity.
- **Routing rule** (stated in the coordinator guide, not enforced by pod).
  When a worker looks stuck: read its pane or transcript and name the exact
  blocker. A permission or approval prompt is an unblock task for the
  coordinator, not a slow worker. Ambiguity goes to the worker that owns the
  knowledge, by replying with a pointer or by having the asker use `ask --to`.
  Only a genuine product decision goes to the user, through a user gate, once.
  A step that completes cleanly never creates a gate. A blocked worker is
  never reported as in progress.

## 10. Worker preamble

Injected as one prompt. `<POD>` is the absolute path of the binary.

```
Please carry out this task from my pod coordinator by following the brief below. A copy is stored at <brief_path>; read it again after a context reset or compaction.

You are working inside herdr as a dispatched worker.
Your coordinator's handle is: <coordinator_handle>
Your task ID is: <task_id>
Your dispatch ID is: <dispatch_id>
Your worker handle is: <worker_handle>

The coordinator cannot see this terminal. Reach it only with the commands below;
a question or result left only in this terminal never gets to it.
Do not post to Slack, GitHub, or other channels during the run.

=== COMMANDS ===
# Report the outcome. Required, exactly once. --body is three sentences: what you
# did, what you found, what is left. Use --outcome failed when the work is not
# complete; never encode failure only in prose and never silently exit. A failed
# outcome must say why: --reason no_follow_on|blocked_on|denied|canceled|escalated|superseded|handed_off,
# and blocked_on, handed_off, superseded also need --reason-target <task or dispatch id>.
<POD> send --from <worker_handle> --type worker_done --subject "<short status>" --body "<three sentences>" --task-id <task_id> --dispatch-id <dispatch_id> --outcome succeeded [--reason <reason> --reason-target <id>]

# Heartbeat every <N> minutes while working. Skip while blocked in `ask` or `check --wait`.
<POD> send --from <worker_handle> --type heartbeat --subject "alive" --task-id <task_id> --dispatch-id <dispatch_id> --phase "<investigating|implementing|reviewing|waiting>"

# Ask the coordinator and wait for the answer. Use this instead of any local
# question dialog. On timeout, resume with the returned message ID. To ask a
# worker listed under SPECIALISTS instead, add --to dispatch:<its dispatch id>;
# the coordinator is copied.
<POD> ask --from <worker_handle> --question "<question>" --options "<a,b>" --timeout-ms 600000 [--to dispatch:<id>]

# Answer a specialist question received through check. Either copy's ID works.
<POD> reply --id <message_id> --body "<answer>"

# Escalate a blocker before completion. Evidence is a file path, a message ID,
# or a worker-read cursor; recommendation is what you would do by default.
<POD> send --from <worker_handle> --type escalation --subject "Blocked: <reason>" --body "<details>" --evidence "<ref>" --recommendation "<default>" --task-id <task_id> --dispatch-id <dispatch_id>

# Read coordinator follow-ups. Nothing interrupts you: run this before starting a
# new file, after a test run, and once more immediately before worker_done.
<POD> check --terminal <worker_handle> --json

=== AFTER worker_done ===
Stop and return to an idle prompt. Do not start new work, poll, or keep calling
check. Do not exit. A direct instruction from the user is new work and must not
reuse these IDs. The coordinator may re-engage this terminal with a fresh brief.

[=== SPECIALISTS === <dispatch_id> <handle>: <task title> …]  (other live workers in this Run, when any)
[=== DECISION ===  Question: … / Resolution: …]        (when a gate was resolved)
[=== SUB-DISPATCH === …]                                 (only when depth < max)

=== TASK ===
<spec>
```

## 11. Transcript reading

`worker-read --source auto` returns the transcript when the session is proven,
otherwise bounded screen output with a typed `fallbackReason`.

Proof of session, in order:

1. herdr reports the pane's `agent_session_id` / `agent_session_path` (from the
   installed herdr agent integration).
2. pod set the session ID itself at launch (Claude: `--session-id <uuid>`).
3. Otherwise there is no proof and no transcript read. pod never selects a file
   by recency.

Readers in version 1:

| Agent | Source | Session proof |
|---|---|---|
| Claude Code | `~/.claude/projects/<cwd slug>/<id>.jsonl` | herdr integration, or pod-assigned `--session-id` |
| Codex | `~/.codex/sessions/…/rollout-*-<id>.jsonl` | herdr integration |
| Grok | session store under `~/.grok/` | herdr integration |
| Cursor Agent | session store under `~/.cursor/` | herdr integration |

Each reader implements one trait (`locate(session) -> source`, `read(source,
cursor, limit) -> rows`), so a kind is added without touching the command. The
Grok and Cursor on-disk formats are established by a discovery task at the start
of phase 4 (run a session, record where and how it is stored, capture sample
files for tests); see section 19. Grok and Cursor readers are best-effort in
version 1 and are tested only when sample files exist. Other kinds use screen
output. Rows are `{role, kind, text, tool, at}` with large tool output trimmed.

Paging: `--limit`, `--cursor`; the cursor is pinned to its source; a change of
source returns `source_changed`. Output includes `contentComplete`, `clipping`,
and `warnings`. Transcript files persist after release; the final screen text is
archived at release for every kind, whether or not a reader exists, because a
transcript can be missing or unproven.

## 12. Board export

After every committed change pod rewrites `run.json` (herdr-dagr contract v3,
`CONTRACT.md` of herdr-dagr 0.3.1; `dagr check --strict` must pass) through a
temporary file and rename.

| pod | dagr |
|---|---|
| Run | `dagr: 3`, `run {id, title, started_at, orchestrator {agent: <coordinator handle>}}` (pod records no pane id for the coordinator) |
| Task | `task {id, title, kind: impl, state, deps, project, owner, note}`; `note` is the type and subject of the newest message a worker of the Task sent |
| Task `--parent` | `projects[]` |
| Dispatch | `attempt {id, n, cause {type, ref}, actor, model, locator {pane, agent} (working only), state, started_at, ended_at, outcome {result, evidence, reason, receipt}, progress {note}, liveness {prompt_acknowledged, last_output_at, queued_input} (working only)}` |
| `--retry-of` | `cause.type: followup` or `sent_back` with `ref` |
| `worker_done` accepted | attempt `done` or `failed`, `outcome.result` equal to the state, `evidence: reported` |
| ending recorded without a report (fence, gate, circuit breaker) | same, `evidence: heuristic` |
| Task `completed` or `failed` with no matching attempt (`task-update`, a gate) | an appended coordinator attempt `<task>·aN`, `evidence: asserted` |
| `exited` without a recorded ending | attempt `lost`; its Task is `blocked`, `unblock: coordinator`; recorded endings retain their outcome |
| pending `ask` | task `blocked`, `unblock: coordinator` (or `unblock: <specialist handle>` when routed) |
| user gate | task `kind: question`, `state: blocked`, `owner` and `unblock: user`, `criteria: <question>`, `note` with options, recommendation, evidence. A gate without `--task` is exported as its own task `gate:<gate id>` with no attempts and no project. |
| heartbeat `--phase` | `progress.note`; the newest of the last heartbeat, the last herdr observation, and the last message is `liveness.last_output_at` |
| pod events | `events[]`: `dispatch.dispatched` → `attempt_started`, a Dispatch ending → `attempt_settled`, everything else → `note`; `detail` is the pod event name and its detail JSON as one string |

Task state mapping: `pending`/`ready` → `queued` (`review` when its latest
attempt is `done`, `working` when one is still working); `dispatched` →
`working` (`blocked` on the coordinator when no attempt is working);
`completed` → `done`; `failed` → `failed`; `blocked` → `blocked`. dagr holds
every task state to its attempt record, so the export never contradicts it.

`pod board` opens `dagr view <file>` in a new tab with `DAGR_RUN` pointing at
the file, or prints the install command when dagr is absent. pod also writes a short status
(`T12 · implementing`) to each worker workspace's herdr sidebar metadata; while a
user gate is open it leads with `DECISION · <question, 40 characters> · `. The
board is optional; every fact is available through `worker-list`, `task-list`,
`inbox`, and `log`.
Untasked gates create no pod Task and cannot affect dependency promotion; their
board card is derived from the gate row, and `gate-list` exposes them
regardless of board availability.

### 12.3 Account usage

`pod usage` prints one line for herdr's tab bar with both accounts' rate-limit
windows, Claude's session cost, and Codex's credits. Claude's numbers arrive
through Claude Code's status-line hook (`pod usage --record`, which prints
nothing in the pane); Codex's are read from `codex app-server`
(`account/rateLimits/read`) and cached for a minute. Records live in
`<state dir>/usage/`. `pod usage --install` wires Claude's `settings.json` and
herdr's `config.toml` (`[ui] tab_bar_right` and a `prefix+u` binding for
`pod usage --toggle`, which collapses the entry to `«`; position untouched) and is
idempotent; `pod doctor` reports the wiring as `usage`.

### 12.4 Limits and cost

`[limits]` in config caps the fleet: `max_active_workers`, `max_active_per_kind`,
and `pause_above_percent`. `worker-start` and `dispatch` refuse with
`limit_reached` (exit 1, `nextSteps` naming the limit and the current count)
when a cap is reached, or when the latest `pod usage` record for the worker's
vendor shows a window at or above `pause_above_percent` and is younger than 10
minutes. Stale or missing usage never blocks a start. The check runs before a
Dispatch exists, so a refusal leaves nothing behind.

`pod usage --record` also stores each status-line payload by `session_id`. A
Claude worker's Dispatch matches it through `workers.session_id`, and
`worker-show`, `worker-list`, and the board task note show `costUsd` (`null`
when unknown, never `0`; the note says `reported`). Codex's `app-server` has
no per-thread usage query (only account usage and a notification to the
connected client, neither in dollars), so Codex workers show `null`.

`run-create --budget-usd <n>` sets a soft budget. `run-show` reports `spentUsd`
(the sum of the matched Dispatch costs, each session counted once) and
`budgetUsd`; `worker-start` refuses with `budget_exceeded` once spent is at or
above the budget, and a start receipt carries a warning above 80 percent. Cost
is estimated from the agents' own reports and may differ from the bill.

## 13. Failure handling

Rule: act on proof, never on absence.

| Situation | Behaviour |
|---|---|
| Agent exits without a recorded ending | Observation `exited`; Dispatch `outcome_unknown` in the projection; next action: inspect, then `worker-stop` or `worker-abandon`. |
| Agent stuck at an approval or question prompt | Attention `approval`/`input` (herdr's bare `blocked` status counts as `input`). pod never answers the prompt. A `check --wait` in progress returns with `wokeOn: attention`. |
| Worker blocked on a missing fact or ambiguity | Worker asks the specialist (`ask --to`) or escalates with evidence; the coordinator routes per 9.1. Never left to spin. |
| Coordinator cannot decide | `gate-create --for user`; inspect with `gate-list` and wait on the resolution message `decision_gate`; only a Task-linked gate has a board card. |
| Worker silent | `stale` warning after 10 minutes. Never auto-failed. |
| Agent present but activity unknown | `liveness: live`, `activity: unknown`; keep waiting. Only `presence: exited` or `absent` is proof of exit. |
| Wake ladder exhausted | Attention `unreachable`; the message stays stored and unread; next action `worker-show`, then `worker-read`. pod never retypes the message body. |
| Pasted text not confirmed on screen | Attempt recorded `rendered_unconfirmed`, Enter withheld; later nudge attempts skip until the input is proven cleared. |
| Process presence cannot be established | `presence: null`, `unverifiable`. No destructive action; inspect or wait. |
| `worker-start` fails partway | Exit 1, receipt with `failedStage` and `residualResources`; worker `reclaimable` if a pane was created (7.1). |
| Prompt typed but turn not observed | `dispatched_unobserved`. Never re-sent. |
| Reply to a mutation lost | `request-show` → `completed` (read the receipt), `pending` (replay with `--retry-request`), `absent` (inspect before retrying). |
| Coordinator crash or context reset | New session runs `run-use`; unacknowledged mail replays. |
| herdr restart or update | Startup hook reconciles; missing panes become `exited`, never `failed`. |
| Concurrent writers | One transaction each, busy timeout and bounded retry. |
| Store not writable (sandbox) | `store_access_denied` with the exact fix for that agent kind. |
| User takes over or closes a worker pane | Pane becomes user-owned; release leaves it; a closed pane is `exited`. |
| pod updated during a run | Additive migrations only; running workers keep working. |

`pod reset --scope run:<id>` deletes one Run's state. It is refused while that
Run has live workers unless `--force`. `--scope all` requires `--force`.

## 14. Deliberate differences from Orca

| Topic | Orca | pod | Why |
|---|---|---|---|
| Runtime | Electron app is the daemon | No daemon; herdr hooks plus the waiting coordinator | herdr already owns the long-lived process |
| Worker mailbox | Replay-until-ack, but workers are never told to ack | Worker `check` marks read on return | Avoids a batch that replays forever and blocks nudges |
| Failed dependency | Dependants silently stay `pending` | Same status, plus an attention category and next action | A run must not hang with no signal |
| Liveness | Two layers (agent-status hooks and PTY) | Three axes (`activity`, `presence`, `needs_input`) from herdr's agent state and worker reports; `liveness` is derived | herdr's detection is authoritative; separate axes stop a live pane from hiding a dead agent (OpenRig) |
| Worker exit report | `--outcome` only | Failed reports require a reason; success defaults to `no_follow_on`; only `handed_off`, `blocked_on`, `superseded` require a target | A failure without a why is a dead end for the coordinator (OpenRig) |
| Typing into a pane | Type, then Enter | herdr's `agent prompt` first; paste, confirm rendered, then Enter as the fallback; outcome recorded | A blind Enter can submit the wrong thing (OpenRig) |
| Undelivered mail | One nudge, then re-offer | Bounded store-derived wake policy (5 attempts), then `unreachable` | A run must not stall silently; restarts keep the count (OpenRig) |
| Stuck worker | Asks the coordinator; escalation is free text | Ask routed to a specialist; escalation needs evidence and a recommendation; user gate with evidence and a recommended default; board card only when Task-linked | Context is distilled at each hop, so what reaches a person is a decision, not narration (OpenRig) |
| Clock | The app is always on, so it can act on its own schedule | pod ticks inside the waiting coordinator (`check --wait`, `ask`) and the tab bar entry (`pod usage --tick`); with neither running, nothing ticks | No daemon; the clock rides on processes that already exist (2026-10-14) |
| Dispatch capability | Minted and printed until v1.4.219; reports are accepted without it from v1.4.219 | `--dispatch-capability` is accepted and ignored (3.4) | Nothing to mint; the Dispatch ID and the terminal identity fence a stale report |
| Blocked detection | Codex's plan menu and open agent questions count as blocked from v1.4.221 (still so at v1.4.224); Orca reads the screen for several agents | herdr's bare `blocked` status is `needs_input` with reason `blocked`; pod reads no screen for it | pod trusts herdr's detection; the same screens count only when herdr reports them |
| Waiting | `check --wait` wakes on messages only | Also returns when a worker newly requires action (`wokeOn: attention`, with the workers listed) | A prompt or an exit must not wait out a ten-minute timeout |
| Identity | Pane key, process incarnation, launch token | Terminal ID plus agent-process ancestry | No environment injection needed at launch |
| Presets | Executable aliases only | Aliases plus named presets (kind, model, effort, fast, yolo, args) | Needed for the terra/sol workflow |
| Cost | None per worker | `costUsd` on worker rows from the agent's own report (`null` when none), fleet caps, a soft Run budget | Limits need a number; it may differ from the bill |
| Transcripts | All supported agents | Claude, Codex, Grok, Cursor | Others added per reader |
| Remote, legacy migration | Present | Absent | Out of scope for version 1 |

## 15. Security

- Store and board files are owner-only (`0600`, directories `0700`).
- pod executes only: herdr, configured agent executables, and configured repo
  setup argv. It never executes text from messages, specs, or transcripts.
- Message bodies, specs, and transcript rows are data. The coordinator guide
  says so explicitly.
- The preamble forbids posting to external channels during a run.
- The plugin's install preview lists every hook command, per herdr's plugin rules.

## 16. Performance targets

| Operation | Target |
|---|---|
| Any non-waiting command, excluding herdr round-trips | < 50 ms |
| Wake of a waiting `check`/`ask` after a matching message commits | < 100 ms |
| `worker-list` for a 100-worker Run | < 100 ms |
| Board export for 500 tasks | < 30 ms |

Measured in the test suite on macOS arm64 and Linux x86-64.

## 17. Testing

1. **Engine tests** with a fake `Herdr` and a fake clock: task state machine,
   promotion, FIFO/ack/replay, single check waiter alongside independent ask waits, ask/resume/close, gates,
   cumulative breaker including failed reports and denied/canceled exceptions,
   dependency-aware gate resolution, retry rules, identity checks, receipts, group resolution,
   closure-reason enforcement and unknown-target warnings, the two evidence sources,
   nullable presence and recorded-outcome precedence, bounded nudge counts after restart,
   separate preamble input recording, skipped input after an unconfirmed paste,
   ask routing and first-reply precedence, escalation and user-gate field enforcement,
   and the board card of untasked gates. Each rule in sections 7–9 has
   at least one test.
2. **Contract tests**: for each command, a recorded sample of Orca's JSON output
   (captured while Orca is still installed) is compared to pod's for field names
   and types. Differences listed in section 14 are allowed explicitly.
3. **Transcript reader tests** against real sample Claude and Codex files,
   including a refusal test when the session is unproven.
4. **End-to-end tests** against a real herdr in an isolated named session, using
   a scripted fake agent that reads the preamble, asks a question, heartbeats,
   and reports done, with the paste-confirm-Enter path exercised on a real
   pane. No model usage. Runs in CI on macOS and Linux.
5. **Board tests**: every exported file passes `dagr check --strict`.
6. **Live pilot**: one small terra → sol task in a real repository, coordinated
   from a Claude session, before anyone else installs it.

## 18. Packaging and rollout

- Repository: `github.com/mrrobot-ec/pod`, public, MIT, with the GitHub topic
  `herdr-plugin` so herdr's marketplace lists it. A checkout is used through
  `herdr plugin link <folder>`.
- Releases: CI builds macOS (arm64, x86-64) and Linux (x86-64, arm64) binaries
  with checksums on every `v*` tag; users install with
  `herdr plugin install mrrobot-ec/pod` and `npx skills add mrrobot-ec/pod`.
- Skills are stubs; the real guides come from `pod skills get`.
- `pod doctor` checks: herdr reachable and ≥ 0.9.3; store writable and passing
  an integrity check; `pod` on `PATH`; herdr agent integrations installed for
  each configured agent kind; each agent's sandbox can write the state
  directory; herdr-dagr present (optional).
- Licence: MIT, with a NOTICE crediting Orca (MIT) for the orchestration
  contract and preamble wording, and OpenRig (Apache-2.0) for the
  closure-reason, safe-input, wake-ladder, activity-axis, and escalation-routing ideas.

### Build phases

| Phase | Deliverable |
|---|---|
| 0 | Crate skeleton, store with migrations, path and identity resolution, `doctor`, `status` |
| 1 | Engine without herdr: runs, tasks, messages, deliveries, ask/reply, gates, receipts, `reset`, `log` |
| 2 | herdr adapter; `worker-start` (placement, setup, launch, inject); show/list/stop/abandon/retain/release; `dispatch` |
| 3 | Waiting and wake, safe input and the wake ladder, plugin hooks, startup reconciliation, activity axes, liveness and attention projection |
| 4 | Transcript readers and `worker-read` |
| 5 | Board export, sidebar metadata, `board` |
| 6 | Plugin manifest, install script, skills and guides, end-to-end suite, contract tests, live pilot |
| 7 | Release pipeline (tagged releases) |

### Leaving Orca

1. Build through phase 6 with Orca still installed; capture contract samples.
2. Run one real multi-task job on pod.
3. Switch the coordinator habit to `pod`; finish any in-progress Orca runs in Orca.
4. Uninstall Orca.

## 19. Items to confirm during implementation

Each has a defined fallback, so none blocks the design.

| Item | Fallback if it does not hold |
|---|---|
| herdr returns `agent_session_id`/`agent_session_path` for a pane once the integration is installed | Claude: pod-assigned session ID. Codex: screen output with `fallbackReason: session_unproven`. |
| A `pod` process started by an agent's shell tool can walk its parent chain to the agent's PID, including under the Codex sandbox | Identity check uses terminal ID alone. |
| `herdr agent prompt` reliably delivers a multi-kilobyte preamble | It is the primary path, and every brief is a file in the state directory with a one-line pointer prompted to it; hand paste is the fallback when herdr cannot prompt. |
| Grok and Cursor store sessions in a readable, documented-enough format, and herdr's integration reports their session IDs | That kind ships with screen output and `fallbackReason: reader_unavailable`; the other readers are unaffected. |
| Claude Code's command-line flag for effort (`--effort`, confirmed on the installed Claude Code) | Presets pass effort through `args`. |
| Codex's fast mode is the `service_tier` config key with value `fast` (confirmed on Codex 0.160.1); Claude Code has no command-line flag for fast mode | `--fast` is refused for `claude`; a person toggles it inside the session. |
| Codex trusts a folder through `[projects."<path>"] trust_level = "trusted"` in its config file, keyed on the repository root for a linked worktree (confirmed on Codex 0.160.1, where a new worktree otherwise shows "Trust and continue"; a `-c` override of the same key does not clear the prompt) | The start fails at stage `inject` with `agent is not idle`; the person trusts the folder in the pane and retries. |
| Codex 0.160.1 draws a long paste in a scrolling, soft-wrapping composer (the marker line shows a later, wrapped line of the paste) rather than a collapsed summary; pod confirms the paste by comparing whitespace-free text from the marker down | Enter is withheld (`rendered_unconfirmed`); the person presses Enter. |
| herdr 0.9.3 reports an approval prompt as agent status `blocked` with no state label | pod records `needs_input` reason `blocked` from the status alone. |
| herdr workspace metadata accepts a status token from a plugin | Skip sidebar status; the board and `worker-list` remain. |
| herdr can paste text into a pane without submitting it and return the pane's current screen text | Withhold automatic input; record `skipped` if nothing was pasted or `rendered_unconfirmed` if confirmation failed after paste, always withholding Enter. Fail an unperformed initial injection, or report `start_unknown` when its effects are uncertain, with recovery instructions. Stored mail remains available through `check`. |

`BASE DRIFT` was dropped on 2026-10-14: counting commits needs git, which pod does not run.
