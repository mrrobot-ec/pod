# pod v1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. In this project the "subagents" are Orca workers; see "Execution model" below.

**Goal:** Ship pod v1, a single Rust binary herdr plugin that replaces Orca's orchestration while keeping Orca's CLI contract, built by parallel worker fleets and reviewed by Opus.

**Architecture:** `cli` → `engine` → `store` (SQLite WAL), with the engine talking to herdr only through a `Herdr` trait so every rule is testable against a fake. One worker establishes the crate skeleton, traits, schema, and output envelope first; after that, workers own disjoint modules and files and work in their own git worktrees, so nothing conflicts at merge time. Opus reviews each branch before merge and the integrated tree after each wave.

**Tech Stack:** Rust 1.97 (edition 2024), `clap` 4 (derive), `rusqlite` 0.32 (bundled, `0600` files), `serde` + `serde_json` + `toml`, `thiserror`, `chrono` (RFC 3339 UTC), `notify` 7 (file watch), `sha2`, `uuid`, `tempfile` (tests), `assert_cmd` + `predicates` (CLI tests), `criterion` (benches). No async runtime; the socket client is blocking JSON over a Unix socket.

**Spec:** `docs/specs/2026-10-05-pod-design.md` (authoritative). Supporting references the implementer must read for their task: `docs/reference/commands.md`, `commands-workers.md`, `commands-messages.md`, `errors.md`, `state-machines.md`, `config.md`; `docs/guides/coordinator.md` and `worker.md`; `docs/examples/full-run.md`.

## Global Constraints

- Rust edition 2024, MSRV 1.97; builds on macOS arm64 and Linux x86-64 with no warnings under `cargo clippy --all-targets -- -D warnings`.
- `min_herdr_version = "0.9.3"`, `platforms = ["macos", "linux"]`. herdr socket protocol 22 (from `herdr api schema --json`); the vendored copy lives at `vendor/herdr/schema-22.json`.
- Every command accepts `--json`. Every mutating command accepts `--retry-request <id>`.
- Errors are `{"error": {"code", "message", "data": {"nextSteps"}}}` on stderr; exit 1 for refused or failed, 2 for usage. Error codes are exactly the ones in `docs/reference/errors.md`; a new code requires editing that file in the same change.
- Store: SQLite WAL, file mode `0600`, directories `0700`, `user_version` migrations that only add tables and columns.
- Paths: state `$XDG_STATE_HOME/pod/<session>/pod.db` (override `POD_STATE_DIR`), config `$XDG_CONFIG_HOME/pod/config.toml` (override `POD_CONFIG`), board `<state dir>/<session>/runs/<run_id>/run.json`.
- Worker handles match `[a-z][a-z0-9_-]{0,31}`. IDs are `run_`, `task_`, `ctx_` (dispatch), `msg_`, `dlv_`, `gate_`, `wake_`, `req_` followed by 12 lowercase hex characters.
- pod executes only herdr, configured agent executables, and configured repo setup argv. Never text from messages, specs, or transcripts. pod never removes, cleans, or commits in a worktree.
- Performance: any non-waiting command < 50 ms excluding herdr round trips; wake < 100 ms; `worker-list` for 100 workers < 100 ms; board export for 500 tasks < 30 ms.
- Licence MIT; NOTICE credits Orca and OpenRig (already written, do not change).
- Personal project. Git identity is the local one set in `.git/config`. Never run `gh`, never push, never add a remote.
- Workers do not post to Slack, GitHub, or any external channel. Workers never run `orca` or `pod` against the live Orca run except through the preamble commands.

## Review Focus

Inputs the spec implies but no single task's happy path exercises. Each line has a pinned test in the task named.

1. **Two `pod` processes commit to the same store at the same moment** (coordinator `send` while a hook fires). Expect both to succeed after bounded busy retries, never a `database is locked` error surfacing to the user. Test in T1 (`store_concurrent_writers_both_commit`).
2. **A worker sends `worker_done` after the coordinator already ran `worker-stop`** on it. Expect `inactive_dispatch`, the stop's `canceled` reason preserved, Task state unchanged. Test in T3 (`worker_done_after_stop_is_inactive_dispatch`).
3. **A preamble larger than herdr's prompt accepts** (section 19 item 3). Expect the file-pointer fallback, `preamble_input` recorded once, never a second paste. Test in T7 (`preamble_oversize_uses_pointer_and_records_once`).
4. **A `--retry-request` replay from a different terminal with the same request ID.** Expect a fresh mutation for the second caller, not the first caller's receipt. Test in T2 (`receipt_replay_is_scoped_to_caller_fingerprint`).
5. **herdr restarts while a wake ladder is at attempt 4.** Expect the startup hook to leave the count at 4 and the next eligible trigger to be attempt 5, then `unreachable`. Test in T8 (`wake_ladder_survives_restart`).

---

## Execution model

This plan is run as one Orca Run from this Claude session (coordinator). The owner has chosen the fleet:

| Role | Launch | Used for |
|---|---|---|
| **sol** | `--agent codex --model gpt-5.6-sol --effort high` | Engine, store, herdr adapter, lifecycle, waiting and wake, end-to-end suite. Anything with concurrency, identity, or state-machine rules. |
| **sonnet** | `--agent claude --model claude-sonnet-5-5 --effort medium` | Transcript readers, board export, skills and packaging, contract samples, benches, doc sync. Well-bounded work with a clear oracle. |
| **opus** (reviewer) | `--agent claude --model claude-opus-5-5 --effort high` | Reviews and fixes in place: one per finished branch, then one integration review per wave. |

Permission mode is the owner's new-agent-tab setting (bypass). Known Orca issue (2026-10-12): `worker-start --agent codex` can time out at `agent_readiness` even though the pane is live. Fallback: `task-create`, open a Codex pane by hand, `dispatch --task <id> --to <terminal> --inject`.

**Isolation.** Every worker runs in its own worktree: `--worktree new-top-level --name <branch> --repo path:~/Documents/pod --base-branch main --setup skip`. Branch names are the task IDs below (`t1-store`, …). Workers commit on their branch only. T0 is the exception: it runs with `--worktree current` because the repository has only docs until it lands.

**Waves and the merge gate.** A wave starts only when every branch it depends on is merged to `main`. Merge procedure, done by the coordinator after the Opus branch review reports done:

```bash
cd ~/Documents/pod
git merge --no-ff <branch> -m "merge: <branch>"
cargo build --all-targets && cargo test && cargo clippy --all-targets -- -D warnings
```

A failing merge build goes back to the branch's worker as a new Task on the same terminal. After the last branch of a wave merges, one Opus integration review runs on `main` with `--worktree current`.

| Wave | Tasks (parallel) | Depends on |
|---|---|---|
| 0 | T0 (sol) | nothing |
| 1 | T1 store (sol), T2 runs/tasks/gates/receipts (sol), T3 messaging (sol), T4 herdr adapter (sol), T5 transcript readers (sonnet), T6 board export (sonnet), T11 contract samples (sonnet), T12 packaging and skills (sonnet) | T0 |
| 2 | T7 worker lifecycle (sol), T8 waiting, wake, hooks, projection (sol), T9 `worker-read` and release archive (sonnet) | T1, T2, T3, T4 (T9 also T5) |
| 3 | T10 end-to-end suite and pilot harness (sol), T13 benches, doctor completeness, doc sync (sonnet) | all of wave 2 |
| 4 | Opus final review of `main`, then the live pilot (coordinator plus owner) | wave 3 |

Peak parallelism is eight workers in wave 1 (five sol-class would be heavy; the owner may cap at four sol plus four sonnet, which only delays T4 by one slot).

**Worker brief.** Every Task spec sent through Orca contains, verbatim: the task section below, the Global Constraints, the file ownership list, the test list, and this line: "Read the spec sections named in your task before writing code. Write the failing test first for each listed test, then the implementation, then run `cargo test` and `cargo clippy --all-targets -- -D warnings`, then commit on your branch with a conventional message. Report done with `--outcome succeeded` only when every listed test passes; otherwise `--outcome failed --reason blocked_on` with the exact failure in the body. Ask the coordinator with `ask` when the spec is silent; do not invent behaviour."

**Reviewer brief (Opus).** "Review branch `<branch>` against the spec sections named in task `<id>` and against the task's test list. Fix defects in place and commit. Refuse to merge, in your report, if any listed test is missing, if an error code not in `docs/reference/errors.md` is emitted, if a file outside the task's ownership list changed, or if clippy fails. Report what you changed and what you could not verify."

---

## File structure

Locked in T0. Later tasks create only the files listed under their own heading and modify only the files their heading names.

```
Cargo.toml                       package pod, bin pod, features: fake-herdr
rust-toolchain.toml              1.97.1
herdr-plugin.toml                herdr plugin manifest (T12); name required by herdr 0.9.3
scripts/install.sh               (T12)
skills/{coordinator,worker}/SKILL.md   stubs (T12)
vendor/herdr/schema-22.json      socket schema, reference only (T0)
src/lib.rs                       crate root: declares every module, re-exports Engine, Store, FakeHerdr, PodError, ErrorCode, Paths, Config, Caller, Clock
src/main.rs                      thin binary: calls pod::cli::run(), maps PodError to exit code
src/cli/mod.rs                   clap root, Command enum, run()
src/cli/output.rs                success envelope, error envelope, --json vs text
src/cli/commands/env.rs          doctor, status, skills, log, board, hook
src/cli/commands/runs.rs         run-create, run-use, run-current, run-list, run-show
src/cli/commands/tasks.rs        task-create, task-list, task-update
src/cli/commands/workers.rs      worker-* and dispatch, dispatch-show
src/cli/commands/messages.rs     send, check, ask, reply, inbox
src/cli/commands/gates.rs        gate-create, gate-resolve, gate-list
src/cli/commands/recovery.rs     request-show, reset
src/error.rs                     ErrorCode enum (one variant per errors.md row), PodError
src/ids.rs                       Id newtypes and generation
src/paths.rs                     Paths::resolve
src/config.rs                    Config, AgentResolution, launch flags
src/identity.rs                  Caller resolution
src/clock.rs                     Clock trait, SystemClock, FakeClock
src/store/mod.rs                 Store::open, Store::tx, busy retry
src/store/schema.rs              migrations
src/store/signal.rs              signal file touch and watch
src/store/rows/*.rs              one file per table: typed row + CRUD (T1)
src/engine/mod.rs                Engine struct and constructor
src/engine/receipts.rs           (T2)
src/engine/runs.rs               (T2)
src/engine/tasks.rs              (T2)
src/engine/gates.rs              (T2)
src/engine/messages.rs           (T3)
src/engine/deliveries.rs         (T3)
src/engine/questions.rs          (T3)
src/engine/groups.rs             (T3)
src/engine/placement.rs          (T7)
src/engine/preamble.rs           (T7)
src/engine/lifecycle.rs          (T7)
src/engine/safe_input.rs         (T8)
src/engine/wake.rs               (T8)
src/engine/observe.rs            (T8)
src/engine/waiting.rs            (T8)
src/engine/reconcile.rs          (T8)
src/herdr/mod.rs                 Herdr trait and types (T0)
src/herdr/socket.rs              real client (T4)
src/herdr/fake.rs                FakeHerdr with scripted responses (T0 skeleton, T4 completes)
src/transcript/mod.rs            Reader trait, Row, registry (T0 trait, T5 impl)
src/transcript/{claude,codex,grok,cursor,screen}.rs   (T5)
src/board/mod.rs                 export(), contract types (T6)
src/doctor.rs                    (T0 basic, T13 completes)
src/skills.rs                    include_str! of docs/guides (T12)
tests/support/mod.rs             TestEnv: temp XDG dirs, FakeHerdr, FakeClock, Engine (T0)
tests/engine_*.rs                engine tests per module
tests/cli_*.rs                   assert_cmd tests
tests/contract/samples/*.json    Orca captures (T11)
tests/contract/mod.rs            field name and type comparison (T11)
tests/transcript/samples/        real sample files (T5)
tests/e2e/                       real herdr, fake agent script (T10)
benches/commands.rs              (T13)
```

---

### T0: Skeleton, traits, schema, envelope (sol, wave 0, `--worktree current`)

Spec sections: 3, 3.3, 3.4, 4, 5, 6 (envelope only), 18 doctor list.

**Files:** everything under "File structure" marked T0, plus empty `mod` files for every later module so the tree compiles. Also the first commit of the docs that already exist.

**Interfaces produced (every later task depends on these exact names):**

```rust
// src/error.rs
pub enum ErrorCode { /* one variant per row of docs/reference/errors.md, snake_case serialized */ }
pub struct PodError { pub code: ErrorCode, pub message: String, pub next_steps: Vec<String> }
impl PodError { pub fn usage(msg: impl Into<String>) -> Self; pub fn exit_code(&self) -> i32; }
pub type Result<T> = std::result::Result<T, PodError>;

// src/ids.rs
pub struct RunId(String); pub struct TaskId(String); pub struct DispatchId(String);
pub struct MessageId(String); pub struct DeliveryId(String); pub struct GateId(String);
pub struct WakeId(String); pub struct RequestId(String);
pub fn new_id(prefix: &str) -> String;       // prefix + 12 lowercase hex

// src/clock.rs
pub trait Clock: Send + Sync { fn now(&self) -> chrono::DateTime<chrono::Utc>; }
pub struct SystemClock; pub struct FakeClock(pub std::sync::Mutex<chrono::DateTime<chrono::Utc>>);
impl FakeClock { pub fn advance(&self, d: std::time::Duration); }

// src/paths.rs
pub struct Paths { pub session: String, pub state_dir: PathBuf, pub config_path: PathBuf, pub db_path: PathBuf, pub board_dir: PathBuf, pub signal_path: PathBuf }
pub fn resolve(env: &dyn Fn(&str) -> Option<String>) -> Result<Paths>;

// src/identity.rs
pub struct Caller { pub terminal_id: String, pub pane_id: Option<String>, pub pid: u32, pub session: String }
impl Caller { pub fn fingerprint(&self) -> String; }    // sha256(terminal_id + "\n" + session), hex
pub fn resolve(env: &dyn Fn(&str) -> Option<String>, herdr: &dyn Herdr) -> Result<Option<Caller>>; // None when HERDR_ENV unset

// src/config.rs
pub struct Config { pub defaults: Defaults, pub agents: BTreeMap<String, AgentAlias>, pub presets: BTreeMap<String, Preset>, pub repos: BTreeMap<PathBuf, RepoConfig> }
pub struct Launch { pub kind: String, pub command: String, pub model: Option<String>, pub effort: Option<String>, pub args: Vec<String> }
impl Config { pub fn load(path: &Path) -> Result<Config>; pub fn resolve_agent(&self, name: &str, model: Option<&str>, effort: Option<&str>) -> Result<Launch>; }
pub fn launch_argv(l: &Launch) -> Vec<String>;   // codex: -m, -c model_reasoning_effort=; claude: --model, effort via args

// src/store/mod.rs
pub struct Store { /* rusqlite::Connection, signal path */ }
pub struct Tx<'a>(pub &'a rusqlite::Transaction<'a>);
impl Store {
    pub fn open(paths: &Paths) -> Result<Store>;                   // creates dirs 0700, file 0600, WAL, migrates
    pub fn tx<T>(&self, f: impl FnOnce(&Tx) -> Result<T>) -> Result<T>; // IMMEDIATE tx, busy retry 5x50ms, touches signal on commit
    pub fn read<T>(&self, f: impl FnOnce(&Tx) -> Result<T>) -> Result<T>;
    pub fn user_version(&self) -> Result<u32>;
    pub fn integrity_ok(&self) -> Result<bool>;
}

// src/herdr/mod.rs
pub trait Herdr: Send + Sync {
    fn ping(&self) -> Result<HerdrInfo>;                                   // {version, protocol}
    fn pane_current(&self) -> Result<PaneInfo>;
    fn pane_get(&self, pane_id: &str) -> Result<Option<PaneInfo>>;
    fn pane_process_info(&self, pane_id: &str) -> Result<ProcessInfo>;    // {shell_pid, foreground_pid, pids: Vec<u32>}
    fn pane_read(&self, pane_id: &str, lines: u32) -> Result<String>;
    fn pane_send_text(&self, pane_id: &str, text: &str) -> Result<()>;    // never submits
    fn pane_send_keys(&self, pane_id: &str, keys: &[&str]) -> Result<()>; // "Enter"
    fn pane_close(&self, pane_id: &str) -> Result<()>;
    fn tab_create(&self, workspace_id: &str, name: &str, cwd: &Path, focus: bool) -> Result<TabInfo>; // {tab_id, pane_id}
    fn tab_close(&self, tab_id: &str) -> Result<()>;
    fn workspace_get(&self, workspace_id: &str) -> Result<Option<WorkspaceInfo>>;
    fn workspace_report_metadata(&self, workspace_id: &str, status: &str) -> Result<()>;
    fn worktree_create(&self, req: &WorktreeCreate) -> Result<WorktreeInfo>; // {workspace_id, path, branch, tab_id, pane_id}
    fn worktree_list(&self) -> Result<Vec<WorktreeInfo>>;                    // wave-2 additive: protocol 22 worktree.list
    fn worktree_open(&self, path: &Path) -> Result<WorktreeInfo>;
    fn agent_start(&self, name: &str, kind: &str, pane_id: &str, argv: &[String], timeout_ms: u64) -> Result<AgentInfo>;
    fn agent_get(&self, name: &str) -> Result<Option<AgentInfo>>;         // {name, kind, pane_id, pid, status: AgentStatus, session_id, session_path, needs_input: Vec<String>, observed_at}
    fn agent_list(&self) -> Result<Vec<AgentInfo>>;
    fn agent_prompt(&self, name: &str, text: &str, wait_ms: Option<u64>) -> Result<PromptOutcome>; // Submitted | Stalled | Blocked
    fn agent_read(&self, name: &str, lines: u32) -> Result<String>;
    fn events_subscribe(&self, pane_ids: &[String]) -> Result<Box<dyn EventStream>>;
}
pub enum AgentStatus { Idle, Working, Blocked, Done, Unknown }
pub trait EventStream { fn next(&mut self, timeout: std::time::Duration) -> Result<Option<HerdrEvent>>; }
pub enum HerdrEvent { PaneExited { pane_id: String }, PaneClosed { pane_id: String }, AgentStatusChanged { pane_id: String, status: AgentStatus, needs_input: Vec<String> } }

// src/herdr/fake.rs  (feature fake-herdr, always on for tests)
pub struct FakeHerdr { /* Mutex<State> */ }
impl FakeHerdr { pub fn new() -> Self; pub fn add_pane(&self, pane_id: &str, terminal_id: &str, workspace_id: &str) ; pub fn set_agent(&self, name: &str, info: AgentInfo); pub fn push_event(&self, e: HerdrEvent); pub fn calls(&self) -> Vec<String>; pub fn screen(&self, pane_id: &str) -> String; pub fn set_screen(&self, pane_id: &str, text: &str); }

// src/transcript/mod.rs
pub struct Row { pub role: String, pub kind: String, pub text: String, pub tool: Option<String>, pub at: Option<String> }
pub struct Page { pub rows: Vec<Row>, pub cursor: Option<String>, pub content_complete: bool, pub clipping: bool, pub warnings: Vec<String> }
pub trait Reader { fn kind(&self) -> &'static str; fn locate(&self, session_id: &str, session_path: Option<&Path>, cwd: &Path) -> Result<Option<PathBuf>>; fn read(&self, source: &Path, cursor: Option<&str>, limit: u32) -> Result<Page>; }
pub fn reader_for(kind: &str) -> Option<Box<dyn Reader>>;

// src/engine/mod.rs
pub struct Engine { pub store: Store, pub herdr: Box<dyn Herdr>, pub clock: Box<dyn Clock>, pub config: Config, pub paths: Paths, pub caller: Option<Caller> }

// src/cli/output.rs
pub fn emit_ok<T: Serialize>(json: bool, value: &T) -> ();                 // {"ok":true,...fields of value} per docs/reference/commands.md
pub fn emit_err(json: bool, e: &PodError) -> i32;

// tests/support/mod.rs
pub struct TestEnv { pub engine: Engine, pub herdr: std::sync::Arc<FakeHerdr>, pub clock: std::sync::Arc<FakeClock>, pub tmp: tempfile::TempDir }
impl TestEnv { pub fn new() -> Self; pub fn as_caller(&mut self, terminal_id: &str, pid: u32) -> &mut Self; }
```

**Interfaces added in the T0 review (2026-10-12); later tasks code against these:**

```rust
// src/cli/mod.rs: every command dispatches to one handler in its owning commands/*.rs file.
// Later tasks replace their handler bodies and never edit cli/mod.rs.
pub struct Ctx { pub json: bool, pub retry_request: Option<String> }
impl Ctx {
    pub fn env(name: &str) -> Option<String>;            // empty values are unset
    pub fn paths(&self) -> Result<Paths>;
    pub fn herdr(&self) -> Result<Box<dyn Herdr>>;       // herdr::socket::open_from_env
    pub fn config(&self, paths: &Paths) -> Result<Config>; // POD_CONFIG naming a missing file is usage
    pub fn engine(&self) -> Result<Engine>;              // paths, config, store, herdr, SystemClock, caller
}
// handler shape, e.g. src/cli/commands/runs.rs:
pub fn run_create(ctx: &Ctx, args: RunCreateArgs) -> Result<()>;   // run_current(ctx) / run_list(ctx) take no args
// Handlers: env::{doctor,status,skills,log,board,hook}, runs::{run_create,run_use,run_current,run_list,run_show},
// tasks::{task_create,task_list,task_update}, gates::{gate_create,gate_resolve,gate_list},
// recovery::{request_show,reset}, messages::{send,check,ask,reply,inbox},
// workers::{worker_start,worker_show,worker_read,worker_list,worker_stop,worker_abandon,worker_retain,worker_release,dispatch,dispatch_show}

// src/error.rs
impl PodError { pub fn new(code: ErrorCode, message: impl Into<String>, next_steps: &[&str]) -> Self; }
impl ErrorCode { pub const ALL: &[ErrorCode]; }   // test error_codes_match_errors_md keeps it equal to errors.md
// Codes added for refusals whose owners cannot edit error.rs: herdr_unreachable, herdr_error (T4),
// terminal_busy, unknown_agent, launch_mapping_unsupported (T7, config), caller_mismatch,
// invalid_recipient, question_closed (T3), task_guard_refused, reset_requires_force (T2).

// src/paths.rs
pub fn session_from_env(env: &dyn Fn(&str) -> Option<String>) -> String;
// herdr names every socket herdr.sock: the default socket (or none) is "default",
// <dir>/herdr.sock is <dir>'s name, any other socket file is its stem.
pub fn default_herdr_sockets(env: &dyn Fn(&str) -> Option<String>) -> Vec<PathBuf>;

// src/identity.rs: resolve() is also None when HERDR_PANE_ID is unset (pane.current is the
// focused pane, never the caller); an unknown HERDR_PANE_ID is not_in_herdr.

// src/herdr/socket.rs (T0 skeleton, T4 completes): SocketHerdr::connect, default_socket_path,
// open_from_env as in T4's interface. src/herdr/fake.rs and the FakeHerdr re-export exist only
// under the fake-herdr feature, which Cargo.toml turns on for every test build through a
// self dev-dependency; release builds have no POD_FAKE_HERDR hook.

// src/config.rs
pub const HERDR_AGENT_KINDS: &[&str];   // IntegrationTarget enum of schema-22; only codex and claude have launch flags
// resolve_agent errors: unknown_agent, launch_mapping_unsupported (exit 1). [agents.<kind>] keys must be herdr kinds.

// src/doctor.rs
pub fn run(paths: &Paths, herdr: &dyn Herdr) -> DoctorReport;   // failing checks are reported, never raised
```

**Schema:** `src/store/schema.rs` migration 1 creates every table and index from spec section 5, verbatim column names, `CHECK` constraints for every enumeration, and the three invariants as partial unique indexes: `dispatches(task_id) WHERE status IN ('pending','dispatched')`, `dispatches(assignee_terminal_id) WHERE status IN ('pending','dispatched')`, `deliveries(mailbox) WHERE status='outstanding'`.

**Commands delivered working:** `doctor` (herdr reachable and version ≥ 0.9.3, store writable and integrity, `pod` on PATH; the integration and sandbox checks come in T13), `status`. All other commands exist in clap with correct flags and return `{"error":{"code":"not_implemented"…}}` exit 1. `not_implemented` is added to `errors.md` under a "Removed before release" note and deleted in T13.

**Tests:**
- [ ] `paths_default_and_overrides` (XDG defaults, `POD_STATE_DIR`, `POD_CONFIG`, session from `HERDR_SOCKET_PATH`)
- [ ] `store_open_creates_0600_file_and_0700_dir_and_wal`
- [ ] `store_migrates_to_user_version_1_and_is_idempotent`
- [ ] `schema_rejects_invalid_enum` (insert `tasks.status='bogus'` fails)
- [ ] `schema_one_active_dispatch_per_task_and_per_terminal`
- [ ] `config_resolve_agent_order_preset_alias_kind` and `config_alias_rejects_shell_wrappers`
- [ ] `launch_argv_codex_and_claude`
- [ ] `identity_none_outside_herdr` and `identity_fingerprint_stable`
- [ ] `cli_every_command_parses_documented_flags` (one `assert_cmd` per command in section 6 with `--help`)
- [ ] `cli_error_envelope_and_exit_codes` (usage → 2, refused → 1)
- [ ] `doctor_reports_store_and_herdr` (FakeHerdr via `POD_FAKE_HERDR=1` env hook in `main.rs`, test-only feature)

**Acceptance:** `cargo test` green, clippy clean, `git log` shows an initial docs commit then the skeleton commit, `pod status --json` prints paths.

---

### T1: Store rows and queries (sol, wave 1, branch `t1-store`)

Spec sections: 5, 13 (concurrent writers, retention), 16.

**Files:** `src/store/rows/{runs,tasks,dispatches,workers,worker_archives,observations,messages,deliveries,wake_attempts,questions,gates,receipts,events}.rs`, `src/store/signal.rs`, `tests/engine_store.rs`.

**Interfaces produced:** one `pub struct <Table>Row` per table mirroring section 5 columns with `serde` and `chrono` types, and per table: `insert(tx, &Row) -> Result<()>`, `get(tx, id) -> Result<Option<Row>>`, `update_<field>` setters the engine needs, and the list queries the spec names: `tasks::list_by_run(tx, run, status: Option<&str>)`, `tasks::ready_dependants(tx, completed_task)`, `dispatches::active_for_task`, `dispatches::active_for_terminal`, `messages::unread_for_mailbox(tx, mailbox, limit)`, `messages::history(tx, mailbox, limit)`, `deliveries::outstanding(tx, mailbox)`, `wake_attempts::count_since_last_consume(tx, mailbox)`, `events::append(tx, &EventRow)`, `events::list(tx, run)`, `receipts::get(tx, fingerprint, request_id)`, `receipts::prune(tx, now)` (10,000 rows or 30 days). `signal::touch(path)` and `signal::wait(path, deadline) -> Result<bool>` (notify watch with 1 s `data_version` poll). Added in the T1 review: `signal::snapshot(path) -> Snapshot` and `Snapshot::wait(&self, deadline) -> Result<bool>`; `wait(path, deadline)` is `snapshot(path).wait(deadline)`. A waiter (T8) takes the snapshot before querying the store and waits on it, so a commit landing between its query and its wait is not lost. The `data_version` poll reads the `pod.db` beside the signal file. Changed in the wave 1 integration: the watch is created on a helper thread and the waiter re-reads the signal file every 25 ms meanwhile (creating an FSEvents watch took over 100 ms under load); `data_version` is still polled once a second. Added in the wave 1 integration (queries the engine needed): `runs::{bound_to_terminal, list_by_updated, delete, delete_all}`, `tasks::{dependants, count_by_status}`, `dispatches::{list_by_task, list_by_run, latest_for_terminal, dispatched_by_handle, max_failure_count_for_task}`, `messages::{select, get_many, mark_read, first_worker_done, in_thread}` (`messages::insert` returns the new `sequence`), `deliveries::fence_below_generation`, `questions::close_pending_for_dispatch`, `gates::{list_by_run, has_pending_for_task}`, `receipts::delete_all`. `TaskRow.deps` is `Vec<TaskId>`, `GateRow.options` is `Vec<String>`, `DeliveryRow.message_ids` is `Vec<MessageId>`.

**Tests:**
- [ ] one round-trip test per table (insert, get, list)
- [ ] `events_sequence_is_monotonic`
- [ ] `receipts_prune_by_count_and_age`
- [ ] `store_concurrent_writers_both_commit` (two threads, two `Store::open` on one file, 200 inserts each, no error): Review Focus 1
- [ ] `signal_wait_wakes_on_touch_within_100ms`
- [ ] `signal_wait_times_out_false`

---

Continued in [part 2](2026-10-12-implementation-plan-part-2.md) (T2 to T6, T11, T12) and [part 3](2026-10-12-implementation-plan-part-3.md) (T7 to T10, T13, review tasks). The plan was split only to keep each file under 400 lines; nothing else changed.
