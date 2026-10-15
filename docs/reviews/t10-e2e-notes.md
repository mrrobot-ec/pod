# T10 end-to-end notes

- ASSUMPTION: ignored Rust tests are enabled only by `scripts/e2e.sh`, which
  sets `POD_E2E=1` and passes `--ignored`; direct `cargo test` stays hermetic.
- ASSUMPTION: the scripted `pi` command alias may self-report through
  `pane report-agent` while it waits at a literal `> ` prompt. This is the
  herdr-documented custom-agent mechanism and lets protocol 22 observe an idle
  no-model process without weakening pod's real safe-input checks.
- FALLBACK: herdr 0.9.3 treats protocol `agent.start.args` as arguments to the
  canonical `pi` executable; it does not execute pod's first alias argv as the
  command. The isolated PATH therefore supplies a mode-0700 `pi` copy of the
  fake script, while test `config.toml` still exercises `[agents.pi].command`.
- ASSUMPTION: a named session can share no user state when the harness gives
  herdr temporary `HOME`, `XDG_CONFIG_HOME`, and `XDG_STATE_HOME`; the session
  remains named `pod-e2e-<pid>` and is stopped, unlinked, and deleted in `Drop`.
- ASSUMPTION: Herdr's v0.9.3 release page publishes no checksum asset. CI pins
  the official Linux x86_64 release URL and verifies the SHA-256 measured from
  that immutable tag asset (`18a8dc65…f4dba7`).
- Coordinator decision: the Linux e2e job is `continue-on-error` until the owner
  observes it on a real remote CI run, because this repository has no remote.
- Coordinator-approved cross-ownership fix: safe-input confirmation now reads
  `pasted line count + 20` lines, capped at 2,000. A live `pi` pane proved that
  the former fixed 40-line snapshot clipped the mandatory preamble's prompt.
- Coordinator-approved fallback: the scripted `pi` redraws a size-matched
  collapsed-paste summary because herdr 0.9.3 `recent_unwrapped` dropped a
  segment from a long echoed input line. A mismatched count withholds Enter.
- CONFIRMED: protocol 22 acknowledges `pane.send_text` before the target has
  necessarily redrawn. Safe input polls snapshots for at most 100 ms, never
  resends text, and still withholds Enter unless the exact paste is proven.

## Review (Opus)

Verified by running `POD_E2E=1 scripts/e2e.sh` against isolated named herdr
0.9.3 sessions on macOS arm64: all seven scenarios pass (repeatedly, and with
bogus `HERDR_SOCKET_PATH`/`HERDR_PANE_ID` set to prove isolation).
`cargo build --all-targets`, `cargo test`, `cargo clippy --all-targets -- -D
warnings`, `cargo fmt --check` and `shellcheck` on the three scripts pass.

Verdicts on the entries above:

- ASSUMPTION (ignored tests run only through `scripts/e2e.sh`): accepted.
  Stable Rust cannot un-ignore at run time; `run()` asserts `POD_E2E=1`, so
  `cargo test -- --ignored` without it fails loudly rather than passing
  vacuously. Plain `cargo test` stays hermetic.
- ASSUMPTION (scripted `pi` self-reports via `pane report-agent`): accepted.
  It is herdr's documented custom-agent mechanism and does not relax pod's
  checks. Observed side effect, not T10's to fix: the Dispatch records
  `agentKind: "unknown"`, because T7 records the kind herdr lists at
  detection, before the script's first report.
- FALLBACK (`pi` on the isolated PATH; `[agents.pi].command` still set):
  accepted. `launchEffective.command` shows the configured alias.
- ASSUMPTION (temporary `HOME`/`XDG_*` give a named session no user state):
  accepted, and corrected: the harness now also removes inherited
  `HERDR_ENV`, `HERDR_SOCKET_PATH`, `HERDR_CLIENT_SOCKET_PATH`,
  `HERDR_PANE_ID`, `HERDR_TAB_ID`, `HERDR_WORKSPACE_ID`, `HERDR_BIN_PATH`.
  `herdr --session` already wins over `HERDR_SOCKET_PATH` (checked), but the
  isolated server and its panes inherited them. Teardown no longer panics
  while unwinding (which aborts the test binary) and kills a server that
  ignores `session stop` instead of waiting forever.
- ASSUMPTION (Linux digest measured from the tag asset): accepted; re-measured
  independently (`18a8dc65…f4dba7`). Corrected the job to spec 17 item 4,
  which requires CI on macOS and Linux: it is now a matrix adding `macos-14`
  with `herdr-macos-aarch64` (`5173a3e0…884157`, identical to the herdr the
  suite passed with locally). `shasum -a 256 -c` works on both images.
- Coordinator decision (`continue-on-error`): accepted as recorded.
- Cross-ownership fix (confirmation read = lines + 20, cap 2,000): accepted
  against spec 8.2; it widens only what is read, never what is typed. The
  bounded 100 ms redraw poll had no unit test; added
  `safe_input_waits_briefly_for_redraw_without_resending` and
  `safe_input_redraw_wait_is_bounded_and_withholds_enter` (the first fails
  if the poll is removed).
- Cross-ownership fix (`pi` collapsed-paste summary): accepted. The count
  must equal the submitted chars or bytes, the same proof as Codex's form, so
  Enter is still never sent for unproven text. Corrected the docs: the new
  herdr-adapter entry had renumbered "User takeover" from item 10 to 11 while
  `commands-workers.md` still cites item 10; the new entry is now item 11 and
  also records the redraw wait and read size.
- CONFIRMED (send_text acknowledged before redraw): accepted.

Scenario gaps closed in `tests/e2e/session.rs`: the happy path now proves the
worker received `approved`, the heartbeat landed, the released worker reads
back `released`, the log order the pilot relies on, the board export, and the
0700/0600 modes of the state directory, `pod.db` and `run.json`; the nudge
requires a `delivered` attempt and a ladder cleared by the consuming check;
the closed pane must project `liveness: exited`, `outcome_unknown` (herdr
reports `presence: absent`) and leave nothing reclaimable; the restart
compares `worker-show`'s ladder before and after; the specialist asker must
see the winning reply; the user-gate wait must still be blocked when the gate
is resolved and wake within 3 s.

`scripts/pilot.sh` corrected: its checklist told the pilot to find two release
events in `pod log`, but release writes no event (`worker-show`'s
`terminalState` is the record), and it built the board path by hand, ignoring
`XDG_STATE_HOME`; it now uses the `boardFile` that `pod board` prints.

Not verified here: the Linux and macOS CI jobs (no remote), and the live
pilot itself. Observations for their owners, not T10 defects: a just-created
herdr 0.9.3 tab can reject `agent.start` with "not an available shell", which
the harness retries once (it did not fire in three full local runs) but which
a person running `worker-start` would see as a failed start (T7); and
`worker-release` emits `terminal_state` while `worker-show` emits
`terminalState`.
