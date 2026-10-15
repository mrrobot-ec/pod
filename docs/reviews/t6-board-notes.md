# T6 board export: notes

Coordinator-approved deviations:

- `export()` reads with raw SQL over `tx.0`, so T6 does not depend on T1 row types. `sidebar_status` takes `board::contract::{TaskView, DispatchView}`; T7 maps T1 rows into them.
- `Cargo.toml`: one line added, `jsonschema = "0.28"` (default-features off) under `[dev-dependencies]`, for `export_validates_against_v3_schema`.

ASSUMPTION: entries (spec section 12 is silent):

- ASSUMPTION: the herdr-dagr v3 schema could not be obtained (dagr not installed). `tests/board/dagr-v3.schema.json` is written from the section 12 table and marked ASSUMPTION at the top. T13 replaces it; field names, enums, and the top-level layout may change.
- ASSUMPTION: top level is `{version: 3, generated_at, run, projects, tasks, events}`; attempts nest under `tasks[].attempts`. `generated_at` is the `now` argument.
- ASSUMPTION: `run.orchestrator.pane` is `runs.coordinator_terminal_id` (runs has no pane column) and `agent` is `coordinator_handle`.
- ASSUMPTION: `run.title` is `runs.objective`. Task `title` is `title`, else `display_name`, else the task ID.
- ASSUMPTION: task `kind` is `task`, or `gate` while the task has a pending gate. A resolved gate reverts the kind to `task`. With several pending gates on a task, the latest one is exported.
- ASSUMPTION: a gate with a task exports `state: blocked` and `unblock` equal to the audience (`user` or `coordinator`). `inputs` is `{question, options, recommendation, evidence}`, omitting absent fields. Gate `options` parse as a JSON array, falling back to comma-split.
- ASSUMPTION: `projects[]` has one `{id, title}` per distinct `--parent`, using the parent task's ID and title. `task.project` is the parent task ID. Parents remain ordinary tasks too.
- ASSUMPTION: attempt `n` is the 1-based position of the Dispatch among the Task's Dispatches ordered by `created_at`.
- ASSUMPTION: `cause.kind` is `initial` without `--retry-of`. With it, `followup` when the replaced Dispatch is `completed`, otherwise `sent_back`; `ref` is the replaced Dispatch ID.
- ASSUMPTION: attempt states are `queued` (pending), `working` (dispatched), `done`, `failed` (also `circuit_broken`), and `lost`. `lost` applies only to an active Dispatch whose latest observation is `exited`; a recorded ending always keeps its outcome.
- ASSUMPTION: `outcome.evidence` is `reported` when the Dispatch has a `termination_reason` (a `worker_done`), else `recorded` (gate completion, failed start, fencing without a stored reason). `reason` and `target` copy the termination fields.
- ASSUMPTION: `model` comes from `launch_effective.model`, else `launch_requested.model`. `locator` is the worker `pane_id`, else the assignee terminal ID. `actor` is `assignee_handle`.
- ASSUMPTION: `progress` is `{note: last_phase, at: last_heartbeat_at}`. `liveness` is the latest observation's liveness, and is omitted for an ended attempt with no observation.
- ASSUMPTION: a pending question of an active Dispatch makes its task `blocked`. `unblock` is `coordinator`, unless a `question` message in the same thread (or the question message itself) sits in a `dispatch:<id>` mailbox, in which case it is that Dispatch's `assignee_handle` (the dispatch ID if no handle).
- ASSUMPTION: `events[]` is `{seq, at, type, task, attempt, actor, detail}` from the `events` table; `detail` is parsed as JSON when it is valid JSON, else kept as a string.
- ASSUMPTION: a missing Run returns `task_not_found` with message `run <id> not found`, because `errors.md` has no run-specific code and I may not add one.
- ASSUMPTION: `sidebar_status` is `<display_name | title | id> · <last_phase>`, falling back to `starting` (pending), `working` (dispatched), or the dispatch status when there is no phase.
- ASSUMPTION: `write` creates `<board_dir>/<run>/` as `0700` and a `.run.json.<pid>.tmp` file as `0600`, then renames it over `run.json`. It does not fsync.

Not done here: the `Engine::after_commit(run)` hook belongs to T2 (it adds the one-line call); T6 only supplies `export` and `write`. `tests/board/fixtures/` holds no files; tests seed the store directly.

`export_500_tasks_under_30ms` is `#[ignore]` in debug builds; run `cargo test --release --test board_export`. Release timing was well under the budget (suite finished in 0.06 s).

## Review (Opus)

Reviewed `git diff main...t6-board` against spec sections 5, 9.1, 12, 16, `docs/reference/` (errors, commands, state-machines, config) and the T6 test list. All nine listed tests are present under their plan names. Fixes are in separate commits on `t6-board`.

Coordinator-approved deviations: accepted. The `sidebar_status` signature (`TaskView`/`DispatchView`) and `export`'s `now: DateTime<Utc>` are now recorded in the plan's T6 section. The `jsonschema` dev-dependency is accepted.

Verdicts per ASSUMPTION:

- Schema not obtainable: accepted. A web search on 2026-10-12 also found no published herdr-dagr v3 schema. T13 replaces the file and runs `dagr check --strict` (spec 17 item 5).
- Top-level layout `{version, generated_at, run, projects, tasks, events}`: accepted.
- `run.orchestrator.pane` is `coordinator_terminal_id`, `agent` is `coordinator_handle`: accepted.
- `run.title` is the objective; task title falls back to `display_name`, then the ID: accepted.
- `kind: gate` only while a gate is pending; latest pending gate wins: accepted.
- Gate exports `blocked` with `unblock` = audience and `inputs`; options are a JSON array with a comma-split fallback: accepted.
- `projects[]` from distinct parents: accepted.
- Attempt `n` is the 1-based order by `created_at`: accepted.
- `cause` is `initial`, `followup` (replaced Dispatch completed), or `sent_back`: accepted.
- Attempt states, and `lost` only for an active Dispatch whose latest observation is `exited`: accepted.
- `outcome.evidence` is `reported` when `termination_reason` is set: **corrected**. `worker-stop`/`worker-abandon` store `canceled` (state-machines D8) without any worker report, so a fenced Dispatch was shown as `reported`. `evidence` is now `reported` only when an accepted `worker_done` message (`rejected_reason IS NULL`) has `payload.dispatch_id` equal to the Dispatch, which matches the payload in commands-messages.md. New test: `fenced_or_gated_ending_is_recorded_not_reported`.
- `model`, `locator`, `actor` sources: accepted.
- `progress` and `liveness`: accepted. The `liveness` expression was simplified to the latest observation, which behaves the same.
- Pending question makes the task `blocked`; `unblock` is the specialist's handle when routed: accepted.
- `events[]` shape, with `detail` parsed as JSON: accepted.
- Missing Run returns `task_not_found`: **corrected**. commands.md and errors.md (line 66) say that a missing Run named by `--run` is a usage error, so it now returns `usage` (exit 2). New test: `missing_run_is_usage_error`.
- `sidebar_status` format and fallbacks: accepted.
- `write` creates the dir `0700` and a `.run.json.<pid>.tmp` file `0600`, then renames, with no fsync: **corrected** in part. The temp name was the same for every writer in a process, so concurrent writers raced, and one rename failed with ENOENT (the new test failed on the old code). The name is now `<pid>.<sequence>`. A stale temp file kept its old mode through `open`, so it is now removed and created with `create_new`. An existing run directory is now forced to `0700`, and a failed write removes its temp file. No fsync is still accepted: `run.json` is a derived view, rewritten after every commit. New test: `write_tightens_existing_run_dir_and_concurrent_writers_never_tear`.

`pod board` handler: main's plan (changed after this branch was cut) gives T6 the `board` handler in `src/cli/commands/env.rs`. The worker had left it as the `not_implemented` stub. On the coordinator's instruction it is now a partial handler. It exports and atomically writes `run.json`, detects `dagr` on `PATH` without running it, and prints the commands.md shape with `opened: false` and the install hint when dagr is absent. Its `board` line is removed from the t0 `not_implemented` test. Tests: `board_command_writes_run_json_and_reports_dagr`, `board_command_without_run_or_with_unknown_run_is_usage`.

- ASSUMPTION: `pod board` requires `--run` until T2 supplies the bound-Run default, which removes that requirement. T13 wires the step that opens the dagr pane with `DAGR_RUN`, so until then `opened` is always `false`.

Not covered or not verified: dagr itself (not installed), the `Engine::after_commit` hook (T2), and the herdr sidebar write (T7). Export scales with the never-pruned `events` and `observations` tables. With 500 tasks, 10,000 events and 10,000 observations it measured 13 ms in release on macOS arm64, inside the 30 ms budget. Linux was not measured. Attempt `started_at` falls back to `created_at` for a queued attempt. That is harmless, but T13 should check it against the real contract.
