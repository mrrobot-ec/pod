# Orca and OpenRig compared with pod

Written 2026-10-14 by worker `research-orca` (task_a3cb6aff6845). Read first: pod README, spec sections 1 to 3, 7, 9, 11 (and 14 for the Orca differences table), and `docs/reviews/2026-10-12-implementation-notes.md`. No pod file was edited.

**Reading note on evidence.** Orca's docs site `orca.build` refused connections from this machine; `onorca.dev` served the same pages. The GitHub releases page prints dates without a year (for example "10 Oct"); this report assumes 2026 because the sequence runs from v1.4.217 (29 Sep) to v1.4.224 (10 Oct) and the page footer is 2026. Only the first 100,000 characters of Orca's releases page and of OpenRig's were read in full; the Orca remainder was checked for 1 to 14 October items and held only v1.4.219. Anything not read is marked below. I could not read Orca's or OpenRig's source, so claims about internals come from docs and release notes, not code.

## 1. Orca today

**What it is.** An open-source (MIT) "agent development environment" from Stably: an Electron app that runs many CLI agents side by side, each in its own git worktree. The repository shows about 92.2k stars and 5.9k forks. It lists 30+ supported agents, a built-in Chromium browser with "Design Mode", GitHub, GitLab, Linear and Jira task views, SSH worktrees, a phone companion (iOS and Android), account switching with usage display, Computer Use, and a CLI (`orca`) that agents and scripts use to drive the app. (github.com/stablyai/orca, fetched 2026-10-14.)

**Orchestration is a layer on top, and the docs call it experimental.** It must be switched on under Settings, Experimental, and `orca status --json` must succeed first. The documented model (onorca.dev/docs/cli/orchestration, fetched 2026-10-14):

- **Run**: durable namespace and the coordinator's inbox; it never schedules or places workers.
- **Task**: spec plus dependencies; status `pending | ready | dispatched | completed | failed | blocked`. Tasks form a DAG.
- **Dispatch**: one attempt of a task on a terminal; the lifecycle authority for `worker_done` and `heartbeat`. Worker messages carry both `taskId` and `dispatchId` so a stale retry cannot complete the wrong dispatch.
- **Messages**: `status`, `dispatch`, `worker_done`, `escalation`, `question`, `heartbeat`. `check` returns the oldest unacknowledged delivery first; `--peek` and `--all` do not consume; a wait writes heartbeat JSON to stderr every 15 s.
- **Group addresses**: `@all`, `@idle`, `@claude`, `@codex`, `@worktree:<id>` and others, never valid for `worker_done` or `heartbeat`.
- **Decision gates**: `gate-create` blocks a task on a question with options; `gate-resolve` records the answer.
- **Worker commands**: `worker-start` (`--worktree current | new-child | new-top-level`, `--setup run`, `--model`, `--effort`, `--retry-of`), `worker-show`, `worker-read`, `worker-stop`, `worker-release`, `worker-retain`, `worker-abandon`, plus `dispatch --inject`, `dispatch-show`, `task-list`, `task-update`, `reset`.
- **Remote placement ("federated workers")**: `worker-start --on <host> --repo ... --worktree new-top-level`; later commands route by Dispatch ID. Antigravity supervision is local only.
- **Worker contract**: preamble tells the worker to send `worker_done` exactly once even on failure, heartbeat periodically, and use `orca orchestration ask` instead of a local prompt.
- **Legacy commands** `run`, `run-stop`, `coordinator-start`, `coordinator-stop` "now have no effect" and only print recovery text. So Orca has **no automatic coordinator loop today**; the coordinator is always an agent you start. This makes pod's spec line "automatic coordinator loop: present in Orca, absent in pod" (section 14) out of date.
- **Skills**: the real reference ships inside the binary (`orca skills get orchestration`); the published skill page is a stub that tells agents not to guess subcommands. (claudeskills.info mirror of the stub, fetched 2026-10-14.)

**Scheduled automations** (separate from orchestration): presets (`hourly`, `daily`, `weekdays`, `weekly`), cron or RRULE, `--timezone`, a `--precheck` shell command that can skip a run, `--missed-run-grace-minutes`, `--reuse-session`, `--extra-agent-args`, manual `orca automations run`. Runs target a repo, an existing workspace, or a project on a connected host. The docs do not say whether an automation can start an orchestration Run. (onorca.dev/docs/cli/automations.)

**Release cadence.** Seven stable releases in the 12 days to 2026-10-10, so the contract pod copies is moving. The next section lists what changed.

### 1.1 Orca releases since 2026-10-01

Source for all rows: github.com/stablyai/orca/releases (fetched 2026-10-14). v1.4.224 is marked Latest.

| Version | Date | Orchestration-relevant change |
|---|---|---|
| v1.4.219 | 02 Oct | A worker's `worker_done` is no longer rejected after its conversation is compacted. `worker_done` is retried for about two minutes if Orca is briefly unreachable (PR #23984). Reports are accepted without the dispatch capability (PR #23982); Orca stops minting and printing the capability (PR #23994). `worker-abandon` always settles a stuck worker and records who did it (PR #23983). |
| v1.4.220 | 04 Oct | Antigravity, Cline and Prime Agent readiness is read from the live screen. Agents can learn their own `orca_session_id`. Claude Stop ends its process. Optional SSH runtime needs no Node or compiler. |
| v1.4.221 | 05 Oct | `worker-start` accepts `max` and `ultra` effort for Codex models Orca does not list yet. Codex's "Implement this plan?" menu and open agent questions now count as **blocked**. OMP 18.4 workers start. Security: v1.4.219 and v1.4.220 left `~/.copilot/config.json` world-readable; fixed. |
| v1.4.222 | 07 Oct | An OpenCode worker gets its task only after OpenCode can submit it. A worker can use a requested model without changing the default. Usage menu shows Claude credits, OpenCode Zen balance, Codex credits. |
| v1.4.223 | 08 Oct | Open files, live agent tabs and resume offers survive SSH reconnects. Grok and Claude (API-key users) open as structured chats. GitHub Projects boards as kanban. |
| v1.4.224 | 10 Oct | Model and effort selection works for Claude, Codex, Grok, CodeBuddy, Cursor, OMP. SSH hosts run a **managed Orca server** installed on connect, with update, roll back, Recover and Forget. Several PRs or tasks can attach to one workspace. Automations can set model and effort per run. Known issue: `orca` on an SSH host reaches only that host's terminals unless "Allow this host's orca CLI to control Orca" is enabled. |
| mobile-android v0.0.55 | 13 Oct (pre-release) | Phone screens come from the paired desktop; messages sent while an agent is busy queue in order and failed sends are marked "Not sent". |

**What changed the picture.**

1. **Dispatch capability is going away.** v1.4.219 removes it from output and accepts reports without it. pod already ignores `--dispatch-capability` (spec 3.4), so no change is needed, but pod's contract samples (35 in `allowed-differences.toml`) were captured before this; a recapture would show the field gone. Low risk, worth one check.
2. **`worker_done` hardening.** Orca now retries a report for about two minutes and tolerates a compacted conversation. pod has no "runtime unreachable" case for the report itself (a local SQLite write), but it has the sibling problem: a worker whose context was compacted may forget the preamble. Nothing in the pod notes tests that.
3. **Blocked detection widened.** Orca counts Codex's plan menu and open agent questions as blocked. pod treats a bare herdr `blocked` as `needs_input` (live findings item 3), which covers the same screens only when herdr reports them. Orca reads the screen itself for several agents.
4. **`worker-abandon` records the actor.** pod's abandon fences and "touches no process"; the `events` table has an `actor` column, so this is likely already satisfied, but I did not verify it in code.
5. **Remote is now a managed server, not just SSH.** Orca's remote story got bigger in the same twelve days. pod's v1 exclusion of `--on` is now a wider feature gap than when the spec was written on 2026-10-05.
6. **Usage in the UI** (Claude credits, Codex credits) shipped in v1.4.222. pod's `pod usage` tab-bar entry already covers both accounts, so that is parity.

## 2. OpenRig today

**What it is.** Open-source (Apache 2.0), TypeScript, npm package `@openrig/cli`, repository `mvschwarz/openrig`. Node 22 or 24 and tmux on macOS or Linux (Windows via WSL2). The README shows about 7.4k stars and 569 forks; the earlier pod review recorded about 5.3k on 2026-10-05, and one third-party directory still lists 976, so treat star counts as unreliable. Authorship is reported inconsistently (news sites say the developer "mvschwarz", a Show HN post says "mschwarz", EveryDev.ai says Esoteric Labs); I could not reconcile it.

**Architecture.** A local daemon (Hono HTTP over domain services), CLI `rig`, terminal UI, and MCP server, over SQLite and tmux. Not an orchestration contract like Orca's; it is a **persistent team runtime**.

- **Topology as code**: a RigSpec in YAML defines pods, seats, edges and continuity policy; `rig up` boots it. AgentSpecs and portable RigBundles share teams across machines. Built-in teams `starter` (builder plus reviewer), `workshop`, `factory` (seven agents), `code-review`, `research`, `pm`.
- **Seats**: a stable address like `dev-build@starter`; the conversation inside can change while identity and context persist.
- **Queues and messaging**: `rig send`, `rig broadcast`, `rig chatroom`, `rig queue list`. "Sending a message does not itself create a queue item; the builder records the task."
- **Recovery**: `rig down --snapshot`, `rig up <name>` restores with per-node outcomes (resumed, fresh-primed, awaiting-decision, failed).
- **Activity**: hook relays report event metadata, not prompt text or tool arguments; a Feed view; context and token usage are recorded.
- **Adoption**: `rig discover` and `rig adopt` bring existing tmux sessions under management. herdr or cmux can show the terminals.
- **Agents manage their own topology** through MCP tools (`rig_up`, `rig_ps`, `rig_send`, `rig_chatroom_send`). A typing guard holds automatic messages for a seat you are typing into.
- **Slack** can be connected through a user-created app (replies kept in thread, retry backlog, catch-up after disconnect).
- **Machine changes**: install and launch write provider hooks, trust settings and skills into `~/.claude`, `~/.agents`, `~/.codex/config.toml`, `~/.claude.json`; the README tells you to back them up first.

**Releases since 2026-09-25** (github.com/mvschwarz/openrig/releases, fetched 2026-10-14; year assumed 2026):

| Version | Date | Notable |
|---|---|---|
| v0.6.4 | 02 Oct | RC promoted; web UI off by default; daemon checks request host and origin. |
| v0.6.5 | 04 Oct | Per-seat reasoning `effort` for Claude and Codex; `rig ps --resources`; fleet performance work; compaction waits for a restore map. |
| v0.6.6 | 07 Oct | Kernel operator greets new users and launches a team; teams installable from a GitHub folder link; Slack catch-up. |
| v0.6.7 | 08 Oct | `rig setup` installs **herdr** by default; `rig doctor` checks Claude and Codex installs and credentials; fewer permission prompts. |
| v0.6.8 | 09 Oct | `rig workflow validate` catches steps that can never finish; seats that stay quiet after compaction are restored. |
| v0.6.9 | 11 Oct | Cross-host sends report "Delivery UNCONFIRMED" when the outcome is unknown; `rig chatroom wait` survives daemon restarts. |
| v0.6.10 | 12 Oct (Latest) | Messages reach Codex seats behind a launcher and Claude seats showing a faint suggested prompt; `rig launch` no longer reports an exited agent as running. Two DB migrations (099, 101). 56 changes, most from outside contributors. |

OpenRig now treats herdr as a first-class host. That makes it the nearest competitor to pod on pod's own platform, and a source of live-herdr bug reports pod can learn from.

## 3. pod versus Orca

pod's stated goal is Orca's workflow without Orca's Electron app. The comparison is therefore feature by feature against that contract.

### What Orca's orchestration offers that pod does not (today)

| Orca | pod | Weight for this user |
|---|---|---|
| Remote and federated workers (`--on <host>`, managed Orca server, reconnect survival) | Refused with `remote_unsupported`; out of v1 | Low now (all work is local), rising |
| Scheduled automations (cron, RRULE, precheck, missed-run grace, reuse session) | None | Medium; pod has no timer by design |
| Always-on runtime: the app is a daemon, so mail, nudges and `worker_done` retries do not depend on a command or a herdr event | No daemon; nudges happen only on a herdr idle hook or a mutating command at least 60 s later | **High** (see section 6) |
| Group addresses (`@all`, `@idle`, `@claude`, `@codex`, `@worktree:<id>`) | Not in the spec's `send` flags | Low |
| Task IDs in terminal output are clickable and focus the worker | Board (herdr-dagr) and sidebar status only | Low |
| GUI: kanban, task views for GitHub, GitLab, Linear and Jira, phone companion | Board export for dagr; no phone | Low |
| 30+ agents with launch flags for model and effort across Claude, Codex, Grok, CodeBuddy, Cursor, OMP (v1.4.224) | Built-in launch mapping for `codex` and `claude` only; other kinds reject `--model`, `--effort`, `--yolo` | Low (user runs codex and claude) |
| Screen-based readiness for Antigravity, Cline, Prime Agent; structured chats | Readiness comes from herdr's agent state only | Low |
| Transcript readers for all supported agents | Claude, Codex, Grok, Cursor; others fall back to screen text | Low |
| `worker_done` retried for about two minutes when the runtime is unreachable | Not needed for local SQLite, but a worker whose `pod` call fails has no retry story | Low |
| Browser, Computer Use, artifacts, accounts | Deliberately out | None |

### What pod does that Orca does not

All from spec section 14 and the README; the source for Orca's side is the Orca docs above.

- **No app, no daemon.** Runs inside herdr as one binary; every command is a short transaction.
- **Required closure reasons** on a failed `worker_done` (`no_follow_on`, `handed_off`, `blocked_on`, `denied`, `canceled`, `escalated`, `superseded`), with a circuit breaker that treats `denied` and `canceled` as immediate failure. Orca's `--outcome` alone says that something failed, not why.
- **Three-axis observation** (`activity`, `presence`, `needs_input`) with the evidence source recorded, and `worker-list` showing a literal `nextAction`. Orca documents two layers (agent-status hooks and PTY).
- **Safe pane input**: paste, confirm on screen, then Enter, with a recorded outcome. Orca types then Enter.
- **Bounded wake ladder**: five nudges per mailbox, store-derived so restarts keep the count, then attention `unreachable`. Orca: one nudge then re-offer.
- **Escalation routing**: `ask --to dispatch:<id>` to a specialist with the coordinator copied; escalations require `--evidence` and `--recommendation`; user gates need evidence, options and a recommended default.
- **`check --wait` wakes on attention** (`wokeOn: attention`), not just on messages, so a stuck prompt does not wait out a 10-minute timeout.
- **Failed-dependency visibility**: dependants stay `pending` (as Orca) but are listed under `waiting_on_failed_dependency` with a next action.
- **Presets** (`terra`, `sol`) with model, effort, fast and yolo, and trust-file writes for `--yolo` launches. Orca has executable aliases only.
- **Release archives the final screen** for every agent kind, and `pod usage` shows both accounts' rate-limit windows on herdr's tab bar.
- **Identity by terminal ID plus agent-process ancestry**, with no environment injection at launch.
- **Mutation receipts** on every mutating command, so a lost reply is recoverable with `request-show`.

### Contract risk

pod copies a moving target. Orca calls orchestration experimental, ships its real reference inside the binary, and changed the worker-report contract on 2 Oct. pod's 35 contract samples include seven hand-authored ones and nine commands never captured from a live Orca (implementation notes, "Not verified"). The practical risk is drift, not a missing feature.

## 4. pod versus OpenRig

### What pod borrowed (spec section 14 marks each "(OpenRig)")

1. Closure reasons on worker exit, from OpenRig's hot-potato enforcer.
2. Paste, confirm, then Enter pane input, from its session transport.
3. A bounded wake ladder that points at the stored message row.
4. A three-axis activity taxonomy with ranked evidence.
5. Evidence-carrying escalation and a once-only, evidence-attached human decision (its human-in-the-loop rule).

pod borrowed ideas and no code, as the 2026-10-05 review recommended. It also avoided OpenRig's screen-scraping regexes and its wide edits to trust files, though `--yolo` trust writes to `~/.claude.json` and `~/.codex/config.toml` are the same class of change, scoped to one path.

### What OpenRig still does better

| OpenRig | pod today | Note |
|---|---|---|
| **Persistence of a team across reboots**: `rig down --snapshot`, `rig up` with per-node outcomes | pod's store survives a reboot; `startup` hook reconciles dispatches against surviving panes. It does not relaunch agents or restore sessions | Real gap if herdr or the machine restarts mid-Run |
| **Topology as code** (RigSpec, bundles, built-in teams) | Tasks and presets only; no reusable team definition | Different goal; pod is task-first |
| **Daemon plus MCP**: agents call tools instead of shelling out | CLI only | Shell-out works for Claude and Codex and is simpler to audit |
| **Adopt existing sessions** (`rig discover`, `rig adopt`) | `worker-start --terminal` reuses a pane; no discovery | Small |
| **Typing guard** holds automatic messages while a person types in a seat | pod detects only a *closed* pane as user-owned; typing into a live pane is not detected (deferred item 4, herdr has no pane-input event) | Real gap; herdr limitation |
| **Cross-host sends with an honest "Delivery UNCONFIRMED"** | Single host | n/a |
| **`rig workflow validate`** finds steps that can never finish | Dependencies only; a Task blocked on a failed dependency is surfaced after the fact | Cheap idea to borrow: a static check of a Task graph for deadlocks at `task-create` time |
| **Compaction handling**: restore map before compaction; quiet-after-compaction seats restored | None | Same as the Orca compaction fix |
| **Activity feed, context and token usage** per seat | `pod usage` shows account windows and cost, not per-worker context | Medium |
| **Slack bridge** for decisions | None (and pod's rules forbid posting during a run) | Out of scope |
| **Community**: 56 of v0.6.10's changes from outside contributors; a bug report per quirk | One author | Not fixable |

### Where pod is ahead of OpenRig

pod is smaller (a single Rust binary, no Node, no daemon, no UI), has a documented contract and receipts, tests every spec rule (340 tests passing on 2026-10-12), and states what it did not verify. Its safe-input rule is documented alongside its measured failure (a Codex composer that scrolls and soft-wraps), which OpenRig's v0.6.10 fix for "faint suggested prompt" suggests it is still meeting in other forms.

## 5. Gaps in pod

Ordered by how likely each is to cost this user a run.

1. **Nothing acts without a coordinator or a herdr event.** No timer by design. If the coordinator exits or is compacted away, or herdr reports no idle event, undelivered mail waits for the next mutating command at least 60 s later. Both Orca (app) and OpenRig (daemon) have an always-on process that can nudge, retry and notify. The implementation notes already record the consequence: mail sent within 3 s of the recipient going idle waits for the next trigger.
2. **Real-agent behaviour is thinly proven.** The notes say the quiet-window proof, herdr's `agent_session` report, the ancestry walk from a Codex sandbox, and `worker_done` out of a new worktree are verified only with a scripted agent or not at all; Claude Code's trust entry was not exercised live; a real `dagr check --strict` has run once. The ten-researcher Run is the only live evidence. Orca and OpenRig both found bugs only by running real agents (their release notes are mostly such fixes); expect the same.
3. **Context compaction is unaddressed.** Orca fixed `worker_done` after compaction in v1.4.219; OpenRig restores quiet seats after compaction. pod relies on the preamble staying in the worker's context. A long-running worker that compacts can lose the `pod` commands. Worth adding a reminder to the nudge text or a short "re-read your contract" file the preamble points to.
4. **No restart recovery beyond reconcile.** After a herdr or machine restart pod marks workers `lost` or `reclaimable` but cannot resume agents (OpenRig can restore seats with `rig up`). For a 100-task Run that is the expensive failure.
5. **Typing into a live worker pane is not detected** (deferred item 4); a person's edits can collide with a nudge paste. herdr protocol 22 has no pane-input event.
6. **Deferred spec items**: `start_unknown`, the `guidance` and `interruption` attention categories, the base-drift preamble section, and the untasked-gate board card (implementation notes, "Deferred past v1").
7. **Spec text out of date after 2026-10-02.** Section 14's "automatic coordinator loop" row is moot (Orca's no longer has one), and the spec's description of the dispatch capability and of Orca's `blocked` rules predates v1.4.219 and v1.4.221.
8. **Narrow agent coverage**: launch flags for `codex` and `claude` only. Orca's v1.4.224 extends model and effort selection to six agents.
9. **No remote placement**, accepted in v1 and now a larger Orca feature (managed servers).
10. **No static graph check** for Tasks that can never run (OpenRig's `workflow validate`).

### Recommendations, in order

1. Decide whether "no timer" is a principle or an accident. A `pod tick`, run by herdr on an interval or by launchd, would give the wake ladder a clock without a daemon. (Needs a herdr capability check; the spec says herdr's plugin hooks are event-only.)
2. Run a pilot with a long worker that compacts, and record what happens.
3. Recapture Orca's contract samples against v1.4.224 and update `allowed-differences.toml`.
4. Fix the spec text from point 7 (a documentation change; this report did not make it).
5. Borrow `workflow validate`: reject a `task-create` whose dependency graph has a cycle or a dependency that is already `failed`.

## 6. Sources

Orca
- https://github.com/stablyai/orca (repository, stars, features), fetched 2026-10-14
- https://github.com/stablyai/orca/releases (v1.4.217 to v1.4.224, mobile-android v0.0.52 and v0.0.55), fetched 2026-10-14
- https://www.onorca.dev/docs (docs index; `orca.build` mirror unreachable from this machine), fetched 2026-10-14
- https://www.onorca.dev/docs/cli/orchestration (orchestration docs), fetched 2026-10-14
- https://www.onorca.dev/docs/cli/automations (scheduled automations), fetched 2026-10-14
- https://claudeskills.info/zh/skills/stablyai/orca/orchestration/ (mirror of the orchestration skill stub), fetched 2026-10-14

OpenRig
- https://github.com/mvschwarz/openrig (README), fetched 2026-10-14
- https://raw.githubusercontent.com/mvschwarz/openrig/main/README.md, fetched 2026-10-14
- https://github.com/mvschwarz/openrig/releases (v0.6.4 to v0.6.10), fetched 2026-10-14
- https://hn.svelte.dev/item/47772935 (Show HN post), from search 2026-10-14
- https://www.everydev.ai/tools/openrig and https://www.opensourcealternatives.to/item/openrig (directory entries, conflicting stars and authorship), from search 2026-10-14
- https://aitoolly.com/ai-news/article/2026-09-30-openrig-launches-as-an-open-source-multi-agent-framework-unifying-claude-code-and-codex-into-one-sys and the 2026-10-03 follow-up (news summaries, not relied on for facts)

pod (local, read for this report)
- ~/Documents/pod/README.md
- ~/Documents/pod/docs/specs/2026-10-05-pod-design.md (sections 1 to 3, 7, 9, 11, 14)
- ~/Documents/pod/docs/reviews/2026-10-12-implementation-notes.md
- OpenRig review of 2026-10-05 (pod project memory), for the borrowed-ideas list

### Limits of this report

- No source code of Orca or OpenRig was read; internals come from documentation and release notes.
- Orca's full orchestration reference lives in `orca skills get orchestration`, which needs the Orca binary; it was not run. Details of the supervised loop come from the public doc page.
- The Orca release dates have no year on the page; the OpenRig ones likewise.
- Section 5 point 4 on abandon recording the actor, and point 7 on the spec's staleness, were not checked against pod's code.
