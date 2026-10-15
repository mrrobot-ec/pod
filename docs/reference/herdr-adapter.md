# herdr socket adapter

`SocketHerdr` is pod's only direct herdr client. It implements the `Herdr`
trait over protocol 22 from `vendor/herdr/schema-22.json`, as shipped by herdr
0.9.3. The engine uses only the trait and tests normally use `FakeHerdr`.

## Transport

- The socket path is `HERDR_SOCKET_PATH`, otherwise herdr's default-session
  socket under `$XDG_CONFIG_HOME/herdr` or `~/.config/herdr`.
- Each ordinary call opens a Unix socket, writes one compact JSON request plus
  `\n`, and reads one newline-delimited response. A request is
  `{"id":"pod:<pid>:<sequence>","method":"...","params":{...}}`.
- `events.subscribe` keeps its connection and buffered reader open after the
  `subscription_started` response. A frame cut by an `EventStream::next`
  timeout is kept and completed by the next call.
- Every request has a read/write deadline: 30 s, 120 s for `worktree.create`
  and `worktree.open`, and the herdr-side timeout plus 30 s for `agent.start`
  and a waiting `agent.prompt`. A missed deadline is `herdr_unreachable`
  ("herdr did not answer <method> in time").
- Response IDs must equal request IDs. Malformed or unexpected response shapes
  are `herdr_error`.
- Connect, write, read, EOF, and event-stream I/O failures are
  `herdr_unreachable`.
- Event hook commands receive their event name in `HERDR_PLUGIN_EVENT` and
  the JSON payload in `HERDR_PLUGIN_EVENT_JSON`. `pod hook event` parses only
  these data fields; an absent or unrecognized payload triggers startup-style
  reconciliation instead of being executed or interpreted as command text.

Every error response becomes `PodError { code: herdr_error, message,
next_steps: ["herdr status server"] }`, except:

- `agent_prompt_stalled` becomes `PromptOutcome::Stalled`, and so does
  `timeout` when a wait was requested (herdr reports submission before it
  waits, so the text went in but no turn was observed).
- `agent_blocked` becomes `PromptOutcome::Blocked`.
- `pane_not_found`/`target_pane_not_found`, `workspace_not_found`, and
  `agent_not_found`/`agent_name_not_found`/`agent_pane_not_found` become
  `Ok(None)` in their corresponding `get` method. A target-specific message
  containing `not found`, `does not exist`, or `unknown target` is handled the
  same way. The exact codes were observed from a live herdr 0.9.3 server
  (`pane w9:p9 not found`, `workspace w99 not found`, `agent target nope not
  found`); the protocol schema leaves `error.code` open as a string.

## Trait mapping

| Trait method | Protocol method and translation |
|---|---|
| `ping` | `ping {}` → `pong.version`, `pong.protocol`. |
| `pane_current` | `pane.current {}` → `pane_current.pane`. |
| `pane_get` | `pane.get {pane_id}` → `pane_info.pane`; confirmed missing targets are `None`. |
| `pane_process_info` | `pane.process_info {pane_id}` → shell PID, foreground PID, and the unique shell/foreground PID list. herdr does not order `foreground_processes` (0.9.3 listed `cat, sleep, bash` for a `bash -c 'sleep 30 \| cat'` job), so the foreground PID is the process whose PID equals `foreground_process_group_id`, then the first listed process, then the group ID, then the shell PID. |
| `pane_read` | `pane.read` with `source=recent_unwrapped`, `format=text`, `strip_ansi=true`, and `lines` → `pane_read.read.text`. |
| `pane_send_text` | `pane.send_text {pane_id,text}` → `ok`. This never sends Enter. |
| `pane_send_keys` | `pane.send_keys {pane_id,keys}` → `ok`; safe input uses `Enter` only after confirming pasted text. |
| `pane_close` | `pane.close {pane_id}` → `ok`. |
| `tab_create` | `tab.create {workspace_id,label,cwd,focus,env:{}}` → `tab_created.tab.tab_id` plus `root_pane.pane_id`. |
| `tab_close` | `tab.close {tab_id}` → `ok`. |
| `workspace_get` | `workspace.get {workspace_id}` → ID, label, optional worktree checkout path, and `tab_count`, `pane_count`, and `agent_status` when herdr reports them; confirmed missing targets are `None`. |
| `workspace_close` | `workspace.close {workspace_id}` → `ok`. Used by release for a space herdr opened for the placement, only when `workspace_get` shows one tab, one pane, and no agent. |
| `workspace_report_metadata` | `workspace.report_metadata {workspace_id,source:"pod",tokens:{status}}` → `ok`. |
| `worktree_create` | `worktree.create` with `cwd=repo`, `branch=name`, optional `base` and `label`, `focus=false`, `trust_repository=false` → workspace, worktree, tab, and root-pane IDs. Protocol 22 has no comment field. |
| `worktree_list` | `worktree.list {cwd,trust_repository:false}` → the repository's source checkout (`source.source_checkout_path`) and its worktrees: paths, branches, and optional open workspace IDs. `worker-start` matches `branch:<name>` against both `<name>` and `refs/heads/<name>` before opening its listed path. List rows have no tab or pane, so those fields are empty until open. |
| `worktree_open` | `worktree.open {cwd:<source checkout>,path,focus:false,trust_repository:false}` → workspace, worktree, tab, and root-pane IDs. `cwd` is the folder herdr starts from: the source checkout its listing named (`worktree.create` takes the same). Without it herdr looks at its focused workspace's repository, whatever the person is viewing, and a linked worktree is refused as the start (`linked_worktree_source`). `already_open` in the result says whether the worktree's own space existed. herdr also opens the repository's parent space, with one shell pane, when none is open, and does not say so; placement detects it with `pane.list` (`workspace_for_folder`) before and after the call. |
| `agent_start` | `agent.start {name,kind,pane_id,args,timeout_ms}` → `agent_started.agent`; then `pane.process_info` supplies the foreground PID. |
| `agent_get` | `agent.get {target}` → `agent_info.agent`; confirmed missing targets are `None`. |
| `agent_list` | `agent.list {}` → each `agent_list.agents[]`, with `pane.process_info` supplying its PID. |
| `agent_prompt` | `agent.prompt {target,text,wait}` → `Submitted`; the semantic error outcomes are listed above. `wait_ms` becomes `wait:{timeout_ms,until:["working","blocked"]}`, so herdr returns once the turn is observed instead of when it settles. herdr types the text **and Enter** as one submission, so it is the primary path for the brief pointer and every nudge: `Submitted` is `delivered`, `Blocked` is `skipped`, and `Stalled` (Enter already pressed, no turn seen) is `rendered_unconfirmed`, never a second paste. Any other herdr error (an agent herdr cannot prompt) falls back to the 8.2 safe-input procedure, which uses `pane_send_text`, `pane_read`, then `pane_send_keys(["Enter"])`; a transport error (`herdr_unreachable`) fails the input with no paste, since the text may already have gone in. A nudge from the tab bar passes no `wait_ms`, so herdr answers at submission and the bar's 5-second timeout holds. |
| `agent_read` | `agent.read` with `source=recent_unwrapped`, text format, ANSI stripped, and `lines` → `pane_read.read.text`. |
| `events_subscribe` | Subscribes to `pane.exited`, `pane.closed`, and one `pane.agent_status_changed` per requested pane. herdr delivers the global ones under their `EventKind` names, `{"event":"pane_exited","data":{"type":"pane_exited","pane_id","workspace_id"}}` (likewise `pane_closed`), and the pane-scoped one as `pane.agent_status_changed`; the adapter accepts both spellings of each. Global close/exit events are filtered to the requested pane IDs in the adapter. |

Agent responses map `agent_status` directly. `agent_session.kind=id` fills
`session_id`; `kind=path` fills `session_path`. Protocol 22 has no observation
timestamp, so the adapter records receipt time. `state_labels` values become
`needs_input` reasons (falling back to the key for an empty/non-string value).

## Live behaviour on herdr 0.9.3

Observed against an isolated `herdr server` (own `HOME`,
`XDG_CONFIG_HOME`, and socket), driving `SocketHerdr` directly:

- A shell that exits produces `pane_exited` only; herdr then closes the pane
  and its tab itself (a later `tab.close` is `tab ... not found`) without a
  `pane_closed` event. `pane.close` produces `pane_closed`.
- `pane.current` from outside any pane returns the focused pane, not an
  error, so it identifies the caller only when run inside a herdr pane.
- Per herdr's own `agent prompt` help, a waiting prompt returns `timeout` when
  the caller timeout ends first and `agent_prompt_stalled` when no
  `working`/`blocked` state follows within 5 s of submission. No agent ran on
  that server; the live test `live_agent_prompt_stalled_is_reported` (below)
  covers this case.

## Section 19 confirmations on herdr 0.9.3

0. **Approval prompts.** herdr 0.9.3 reports a Codex approval prompt as
   agent status `blocked` with no state label (seen live 2026-10-13), so
   `record_herdr_state` records `needs_input` reason `blocked` from the
   status alone; a label, when herdr sends one, is kept as the reason.

1. **Item 1.** Protocol 22 exposes nullable `agent_session` with an `id` or
   `path` reference, and the adapter preserves either. herdr does not always
   report it, so pod keeps the spec's fallbacks: a Claude worker uses the
   session ID pod assigned at launch, and a worker with no proven session
   reads its screen with the reason `session_unproven`.
2. **Item 2.** `pane.process_info` exposes the shell PID, the
   foreground process group ID, and every foreground process; live, a
   `bash -c` job reported its leader and both children, and the adapter's
   `pids` carries all of them for ancestry checks. When pod cannot walk the
   process chain (for example inside an agent sandbox) and the pane's process
   list does not show both processes, identity falls back to the terminal ID
   alone, with a warning.
7. **Item 7.** On a live 0.9.3 server, `pane_send_text(pane,
   "exit")` left `exit` unsubmitted at the shell prompt, `pane_read(pane, 5)`
   returned it, and only `pane_send_keys(pane, ["Enter"])` ran it (the pane
   then exited). This supports paste, confirm, then Enter; any failed proof
   still withholds Enter as required by the safe-input rule.
8. **Hook transport.** Per herdr's official event-hook
   documentation, hook processes receive `HERDR_PLUGIN_EVENT` and
   `HERDR_PLUGIN_EVENT_JSON`. `pod hook` consumes those variables and falls
   back to full reconciliation when the payload cannot identify a supported
   event. The payload is the protocol's `EventData`, so an agent status
   change carries `agent_status` and `state_labels`, as on the
   event stream. On that stream, `EventStream::next(Duration::ZERO)` returns
   `None` without reading the socket; a waiter reads with a short nonzero
   timeout.
9. **Agent input screens.** Observed on an isolated 0.9.3 server
   with Claude Code 2.1.296 and Codex 0.160.1, through `pane read` (plain
   text): the input line is not the last line (a border and footer follow
   it); an empty input shows placeholder text after the marker, styled dim
   only in ANSI (`❯ Try "write a test for <filepath>"`,
   `› Ask Codex to do anything`); typed text replaces it on the same line.
   Claude shows a multi-line paste as `[Pasted text #1 +N lines]` (N counts
   newlines); Codex shows pastes over 1,000 characters as
   `[Pasted Content N chars]` and shorter ones in full with continuation
   lines indented. `agent_status` stays `idle` while unsent text sits in the
   input, so only the screen proves it empty. Both agents report `blocked` at
   their trust dialogs. Safe input (`engine/safe_input.rs`) recognises these
   forms; anything else withholds Enter.
10. **User takeover (spec 7.4).** Protocol 22 has no event for
    input a person types into a pane (its pane events are created, closed,
    exited, focused, moved, updated, output changed, agent detected, agent
    status changed; `pane.send_input` is a method, not an event), so typing
    is seen only through the screen. The styled screen read
    (`pane_read_styled`) that safe input makes before every prompt or paste
    drops the agent's dim placeholder; any text left in the input that pod
    did not type is an interruption: pod leaves the pane alone, records
    `user_typing` on the Dispatch, and raises attention `interruption`
    (`requiresAction: false`). A later read that finds the input bare clears
    it, and a pending nudge then goes through. Typing alone does not make
    the worker user-owned. The takeover pod treats as ownership is a close:
    a `pane_closed` hook, a startup reconcile, or a release that finds the
    worker's pane gone while pod was neither stopping nor releasing it calls
    `lifecycle::pane_gone`, which marks the worker `user_owned`
    (`pane_closed_outside_pod`); a stop whose close was unconfirmed
    becomes `stopped` instead (state machines W10). `worker-retain` marks it too.
    Stop and release never close a user-owned pane, and the worker stays
    `reclaimable` until `worker-release` accounts for it.
11. **Scripted pi input.** On a real isolated 0.9.3
    pane, `recent_unwrapped` dropped a middle segment from one long echoed
    shell-input line even though the process received the complete paste.
    The no-model e2e `pi` therefore redraws `[Pasted Content N chars]`, and
    safe input accepts it only when N exactly matches the submitted character
    or byte count. A mismatch still withholds Enter. Protocol 22 also
    acknowledges `pane.send_text` before the agent necessarily redraws, so
    safe input re-reads for at most 100 ms (never pasting again), and reads
    the pasted line count plus 20 lines, capped at 2,000, so a long
    preamble's prompt line stays inside the snapshot.

## Fake

`FakeHerdr` records every call, stores panes/screens/agents, scripts agent
status and prompt outcomes in FIFO order, records submitted screen text when
`Enter` is sent, and delivers queued events in FIFO order, filtered to the
subscribed panes like the socket stream (events for other panes stay queued).
No queued event for those panes returns `None` at once, modeling a bounded
subscription timeout.

Under `POD_FAKE_HERDR=1` (test builds only) a `pod` subprocess gets
`FakeHerdr::from_env`: `POD_FAKE_PANES` (`pane_id=terminal_id,...`) seeds panes,
so with `HERDR_ENV=1` and `HERDR_PANE_ID` the caller resolves as in a real
pane, and `POD_FAKE_AGENTS` (`name=kind:pane_id:pid,...`) seeds idle agents.
`POD_FAKE_AGENT_START_ERROR` makes every agent start fail with its value, and
`POD_FAKE_RENDER_SENT_TEXT=0` keeps pasted text off the screen (an unconfirmed
paste). Tabs and panes such a fake creates are numbered from the process ID,
so separate `pod` processes on one store never reuse a terminal ID.

## Live tests

`tests/herdr_socket.rs` marks its live tests `#[ignore]`. Run them with
`POD_E2E=1 HERDR_SOCKET_PATH=<socket> cargo test --test herdr_socket --
--ignored`. The socket path must fit `sun_path` (about 100 bytes), so a
symlink under `target/` helps for a deep scratch directory.
`live_agent_prompt_stalled_is_reported` also needs `POD_E2E_AGENT` naming a
detected agent that will not start a turn (for example one stopped with
`SIGSTOP`); it waits 10 s so herdr's 5 s stall check, not the caller timeout,
decides the outcome.
