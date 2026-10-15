# herdr ecosystem in 2026 (research 4)

Date: 2026-10-14. Scope: herdr's own automation surface, its plugin marketplace, herdr-dagr, and anything that coordinates several agents on herdr. Read against pod's README (pod = Rust single binary, no daemon, herdr plugin, Orca-contract Runs/Tasks/Dispatches/mailboxes/gates, needs herdr >= 0.9.3).

Method and limits: docs pages read from the v0.9.4 llms.txt index; plugin inventory from the GitHub search API (`topic:herdr-plugin`, 1,722 repos returned, many are dotfiles or unrelated apps tagged with the topic; first 100 by stars reviewed) plus the awesome-herdr list (README says 855 "run and orchestrate" projects, 107 in "fleets and supervisors"). The herdr.dev/plugins page is client-rendered and returned no listings to a fetch, so the topic search is the authoritative inventory. Preview-channel notes were not available beyond commit links (see Overlap). Repo READMEs were read for the 8 closest candidates only; the rest are judged from their GitHub description.

## 1. herdr primitives

Source: agent-automation, socket-api, plugins docs (v0.9.4).

Three layers: layout (workspaces, tabs, panes), pane (raw terminal), agent (recognized coding agent, addressed by name or pane id).

CLI and socket (same surface; `herdr api schema --json` has the full schema):
- Create: `workspace create --cwd --label --no-focus`, `tab create`, `pane split --direction`, `pane move`; socket `layout.apply` / `layout.export` (declarative tab trees).
- Worktrees as first-class: `worktree.list/create/open/remove` (params `branch`, `focus`, `force`), events `worktree.created/opened/removed`.
- Pane I/O: `pane run`, `send-text`, `send-keys`, `send_input`, `read` (sources visible/recent/recent-unwrapped/detection), `wait-output` (text or Rust regex, matches existing text immediately), `pane.wait_for_output`.
- Agents: `agent start <name> --kind <kind> --pane [--timeout 3000..300000]`, `agent prompt "<text>" [--wait --until <state> --timeout]` (submit + wait in one request, no race), `agent wait --until <state>` (server-side, event-driven, pins the pane occupant so a replacement process cannot satisfy it), `agent read/get/rename/focus/send-keys/explain`.
- Agent state: `idle`, `working`, `blocked` (approval or question UI recognized), `done` (idle and not yet seen), `unknown`. Reports from integrations via `pane.report_agent` and `pane.report_agent_session` (with `resume_argv`). Wait errors: `agent_blocked`, `agent_prompt_stalled` (no working/blocked within 5 s), `agent_not_ready`, `agent_not_idle`, `agent_not_running`. A timeout does not prove no input was sent.
- Events: `events.subscribe` (long-lived stream) and `events.wait` (one-shot). Types include `pane.agent_detected`, `pane.agent_status_changed` (filterable by pane and status), `pane.output_matched`, `pane.exited`, `workspace.*`, `tab.*`, `layout.updated`, `worktree.*`. Not replayed, not durable; `events_lost` forces resubscribe plus `session.snapshot`.
- Notifications: `notification.show` (title, body, position, sound none/done/request). Display metadata: `workspace.report_metadata`, `pane.report_metadata` (tokens, ttl, seq), `agent.view.set/clear`.
- Remote: `herdr --machine <label>` runs agent/pane/workspace/worktree commands on saved SSH machines (0.9.1).
- Plugins: directory with `herdr-plugin.toml`; `[[actions]]`, `[[events]]` hooks (receive `HERDR_PLUGIN_EVENT_JSON`), `[[panes]]` (overlay/popup/split/tab/zoomed), `[[startup]]` one-shots (not daemons), `[[build]]` (GitHub install only), `[[link_handlers]]`. Env gives `HERDR_BIN_PATH`, `HERDR_SOCKET_PATH`, config and state dirs. "No separate plugin SDK": the CLI is the API. No sandbox, no review. Install: `herdr plugin install owner/repo[/subdir]`, `link`, `update` (0.9.4), `action invoke`, `log list`.
- Marketplace: herdr.dev/plugins auto-indexes public GitHub repos with topic `herdr-plugin` and a root `herdr-plugin.toml`; refresh every 30 minutes; forks, archived and unparsable repos excluded; unvetted.
- Official agent skill: SKILL.md (inspect workspaces, start helpers, send input, wait for status changes).

What herdr does not provide: durable messaging between agents, task/dependency model, completion accounting, retry or circuit-breaking, closure reasons, decision gates. It provides observation and control; orchestration policy is left to plugins and the coordinator agent.

## 2. Plugins found

Count: 1,722 repos carry the topic (inflated by non-plugins). Below: everything relevant to multi-agent coordination first, then the rest of the top of the list by stars. Stars as of 2026-10-14.

### Coordinates several agents (candidate competitors or neighbours)
- eliasstravik/herdr-projects (673★): one coordinator conversation, a worker thread per task on its own branch, shared memory, sidebar overview of what needs you. Closest in spirit to pod. MIT, herdr 0.9.1+.
- zhenyufu/herdr-cadence (3★): one Lead launches role-based agents in tabs and git worktrees; harnesses Codex, Claude, Pi, OMP; yolo setting; compaction reminder hook.
- StructuPath/herdr-conductor (4★, topic orchestration): producer and reviewer roles, strict task/report contracts, hash-chained journal, git compare-and-swap, passive board; operator-attended, pinned to herdr 0.7.5 (0.9.3 development candidate only).
- cowcow02/herdr-agent-orchestrator (0★): herdr state events go to a durable queue, one controller agent is prompted when free; a skill defines what it may do. Event-driven wake without polling.
- DerekStride/agent-orchestrator (0★, Rust): drives one dependency-scoped SQ task graph through git worktrees and OMP workers on herdr, durable local ledger, no silent retries. Depends on sq, agent-id, agent-mail.
- hkandala/orchestratr (`orcr`, 2★, Rust, not a plugin): cross-provider orchestrator on herdr; agents as addressable paths, `orcr agent run/wait/send/logs/kill`, TS SDK, scheduling.
- nelsonPires5/herdr-board (178★, Rust): kanban where each card is a prompt run by an agent in a visible pane; plan/execute/review stages and human gates.
- aemrebarut/herdr-dagr (99★, Rust TUI): live DAG of a run (tasks, attempts, review gates, evidence) from a `run.json`; a producer skill teaches agents to write it. A view, not an orchestrator; pod already exports to it.
- aashishd/herdr-agent-messenger: small call-sign messaging protocol between agents in separate panes.
- junghan0611/entwurf (28★): visible sibling sessions and message exchange across pi, Claude Code, Codex, Copilot, OMP, Antigravity.
- StructuPath/herdr-swarm and ArtMoreno/herdr-swarm: race agents in worktrees, compare diffs, apply a winner.
- aorumbayev/herdr-workflows (32★): YAML ordered command/agent steps.
- husniadil/herdr-sched: cron, webhook, file-watcher triggers firing actions.
- tomoasleep/herdr-symphony, steig/worktender, bredebjorhovd/herdr-board: GitHub issue to worktree to agent.
- neospark-sol/agentflock, C1TRuSovo831/herdr-codex-team, 0-CYBERDYNE-SYSTEMS-0/herdr-dev-team, letya999/workflow-herdr, hewel/herdr-harness-coordinator, ShenghaiWang/helm, kylezk777/herdr-orchestrator, erdostom/herdr-orchestrate, charlieYong/herdr-live: role/planner/coder/reviewer team harnesses (unread; descriptions only).
- xenking/omp-extensible-workflows, vekexasia/pi-extensible-workflows (245★), osolmaz/pi-workflows (331★): Pi/OMP-specific workflow engines.
- JefeLabs/herdr-web-broker: REST/WebSocket API to spawn and steer agents with parent-child federation.
- Northern-Lighthouse/herdr-fleet: capacity-aware agent dispatch across machines.
- natori-hrj/herdr-triage, kay-ws/herdr-island, hhdebb/herdr-radar (181★), aigorahub/herdr-lantern (68★), patraianton/sheepdog: attention triage and fleet status.
- waynewu411/herdr-event-log: durable cursor-resumable log of `pane.agent_status_changed`.
- joelhooks/herdr-janitor: stale-worker cleanup.
- eliasstravik/herdr-agent-progress (32★): agent-reported task progress in the sidebar.
- nidhi-singh02/agent-router (114★): picks agent, model, effort per task, then launches it (preset-like).
- kunchenguid/firstmate (7.9k★, not herdr-only) and mvschwarz/openrig (7.4k★): the crews/rigs families; openrig already reviewed (memory).

### Notifications, mobile, remote
herdr-web-ui (devswha, 950★), AltanS/collie (1.3k★), Heeler (iOS, 576★), dcolinmorgan/herdr-remote, 0cv/herdr-mobile-relay, powerfooI/roamgate, pairfob, permgps/herdr-telegram-agents, hkdom/herdr-telegram-gate (Telegram approve/deny with risk tiers), fulanto/herdr-oncall, yankewei/herdr-focus-notify (clickable macOS toast on blocked/done), robbyrussell/herdr-ohmyzsh, Unayung/herdr-watch, timvdhoorn/stream-deck-herdr-plugin, penso/herdr-gpui, missuo/herdrm, termote.

### Review and annotation
persiyanov/herdr-reviewr (885★), plannotator/herdr-annotate (663★), jhochenbaum/herdr-hunk-diff, JonasBaeumer/herdr-file-annotator, smarzban/herdr-file-viewer, alexarthurs/herdr-sidebar, Crokily/herdr-lazygit.

### Workspace, worktree, session management
devashish2203/herdr-worktrunk, tdi/herdr-worktree-setup, NathanFlurry/herdr-plugin-jj-workspace, razajamil/herdr-plugin-workspace-manager, yuk1ty/herdr-spreader, andrewchng/herdr-sessionizer, fullerzz/herdr-plugin-sesh, ntindle/herdr-resurrect, nikok6/herdr-mirror (remote servers into local sidebar), speardragon/herdr-plugin-manager (install/update/browse marketplace), cloudmanic/herdr-plus, thanhdat77/herdr-navigator, jeffarese/herdr-bar, JanTvrdik/herdr-command-palette, kryptamine/herdr-auto-title, qu8n/herdr-automatic-rename, sh1ma/herdr-auto-title, iurysza/herdr-tab-smart-rename, rjyo/herdr-window-title-sync, Tyru5/herdr-floax.

### Usage and cost
senna-lang/herdr-agent-usage (49★): context meters and provider rate limits (overlaps pod usage); fkiene/llmtrim-herdr (token compression proxy); uwuclxdy/clauth (Claude account manager, 242★).

### Other tools and misc
ogulcancelik/herdr-browser and zenbu-labs/terminal-browser, terminal-code (embedded browser/editor), ChmaraX/herdr-nvim, vim-herdr-navigation, herdr-splits.nvim, ImArtisann/zed-herdr, smarzban/tsk (agent task board), miiraheart/herdr-beads (bd board), oborchers/proqi (prompt composer), madarco/agentbox and agentbox-herdr-plugin (VM-sandboxed parallel agents), openclaw/crabbox, nicosuave/memex, ragamo/herdr-flock, deimantasnork/captains-deck (read-only Firstmate kanban), iurysza/termscope, speardragon/herdr-yazi. Not herdr plugins despite the topic: babarot/dotfiles, edmundmiller/dotfiles, ThorstenRhau/token, eugenioenko/ttt, lararosekelley/dotfiles, osolmaz/ghzinga, taxueseek/session-digger.

### herdr-dagr specifically
aemrebarut/herdr-dagr, 99★, last push 2026-10-13, Rust, installs with `herdr plugin install aemrebarut/herdr-dagr`, action `open-dagr` (and `-right/-down/-up/-left`, Windows variant). Reads `run.json` that agents (via a `dagr-producer` skill) keep current; shows attempts, review gates, evidence level, history. Pure display, no worker control.

## 3. Overlap with pod

What herdr 0.9.x added (releases page, v0.9.1 16 Sep to v0.9.4 13 Oct):
- 0.9.1: `--machine` SSH machine control, Windows SSH hosts, worktree fixes (focus new workspace, retry partial removals), Codex stays working through queued follow-ups.
- 0.9.2 (29 Sep): agent resume (agents report their own resume command, restored one at a time), multiple prefix keys, false "done" at startup fixed, Codex not idle during active output, self-reported agents clear when pane returns to shell.
- 0.9.3: hotfix for Escape-sequence and Option-key input.
- 0.9.4 (13 Oct): `plugin update`, link handlers, `pane run`/`agent start` refuse over-long commands on macOS while the shell is starting (relevant to pod's paste-then-Enter path), worktree groups survive unavailable checkouts, Windows notification click-through, Antigravity/Grok idle/blocked fixes.
- Waits: no wait changes listed in 0.9.x; `agent prompt --wait` and `agent wait` already existed.
- Preview channel: builds 2026-09-16, 09-21, 09-28, 09-29 (x2), 10-12 list only commit links on the releases page; the preview docs (https://herdr.dev/llms-preview.txt) were not read, so unreleased additions are unknown. Follow-up needed before relying on any preview-only feature.

Where herdr or the ecosystem overlaps pod:
- Worker launch: `agent start`, `agent prompt`, `worktree create`, `layout.apply` cover pod's placement and launch. pod adds presets, brief-as-file, paste confirmation, and accounting. herdr's `agent prompt --wait` and `agent_prompt_stalled` are the native version of pod's "confirm on screen then Enter".
- Waits and state: `agent wait` is server-side and pins the occupant. pod's three-axis observation (`activity`, `presence`, `needs_input`) is richer and deliberately treats self-report as activity-only, which herdr does not model. herdr state (`blocked`/`done`) is what pod already reads as authoritative for presence.
- Notifications: `notification.show` plus sound; many third-party plugins (focus-notify, telegram-gate, oncall, web push). pod has durable mailboxes and gates but no push channel; herdr-telegram-gate style approval inbox maps onto pod user gates.
- Agent state reporting: `pane.report_agent` is what integrations use; pod's `herdr-agent-state`-like reporting is not needed.
- Sidebar and tab bar: `workspace.report_metadata`, `pane.report_metadata`, `agent.view.set`; pod already writes `<task> · <phase>` and the usage entry. herdr-agent-usage overlaps `pod usage`.
- Events: herdr events are volatile (`events_lost`); pod's hook-driven, store-backed deliveries and replay have no equivalent in herdr itself. herdr-event-log and herdr-agent-orchestrator add a durable queue only for status events.
- Direct competitors: none found that implements pod's contract (Orca-compatible Runs, Tasks with deps, Dispatches, durable FIFO mailboxes with ack/replay, gates, closure reasons, completion accounting). Nearest by intent: herdr-projects (coordinator plus worker threads, shared memory, no dependency graph or accounting that I could confirm from the README), herdr-cadence (Lead plus worktree workers), herdr-conductor (strict task/report contracts and a journal, but pinned to herdr 0.7.5 and attended), DerekStride/agent-orchestrator (Rust task-graph driver tied to SQ and OMP), orchestratr/`orcr` (agent lifecycle and SDK, not a task contract). None targets Orca users. herdr-projects is the one to watch because it ships as a plugin with a polished onboarding and 673 stars.

## 4. Opportunities

1. Use `agent prompt --wait --until` for the launch handshake where pod now pastes and polls, and map `agent_prompt_stalled` / `agent_blocked` errors to pod's closure reasons and `needs_input`. Lower race risk, fewer screen reads.
2. Adopt `layout.apply` for multi-worker placement (one declarative call for a worker tab layout) and `worktree create` events (`worktree.created` hook) instead of shelling out.
3. Subscribe with `events.subscribe` (`pane.agent_status_changed`, `pane.exited`) in `pod hook`-adjacent paths only if a long-lived process is ever acceptable; with no daemon, keep hooks but note `events_lost` semantics mean pod must always reconcile from `agent.list`/`session.snapshot`, never trust a missed event (matches "act on proof, never on absence").
4. Telegram/push approval: pod user gates could be mirrored through an existing plugin such as hkdom/herdr-telegram-gate or yankewei/herdr-focus-notify via a documented hook or `notification.show`, instead of building a channel. Closes the "board card and sidebar status deferred" gap for gates without a Task.
5. Publish pod to the marketplace: add topic `herdr-plugin` and a root `herdr-plugin.toml`; discoverability costs nothing and the index refreshes every 30 minutes. `herdr plugin update` (0.9.4) fits pod's install script flow; confirm `[[build]]` is acceptable or keep the release-binary download in install.sh.
6. Positioning: Orca-contract compatibility is the unoccupied niche. Docs should say what pod gives that herdr-projects, cadence, conductor and orcr do not (durable mailboxes, deps, gates, accounting, closure reasons, Orca skill compatibility). Consider interop: herdr-dagr already consumes pod's `run.json`; herdr-board and herdr-radar could read pod's sidebar tokens.
7. Raise `min_herdr_version` consideration: README pins 0.9.3; 0.9.4 adds the macOS over-long-command refusal that affects `pane run`/`agent start`; pod's paste path (pane input) is unaffected but doctor could warn on < 0.9.4.
8. Reuse, do not rebuild: herdr-agent-usage overlaps `pod usage` (compare before investing further); herdr-event-log's cursor-resumable log is a design reference for replay.
9. Watch list: herdr-projects, herdr-cadence (Claude as lead is untested there), orcr. Re-check in a month; the topic has grown to 1,722 repos.

Risks: plugin trust model is "no sandbox, no review", so pod's doctor and docs should not suggest installing third-party plugins blindly; star counts on this topic are noisy (dotfiles repos tagged).

## 5. Sources

- herdr docs index: https://herdr.dev/llms.txt (page URLs resolve to https://raw.githubusercontent.com/herdrdev/herdr/v0.9.4/docs/next/website/src/content/docs/{agent-automation,socket-api,plugins,marketplace}.mdx)
- Marketplace page: https://herdr.dev/plugins/ ; docs https://herdr.dev/docs/marketplace/ and https://herdr.dev/docs/preview/marketplace/
- Preview docs index (not read): https://herdr.dev/llms-preview.txt
- herdr releases: https://github.com/herdrdev/herdr/releases
- GitHub topic search: `gh api search/repositories q=topic:herdr-plugin` (2026-10-14)
- awesome-herdr: https://github.com/yigitkonur/awesome-herdr
- herdr-dagr: https://github.com/aemrebarut/herdr-dagr
- herdr-projects: https://github.com/eliasstravik/herdr-projects
- herdr-cadence: https://github.com/zhenyufu/herdr-cadence
- herdr-conductor: https://github.com/StructuPath/herdr-conductor
- herdr-agent-orchestrator: https://github.com/cowcow02/herdr-agent-orchestrator
- agent-orchestrator (SQ): https://github.com/DerekStride/agent-orchestrator
- orchestratr: https://github.com/hkandala/orchestratr
- herdr-board: https://github.com/nelsonPires5/herdr-board
- Third-party guides: https://flaviocopes.com/herdr-plugins ; https://www.bitdoze.com/best-herdr-plugins/
