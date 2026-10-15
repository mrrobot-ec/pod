# T1 store notes

ASSUMPTION: none. Every point where the specification was silent was sent to
the coordinator through `orca orchestration ask`; the decisions are recorded
below for downstream tasks.

## Coordinator decisions

- Row identifiers and foreign keys use the newtypes from `src/ids.rs`.
- Timestamps use `DateTime<Utc>` and JSON columns use `serde_json::Value`.
- Generic `list(tx, limit)` queries use newest-first `created_at`, then `rowid`.
- `wake_attempts::count_since_last_consume` recognizes an event whose type is
  `mailbox.consumed` and whose detail is `{"mailbox":"...","delivery_id":null,
  "consumer":"..."}`. T3 emits it; T8 reads it.
- `notify = "7"` was authorized in `Cargo.toml` because T0 omitted the planned
  dependency. `signal::wait` uses a filesystem watch plus a one-second content
  version poll.

## Setter inventory

- `runs`: `update_objective`, `update_coordinator_terminal_id`,
  `update_coordinator_handle`, `update_consumer_generation`, `update_home_cwd`,
  `update_depth`, `update_updated_at`.
- `tasks`: `update_title`, `update_display_name`, `update_spec`, `update_status`,
  `update_deps`, `update_result`, `update_completed_at`.
- `dispatches`: `update_status`, `update_failure_count`, `update_last_failure`,
  `update_termination_reason`, `update_termination_target`,
  `update_assignee_handle`, `update_assignee_terminal_id`, `update_assignee_pid`,
  `update_agent_kind`, `update_launch_requested`, `update_launch_effective`,
  `update_dispatched_at`, `update_last_heartbeat_at`, `update_last_phase`,
  `update_completed_at`; `fence` conditionally ends an active Dispatch.
- `workers`: `update_state`, `update_stage`, `update_placement`,
  `update_worktree_path`, `update_workspace_id`, `update_tab_id`,
  `update_pane_id`, `update_terminal_id`, `update_owns_pane`,
  `update_setup_state`, `update_setup_pane_id`, `update_preamble_input`,
  `update_effects`, `update_residual_resources`, `update_terminal_state`,
  `update_retained_reason`, `update_session_id`, `update_transcript_path`,
  `update_transcript_proof`, `update_last_error`, `update_updated_at`.
- `messages`: `update_read`, `update_delivered_at`, `update_type`,
  `update_priority`, `update_rejected_reason`.
- `deliveries`: `update_status`, `update_acknowledged_at`.
- `questions`: `update_status`, `update_answer_message_id`,
  `update_answer_body`, `update_answered_at`, `update_closed_at`.
- `gates`: `update_status`, `update_resolution`, `update_resolved_at`.
- `receipts`: `update_state`, `update_receipt`, `update_updated_at`.
- `worker_archives`, `observations`, `wake_attempts`, and `events` are append-only.

## Review (Opus)

The worker recorded no ASSUMPTION entries; verdicts below cover each
coordinator decision and the review's own findings.

### Verdicts

- Newtype IDs and foreign keys from `src/ids.rs`: **accepted**.
- `DateTime<Utc>` timestamps and `serde_json::Value` JSON columns: **accepted**.
  Every timestamp goes through `runs::timestamp` (RFC 3339, microseconds, `Z`),
  so string comparison in SQL orders correctly; keep it that way.
- Generic `list(tx, limit)` newest-first by `created_at`, then `rowid`:
  **accepted**.
- `mailbox.consumed` event shape for `count_since_last_consume`: **accepted**.
  Note for T3/T8: an attempt with exactly the same timestamp as the consume
  event is not counted (strict `>`). With a frozen `FakeClock` this can
  under-count; advance the clock between a consume and the next attempt.
- `notify = "7"` in `Cargo.toml`: **accepted** (planned dependency). The
  "one-second content version poll": **corrected**, see below.
- Setter inventory: **accepted**. Setters on an unknown key silently update
  zero rows; callers check existence with `get` first.

### Defects fixed

1. **Concurrent first open failed** (Review Focus 1). Two `Store::open` calls on
   a fresh file at once returned `store_access_denied: database is locked` (WAL
   switch before the busy handler, no retry) or `table runs already exists`
   (`user_version` read outside the migration transaction). `src/store/mod.rs`
   now sets the busy timeout first, retries the WAL switch and `BEGIN IMMEDIATE`
   (5 × 50 ms, as `Store::tx`), and reads `user_version` inside the write
   transaction. `store_concurrent_writers_both_commit` now opens both stores
   inside the racing threads; it failed 10/10 runs on the old code.
2. **`signal::wait` did not poll SQLite's `data_version`** (spec 3.2). It
   polled the signal file's bytes, so a commit whose touch was lost never woke
   the waiter. It now also polls `data_version` on the `pod.db` beside the
   signal file. `Store::tx` no longer returns an error for a committed
   transaction when the touch after it fails (that invited duplicate retries).
   Test: `signal_wait_wakes_on_commit_without_touch`.
3. **Lost wakeup between a waiter's query and its wait.** `wait` took its
   baseline on entry, so a commit in that gap became the baseline. Added
   `signal::snapshot(path) -> Snapshot` and `Snapshot::wait(deadline)`;
   `wait(path, deadline)` is `snapshot(path).wait(deadline)`. Plan T1
   interfaces updated. **T8 must snapshot before its store query.** Test:
   `signal_snapshot_counts_commit_before_wait`.
4. **File modes.** `touch`/`wait` created missing directories with the umask
   mode and the signal file before `chmod`; now `0700` / `0600` from creation.
   Test: `signal_touch_creates_private_file_and_dir`.

Added `dispatches_fence_ends_active_once_and_keeps_accepted_ending` because T3's
Review Focus 2 test depends on `fence`.

### Not verified / notes for the merge

- Linux x86-64 was not run (macOS arm64 only); the `notify` backend differs
  (inotify vs FSEvents).
- Every SQLite error, including constraint violations of the store invariants,
  maps to `store_access_denied` through T0's `From<rusqlite::Error>`. The engine
  must check `active_for_task` / `active_for_terminal` / `outstanding` before
  inserting, or a race surfaces as a misleading code.
- `Cargo.lock` conflicts with `t6-board` (both add dependencies); resolve by
  regenerating the lock file at merge. `main` merges cleanly.
