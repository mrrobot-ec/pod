# Contract capture

How the Orca samples under `tests/contract/samples/` were obtained, and how to
read them. Spec section 17 item 2 and section 14. Captured 2026-10-12 with
Orca app 1.4.215, `orca orchestration ... --json`.

## Sample file format

Each `<command>[.<variant>].json` file is Orca's output envelope plus two keys
the harness strips:

- `_provenance`: `captured` (Orca's real output) or `hand-authored` (shape
  written by hand because the command could not be run safely).
- `_args`: the argv, after `pod`, that the contract test runs (`--json` is added).
  It is pod's argv, not Orca's: IDs are `{name}` placeholders and Orca-only
  flags are dropped (for example `run-list --limit`).
- `_setup` (optional): pod commands run first, in the same state directory,
  each `{"args": [...], "bind": {"<name>": "<JSON pointer>"}}`. `bind` reads a
  string from that step's output (pod's envelope minus `ok`) and makes
  `{<name>}` available to later steps and `_args`. A failing step fails the
  test with the step's output.
- `_fake_herdr` (optional): `POD_FAKE_*` variables set for the sample command
  only, so the fake herdr behaves as herdr did in Orca's capture (for example
  `POD_FAKE_AGENT_START_ERROR` for an agent that never became ready).
- `id`, `ok`, `_meta`: Orca's envelope. The harness drops them and compares
  `result` with pod's flat envelope minus `ok`.

Sanitizing, applied to every sample:

- Absolute paths under `/Users/<name>` are rewritten to `/Users/me`.
- Dispatch capabilities (`dcap_...`) become `dcap_REDACTED`; the
  `launch_token_hash` in `dispatch-show` is `REDACTED`.
- Arrays are cut to their first two elements and strings to 300 characters, so
  the large `task-list` and `worker-list` outputs stay reviewable. Field names
  and JSON types are unchanged, which is all the harness compares.
- Nothing else in `result` is edited. Run and task IDs, message bodies, and
  timestamps are Orca's real values from the pod build Runs.

## Capture procedure

The samples were not captured from a throwaway Run, because `run-create`
would rebind the capturing terminal and break its own dispatch. Instead:

1. Read-only commands were run live with an explicit `--run`, `--id`,
   `--dispatch`, `--task`, or `--request` against the live Run
   `run_31fc2c96a4a1`.
2. Mutating commands with real receipts were captured by the coordinator
   during the build (`worker-start` in four states and `check --wait` or
   `--ack` deliveries) and saved as files.
3. Commands that could not be run were hand-authored from `orca orchestration
   <command> --help` and from the shapes of captured samples.

The capture changed no Orca state.

## Samples

| Sample | Command run | Provenance |
|---|---|---|
| `run-list` | `run-list --limit 2` | captured |
| `run-show` | `run-show --id run_31fc2c96a4a1` | captured |
| `run-current` | `run-current` (unbound terminal, so `run` is null) | captured |
| `task-list` | `task-list --run <run>` | captured |
| `task-list.brief` | `task-list --run <run> --brief` | captured |
| `task-list.ready` | `task-list --run <run> --ready` (empty) | captured |
| `inbox` | `inbox --limit 2` | captured |
| `check.peek` | `check --peek --terminal run:<run>` | captured |
| `gate-list` | `gate-list --run <run>` (no gates) | captured |
| `worker-list` | `worker-list --run <run> --limit 2` | captured |
| `worker-show` | `worker-show --dispatch ctx_f4e457f62961` | captured |
| `worker-read` | `worker-read --dispatch ctx_f4e457f62961 --limit 3` | captured |
| `dispatch-show` | `dispatch-show --task task_e62309f8a66f` | captured |
| `request-show` | `request-show --request 33abc4a2-...` (a `check` receipt) | captured |
| `check.wait-question` | `check --wait`, one question delivered | captured by coordinator |
| `check.wait-heartbeats-and-question` | `check --wait`, heartbeats and a question | captured by coordinator |
| `check.wait-timed-out` | `check --wait` timing out | captured by coordinator |
| `check.ack-wait-worker-done` | `check --ack <delivery> --wait`, a `worker_done` | captured by coordinator |
| `check.ack-wait-mixed` | `check --ack <delivery> --wait`, mixed message types | captured by coordinator |
| `worker-start.ready` | `worker-start`, state `ready` | captured by coordinator |
| `worker-start.ready-worktree-path` | `worker-start --worktree path:<p>` | captured by coordinator |
| `worker-start.failed-agent-readiness` | `worker-start`, state `failed` at readiness | captured by coordinator |
| `worker-start.outcome-unknown` | `worker-start`, outcome unknown | captured by coordinator |
| `run-create` | `run-create --objective ...` | hand-authored |
| `task-create` | `task-create --spec ...` | hand-authored |
| `send` | `send --subject ... --to run:<run>` | hand-authored |
| `reply` | `reply --id ... --body ...` | hand-authored |
| `gate-create` | `gate-create --task ... --question ...` | hand-authored |
| `gate-resolve` | `gate-resolve --id ... --resolution ...` | hand-authored |
| `worker-release` | `worker-release --dispatch ...` | hand-authored |

Hand-authored samples reuse real sub-objects (the run, task, and message rows
come from captured samples) inside the receipt layout implied by the captured
`mutation: {requestId, replayed}` block. The gate row is guessed from the
message and task rows. They are the least reliable samples; a reviewer who
sees a diff against one of them should check Orca before changing pod.

## Commands with no sample

Orca has them, but they mutate live worker or Run state, so nothing was run
and no shape was invented: `run-use`, `ask`, `dispatch`, `worker-stop`,
`worker-abandon`, `worker-retain`, `reset`. Pod's `doctor`, `status`, `log`,
`board`, `hook`, `skills`, and `task-update` have no counterpart in `orca
orchestration` (Orca's `status` is the app status, not a Run command).
`task-update` exists in Orca but was not captured because it mutates the live
Run. `run-use`, `task-update`, `reset`, and `dispatch --dry-run` can only be
captured from a throwaway Run, so they have no sample.

## Reading a contract test

`tests/contract/mod.rs` has one test per sample, named `sample_<command>`
(`.` and `-` become `_`). Each runs the sample's `_setup` steps and then
`pod <_args> --json` with `POD_FAKE_HERDR=1` and isolated state, compares field
names and JSON types with `compare`, and filters the result through the
entries for that command in `allowed-differences.toml`. Every sample's test
is active; none is ignored. When a test fails, either fix pod or, for a
section 14 row or a shape pod's reference documents, add an entry with its
reason to `allowed-differences.toml`.

Every step runs as the coordinator terminal `term_contract` through a fake
herdr pane: the harness sets `HERDR_ENV`, `HERDR_PANE_ID`, and
`POD_FAKE_PANES` (`tests/support::fake_caller_env`), so `identity::resolve`
returns a real caller. A step may also carry `"as": "<terminal>"` to run as
another terminal, or be `{"seed_worker": {"task", "terminal", "handle"}}`,
which stores a `dispatched` Dispatch and a ready worker for that Task in place
of `worker-start`, so worker-side setup such as `reply`'s `ask` can run;
its `bind` reads `{"dispatch": {"id"}}`, as `worker-read`'s setup does.
Each sample's `_setup` matches pod's real output. The `failed` and
`outcome_unknown` `worker-start` variants run with `_fake_herdr` set to
`POD_FAKE_AGENT_START_ERROR` and `POD_FAKE_RENDER_SENT_TEXT=0`.

Three tests guard the fixtures without running pod commands:
`samples_are_well_formed_and_each_has_a_test` (one correctly named
`sample_test!` per sample file), `sample_argv_parse_as_pod_commands` (every
setup and sample argv parses with pod's CLI, and every placeholder is bound by
an earlier step), and `allowed_differences_name_their_section_14_row` (every
entry names its row, and its path exists in a sample of its command, or for
`extra`, its parent does and it does not).

Comparison rules: `null` on either side is compatible with any type; arrays
are compared element by element against the sample's elements (closest match
reported); an empty array asserts nothing; fields only in pod are `extra`,
fields only in the sample are `missing`.
