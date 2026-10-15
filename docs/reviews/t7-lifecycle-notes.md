# T7 worker lifecycle notes

- ASSUMPTION: Protocol 22 has no direct setup-process launch or exit-status
  method. With coordinator approval, pod shell-quotes only the configured repo
  setup argv token by token, submits it in a dedicated setup tab, appends a
  unique exit-code sentinel, and polls that pane for the sentinel under the
  start timeout. The setup pane is never closed.
- ASSUMPTION: When a rendered preamble exceeds 8 KiB, pod writes it to a
  mode-0600 file under the Run's board directory and injects one short absolute
  path pointer. `preamble_input` is still recorded exactly once.

Coordinator-approved integration edit: removed only the nine implemented T7
commands from T0's skeleton-only `not_implemented` assertion. `worker-read`
remains there for T9.

Coordinator-approved interface split: migration 2 adds `workers.user_owned`
and `user_owned_reason`; T7 exposes `lifecycle::mark_user_owned`, and stop and
release never close a marked pane. Retain marks its pane user-owned, and a pane
found already closed at release is marked `pane_closed_outside_pod`. T8 owns
confirming whether herdr exposes a pane-input event and, if so, routing it to
this setter.

Coordinator-approved Herdr trait addition: `worktree_list` maps protocol 22
`worktree.list`. `branch:<name>` matches either the short branch or
`refs/heads/<name>`, then opens the returned path; a missing match is usage.

## Review (Opus)

### ASSUMPTION verdicts

- **Setup through a typed command and exit sentinel: corrected.** Kept the
  approach (protocol 22 has no process launch with an exit status), but the
  line typed into the setup pane assumed a POSIX interactive shell
  (`__pod_setup_status=$?` is a syntax error in fish). It now runs the
  shell-quoted argv under `sh -c '"$@"; printf "\n%s%s\n" "$0" "$?"'
  <sentinel> <argv...>`, which works from zsh, bash, and fish. The "did it
  render" check now ignores whitespace, so a soft-wrapped command line in a
  narrow pane no longer fails setup. The setup pane is never closed: release
  of a start that failed at setup leaves it alone (now tested). Setup shares
  `--timeout-ms` (default 30 s) with the other start stages; a slow real
  setup needs a larger `--timeout-ms`.
- **Oversize preamble pointer file: accepted.** The test now asserts the
  file is mode 0600 and holds the full preamble; `preamble_input` is still
  recorded once.

### Coordinator-approved items

Accepted as recorded: the t0.rs deletions, migration 2 with
`mark_user_owned`, and `Herdr::worktree_list`. One consequence to know:
because retain marks the pane user-owned, a later `worker-release` of a
retained worker archives it but never closes its tab. Reuse after retain
does not carry retain's own mark (only a real takeover is carried).

### Defects fixed

- A failure after the Dispatch row existed (tab create, store, preamble,
  inject errors, a missing coordinator pane) returned an error and left the
  Dispatch `pending` forever, blocking the Task. Every such failure now
  settles through `failed_start`; caller-pane and agent checks run first.
- A failed start with no pane stayed `terminal_state = active`, which reads
  as a live worker and blocks `task-update`; it is now `released`.
- `path:`/`branch:` used the tab returned by `worktree.open` and marked it
  pod-owned, so the agent could start in a tab the user had open and release
  would close it. Existing worktrees now get a new tab (spec 7.2).
- A stalled preamble reported `state: start_unknown` while the rows said
  `ready`/`dispatched` (spec: `start_unknown` blocks the Task). The receipt
  now carries the stored state with `outcome: dispatched_unobserved`, and the
  CLI exits 1 for it. An injection that typed nothing fails the start at
  `inject`; a paste whose confirmation or Enter failed is
  `rendered_unconfirmed`, never `failed`.
- A worker could start a worker by passing `--run`: depth now counts the
  caller's own Dispatch. Same for `dispatch`.
- `--spec` with unmet `--deps` created the Task and started anyway; it is
  now refused and the Task rolled back. `--display-name` (a worktree flag)
  was also written as the Task's display name. `--terminal` silently
  ignored `--worktree`; now a usage error. `--repo name:<n>` was always
  refused; it now resolves a configured repo by folder name.
- `worker-stop` on an accepted ending rewrote the worker to `stopped` and
  `reclaimable` (even after release); stop and abandon on an ended Dispatch
  now change nothing. The fence no longer records a fake `blocked_on`
  reason in `last_failure` and the event log. A `stop_unknown` stop retries
  its close, and stop never closes the coordinator's or a user-owned pane.
- `worker-release` on an `active` worker archived a non-final screen and
  marked it `released`; it now leaves it active. Release never closes a pane
  another Dispatch now uses (`dispatch --to` onto a reclaimable pane), skipped
  `release_pending` retries, and marked a pane pod's own stop had closed as
  user-taken-over.
- `dispatch --inject` failure left the worker `active`; it is `released`
  with receipt `outcome: failed` and exit 1.
- `worker-list` reported `scope.source: bound` for an unbound caller while
  listing every Run.
- Hollow tests: the two setup tests, the retry, stop, release and list tests
  only checked helpers or one branch; they now run the behaviour their names
  describe.

### For the merge with main (T8 and T9 are merged)

- `lifecycle.rs`, `workers.rs`, and `tests/t0.rs` conflict with main; all are
  T7-owned (t0.rs: both sides delete adjacent lines; keep both deletions).
- `archive_final_screen` here is T9's block verbatim; keep one copy.
- main's `Engine::after_commit` takes `(run, mailboxes)`: every call here
  needs `, &[]`.
- Re-point the private `inject` to `safe_input::inject_preamble`, keeping
  the oversize pointer conversion in T7. T8's `Failed` can follow a paste and
  its `Skipped` means nothing was typed, so the "typed nothing → failed start
  at inject" rule must map to T8's pre-paste `Skipped`/`Failed`, and a start
  whose agent is not yet idle is skipped by T8 rather than typed into.
- `show`/`list` still return the stubbed projection; wire `observe::project`.
