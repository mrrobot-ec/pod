# T3 messaging notes

- ASSUMPTION: An empty consuming check is represented internally by a
  Delivery with status `empty`; the CLI emits `delivery: null` and no row is
  stored.
- ASSUMPTION: Priority tags are presentation-only and are added by
  `check --format`; the stored subject remains unchanged so priority cannot
  affect FIFO ordering.
- ASSUMPTION: A reply notification uses message type `status`, because the
  schema has no `reply` type; the linked questions row remains authoritative.
- ASSUMPTION: Until T8 adds blocking, CLI `ask --question` stores and returns a
  pending question immediately, while `ask --resume` and `check --wait` retain
  their T0 placeholder errors.
- ASSUMPTION: A group member is live when its Dispatch is `dispatched` and its
  worker is `ready` with `terminal_state = active`; `@idle` additionally needs
  the latest observation to say `present` and `idle_at_prompt`.
- ASSUMPTION: On macOS, T3 first uses herdr's pane process list to corroborate
  the recorded agent and caller PIDs. If that cannot establish the chain, it
  applies section 19's terminal-only fallback and returns a warning; Linux
  walks `/proc`.
- ASSUMPTION: The plan checklist's parenthetical mapping of a wrong terminal to
  `worker_identity_changed` was stale. Per the coordinator and
  `docs/reference/errors.md`, terminal mismatch is `caller_fenced`; verified
  process ancestry mismatch is `worker_identity_changed`.
- ASSUMPTION: T3's temporary public row and settlement types exactly mirror
  schema/T2 shapes. The wave integration review replaces them and the isolated
  private SQL sections with T1/T2 implementations, as directed by the
  coordinator.
- Coordinator-approved integration edit: removed the five implemented message
  commands from T0's skeleton-only not_implemented assertions.

## Review (Opus)

Verdicts on the assumptions above, in order:

1. Empty consuming check as an internal `empty` Delivery with no stored row:
   **accepted.** T8 must treat an `empty` Delivery as "no Delivery" and never
   ack its ID.
2. Priority tags are presentation-only: **accepted.** `--format` now renders
   type, ID, sender, subject, and body, and applies to every `check` form
   (worker, peek, all), not only the consuming one.
3. Reply notification stored as `status`: **accepted.** The answer row now
   records the answering terminal in `sender_terminal_id`.
4. `ask` stores and returns until T8 adds blocking: **accepted.**
5. Group liveness (`dispatched` + `ready` + `active`; `@idle` also needs the
   latest observation `present` and `idle_at_prompt`): **accepted.** Evidence
   age is not checked; T8 owns observation freshness.
6. macOS identity through herdr's pane process list: **corrected.** macOS now
   walks the parent chain with libproc `proc_pidinfo(PROC_PIDT_SHORTBSDINFO)`
   (the full BSD-info flavor fails on root-owned ancestors such as `login`, so
   every walk fell back). Ancestry is decided first on both platforms; herdr's
   pane list only corroborates when the chain cannot be read. Before, a proven
   non-ancestor on Linux could be overridden to "verified" by herdr, and on
   macOS `worker_identity_changed` could never occur. Plan T3 updated.
7. Wrong terminal is `caller_fenced`: **accepted**, matches errors.md. The
   plan's test-list parenthetical is corrected, and
   `worker_identity_changed_when_agent_process_is_not_an_ancestor` now covers
   the ancestry case, which had no test.
8. Temporary row and settlement types plus isolated SQL: **accepted** for the
   wave-1 integration. When T2's `settle_dispatch` replaces the local one, keep
   the `close_questions_for_dispatch` call after a failed `worker_done` (added
   in this review, spec 8.4) unless T2 does it. `worker_done_after_stop_is_inactive_dispatch`
   fakes the fence in SQL because T1's `dispatches::fence` is not on main; the
   integration review should switch it.
9. Removing the five message commands from T0's not-implemented assertions:
   **accepted** (the allowed deletion in `tests/t0.rs`).

Other defects fixed in this review:

- Terminal ownership is checked before duplicate or inactive handling, so a
  different pane cannot get `settled`/`duplicate: true` for another worker's
  Dispatch.
- Rejected lifecycle rows recorded the Dispatch's terminal as sender; they now
  record the calling terminal. Refused non-lifecycle sends (for example
  `caller_mismatch` on a `status`) no longer store a high-priority row.
- A caller inside herdr with no bound Run got `not_in_herdr`; it now gets
  `invalid_recipient` from `send` and a usage error from reads.
- Handle recipients resolved across all Runs; they now resolve within the
  sender's Run, and every row's `run_id` is the recipient mailbox's Run.
- A failed `worker_done` did not close the Dispatch's pending questions.
- A worker's bare `pod check` (what the nudge text tells it to run) consumed
  the coordinator's Run mailbox; it now takes the worker form.
- `check --terminal` worked outside herdr and could drain any worker's mailbox;
  it now needs the caller pane that owns that handle's active Dispatch.
- `check` on an unknown Run or Dispatch reported `store_access_denied`; it is
  now a usage error, as for any missing `--run`.
- `reply` worked outside herdr; it is now `not_in_herdr`, as documented.
- Unrouted questions stored `routed_to: null, copy_of: null`.

Not verified here: the Linux `/proc` walk (reviewed on macOS only); agents
running inside a PID namespace on Linux would see a chain that never reaches
the recorded agent PID and get `worker_identity_changed` (section 19 item 2
is still open); the CLI routing of a worker's bare `check` has engine tests
only, because the fake herdr cannot give a CLI subprocess a caller pane.
`--task-id` is not cross-checked against the Dispatch's Task; the spec is
silent.
