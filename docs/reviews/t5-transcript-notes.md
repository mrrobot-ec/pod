# T5 transcript readers: assumptions

- ASSUMPTION: the trait's `session_id: &str` has no "none" value, so an empty
  or unsafe id means unproven and `locate` returns `Ok(None)`. T0's signature
  was not changed.
- ASSUMPTION: a `session_path` supplied by herdr that exists as a file is
  accepted as proof for every reader without checking its location.
- ASSUMPTION: the shared paging helpers (`read_lines`, `json_lines`, `valid_id`)
  live in `src/transcript/screen.rs`, because `mod.rs` is limited to the
  registry body. They are `pub(crate)`.
- ASSUMPTION: each reader has `new()` (uses `$HOME`) and `with_home(path)` for
  tests. `reader_for` uses `new()`.
- ASSUMPTION: the cursor hash is SHA-256 of the source path string, not of the
  file contents, so appends do not change it. "Source" means the file.
- ASSUMPTION: an offset past the end of the file, or a source that cannot be
  opened, is reported as `source_changed`. A malformed cursor is a usage error.
- ASSUMPTION: `limit` counts rows and is checked between lines; a line that
  yields several rows can exceed it. `limit = 0` returns no rows and an
  unchanged cursor.
- ASSUMPTION: `content_complete` means every byte was consumed with no partial
  trailing line. A page that stops at `limit` is not complete.
- ASSUMPTION: only `tool_result` rows are trimmed (4,000 characters, no
  marker text); `clipping` is true when any row was trimmed.
- ASSUMPTION: Claude `tool_call` text is the JSON of `input`; Codex and Grok
  use the raw input/arguments string. `thinking` blocks, `isMeta` lines and
  non-message line types are skipped.
- ASSUMPTION: Codex `developer` messages and the injected
  `<environment_context>` user message are not transcript content.
- ASSUMPTION: the Claude slug replaces every non-alphanumeric character with
  `-`; the canonicalised cwd is tried as a second slug (macOS `/tmp` symlinks).
- ASSUMPTION: Cursor transcripts have no tool rows (none are recorded) and no
  timestamps; Grok has no timestamps.
- ASSUMPTION: `ScreenReader::read` treats its source as a text file of
  archived screen text. Its `locate` only echoes an existing `session_path`.
  The live screen text path (herdr `pane read`) is T9's job.
- ASSUMPTION: Grok was not signed in and `cursor-agent` is not installed, so
  those samples come from existing session stores (Grok scrubbed, Cursor
  synthetic); see `docs/reference/transcript-formats.md`.

## Review (Opus)

Verdicts, in the order above:

1. Empty or unsafe id means unproven: **accepted**. The trait signature is the
   plan's; `""` is the "none" value T9 passes.
2. A herdr `session_path` that is a file is proof: **accepted**. herdr is the
   trusted source in spec section 11. Not verified: whether herdr reports the
   Grok session directory rather than `chat_history.jsonl`; a directory is not
   a file, so it falls back to the id lookup.
3. Shared helpers in `screen.rs`: **accepted** (`mod.rs` is registry-only).
4. `new()` / `with_home()`: **accepted**; extra constructors, no plan change.
   With `HOME` unset the readers look under a relative `.claude/` etc.; left
   as is (agents always run with `HOME`).
5. Cursor hash over the source path: **accepted**, and strengthened: an offset
   that no longer falls between lines now returns `source_changed`, which
   catches a file rewritten under the cursor.
6. Offset past the end or unopenable source is `source_changed`; malformed
   cursor is `usage`: **accepted**; the malformed case is now tested.
7. `limit` counts rows between lines; `limit = 0` returns nothing: **accepted**.
8. `content_complete` means every byte consumed: **corrected**. A last line
   without a newline is now consumed when it parses, because Cursor never ends
   a transcript with a newline (most transcripts on this machine), so the last
   answer was never returned and `content_complete` never became true. A
   half-written last line is still left for the next read, without a warning.
   The engine also streams line by line instead of loading the rest of the
   file per page (a 256 MB Claude transcript: first page 1.5 ms).
9. Only `tool_result` rows trimmed to 4,000 characters: **accepted** (plan
   says tool output).
10. Tool-call text: **accepted**.
11. Codex `developer` and `<environment_context>` skipped: **corrected** to
    also skip the injected `# AGENTS.md instructions for <dir>` user message,
    seen in real rollouts; workers run in repos that have one.
12. Claude slug: **accepted**. Not verified: Claude's handling of very long
    cwds (no project directory here is longer than 107 characters).
13. Cursor has no tool rows: **corrected**. Cursor transcripts since September
    2026 record `tool_use` blocks (thousands in one real transcript) and prefix
    user text with `<timestamp>`, and write `turn_ended` lines. The reader now
    emits `tool_call` rows and returns only the `<user_query>` text; the
    synthetic sample and the test cover all three. Tool output is still not
    recorded by Cursor. No timestamps in Cursor or Grok: accepted.
14. `ScreenReader` reads an archived text file: **accepted**. Its last line is
    returned even without a final newline.
15. Grok and Cursor samples not from fresh runs: **accepted**; the shapes were
    re-checked against real stores on this machine.

Also fixed: Codex `locate` matched any suffix of the rollout name, so a tail
of the UUID (`d672d6d6a4ff`) found the session. It now parses the id after the
fixed-width timestamp; the test covers it.

No public signature changed; the plan needs no update.
