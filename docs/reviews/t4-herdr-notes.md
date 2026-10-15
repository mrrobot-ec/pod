# T4 herdr adapter notes

- ASSUMPTION: protocol 22 has no agent observation timestamp, so
  `AgentInfo.observed_at` is the local receipt time.
- ASSUMPTION: `AgentInfo.pid` is the first `foreground_processes` PID, falling
  back to the foreground process-group ID and then the shell PID. The complete
  PID vector is the unique shell PID plus foreground PIDs.
- ASSUMPTION: `state_labels` string values are `needs_input` reasons; an empty
  or non-string value falls back to its key.
- ASSUMPTION: `worktree.create` maps `repo` to `cwd`, `name` to `branch`,
  `base_branch` to `base`, and `display_name` to `label`. Protocol 22 has no
  worktree comment field, so `comment` is not sent.
- ASSUMPTION: workspace sidebar status uses metadata source `pod` and token
  name `status`.
- ASSUMPTION: close and exit subscriptions are global in protocol 22, so the
  adapter filters their events to the requested pane IDs.
- ASSUMPTION: `SocketHerdr::connect` validates reachability immediately, while
  each request uses a fresh connection; only a subscription keeps one open.
- ASSUMPTION: the ignored stalled-prompt live test uses `POD_E2E_AGENT` to name
  the isolated test agent because the plan specifies no agent-name variable.

The coordinator approved the first five field mappings above and directed the
adapter to translate confirmed `pane`, `workspace`, and `agent` not-found
responses to `Ok(None)`. It also directed `agent_prompt_stalled` and
`agent_blocked` to typed `PromptOutcome` values rather than `herdr_error`.

Integration note: this branch predates T12 and its `src/skills.rs` is empty.
The coordinator will register `herdr-adapter.md` in T12's `REFERENCES` array
when merging; it explicitly directed T4 not to copy the newer file into this
branch.

## Review (Opus)

Checked against spec 3.1, 3.2, 7.1 step 5, 8.2, 19 items 1/2/7, the vendored
schema, `docs/reference/errors.md`, and a live, isolated herdr 0.9.3 server
(own `HOME`, `XDG_CONFIG_HOME`, and socket in the review scratch directory)
driven through `SocketHerdr`. All eight listed tests are present under their
plan names; plan signatures are unchanged; only `herdr_error` and
`herdr_unreachable` are used.

Verdicts on the ASSUMPTION entries:

1. `observed_at` is receipt time: **accepted**. Protocol 22 `AgentInfo` has
   no timestamp.
2. `pid` is the first foreground PID: **corrected**. Live, herdr listed
   `cat, sleep, bash` for a `bash -c 'sleep 30 | cat'` job; the first entry is
   a child, not the process the shell started. The adapter now picks the
   process whose PID equals `foreground_process_group_id`, then falls back as
   before. `pids` was already right. Test
   `socket_process_info_prefers_process_group_leader`.
3. `state_labels` values are `needs_input` reasons: **accepted**. The schema
   types the values as strings; not exercised live (no agent ran).
4. `worktree.create` field mapping and no comment: **accepted**. Matches
   `WorktreeCreateParams`; not exercised live, since it creates a git worktree.
5. Metadata source `pod`, token `status`: **accepted**. Live
   `workspace.report_metadata` returned `ok`; whether the sidebar shows the
   token was not checked.
6. Close/exit subscriptions are global, filtered in the adapter:
   **accepted, with a correction to event parsing**. Live, herdr delivers
   them as `{"event":"pane_exited",...}` / `"pane_closed"` (schema
   `EventKind`), not `pane.exited` / `pane.closed`, so the adapter dropped
   every exit and close event. It now accepts both spellings of all three
   events. Test `socket_events_use_observed_wire_names_and_filter_panes`.
7. `connect` checks reachability, one connection per request: **accepted**.
   Added read/write deadlines, which were missing: 30 s by default, 120 s for
   worktree calls, and herdr's own timeout plus 30 s for `agent.start` and a
   waiting `agent.prompt`. Before this, a wedged server hung `pod` forever.
8. `POD_E2E_AGENT` names the stalled-prompt agent: **accepted**, but the wait
   is now 10 s rather than 50 ms. With 50 ms herdr's caller timeout fires
   before its 5 s stall check, so the test proved nothing about stalls.

Coordinator directives: not-found becomes `Ok(None)`. **Accepted**, and the
exact codes `pane_not_found`, `workspace_not_found` and `agent_not_found` were
seen live. `agent_prompt_stalled`/`agent_blocked` are typed outcomes:
**accepted, extended**. `wait_ms` now sends
`until: ["working","blocked"]`. Without it herdr waits for the turn to
*settle*, so any observed turn longer than `wait_ms` came back as a `timeout`
`herdr_error` after the text was already submitted. A `timeout` with a wait
requested now maps to `Stalled` (text in, turn unobserved; never resend).

Other defects fixed (commit d059db0):

- A frame split across an `EventStream::next` timeout was dropped and the
  next call failed to parse. The stream now keeps the partial bytes
  (`socket_event_frame_split_across_timeout_is_kept`).
- On macOS, setting `SO_RCVTIMEO` after herdr closes its end fails with
  `EINVAL`, which turned events herdr had already sent into
  `herdr_unreachable`. The stream now still drains them. The new events test
  caught this.
- A request is now one `write_all`, so a broken pipe is `herdr_unreachable`
  rather than an "encode" `herdr_error`.
- `FakeHerdr` now applies the subscription's pane filter, as the socket stream
  does (`fake_events_are_filtered_to_subscribed_panes`).

Open, outside T4 or not fixed:

- Spec 7.1 step 6 says to inject through `herdr agent prompt` and also to
  use the 8.2 paste, confirm, Enter procedure. herdr's `agent.prompt` types
  the text and Enter as one submission, so it cannot do the confirm step.
  T7/T8 must build safe input from `pane_send_text`, `pane_read` and
  `pane_send_keys`. This is recorded in `herdr-adapter.md`.
- `agent_get` and `agent_list` call `pane.process_info` for each agent. A
  pane that closes between the two calls makes the whole call a
  `herdr_error`; a retry clears it.
- `pane_current` outside a herdr pane returns the *focused* pane, not an
  error. Callers needing caller identity must check `HERDR_ENV` first
  (spec 3.4).
- Not verified live: `agent.start`, `agent.prompt`, `agent.read`,
  `agent_status_changed` events, `worktree.*`, `agent_session` (item 1), and
  the parent-chain walk from an agent shell (item 2). Items 1 and 2 stay
  FALLBACK.
