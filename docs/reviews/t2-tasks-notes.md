# T2 implementation notes

- ASSUMPTION: Reusing a caller-scoped request ID for a different method or payload is a usage error (exit 2); the message names both methods.
- ASSUMPTION: A failed mutation leaves its precommitted receipt in `pending`, so the caller can inspect and retry it.
- ASSUMPTION: Resolving an already-resolved gate is idempotent, returns `duplicate: true`, and never stores a second `decision_gate` message.
- ASSUMPTION: `reset --scope all --force` deletes old receipts, then stores its own completed receipt so retrying that reset replays safely.
- ASSUMPTION: `reset --scope run:<missing>` is a usage error (exit 2) that names the missing Run, matching the existing command/error contract.
- ASSUMPTION: If one terminal has bound multiple Runs, the most recent `run-create` or `run-use` binding wins.
- ASSUMPTION: A manual retry may name a `circuit_broken` Dispatch; cumulative failure count remains at or above the breaker threshold.
- ASSUMPTION: `runs.coordinator_handle` is the coordinator terminal ID until a separate handle source exists.
- ASSUMPTION: `request-show` outside herdr is refused with `not_in_herdr`, because no caller fingerprint exists to scope the lookup safely.

Implementation coordination notes:

- T1 row modules were empty on this parallel branch. Per coordinator direction, public temporary row structs and a private `sql` compatibility section live in each owned engine file for integration review to replace with `store::rows`.
- The coordinator authorized the otherwise out-of-ownership `Engine::after_commit` addition. It is a documented no-op for T6 board export and T8 wake wiring.
- The coordinator authorized removing T2-owned commands from T0's stale `not_implemented` assertion; T2's CLI tests cover their flags and behavior.
- CLI integration tests use feature-gated `POD_FAKE_TERMINAL_ID` to supply caller identity to the existing empty `FakeHerdr`.

## Review (Opus)

Reviewed `git diff main...t2-tasks` against spec sections 3.4, 6, 7.5, 9, 9.1, 13, `docs/reference/{commands,errors,state-machines}.md`, and the T2 test list. Every listed test exists under its listed name and passes.

### Verdicts on the assumptions

1. **Reused request ID with another method or payload is usage (exit 2).** Accepted. errors.md has no code for it, and a mismatched replay is malformed input.
2. **A failed mutation leaves its receipt `pending`.** Accepted. `request-show` then says `pending` and a replay re-runs the mutation, which is refused again if the refusal still holds. That is harmless. The receipt is only precommitted when `--retry-request` is given.
3. **Resolving a resolved gate is idempotent with `duplicate: true` and no second message.** Accepted. A different `--resolution` on the replay is not applied, and the receipt shows the original resolution beside `duplicate: true`, so the caller can see that.
4. **`reset --scope all --force` deletes receipts, then stores its own.** Accepted. Covered by `reset_all_own_retry_receipt_survives`.
5. **`reset --scope run:<missing>` is usage.** Accepted. This matches errors.md's rule that a missing Run named by an ID is usage.
6. **The most recent binding wins.** Accepted. The binding is ordered by `runs.updated_at`, which only `run-create` and `run-use` write today. Any later task that writes `runs.updated_at` for other reasons would change which Run a terminal is bound to, so that column must stay binding-only, or the binding needs its own column.
7. **A manual retry may name a `circuit_broken` Dispatch.** Accepted. The spec makes `--retry-of` valid for a `failed` Task, and the count stays cumulative, so the next counted failure breaks the circuit again.
8. **`coordinator_handle` is the terminal ID.** Accepted. The commands.md example shows the same.
9. **`request-show` outside herdr is `not_in_herdr`.** Accepted. Receipts are keyed by the caller fingerprint, so there is nothing to look up without a caller.

The coordination notes are accepted as authorized: the temporary row structs, `Engine::after_commit` in `src/engine/mod.rs`, the `tests/t0.rs` deletions, and the test-only `POD_FAKE_TERMINAL_ID`.

### Defects fixed

- **The errors.md `data` fields were missing.** `PodError` had no `data`, so `task_not_startable` could not carry `status` and `unmetDependencies`, `task_guard_refused` could not carry `guard`, and `reset_requires_force` could not carry `liveWorkers`. Added `PodError.data` and `with_data`, and `emit_err` merges them beside `nextSteps`. No other branch builds `PodError` with a struct literal, so the new field breaks no other branch. The plan's T2 section records it.
- **Gates broke the task state machine.** `gate-create` on a `completed` or `failed` Task set it `blocked`, and `gate-resolve` then returned it to `ready`, which reopened settled work. Now `gate-create` blocks only from `pending`, `ready`, or `dispatched` (T12), and `gate-resolve` releases only a `blocked` Task (T13, T19). While a second gate on the Task is still pending, the Task stays `blocked`. That is a conservative reading; the spec does not cover several gates on one Task. When a gate completes a Dispatch, its worker row is now marked `reclaimable` (it used to stay `active` with no active Dispatch), the same as `settle_dispatch` does.
- **`status` failed when herdr was down.** Inside herdr with the server unreachable, or with a bad `POD_CONFIG`, `status` exited 1. It now opens only paths, store, and herdr, as on `main`, and resolves the caller best effort for the bound Run. commands.md requires exit 0 with the degraded part reported in the body.
- **A receipt race could run a mutation twice.** Two concurrent replays of one request ID could both find the receipt `pending` and both mutate. The mutation's IMMEDIATE transaction now re-reads the receipt first.
- **`task-update` without `--result` erased the stored result.** It now keeps the stored result.
- **The `nextAction` for `waiting_on_failed_dependency` was useless.** It was `pod task-list --run <id>`, which just repeats the command. It is now `pod dispatch-show --task <failed dependency>`.
- **`task-update`, `gate-resolve`, and `reset` refused outside herdr.** They name their target by ID, and commands.md lists no `not_in_herdr` for them. A person answering a user gate may do so from a plain shell. A `--retry-request` still needs a caller.
- **A non-test build warned.** The fake-terminal hook left an unused `mut` when the `fake-herdr` feature was off.

### Not fixed, for integration

- `tests/t0.rs` conflicts with `main` (checked against `main` at `bd3e28a`, after T3 merged) in two hunks of `cli_commands_accept_documented_flags`. Both sides deleted adjacent entries: `main` removed `skills`, `board`, and T3's `send`, `check`, `ask`, `reply`, and `inbox`, and T2 removed `log` and the run, task, gate, `request-show`, and `reset` entries. To resolve it, keep neither side of either hunk, and also drop the `&[` line that is left open just before the second hunk. With that resolution, the merged tree passes `cargo test` and `cargo clippy --all-targets -- -D warnings`.
- T3 on `main` settles `worker_done` through its own temporary `messages::sql::settle_dispatch` and `Settlement`. Integration must point it at `tasks::settle_dispatch`. The two copies differ: T3 counts failures from the Dispatch's own `failure_count`, while T2 takes the maximum across the Task's Dispatches, so the count is cumulative even when a retry Dispatch starts at 0. T3 does not mark the worker `reclaimable` or write events. T3 takes `outcome: &str`, while T2 takes the `Outcome` enum. T2's `Settlement` serializes in camelCase.
- `reset` deletes store rows but leaves `<state>/runs/<run_id>/run.json` from T6. Once T6's export is wired through `after_commit`, reset should remove or rewrite that file.
- The read-only T2 commands (`task-list`, `gate-list`, `run-show`, `log`) still go through the full engine. With `--run` inside herdr while herdr is down, they fail on caller resolution even though they do not need a caller.
