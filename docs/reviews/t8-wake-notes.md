# T8 implementation notes

- CONFIRMED: official herdr event-hook documentation defines
  `HERDR_PLUGIN_EVENT` and `HERDR_PLUGIN_EVENT_JSON`; `pod hook event` consumes
  those values and safely falls back to startup reconciliation.
- Coordinator decision: safe input requires an idle agent observation no more
  than 15 seconds old and a recognized bare prompt; pane quietness is two
  identical last-20-line reads three seconds apart.
- Coordinator decision: Codex and Claude store-access errors include their
  agent-specific sandbox remediation text.
- ASSUMPTION: waiter ownership and wake-in-flight exclusion use mode-0600
  state-directory lock files so separate pod processes share the exclusion.
- ASSUMPTION: when an event hook has valid JSON but an unknown event shape,
  full reconciliation is safer than rejecting the hook invocation.
- ASSUMPTION: absent `check --wait --timeout-ms` uses the same 600,000 ms
  operational default as `ask`; the specification gives no separate default.
- Integration: removed T8's implemented hook forms from T0's stale
  `not_implemented` list and covered hook/check-wait/ask-resume parsing in the
  T8 waiting tests, as directed by the coordinator.
- ASSUMPTION: `ask` timeout exits 0 as a durable pending checkpoint; this
  follows the core spec's non-error wording and preserves contract setup use.

## Review (Opus)

Verdicts on the ASSUMPTION entries:

- Lock files for the waiter slot and wake in-flight exclusion: **corrected**.
  They were `create_new` files removed in `Drop`, so a `check --wait` killed
  by an agent's tool timeout or Ctrl-C left a file that refused every later
  wait with `waiter_exists` and suppressed Run-mailbox nudges for good (a
  killed wake did the same to its mailbox). `waiting::StateLock` now holds an
  `flock` that the OS releases with the process; the files stay (mode 0600)
  and only a held lock counts, including in `waiter_matches`. Preamble
  injection takes the same kind of lock per Dispatch.
- Unknown hook event shape falls back to full reconciliation: **accepted**,
  and extended to malformed JSON (it was a usage error). The parser itself
  was **corrected**: it read `status`/`needs_input`, but protocol 22's
  `EventData` carries `agent_status`/`state_labels`, so every hook event was
  `unknown` and no idle hook ever re-offered mail.
- `check --wait` default timeout 600,000 ms: **accepted**. A timeout too
  large for `Instant` no longer panics.
- `ask` timeout exits 0: **accepted**; `commands-messages.md` said exit 1
  (its own ASSUMPTION) and is corrected. Orca's `ask` timeout exits 0: the
  captured `reply.json` contract sample runs one as a setup step that must
  exit 0.

Other defects fixed:

- Safe input never typed into a real agent. On herdr 0.9.3 with Claude Code
  and Codex (isolated server, this review), the input line sits above a
  border and footer and an empty input shows placeholder text, so the
  last-non-blank-line test never passed; and Claude collapses a multi-line
  paste to `[Pasted text #1 +N lines]` (Codex over 1,000 characters to
  `[Pasted Content N chars]`), so no preamble could be confirmed. The
  coordinator's "recognised bare prompt" decision stands; recognition now
  finds the lowest marker line, accepts the placeholders, and confirms
  indented continuation lines or a size-matched paste summary
  (`herdr-adapter.md` item 9).
- Wakes used only stored observations. An idle worker produces no further
  events, so its evidence was always over 15 s old and every nudge was
  skipped until `unreachable`. A wake now records herdr's current agent
  state for the pane first.
- `EventStream::next(Duration::ZERO)` never reads the socket stream, so a
  waiter saw no events in production (only the fake returned them). It reads
  for 1 ms per pass, and a stream that fails to open or breaks degrades to
  waiting on the store instead of failing the wait.
- herdr `done` (turn finished, pane not yet looked at) was treated as not
  idle: never typed into and never a re-offer trigger.
- `check --wait` from a worker waited on its Dispatch mailbox as a
  replay-until-ack Delivery and took the Run's only waiter slot. It is now a
  usage error pointing at `check --terminal` and `ask`.
- Reconcile: a closed pane is `absent`; a worker with no recorded pane is no
  longer reported `absent` (it was projected `exited`, `failure`); an idle
  coordinator pane re-offers the Run mailbox; events go to the pane's active
  Dispatch.
- `stale` only warns about a running attempt; `timedOut` is true when a wait
  ends without a matching message; `pod hook` prints nothing on success, as
  `commands.md` says.
- The five T8 contract samples were still ignored; they are un-ignored with
  allowed differences for the documented Delivery receipt and the absent
  daemon connection flags. Appending those entries at the end of
  `allowed-differences.toml` conflicted with T9 on main, so they sit beside
  the other `check` entries.
- Tests the list names but did not exercise are filled in: every nudge
  precondition, every row of the 7.6 table, Review Focus 5 through attempt 5
  and `unreachable`, plus `inject_preamble`, which had no test.

New assumptions from this review:

- ASSUMPTION: an input line whose text has the placeholder's shape is empty
  (Claude `Try "…"`, Codex exactly `Ask Codex to do anything`). Text typed by
  a person that matches would be taken as empty; any other placeholder makes
  safe input skip.
- ASSUMPTION: herdr `done` is at the prompt like `idle`. Neither agent showed
  `done` in the capture; herdr's skill text says idle versus done comes from
  its seen-state.
- ASSUMPTION: a pane that exists with no detected agent has null presence,
  not `present` (the startup hook said `present`).

Left as they are, for the coordinator to weigh:

- The 3-second quiet check runs in the process that triggers a wake, so a
  mutating command that makes a nudge attempt takes over 3 s, and a waiter
  that re-offers mail on an idle event can wake up to 3 s late.
- One lock file per mailbox and Dispatch stays in the state directory.
  Removing a lock file while another process opens it would split the lock.
- `HerdrEvent` (T4) drops the event's `agent` field, so an agent that exits
  back to its shell without closing the pane stays `present` until the pane
  exits.
- The process-local "uncertain paste" guard in `type_and_submit` is keyed by
  the herdr object's address. That is reliable in one `pod` process, but in a
  test binary a dropped double's address can be reused, so tests use
  distinct pane IDs. Wakes persist the guard through `rendered_unconfirmed`
  rows.
- Two `disk I/O error` contract failures during this review came from the
  machine's disk filling up (about 2.8 GiB free), not from T8.
