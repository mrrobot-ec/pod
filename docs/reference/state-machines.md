# State machines

Each table lists every transition as `from / event / to`. IDs are stable so tests can cite them (`T4`, `D2`, ...). `(none)` means the row does not exist yet. Where the spec does not state a transition outright, pod takes the most conservative reading, and the row says why. Spec section references are to `docs/specs/2026-10-05-pod-design.md`.

Invariants (spec section 5): one active Dispatch per Task; one active Dispatch per terminal; one outstanding Delivery per mailbox.

## Tasks: `tasks.status`

| ID | From | Event | To |
|---|---|---|---|
| T1 | (none) | `task-create`, every dependency already `completed` (or none) | `ready` |
| T2 | (none) | `task-create`, at least one dependency not `completed` | `pending` |
| T3 | `pending` | a dependency completes and now all dependencies are `completed` | `ready` |
| T4 | `ready` | `worker-start` or `dispatch` reaches the Record stage; Dispatch becomes `dispatched` | `dispatched` |
| T5 | `failed` | `worker-start --retry-of` with a valid Dispatch reaches Record | `dispatched` |
| T6 | `blocked` | `worker-start --retry-of` with a valid Dispatch reaches Record | `dispatched` |
| T7 | `dispatched` | accepted `worker_done --outcome succeeded` | `completed` |
| T8 | `dispatched` | accepted `worker_done --outcome failed` with `--reason denied` or `canceled` (a retry is pointless) | `failed` |
| T9 | `dispatched` | A counted Dispatch failure leaves `failure_count` below 3, including an accepted `worker_done --outcome failed` with any other reason (`worker-stop` and `worker-abandon` count as failures, see D8) | `ready` |
| T10 | `dispatched`, `ready` | A counted Dispatch failure (excluding `worker_done` with `denied` or `canceled`) takes `failure_count` to 3 (Dispatch becomes `circuit_broken`); from `ready` when a start or `dispatch --inject` fails before Record | `failed` |
| T11 | `ready`, `failed`, `blocked` | `worker-start` ends `start_unknown` (from `failed` or `blocked` on a `--retry-of` start) | `blocked` |
| T12 | `pending`, `ready`, `dispatched` | `gate-create` with no supervised worker live on the Task (completes the active Dispatch, if any: D6) | `blocked` |
| T13 | `blocked` | `gate-resolve`, every dependency `completed` | `ready` |
| T14 | any status but `dispatched` | `task-update --status <s>` for `pending`, `ready`, or `blocked`, with no active Dispatch | `<s>` |
| T15 | any status but `dispatched` | `task-update --status dispatched` while an active Dispatch exists | `dispatched` |
| T16 | any | `task-update --status <s>` (not `dispatched`) while a Dispatch is active, or `dispatched` with no active Dispatch | refused; no transition |
| T17 | any status but `dispatched` | `task-update --status completed` or `failed` with no active Dispatch and no supervised worker live | `completed` or `failed` (`completed` fires T3 for each dependant) |
| T18 | any | `task-update --status completed` or `failed` while a supervised worker is live (for example after `worker_done`, before release) | refused; no transition |
| T19 | `blocked` | `gate-resolve`, at least one dependency not `completed` (a gate never bypasses the dependency rule) | `pending` |

Notes:

- When a dependency becomes `failed`, dependants stay `pending` (no transition). They are reported under `waiting_on_failed_dependency`.
- A `worker-start` that fails before Record leaves the Task in its current status (no transition), except that `start_unknown` sets it `blocked` (T11) and a third cumulative failure sets it `failed` (T10). A start that never reached `dispatched` still counts as a failed Dispatch toward `failure_count`.
- T7 also fires the promotions of T3 for each dependant.
- T8 to T10 follow spec section 7.5: a `worker_done --outcome failed` counts toward `failure_count` like any failed Dispatch, so below 3 the Task returns to `ready` (T9) and at 3 the Dispatch is `circuit_broken` and the Task `failed` (T10). The exception is `--reason denied` or `canceled`, which fail the Task directly (T8), leave the Dispatch `failed`, and do not increment `failure_count`.
- T13 and T19 follow spec section 9: `gate-resolve` returns the Task to `ready` only when every dependency is `completed`, otherwise to `pending`, where T3 promotes it as usual. A gate never bypasses the dependency rule.
- A herdr restart causes no Task transition: missing panes become `exited`, never `failed` (spec section 13).

```text
 (none) ─T2─▶ pending ─T3─▶ ready ◀─(none) T1
                             │  ▲  ▲
                           T4│  │  │T13
                             ▼  │T9│
                         dispatched ─T12─▶ blocked ─T6─▶ dispatched
                          │    │    │
                        T7│  T8│    │T10
                          ▼    ▼    ▼
                  completed   failed ─T5─▶ dispatched
 ready, failed ─T11─▶ blocked        pending, ready ─T12─▶ blocked        ready ─T10─▶ failed
 (task-update rows T14, T15, T17 and gate row T19 are not drawn)
```

## Dispatches: `dispatches.status`

The last column is the `termination_reason` stored on the Dispatch when it ends (spec section 8.1). `termination_target` is stored beside it when the reason takes one. The reported outcome is unchanged by the reason; failed reports with `denied` or `canceled` bypass the breaker and fail the Task directly.

| ID | From | Event | To | `termination_reason` |
|---|---|---|---|---|
| D1 | (none) | `worker-start` or `dispatch` passes Preflight and creates the Dispatch (created `pending` at Preflight) | `pending` | none yet |
| D2 | `pending` | Record stage: agent ready and prompt injected; or `dispatch` records it (with `--inject`, after the prompt is typed) | `dispatched` | none yet |
| D3 | `pending` | start ends `failed` or `start_unknown` | `failed` (the spec names no Dispatch status for a failed start; `failed` keeps one active Dispatch per Task and lets `--retry-of` name it) | none; no worker-reported reason for a failed start |
| D4 | `pending` | `dispatch --inject` fails (counts toward the breaker) | `failed` | none, as D3 |
| D5 | `dispatched` | accepted `worker_done --outcome succeeded` | `completed` | the `--reason` given, default `no_follow_on`; `handed_off`, `blocked_on`, and `superseded` also store the target |
| D6 | `dispatched` | `gate-create` completes it | `completed` | none; a gate is the coordinator's act, not a worker report |
| D7 | `dispatched` | accepted `worker_done --outcome failed` with `denied` or `canceled` (T8, no count increment), or another reason while the incremented count is below 3 (T9) | `failed` | the explicit `--reason` (required); `blocked_on`, `handed_off`, `superseded` also store the target |
| D8 | `dispatched` | `worker-stop` or `worker-abandon` fences it, cumulative `failure_count` below 3 (fencing counts as a failure) | `failed` | `canceled` |
| D9 | `dispatched`, `pending` | a counted failure takes cumulative `failure_count` to 3; excludes reported `denied`/`canceled` | `circuit_broken` | keeps whatever the failing ending (D3, D4, D7, or D8) recorded |

A `worker_done --outcome failed` without a reason, or a targeted reason with no target, is refused before D5 or D7 (`closure_reason_required`, `closure_target_required`); the Dispatch stays `dispatched`. A target that pod cannot find in the Run is accepted with a `warnings` entry and the ending proceeds. Success without a reason defaults to `no_follow_on`; all seven reasons are valid with either outcome. Rejections are stored as high-priority `status` messages. Stop/abandon on an accepted ending do not replace its closure reason.

`failure_count` is cumulative per Task and carried across retries. `retry_of_dispatch_id` links a retry to the Dispatch it replaces. A repeated `worker_done` with the same outcome is `settled` (no transition); a conflicting one is `inactive_dispatch` (no transition).

```text
 (none) ─D1─▶ pending ─D2─▶ dispatched ─D5/D6─▶ completed
                │  │            │ │
              D3│  │D4        D7│ │D8
                ▼  ▼            ▼ ▼
               failed ◀─────────┘ │
                │                 │ D9 (failure_count = 3)
                └──────D9─────────┴──▶ circuit_broken
```

## Workers: `workers.state`

`workers` is keyed by Dispatch, so a reused pane gets a new row for the new Dispatch.

| ID | From | Event | To |
|---|---|---|---|
| W1 | (none) | `worker-start` leaves Preflight and begins Placement (the row is created when stages begin) | `starting` |
| W2 | `starting` | Record stage: herdr detected the agent, prompt injected | `ready` |
| W3 | `starting` | a stage fails with a known outcome | `failed` |
| W4 | `starting` | a stage ends with an unknown outcome | `start_unknown` |
| W5 | `ready` | accepted `worker_done --outcome succeeded` | `succeeded` |
| W6 | `ready` | accepted `worker_done --outcome failed` | `failed` |
| W7 | `ready`, `start_unknown`, `stop_unknown` | `worker-stop` begins | `stopping` |
| W8 | `stopping` | exact agent pane close confirmed | `stopped` |
| W9 | `stopping` | close could not be confirmed | `stop_unknown` |
| W10 | `stop_unknown` | observation proves the pane is gone | `stopped` |
| W11 | `ready`, `start_unknown` | `worker-abandon` | `abandoned` |

```text
 (none) ─W1─▶ starting ─W2─▶ ready ─W5─▶ succeeded
               │    │          │
             W3│    │W4        ├─W6──▶ failed
               ▼    ▼          ├─W11─▶ abandoned
           failed  start_unknown ─W11─▶ abandoned
                    │          │
                  W7│        W7│
                    ▼          ▼
                     stopping ─W8─▶ stopped
                      │ ▲             ▲
                    W9│ │W7           │ W10
                      ▼ │             │
                     stop_unknown ────┘
```

The projected attempt outcome (`active`, `succeeded`, `failed`, `stopped`, `abandoned`, `outcome_unknown`) is computed from worker and observation rows and is not stored. `outcome_unknown` means the agent is proven `exited` and the attempt has no recorded ending; recorded success, failure, stop, or abandonment takes precedence.

## Worker pane accounting: `workers.terminal_state`

| ID | From | Event | To |
|---|---|---|---|
| R1 | (none) | worker row created | `active` |
| R2 | `active` | accepted `worker_done`, or the agent is proven `exited` (spec section 7.4 makes these the points where the coordinator must reuse, retain, or release) | `reclaimable` |
| R3 | `active` | start fails after creating a pane | `reclaimable` |
| R4 | `active` | `worker-stop` confirms the pane closed | `reclaimable` (still needs release accounting) |
| R5 | `active` | `worker-abandon` (the coordinator must still account for the worker; release later leaves an active or unproven pane alone) | `reclaimable` |
| R6 | `reclaimable` | `worker-retain` (records `retained_reason`) | `retained` |
| R7 | `reclaimable`, `retained` | `worker-release` begins (release is allowed after retain) | `release_pending` |
| R8 | `release_pending` | archive written and tab closed, or pane left alone as non-owned | `released` |
| R9 | `release_pending` | close could not be confirmed | `release_unknown` |
| R10 | `release_unknown` | `worker-release` repeated | `release_pending` |
| R11 | `release_unknown` | observation proves the tab gone | `released` |
| R12 | `reclaimable`, `retained` | pane reused: `worker-start --terminal` for a new Dispatch; cleanup ownership moves to the new Dispatch (the old row becomes `released`) | `released` |

The coordinator's turn is not finished while `worker-list --terminal-state reclaimable` returns rows. When a user takes over or closes a pane, pod keeps the same states but release leaves the pane alone (R8).

```text
 (none) ─R1─▶ active ─R2/R3/R4/R5─▶ reclaimable ─R6─▶ retained
                                     │      │             │
                                  R12│      │R7         R7│
                                     ▼      ▼             │
                                released  release_pending ◀┘
                                            │ ▲     │
                                          R9│ │R10  └─R8─▶ released
                                            ▼ │
                                     release_unknown ─R11─▶ released
```

## Deliveries: `deliveries.status`

| ID | From | Event | To |
|---|---|---|---|
| L1 | (none) | coordinator `check` with no outstanding Delivery builds a batch of up to 50 messages | `outstanding` |
| L2 | `outstanding` | `check --ack <delivery_id>` | `acknowledged` |
| L3 | `outstanding` | `run-use` bumps `runs.consumer_generation` above the Delivery's | `fenced` |

An `outstanding` Delivery replays on every `check` until acknowledged; replay is not a transition. After L3, the same messages go out in a new Delivery (L1) to the new consumer. Worker `check --terminal` marks messages read on return and needs no acknowledgement (spec sections 8.3 and 14).

```text
 (none) ─L1─▶ outstanding ─L2─▶ acknowledged
                   │
                   └─L3─▶ fenced
```

## Questions: `questions.status`

| ID | From | Event | To |
|---|---|---|---|
| Q1 | (none) | `ask` stores a `question` message | `pending` |
| Q2 | `pending` | `reply --id <message_id>` | `answered` |
| Q3 | `pending` | the asking Dispatch fails or is fenced | `closed` |

An `ask` timeout leaves the question `pending`; `ask --resume <message_id>` continues it with no new row.

```text
 (none) ─Q1─▶ pending ─Q2─▶ answered
                 │
                 └─Q3─▶ closed
```

## Gates: `gates.status`

`gates.audience` is `coordinator` or `user`. Both audiences share the two states.

| ID | From | Event | To |
|---|---|---|---|
| G1 | (none) | `gate-create` for audience `coordinator` (also completes the Dispatch and blocks the Task: D6, T12) | `pending` |
| G2 | `pending` | `gate-resolve` on a coordinator gate (also returns the Task to `ready` or `pending`: T13, T19; sends no message) | `resolved` |
| G3 | (none) | `gate-create --for user` with `--evidence` and a `--recommendation` that is one of the options; with `--task`, also D6 and T12 as for G1, and the gate is exported to the board as a decision card | `pending` |
| G4 | `pending` | `gate-resolve` on a user gate: also stores a `decision_gate` message in the Run mailbox, which wakes `check --wait --types decision_gate`; with a Task, T13 or T19 | `resolved` |
| G5 | (none) | `gate-create --for user` without evidence, without a recommendation, or with a recommendation outside the options | refused with `gate_evidence_required`; no row |

Gates never time out. Audience `coordinator` requires `--task`. For a user gate without it, `task_id` is null and G3/G4 change no Task or Dispatch. Its board card and the sidebar decision status are deferred past version 1; the gate is visible through `gate-list`; `check --peek` shows G4's resolution notification only while unread. G4's notification is engine-generated, so worker lifecycle identity rules do not apply.

```text
 (none) ─G1/G3─▶ pending ─G2/G4─▶ resolved
 (none) ─G5─▶ refused
```

## Questions with a specialist

A routed question (`ask --to dispatch:<id>`) uses the Q table above with no new states. Q1 also stores a copy in the Run mailbox. Q2 fires on the first `reply` to either copy, resolved through their shared thread; every later `reply` is refused with `question_answered` and causes no transition. `ask --to` is refused with `recipient_not_live` before Q1 when the target does not own an active Dispatch with live presence in the same Run, or is the asker. The question belongs to the asking Dispatch; loss of the specialist does not close it and the coordinator may still answer. `ask` waits independently and does not occupy the Run's single `check --wait` slot.

## Observation and attention derivation

`observations` rows record three independent axes and the evidence behind them (spec section 7.6). They are never collapsed into one state.

| Axis | Values |
|---|---|
| `activity` | `working`, `idle_at_prompt`, `unknown` |
| `presence` | `present`, `exited`, `absent` (the pane is confirmed gone), or null when unproven |
| `needs_input` | `{count, reasons[]}` with reasons `approval`, `input`; a count, never a state |

`evidence_source` is `self_report` (heartbeat, `worker_done`, `ask`) or `herdr_state` (herdr's agent state and the event hooks), with no third source. `self_report` proves activity but never presence; `herdr_state` is authoritative for presence. pod does not read screen text to infer state in version 1, and there is no ranking between sources. Unproven activity is `unknown`; unproven presence is null. Neither is guessed around.

`liveness` and attention are derived from the axes:

| ID | Evidence | `liveness` | Attention |
|---|---|---|---|
| A1 | `presence: exited` or `absent` | `exited` | `failure`, `requiresAction: true`, only for an unsettled attempt |
| A2 | `presence: null` (process presence cannot be established) | `unverifiable` with `reason` | `unverifiable` |
| A3 | `presence: present`, `needs_input.count > 0` | `live` | `approval` or `input`, `requiresAction: true` |
| A4 | `presence: present`, no input needed (including unknown activity) | `live` | none |

Added on top of A1 to A4, from other facts:

| ID | Evidence | Effect |
|---|---|---|
| A5 | no heartbeat for 10 minutes | `stale` added, warning only, `liveness` unchanged |
| A6 | wake ladder exhausted (N4 below) | `unreachable` added with `requiresAction: true`, `liveness` unchanged |
| A7 | the screen read before a prompt or paste found text a person typed | `interruption` added with `requiresAction: false`, `nextAction` `worker-read`; cleared when a later read finds the input bare |
| A8 | a `question` or `escalation` from this Dispatch is unanswered for 5 minutes | `guidance` added with `requiresAction: true`, `nextAction` `reply --id <message>` (question) or `worker-read` (escalation); cleared on the reply or on a `send` to the Dispatch |

`unreachable` is cleared when the recipient makes a consuming `check` (N5). `unverifiable` never authorizes stop, abandon, retry, or release. The projection published in `worker-list` carries `liveness`, `activity`, `presence`, `needsInput`, `attention.categories`, `attention.requiresAction`, and `nextAction`. A1 to A4 separate presence from input attention. Unknown activity never erases proven presence; worker reports and screen text never establish process presence; herdr state does. The one screen read, before a prompt or paste, only looks for typed text (A7).

## Wake ladder: per mailbox

One bounded policy per mailbox, with no named steps. Its position is the number of nudge attempts recorded, computed from `wake_attempts` and `events` on every read and never held in memory, so a herdr restart or a new `pod` process neither loses nor resets it. There is no stored ladder state. Every nudge attempt counts, whatever its outcome (`delivered`, `rendered_unconfirmed`, `skipped`, `failed`). The preamble injection is not in `wake_attempts` (it is recorded once in `workers.preamble_input`) and never counts. New mail does not restart a pending policy. Each hook or mutating command invocation makes at most one attempt. Read-only commands neither advance it nor type; without another eligible trigger it stays put, regardless of elapsed time.

| ID | From | Event | To |
|---|---|---|---|
| N1 | no attempts | a message is committed to a mailbox; attempt 1 in the sender's process, subject to the 8.2 preconditions (recipient `present` and `idle_at_prompt` on evidence under 15 s old, pane quiet 3 s, proven from a screen snapshot because the sender cannot wait ([commands-messages](commands-messages.md#nudges-safe-input-and-the-wake-ladder)), no outstanding Delivery, no wake in flight; else recorded `skipped` with the reason) | 1 attempt |
| N2 | 1 to 4 attempts | an eligible trigger (an idle event hook for the recipient, or a mutating command for the Run) at least 60 s after the previous attempt: the next attempt | one more attempt |
| N3 | 1 to 4 attempts | a trigger fires less than 60 s after the previous attempt | no change |
| N4 | 5 attempts | no consuming `check` by the recipient (derived as soon as attempt 5 is recorded) | exhausted |
| N5 | 1 to 5 attempts, exhausted | consuming `check` by the recipient | no attempts |

Each attempt follows the safe-input procedure and is stored with an outcome. A `rendered_unconfirmed` outcome withholds Enter; later attempts skip until the input is proven cleared. No subsequent attempt appends, submits, or deletes uncertain text. At N4, attention `unreachable` is derived (A6). For a worker mailbox the coordinator's `nextAction` is `worker-show`; for the Run mailbox the status is written to the board for the user, as a `mailbox.unreachable` event in `run.json`'s `events`. pod never retypes the message body. A matching `check --wait` waiter that wakes on commit takes the place of N1 and nothing is typed.

```text
 no attempts ─N1─▶ 1 ─N2─▶ 2 ─N2─▶ 3 ─N2─▶ 4 ─N2─▶ 5 ─N4─▶ exhausted
 any count or exhausted ─N5 (consuming check)─▶ no attempts
```
