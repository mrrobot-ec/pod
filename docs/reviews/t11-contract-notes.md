# T11 contract samples: assumptions

- ASSUMPTION: Cargo does not build `tests/contract/mod.rs` on its own, so a
  two-line shim `tests/contract.rs` (`#[path = "contract/mod.rs"]`) makes it a test
  crate. The shim is outside the Ownership list; it avoids editing `Cargo.toml`.
- ASSUMPTION: the coordinator's instruction replaced the plan's "throwaway Run +
  reset" capture. Read-only commands were captured live, the coordinator supplied
  mutating receipts, and seven commands were hand-authored (marked
  `_provenance: hand-authored`). See `docs/reference/contract-capture.md`.
- ASSUMPTION: samples keep Orca's envelope (`id`, `ok`, `result`, `_meta`); the
  harness compares `result` against pod's flat envelope minus `ok`. The envelope
  difference is structural, so it is not an `allowed-differences.toml` entry.
- ASSUMPTION: arrays in samples are cut to two elements and strings to 300
  characters (field names and types unchanged) to keep fixtures reviewable. The
  spec says "verbatim".
- ASSUMPTION: variants of one command use `<command>.<variant>.json` (for example
  `check.wait-question`, `worker-start.failed-agent-readiness`) and carry their
  own `_args`. The spec asks for one sample per command; variants cover distinct
  result shapes.
- ASSUMPTION: `null` on either side matches any type, because a sample records
  one value of a nullable field. Empty arrays assert nothing about elements.
- ASSUMPTION: extra fields in pod's output are reported (`extra`), not ignored,
  since section 14 lists fields pod adds; each needs an allowed entry or a fix.
- ASSUMPTION: the JSON paths in `allowed-differences.toml` come from the pod
  shapes in `docs/reference/commands*.md`, not from running pod (every command
  except `doctor` and `status` is still `not_implemented`). The reference docs say
  their layouts are provisional, so reviewers should expect to adjust paths when
  they un-ignore a test.
- ASSUMPTION: pod's documented receipts differ from Orca's beyond section 14
  (for example `check` returns `delivery{}` where Orca has `deliveryId`, and run
  and gate rows use different keys). These are left as test failures for each
  task's reviewer rather than allowed here; the spec says only the section 14
  differences are allowed.
- ASSUMPTION: `ignore` reasons use the task that owns the command: T2 runs, tasks,
  gates, `request-show`; T3 `send`, `reply`, `inbox`, `check --peek`; T8
  `check --wait` and `--ack`; T7 worker commands and `dispatch-show`; T9
  `worker-read`.
- ASSUMPTION: no samples for `run-use`, `ask`, `dispatch`, `worker-stop`,
  `worker-abandon`, `worker-retain`, `reset`, `task-update`: they mutate live
  Orca state and the coordinator listed only seven hand-authored commands.
  The "one test per sample" requirement is met; "one sample per command" is not.
- ASSUMPTION: pod's own `doctor`, `status`, `log`, `board`, `hook`, `skills` have
  no Orca equivalent under `orca orchestration`, so they have no sample.
- ASSUMPTION: `pod` is run with a cleared environment plus `POD_FAKE_HERDR=1`,
  `HOME`, `POD_STATE_DIR`, `POD_CONFIG`, and `HERDR_SOCKET_PATH`; samples that
  need existing IDs (for example `worker-show`) will need the reviewer to seed
  state through the engine before the call.
- Extra tests beyond the list: `samples_are_well_formed_and_each_has_a_test` and
  `allowed_differences_name_their_section_14_row`.

## Review (Opus)

Verdicts, in the order of the entries above.

- Shim `tests/contract.rs`: **accepted**. Cargo discovers only `tests/*.rs` and
  `tests/*/main.rs`, so `tests/contract/mod.rs` needs either the shim or a
  `[[test]]` entry in `Cargo.toml`; the shim is the smaller change. No plan
  signature is involved.
- Capture procedure replaced: **accepted as recorded**; the coordinator's
  instruction could not be checked from this worktree. Under the plan's own
  procedure `run-use`, `task-update`, `reset`, and `dispatch --dry-run` would
  have been captured on a throwaway Run. That capture is still owed (see the
  last entry); `contract-capture.md` now says so.
- Envelope (`result` against pod's flat envelope minus `ok`): **accepted**.
  Checked: `pod status --json` prints a flat object with `"ok": true`.
- Arrays cut to two elements, strings to 300 characters: **accepted**. Field
  names and types are all the harness compares; only element variety past
  the second row is lost.
- `<command>.<variant>.json`: **accepted**.
- `null` matches any type; empty arrays assert nothing: **accepted**. As a
  result `gate-list`, `task-list.ready`, and `check.wait-timed-out` check
  only their top-level fields, because Orca's arrays were empty.
- Extra fields reported: **accepted**.
- Allowed paths taken from `commands*.md`: **corrected**. Several entries could
  never match: `worker-show $.workers[]...` (worker-show has no `workers`),
  `attention.categories` (Orca already has it; new categories are values,
  which are not compared), `send $.settled.*` (the send sample is not a
  lifecycle send), `gate-create $.message.**` (only resolving a user gate
  posts a message), and `termination_reason` on `dispatch-show` and
  `worker-show`, where Orca already has the field. Added fields now use the
  casing of their Orca siblings (`terminationTarget`, `preambleInput`,
  `needsInput`, `evidenceSource`), because a pod field named in snake_case
  next to Orca's camelCase one would be reported as missing anyway. Added the
  same liveness entries for `worker-read`'s `projection`.
  `allowed_differences_name_their_section_14_row` now rejects an entry whose
  path is not in a sample of its command (for `extra`: whose parent is not, or
  which is already there).
- Receipts that differ beyond section 14 are left as failures: **accepted**.
  Spec 17 item 2 allows only section 14 differences.
- `ignore` reasons by owning task: **accepted**. Checked against the plan's spec
  coverage map (8.3 `--wait` → T8, 11 → T9, receipts → T2).
- No samples for eight commands: **corrected in the doc, not fixed**.
  `contract-capture.md` said `task-update` has no Orca counterpart and was also
  not captured; it now says Orca has it. The missing captures need `orca`
  commands this review may not run.
- pod-only commands have no sample: **accepted**.
- Cleared environment and seeding: **corrected**. Every `_args` named an Orca ID
  (`run_31fc2c96a4a1`, `ctx_f4e457f62961`, `task_e62309f8a66f`,
  `delivery_e0224cbc11dc`, a UUID request ID) that cannot exist in the fresh
  store each test gets, and `delivery_` and UUID request IDs are not pod
  formats. `run-list --limit 2` is a usage error in pod (spec 6 gives
  `run-list` no flags), and `check --peek --terminal run:<id>` is Orca's form
  of pod's `check --peek --run <id>`. Samples now carry `_setup` steps that run
  in the same state directory and bind IDs into `{name}` placeholders.
  `check --wait` and `--ack --wait` samples get `--timeout-ms 1000`, and each
  pod run has a 30 s timeout, so a wait cannot hang the suite.
- Extra tests: **accepted and tightened**. `samples_are_well_formed_and_each_has_a_test`
  now parses the `sample_test!` lines (one per sample, named after it, no test
  without a sample) instead of searching for the quoted name anywhere in the
  file. New: `sample_argv_parse_as_pod_commands` parses every setup and sample
  argv with `pod::cli::Cli`, so an Orca-only flag fails today, not when a
  later reviewer un-ignores the test.

Also fixed: `sample_test!` had only an `ignore` arm, so deleting the `ignore`
argument, as the plan tells the un-ignoring reviewer to do, did not compile.

Open, outside T11:

- Commands that need a caller cannot succeed through the CLI under
  `POD_FAKE_HERDR=1`: the fake herdr has no panes and the harness sets no
  `HERDR_ENV`, so `run-create` (the first setup step of most samples) returns
  `not_in_herdr`. The CLI needs a way to seed the fake caller pane before T2's
  reviewer un-ignores the first test (T4 completes `src/herdr/fake.rs`).
- `docs/reference/contract-capture.md` must be added to `REFERENCES` in
  `src/skills.rs` at merge (T12's test). This branch predates T12, so the edit
  is left to the merge to avoid a conflict.
- A throwaway-Run capture of `run-use`, `task-update`, `reset`, and
  `dispatch --dry-run` before Orca is removed.
