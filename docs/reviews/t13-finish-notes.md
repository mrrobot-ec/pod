# T13 notes: benches, doctor, doc sync

Delivered: `benches/commands.rs`, doctor completeness (`src/doctor.rs`, `tests/doctor.rs`), `tests/docs_sync.rs`, removal of `not_implemented`, README/CHANGELOG/doc sync. main was merged before the removal and doc sync (no conflicts).

## Edits outside the ownership list (approved by the coordinator)

- `src/error.rs`: removed `ErrorCode::NotImplemented` and `PodError::not_implemented`.
- `src/herdr/fake.rs`, `tests/engine_wake.rs`: the stub error is now `PodError::new(ErrorCode::HerdrError, ...)` naming the unsupported method.
- `tests/t0.rs`: deleted the dead `cli_commands_accept_documented_flags` test (its command list was empty).
- `tests/docs_sync.rs` is a new test file holding `docs_mention_no_removed_flag_or_code` and `no_not_implemented_left`.

## ASSUMPTIONs

- ASSUMPTION: the docs test scans every `.md` under `docs/` except `plans/`, `specs/` and `reviews/` (history, not shipped behaviour).
- ASSUMPTION: flags are checked as `--long` tokens against every long flag on every clap command; flags of other tools named in docs (`--ignored`, `--test`, `--session-id`, `--skip-git-repo-check`, `--help`, `--version`) are allowlisted in the test. Short flags are not checked.
- ASSUMPTION: "documented" for an error code means a row in `docs/reference/errors.md` equal to `ErrorCode::ALL`; for a flag, any mention in the scanned docs.
- ASSUMPTION: `"code": "<snake_case>"` examples in docs must name a real code.
- ASSUMPTION: bench assertion uses the 50 ms target on the in-process command path with FakeHerdr (spec: excluding herdr round trips).
- ASSUMPTION: `pod doctor` prints JSON with and without `--json`, so README documents the JSON entries instead of the earlier invented text layout.
- ASSUMPTION: the 0.1.0 changelog entry is dated 2026-10-13 (replaces `Unreleased`).

## Findings for the coordinator

- Real herdr 0.9.3 has no `herdr integration list`; it has `herdr integration status [--outdated-only]`. The spec and task name `list`, so doctor keeps `list`, and on a real herdr the integration check reports a failure that carries herdr's usage text. Switching to `status` is a one-line change in `src/doctor.rs` if wanted.
- `docs/reference/contract-capture.md` and `herdr-adapter.md` still cite task labels (T7, T8) and `tests/contract` still ignores the `worker-start` samples ("ignored, T7"); wave 2 owns those.

## Review (Opus)

Verdicts on the ASSUMPTIONs above:

- Docs scan excludes `plans/`, `specs/`, `reviews/`: **accepted**, and **corrected** to scan `README.md` too, since it documents install and doctor.
- `--long` flags against every clap long flag, with a foreign-flag allowlist and no short flags: **accepted**. Checked by hand: a planted `--bogus-flag` in the README fails the test.
- Error code "documented" means errors.md rows equal `ErrorCode::ALL`: **accepted**. **Corrected** to also fail on any mention of `not_implemented` in the scanned docs, since only quoted `"code": "x"` examples were checked before.
- `"code": "<snake_case>"` examples must be real codes: **accepted**.
- Bench asserts 50 ms with FakeHerdr: **accepted** (it times the real binary as a process, which is stricter than in-process). **Corrected**: `cargo test` skips bench targets, so CI never ran the assertion. `[[bench]] test = true` now runs it once under `cargo test` in criterion's test mode. Checked by setting the target to 1 ms, which fails with `status took 14ms`.
- `pod doctor` prints JSON with and without `--json`: **accepted**. **Corrected** the README and commands.md to match: compact JSON with `--json`, pretty JSON without it, `"ok": true` in the envelope, and exit 1 only when a check is `fail`. commands.md had claimed text output and a `store_access_denied` error, and the README example `detail` did not match the code.
- 0.1.0 dated 2026-10-13: **accepted**.
- In-code ASSUMPTION, the `herdr integration list` keyword parser: **corrected**. herdr 0.9.3 has only `integration status`, which prints `<kind>[ (experimental)]: <state> (<hook path>)`. The old parser would have read `codex: current (v8) (~/.codex/...)` as not installed, because the line has none of its keywords, and it matched `claude` only through the substring "ok" in "hooks". Doctor now parses `status` lines, warns on `not installed`, `outdated`, or an unlisted kind, and the tests use real herdr output. The plan's T13 line is updated to `status`.
- In-code ASSUMPTION, sandbox probe from doctor's own process: **accepted**. The probe now runs once for all kinds, uses a per-pid file name, and no longer `create_dir_all`s the state directory, which would have created it 0755. troubleshooting.md now says to run `pod doctor` inside the agent's pane to test that sandbox.

Other fixes:

- herdr-dagr: doctor looked only for `herdr-dagr`, but `pod board` looks for `dagr`, so the two could disagree. Doctor now accepts either one on `PATH` or a herdr plugin, and prints the same install command as `pod board`.
- An unloadable config was silently replaced by the defaults. It is now a failed `config` check.

Not fixed, outside T13's files (flagged to the coordinator):

- Spec section 12: `pod board` never opens the dagr pane with `DAGR_RUN`, and pod never writes herdr sidebar status (`board::sidebar_status` has no caller). The README, CHANGELOG, config.md, commands.md, and troubleshooting.md now describe what ships instead of claiming both.
- Spec 17 item 5: `tests/board/dagr-v3.schema.json` is still the ASSUMPTION schema written from spec section 12. dagr is not installed and the upstream schema was not available, so `dagr check --strict` was never run.
- `~/.local/state/pod/` (the parent of the session directory) exists with mode 0755 on this machine. The store creates only the session directory 0700.
- Task labels (T5, T7, T8) remain in herdr-adapter.md, contract-capture.md, and transcript-formats.md, as the worker noted. Wave 2 owns those.
