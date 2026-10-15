# pod v1 implementation notes

Running notes across waves. Branch-level reviews are in `docs/reviews/t*-notes.md`.

## Wave 1 integration (Opus)

Reviewed `main` after the merges of t1-store, t2-tasks, t3-messages, t4-herdr,
t5-transcript, t6-board, t11-contract, and t12-packaging, against the plan
(T0 to T6, T11, T12, Global Constraints) and every `t*-notes.md`. Commits:
`06e53b2` through `2858143` plus this file.

### Swapped

- **T2 engine on T1 rows.** `engine/{runs,tasks,gates,receipts}.rs` have no
  local row structs and no SQL. `runs::RunRow`, `tasks::TaskRow`,
  `tasks::DispatchRow`, `gates::{GateRow, MessageRow}` are `pub use` of
  `store::rows`, so every plan path still resolves.
- **T3 engine on T1 rows.** `engine/{messages,deliveries,questions,groups}.rs`
  likewise: the `sql` modules, `LocalSenderContext`, `LocalDispatchRow`,
  `LocalMessageRow`, and T3's copies of `MessageRow`, `QuestionRow`, and
  `Settlement` are gone (`grep 'struct Local\|mod sql\|tx\.0' src/engine` is
  empty).
- **Settler.** The private `Settler` trait and `SqlSettler` are deleted.
  `worker_done` settles through `tasks::settle_dispatch`, then calls
  `close_questions_for_dispatch` on a failed outcome. `settle_dispatch` is
  the only code that changes a Task's status on a worker report (the other
  writers are `task-update`, dependency promotion, and gates). The two
  former copies differed; T2's rules won: cumulative `failure_count` across
  the Task's Dispatches, the `dispatch.completed`/`dispatch.failed` event,
  and `Settlement` in camelCase. Worker rows now move `ready` to
  `succeeded`/`failed` and `active` to `reclaimable` only from those states
  (W5, W6, R2), where T2 overwrote them unconditionally and T3 did it a
  second time.
- **Store rows** gained the queries the engine needed (listed in the plan's
  T1 section). `TaskRow.deps` is `Vec<TaskId>`, `GateRow.options`
  `Vec<String>`, `DeliveryRow.message_ids` `Vec<MessageId>`, and
  `messages::insert` returns the row's `sequence`.
- **CLI.** `cli/commands/*` no longer hold SQL either (run counts, `reset`,
  `log`, closures). The board export (`src/board`) still reads with raw SQL:
  that is T6's coordinator-approved design, kept for its 30 ms budget.
- **Commit hook.** `Engine::after_commit` runs `board::export` then
  `board::write` (and `board::remove` for a Run that `reset` deleted). Every
  T2 and T3 mutation calls it, from the CLI handlers and from the `&Engine`
  T3 entry points. An export failure is printed on stderr and does not fail
  the committed mutation. `wake::after_commit_nudge` is T8's and is not
  wired.
- **Board command.** `pod board` defaults to the bound Run.

### Fixed (integration defects the branch reviews could not see)

1. **No `mailbox.consumed` event existed.** T1's wake ladder counts attempts
   since that event and the T1 notes say T3 emits it; T3 never did, so the
   ladder could never clear. Every consuming check (coordinator and worker
   form) now appends it with the agreed detail.
2. **Two timestamp formats in one store.** T2 and T3 wrote `to_rfc3339()`
   (`+00:00`, nanoseconds) beside T1's `...000000Z`, and the store orders
   and compares timestamps as strings (`count_since_last_consume`, the
   binding order of `runs.updated_at`). Everything now goes through the
   store's single format.
3. **T3 mutations had no receipts.** `send`, consuming `check`, `--ack`,
   `ask`, and `reply` ignored `--retry-request`. They now run under
   `with_receipt`; a refused lifecycle message replays as the same refusal
   without storing a second row.
4. **Caller identity in CLI tests** went through a T2-only
   `POD_FAKE_TERMINAL_ID` hook, so no T3 command could be driven through the
   CLI. `FakeHerdr::from_env` now seeds panes and agents
   (`POD_FAKE_PANES`, `POD_FAKE_AGENTS`), the caller resolves through
   `identity::resolve`, and the hook is removed. `tests/support` gains
   `TestEnv::pod`, `fake_caller_env`, and `with_fake_agent`.
5. **Read-only commands failed when herdr was down** (open item in the T2
   notes); they now resolve the caller best effort. A `--run` naming a
   missing Run is now a usage error everywhere (task-list and gate-list
   printed an empty list).
6. **`reset` left `run.json` behind** (T2 notes); the hook removes it.
7. **Flaky `signal_wait_wakes_on_touch_within_100ms`.** It failed in most
   full `cargo test` runs: creating the FSEvents watch took 110 to 160 ms
   under load. The watch is now built on a helper thread while the waiter
   polls the signal file every 25 ms.
8. **`worker_done_after_stop_is_inactive_dispatch`** now fences with T1's
   `dispatches::fence` instead of faking the fence in SQL (T3 notes).

Checked and consistent: one module writes the events of each action (only
`settle_dispatch` writes Dispatch endings), one type string per meaning, ID
newtypes across every module boundary (T3 had passed Runs and Dispatches as
`String`), and the board reflects every state T3 produces (a pending
question blocks the Task, a reply releases it, `worker_done` settles it),
pinned by `tests/integration_wave1.rs`.

### Contract samples

All 16 T2 and T3 samples are un-ignored and pass (12 T2, 4 T3; T6 and T12
commands have no Orca sample). Fixed in pod to match Orca: the
`mutation {requestId, replayed}` block on every mutating receipt, `count`
and `runId` on lists, full Run rows and `nextCursor` in `run-list`,
`request-show`'s `requestId`/`createdAt`/`updatedAt`/`interpretation`,
`send` and `reply` returning the stored message row, and full message rows
in `inbox`. The other 89 differences are allowed in
`allowed-differences.toml`, each with a one-line reason: section 14 rows
(identity fields, legacy, typed-pointer and delivery-contract bookkeeping,
escalation evidence) or pod's documented shape (`title`, `deps` and
`options` as arrays, `mailbox`, boolean `read`, object `payload`,
`--brief` dropping the spec, extra Run columns and counts, and the stored
`check` receipt inside `request-show`, which T8's samples compare). The
harness gained `as` and `seed_worker` setup steps; `reply`'s setup now
seeds the asking worker.

### Plan and doc text changed

- Plan: T1 records the added queries, row types, and signal polling; T2's
  "temporary row structs" note is replaced by the resolution; T6 and T8
  say what `after_commit` does now.
- `commands.md`, `commands-messages.md`: receipts as implemented (above),
  the `mutation` convention, the event types `log` prints, `board`'s
  default, `reset` removing board files, usage for unknown or missing
  `--run`. `contract-capture.md` and `herdr-adapter.md`: fake caller
  seeding. All reference pages are under 400 lines.

### Verified

On macOS arm64: `cargo build --all-targets`, `cargo test` (every crate),
`cargo test --release` plus `export_500_tasks_under_30ms` with
`--include-ignored`, `cargo clippy --all-targets -- -D warnings`, and
`cargo fmt --check`. Every test the plan names for T0 to T6, T11, and T12
exists and passes. **Linux:** clippy `-D warnings` and the full test suite
pass on Linux aarch64 (`rust:1.97.1` in Docker), which exercises the
`/proc` ancestry walk and the inotify watch. Linux x86-64 was not built:
there is no x86-64 Linux C toolchain here for the bundled SQLite.

### Not verified, and left for later waves

- No live herdr run (`POD_E2E=1`); T4's live tests stay ignored.
- Contract samples for the T7, T8, and T9 commands stay ignored, and the
  `run-use`, `task-update`, `reset`, and `dispatch --dry-run` captures from
  a throwaway Orca Run (T11 notes) are still owed.
- The `check` receipt (`delivery`, `messages`, `count`) is not yet compared
  with Orca's (`deliveryId`, `acknowledged`, `cancelled`, `connectionLost`);
  that happens when T8 un-ignores the `check.*` samples.
- For T8: `count_since_last_consume` is strict `>`, so with a frozen
  `FakeClock` an attempt in the same microsecond as the consume is not
  counted; advance the clock. `ask` returns at once until T8 blocks it.
- For T7: `worker-start` must create the worker row in `ready` for
  `settle_dispatch`'s state moves, and should call `Engine::after_commit`.
- `receipts::prune` now runs on every completed receipt; it is cheap at
  today's sizes, but T13's benches should cover it at 10,000 rows.

**Main is ready for wave 2.**

## Wave 2 integration (Opus)

Reviewed `main` after the merges of t7-lifecycle, t8-wake, and t9-worker-read
on the wave 1 tree, against the plan (T7, T8, T9, Global Constraints) and the
three branch notes. Resumed after a machine restart: `9b988c1` (item 1) was
the previous reviewer's; commits `dc62417` through `64354b7` plus this
section are this review's.

### Wired

- **Preamble, nudge, setup through safe input** (`9b988c1`, kept). T7's
  private `inject` keeps only the oversize pointer and calls
  `safe_input::inject_preamble`; the duplicate helper is gone. Every pane
  input goes through `safe_input`: preamble and nudge through
  `type_and_submit`, the repo setup argv through its sibling `type_command`
  (a fresh shell pane has no agent to prove idle; paste, confirm, Enter
  still hold). Nothing calls `agent_prompt` (only the adapter and the fake
  define it). The uncommitted edit left by the restart was right: a failed
  `dispatch --inject` ran the commit hook twice (`failed_start` runs it).
- **Wake on commit.** `Engine::after_commit` already called
  `wake::after_commit_nudge` for the mailboxes a mutation wrote (T8 review).
  No lifecycle mutation writes mail, so start, stop, abandon, retain,
  release, and dispatch pass `&[]`, which is correct; each runs the hook
  once. `lifecycle_mutations_write_each_event_once` pins one event per
  action (`dispatch.dispatched`, `dispatch.failed` for a fence,
  `dispatch.start_failed`, `worker.user_owned`), and
  `tests/integration_wave2.rs` drives the merged path: start, coordinator
  `send`, ladder attempt 1 delivered into the worker pane, worker `check`
  clears it, `worker_done` settles through `settle_dispatch`, release
  archives and closes, a failed start returns the Task to the queue, and
  the board validates against dagr v3 at each step.
- **Projection.** `worker-show` and `worker-list` come from
  `observe::project` (the stub is gone), with the latest observation and
  the worker mailbox's `wakeLadder`. `nextAction` is now Orca's
  `{kind, argv}`: `worker-show` for an exit with no recorded ending or an
  exhausted ladder, `worker-read` for a prompt waiting on a person,
  `worker-release` for a settled worker still holding a pane, else
  `none`. A stop projects `stopped` (it was `failed`). Every 7.6 row comes
  out of `lifecycle::show`/`list` in
  `show_and_list_project_every_7_6_row_from_herdr_events` (herdr events
  through `reconcile`, the clock for `stale`, stored attempts for
  `unreachable`).
- **User takeover: FALLBACK.** Protocol 22 has no pane-input event
  (`herdr-adapter.md` item 10), so no `HerdrEvent` variant was added.
  `lifecycle::pane_gone` marks a worker user-owned when its pane closed while
  pod was neither stopping nor releasing it, from the `pane_closed` hook,
  the startup reconcile, and release; retain marks it as before. Stop and
  release never close a user-owned pane and the worker stays `reclaimable`
  until released. Typing into a live pane without closing it is not
  detected.
- **Transcript proof.** A Claude start passes `--session-id <uuid>` (unless
  the preset's args pick a session) and records `pod_assigned`; herdr's
  session report wins as `herdr`. T7 wrote `herdr_session` and
  `herdr_session_path`, which `worker-read` never accepted. Reuse and
  `dispatch --to` carry the proof of the same agent process.
  `claude_start_assigns_session_and_worker_read_follows_the_proof_order`
  reads the transcript end to end on FakeHerdr through the CLI.
- **Contract samples.** The eight T7 samples are un-ignored; no sample is
  ignored now and all 35 pass. Fixed in pod to match Orca: `worker-start`
  gained `runId`, `lastError`, `timeoutMs`, a `setup` report (`requested`,
  `effective`, `source`, `hookFound`, `startupPolicy`, `state`), effects and
  residual resources as objects, and `nextCommands` (was `recovery` argv
  arrays); `worker-show` uses camelCase rows plus `task_id`; `worker-list`
  rows are flat (`dispatchId`, `workerState`, `terminalState`, ...) with
  `counts`, `page`, and `warnings`; worker receipts carry `dispatchId`,
  `state`, and `archive {source, status, bytes}`; `dispatch-show` adds the
  current `dispatch`. The rest (about 100 new entries, 252 in all) are
  section 14 rows or pod's documented shape, each with its reason. The
  harness gained `_fake_herdr` (`POD_FAKE_AGENT_START_ERROR`,
  `POD_FAKE_RENDER_SENT_TEXT=0`) so the failed and unknown starts are
  reproduced, not faked.

### Fixed (integration defects)

1. Separate `pod` processes' fakes numbered panes from 1, so two
   `worker-start`s in one store collided on `assignee_terminal_id`; a
   `from_env` fake now numbers from its PID (harness-only).
2. A started worker read `unverifiable` until a hook arrived: the start now
   records herdr's `present` observation of the agent it detected.
3. `worker-stop` closed the pane without archiving it, so a stopped worker
   had nothing left to read; stop archives the final screen first.
4. State machine W10 (`stop_unknown` to `stopped` once the pane is proven
   gone) had no implementation; `pane_gone` applies it.
5. Release marked a pane user-taken-over when pod's own earlier release had
   closed it (`release_unknown`); only stop was excluded.
6. A failed start computed residual resources for the receipt but never
   stored them in `workers.residual_resources`.
7. `mark_user_owned` stamped events with `Utc::now()` beside the engine
   clock; it takes `now`. A repeated retain no longer appends a second
   `worker.user_owned`.

Checked and consistent: only `settle_dispatch` settles a worker report (the
fence and failed starts are coordinator and pod endings, same detail
shape); release closes a tab only when pod created it (`owns_pane`, and an
initial tab only for a worktree pod created); read-only commands
(`worker-show`, `worker-list`, `worker-read`, `board`, `check --peek`,
`inbox`) never run the commit hook, so they never advance a ladder;
`ask_wait` watches its question row, not Deliveries, so a `check --wait`
consuming the reply cannot lose an answer; the board maps every Task and
Dispatch state the lifecycle produces (a fenced stop is `failed` with reason
`canceled`, a closed pane `lost`).

### Plan and doc text changed

- Plan: T7's `StartReceipt`, `show`, the T7/T8 ownership split
  (`mark_user_owned(tx, now, …)`, the FALLBACK and `pane_gone`), and a
  wave 2 line on receipts, session proof, start observation, and stop
  archiving; T8's `Projection.next_action` is `NextAction {kind, argv}`.
- `commands-workers.md`: every worker receipt as implemented, `nextAction`
  rules, the takeover fallback. `commands.md`: the lifecycle event types.
  `herdr-adapter.md`: item 10 (FALLBACK) and the fake's new variables.
  `contract-capture.md`: `_fake_herdr`, nothing ignored. Outside reference:
  `guides/coordinator.md`, `ops/troubleshooting.md`, and
  `examples/full-run.md` follow the new receipts. All reference pages are
  under 400 lines. The spec is unchanged; its "literal argv" for
  `nextAction` is `nextAction.argv`.

### Verified

On macOS arm64: `cargo build --all-targets`, `cargo test` (291 passed, 5
ignored: the live herdr tests and the release-only timing tests),
`cargo test --release` plus `export_500_tasks_under_30ms` and the new
`worker_list_100_workers_under_100ms` with `--include-ignored`,
`wake_latency_under_100ms` in release, `cargo clippy --all-targets -- -D
warnings`, and `cargo fmt --check`. Every test the plan names for T7 (20),
T8 (25), and T9 (4) exists and passes. **Linux aarch64** (`rust:1.97.1` in
Docker, final tree): clippy `-D warnings` clean, 291 tests pass, 5 ignored.
**Linux x86-64** (`rust:1.97.1 --platform linux/amd64`, qemu emulation on
this Mac): clippy `-D warnings` clean and the build passes; 290 of 291 tests
pass. The one failure is the debug-profile `wake_latency_under_100ms` (263 ms
under emulation); it passes in `--release` there. Emulation also exposed
`install_script_shellcheck_clean` failing where `shellcheck` is absent
(qemu reports a missing program as exit 127, not NotFound); the test now
skips on 127 too. Native x86-64 hardware was not available.

### Not verified, and left for later waves

- No live herdr run (`POD_E2E=1`): T4's live tests stay ignored, and the
  wake, takeover, and session-proof paths are proven only on FakeHerdr. For
  T10.
- Found by T10, being fixed on `t10-e2e`, deliberately not touched here:
  `safe_input` confirms a paste with `pane_read(…, 40)`, so a preamble over
  40 lines is pasted but never confirmed and Enter is withheld.
- For the coordinator: `worker-stop` on a `dispatch --to` worker closes that
  pre-existing agent pane (spec 7.4 "the exact supervised agent pane");
  release would leave it. And release's `tab_close` also closes any pane a
  person split into the worker's tab; herdr gives pod no cheap way to tell.
- For T13: T8's 3-second quiet check runs inside the mutating command that
  triggers a nudge, so such a command takes over 3 s against the 50 ms
  non-waiting budget; benches should cover it, and `worker-list` timing is
  measured in the engine only, not through the CLI.
- Orca's projection identity fields (`provider`, `stage`, `resource`,
  `evidence`) and `worker-list` row `resource` are allowed differences, not
  implemented.

## Deferred past v1

Recorded 2026-10-13 by the final review. Each was a spec rule pod 0.1.0 did not implement; the docs describe the shipped behaviour. Only item 1 is still open; items 2 to 5 were settled in 0.2.0 on 2026-10-14, as listed below.

1. **`start_unknown`** (spec 7.1, 13; state machines T11, D3). No path produces it: every `agent_start` or injection error fails the start at its stage, leaves a created pane `reclaimable`, and returns the Task to `ready` below the breaker; a stalled preamble is already `dispatched_unobserved`. The only uncertain case is a reply lost during `agent.start`, and treating it as `failed` leaves at worst a stray agent in a reclaimable pane that release closes. Revisit if the pilot shows one.

Shipped or settled in 0.2.0 (hardening plan, `docs/superpowers/plans/2026-10-14-hardening/`):

2. **The `guidance` and `interruption` attention categories** (7.6): shipped by lane A4. `guidance` is a question or escalation unanswered for five minutes; `interruption` is text a person typed, seen by the screen read before a prompt.
3. **The `=== BASE DRIFT ===` preamble section** (10): dropped, not built, by lane B3. Counting commits behind the base needs git, which pod does not run, so the section left spec section 10.
4. **Detecting a person typing into a live worker pane** (7.4): shipped by lane A4 without an event, because herdr protocol 22 has none (herdr-adapter item 10). The screen read before a prompt or paste finds typed text, leaves the pane alone, and records `interruption`. A pane closed outside pod, or retained, is still user-owned; typing alone is not.
5. **Sidebar `DECISION` status and a board card for an untasked user gate** (9.1, 12): shipped by lane A5. An untasked user gate is a `gate:<id>` question card, the sidebar leads with `DECISION`, and a new user gate or a newly stuck worker fires a herdr notification.

## Live findings 2026-10-13 (first real runs)

The first Run with real agents (ten Codex researchers started by a Claude coordinator, then a live worktree test on an isolated herdr session) found and fixed, in this order:

1. Every start returned `dispatched_unobserved`: Codex 0.160.1 scrolls and soft-wraps its composer, so the paste was never confirmed line by line. Fixed by whitespace-free comparison (`safe_input::rendered`, fixture `tests/fixtures/codex-0.160.1-scrolled-paste.txt`).
2. Every researcher stopped at a Codex approval prompt for its own `pod check`. Added `--yolo` (preset `yolo = true`); the guide now says every unattended worker takes it.
3. herdr reported those prompts as agent status `blocked` with no label, which pod showed as nothing. A bare `blocked` now counts as `needs_input` (`blocked`), and `check --wait` returns early with `wokeOn: attention` when a worker newly requires action.
4. `pod board` ran `dagr` without `view`, and the run file was written against a guessed schema (dagr 0.3.1 rejected `detail` as an object and `liveness` as a string). The export now follows herdr-dagr's `CONTRACT.md` and the real Run passed `dagr check --strict` with no findings; the board tests run that check whenever `dagr` is on PATH.
5. A Codex worker in a new worktree stopped at "Trust and continue". A `-c projects."<path>".trust_level` override did not clear it; the entry in `~/.codex/config.toml` (keyed on the repository root as well) did. `--yolo` launches now write it (`engine::trust`), and the preamble waits up to 20 s for an idle agent, since a Codex that answers a stray startup prompt first takes about 8 s.

Verified live after the fixes, on an isolated herdr session with a real Codex worker in a new child worktree: worktree and tab created, trust recorded, preamble `delivered`, the worker made the requested change in the worktree. Not verified there: the worker's `worker_done` out of a worktree, because herdr's `worktree.create` takes no environment and the isolated harness's state directory did not reach the new workspace, so the worker's `pod check` hit the default store and was fenced. In normal use every pane shares the default store; the current-folder leg with real workers was proven by the ten-researcher Run (nine dossiers accepted on the first pass). Claude Code's trust entry is written the same way but was not exercised live today.

## Final review (Opus)

Reviewed `main` at `243bc89` (T0 to T13 and both integration reviews merged) against the whole spec, this file, and every `t*-notes.md`. Commits `e927511` through `5055b8d` plus this section.

### Item 1, one decision per letter (all implemented)

- **a.** `Engine::after_commit` writes `<task> · <phase>` to each worker workspace's sidebar through `workspace_report_metadata` and clears it when the workspace's workers have ended; an unreachable herdr is skipped and a refusal is one stderr warning, never a failed mutation (`dee8a43`). `pod board` opens dagr only when a `dagr` executable is on `PATH`: a new focused `board` tab whose shell gets `DAGR_RUN` through `tab.create`'s `env`, with the dagr path typed by safe input, so herdr's shell runs it, not pod; a failure is a `warnings` entry. The `open-board` action, which has no caller pane, uses the focused pane's bound Run (`18c2f2e`). Proven live: the e2e happy path opens a stand-in dagr that records `DAGR_RUN`.
- **b.** `worker-stop` closes only a pane pod created; a `dispatch --to` pane stays and is listed `pane_not_created_by_pod` (`4399c21`).
- **c.** Release asks herdr for the tab's pane count (new `Herdr::tab_pane_count`, `tab.get`) and closes only the worker's pane when a person split another into the tab (`4399c21`).
- **d.** The 3 s rule is kept and proven from a screen snapshot (`screen-<pane>.quiet`, 0600) that pod records whenever herdr reports the recipient idle: a screen unchanged since a snapshot at least 3 s old is quiet with no sleep. Only herdr's hook process may still sleep out an unproven window; a command records `skipped` with the reason, and waiters leave the attempt to the hook (`cd0756c`, documented in `commands-messages.md`). The bench now times `send` on the nudge path. Consequence to watch in the pilot: mail sent within 3 s of the recipient going idle, or to a pane whose screen changed after herdr's last idle event, waits for the next eligible trigger (an idle hook, or a mutating command at least 60 s later). The e2e scripted agent now reports `working` and `idle` per turn as Claude and Codex do; it reported idle only before its preamble, which left the snapshot stale.
- **e.** An existing state root is forced to 0700 (`e927511`).
- **f.** The reference pages, guides, ops page, and example carry no task labels and no ASSUMPTION, FALLBACK, CONFIRMED, or TODO markers; each former assumption is stated as shipped behaviour (`11c3bf2`). The open items found there are under Deferred past v1 and Not verified.
- **g.** Timing tests are ignored in debug with the reason; the bench and the snapshot test assert latency in release only (`a58a427`).
- **h.** `worker-start` waits (within `--timeout-ms`, at most 10 s) for the new tab's shell to be in the foreground and retries herdr's "not an available shell", failing at stage `pane` otherwise; the e2e harness no longer retries. The Dispatch records the launched kind (`pi`, not `unknown`). Worker receipts use `worker-show`'s camelCase (`572a012`, `1a48128`).

### Also fixed

- `pilot.sh` had terra start sol from inside a worker, which the default `nested_depth_max = 1` refuses. It now runs spec 17 item 6: coordinator starts terra, then sol, then reuses terra's pane for the findings (`4227b34`).
- `herdr plugin link` does not run `[[build]]` (checked on a clean clone with isolated herdr config); the README now runs `scripts/install.sh` first, and shows the config that defines `terra` and `sol`, which are not built in. Both verified on the clean clone (`ea73b26`, `4227b34`).
- Spec 9: `worker-list` reports Tasks waiting on a failed dependency. Spec 8.2: an exhausted Run mailbox writes `mailbox.unreachable`, exported to the board (`d783ca6`).
- The plan is split into three files so every `docs/` file but the spec is at most 400 lines (`447b0d3`).

### Spec rules (item 2)

Every numbered rule in sections 3 to 13 was mapped to the test that pins it. 30 new tests in `tests/spec_rules.rs` cover the rules that had none. Still without a test, with the reason: the new-top-level base order (7.2; FakeHerdr does not record the base ref), `--unread` equal to a bare `check` (8.3; one code path), the 15 s `_keepalive` (8.3; would add 15 s to the suite), typing takeover (7.4; no herdr event), and the deferred items above.

### Results (macOS arm64, final tree)

All exit 0 on the final tree:

- `cargo build --all-targets`.
- `cargo test`: 340 passed, 0 failed, 14 ignored (7 e2e, 2 live-herdr adapter tests, the plugin-link dry run, and 4 timing tests that run in release).
- `cargo test --release`: 344 passed, 0 failed, 10 ignored (7 e2e, 2 live-herdr adapter tests, the plugin-link dry run); the timing tests (`wake_latency_under_100ms`, `worker_list_100_workers_under_100ms`, `export_500_tasks_under_30ms`, signal latency) and the bench's 50 ms assertions for `status`, `task-list`, `worker-list`, and `send` pass.
- `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check`: clean.
- `shellcheck scripts/e2e.sh scripts/install.sh scripts/pilot.sh tests/e2e/fake_agent.sh`: clean.
- `POD_E2E=1 cargo test --test plugin_manifest -- --ignored herdr_plugin_link_dry_run`: passes (temporary herdr config; the user's plugin registry is untouched).
- `POD_E2E=1 scripts/e2e.sh` against isolated named herdr 0.9.3 sessions (`pod-e2e-<pid>`, temporary `HOME` and `XDG_*`, inherited `HERDR_*` removed; the user's `default` session was never touched): 7 of 7 passed, three runs in a row, and once more in the final battery.

### Not verified

- Linux: not run in this review (the Docker daemon is not running on this Mac, and starting Docker Desktop was not done). No change here touches the `/proc` ancestry walk or the inotify watch, which wave 2 verified on Linux aarch64.
- Real agents: the quiet-window proof depends on herdr reporting a Claude or Codex agent idle after its final screen; proven with the scripted agent only. Also unverified live: herdr's `agent_session` report through installed integrations, the ancestry walk from a Codex sandbox shell (fallback: terminal ID alone), and Grok and Cursor readers on fresh sessions.
- The sidebar token's rendering (the call returns `ok`); a real herdr-dagr (only a stand-in; `dagr check --strict` never run, and `tests/board/dagr-v3.schema.json` is still written from spec 12).
- The release-download path of `install.sh` and the CI workflows (no remote); the two live-herdr adapter tests (`live_ping_and_pane_current`, `live_agent_prompt_stalled_is_reported`) were not rerun, though the e2e suite exercises the same socket calls.
- `store_access_denied` prints fixed per-kind sandbox advice (Claude, Codex), not the exact settings key of the installed version.
- Orca contract samples for `run-use`, `task-update`, `reset`, `dispatch --dry-run`, `ask`, `dispatch`, `worker-stop`, `worker-abandon`, `worker-retain` (a throwaway Orca Run is needed); seven samples are hand-authored.

### Live pilot

From a checkout, in a shell inside herdr:

```sh
cd ~/Documents/pod
POD_INSTALL_FROM_SOURCE=1 sh scripts/install.sh
herdr plugin link "$PWD" --enabled
mkdir -p ~/.config/pod     # then add the [presets.terra] and [presets.sol] blocks from the README
bash scripts/pilot.sh      # prints the coordinator commands and the checklist
```

Open a Claude session in a herdr pane at the root of a disposable repository and have it run the printed commands in order: `pod doctor`, `pod run-create`, `worker-start --agent terra`, `check --wait` and `--ack`, `worker-start --agent sol`, release sol, `worker-start --terminal <terra handle>` with sol's findings, release terra, then the reclaimable check, `log`, `worker-show`, and `board`, ticking the checklist.

**Verdict: go.** Every command in item 3 passes and the e2e suite drives the same path on a real herdr. Watch in the pilot: a nudge recorded `skipped` because the quiet window was not yet proven (the next trigger retries), a start failing at stage `pane` (a shell startup command in the foreground), and any `release_unknown`.
