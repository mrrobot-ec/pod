# T9 worker-read and archive: assumptions

- ASSUMPTION: the engine function cannot be `workers::worker_read` because the
  CLI handler (fixed by `cli/mod.rs`) already has that name. It is
  `cli::commands::workers::read::worker_read(engine, &dispatch, source, limit,
  cursor)`, with `Source`, `ReadReceipt`, and `Clipping` in the same module.
- ASSUMPTION: the terminal source has no dedicated error code, and
  `errors.md` is not mine to edit. A forced `--source terminal` with no live
  pane and no archived screen is a `usage` error (exit 2). A forced
  `--source transcript` with no readable transcript is also `usage`.
- ASSUMPTION: session proof, in spec order. (1) herdr: the `agent_list` entry
  whose `pane_id` equals the worker's pane and that reports a `session_id` or
  `session_path`. (2) The worker row has `session_id` and `transcript_proof`
  of `pod_assigned` (T7's launch) or `herdr` (stored by a later observation);
  `transcript_path` is passed to `locate` as the session path. A `session_id`
  without a proof value is not proof. T7 must write those proof values.
- ASSUMPTION: `fallbackReason` is `session_unproven` (no proof),
  `reader_unavailable` (proof, but no reader for the agent kind or `locate`
  found no file), `no_terminal` (nothing to read: no live pane, no archive).
  `no_terminal` returns `source: terminal` with no rows and exit 0.
- ASSUMPTION: a herdr failure during `auto` is a warning and `auto` goes on
  to the archive; for `--source terminal` it is the herdr error.
- ASSUMPTION: the terminal source reads the live pane (`pane_read`, 2000
  lines) while herdr knows the pane, and otherwise the `final_screen`
  archive, so a released worker stays readable. Rows are the non-empty,
  right-trimmed lines, `role` and `kind` `screen`, no `tool`, no `at`.
- ASSUMPTION: the terminal cursor is `screen-<hash>:<line>`, the hash being
  SHA-256 (first 8 bytes) of `pane:<id>` or `archive:<dispatch>`; the offset
  counts non-empty lines of the current snapshot, so a screen that scrolled
  can skip or repeat lines, and a screen shorter than the offset is
  `source_changed`. Transcript cursors are the readers' own. A cursor of one
  kind given to the other source is `source_changed`; this includes `auto`
  when the source changed since the cursor was issued.
- ASSUMPTION: `--limit` defaults to 50 and is capped at 500; 0 is `usage`.
- ASSUMPTION: `clipping` is `{"rowsTrimmed": n}` as in the reference. The
  readers return a bool, so `n` counts `tool_result` rows at the trim limit.
  The terminal source never clips.
- ASSUMPTION: `fallbackReason` is omitted from the receipt when there is no
  fallback.
- ASSUMPTION: `archive_final_screen` archives `herdr.pane_read(pane, 2000)`
  as `kind = 'final_screen'` once: an existing archive is kept and the pane
  is not read again. No pane, or a pane herdr no longer knows, archives
  nothing and succeeds, so release is not blocked. Other herdr errors are
  returned so that T7 can decide whether to close the pane.
- `tests/t0.rs` (not mine) listed `worker-read` as a `not_implemented`
  command; the single entry is removed, as earlier waves did for theirs.
- Not covered: reading a live Codex, Grok, or Cursor transcript through
  herdr's session path (T5's samples cover the readers; `worker-read` is
  tested with Claude and with a kind that has no reader).

## Review (Opus)

Verdict per ASSUMPTION, in order:

1. Engine function at `workers::read::worker_read`: accepted. The plan's T9
   section now names the path and `Clipping`.
2. A forced source with nothing to read is `usage` (exit 2): accepted. No code
   in `errors.md` fits, and adding one would edit files T7 and T8 also touch.
   `commands-workers.md` now says so.
3. Session proof order and the `pod_assigned` / `herdr` values: accepted, and
   extended. Nothing wrote `herdr`, and herdr reports a session only while the
   pane lives, so a released Codex, Grok, or Cursor worker lost its transcript
   (spec 11: transcripts persist after release). `archive_final_screen` now
   copies herdr's session report into `session_id`, `transcript_path`, and
   `transcript_proof = 'herdr'` when no proof is stored; a `pod_assigned` proof
   is never replaced. A stored proof may now carry a path without an ID. When
   the Dispatch has no `agent_kind`, herdr's detected kind picks the reader.
   T7 still has to write `pod_assigned` with `session_id` when it launches
   Claude with `--session-id` (the plan's T9 section records this).
4. `fallbackReason` values: accepted. Corrected one edge: a `--cursor` given
   when there is no screen left (`no_terminal`) was silently dropped and now
   returns `source_changed`. `no_terminal` is documented in
   `commands-workers.md` and `errors.md`.
5. herdr failure under `auto` is a warning: accepted, and made consistent. A
   `pane_read` failure after a successful `pane_get` was a hard error under
   `auto`; it is now a warning like the lookup failure.
6. Terminal source reads the live pane, else the archive: corrected. Once the
   archive exists the worker has been released, and a pane left open
   (user-taken-over, pre-existing) may now be someone else's, so the terminal
   source reads the archive and the transcript proof ignores herdr's live
   report. Test: `released_worker_reads_its_archive_not_the_pane_left_open`.
7. Terminal cursor `screen-<hash>:<line>`: accepted with its stated limit (a
   scrolling live screen can skip or repeat lines). A cursor taken on the live
   pane is `source_changed` once the worker is released and reads the archive;
   the `nextSteps` text covers the restart.
8. `--limit` 50 by default, capped at 500, 0 is `usage`: accepted. The cap is
   silent.
9. `clipping.rowsTrimmed` counts `tool_result` rows at the trim limit:
   accepted as an approximation. When one row was trimmed, an untrimmed row of
   exactly 4000 characters is also counted; an exact count needs T5's `Page`
   to report it.
10. `fallbackReason` omitted when there is none: accepted (the contract
    compare treats Orca's `null` as compatible).
11. `archive_final_screen` archives once; no pane, or an unknown pane, is
    success with nothing archived: accepted, with the proof capture of item 3.
    A worker whose pane the user closed before release has no archive and no
    herdr proof left; `worker-read` returns `no_terminal` for it.
12. `tests/t0.rs` entry removed: accepted.
13. Not covered (live Codex, Grok, Cursor through herdr): partly covered now.
    `release_keeps_herdr_session_proof_so_the_transcript_stays_readable`
    reads the Codex sample through the stored herdr proof after release.

Also fixed in review: the `worker-read` contract sample (`ignore "T9"`) was
left ignored. Its setup ran `worker-start` (T7), so it now seeds the worker,
and a `seed_worker` step can bind `{dispatch}`. The remaining differences are
pod's documented receipt shape (spec 11, `commands-workers.md`) and are
recorded in `allowed-differences.toml`. Orca's `dispatchId`, `provider`,
`sourceIdentity`, `sourceExact`, `status`, and `projection` are not in pod's
receipt; adding them would be a change to the reference, not a T9 defect.
