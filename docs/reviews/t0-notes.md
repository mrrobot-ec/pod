# T0 implementation notes

- ASSUMPTION: a set `HERDR_SOCKET_PATH` maps to a session by taking the socket file stem; an unset path maps to `default`.
- ASSUMPTION: the structured code for clap/config usage failures is `usage`. It is documented in `errors.md` and exits 2.
- ASSUMPTION: a missing path passed to `Config::load` means the all-default configuration; callers that distinguish an explicit `POD_CONFIG` override may reject it before loading.
- ASSUMPTION: the known unconfigured herdr agent kinds at this layer are `codex`, `claude`, `grok`, and `cursor`; presets and command aliases resolve before those built-ins.
- ASSUMPTION: Claude effort is supplied through configured `args`, so `launch_argv` adds only Claude's `--model` mapping.
- ASSUMPTION: T0's real herdr skeleton obtains only version/protocol for `doctor` through the allowed `herdr` executable; T4 replaces it with the socket adapter.
- COORDINATOR DECISION: add `src/lib.rs` as the public module root and keep `src/main.rs` as a thin binary so integration tests and later tasks import `pod::...`.

## Review (Opus, 2026-10-12)

Verdict per entry:

- Session from the socket file stem: **corrected.** herdr names every session's socket `herdr.sock` (the default is `~/.config/herdr/herdr.sock`, per `herdr session list`), so the stem put every session in one store `herdr`, and gave the default session a different store inside panes (`herdr`) than outside (`default`). Now: unset or the default socket is `default`, `<dir>/herdr.sock` is `<dir>`'s name, any other socket file is its stem (`paths::session_from_env`). The named-session directory layout was inferred from `session_dir` in the herdr binary and not observed with a live named session; T10's `herdr --session pod-e2e-<pid>` run should confirm it.
- `usage` for clap and config failures, exit 2: **accepted.**
- Missing `Config::load` path means defaults: **accepted**, with the caller rule now implemented: `cli::Ctx::config` refuses a `POD_CONFIG` that names a missing file (config.md).
- Known agent kinds `codex`, `claude`, `grok`, `cursor`: **corrected.** herdr's kinds are the 17 `IntegrationTarget` values in `vendor/herdr/schema-22.json` (`config::HERDR_AGENT_KINDS`); refusing `opencode` or `pi` contradicted "preset, command alias, herdr agent kind". `[agents.<key>]` must now be a herdr kind, since an alias only swaps a built-in kind's executable. Resolution failures now exit 1 with `unknown_agent` / `launch_mapping_unsupported` instead of `usage` (exit 2), as errors.md requires.
- Claude effort through `args`: **accepted** (spec section 19 fallback).
- Real herdr skeleton reads version and protocol through the `herdr` executable: **corrected in part.** `herdr --version` and `herdr api schema --json` work with no server, so `doctor` reported herdr `ok` while `herdr status server` said "not running". `ping` now connects to the session's socket first and fails with `herdr_unreachable`.
- `src/lib.rs` as crate root: **accepted.**

Other defects fixed (commits 274c5e2, c55bcf0, and the docs commit after them):

1. `cli/mod.rs` held the only dispatch `match`, and no later task owns that file: every wave-1 worker would have edited it. Each command now calls one handler in its owning `commands/*.rs` file, and `cli::Ctx` builds the engine from the environment.
2. No later task owns `error.rs` or `errors.md`, but T4 expects `herdr_unreachable` and `herdr_error`, T7 `terminal_busy`, and errors.md listed eight refusals with no code. Ten codes were added, with the reference pages updated; `error_codes_match_errors_md` now pins code-to-document parity.
3. `FakeHerdr` and `POD_FAKE_HERDR` were compiled into every build and the declared `fake-herdr` feature was unused. Both are now feature-gated; a self dev-dependency turns the feature on for test builds.
4. Identity used herdr's focused pane when `HERDR_PANE_ID` was unset; it now returns `None`. An unknown pane is `not_in_herdr`, not `usage`.
5. The store file was created with the umask before the `chmod`, and the parent directories it created were `0755`. The file is now created `0600` and new directories `0700` before SQLite opens anything.
6. `doctor` aborted with an error envelope when the store could not open; it now reports a failing `store` check, runs a write transaction, and checks the executable bit for `pod` on `PATH`.
7. The alias check let an absolute `/usr/bin/env FOO=1 codex` through; whitespace and `=` are now refused in every form.
8. `--run` was added to `task-list`, `gate-list`, and `inbox` (spec 3.4: read-only commands work outside herdr with an explicit `--run`). `check --unread|--peek|--all` are now mutually exclusive.
9. `cli_every_command_parses_documented_flags` only ran `--help`. `cli_commands_accept_documented_flags` now passes every documented flag and expects `not_implemented` (exit 1), and `cli_usage_errors_exit_2` covers the refusals. `tests/support` allows dead code so a test crate that uses part of it passes clippy.

Checked and left as is: every table, column, and enumeration `CHECK` in section 5, and the three partial unique indexes, are present with verbatim names. WAL is on. Every documented flag of section 6 parses. Two other decisions were kept: `--retry-request` and `--json` are global flags (harmless on read-only commands), and `doctor` requires protocol exactly 22.

Not verified: a live named herdr session's socket path (see above); the Linux build (only macOS arm64 was built here). `From<rusqlite::Error>` maps every SQL error, including constraint violations, to `store_access_denied`; T1 and T2 should map unique-index violations to `terminal_busy` or their own refusals before they reach the user.
