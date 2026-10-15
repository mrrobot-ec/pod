# pod v1 Implementation Plan, part 3 of 3

Wave 2 and 3 tasks (T7 to T10, T13), the Opus review tasks, and the self-review notes. The goal, Global Constraints, and T0 to T1 are in [part 1](2026-10-12-implementation-plan.md); T2 to T6, T11, and T12 are in [part 2](2026-10-12-implementation-plan-part-2.md).

### T7: Worker lifecycle (sol, wave 2, branch `t7-lifecycle`)

Spec sections: 7.1 through 7.5, 10, 4 (launch flags), 6 (`worker-start`, `dispatch` flags).

**Files:** `src/engine/{placement,preamble,lifecycle}.rs`, `src/cli/commands/workers.rs` (all but `worker-read`), `tests/engine_lifecycle.rs`, `tests/engine_preamble.rs`, `tests/cli_workers.rs`.

**Interfaces consumed:** `tasks::startable`, `tasks::settle_dispatch`, `messages::close_questions_for_dispatch`, `Herdr` trait, `safe_input::inject_preamble` (T8; until T8 merges, T7 calls a private `fn inject(engine, …) -> Result<InputOutcome>` in `lifecycle.rs` built from `pane_send_text`, `pane_read`, and `pane_send_keys`, which T8's reviewer re-points to `safe_input`).

**Finding from the T4 review (binding):** herdr's `agent.prompt` sends the text and Enter atomically, so it can never implement the spec's paste-confirm-Enter procedure. Preamble injection and nudges use the pane primitives only: `pane_send_text` (never submits), `pane_read` to confirm the text rendered, then `pane_send_keys("Enter")`. `agent_prompt` is used only for `--wait` observation of a turn that is already started by the pane path, or not at all.

**Interfaces produced:**

```rust
pub struct StartInput { pub task: Option<TaskId>, pub spec: Option<String>, pub agent: Option<String>, pub terminal: Option<String>, pub worktree: Worktree, pub name: Option<String>, pub repo: Option<String>, pub base_branch: Option<String>, pub display_name: Option<String>, pub comment: Option<String>, pub setup: Setup, pub model: Option<String>, pub effort: Option<String>, pub task_title: Option<String>, pub deps: Vec<TaskId>, pub parent: Option<TaskId>, pub retry_of: Option<DispatchId>, pub timeout_ms: Option<u64>, pub run: Option<RunId> }
pub enum Worktree { Current, NewChild, NewTopLevel, Path(PathBuf), Branch(String) }
pub enum Setup { Run, Skip, Inherit }
pub struct StartReceipt { pub run_id: RunId, pub task_id: TaskId, pub dispatch_id: DispatchId, pub handle: String, pub state: String /* ready|failed|start_unknown */, pub stage: String, pub failed_stage: Option<String>, pub last_error: Option<String>, pub setup: SetupReport /* requested, effective, source, hook_found, startup_policy, state */, pub launch: LaunchReport /* requested, effective */, pub timeout_ms: u64, pub effects: Vec<Effect /* kind, action, id|state */>, pub residual_resources: Vec<Resource /* kind, role, action, id, surface */>, pub next_commands: Vec<String>, pub outcome: Option<String> /* dispatched_unobserved */ }
pub fn worker_start(engine: &Engine, input: StartInput) -> Result<StartReceipt>;
pub fn dispatch(engine, task: &TaskId, to: &str, inject: bool, dry_run: bool, return_preamble: bool) -> Result<DispatchReceipt>;
pub fn stop(engine, dispatch) -> Result<WorkerReceipt>;      // fences, records canceled, closes only the supervised pane
pub fn abandon(engine, dispatch) -> Result<WorkerReceipt>;   // fences, records canceled, touches nothing
pub fn retain(engine, dispatch, reason: &str) -> Result<WorkerReceipt>;
pub fn release(engine, dispatch) -> Result<WorkerReceipt>;   // archives final screen (every kind), closes tab only if pod created it, idempotent
pub fn show(engine, dispatch) -> Result<WorkerView>;         // dispatch, worker, latest observation, observe::project, wake_ladder
pub fn list(engine, run: Option<&RunId>, terminal_state: Option<&str>, limit: u32, cursor: Option<&str>) -> Result<WorkerList>; // newest first, 100 max, scope.source flag|bound|all
pub fn render_preamble(ctx: &PreambleContext) -> String;     // section 10 verbatim, optional blocks only when non-empty
pub fn handle_for(preset_or_kind: &str, existing: &[String]) -> String;  // <name>-<4 hex>, regex-valid, unique
```

Stage order and receipts exactly as 7.1. Wave 2 integration: receipts take the Orca contract's names (`StartReceipt` above; `WorkerReceipt` adds `dispatch_id`, `state`, and `archive {source, status, bytes}`), a Claude launch gets `--session-id` with `transcript_proof = 'pod_assigned'` (herdr's report wins as `'herdr'`), a start records herdr's `present` observation, and stop archives the final screen before closing the pane. Handle regex `[a-z][a-z0-9_-]{0,31}`. A start that fails after creating a pane leaves `terminal_state = reclaimable`; `start_unknown` sets the Task `blocked`. `--on` → `remote_unsupported`. Reuse via `--terminal` moves cleanup ownership. User-taken-over panes (herdr `pane.input` reports user input, or the user closed it) are never closed.

`branch:<name>` matches protocol-22 `worktree.list` rows whose branch is
`<name>` or `refs/heads/<name>`, then opens the listed path. A missing branch
is a usage error naming it.

**T7/T8 ownership split (coordinator decision):** migration 2 adds
`workers.user_owned` and `user_owned_reason`. T7 exposes
`lifecycle::mark_user_owned(tx, now, dispatch, reason)`, which sets both fields and
appends `worker.user_owned`; stop and release never close a marked pane. T7
calls it for retain and when release discovers a pane already closed outside
pod. T8 confirms whether protocol 22 exposes a pane-input ownership event and,
if it does, adds the adapter event mapping and calls this setter.
Wave 2 integration: protocol 22 has none (FALLBACK, `herdr-adapter.md` item
10), so `lifecycle::pane_gone(tx, now, dispatch)` marks a pane
closed while pod was neither stopping nor releasing it user-owned, and
moves a `stop_unknown` worker to `stopped` (W10), from the `pane_closed`
hook, the startup reconcile, and release.

**Tests:**
- [ ] `preflight_errors_task_not_found_not_startable_with_unmet_deps_depth_exceeded_terminal_busy`
- [ ] `placement_current_new_child_new_top_level_path_branch` (FakeHerdr call log asserted)
- [ ] `creation_flags_rejected_for_current_and_existing`
- [ ] `repo_required_when_not_in_git`
- [ ] `setup_wait_for_setup_blocks_prompt_until_exit_0_and_failure_fails_at_setup`
- [ ] `setup_start_immediately_reports_running`
- [ ] `agent_start_failure_leaves_reclaimable_pane_with_recovery`
- [ ] `prompt_stalled_is_dispatched_unobserved_never_resent`
- [ ] `preamble_oversize_uses_pointer_and_records_once`: Review Focus 3 (FakeHerdr rejects prompts over 8 KB)
- [ ] `preamble_matches_section_10_golden` (golden file `tests/engine_preamble_golden.txt`, including SPECIALISTS, DECISION, BASE DRIFT blocks)
- [ ] `handle_is_valid_and_unique`
- [ ] `reuse_terminal_moves_cleanup_ownership`
- [ ] `release_archives_screen_closes_only_pod_created_tab_and_is_idempotent`
- [ ] `release_leaves_user_taken_over_setup_and_coordinator_panes`
- [ ] `stop_records_canceled_closes_only_supervised_pane_and_does_not_overwrite_accepted_ending`
- [ ] `abandon_touches_no_process`
- [ ] `retry_of_requires_failed_or_blocked_and_never_inherits_placement`
- [ ] `dispatch_inject_failure_counts_toward_breaker` and `dispatch_dry_run_returns_preamble`
- [ ] `worker_list_pagination_scope_source_and_reclaimable_filter`
- [ ] CLI: receipt shapes per `commands-workers.md`, exit 0 only for `ready`

---

### T8: Waiting, wake ladder, safe input, hooks, projection (sol, wave 2, branch `t8-wake`)

Spec sections: 3.2, 7.6, 8.2, 8.3 (`--wait`), 8.4 (blocking), 13, 16 (wake < 100 ms).

**Files:** `src/engine/{safe_input,wake,observe,waiting,reconcile}.rs`, `src/cli/commands/env.rs` (`hook` only), `src/cli/commands/messages.rs` (`check --wait`, `ask` blocking paths only, marked regions), `tests/engine_wake.rs`, `tests/engine_observe.rs`, `tests/engine_waiting.rs`, `tests/engine_reconcile.rs`.

**Interfaces produced:**

```rust
// safe_input.rs
pub enum InputOutcome { Delivered, RenderedUnconfirmed, Skipped(String), Failed(String) }
pub fn type_and_submit(herdr: &dyn Herdr, pane_id: &str, text: &str) -> Result<InputOutcome>; // prove empty+ready, pane_send_text, pane_read to confirm visible, pane_send_keys Enter; never agent_prompt (T4 finding: agent.prompt submits atomically)
pub fn inject_preamble(engine, dispatch, text) -> Result<InputOutcome>;      // records workers.preamble_input once, workers.last_error on skip/fail
// wake.rs
pub fn after_commit_nudge(engine, mailbox: &str) -> Result<Option<WakeAttemptRow>>;  // rule 8.2 step 1-3, attempt 1
pub fn on_trigger(engine, mailbox, trigger: Trigger) -> Result<Option<WakeAttemptRow>>; // IdleHook | MutatingCommand | ReadOnlyCommand (never types); max 5, >= 60 s apart
pub fn ladder_position(tx, mailbox) -> Result<Ladder>;                      // {attempts: u8, exhausted: bool, last_at}
// observe.rs
pub struct Projection { pub liveness: String, pub activity: String, pub presence: Option<String>, pub needs_input: NeedsInput, pub attention: Attention /* categories, requires_action */, pub next_action: NextAction /* kind, argv */, pub outcome: String }
pub fn record_self_report(tx, now, dispatch, activity: &str) -> Result<()>;
pub fn record_herdr_state(tx, now, dispatch, status: AgentStatus, needs_input: &[String], presence: Option<&str>) -> Result<()>;
pub fn project(tx, now, dispatch) -> Result<Projection>;                     // table in 7.6, stale after 10 min, unreachable from ladder
// waiting.rs
pub fn check_wait(engine, types: &[String], timeout_ms: u64) -> Result<Delivery>;   // waiter_exists; keepalive every 15 s on stderr; signal file + events.subscribe
pub fn ask_wait(engine, message: &MessageId, timeout_ms: u64) -> Result<AskOutcome>; // Answered(body) | Timeout(message_id)
// reconcile.rs
pub fn startup(engine) -> Result<ReconcileReport>;   // {inspected, present, missing, unproven}: every active Dispatch vs existing panes; missing → presence absent → exited, never failed; no recorded pane or no detected agent → presence null
pub fn event(engine, e: HerdrEvent) -> Result<()>;   // updates observations, re-offers pending mail on idle via wake::on_trigger
```

`Engine::after_commit` (added by T2, wired to `board::export` and `board::write` in the wave 1 integration) also calls `wake::after_commit_nudge` for each mailbox the transaction wrote to. `ask --to` routing and `decision_gate` waking are already stored by T3 and T2; this task only adds the blocking and waking.

Kept in the T8 review: `Engine::after_commit(run, mailboxes)` names the mailboxes the transaction wrote to (callers pass `&[]` otherwise) and, after the first nudges, offers each of the Run's uncleared ladders one `MutatingCommand` retry. `check --wait` is the coordinator's Run-mailbox wait; a worker caller or `--terminal` is a usage error. The waiter slot, the wake in-flight guard, and preamble injection hold OS file locks (`waiting::StateLock`, crate-private) that die with their process. A wake refreshes the recipient's herdr agent state before checking the 15-second bound. `cli::commands::env::hook_event_from(name, json)` parses a hook payload (protocol-22 `EventData`), public for its test. The five T8 `check.*wait*` contract samples are un-ignored.

**Tests:**
- [ ] `nudge_skipped_unless_present_idle_fresh_quiet_no_outstanding_delivery_no_inflight`
- [ ] `nudge_text_matches_spec`
- [ ] `safe_input_pastes_confirms_then_enters` and `unconfirmed_paste_withholds_enter_and_later_attempts_skip_until_cleared`
- [ ] `never_bare_enter`
- [ ] `ladder_max_5_60s_apart_eligible_triggers_only_then_unreachable`
- [ ] `ladder_counts_skipped_and_failed` and `new_mail_does_not_restart_ladder` and `consuming_check_clears_ladder`
- [ ] `read_only_commands_never_type`
- [ ] `wake_ladder_survives_restart`: Review Focus 5 (new `Engine` over the same store continues at the stored count)
- [ ] `projection_table_7_6_every_row` (one assertion per row of the evidence table)
- [ ] `self_report_proves_activity_never_presence` and `herdr_state_proves_presence`
- [ ] `recorded_ending_beats_exit_observation` and `outcome_unknown_when_exited_without_ending`
- [ ] `unverifiable_never_authorizes_stop_abandon_retry_release`
- [ ] `stale_after_10_minutes_is_warning_only`
- [ ] `check_wait_single_waiter_waiter_exists` and `ask_does_not_occupy_waiter_slot`
- [ ] `wake_latency_under_100ms` (release profile)
- [ ] `ask_timeout_leaves_pending_and_resume_does_not_duplicate`
- [ ] `startup_reconcile_marks_missing_panes_exited_never_failed`
- [ ] `event_idle_reoffers_pending_mail`
- [ ] `store_access_denied_has_agent_specific_fix`

---

### T9: `worker-read` and release archive wiring (sonnet, wave 2, branch `t9-worker-read`)

Spec sections: 11, 7.4 (archive), 6 (`worker-read` flags).

**Files:** `src/cli/commands/workers.rs` (`worker-read` only, marked region), `src/engine/lifecycle.rs` (the `archive_final_screen` function only, marked region), `tests/cli_worker_read.rs`.

**Interfaces produced:** `pub fn worker_read(engine, dispatch, source: Source, limit, cursor) -> Result<ReadReceipt { source, rows, cursor, content_complete, clipping, warnings, fallback_reason: Option<String> }>` with `Source::{Auto, Transcript, Terminal}`; proof order from section 11; `fallbackReason` values `session_unproven | reader_unavailable | no_terminal`. `pub fn archive_final_screen(engine, dispatch) -> Result<()>` writes `worker_archives(kind='final_screen')` from `herdr.pane_read(…, 2000)`.

T9 review (2026-10-12): the engine function lives at `cli::commands::workers::read::worker_read` (with `Source`, `ReadReceipt`, and `Clipping { rows_trimmed }`), because the CLI handler `workers::worker_read` already owns the name. Stored session proof is `workers.transcript_proof`: `pod_assigned` (T7 writes it with `session_id` when it launches Claude with `--session-id`) or `herdr` (`archive_final_screen` copies herdr's session report there at release, when no proof is stored, so the transcript stays readable after the pane closes). Once the archive exists, `worker-read` reads it and the stored proof instead of the pane, which may no longer be this worker's.

**Tests:**
- [ ] `auto_prefers_proven_transcript_then_pod_assigned_session_then_screen_with_fallback_reason`
- [ ] `source_terminal_refused_when_no_pane`
- [ ] `cursor_pinned_to_source_source_changed`
- [ ] `archive_written_once_for_every_kind_and_readable_after_release`
- [ ] CLI receipt shape per `commands-workers.md`

---

### T10: End-to-end suite and pilot harness (sol, wave 3, branch `t10-e2e`)

Spec sections: 17 items 4 and 6, 3.2, 8.2.

**Files:** `tests/e2e.rs` (two-line test-target root; Cargo does not discover `tests/e2e/mod.rs` alone), `tests/e2e/mod.rs`, `tests/e2e/fake_agent.sh` (reads the preamble from stdin, runs the `ask`, `heartbeat`, `check`, `worker_done` commands with `pod`), `tests/e2e/session.rs` (starts `herdr --session pod-e2e-<pid>` headless, links the plugin, tears down), `scripts/e2e.sh`, `.github/workflows/ci.yml` (add the e2e job on macOS and Linux; T12 owns the rest of the file, coordinate through the reviewer).

The fake agent is registered with herdr as kind `pi` with a command alias in a test `config.toml` so detection works without a model.

**Tests (all `#[ignore]` unless `POD_E2E=1`):**
- [ ] `e2e_run_create_worker_start_ask_reply_heartbeat_done_release` (full happy path on a real pane, preamble pasted and confirmed before Enter)
- [ ] `e2e_nudge_delivered_to_idle_fake_agent_and_consumed_by_check`
- [ ] `e2e_pane_closed_by_user_becomes_exited_and_release_leaves_it`
- [ ] `e2e_herdr_restart_reconciles_and_ladder_count_persists`
- [ ] `e2e_two_workers_specialist_ask_first_reply_wins`
- [ ] `e2e_user_gate_wakes_check_wait_decision_gate`
- [ ] `e2e_runs_on_linux` (CI only; the job runs the same suite in a Linux container with herdr installed)

**Pilot harness:** `scripts/pilot.sh` prints the exact coordinator steps for the live pilot (one terra → sol task in a real repository) and the checklist to confirm afterwards: `worker-list` empty of reclaimable rows, `log` shows the expected event sequence, `run.json` validates.

---

### T13: Benches, doctor completeness, doc sync (sonnet, wave 3, branch `t13-finish`)

Spec sections: 16, 18 (`doctor` checks), 17 item 5, and every doc.

**Files:** `benches/commands.rs`, `src/doctor.rs` (integration check per configured agent kind via `herdr integration status`, since herdr 0.9.3 has no `integration list` (T13 review); sandbox write check per agent kind; herdr-dagr presence), `docs/reference/errors.md` (remove `not_implemented`), every file under `docs/` (sync to shipped behaviour, keep each under 400 lines), `CHANGELOG.md` (0.1.0 entry), `README.md` (install and quick start from the real `install.sh`).

**Tests:**
- [ ] `bench_non_waiting_command_under_50ms` (`status`, `task-list` with 100 tasks, `worker-list` with 100 workers, criterion, asserted in CI against the targets table)
- [ ] `doctor_reports_every_check_in_section_18` and `doctor_fix_hints_per_agent_kind`
- [ ] `docs_mention_no_removed_flag_or_code` (a test that greps `docs/` for every `ErrorCode` variant and every clap flag and fails on an unknown one, and the reverse: every variant and flag is documented)
- [ ] `no_not_implemented_left` (grep of `src/`)

---

## Opus review tasks

Created by the coordinator, one per finished branch, with `--worktree branch:<branch>`; and one integration review per wave on `main` with `--worktree current`. Each review Task spec is the reviewer brief above plus the task section and the diff summary (`git diff main...<branch> --stat`). The reviewer commits fixes on the branch and reports: tests added or changed, spec deviations found, anything it could not verify. The integration review additionally runs `cargo test --release -- --include-ignored` with `POD_E2E=1` where herdr is available, and un-ignores the T11 contract tests whose commands the wave delivered.

Review order inside a wave is arrival order. A branch whose review reports a defect it could not fix goes back to the original worker's terminal as a new Task with the reviewer's report as the spec.

---

## Self-review notes

**Spec coverage.** Sections 1 through 19 map to tasks as follows: 3 and 4 → T0; 5 → T0, T1; 6 → T0 (parsing), T2, T3, T7, T8, T9 (behaviour); 7.1 to 7.5 → T7; 7.6 → T8; 8.1, 8.3, 8.4 (store), 8.5 → T3; 8.2, 8.3 (`--wait`), 8.4 (blocking) → T8; 9 and 9.1 → T2 (gates, validation), T3 (ask routing); 10 → T7; 11 → T5, T9; 12 → T6; 13 → T2 (`reset`), T8 (reconcile, concurrent writers in T1); 14 → T11 allowed differences; 15 → T0 (file modes), T12 (install preview); 16 → T6, T8, T13; 17 → T1 to T13 tests, T10, T11; 18 → T12, T13; 19 → T4, T5, T7.

**Type consistency.** `Settlement`, `settle_dispatch`, `InputOutcome`, `Projection`, `Herdr`, `Reader`, `Page`, `TestEnv` are defined once (T0 or T2) and used by name in T3, T7, T8, T9. `Engine::after_commit` is introduced in T2 and extended in T6 and T8.

**Ownership conflicts resolved.** `src/cli/commands/env.rs` is shared by T0 (`doctor`, `status`), T2 (`log`), T8 (`hook`), T12 (`skills`), T6 (`board`); each adds one function in a marked region and merges in wave order. `src/cli/commands/workers.rs` is shared by T7 and T9 in marked regions. `.github/workflows/ci.yml` is shared by T12 and T10; T10 appends a job.
