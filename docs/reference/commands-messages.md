# Command reference: messages

Part of the [command reference](commands.md); conventions are there. Receipt layouts beyond spec-named fields are explained on that page.

## `pod send`

Stores a message. Success means the message is durably stored, nothing more. An active worker defaults to its Run mailbox unless it supplies an allowed explicit recipient.

| Flag | Meaning |
|---|---|
| `--subject <s>` | Required. |
| `--to <recipient>` | `run:<id>`, `dispatch:<id>`, a handle, or a group (`@all`, `@idle`, `@<agent kind>`, `@worktree:<path>`). `task:` recipients are rejected. A sender bound to no Run must use `run:<id>` or `dispatch:<id>`. |
| `--body <text>` | Message body. |
| `--type <t>` | `worker_done`, `heartbeat`, `escalation`, `decision_gate`, `status`, `merge_ready`, `handoff`, `dispatch`. The `question` type is created by `ask`, not by `send`. |
| `--priority <p>` | `normal`, `high`, or `urgent`. Adds a `[HIGH]` or `[URGENT]` tag; never reorders or interrupts. |
| `--thread-id <id>` | Thread for the message. |
| `--payload <json>` | Raw payload. Mutually exclusive with the typed flags. |
| `--task-id <id>`, `--dispatch-id <id>` | Task and Dispatch the message concerns. |
| `--outcome <o>` | `succeeded` or `failed`. Valid only with `--type worker_done`. |
| `--reason <r>` | Closure reason: `no_follow_on`, `handed_off`, `blocked_on`, `denied`, `canceled`, `escalated`, or `superseded`. Valid only with `--type worker_done`. `--outcome succeeded` defaults to `no_follow_on`; `--outcome failed` requires an explicit reason (`closure_reason_required`). |
| `--reason-target <id>` | A Task or Dispatch ID, looked up in the same Run. Valid only with `--type worker_done`. Required with `handed_off`, `blocked_on`, and `superseded` (`closure_target_required`). A target pod cannot find in the Run is accepted, with a `warnings` entry in the receipt. |
| `--evidence <ref>` | A file path, a message ID, or a `worker-read` cursor. Required on `--type escalation` (`escalation_evidence_required`). pod stores it and validates nothing. |
| `--recommendation <text>` | What the sender would do by default. Required on `--type escalation` (`escalation_evidence_required`). pod stores it and validates nothing. |
| `--files-modified <csv>` | Real paths only. |
| `--report-path <path>` | Real path only. |
| `--phase <p>` | Heartbeat phase; becomes `last_phase`. |
| `--from <handle>` | Must match the resolved caller when both are known. |
| `--dispatch-capability <c>` | Accepted for Orca compatibility and ignored. |

Behaviour by type:

- `worker_done`: ends the Dispatch and updates the Task under section 7.5; promotes dependants on success; records the closure reason. Always goes to `run:<id>` regardless of `--to`. The reason and target are stored on the Dispatch as `termination_reason` and `termination_target`, enforced in the engine at the store boundary, and shown by `worker-show`, `task-list`, and `log`. A reason does not change the reported outcome; on failed reports, `denied` and `canceled` select direct Task failure instead of the breaker. It also tells the coordinator where the work went. `worker-stop` and `worker-abandon` record `canceled`. All seven reasons are valid with either outcome. Stop and abandon record `canceled` only while fencing an active Dispatch; they preserve accepted endings and do not report a worker outcome. A failed `worker_done` counts toward the circuit breaker like any failed Dispatch, except `--reason denied` or `canceled`, which fail the Task directly without incrementing `failure_count`; their Dispatch is `failed`, not `circuit_broken`.
- `heartbeat`: updates `last_heartbeat_at` and `last_phase` of the active Dispatch only. Always goes to `run:<id>`.
- `escalation`, `decision_gate`: must come from the Dispatch's owner; the dispatch ID is stamped into the payload. No automatic state change. An `escalation` stores its `--evidence` and `--recommendation` in the message's `evidence` and `recommendation` columns; both are available to the coordinator. They are stored in those columns, not in `payload`. `--evidence` and `--recommendation` are accepted on any `--type` and required only for `escalation`.
- `status`, `merge_ready`, `handoff`, `dispatch`: labels only.
- Lifecycle types cannot target groups. A group resolves to the live Dispatches of the sender's Run, excluding the coordinator, one row per recipient with a shared generated thread ID. Zero recipients is `invalid_recipient`, as are a lifecycle type sent to a group and a `task:` recipient.

After commit, pod may nudge the recipient in the sender's process. Durable storage does not depend on nudge success. See the next section.

### Nudges, safe input, and the wake ladder

The stored message is the obligation; a nudge only points at it, so a nudge can repeat without duplicating the message. After commit, in the sender's process: a matching `check --wait` waiter on the mailbox wakes and nothing is typed. Otherwise pod attempts a wake only when the recipient's `presence` is `present` and `activity` is `idle_at_prompt` on evidence no older than 15 seconds (an absent, unproven, or stale recipient is recorded `skipped`), the pane has been quiet for 3 seconds, the mailbox has no outstanding unacknowledged Delivery, and no wake for that mailbox is in flight. The text is ``You have N orchestration message(s). Run `pod check --terminal <handle>`. Your brief: <path>.`` for a worker, where `<handle>` is its worker handle and `<path>` its brief file (`briefPath` in `worker-show`), so a worker that lost its context finds the brief again; the brief sentence is left out when the file does not exist. A nudge to the coordinator (the Run mailbox) has neither a handle nor a brief: ``You have N orchestration message(s). Run `pod check`.``

**The quiet check never blocks a command.** `send` returns as soon as the message is committed, within spec 16's 50 ms, so the 3-second rule is proven from a recorded screen instead of a sleep. Whenever herdr reports a recipient idle (the `pane.agent_status_changed` hook, or an event a waiter sees), pod records a snapshot of the pane's last 20 lines and when they were first seen (`screen-<pane>.quiet` in the state directory, mode 0600). A wake attempt reads the pane once: unchanged since a snapshot at least 3 seconds old is quiet, and pod types at once. Otherwise the snapshot restarts, and only herdr's hook process, which nobody waits on, sleeps out the window and reads again; a command records nothing but that screen: an unproven window is not now, not an attempt. The next eligible trigger (an idle hook, a tick, or a mutating command, as soon as 3 seconds later) finds the screen unchanged and types without waiting. The usual case, a worker that went idle more than 3 seconds before the coordinator's message, is nudged on the first attempt; mail sent within 3 seconds of the recipient going idle waits for the next trigger. A `check --wait` or `ask` waiter records observations and snapshots from the events it sees and makes no wake attempt for them (herdr's hook for the same event does); its once-a-minute tick does make one. The same holds for a person typing: pod reads the input only in an attempt that got past the quiet window, so `interruption` appears at the next tick or hook after the typing stops changing the screen, not at the keystroke, and with nobody waiting and no hook it is the tab bar tick (every 30 seconds) that notices it.

**Safe input.** Every line pod types into an agent pane, the nudge and the preamble alike, is pasted without submitting it (preserving embedded preamble newlines), then pod re-reads the pane and confirms the pasted text is visible, and only then sends Enter. pod never sends a bare Enter it cannot attribute to text it just confirmed. The input must be proven empty and ready before paste; after an unconfirmed paste later attempts skip until it is proven cleared. pod does not append, submit, or delete uncertain text. Each nudge attempt is stored in `wake_attempts`, and the preamble injection is recorded once in `workers.preamble_input`; both use the outcomes `delivered` (text confirmed, Enter sent), `rendered_unconfirmed` (text pasted, confirmation unavailable, Enter withheld), `skipped` (a precondition failed, reason in `wake_attempts.detail` for a nudge or `workers.last_error` for injection), or `failed`.

**Wake ladder.** Nudges for one mailbox form one bounded, store-derived policy: at most 5 attempts, the first at commit in the sender's process, each later one only on an eligible trigger (an idle event hook for the recipient, or a mutating `pod` command for the Run, which retries every mailbox of the Run with unread mail not already in an outstanding Delivery, at most one attempt per mailbox per invocation) and no sooner than 60 seconds after the previous recorded attempt. Read-only commands do not advance it or type. There is no timer outside a bounded wait and the tab bar tick, which are eligible triggers (below); exhaustion is still a count, not an elapsed time. Only recorded attempts count: typed nudges (`delivered`, `rendered_unconfirmed`), `failed` inputs, and `skipped` attempts against a recipient pod could not reach (presence unproven or absent, evidence stale, an outstanding Delivery, an unconfirmed earlier paste, or safe input refused). A working recipient, an unproven quiet window, or a wake in flight records nothing, whatever the trigger, and the next trigger tries again at once. The preamble record does not count. New mail does not restart a pending policy. After the fifth attempt without a consuming `check` by the recipient, attention is `unreachable` with `requiresAction: true`. For a worker mailbox the coordinator's `nextAction` is `worker-show` (see [worker commands](commands-workers.md)); for the Run mailbox the status is written to the board for the user, as a `mailbox.unreachable` event in `run.json`'s `events`. The position is computed from `wake_attempts` and `events`, so a herdr restart or a new `pod` process neither loses nor resets it. A consuming `check` by the recipient clears it. pod never retypes the message body, and a nudge proves nothing about reading.

Receipt: the stored message row (as `check` returns it), plus `settled` for an accepted `worker_done`, `warnings` when there are any, and `mutation` (see [conventions](commands.md#conventions)). A group send also lists every row under `messages` with `mailbox: "group:<name>"`. The closure itself is stored on the Dispatch (`termination_reason`, `termination_target`) and shown by `log`, `task-list`, and `worker-show`.

```json
{"message": {"id": "msg_31ac90", "sequence": 118, "run_id": "run_7f3a2c", "mailbox": "run:run_7f3a2c", "type": "worker_done", "from_handle": "terra-k3x9", "subject": "limiter implemented", "priority": "normal", "payload": {"dispatch_id": "ctx_9d41e7", "outcome": "succeeded"}, "read": false},
 "settled": {"taskStatus": "completed", "dispatchStatus": "completed", "failureCount": 0, "promoted": ["task_d4e5f6"]},
 "mutation": {"requestId": null, "replayed": false}}
```

A failed `worker_done` with a target, and the refusal when the reason is missing (exit 1, stored as a rejected lifecycle `status` message; see [errors.md](errors.md)):

```sh
pod send --type worker_done --subject "blocked on schema" --body "..." --task-id task_a1b2c3 --dispatch-id ctx_9d41e7 --outcome failed --reason blocked_on --reason-target task_d4e5f6
pod send --type worker_done --subject "blocked on schema" --body "..." --task-id task_a1b2c3 --dispatch-id ctx_9d41e7 --outcome failed
```

```json
{"error": {"code": "closure_reason_required", "message": "--outcome failed needs --reason", "data": {"nextSteps": "Resend with --reason <no_follow_on|handed_off|blocked_on|denied|canceled|escalated|superseded>."}}}
```

A `--reason-target` that pod cannot find in the Run is accepted, not refused; the receipt carries a `warnings` entry (the wording of the entry is illustrative):

```json
{"message": {"id": "msg_31ac92", "mailbox": "run:run_7f3a2c", "type": "worker_done"},
 "settled": {"taskStatus": "ready", "dispatchStatus": "failed", "failureCount": 1, "promoted": []},
 "warnings": ["reason target task_zzzzzz was not found in this Run"]}
```

An escalation carries its evidence:

```sh
pod send --type escalation --subject "Blocked: schema unclear" --body "Two schemas match." --evidence msg_31ac40 --recommendation "Use v2" --task-id task_a1b2c3 --dispatch-id ctx_9d41e7
```

Repeated `worker_done` with the same outcome returns `settled` with `duplicate: true` as a normal receipt on stdout, not an error envelope (exit 0, because idempotent repeats succeed; see [errors.md](errors.md#values-that-look-like-errors-but-are-not)):

```json
{"message": null, "duplicate": true, "settled": {"taskStatus": "completed", "dispatchStatus": "completed", "failureCount": 0, "promoted": []}}
```

A rejected lifecycle message is stored anyway in the Run mailbox, rewritten to type `status`, priority `high`, with `rejected_reason` set to the error code, and delivered to the coordinator; the command prints the error envelope and exits 1. Replaying it with the same `--retry-request` returns the same error without storing a second row.

```json
{"error": {"code": "caller_fenced", "message": "the Dispatch is no longer active", "data": {"nextSteps": ["Do not retry. The message was stored for the coordinator; stop work and go idle."]}}}
```

Errors: `worker_identity_changed`, `caller_fenced`, `inactive_dispatch` (a conflicting repeat of `worker_done`), `closure_reason_required`, `closure_target_required`, `escalation_evidence_required`, `not_in_herdr`, `store_access_denied`. `caller_mismatch` when `--from` does not match the resolved caller, `invalid_recipient` for a refused `--to` (both exit 1). Using `--payload` with a typed flag, or `--outcome`, `--reason`, or `--reason-target` with another type, is a usage error (exit 2). Exit 0 on success.

## `pod check`

Reads a mailbox.

| Flag | Meaning |
|---|---|
| `--terminal <handle>` | Worker form: read the Dispatch mailbox of that worker. |
| `--run <id>` | Select the Run. |
| `--ack <delivery_id>` | Mark that batch read, then check again. |
| `--unread` | Explicit consuming form of the default. |
| `--peek` | Read-only; unread messages, newest 100. |
| `--all` | Read-only; history, newest 100. |
| `--types <csv>` | With `--wait`, decides only when to wake. With `--peek` and `--all`, filters rows by type. |
| `--format` | Also render rows as agent-readable text. |
| `--wait` | Block until a matching message exists. A consuming `check` by the recipient also clears that mailbox's wake ladder. |
| `--timeout-ms <n>` | Bound for `--wait`. |

Coordinator `check` returns the oldest unacknowledged FIFO batch, up to 50 messages, as one Delivery, and replays it until acknowledged. Every consuming form (coordinator or worker) runs under `--retry-request`, carries `mutation`, and records a `mailbox.consumed` event; `--peek` and `--all` are read-only. Every form carries `count`, and `formatted` only with `--format`. Only one `check --wait` waiter exists per Run. `ask` and `ask --resume` wait on their question independently, consume no mailbox Delivery, and do not take this slot. While waiting, pod prints `{"_keepalive": true}` to stderr every 15 seconds. A timeout or empty result is a checkpoint, not a failure.

```json
{"delivery": {"id": "dlv_8b21", "run_id": "run_7f3a2c", "mailbox": "run:run_7f3a2c", "status": "outstanding", "consumer_generation": 1, "message_ids": ["msg_31ac90"]},
 "messages": [{"id": "msg_31ac90", "type": "worker_done", "from_handle": "terra-k3x9", "subject": "limiter implemented", "body": "I added the token bucket. cargo test limiter passes. Nothing is left.", "priority": "normal", "payload": {"dispatch_id": "ctx_9d41e7", "outcome": "succeeded"}}], "count": 1}
```

Empty result (exit 0; `timedOut` is true only after a `--wait` timed out):

```json
{"delivery": null, "messages": [], "count": 0, "timedOut": false}
```

A `--wait` (and an `ask`) ticks while it blocks: once a minute it runs one wake ladder pass for the Run's mailboxes with unread mail (the 60-second rule and the five-attempt count still hold, the pass may spend the 3-second quiet check, and a pass that finds the recipient working, its pane not yet quiet, or another process's wake in flight records no attempt, like any trigger, so a long turn cannot exhaust the ladder) and recomputes attention. The tab bar entry, `pod usage --tick`, does the same every 30 seconds for a coordinator that is not waiting. A `--wait` result also carries `wokeOn` (`message`, `attention`, or `timeout`) and `attention`: the active workers of the Run whose projection requires action right now, or that went `stale` (no activity, heartbeat, or message for 10 minutes; `requiresAction` stays false), each as `{dispatchId, categories, nextAction}` (the same values as `worker-list`). The wait ends early, with `wokeOn: attention` and `timedOut: false`, when a worker newly requires action while it waits (an approval or input prompt, an exit without a report, an exhausted wake ladder, going `stale`); workers that already required action when the wait began do not end it again, so an unhandled prompt cannot make every wait return at once.

Worker form: returns unread messages in the Dispatch mailbox and marks them read on return; no ack is needed. A question routed to this worker with `ask --to` arrives here as a `question` message; answer it with `pod reply`. This read is the consuming `check` that clears the worker mailbox's wake ladder.

```json
{"messages": [{"id": "msg_31ac95", "type": "status", "from_handle": "term_0f69c67b", "subject": "Use the v2 schema", "body": "Please use v2."}], "count": 1}
```

When the Dispatch is no longer the worker's:

```json
{"error": {"code": "consumer_fenced", "message": "Dispatch ctx_9d41e7 is no longer this worker's", "data": {"nextSteps": "Stop work. Do not send worker_done. Go idle."}}}
```

`--wait` is the coordinator's wait on the Run mailbox. A worker (or `--terminal`) cannot use it, because its mailbox is read with `check --terminal` and it waits for answers with `ask`; that is a usage error (exit 2). On a timeout, `timedOut` is true and any waiting non-matching batch is still returned. On `wokeOn: attention` the batch is returned the same way (possibly empty) and `attention` names the workers to look at; act on their `nextAction`, then wait again.

Errors: `waiter_exists` (a second `--wait` on the Run), `consumer_fenced`, `not_in_herdr`, `store_access_denied`. A coordinator Delivery fenced by `run-use` is replaced by a new one, not an error. Exit 1 on errors.

## `pod ask`

Stores a `question` message and blocks until it is answered.

| Flag | Meaning |
|---|---|
| `--question <text>` | The question. |
| `--options <a,b>` | Suggested answers, comma-separated. |
| `--timeout-ms <n>` | Default 600000; maximum 1800000. |
| `--resume <message_id>` | Resume a timed-out question without creating a duplicate. |
| `--to dispatch:<id>` | Route the question to another live worker in the same Run (a specialist) instead of the coordinator. The Run mailbox receives a copy in the same thread. |
| `--from <handle>` | Must match the resolved caller. |

Answered:

```json
{"question": {"message_id": "msg_31ac40", "status": "answered", "answer_body": "v2"}}
```

On timeout the question stays `pending` and the command returns the message ID to resume with. It exits 0: a timeout is a checkpoint, not a failure (spec 8.3), and Orca's `ask` timeout exits 0 too (the contract sample `reply.json` runs one as a setup step). Read `timedOut` and `status`, not the exit code, to tell it from an answer:

```json
{"question": {"message_id": "msg_31ac40", "status": "pending"}, "timedOut": true, "resume": "pod ask --resume msg_31ac40"}
```

Pending questions are closed when their Dispatch fails or is fenced; resuming one then returns `status: closed`.

### Asking a specialist

`ask --to dispatch:<id>` sends the question to a live worker named under `=== SPECIALISTS ===` in the asker's preamble, instead of the coordinator. The question is stored in the specialist's `dispatch:<id>` mailbox and the Run mailbox receives a copy in the same thread, so the coordinator sees it and may answer first. The asker blocks as for any `ask`, with the same timeout and `--resume`. The first `reply` wins; a later one is refused with `question_answered`. The target must own an active Dispatch, be live in the same Run, and not be the asker (`recipient_not_live`, also for unknown or other-Run IDs). Losing the specialist does not close the question: its owning Dispatch is the asker's, and the coordinator can still answer. A routed question shows `unblock: <specialist handle>` on the board instead of `unblock: coordinator`.

```sh
pod ask --from terra-k3x9 --question "Rate limit by API key or client IP?" --options "api_key,client_ip" --timeout-ms 600000 --to dispatch:ctx_c4d5e6
```

The coordinator's copy, as `check` returns it. `payload.routed_to` names the specialist's mailbox and `payload.copy_of` the specialist's copy, in the same thread:

```json
{"id": "msg_31ac43", "type": "question", "mailbox": "run:run_7f3a2c", "from_handle": "terra-k3x9", "thread_id": "thr_5e90", "body": "Rate limit by API key or client IP?", "payload": {"dispatch_id": "ctx_9d41e7", "options": ["api_key", "client_ip"], "routed_to": "dispatch:ctx_c4d5e6", "copy_of": "msg_31ac42"}}
```

Errors: `recipient_not_live`, `not_in_herdr`, `store_access_denied`. Usage error (exit 2) when neither `--question` nor `--resume` is given, or when both are.

## `pod reply --id <message_id> --body <text>`

Answers a pending question and wakes the asker. For a routed question the first `reply` wins. A reply to either copy (the specialist's or the coordinator's) answers the one question, resolved by the shared thread.

The `reply` receipt is the stored answer message (a `status` row in the asker's Dispatch mailbox, in the question's thread) and the answered question row:

```json
{"message": {"id": "msg_31ac41", "mailbox": "dispatch:ctx_9d41e7", "type": "status", "subject": "Question answered", "body": "v2", "thread_id": "thread_5e90a1b2c3d4"},
 "question": {"message_id": "msg_31ac40", "status": "answered", "answer_message_id": "msg_31ac41", "answer_body": "v2"}}
```

Errors: `question_answered` (the question is already `answered`, for example a later reply to a routed question), `not_in_herdr`, `store_access_denied`. Replying to a `closed` question is `question_closed` (exit 1). Exit 0 on success.

## `pod inbox [--limit <n>] [--terminal <handle>] [--full] [--run <id>]`

Lists messages in the Run mailbox (or, with `--terminal`, the Dispatch mailbox of that worker), newest first. Read-only: it never consumes messages, creates a Delivery, or changes `read`. `--limit` defaults to 100, the same cap as `check --all`. Rows are full message rows, as `check` returns them (as Orca's are); without `--full` each `body` is cut to its first 200 characters.

```json
{"mailbox": "run:run_7f3a2c", "messages": [
  {"id": "msg_31ac90", "sequence": 118, "run_id": "run_7f3a2c", "mailbox": "run:run_7f3a2c", "type": "worker_done", "from_handle": "terra-k3x9", "subject": "limiter implemented", "body": "I added the token bucket.", "priority": "normal", "payload": {"dispatch_id": "ctx_9d41e7"}, "read": true, "created_at": "2026-10-05T14:11:02.000000Z"}], "count": 1}
```

Errors: `store_access_denied`. Exit 0.
