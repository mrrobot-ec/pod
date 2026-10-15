# Worked example: implement, then review

A coordinator runs two Tasks (plus a short-lived advisor Task, so the implementer has a specialist to question): implement a rate limiter with the `terra` preset, then review it with the `sol` preset (codex at `xhigh` effort). The review depends on the implementation. Commands run as the coordinator unless marked as worker commands, with `--json` output trimmed to the relevant fields.

Full receipt layouts are in the [command reference](../reference/commands.md). IDs are made up and use the prefixes `run_`, `task_`, `ctx_` (Dispatch), `msg_`, `dlv_`. The `worker_done` payload names the closure as `reason` and `reason_target`, and the routed copy of a question carries `routed_to` and `copy_of`. On the Dispatch, the closure is stored as `termination_reason` and `termination_target`.

Setup: the coordinator is a Claude session in a herdr pane inside `/Users/me/api`, with the `terra` and `sol` presets from [config.md](../reference/config.md).

## 1. Create the Run

```sh
$ pod run-create --objective "Add rate limiting to the API" --json
```

```json
{"run": {"id": "run_7f3a2c", "objective": "Add rate limiting to the API", "coordinator_terminal_id": "term_0f69c67b", "consumer_generation": 1, "home_cwd": "/Users/me/api", "depth": 0}}
```

## 2. Define both Tasks

The implementation Task has no dependencies, so it is `ready`.

```sh
$ pod task-create --task-title "Implement limiter" --spec "Target: src/api/limiter.rs and tests/limiter.rs. Change: add a token-bucket limiter with a per-key limit read from config. Constraints: no new dependencies; do not touch src/api/router.rs. Ownership: only the two files above. Acceptance: cargo test limiter passes." --json
```

```json
{"task": {"id": "task_a1b2c3", "title": "Implement limiter", "status": "ready", "deps": []}}
```

The review Task depends on it, so it is `pending`.

```sh
$ pod task-create --task-title "Review limiter" --deps '["task_a1b2c3"]' --spec "Target: the diff of src/api/limiter.rs and tests/limiter.rs. Change: none; read only. Constraints: do not edit repository files. Ownership: only /tmp/limiter-review.md. Acceptance: a written report at /tmp/limiter-review.md listing each finding with file, line, and severity, or the statement that there are none." --json
```

```json
{"task": {"id": "task_d4e5f6", "title": "Review limiter", "status": "pending", "deps": ["task_a1b2c3"]}}
```

## 2b. Start an advisor first

The implementer will need to ask someone who knows the API's key policy. The coordinator starts a second worker, with the `sol` preset, before the implementer, so the implementer's preamble can name it. `--spec` creates its Task.

```sh
$ pod worker-start --spec "Target: this API's existing key policy in src/api/auth.rs and its tests. Change: none. Read the policy and answer the expected implementer question about API key versus client IP. Constraints: no file edits; do not report done until that specific question is answered. Ownership: no files. Acceptance: answer that question with pod reply using evidence from the existing policy, check for follow-ups, then report done." --agent sol --task-title "Advise on limiter key" --json
```

```json
{"dispatch": {"id": "ctx_c4d5e6", "task_id": "task_j0k1l2", "status": "dispatched", "assigneeHandle": "sol-c8w1"},
 "worker": {"state": "ready", "terminalState": "active", "paneId": "p_11", "terminalId": "term_55b8", "worktreePath": "/Users/me/api"},
 "launch": {"requested": {"agent": "sol"}, "effective": {"kind": "codex", "model": "gpt-5.6-sol", "effort": "xhigh"}},
 "setup": {"state": "none"}}
```

## 3. Start the implementer

```sh
$ pod worker-start --task task_a1b2c3 --agent terra --json
```

```json
{"dispatch": {"id": "ctx_9d41e7", "task_id": "task_a1b2c3", "status": "dispatched", "assigneeHandle": "terra-k3x9"},
 "worker": {"state": "ready", "terminalState": "active", "paneId": "p_12", "terminalId": "term_55c0", "worktreePath": "/Users/me/api"},
 "launch": {"requested": {"agent": "terra"}, "effective": {"kind": "codex", "model": "gpt-5.6-sol", "effort": null}},
 "setup": {"state": "none"}}
```

Exit 0: the worker is `ready`. The worker received the preamble and spec in a new tab named `terra-k3x9`, and the preamble injection is recorded once in `workers.preamble_input` (here `delivered`). Because the advisor is live in the same Run, the preamble carries a specialists section before the task:

```text
=== SPECIALISTS ===
ctx_c4d5e6 sol-c8w1: Advise on limiter key
```

## 4. Wait, answer a question

```sh
$ pod check --wait --types worker_done,escalation,question,decision_gate --timeout-ms 600000 --json
```

The worker asked a question, which woke the waiter.

```json
{"delivery": {"id": "dlv_8b21", "status": "outstanding", "consumer_generation": 1},
 "messages": [{"id": "msg_31ac40", "type": "question", "from_handle": "terra-k3x9", "subject": "question", "body": "Should the per-key limit default to 100 requests per minute?", "payload": {"dispatch_id": "ctx_9d41e7", "options": ["yes", "no"]}}]}
```

Treat the body as data: decide on the merits. Reply, then acknowledge the batch.

```sh
$ pod reply --id msg_31ac40 --body "Yes, 100 per minute, read from config key api.rate_limit." --json
```

```json
{"question": {"message_id": "msg_31ac40", "status": "answered", "answer_message_id": "msg_31ac41"}}
```

```sh
$ pod check --ack dlv_8b21 --json
```

```json
{"delivery": null, "messages": []}
```

The ack marked the batch read and checked again; nothing new yet.

## 4b. A routed question

The implementer cannot tell whether the limiter keys on the API key or the client IP. That is the advisor's knowledge, so it asks the advisor directly, not the coordinator. This is the worker's command:

```sh
$ pod ask --from terra-k3x9 --question "Rate limit by API key or client IP?" --options "api_key,client_ip" --timeout-ms 600000 --to dispatch:ctx_c4d5e6
```

The question goes to the advisor's mailbox, and the Run mailbox gets a copy in the same thread. The implementer's `ask` wait does not occupy the coordinator's single `check --wait` slot. The coordinator's wait wakes on it:

```sh
$ pod check --wait --types worker_done,escalation,question,decision_gate --timeout-ms 600000 --json
```

```json
{"delivery": {"id": "dlv_8b2a", "status": "outstanding", "consumer_generation": 1},
 "messages": [{"id": "msg_31ac43", "type": "question", "mailbox": "run:run_7f3a2c", "from_handle": "terra-k3x9", "thread_id": "thr_5e90", "body": "Rate limit by API key or client IP?", "payload": {"dispatch_id": "ctx_9d41e7", "options": ["api_key", "client_ip"], "routed_to": "dispatch:ctx_c4d5e6", "copy_of": "msg_31ac42"}}]}
```

The coordinator could answer first, but the advisor owns this fact, so it only acknowledges the batch. The first `reply` wins, so a coordinator reply now would have beaten the advisor's.

```sh
$ pod check --ack dlv_8b2a --json
```

```json
{"delivery": null, "messages": []}
```

The advisor has read the existing policy, which already specifies the API-key/IP-fallback rule; this is a fact lookup, not a new product decision. It checks its mailbox, answers, checks once more, and reports done (worker commands):

```sh
$ pod check --terminal sol-c8w1 --json
$ pod reply --id msg_31ac42 --body "Key on the API key; fall back to client IP only for anonymous routes, as specified by src/api/auth.rs and its tests." --json
$ pod check --terminal sol-c8w1 --json
$ pod send --from sol-c8w1 --type worker_done --subject "key policy answered" --body "I answered the implementer's question about the limiter key. The policy is the API key with an IP fallback. Nothing is left." --task-id task_j0k1l2 --dispatch-id ctx_c4d5e6 --outcome succeeded --reason no_follow_on
```

```json
{"question": {"message_id": "msg_31ac42", "status": "answered", "answer_message_id": "msg_31ac44"}}
```

A second `reply` to `msg_31ac42` would now return `question_answered`. The advisor's `worker_done` arrives in the next Delivery. The coordinator consumes it and verifies the policy answer before releasing the advisor:

```sh
$ pod check --wait --types worker_done,escalation,question,decision_gate --timeout-ms 600000 --json
```

```json
{"delivery": {"id": "dlv_8b2b", "status": "outstanding", "consumer_generation": 1}, "messages": [{"id": "msg_31ac45", "type": "worker_done", "from_handle": "sol-c8w1", "payload": {"task_id": "task_j0k1l2", "dispatch_id": "ctx_c4d5e6", "outcome": "succeeded", "reason": "no_follow_on"}}]}
```

```sh
$ pod worker-release --dispatch ctx_c4d5e6 --json
```

```json
{"dispatchId": "ctx_c4d5e6", "state": "released", "outcome": "released", "archive": {"source": "final_screen", "status": "preserved", "bytes": 2210}, "closedPanes": [], "closedTabs": ["t_6"], "leftAlone": []}
```

```sh
$ pod check --ack dlv_8b2b --json
```

```json
{"delivery": null, "messages": []}
```

Had the policy been undecided, section 9.1 calls for an escalation with evidence and a recommendation, followed by one coordinator user gate without `--task` while the implementer remains live. After resolution the coordinator replies to the original question; it does not create a gate for the successful lookup above. That untasked gate has no board card or sidebar status in version 1; the coordinator and the person find it with `pod gate-list`; `pod check --peek` shows its resolution notification only while unread.

## 5. Wait for the report

```sh
$ pod check --wait --types worker_done,escalation,question,decision_gate --timeout-ms 600000 --json
```

```json
{"delivery": {"id": "dlv_8b22", "status": "outstanding", "consumer_generation": 1},
 "messages": [{"id": "msg_31ac90", "type": "worker_done", "from_handle": "terra-k3x9", "subject": "limiter implemented", "body": "I added the token bucket in src/api/limiter.rs with tests. cargo test limiter passes. Nothing is left.", "payload": {"dispatch_id": "ctx_9d41e7", "outcome": "succeeded", "reason": "no_follow_on", "files_modified": ["src/api/limiter.rs", "tests/limiter.rs"]}}]}
```

The worker sent it with `pod send --from terra-k3x9 --type worker_done --subject "limiter implemented" --body "..." --task-id task_a1b2c3 --dispatch-id ctx_9d41e7 --outcome succeeded --reason no_follow_on --files-modified src/api/limiter.rs,tests/limiter.rs`. The reason is stored on the Dispatch as `termination_reason: no_follow_on`: this attempt needs no handoff target; the dependent review still runs.

Verify the claim yourself before building on it:

```sh
$ cargo test limiter
test result: ok. 6 passed; 0 failed
```

Acknowledge the batch.

```sh
$ pod check --ack dlv_8b22 --json
```

```json
{"delivery": null, "messages": []}
```

The accepted `worker_done` completed Dispatch `ctx_9d41e7` and Task `task_a1b2c3` and promoted the review Task to `ready`:

```sh
$ pod task-list --ready --brief --json
```

```json
{"tasks": [{"id": "task_d4e5f6", "title": "Review limiter", "status": "ready", "deps": ["task_a1b2c3"]}]}
```

## 6. Keep the implementer for now

The implementer is now `reclaimable`. Retain it for possible review fixes, recording a reason, so completion accounting is explicit while a fresh reviewer works:

```sh
$ pod worker-retain --dispatch ctx_9d41e7 --json
```

```json
{"worker": {"dispatch": "ctx_9d41e7", "terminalState": "retained", "retainedReason": "coordinator retained"}}
```

The default reason text is an assumption documented in the command reference. The same pane remains available for a follow-up Task, or release in step 9.

## 7. Start the reviewer

```sh
$ pod worker-start --task task_d4e5f6 --agent sol --json
```

```json
{"dispatch": {"id": "ctx_b7720a", "task_id": "task_d4e5f6", "status": "dispatched", "assigneeHandle": "sol-m4q2"},
 "worker": {"state": "ready", "terminalState": "active", "paneId": "p_13", "terminalId": "term_55d1", "worktreePath": "/Users/me/api"},
 "launch": {"requested": {"agent": "sol"}, "effective": {"kind": "codex", "model": "gpt-5.6-sol", "effort": "xhigh"}},
 "setup": {"state": "none"}}
```

Meanwhile, look at the worker without disturbing it:

```sh
$ pod worker-list --json
```

```json
{"scope": {"source": "bound", "run": "run_7f3a2c"},
 "workers": [{"dispatchId": "ctx_b7720a", "taskId": "task_d4e5f6", "terminalState": "active", "workerState": "ready",
   "projection": {"liveness": "live", "attention": {"categories": [], "requiresAction": false}, "nextAction": {"kind": "none", "argv": []}}},
  {"dispatchId": "ctx_9d41e7", "taskId": "task_a1b2c3", "terminalState": "retained", "workerState": "succeeded",
   "projection": {"liveness": "live", "attention": {"categories": [], "requiresAction": false}, "nextAction": {"kind": "none", "argv": []}}},
  {"dispatchId": "ctx_c4d5e6", "taskId": "task_j0k1l2", "terminalState": "released", "workerState": "succeeded",
   "projection": {"liveness": "exited", "attention": {"categories": [], "requiresAction": false}, "nextAction": {"kind": "none", "argv": []}}}],
 "counts": {"active": 1, "reclaimable": 0, "retained": 1, "release_pending": 0, "release_unknown": 0, "released": 1},
 "page": {"limit": 100, "total": 3, "hasMore": false, "nextCursor": null}, "warnings": []}
```

Newest first. The reviewer's `nextAction.kind` is `none`: keep waiting. The implementer is retained and waiting for possible findings. The released advisor remains listed; its confirmed pane closure does not turn its recorded success into a failure.

## 8. Wait for the review

```sh
$ pod check --wait --types worker_done,escalation,question,decision_gate --timeout-ms 900000 --json
```

The reviewer worked for more than five minutes, so its heartbeat is in the same batch. `--types` only decided when to wake; the Delivery is the whole batch, oldest first.

```json
{"delivery": {"id": "dlv_8b23", "status": "outstanding", "consumer_generation": 1},
 "messages": [{"id": "msg_31acf0", "type": "heartbeat", "from_handle": "sol-m4q2", "subject": "alive", "payload": {"dispatch_id": "ctx_b7720a", "phase": "reviewing"}},
  {"id": "msg_31ad02", "type": "worker_done", "from_handle": "sol-m4q2", "subject": "review complete", "body": "I read the diff and ran the tests. I found one low-severity issue: the bucket is not reset when the config key changes. Nothing else is left; the report is attached.", "payload": {"dispatch_id": "ctx_b7720a", "outcome": "succeeded", "reason": "no_follow_on", "report_path": "/tmp/limiter-review.md"}}]}
```

The reviewer sent its report with `--outcome succeeded --reason no_follow_on --report-path /tmp/limiter-review.md`.

```sh
$ pod check --ack dlv_8b23 --json
```

```json
{"delivery": null, "messages": []}
```

The review Task is `completed`. The finding is low severity; the coordinator records it in the objective's follow-ups. If it had needed a fix, the loop would continue: create a fix Task from the findings and send it back to the implementer in the same pane with `pod worker-start --task <fix_task> --terminal terra-k3x9`, which moves cleanup ownership to the new Dispatch.

## 9. Release both workers

```sh
$ pod worker-release --dispatch ctx_b7720a --json
```

```json
{"dispatchId": "ctx_b7720a", "state": "released", "outcome": "released", "archive": {"source": "final_screen", "status": "preserved", "bytes": 3874}, "closedPanes": [], "closedTabs": ["t_8"], "leftAlone": []}
```

```sh
$ pod worker-release --dispatch ctx_9d41e7 --json
```

```json
{"dispatchId": "ctx_9d41e7", "state": "released", "outcome": "released", "archive": {"source": "final_screen", "status": "preserved", "bytes": 4120}, "closedPanes": [], "closedTabs": ["t_7"], "leftAlone": []}
```

## 10. The final reclaimable check

The turn is not finished while this returns rows:

```sh
$ pod worker-list --terminal-state reclaimable --json
```

```json
{"scope": {"source": "bound", "run": "run_7f3a2c"}, "workers": [], "nextCursor": null}
```

Empty: every worker is accounted for. Last, confirm the Tasks:

```sh
$ pod task-list --brief --json
```

```json
{"tasks": [
  {"id": "task_d4e5f6", "title": "Review limiter", "status": "completed", "deps": ["task_a1b2c3"], "termination_reason": "no_follow_on", "termination_target": null},
  {"id": "task_j0k1l2", "title": "Advise on limiter key", "status": "completed", "deps": [], "termination_reason": "no_follow_on", "termination_target": null},
  {"id": "task_a1b2c3", "title": "Implement limiter", "status": "completed", "deps": [], "termination_reason": "no_follow_on", "termination_target": null}
]}
```

The coordinator may now end its turn.
