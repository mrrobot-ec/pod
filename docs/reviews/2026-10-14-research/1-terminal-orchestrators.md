# Terminal and multiplexer orchestrators for coding agents (2026 survey)

Date: 2026-10-14. Author: research-tools worker (task_b7197d29df70).

## Method and caveats

- Compared against pod as described in `README.md` and `docs/specs/2026-10-05-pod-design.md` sections 1-3 and 7.
- Sources: GitHub README pages (fetched), vendor docs, and web-search summaries. Many search hits are SEO or aggregator pages; those are marked **(secondary)**.
- Star counts, release dates and "last activity" were frequently not visible in the fetched pages (GitHub release sections came back empty). Where a figure is missing it says "not verified". Star counts disagree between sources and are approximate.
- Nothing here was run or installed. "Completion detection" below is what the docs say, not what I observed.

## What pod is (the yardstick)

From the README and spec:

- Single Rust binary, **no daemon**, shipped as a **herdr plugin**. herdr is the only long-lived process; it runs `pod hook` on pane events (spec 3.2).
- Follows **Orca's orchestration contract** (Run, Task with dependencies, Dispatch, durable mailboxes, FIFO Deliveries with ack and replay, questions, decision gates).
- Placement: new tab in the current folder, or a new/existing worktree via `herdr worktree create` (spec 7.2). Never removes or commits in a worktree.
- Completion: **only a valid `worker_done` from the active Dispatch**; "looking idle is never completion" (7.4). Closure reasons, circuit breaker at 3 failures (7.5).
- Observation on three axes (`activity`, `presence`, `needs_input`) with recorded evidence source (7.6).
- Safe pane input (paste, confirm on screen, Enter) and a bounded wake ladder, no timer (README, spec 8.2).
- Reuse / retain / release accounting for every worker (7.4).
- Out of scope in v1: SSH placement, scheduling, GUI, Windows, concurrency cap.

Five properties define pod's niche: (1) task DAG with dispatch authority, (2) durable acknowledged mailboxes, (3) completion only by proof, (4) worker lifecycle accounting, (5) daemonless on top of herdr.

## Tools

### 1. Orca (stablyai/orca)

- **What:** Desktop "ADE" for parallel agents (Codex, Claude Code, OpenCode, Pi); the origin of the contract pod copies.
- **Launch/placement:** One git worktree per agent; `orca` CLI lets agents create worktrees and drive the app; SSH worktrees; mobile companion.
- **Completion:** Notifies you when an agent finishes or needs attention; the orchestration layer (messages, `worker_done`, task DAG) is the contract pod follows.
- **Daemon/GUI:** Electron desktop app is the long-lived process (pod README says this is the reason pod exists).
- **License:** MIT.
- **2026 activity:** Active (README advertises mobile companion and SSH). Releases not verified.
- **vs pod:** Same workflow, broader features (browser, SSH, mobile), but needs the GUI process. This is the baseline, not a competitor on design.

### 2. claude-squad (smtg-ai/claude-squad)

- **What:** Terminal TUI (`cs`) managing multiple agent sessions (Claude Code, Codex, Aider, others via `-p`).
- **Launch/placement:** Each agent in its own **tmux session + git worktree/branch**; named profiles; needs tmux and `gh`.
- **Completion:** No documented completion protocol; `--autoyes` auto-accepts prompts. Review is human, in the TUI. A third-party write-up says worktree cleanup is manual **(secondary)**.
- **Daemon/GUI:** TUI; the repo has a `daemon` directory used for background autoyes, not documented in the README.
- **License:** AGPL-3.0.
- **2026 activity:** ~8.6k stars on the GitHub page; 222 commits; last commit date not verified.
- **vs pod:** Session manager for a human. No tasks, messages, or completion accounting.

### 3. Vibe Kanban (BloopAI/vibe-kanban)

- **What:** Kanban board that runs Claude Code, Codex, Gemini CLI, Amp, OpenCode in parallel with diff review and PR creation; also exposes an MCP server.
- **Launch/placement:** `npx vibe-kanban`; one worktree and branch per task; backend server plus web UI/desktop app.
- **Completion:** Not documented on the README; human reviews diffs.
- **Daemon/GUI:** Yes, local server + web UI.
- **License:** Apache-2.0.
- **2026 activity:** README says **"Vibe Kanban is sunsetting"**; the company (bloop) shut down in April 2026 and the project continues community-maintained **(secondary for the shutdown date)**.
- **vs pod:** GUI-first and winding down. MCP task API is the interesting part, but there is no lifecycle accounting.

### 4. Conductor (conductor.build)

- **What:** Mac app to run parallel Claude Code, Codex and Cursor agents in isolated workspaces and review/merge.
- **Launch/placement:** One git worktree + branch per workspace, with terminal, diff and checks. Launch mechanics not documented on the homepage.
- **Completion:** Not documented; dashboard for review.
- **Daemon/GUI:** macOS GUI app. Homepage shows version 0.90.0; a "Conductor Cloud" page exists.
- **License:** Not stated; closed-source by all indications. Free app, you pay model usage **(secondary)**.
- **2026 activity:** Active (version 0.90.0, cloud product).
- **vs pod:** GUI and Mac-only; no scriptable task/message contract found.

### 5. agent-of-empires (`aoe`, njbrake/agent-of-empires)

- **What:** Rust tmux session manager with TUI and web dashboard (`aoe serve`); supports Claude Code, Codex, Gemini CLI, OpenCode, Copilot CLI and others.
- **Launch/placement:** One tmux session per agent; optional worktrees, multi-repo workspaces, Docker/Podman/Apple Container sandboxing.
- **Completion:** "Agent-aware status detection" and notifications; mechanism not documented. No agent-side completion protocol found.
- **Daemon/GUI:** TUI, optional web server and HTTP API; agents persist in tmux.
- **License:** MIT.
- **2026 activity:** ~3.3k stars on the GitHub page; the repo owner differs between sources, so confirm the canonical repo.
- **vs pod:** Strong on isolation (containers) and remote access; weak on coordination. Status is observed, not proven.

### 6. Claude Flow / Ruflo (ruvnet/ruflo)

- **What:** Multi-agent orchestration layer for Claude Code: swarms, memory, MCP server with 300+ tools, "60+ agent types".
- **Launch/placement:** `npx ruflo@latest init`; swarm topologies (hierarchical, mesh, adaptive). Not a pane/worktree tool; agents are largely virtual roles inside Claude Code.
- **Completion:** Not documented in README; "auto-triggered workers" and a background daemon in the full install.
- **Daemon/GUI:** CLI + daemon + MCP.
- **License:** MIT.
- **2026 activity:** Very active (~74.6k stars, 9.5k commits on the page). Performance claims (e.g. SWE-bench) are from one third-party guide and unverified **(secondary)**.
- **vs pod:** Different category (prompt-level swarm framework). Large surface, little evidence of supervised worker accounting.

### 7. Crystal -> Nimbalyst (stravu)

- **What:** Electron app running multiple Codex/Claude Code sessions in git worktrees. **Crystal is deprecated** (final release v0.3.5, 2026-02-26 per search results); successor Nimbalyst adds kanban, editors, mobile.
- **Launch/placement:** Worktree per session.
- **Completion:** GUI session status; no contract.
- **Daemon/GUI:** GUI.
- **License:** Crystal open source; Nimbalyst licensing reported inconsistently **(secondary)**.
- **vs pod:** Dead end for this comparison.

### 8. Tmux-Orchestrator (Jedward23/Tmux-Orchestrator)

- **What:** Shell-script toolkit: an Orchestrator agent, Project Managers and Engineers as Claude sessions in tmux, with self-scheduled check-ins.
- **Launch/placement:** tmux windows; `send-claude-message.sh`, `schedule_with_note.sh`.
- **Completion:** By convention (agents message and commit); no enforcement.
- **Daemon/GUI:** None beyond tmux (scheduling uses sleeping shells).
- **License:** Not verified.
- **2026 activity:** ~1.5k stars **(secondary)**; the 2025 original, now mostly a template that others copied.
- **vs pod:** Same idea at a much lower rigor level.

### 9. Cyclops (cyclops-team/cyclops)

- **What:** Terminal tool coordinating coding agents in tmux through a **durable mailbox**.
- **Launch/placement:** `cyclops start --preset duo` builds a named tmux layout; panes get addresses (`cyclops name --self`). Agent detection via 52 manifests, 5 verified (Claude Code, Codex CLI, Antigravity, Cursor Agent, Kimi Code).
- **Completion:** "Completed requires an explicit agent-side reply. Cyclops does not infer task completion from idle state." Message states: accepted, notified, claimed, completed.
- **Delivery:** Append-only fsynced journal; FIFO doorbell per recipient bound to pane and process identity; held while a human draft or blocking screen is visible; `submitted_unverified` and `attention_required` outcomes; **no timer retries**; restart replays the journal.
- **Daemon/GUI:** `cyclopsd` daemon (panes and messages survive `cyclops stop`); optional workspace UI.
- **License:** MIT. Version 1.1.0.
- **2026 activity:** 952 commits, but **14 stars** and no forks on the fetched page, so it is effectively a one-person project.
- **vs pod:** Philosophically the closest to pod's messaging layer (proof over idleness, no timer, honest unverified outcomes). No task DAG, dispatch authority, worktree placement, or lifecycle accounting found in the README. Needs a daemon.

### 10. OpenRig (mvschwarz/openrig)

- **What:** Self-hosted multi-agent harness: YAML "rigs" (pods, edges) boot a Claude Code/Codex team in tmux, with a lead agent delegating to specialists.
- **Launch/placement:** `rig up` starts tmux sessions, launches harnesses, runs readiness checks; `rig down --snapshot` and `rig up <name>` restore with per-seat outcomes (resumed, fresh-primed, awaiting-decision, attention_required, failed); `rig discover`/`rig adopt` take over existing tmux sessions.
- **Completion/activity:** Hook relays post event type, seat and session identity to the daemon (`/api/activity/hooks`), excluding prompt text. Completion detection is not documented; a queue item is recorded by the receiving seat.
- **Messaging:** `rig send` (with `--verify`), `broadcast`, `chatroom`, `queue list`; a typing guard holds automatic messages.
- **Daemon/GUI:** Local Hono HTTP daemon + CLI + TUI + MCP server, SQLite; legacy React UI in maintenance.
- **License:** Apache-2.0.
- **2026 activity:** v0.6.8 pinned in the README; ~7.4k stars; 3,854 commits; one review says almost all commits are from one author and that it writes trust settings and hooks into Claude/Codex configs **(secondary)**.
- **vs pod:** pod already borrows its activity taxonomy (spec 7.6). OpenRig is a fleet/topology tool (persistent rigs, restore) rather than a task-with-dependencies tool; it needs a daemon and tmux.

### 11. ClawTeam (HKUDS/ClawTeam)

- **What:** Lets agents spawn and manage teammates.
- **Launch/placement:** `clawteam spawn`: per worker a **git worktree** (`clawteam/{team}/{agent}`), a **tmux window**, an identity; tiled view with `board attach`; `subprocess` backend for non-interactive tools.
- **Tasks:** `pending / in_progress / completed / blocked`, `--blocked-by`, automatic unblock on completion.
- **Messaging:** Point-to-point inboxes with `send`, `receive` (consumes), `peek`; `broadcast`; file transport or ZeroMQ P2P.
- **Completion:** Agent-reported (`task update --status completed`, `task wait --timeout`, `lifecycle idle`); no process monitoring documented.
- **Daemon/GUI:** No daemon documented.
- **License:** MIT. ~5.5k stars; v0.2.0 on 2026-03-23 per its news, release state unclear.
- **vs pod:** Closest in *shape* (tasks with deps, inbox, worktree + tmux, daemonless). Missing from its README: dispatch authority/fencing, ack and replay deliveries, closure reasons, circuit breaker, presence evidence, release accounting.

### 12. Claude Code agent teams (Anthropic, built in)

- **What:** Experimental: a lead Claude session spawns teammates that share a task list and message each other. Needs `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`.
- **Launch/placement:** In-process by default; split panes in **tmux or iTerm2** (`teammateMode`). Not supported in VS Code terminal, Windows Terminal, Ghostty. No worktree placement (the docs tell you to split files per teammate and mention worktrees only as a manual option).
- **Tasks/messaging:** Shared task list with dependencies and file-locked claiming; JSON mailbox files per agent; a message is reported sent only when the write succeeds.
- **Completion:** Teammates notify the lead when they stop, including the final answer or API error; hooks `TeammateIdle`, `TaskCreated`, `TaskCompleted` can veto. Docs list "task status can lag" and "teammates may stop after errors" as limitations.
- **Daemon/GUI:** None; runs inside Claude Code.
- **License:** Proprietary (Claude Code). Claude only; one team per session, no nested teams, no resume for in-process teammates.
- **vs pod:** The strongest *native* alternative for Claude-only work. It cannot run Codex workers, has no ack/replay delivery, no worktree accounting, and is explicitly experimental.

### 13. Smaller tmux/worktree managers

- **workmux (raine/workmux):** MIT, ~2.8k stars. `workmux add <branch>` = worktree + tmux window (also kitty, WezTerm, Zellij). Window-name status icons for working / waiting / done for Claude Code, Codex, Gemini CLI and others; batch spawn with `--count`, `--foreach`, `--max-concurrent`; container/VM sandbox. No messaging or task graph.
- **dmux (standardagents/dmux):** MIT per a search result **(secondary)**; tmux + worktrees multiplexer.
- **agent-deck (asheshgoplani/agent-deck):** MIT, ~1.1k stars. TUI with live status (running / waiting / idle / error); "conductor" sessions that monitor children, parent linkage, heartbeats every 15 minutes, Telegram/Slack bridges; `notify-daemon` nudges the parent when a child changes state. Idle-based, with a daemon.
- **Superset (superset.sh):** Desktop app, worktree + persistent terminal per agent, macOS only. **Elastic License 2.0 (source-available)** and an account login per a competitor page **(secondary)**.
- **Agent Orchestrator (ComposioHQ):** Worktree + branch per agent, tmux/Docker/Kubernetes runtimes, routes CI failures and review comments back to the owning agent, escalates to a human; config in `agent-orchestrator.yaml`. Details from docs and blogs **(secondary)**.
- **cmux (manaflow-ai/cmux):** Ghostty-based macOS terminal with per-agent notifications.
- **Others on the awesome lists (not examined):** agent-manager, amux, thurbox (inter-session messaging, SSH), repomon, Crewly, Contrabass, YYLO, multi-agent-shogun, ai-devkit, pappardelle, tmux-ide.

### 14. herdr itself (herdrdev/herdr)

- **What:** The runtime pod plugs into: single Rust binary, background server keeps sessions alive, panes labelled working / blocked / idle, CLI and socket API so agents spawn panes, prompt each other, and wait until another agent is blocked.
- **License:** Apache-2.0. ~44k stars on the page, so the host is far more popular than any orchestrator above.
- **vs pod:** herdr gives primitives (spawn, prompt, wait for blocked) but not Runs, Tasks, durable mail, or accounting. That gap is pod's reason to exist.

## Comparison table

Legend: Y = documented, p = partial, n = not found in README/docs (not proof of absence), - = not applicable.

| Tool | Placement | Task deps | Durable acked mail | Completion by proof | Worker accounting | Daemon / GUI | Agents | License | 2026 status |
|---|---|---|---|---|---|---|---|---|---|
| **pod** | herdr tab, worktree | Y | Y (FIFO, ack, replay) | Y (`worker_done` only) | Y (reuse/retain/release) | none / none (dagr optional) | any herdr agent | MIT | new, personal |
| Orca | worktree, SSH | Y (contract) | Y | Y (contract) | Y | Electron app | Claude, Codex, OpenCode, Pi | MIT | active |
| claude-squad | tmux + worktree | n | n | n | n | TUI | Claude, Codex, Aider | AGPL-3.0 | ~8.6k stars |
| Vibe Kanban | worktree | board only | n | n (human review) | n | server + web | Claude, Codex, Gemini, Amp | Apache-2.0 | sunsetting |
| Conductor | worktree | n | n | n | n | Mac GUI | Claude, Codex, Cursor | not stated | active |
| agent-of-empires | tmux, worktree, container | n | n | n (status) | n | TUI + web | many | MIT | active |
| Ruflo | virtual swarm | p | p | n | n | daemon + MCP | Claude-centric | MIT | very active |
| Crystal/Nimbalyst | worktree | n | n | n | n | Electron | Claude, Codex | mixed | Crystal EOL |
| Tmux-Orchestrator | tmux window | n | scripts | convention | n | none | Claude | not verified | stale template |
| **Cyclops** | tmux panes | n | **Y** | **Y** (agent reply) | n | **daemon** | 5 verified | MIT | v1.1.0, 14 stars |
| **OpenRig** | tmux rig | p (queue) | p (`--verify`) | n | p (snapshot/restore) | **daemon** + MCP | Claude, Codex, Pi | Apache-2.0 | v0.6.8, one author |
| **ClawTeam** | tmux + worktree | **Y** | p (consume/peek) | p (agent-reported) | n | none | multiple | MIT | v0.2.0 |
| Claude agent teams | in-process, tmux, iTerm2 | **Y** | p (mailbox file) | p (idle notice, hooks) | n | none | Claude only | proprietary | experimental |
| workmux | tmux/kitty/WezTerm/Zellij + worktree | n | n | n (icons) | n | none | many | MIT | active |
| agent-deck | tmux | n | p (nudges) | n (idle based) | n | notify daemon | Claude, Codex, Gemini, OpenCode | MIT | active |
| herdr (host) | panes, worktree | n | n | n | n | server | many | Apache-2.0 | active |

## Verdict

**No tool found does all of what pod does while also fitting pod's constraints** (daemonless, herdr-hosted, any agent kind, Orca-compatible commands). Evidence by pod property:

1. **Orca does what pod does, and better in breadth.** It is the contract pod copies, and it is MIT. It is only "worse" on the one axis pod was written for: it needs the Electron app running. If that is acceptable, Orca remains the better-tested option. (Source: pod README "Why pod exists"; https://github.com/stablyai/orca)
2. **Claude Code agent teams are the strongest native competitor for Claude-only work.** Shared task list with dependencies, file-locked claiming, mailbox with write-confirmed delivery, hooks on idle/task completion, tmux or iTerm2 split panes, no daemon. Gaps against pod: experimental and off by default, Claude only (no Codex terra/sol loop), no worktree placement, one team per session, no resume of in-process teammates, "task status can lag" is a documented limitation (so completion is weaker than pod's `worker_done` rule). Re-check as it matures. (https://code.claude.com/docs/en/agent-teams)
3. **Cyclops is the closest on the messaging philosophy and is arguably more careful in one area.** It refuses to infer completion from idleness, journals before writing to the terminal, holds doorbells behind human drafts and modal screens, records `submitted_unverified`, and uses no retry timer. pod's safe pane input and wake ladder cover similar ground, and pod additionally has tasks, dispatch authority, worktrees and release accounting. Cyclops needs a daemon and has 14 stars. Worth reading for the draft/modal hold rules before pod finishes its own safe-input work. (https://github.com/cyclops-team/cyclops)
4. **ClawTeam is the closest in shape** (task deps + inbox + worktree/tmux, no daemon) and is MIT with ~5.5k stars. Its README shows agent-reported completion only, with no dispatch fencing, ack/replay, closure reasons, circuit breaker or presence evidence. It is a plausible lighter substitute if those guarantees are not needed. (https://github.com/HKUDS/ClawTeam)
5. **OpenRig is better at persistent fleets** (YAML topologies, snapshot/restore, adopt existing sessions, MCP) and is the source of pod's activity taxonomy, but it is daemon-based and not task-DAG-first. (https://github.com/mvschwarz/openrig)
6. **Session managers (claude-squad, agent-of-empires, workmux, agent-deck, Conductor, Superset) do not compete on orchestration.** They place agents and show status for a human; none documents a completion protocol, task graph or worker accounting. agent-of-empires' container sandboxing and workmux's multi-backend support are features pod lacks, but they solve a different problem.
7. **Vibe Kanban and Crystal should not be adopted** (sunsetting and end-of-life respectively).

Bottom line: pod's differentiators still hold after this survey: completion only by proof, durable acknowledged mail with replay, dispatch authority, and release accounting, all without a daemon. The tools that come closest on one axis each are Cyclops (messaging), ClawTeam (shape), and Claude agent teams (native Claude-only). Things pod does not have that others do: container sandboxes, remote/SSH, a GUI board, and CI-event reactions (Agent Orchestrator).

Limits of this verdict: README-level evidence only, nothing run; "not found" means not documented on the pages fetched.

## Sources

Fetched directly:
- https://github.com/kanischev/awesome-agent-orchestrators
- https://github.com/smtg-ai/claude-squad
- https://github.com/njbrake/agent-of-empires
- https://github.com/BloopAI/vibe-kanban
- https://github.com/ruvnet/ruflo
- https://github.com/raine/workmux
- https://github.com/stablyai/orca
- https://www.conductor.build
- https://github.com/cyclops-team/cyclops
- https://github.com/asheshgoplani/agent-deck
- https://github.com/HKUDS/ClawTeam
- https://github.com/herdrdev/herdr
- https://github.com/mvschwarz/openrig
- https://code.claude.com/docs/en/agent-teams

Web-search results (secondary, not opened):
- https://www.sourcepulse.org/projects/2338008 (claude-squad)
- https://starlog.is/articles/ai-dev-tools/smtg-ai-claude-squad (claude-squad cleanup claim)
- https://virtuslab.com/blog/ai/vibe-kanban/ and https://www.blog.brightcoding.dev/2026/07/17/vibe-kanban-the-revolutionary-ai-agent-manager-every-dev-needs (Vibe Kanban shutdown)
- https://www.morphllm.com/conductor-ai-coding (Conductor)
- https://betterstack.com/community/guides/ai/agent-of-empires/ (agent-of-empires)
- https://github.com/ruvnet/ruflo/wiki/CLAUDE and https://pasqualepillitteri.it/en/news/774/claude-flow-ruflo-multi-agent-orchestration-guide (Ruflo)
- https://github.com/stravu/crystal and https://rywalker.com/research/crystal (Crystal -> Nimbalyst)
- https://github.com/Jedward23/Tmux-Orchestrator and https://www.sourcepulse.org/projects/11036576 (Tmux-Orchestrator)
- https://andrew.ooo/posts/openrig-review-multi-agent-harness-claude-code-codex/ and https://pyshine.com/OpenRig-Multi-Agent-Control-Plane-Deep-Dive/ (OpenRig)
- https://www.mintlify.com/ComposioHQ/agent-orchestrator/concepts/architecture (Agent Orchestrator)
- https://superset.sh/compare/superset-vs-claude-squad and https://paseo.sh/alternatives/superset (Superset license)
- https://github.com/bradagi/awesome-cli-coding-agents (directory)

Pod files read: `README.md`, `docs/specs/2026-10-05-pod-design.md` (sections 1-3, 7).
