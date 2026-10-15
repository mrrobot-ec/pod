# Transcript formats

The transcript formats pod's readers parse, as observed on 2026-10-12.
Samples are in `tests/transcript/samples/`. All rows map to
`{role, kind, text, tool, at}` where `role` is `user | assistant | tool` and
`kind` is `message | tool_call | tool_result`.

## Paging (all readers)

- Cursor: `"<source-hash>:<byte offset>"`. The hash is the first 8 bytes (hex)
  of SHA-256 over the source path. The offset is the byte after the last line
  consumed. A different hash, an offset past the end of the file, or an
  offset that no longer falls between lines (the file was rewritten) returns
  `source_changed`. A malformed cursor is a usage error.
- `limit` counts rows and is checked between lines, so a line is never split;
  one line can add several rows past the limit.
- The file is streamed line by line from the cursor, never loaded whole.
- A last line without a newline is consumed only if it parses (Cursor never
  ends a transcript with a newline). A half-written last line is left for the
  next read, is not a warning, and `content_complete` is `false`.
- `content_complete` is true when every byte of the file was consumed.
- Rows with `kind: tool_result` are trimmed to 4,000 characters and set
  `clipping: true`. Prose and tool calls are never trimmed.
- Unparseable lines are skipped and counted in `warnings`.
- `locate` returns a herdr-reported `session_path` if that file exists;
  otherwise it looks up the exact session id (never a prefix or a suffix of
  it). An empty or unsafe id (anything
  outside `[A-Za-z0-9_-]`) returns `Ok(None)`. No reader ever picks a file by
  recency.

## Claude Code

Status: confirmed. Run: `claude -p "..." --session-id <uuid> --model haiku`
with `cwd=/private/tmp/claude-501/disc`.

- Path: `~/.claude/projects/<cwd slug>/<session id>.jsonl`.
- Slug: every character outside `[A-Za-z0-9]` becomes `-`
  (`/private/tmp/claude-501/disc` -> `-private-tmp-claude-501-disc`;
  `.../monaco/.worktrees/1941` -> `...-monaco--worktrees-1941`).
- One JSON object per line. Relevant `type` values: `user`, `assistant`.
  Others seen and ignored: `attachment`, `queue-operation`, `last-prompt`,
  `atis-latch`, `system`, `cost-state`.
- `user`: `message.content` is a string (prompt) or a block list holding
  `tool_result` blocks (`tool_use_id`, `content` string or `[{type:text}]`).
- `assistant`: `message.content` blocks `thinking` (empty text, signature),
  `tool_use` (`name`, `input`), `text`. Each streamed block is its own line.
- `timestamp` (ISO 8601) on every message line becomes `at`.
- Sample: `samples/claude/<id>.jsonl` (user/assistant lines only, `usage` and
  tool-result side data dropped, thinking signature replaced).

## Codex

Status: confirmed. Run: `codex exec --skip-git-repo-check "..."`.

- Path: `~/.codex/sessions/YYYY/MM/DD/rollout-<local time>-<session id>.jsonl`.
- Line envelope: `{timestamp, ordinal, type, payload}`. `type` values:
  `session_meta`, `response_item`, `event_msg`, `world_state`, `turn_context`,
  `token_usage_record`. Only `response_item` is read.
- `payload.type` under `response_item`: `message` (`role` user, assistant,
  developer; `content[]` of `input_text` / `output_text`), `custom_tool_call`
  (`name`, `input` string), `custom_tool_call_output` (`output[]` of
  `input_text`), `function_call` (`name`, `arguments` string),
  `function_call_output` (`output`), `reasoning` (ignored).
- `developer` messages and the injected `<environment_context>` and
  `# AGENTS.md instructions for <dir>` user messages are skipped.
- The id is everything after the 19-character timestamp, so `locate` parses
  the name rather than matching a suffix (a UUID's last group would match).
- Sample: `samples/codex/rollout-...jsonl` (developer messages, reasoning,
  events, token records removed; account ids removed from `session_meta`).

## Grok (best-effort)

Status: store found and readable on this machine from older sessions. A
fresh run was not possible: `grok -p` fails with "Not signed in", so the
sample is a scrubbed copy of an existing headless session.

- Path: `~/.grok/sessions/<url-encoded cwd>/<session id>/chat_history.jsonl`
  (also `summary.json`, `updates.jsonl`, `events.jsonl`, `usage.json`).
  `~/.grok/active_sessions.json` was `[]`, so it is not a lookup source.
- `chat_history.jsonl` lines have a `type`:
  `system`, `user`, `reasoning`, `assistant` (`content` string, optional
  `tool_calls[]` of `{id, name, arguments}`), `tool_result`
  (`tool_call_id`, `content` string).
- Real prompts are `user` lines with an array `content` and a `prompt_index`.
  Harness context lines have a string `content` or a `synthetic_reason`.
- No timestamps per line, so `at` is null.
- Sample: `samples/grok/<id>/chat_history.jsonl`: the real headless session
  with system prompt, reminders and encrypted reasoning replaced, plus one
  hand-written `tool_calls` / `tool_result` pair copied in shape from a
  private session (content is synthetic).
- If the store is absent, `locate` returns `Ok(None)` and the caller reports
  `reader_unavailable`.

## Cursor Agent (best-effort)

Status: store found and readable from Cursor sessions. The `cursor-agent`
binary is not installed, so no fresh run was made.

- Path: `~/.cursor/projects/<slug>/agent-transcripts/<id>/<id>.jsonl`
  (a `subagents/` directory may sit beside it).
- Line: `{"role": "user"|"assistant", "message": {"content": [...]}}`, plus
  `{"type": "turn_ended", "status", "error"?}` between turns (skipped).
- Blocks: `{type:"text", text}` and, in transcripts from September 2026 on,
  `{type:"tool_use", name, input}` (emitted as `tool_call` rows). Tool output
  is not recorded, so there are no `tool_result` rows.
- User text is `[<timestamp>…</timestamp>\n]<user_query>…</user_query>`; only
  the query is returned.
- Older transcripts end without a final newline; newer ones end with one.
- No timestamps per line, so `at` is null.
- Sample: `samples/cursor/<id>/<id>.jsonl` is synthetic (the real transcripts
  hold private work, including secrets pasted into prompts); it follows the
  structure above. Its shapes, including the timestamp, `tool_use`, and
  `turn_ended` lines, match real transcripts checked on 2026-10-12.
- If the store is absent, `locate` returns `Ok(None)` and the caller reports
  `reader_unavailable`.

## Screen

`ScreenReader` reads a text file (the archived screen text) as one `screen`
row per non-empty line, with the same paging. Its last line is returned even
without a final newline, so it suits finished archives, not a file still being
written. `locate` only returns a path
that the caller passes in.
