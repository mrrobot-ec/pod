# pod v1 Implementation Plan, part 2 of 3

Wave 1 tasks T2 to T6, T11, and T12. The goal, Global Constraints, execution model, file structure, T0, and T1 are in [part 1](2026-10-12-implementation-plan.md); wave 2 and 3 tasks are in [part 3](2026-10-12-implementation-plan-part-3.md).

### T2: Runs, tasks, gates, receipts, reset, log (sol, wave 1, branch `t2-tasks`)

Spec sections: 3.4 (run binding), 6, 7.5 (breaker arithmetic only), 9, 9.1 (user gate validation and `decision_gate` message), 13 (`reset`).

**Files:** `src/engine/{receipts,runs,tasks,gates}.rs`, `src/cli/commands/{runs,tasks,gates,recovery}.rs`, `src/cli/commands/env.rs` (`log`, and the bound `run` field of `status`), `tests/engine_tasks.rs`, `tests/engine_gates.rs`, `tests/engine_receipts.rs`, `tests/cli_runs_tasks.rs`.

**Interfaces produced:**

```rust
// receipts.rs: wraps every mutation
pub fn with_receipt<T: Serialize + DeserializeOwned>(engine: &Engine, request_id: Option<&RequestId>, method: &str, payload_hash: &str, f: impl FnOnce(&Tx) -> Result<T>) -> Result<T>;
// tasks.rs
pub enum Outcome { Succeeded, Failed }
pub struct Settlement { pub task_status: String, pub dispatch_status: String, pub failure_count: u32, pub promoted: Vec<TaskId> }
pub fn settle_dispatch(tx: &Tx, now: DateTime<Utc>, dispatch: &DispatchId, outcome: Outcome, reason: &str, target: Option<&str>) -> Result<Settlement>;
pub fn create(tx: &Tx, now, run: &RunId, spec: &str, title: Option<&str>, display: Option<&str>, deps: &[TaskId], parent: Option<&TaskId>, creator: &str) -> Result<TaskRow>;
pub fn promote_dependants(tx: &Tx, now, completed: &TaskId) -> Result<Vec<TaskId>>;
pub fn update_status(tx: &Tx, now, task: &TaskId, status: &str, result: Option<Value>, worker_live: bool) -> Result<TaskRow>;
pub fn startable(tx: &Tx, task: &TaskId, retry_of: Option<&DispatchId>) -> Result<()>; // task_not_found, task_not_startable with status + unmetDependencies
// gates.rs
pub fn create(tx, now, run, audience: &str, task: Option<&TaskId>, question, options: &[String], evidence: Option<&str>, recommendation: Option<&str>, worker_live: bool) -> Result<GateRow>;
pub fn resolve(tx, now, gate: &GateId, resolution: &str) -> Result<(GateRow, Option<MessageRow>)>; // decision_gate message for user gates
// runs.rs
pub fn create(tx, now, objective, caller: &Caller, home_cwd) -> Result<RunRow>;
pub fn use_run(tx, now, run, caller) -> Result<RunRow>;   // bumps consumer_generation, fences outstanding deliveries
pub fn bound_run(tx, caller) -> Result<Option<RunRow>>;
```

`settle_dispatch` is the single place that applies 7.5: `failure_count` cumulative per Task; failed below 3 → Task `ready`; at 3 → Dispatch `circuit_broken`, Task `failed`; `denied`/`canceled` on a failed outcome → Task `failed`, Dispatch `failed`, count unchanged; success → Task `completed`, dependants promoted. T3 and T7 call it; they never touch Task status directly.

**Interfaces added in the T2 review (2026-10-12); the signatures above are unchanged:**

```rust
// src/error.rs: errors.md `data` fields beside nextSteps (task_not_startable: status,
// unmetDependencies; task_guard_refused: guard; reset_requires_force: liveWorkers).
pub struct PodError { pub code, pub message, pub next_steps, pub data: serde_json::Map<String, Value> }
impl PodError { pub fn with_data(self, key: &str, value: impl Into<Value>) -> Self; }
// src/engine/mod.rs (coordinator-authorized): no-op hook for T6 board export and T8 wake.
impl Engine { pub fn after_commit(&self, run: &RunId, mailboxes: &[String]) -> Result<()>; }
// Wave 1 integration: the engine files hold no SQL; tasks::TaskRow, runs::RunRow,
// gates::{GateRow, MessageRow}, and the T3 row types are `pub use` of store::rows (T1).
// receipts::with_receipt_replay (also returns `replayed`) and receipts::payload_hash;
// tasks::{settlement_of, latest_closure, is_live_worker}; Outcome::parse.
// Read helpers: tasks::{get, list_by_run, failed_dependency, waiting_on_failed_dependency,
// has_live_worker}, gates::{get, list}, runs::get, receipts::show -> Option<ReceiptState>,
// cli::commands::env::{list_events, EventRow}, cli::commands::recovery::reset_engine.
```

`task_guard_refused` guards are `active_dispatch_required`, `dispatch_active`, `worker_live`. `gate-create` blocks only a `pending`, `ready`, or `dispatched` Task (T12) and marks the completed Dispatch's worker `reclaimable`; `gate-resolve` releases only a `blocked` Task with no other pending gate (T13, T19).

**Tests:**
- [ ] `task_created_ready_when_deps_completed_else_pending`
- [ ] `promotion_on_dependency_completion` and `failed_dependency_keeps_dependants_pending_with_waiting_attention`
- [ ] `breaker_counts_failed_reports_cumulatively_across_retries` (3 failures → `circuit_broken`, `failed`)
- [ ] `denied_and_canceled_fail_task_directly_without_increment`
- [ ] `task_update_guards` (dispatched needs active dispatch; completed refused while worker live)
- [ ] `gate_create_coordinator_requires_task` and `gate_refused_while_worker_live`
- [ ] `gate_create_user_requires_evidence_recommendation_options_and_recommendation_in_options`
- [ ] `gate_resolve_ready_only_when_deps_completed_else_pending`
- [ ] `gate_resolve_user_gate_stores_decision_gate_message`
- [ ] `untasked_gate_changes_no_task`
- [ ] `receipt_replay_returns_stored_receipt` and `receipt_replay_is_scoped_to_caller_fingerprint`: Review Focus 4
- [ ] `run_use_bumps_generation_and_fences_deliveries`
- [ ] `reset_refused_with_live_workers_unless_force` and `reset_all_requires_force`
- [ ] `log_lists_events_in_sequence`
- [ ] CLI: `run-create`, `task-create --deps`, `task-list --ready --brief`, `gate-*`, `request-show` states `completed|pending|absent`, `reset`: JSON shapes per `commands.md`

---

### T3: Messaging engine (sol, wave 1, branch `t3-messages`)

Spec sections: 3.4 (lifecycle identity), 8.1, 8.3 (non-waiting forms), 8.4 (store and reply; blocking wait is T8), 8.5, 9.1 (ask routing, escalation fields).

**Files:** `src/engine/{messages,deliveries,questions,groups}.rs`, `src/cli/commands/messages.rs`, `tests/engine_messages.rs`, `tests/engine_questions.rs`, `tests/cli_messages.rs`.

**Interfaces produced:**

```rust
pub struct SendInput { pub to: Option<String>, pub subject: String, pub body: Option<String>, pub r#type: String, pub priority: String, pub thread_id: Option<String>, pub payload: Option<Value>, pub task_id: Option<TaskId>, pub dispatch_id: Option<DispatchId>, pub outcome: Option<String>, pub reason: Option<String>, pub reason_target: Option<String>, pub files_modified: Vec<String>, pub report_path: Option<String>, pub phase: Option<String>, pub evidence: Option<String>, pub recommendation: Option<String>, pub from: Option<String> }
pub struct SendReceipt { pub message_ids: Vec<MessageId>, pub mailbox: String, pub settled: Option<Settlement>, pub duplicate: bool, pub warnings: Vec<String>, pub rejected_reason: Option<String> }
pub fn send(engine: &Engine, input: SendInput) -> Result<SendReceipt>;        // stores, validates identity, calls tasks::settle_dispatch for worker_done; returns Err for rejected lifecycle AFTER storing the rewritten status message
pub fn check_consume(engine, mailbox: &str) -> Result<Delivery>;              // oldest outstanding or new FIFO batch of 50
pub fn check_ack(engine, delivery: &DeliveryId) -> Result<Delivery>;
pub fn check_peek(engine, mailbox, types: &[String]) -> Result<Vec<MessageRow>>;
pub fn check_all(engine, mailbox, types) -> Result<Vec<MessageRow>>;
pub fn worker_check(engine, terminal: &str) -> Result<Vec<MessageRow>>;       // marks read; consumer_fenced
pub fn ask_store(engine, question, options, to: Option<&str>) -> Result<(MessageId, Option<MessageId>)>; // run copy id, specialist copy id
pub fn reply(engine, message: &MessageId, body: &str) -> Result<QuestionRow>;   // question_answered on second reply
pub fn close_questions_for_dispatch(tx, now, dispatch) -> Result<u32>;
pub fn resolve_group(tx, run, group: &str, sender_terminal) -> Result<Vec<DispatchId>>;
pub fn mailbox_for(engine, to: Option<&str>) -> Result<String>;               // run:<id> | dispatch:<id>; task: rejected
pub fn caller_worker_handle(engine) -> Result<Option<String>>;              // handle of the caller pane's active Dispatch; routes a worker's bare `check` to the worker form
```

Identity rule from 3.4: a lifecycle message is accepted only when `caller.terminal_id == dispatch.assignee_terminal_id` and `dispatch.assignee_pid` is an ancestor of `caller.pid` (walk `/proc` on Linux, libproc `proc_pidinfo(PROC_PIDT_SHORTBSDINFO)` on macOS, which unlike `kinfo_proc`'s full BSD info also reads root-owned ancestors such as `login`; when the chain cannot be read, herdr's pane process list may corroborate it, otherwise fallback to terminal ID alone with a warning, per section 19 item 2). A wrong terminal or a fenced Dispatch is `caller_fenced`; a matching terminal whose recorded agent is not an ancestor is `worker_identity_changed` (errors.md). `--from` must equal the resolved handle when both known. `--dispatch-capability` ignored.

**Tests:**
- [ ] `send_default_mailbox_is_run_for_active_worker` and `task_recipient_rejected`
- [ ] `worker_done_requires_reason_on_failed` (`closure_reason_required`), `target_required_for_handed_off_blocked_on_superseded`, `unknown_target_accepted_with_warning`
- [ ] `success_defaults_no_follow_on`
- [ ] `worker_done_duplicate_same_outcome_settled_duplicate_true` and `conflicting_outcome_inactive_dispatch`
- [ ] `worker_done_after_stop_is_inactive_dispatch`: Review Focus 2 (uses a dispatch fenced via `dispatches::fence` from T1)
- [ ] `rejected_lifecycle_stored_as_high_status_and_exits_1` (wrong terminal → `caller_fenced`; fenced → `caller_fenced`) and `worker_identity_changed_when_agent_process_is_not_an_ancestor`
- [ ] `heartbeat_updates_last_heartbeat_and_phase_only_for_active_dispatch`
- [ ] `escalation_requires_evidence_and_recommendation` and `escalation_stamps_dispatch_id`
- [ ] `check_fifo_batch_of_50_replays_until_ack` and `ack_then_next_batch`
- [ ] `peek_and_all_filter_by_type_and_do_not_consume`
- [ ] `worker_check_marks_read_and_consumer_fenced_after_generation_bump`
- [ ] `group_resolution_all_idle_kind_worktree_one_row_per_recipient_shared_thread` and `group_zero_recipients_error` and `lifecycle_cannot_target_group`
- [ ] `ask_to_specialist_copies_run_mailbox_same_thread_one_question_row`
- [ ] `ask_to_requires_live_other_worker_same_run_not_self` (`recipient_not_live`)
- [ ] `first_reply_wins_either_id` and `second_reply_question_answered`
- [ ] `questions_closed_when_dispatch_fenced`
- [ ] `priority_tag_never_reorders`
- [ ] CLI: `send` typed flags vs `--payload` exclusive; `inbox --terminal --full`; `check --peek --types`

---

### T4: herdr socket adapter (sol, wave 1, branch `t4-herdr`)

Spec sections: 3.1 (adapter is the only herdr caller), 3.2, 7.1 step 5, 8.2 (primitives only), 19 items 1, 2, 7.

**Files:** `src/herdr/socket.rs`, `src/herdr/fake.rs` (complete scripted behaviour), `tests/herdr_socket.rs` (runs only when `HERDR_SOCKET_PATH` is set and `POD_E2E=1`; otherwise `#[ignore]`), `docs/reference/herdr-adapter.md` (what each trait method maps to; under 400 lines).

**Interfaces produced:** `pub struct SocketHerdr; impl SocketHerdr { pub fn connect(socket_path: &Path) -> Result<Self>; } impl Herdr for SocketHerdr`. Also `pub fn default_socket_path() -> PathBuf` and `pub fn open_from_env(env) -> Result<Box<dyn Herdr>>` which returns `FakeHerdr` when `POD_FAKE_HERDR=1` (test builds only).

Wave-2 additive interface: T7 adds `Herdr::worktree_list`, mapped to protocol
22 `worktree.list`, so existing `branch:<name>` placement can resolve a listed
path without invoking git.

**Mapping (from `vendor/herdr/schema-22.json`, protocol 22):** `ping`, `pane.current`, `pane.get`, `pane.process_info`, `pane.read`, `pane.send_text`, `pane.send_keys`, `pane.close`, `tab.create`, `tab.close`, `workspace.get`, `workspace.report_metadata`, `worktree.create`, `worktree.open`, `agent.start`, `agent.get`, `agent.list`, `agent.prompt`, `agent.read`, `events.subscribe` (subscription events `pane.exited`, `pane.closed`, `pane.agent_status_changed`). Request framing and the response envelope come from the `request`, `success_response`, `error_response`, `subscription_event` schemas. Every herdr error maps to `PodError { code: herdr_error, message, next_steps: ["herdr status server"] }`; an unreachable socket maps to `herdr_unreachable`.

**Tests:**
- [ ] `fake_records_calls_and_scripts_agent_status`
- [ ] `fake_send_text_appends_to_screen_and_enter_submits`
- [ ] `fake_events_delivered_in_order_and_timeout_returns_none`
- [ ] `socket_framing_roundtrip` (a mock Unix socket server in the test answering `ping`)
- [ ] `socket_error_maps_to_herdr_error_with_next_steps`
- [ ] `socket_unreachable_maps_to_herdr_unreachable`
- [ ] `live_ping_and_pane_current` (`#[ignore]` unless `POD_E2E=1`)
- [ ] `live_agent_prompt_stalled_is_reported` (`#[ignore]`)

Deliverable also records, in `docs/reference/herdr-adapter.md`, the answers to section 19 items 1, 2, and 7 as observed on herdr 0.9.3, each marked `CONFIRMED:` or `FALLBACK:`.

---

### T5: Transcript readers (sonnet, wave 1, branch `t5-transcript`)

Spec sections: 11, 19 item 4.

**Files:** `src/transcript/{claude,codex,grok,cursor,screen}.rs`, `src/transcript/mod.rs` (registry body only), `tests/transcript_readers.rs`, `tests/transcript/samples/{claude,codex,grok,cursor}/…` (real files, secrets scrubbed, under 200 KB each), `docs/reference/transcript-formats.md` (discovery notes, under 400 lines).

**Interfaces produced:** `ClaudeReader`, `CodexReader`, `GrokReader`, `CursorReader`, `ScreenReader` implementing `Reader`; `reader_for("claude"|"codex"|"grok"|"cursor")`; cursor format `"<source-hash>:<byte offset>"` so `source_changed` is detectable; tool output trimmed to 4,000 characters with `clipping: true`.

**Discovery steps (do first):** run one short session of each agent on this machine, record where the file lands and its line format in `transcript-formats.md`, copy a scrubbed sample into `samples/`. Grok and Cursor are best-effort: if no session store is found, record `reader_unavailable` in the doc and make the reader return `Ok(None)` from `locate`.

**Tests:**
- [ ] `claude_locate_by_session_id_in_cwd_slug` and `claude_read_rows_roles_kinds_tools`
- [ ] `codex_locate_rollout_by_id` and `codex_read_rows`
- [ ] `read_pages_with_cursor_and_limit` and `cursor_from_other_source_is_source_changed`
- [ ] `large_tool_output_is_trimmed_with_clipping`
- [ ] `unproven_session_refuses_read` (`locate(None)` never picks by recency)
- [ ] `grok_and_cursor_tests_skip_when_no_sample`

---

### T6: Board export (sonnet, wave 1, branch `t6-board`)

Spec section: 12, 9.1 (gate export), 16 (30 ms for 500 tasks).

**Files:** `src/board/mod.rs`, `src/board/contract.rs` (dagr contract v3 types), `src/cli/commands/env.rs` (`board` handler only; added in the T0 review because no task owned the `board` command), `tests/board_export.rs`, `tests/board/fixtures/*.json`.

**Interfaces produced:** `pub fn export(tx: &Tx, run: &RunId, now: DateTime<Utc>) -> Result<serde_json::Value>` and `pub fn write(paths: &Paths, run: &RunId, value: &Value) -> Result<PathBuf>` (temp file plus rename, `0600`). `pub fn sidebar_status(task: &board::contract::TaskView, dispatch: &board::contract::DispatchView) -> String` (`T12 · implementing`); `TaskView {id, display_name, title}` and `DispatchView {status, last_phase}` keep T6 independent of T1's row types, and T7 maps `TaskRow`/`DispatchRow` into them. The engine calls `board::export` at the end of every committing `Store::tx` from T2 onward through a hook `Engine::after_commit(run, mailboxes)`, which T2 adds to `engine/mod.rs` as a one-line call. `mailboxes` names the mailboxes written by the transaction (or is empty). Wired in the wave 1 integration: `after_commit` runs `board::export` then `board::write`, and `board::remove(paths, run)` for a Run that no longer exists (`reset`); an export failure is reported on stderr and never fails the committed mutation.

dagr is not installed on this machine. Vendor the v3 contract as a JSON Schema in `tests/board/dagr-v3.schema.json`, obtained from the herdr-dagr repository README or release; if it cannot be obtained, write the schema from spec section 12's table and mark the file `ASSUMPTION:` at the top for T13 to replace.

**Tests:**
- [ ] `state_mapping_pending_ready_queued_dispatched_working_completed_done_failed_blocked`
- [ ] `attempt_cause_followup_or_sent_back_with_ref`
- [ ] `exited_without_ending_is_lost_recorded_ending_keeps_outcome`
- [ ] `pending_ask_blocks_task_with_unblock_coordinator_or_specialist`
- [ ] `user_gate_with_task_exports_inputs_untasked_gate_omitted`
- [ ] `heartbeat_phase_is_progress_note`
- [ ] `export_validates_against_v3_schema`
- [ ] `export_500_tasks_under_30ms` (release profile, `#[ignore]` in debug)
- [ ] `write_is_atomic_and_0600`

---

### T11: Orca contract samples (sonnet, wave 1, branch `t11-contract`)

Spec section: 17 item 2, 14.

**Files:** `tests/contract/samples/<command>.json` (one per command in section 6 that Orca also has), `tests/contract/mod.rs`, `tests/contract/allowed-differences.toml`, `docs/reference/contract-capture.md` (how each sample was captured; under 200 lines).

**Capture:** with Orca still installed, create a throwaway Orca Run named `pod-contract-capture`, run each command with `--json`, save the output verbatim, then `reset` that Run. Only read-only and self-contained mutating commands (`run-create`, `task-create`, `task-list`, `send`, `check --peek`, `inbox`, `gate-*`, `request-show`, `worker-list` with zero workers) are captured; worker launches are not. Sanitize absolute paths to `/Users/me`.

**Test harness:** `compare(sample: &Value, actual: &Value, allowed: &Allowed) -> Vec<Diff>` comparing field names and JSON types recursively; `allowed-differences.toml` lists the section 14 differences by command and JSON path. The harness is wired to one `#[test]` per sample that invokes `pod <command> --json` through `assert_cmd` with `POD_FAKE_HERDR=1`; tests for not-yet-implemented commands are `#[ignore]` with the reason `"T<n>"` and un-ignored by that task's reviewer.

**Tests:**
- [ ] `compare_reports_missing_field_and_type_mismatch`
- [ ] `allowed_difference_suppresses_diff`
- [ ] one test per sample

---

### T12: Packaging, manifest, skills, CI (sonnet, wave 1, branch `t12-packaging`)

Spec sections: 3.5, 18, 15 (install preview lists hooks).

**Files:** `herdr-plugin.toml`, `scripts/install.sh`, `skills/coordinator/SKILL.md`, `skills/worker/SKILL.md`, `src/skills.rs`, `src/cli/commands/env.rs` (`skills get` only), `.github/workflows/ci.yml`, `.github/workflows/release.yml` (tagged releases: macOS arm64 and x86-64, Linux x86-64 and arm64, sha256 checksums), `tests/cli_skills.rs`, `tests/plugin_manifest.rs`.

Workflows are committed but never pushed; this repository has no remote until the owner creates one.

**Interfaces produced:** `pub fn get(role: &str, reference: Option<&str>, full: bool) -> Result<String>` returning `docs/guides/<role>.md` via `include_str!`, with `--reference <name>` returning the matching `docs/reference/<name>.md` and `--full` concatenating guide plus every reference the guide links.

`herdr-plugin.toml` fields exactly as in spec 3.5: `[[build]]` → `scripts/install.sh`; `[[startup]]` → `pod hook startup`; `[[events]]` for the three events → `pod hook event`; `[[actions]]` `open-board`, `worker-list`, `doctor`; `min_herdr_version = "0.9.3"`; `platforms`. Confirm the manifest key names against `https://herdr.dev/llms.txt` (fetch it, quote the relevant lines in a comment at the top of the file). `install.sh` verifies the checksum, or builds with `cargo build --release` when no release matches, then links `~/.local/bin/pod`.

**Tests:**
- [ ] `skills_get_coordinator_and_worker_match_docs_guides` (byte equality with the files)
- [ ] `skills_get_reference_and_full`
- [ ] `plugin_manifest_parses_and_lists_three_events_and_three_actions`
- [ ] `install_script_shellcheck_clean` (skip when shellcheck absent)
- [ ] `herdr_plugin_link_dry_run` (`#[ignore]` unless `POD_E2E=1`; `herdr plugin link <repo> --disabled` then `herdr plugin list` shows pod, then `unlink`)

---

