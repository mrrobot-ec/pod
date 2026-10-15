# Vendor-native multi-agent features versus pod

Research date: 2026-10-14. Installed versions: Claude Code 2.1.299, codex-cli 0.160.1.
Method: official docs (fetched), `claude --help`, `claude agents --help`, `codex --help`, `codex features list`,
`codex cloud/exec/queue/app-server --help`. No interactive sessions were started and the pod repository was not edited.
Read first: `README.md` and `docs/specs/2026-10-05-pod-design.md` sections 1 to 3 and 7.

## Verdict

**pod is not redundant.** Every vendor feature coordinates only that vendor's own agents, and none of them gives pod's
proof-based completion, single durable ledger, or one place a person watches both vendors' panes. Several pieces of pod's
*lower layer* (spawning, isolation, background hosting, per-agent resume) are now native and pod should not grow
into them.

What pod is for (spec section 1 success criteria): a Claude coordinator drives about 100 tasks across worktrees and repos, and a
`terra` (codex) implementer plus a `sol` (codex, xhigh) reviewer loop in the same panes. Criterion 2 is the cross-vendor one, and it
is the one the vendors cannot meet.

## Claude Code native

Version 2.1.299. The docs name five parallel mechanisms plus helpers.

| Mechanism | What it is | State |
|---|---|---|
| Subagents | Markdown + YAML definitions (`.claude/agents/`, `--agents` JSON); own context; per-agent `model`, `tools`, `permissionMode`, `isolation: worktree`, `background`. Up to 3 nesting layers, 20 concurrent by default. Resumable by `SendMessage`. | stable |
| Agent teams | A lead plus teammates, each a full session. Shared task list with dependencies and file-locked claiming, per-agent JSON mailboxes under `~/.claude/teams/`, `TeammateIdle`/`TaskCreated`/`TaskCompleted` hooks, in-process or tmux/iTerm2 split panes. | experimental, off by default (`CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`) |
| Dynamic workflows | A JavaScript script the runtime executes: `agent()`, `parallel()`, `pipeline()`; up to 16 concurrent agents, 1,000 agents per run, schema-validated outputs, adversarial verification patterns, resumable in the same session, saveable as `/name`. | shipping on paid plans |
| Agent view / background sessions | `claude --bg`, `claude agents [--json]`, `attach`, `logs`, `stop`, `respawn`, `rm`. A supervisor process hosts sessions, restarts a crashed session, survives terminal close. | research preview |
| Cloud sessions | `claude --cloud "task"`, runs on Anthropic VMs, survives laptop close, `claude -p "msg" --cloud <id>` queues a follow-up, `--teleport` pulls it local. Projects start parallel cloud threads. | shipping, Anthropic account only |
| Cross-session messaging | `ListAgents` / `SendMessage` over a per-session Unix socket (local) or Anthropic servers (other machine, cloud). Plain text only, inbound controls (`accept`/`hold`/`refuse`). | shipping since 2.1.224 |
| Worktrees | `claude --worktree [name]`, `isolation: worktree` for subagents, automatic worktree for dispatched background sessions, `/batch` for 5 to 30 worktree-isolated subagents. | shipping |
| Fleet | `claude agents` is the "fleet" screen: sessions grouped by state (Needs input, Working, Completed), peek, reply, attach, pin. Scriptable state: `working, blocked, done, failed, stopped`. | research preview |

### What is strong

- **Isolation and cleanup.** Subagent worktrees are removed when unchanged; Bash commands that resolve into the main
  checkout are blocked; `claude rm` keeps a worktree with uncommitted or unpushed work. This is better enforced than pod's "pod never
  removes, cleans, or commits in a worktree" rule, because the vendor controls the shell.
- **Crash supervision for its own sessions.** The agent-view supervisor restarts a session whose process exits, moves idle sessions
  across auto-updates, and a machine shutdown leaves a resumable "failed" session for 48 hours.
- **Scale of orchestration.** A workflow can run hundreds of agents with an explicit script; agent teams give peer messaging
  that pod does not try to provide (pod messages go coordinator to worker and worker to specialist, not peer chatter).
- **Quality gates.** Hooks `TaskCompleted` and `TeammateIdle` can reject completion with exit code 2.

### Limits stated by the docs themselves

- "In every approach the workers are Claude sessions. To involve a different tool, expose it to Claude as an MCP server."
  (code.claude.com/docs/en/agents). This is the cross-vendor answer: Codex can only be a tool, not a worker with a pane and a mailbox.
- Agent teams: no session resumption for in-process teammates (`/resume` does not restore them), task status can lag, one team per
  session, no nested teams, lead fixed, teammates cannot run background subagents, split panes need tmux or iTerm2 (not
  Ghostty, not VS Code terminal). herdr is not a supported display backend.
- Agent teams do not isolate teammates in worktrees; the docs say to partition files by ownership.
- Agent view is local only ("stop if it shuts down") and in preview. Background sessions use subscription usage.
- Workflows cannot take mid-run user input; a resumed run reruns every agent after the first changed or failed one;
  resume works in the same session or a backgrounded one.
- Cross-session messages are plain text, capped at about a million characters, throttled for loops, and a message "never counts as
  your consent", so a worker cannot approve for another.

## Codex native

Version 0.160.1. Surfaces found in help output and docs.

| Mechanism | What it is | State |
|---|---|---|
| Subagents / `multi_agent` | On by default in current releases (`codex features list`: `multi_agent stable true`; `multi_agent_v2 stable false`). Model-callable tools spawn, message, wait on and close child agents. Custom agents in `~/.codex/agents/` or `.codex/agents/` as TOML (`name`, `description`, `developer_instructions`, optional `model`, `model_reasoning_effort`). `agents.max_concurrent_threads_per_session`. `/subagents` switches between threads. A hexdocs SDK guide reports defaults of 6 threads and depth 1 (third party). | stable |
| App server | `codex app-server`: JSON-RPC 2.0 over stdio, Unix socket, or experimental WebSocket (`--listen`). Threads, turns, items; `thread/start`, `thread/resume`, `thread/fork`, `turn/steer`, `turn/interrupt`; approvals as server requests; `thread/list` filters by `parentThreadId` and `sourceKinds` such as `subAgentThreadSpawn`. `codex app-server daemon`, `codex remote-control`, `codex --remote ws://...`, `codex agents` (browse sessions on the shared daemon). | experimental protocol, shipping daemon |
| Non-interactive exec | `codex exec` with `--json`, `--output-schema`, `-o` last message, `--ephemeral`, `resume`, `fork`; `--dangerously-bypass-approvals-and-sandbox`. | stable |
| `codex queue` | `codex queue --thread <id|name> --message <text>` queues a message into an existing session. | present |
| Cloud tasks | `codex cloud exec --env <id> [--attempts N] [--branch B]`, `status`, `list`, `diff`, `apply`. Each task gets its own workspace and keeps running while the computer sleeps; results come back as a diff or PR. Configurable network access. | experimental CLI, ChatGPT account |
| herdr integration | `herdr integration install codex` supplies lifecycle state and the native session id (what lets Codex resume after a herdr restart). Third-party reviews say Codex and Claude integrations mainly supply session identity and screen-based detection stays primary. | external (herdr), not vendor |
| Not found | A feature flag `agent_message_board` is "under development, false"; `enable_fanout` is "removed". No supported swarm, task DAG, or peer mailbox is shipped. | n/a |

### What is strong

- **A real programmatic protocol.** The app server gives structured thread, turn and approval events and stable thread ids, which is a
  better control surface than pane scraping. It is the strongest reason to consider an app-server adapter for pod later.
- **Subagent approvals are inherited and visible** (press `o` to open the requesting thread in the interactive CLI); in
  non-interactive runs a needed approval fails and the error returns to the parent.
- **Cloud tasks** run unattended and in parallel with best-of-N attempts.

### Limits

- Subagents are children of one thread. They inherit the parent's sandbox and approvals and cannot override provider or MCP servers.
  Nothing in the docs gives a Codex parent a Claude child, or a durable task graph.
- Subagent tokens are additive ("subagent workflows use more tokens than comparable single-agent runs").
- The app-server WebSocket transport is documented as experimental and unsupported; the page also warns that non-loopback listeners
  allow unauthenticated connections by default during rollout.
- Cloud tasks need GitHub-connected environments and return diffs; they do not appear in herdr panes.

## What they cannot do

Mapped to the five axes in the brief.

### 1. Cross-vendor coordination (Claude coordinator, Codex workers)

- Claude: workers are Claude sessions only. Codex could be wired in as an MCP server (the Claude docs' own suggestion; a third-party
  article also describes `codex mcp-server` driven from the Agents SDK), which gives request/response calls, not a supervised worker with its own
  pane, reviewer loop in the same context, retries, and closure reasons.
- Codex: subagents are Codex threads; no Claude child.
- Neither vendor's mailbox or task list is readable by the other. Claude's mailboxes are JSON files under `~/.claude/teams/`, Codex's are
  thread ids on its app server. Only a neutral layer (pod over herdr panes) can address both. pod's identity model (terminal id plus
  ancestor-process check, spec 3.4) and presets (`terra`, `sol`) exist precisely for this.
- **Verdict:** not met by either vendor. This alone justifies pod for the terra to sol loop.

### 2. Durability across crashes

- Claude agent view survives terminal close and process crash, but only for sessions it hosts; it is local and in preview. Agent teams explicitly
  do not restore in-process teammates on `/resume`; the team config directory is deleted at session end; only the task list persists.
  Workflows resume in the same session (or backgrounded), by replaying agents, and rerun everything after the first failed agent.
- Codex: app-server threads are stored and resumable by id; subagent archive/delete reaches descendants. There is no cross-thread task
  ledger and nothing records "attempt 2 of task X failed because Y".
- pod: SQLite WAL store, Dispatch authority, FIFO deliveries replayed until acknowledged, startup hook that reconciles active Dispatches
  against surviving panes, circuit breaker at 3 failures. This is a vendor-neutral ledger the vendors do not have.
- **Verdict:** vendors cover process restart for their own sessions; pod covers *work-state* durability across both. They compose
  rather than compete: pod could rely on `claude --bg` or `codex app-server daemon` for process hosting.

### 3. Proof-based completion

- Claude: teammates notify the lead when idle with a final answer; task completion is a status flag the docs admit "can lag" and teammates
  "sometimes fail to mark tasks as completed". A `TaskCompleted` hook can veto, which is a good primitive but is your own script.
  Workflows return agent results through schemas, which is real proof for that run.
- Codex: `turn/completed` carries a final status; a subagent result is "collected into the final response" by the parent model.
- Neither has a typed completion report with closure reasons, an identity check that the reporter is the active attempt, or the rule that
  looking idle is not completion. pod's `worker_done` (outcome, reason, target, evidence) plus fencing (`caller_fenced`,
  `inactive_dispatch`) is stricter than any vendor surface found.
- **Verdict:** not met by either vendor.

### 4. Terminal visibility to a person

- Claude teams: in-process panel, or tmux/iTerm2 panes. Unsupported in herdr, Ghostty, VS Code terminal.
  Agent view is a Claude-only list. Workflows have `/workflows`.
- Codex: `/subagents` inside one TUI; `codex agents` lists sessions on the daemon; cloud tasks are in a web UI.
- Neither shows the other's agents. herdr already gives one workspace with real panes for both vendors, state tracking, and a tab bar; pod
  adds a coordinator-visible view (`worker-list`, board export, sidebar status, usage line).
- **Verdict:** each vendor is partly met for itself; the combined view is not met by either.

### 5. Cost

- Claude: agent teams about 3 to 4x tokens (third party), and "approximately 7x" in plan mode (official costs page); each teammate keeps
  consuming until it exits; background sessions, workflows and cloud sessions all draw on subscription limits; teammates' cache TTL is 5 minutes
  unless `subagentPromptCacheTtl` is set. Official advice: use Sonnet for teammates, keep teams small.
- Codex: subagents are additive; cloud tasks bill against ChatGPT usage.
- pod adds almost no model cost itself (no daemon, no model calls; `pod usage` is a status line). It does let the coordinator put
  expensive review (`sol` at xhigh) only where wanted, and it uses each vendor's own subscription. The cost comparison is therefore
  "vendor features add parallelism overhead; pod adds orchestration without extra tokens, but also does not remove the overhead of running N
  full sessions".
- **Verdict:** a tie on tokens; pod's advantage is the control to mix a cheap and an expensive vendor per role, which neither vendor offers.

### Additional gaps

- **Approvals and trust dialogs.** Both vendors prompt per pane; pod's observed quirks (trust dialog exits, `needs_input` axis) are handled by
  its own layer. Claude teammates' permission prompts bubble to the lead, which does not help a Codex pane.
- **Specialist routing.** pod's `ask --to dispatch:<id>` and user gates have no vendor equivalent.
- **Outside-in control.** Both vendors assume the model decides to spawn. pod lets the coordinator (any agent kind) own the plan and
  the person see it.

## Where pod is redundant

Items where a vendor feature now does what pod either does or plans to, with a recommendation. "Redundant" means pod need not
build or maintain it for single-vendor use.

| pod capability | Vendor equivalent | Redundant? | Recommendation |
|---|---|---|---|
| Worker spawn with model/effort for Claude-only fan-out | Subagents with `model`, `effort`, `isolation: worktree`; workflows | Yes, for Claude-only work inside one session | For Claude-only tasks, tell the coordinator to use native subagents or a workflow. Keep pod for runs with Codex workers, multi-repo runs, or work that must outlive the coordinator. |
| Worktree creation and enforced isolation | `--worktree`, `isolation: worktree`, background sessions' automatic worktree, command-level guards | Partly | pod already delegates creation to `herdr worktree create`. Do not build pod-side isolation guards. |
| Process hosting and restart of a Claude worker | `claude --bg` supervisor, `respawn` | Partly | pod's startup reconcile covers pane loss; consider an optional `--bg` backed Claude placement later instead of a pane (loses live pane visibility). |
| Peer messaging between workers | Agent-team mailboxes, `SendMessage`, cross-session messaging | Yes for Claude peers | Do not add peer messaging to pod; spec already limits worker to coordinator and specialist questions. |
| Cloud/unattended execution | `claude --cloud`, `codex cloud exec`, Claude routines | Yes (pod v1 excludes remote placement) | Keep it out of v1; document that pod complements, not wraps, cloud tasks. |
| Programmatic Codex control | `codex app-server` (JSON-RPC), `codex queue`, `exec --json` | Not redundant but a better substrate | Evaluate an app-server adapter behind the `Herdr`-style trait to replace pane scraping for Codex input and status; spec 8.2's safe-paste ladder exists because panes are the only channel today. `codex queue` also hints at a cleaner nudge path. Keep pane path as fallback. |
| Quality gates before completion | `TaskCompleted` / `TeammateIdle` hooks | Complementary | Document how a Claude coordinator can add a `TaskCompleted` hook, but pod's `worker_done` evidence rules remain the source of truth for cross-vendor runs. |
| Task graph and dependencies | Agent-team task list (Claude only), workflow script control flow | Overlap for Claude-only | pod's graph is vendor-neutral and persisted; keep. |
| Fleet view | `claude agents`, `codex agents`, `/workflows`, `/subagents` | Overlap per vendor | herdr plus dagr is the cross-vendor view; no change. |

### Risks to watch (these could make pod redundant later)

1. Claude agent teams leaving experimental and adding herdr as a pane backend or a documented non-Claude teammate type. Current docs
   say workers are Claude sessions only. Re-check on each Claude Code release.
2. Codex shipping `agent_message_board` (flag currently "under development"). That would give Codex a native mailbox but still Codex only.
3. A vendor-neutral protocol (for example ACP-style agent servers; a Hermes issue notes Codex does not yet ship an official ACP
   server). If both vendors expose one, pod's pane-based safe input could become a thin client, which is an opportunity, not a loss.
4. Anthropic's Claude `projects` (cloud threads) mean a Claude-only user may no longer need local orchestration at all. pod's user is the
   cross-vendor, local, herdr user, so this is a shrinking-market risk, not a feature risk.

### Recommended actions for pod (documentation level, no repo edits made here)

- State in README "Why pod exists" that pod's reason is cross-vendor supervision with proof-based completion, not spawning; add a short
  "When to use native subagents instead" paragraph.
- Track a spec item: evaluate `codex app-server` and `codex queue` as the Codex adapter, and `claude agents --json` as a read-only
  observation source for Claude workers started with `--bg`.
- Keep presets and `worker_done` strictness as the differentiators; do not add peer messaging, cloud placement or custom isolation.

## Open questions and unverified points

- Claude agent teams' behavior inside herdr panes was not tested (no interactive sessions allowed); the docs list only tmux and iTerm2.
- The Codex "multi-agent v2" feature (`multi_agent_v2`, stable, default false) was not documented in the fetched pages.
- Default Codex subagent limits (6 threads, depth 1) come from a third-party SDK guide, not the official page.
- "Fleet" as a product name was not found in official docs; this report treats `claude agents` (agent view) as the equivalent.
- Codex cloud limits and pricing were not covered by the fetched page.

## Sources

Official Claude Code
- Run agents in parallel: https://code.claude.com/docs/en/agents
- Agent teams: https://code.claude.com/docs/en/agent-teams
- Subagents: https://code.claude.com/docs/en/sub-agents
- Dynamic workflows: https://code.claude.com/docs/en/workflows
- Background sessions and agent view: https://code.claude.com/docs/en/agent-view
- Cross-session messaging: https://code.claude.com/docs/en/cross-session-messaging
- Cloud sessions (Claude Code on the web): https://code.claude.com/docs/en/claude-code-on-the-web
- Worktrees: https://code.claude.com/docs/en/worktrees
- Costs: https://code.claude.com/docs/en/costs

Official Codex (developers.openai.com redirects to learn.chatgpt.com)
- App server: https://developers.openai.com/codex/app-server (fetched at https://learn.chatgpt.com/docs/app-server)
- Cloud tasks: https://developers.openai.com/codex/cloud (fetched at https://learn.chatgpt.com/docs/cloud)
- Subagents: https://developers.openai.com/codex/subagents (fetched at https://learn.chatgpt.com/docs/agent-configuration/subagents)

Third party (lower confidence)
- Codex subagent guide with default limits: https://hexdocs.pm/codex_sdk/10-subagents.html
- Codex as an MCP server with the Agents SDK: https://codex.danielvaughan.com/2026/05/10/codex-cli-agents-sdk-mcp-server-multi-agent-orchestration/
- Hermes agent issue on Codex app-server and ACP: https://github.com/NousResearch/hermes-agent/issues/24445
- Claude agent teams token cost: https://hackernoon.com/navigating-claude-code-agent-teams-in-practice
- herdr overview and integrations: https://www.bitdoze.com/herdr-agent-multiplexer/ and https://smartscope.blog/en/blog/herdr-agent-multiplexer/

Local
- `claude --help`, `claude agents --help` (Claude Code 2.1.299); `codex --help`, `codex features list`, `codex cloud exec --help`,
  `codex queue --help`, `codex app-server --help`, `codex exec --help` (codex-cli 0.160.1)
- ~/Documents/pod/README.md and docs/specs/2026-10-05-pod-design.md sections 1 to 3 and 7
