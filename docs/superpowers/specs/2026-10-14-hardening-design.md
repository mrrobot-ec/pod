# pod 0.2 hardening: reliable on one machine, limits and reach

Status: draft for review, 2026-10-14. Companion to the design spec
`docs/specs/2026-10-05-pod-design.md`, which stays the source of truth; every
rule here names the spec section it changes. Source of the gaps: the five
research reports under `docs/reviews/2026-10-14-research/`.

## 1. Goal and scope

Close every gap the research found that can be closed on one machine, in two
phases, so that a hundred-seat swarm later multiplies a reliable core and not
today's bugs.

- **Phase A, reliable on one machine:** injection through herdr's own prompt
  path, a clock for the wake ladder without a daemon, briefs that survive
  compaction, the two attention categories never produced, decisions visible
  away from the terminal, and a live suite that proves it with real agents.
- **Phase B, limits and reach:** concurrency caps, usage-window pauses,
  per-worker cost and a soft budget, launch flags for more agent kinds, spec
  hygiene, and the README positioning.

Out of scope, each with its own later spec or decision: remote placement
(the swarm's "other machines" sub-project; each machine runs its own herdr
server and a remote worker has no path to the coordinator's store), scheduled
jobs, `start_unknown` (no producing case), the marketplace listing (needs a
repository), and the `BASE DRIFT` preamble section, which is dropped from the
spec because pod may not run git.

## 2. Phase A

### A1. Injection through `herdr agent prompt` (spec 7.1 step 6, 8.2, 19)

Today the engine pastes with `pane.send_text`, reads the screen, and presses
Enter, and never calls the adapter's `agent_prompt`. herdr's `agent.prompt`
types text and Enter as one ordered submission, honours bracketed paste even
while the agent is working, rejects a `blocked` agent before typing, and with
`wait` reports `agent_prompt_stalled` when no `working` or `blocked` state
follows within five seconds. That is the path the spec names, and it removes
the composer-rendering fixtures from the critical path.

Rules:

1. **Every worker's brief is a file.** `inject` writes the full preamble to
   `<state dir>/runs/<run>/preambles/<dispatch>.txt` (mode 0600, as today for
   oversized briefs) for every agent kind, and the text typed is the one-line
   pointer: `The full worker brief is stored at <path>. Read that file, then
   carry out the task.` The `preamble_file` effect is recorded as today.
2. **The pointer and every nudge go through `agent_prompt`** with
   `wait: {timeout_ms: 5000, until: [working, blocked]}`. `Submitted` records
   `delivered`. `Blocked` records `skipped` with reason `agent_blocked` and the
   worker gets attention `approval` or `input` from its herdr labels, as today.
   `Stalled` means herdr already pressed Enter, so it is `rendered_unconfirmed`
   (a start is `dispatched_unobserved`) and is never pasted again (recorded in
   lane A1's plan). A transport error (herdr unreachable) fails the input with
   no paste, since the text may already have gone in.
3. **Safe paste is the fallback, not the default.** When `agent_prompt` is
   unavailable (a herdr error other than the two above, or a kind herdr cannot
   prompt), pod runs the 8.2 procedure unchanged: readiness, settle
   window (`POD_INPUT_SETTLE_MS`), paste, confirm, Enter. The Claude tail rule
   and the Codex scrolled rule stay for this path only.
4. **One screen read, no readiness wait, before a prompt.** herdr orders the
   submission itself, so pod no longer waits for a bare prompt before
   `agent_prompt`. It reads the pane once to detect a person typing (A4) and
   skips the prompt in that case; it refuses a `blocked` agent through the
   call's own error. `READY_WAIT_MS` and the settle window apply to the
   fallback only.
5. **Receipts.** `launch.inject` (new field on the start receipt) is
   `prompt` or `paste`, so a person can see which path ran. `preamble_input`
   values are unchanged.

### A2. A clock without a daemon (spec 3.2, 8.2)

The ladder's rules do not change: at most five attempts per mailbox, each at
least 60 seconds after the previous, store-derived, cleared by a consuming
`check`. What changes is the set of eligible triggers.

1. **The waiting coordinator ticks.** While `check --wait` or `ask` blocks,
   every 60 seconds it runs one ladder pass for the Run's mailboxes (the same
   code the idle hook runs, trigger `WaitTick`, allowed to spend the quiet
   check since nobody waits on the tick itself) and one stale pass.
2. **The tab bar ticks when nobody waits.** `pod usage --tick` is the tab
   bar command (`--install` writes it in place of `pod usage`). Besides
   printing the bar line it runs the same ladder and stale pass for every Run
   with an active Dispatch in this session, at most once every 30 seconds
   (a `tick.at` stamp file in the state directory decides; the other
   invocations return in the usual few milliseconds; two bar processes that
   race past it are kept apart by the per-mailbox lock and the 60-second
   rule). A tick never runs `reconcile`, never sleeps, and asks herdr for no
   turn wait on its nudge, so it fits the bar command's 5-second timeout.
5. **A tick is not evidence.** A tick that finds the recipient present and
   `working`, or another process's wake for the mailbox in flight, records
   no attempt; only a tick against an idle (or absent, or stale) recipient
   does (8.2). Five ticks during one long turn exhausted a healthy worker's
   ladder in the first live run.
3. **`stale` wakes the waiter.** A worker with no activity, heartbeat, or
   message for 10 minutes carries attention `stale` (`requiresAction: false`,
   as today, so A5.3 does not announce it) and, because it is a new attention
   category, `check --wait` returns with `wokeOn: attention` so the
   coordinator looks instead of waiting out its timeout.
4. **Spec wording.** 8.2's "There is no timer" becomes: "There is no timer
   outside a bounded wait and the tab bar tick; exhaustion is still a count."
   3.2 lists the tick beside the hooks.

### A3. Briefs that survive compaction (spec 10, guides)

1. Every nudge names the brief: `You have N orchestration message(s). Run
   `pod check --terminal <handle>`. Your brief: <path>.`
2. The worker guide gains a rule: after a context reset or compaction, read
   the brief file named in the last nudge or in `pod worker-show`, which now
   prints `briefPath`.
3. The preamble's first paragraph says the brief is also stored at that path.

### A4. `interruption` and `guidance` (spec 7.4, 7.6)

1. **`interruption`:** the screen read before a prompt or paste found text on
   the input line that pod did not type (not the placeholder, not pod's own
   pending paste). pod records
   observation `user_typing` with the first 80 characters, the worker carries
   attention `interruption` (`requiresAction: false`, `nextAction`
   `worker-read`), and no nudge or paste touches the pane until a later
   readiness check finds it bare, at which point the category clears. A pane
   is not marked user-owned by this alone; closing or retaining still is.
2. **`guidance`:** the worker has a `question` or `escalation` in the Run
   mailbox that the coordinator has not answered (no `reply`, no `send` to
   that Dispatch after it) for 5 minutes. The worker carries `guidance`
   (`requiresAction: true`, `nextAction` `reply --id <message>` or
   `worker-read`). It clears on the reply.

### A5. Decisions visible away from the terminal (spec 9.1, 12)

1. **Untasked user gates export to the board** as blocked `question` tasks
   (`gate:<id>`, `unblock: user`) with no attempts, as a tasked gate with no
   worker has none; the two deferrals in the implementation notes close.
2. **`DECISION` sidebar status:** while a user gate is open, every worker
   workspace of the Run shows `DECISION · <question, 40 chars>` ahead of its
   usual status, written through the existing sidebar path and cleared on
   resolution. pod writes only worker workspaces' sidebars; with none, the
   notification (rule 3) carries the decision (lane A5's plan).
3. **Notifications:** on every new user gate and on every worker that newly
   carries `requiresAction: true`, pod calls herdr `notification.show` with
   the Run objective as title, the question or next action as body, and
   `sound: request`. One notification per event, never repeated by ticks.
4. **`[notify] command`** in `config.toml`: an argv pod runs with the same
   title and body as `NOTIFY_TITLE` and `NOTIFY_BODY`, for push plugins. A
   failure is a warning in the receipt, never a failed mutation.

### A6. Live suite (spec 17)

`scripts/live.sh` starts an isolated herdr server (`herdr --session pod-live
server`), links the plugin, and runs `cargo test --test live -- --ignored`
with `POD_LIVE=1`. The state directory is derived from the socket path, never
overridden, which is what broke the worktree leg last time. Cases, each
against real `claude` and real `codex` with `--yolo`:

1. start in the current folder, brief delivered through `agent_prompt`,
   `worker_done` accepted, release closes the tab;
2. start in a new child worktree (trust written, including Claude's
   `~/.claude.json` entry), `worker_done` from the worktree;
3. reuse the same pane for a second Task, tab relabelled;
4. a nudge on a worker that is mid-turn is delivered or recorded `skipped`
   with a reason, never lost;
5. `interruption` after a scripted keystroke into the pane;
6. a user gate shows on the board and fires a notification;
7. the clock: a message sent while the recipient is busy is nudged by the
   waiting coordinator's tick without any hook.

The suite records each case's receipts under `docs/reviews/live/<date>.md`.
Linux runs the unit and engine suites in Docker when a daemon is available;
the live suite is macOS only.

## 3. Phase B

### B1. Limits, cost, and budget (spec 7.1, 12, new 12.4)

```toml
[limits]
max_active_workers = 20          # default: no cap
max_active_per_kind = { codex = 15, claude = 10 }
pause_above_percent = 95         # any usage window at or above this pauses starts
```

1. `worker-start` and `dispatch` refuse with `limit_reached` (exit 1,
   `nextSteps` naming the limit and the current count) when a cap is hit, or
   when the latest `pod usage` record for the worker's vendor shows a window
   at or above `pause_above_percent` and is younger than 10 minutes. Stale or
   missing usage never blocks a start.
2. **Per-worker cost.** `pod usage --record` stores every status-line payload
   by `session_id`; a Claude worker's Dispatch is matched by
   `workers.session_id`, and `worker-show`, `worker-list`, and the board
   (`evidence: reported`) show `costUsd`. Codex per-thread cost is recorded
   only if `codex app-server` exposes a thread usage query; the
   implementation checks and documents the answer.
3. **Soft budget.** `run-create --budget-usd <n>`; `run-show` reports
   `spentUsd` (sum of matched Dispatch costs) and `budgetUsd`; `worker-start`
   refuses with `budget_exceeded` once spent is at or above the budget, and a
   warning is added to receipts above 80 percent. Cost is estimated from the
   agents' own reports and may differ from the bill.

### B2. Launch flags for more agent kinds (spec 7.1, config)

```toml
[agents.grok]
launch = { model = ["-m", "{model}"], effort = ["--reasoning-effort", "{effort}"], yolo = ["--permission-mode", "bypassPermissions"] }
```

Built-in tables for `codex` and `claude` move to the same shape; they are the
only built-in tables. Every other kind gets flags from a config `launch`
table, of which `[agents.grok]` above is the documented example; without one
a flag stays `launch_mapping_unsupported` with the config snippet in
`nextSteps`. A config table's keys merge over the built-in (a key left out
keeps it), unknown keys are refused at load, and a `model` or `effort`
fragment must carry its placeholder. `pod doctor` lists the kinds whose table
maps a flag.

### B3. Spec hygiene and positioning

1. Spec section 14 drops the "automatic coordinator loop" row and the
   dispatch-capability note, and states Orca's `blocked` rules as of
   v1.4.224. Section 10 drops `BASE DRIFT`.
2. Graph validation: a dependency must exist and belong to the Run (already
   refused); cycles cannot form because dependencies are fixed at creation;
   both are stated in commands.md. A dependency that is already `failed`
   creates the Task `pending` with `waiting_on_failed_dependency`, as today.
3. README: "Why pod exists" rewritten from the research (cross-vendor
   supervision with proof-based completion, no daemon), plus "When to use
   native subagents instead" and a short comparison list naming Orca, Claude
   agent teams, ClawTeam, Cyclops, OpenRig, herdr-projects.

## 4. Errors, receipts, and docs

New error codes: `limit_reached`, `budget_exceeded`. New receipt fields:
`launch.inject`, `briefPath`, `costUsd`, `spentUsd`, `budgetUsd`. New
attention categories produced: `interruption`, `guidance`, `stale` wakes.
Every change lands in `docs/reference/*.md`, the guides, `errors.md`,
`state-machines.md` where a transition changes, and `CHANGELOG.md` as 0.2.0.
Docs stay at or under 400 lines each except the design spec.

## 5. Testing

- Engine tests on `FakeHerdr` pin every rule above, including the prompt
  path, the fallback, the tick (a fake clock advances 60 seconds), the two
  categories, gate export, notifications (recorded calls), limits, cost
  matching, and the budget.
- `tests/live.rs` as in A6, ignored unless `POD_LIVE=1`.
- The existing suites, clippy with warnings denied, fmt, docs caps, and the
  contract tests stay green at every merge.

## 6. Work breakdown

Each lane is one Task for one worker in its own child worktree, with file
ownership so lanes do not collide. Phase A lanes run in parallel, then one
Opus review of the merged result; the same for Phase B.

| Lane | Owns | Depends on |
|---|---|---|
| A1 prompt injection | `src/engine/safe_input.rs`, `src/engine/lifecycle.rs` (inject), `src/herdr/{mod,socket,fake}.rs` (prompt wait), docs 7.1/8.2 | none |
| A2 clock | `src/engine/waiting.rs`, `src/engine/wake.rs`, `src/cli/commands/env.rs` (tick), `src/usage/`, spec 3.2/8.2 | none |
| A3 briefs | `src/engine/preamble.rs`, nudge text in `wake.rs` (one function), `docs/guides/worker.md`, `worker-show` | A1 (file for every kind) |
| A4 attention | `src/engine/observe.rs`, `src/engine/wake.rs` (readiness observation), docs 7.6 | A1 |
| A5 decisions | `src/board/`, `src/engine/gates.rs`, `src/engine/mod.rs` (after_commit), `src/config.rs` (notify), `src/herdr/*` (notification) | none |
| A6 live suite | `scripts/live.sh`, `tests/live.rs`, `docs/reviews/live/` | A1 to A5 merged |
| B1 limits | `src/config.rs`, `src/engine/lifecycle.rs` (preflight), `src/usage/`, `src/engine/runs.rs`, board cost | A2 (usage tick) |
| B2 agents | `src/config.rs` (launch mapping), `src/doctor.rs` | none |
| B3 docs | spec sections 10/14, README, commands.md | A and B merged |

Merging is the coordinator's job; pod never runs git.
