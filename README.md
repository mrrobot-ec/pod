# pod

Run a team of coding agents from one chat, inside [herdr](https://herdr.dev).

You open herdr in a repository, start Claude Code, Codex, or Grok in a pane, and describe the job in plain language: what you want, how many workers, which models, who reviews, who advises. That agent becomes the coordinator. It splits the work into tasks, starts the workers in their own tabs and worktrees, waits for their reports, sends reviewers after them, asks an advisor when it is stuck, and asks you only when a decision is yours. pod is the ledger and the messenger underneath. You never type its commands.

## Why install it

- **One request, a whole team.** Ten researchers, two reviewers, and an advisor, without opening a tab or pasting a brief yourself.
- **Mix vendors.** A Claude coordinator can run Codex and Grok workers, a Codex coordinator can run Claude workers, and any agent herdr supports can be a worker. No vendor's own multi-agent feature does that.
- **Nothing finishes on a guess.** A worker is done only when it files a report that proves it, never because its pane looks idle. Every failed attempt says where the work went, and a circuit breaker stops endless retries.
- **Nothing gets lost.** Messages between the coordinator and its workers are durable and acknowledged. They survive context compaction, restarts, and a coordinator that comes back hours later.
- **No daemon.** herdr is the only long-lived process. Every pod call is one short transaction or one bounded wait, so there is nothing to keep running and nothing to crash.
- **You decide, you do not babysit.** A decision only you can make becomes a gate: a herdr notification, a `DECISION` line in the sidebar, a card on the board. Everything else proceeds without you.
- **Stay within limits.** Caps on workers in total and per vendor, a pause when a Claude or Codex rate-limit window is nearly spent, a soft budget per run, and both accounts' usage on herdr's tab bar. Usage and cost are tracked for Claude and Codex; other agents are capped by count.

## Install

Three commands. The first two run anywhere; the third runs inside herdr.

```sh
herdr plugin install mrrobot-ec/pod   # the plugin: binary, hooks, board action
npx skills add mrrobot-ec/pod         # the skills: how your agents learn pod
pod doctor                            # inside herdr: fix anything it reports
```

`herdr plugin install` downloads the release binary for your platform (macOS arm64 or x86-64, Linux x86-64 or arm64), verifies its SHA-256 checksum, and links `~/.local/bin/pod`; when no release matches, it builds from source with cargo. `npx skills add` installs two small skills, `pod-coordinator` and `pod-worker`, into your agents. Each tells the agent to read the full guide from the installed binary (`pod skills get coordinator`), so the guide always matches the version you run. Make sure `~/.local/bin` is on your `PATH`, then run `pod doctor` and fix every `fail` it reports; each entry carries its fix.

Optional, once: `pod usage --install` puts both accounts' rate-limit windows, Claude's session cost, and Codex's credits on herdr's tab bar, and lets the coordinator pause new workers when a window is nearly spent.

Requirements: herdr 0.9.3 or newer on macOS or Linux; Claude Code, Codex, or Grok, or any other agent herdr supports, with its herdr integration installed (`herdr integration install claude`, and the same for `codex` or `grok`); Node for `npx`; Rust only when no release binary matches your platform. herdr-dagr is optional and gives you a board.

To work on pod itself, clone it, run `POD_INSTALL_FROM_SOURCE=1 sh scripts/install.sh`, then `herdr plugin link "$PWD" --enabled`. The link points at `target/release/pod`, so rerun the install script after pulling changes.

## Your first run

Open herdr in a repository, start Claude Code or Codex in a pane, and say what you want. Name the team; pod handles the rest. For example:

> This repo is a course site that teaches machine learning to web developers. I want a second course that takes the same reader from zero to shipping a production Rust service, ending with a chapter on async and FFI, and the site's search is slow on long pages. Use pod. Research and draft the chapters with 10 Codex workers on terra at high effort in fast mode, one chapter each, in their own worktrees. Give the search fix to one Claude implementer. You coordinate: sequence the work, merge, verify, and keep one voice across chapters. Nothing merges before a Claude reviewer on Opus 5.5 at medium effort has read it, and a Grok reviewer at high reasoning effort gives a second opinion on every chapter. When you are unsure how to split or order the work, ask an advisor, Codex on sol at high effort in fast mode. Ask me only for decisions that are mine, like which chapters to cut.

What happens next, without you typing a command:

1. The coordinator reads the pod skill, creates a Run, and turns the request into tasks with dependencies: ten chapters, one search fix, two reviews for each, and the merges.
2. It starts the researchers in their own tabs and worktrees, the implementer, and the advisor, each with a brief that says what to deliver and how to report back. Tabs are named `<handle> · <task>`, so any of them is one glance away.
3. It waits. Workers heartbeat, ask questions, escalate blockers with evidence and a recommendation, and finish with a report that says what they did and where the work is. The coordinator answers, routes each blocker to whoever owns it, and starts a reviewer for every finished change.
4. It asks you once per real decision. herdr shows a notification, the sidebar shows `DECISION`, the board shows the card. Reply in the coordinator's pane, or answer the gate from any shell with `pod gate-resolve`; the run continues.
5. It merges reviewed work, releases every worker, and tells you what shipped, what the reviewers found, and what it could not finish and why.

Name models the way your agents name them; the coordinator passes model, effort, and fast mode through as it starts each worker. If an agent has no flag for what you asked, for example fast mode on Claude, pod refuses that start and the coordinator tells you instead of guessing.

### Things you can say

- "Use pod" near the start, so the agent picks the skill on the first try.
- How many workers, and which agent, model, and effort for each role: researchers, implementers, reviewers, an advisor. The agent can be Claude Code, Codex, Grok, or any other agent herdr supports. Claude and Codex take model, effort, and fast mode out of the box; for another agent the coordinator writes its flags into the config once, as a launch table, and from then on that agent takes them too.
- Where they work: the current folder, a worktree per worker, or another repository.
- Rules for review: who reviews, and that nothing merges without it.
- Limits: a budget for the run in dollars, a cap on simultaneous workers, or a pause when rate limits are nearly spent. The coordinator writes the config for you; you never edit a file.
- Where to reach you: herdr notifies by default, and a `[notify]` command in the config can push to anything else.

### Looking over its shoulder

You do not have to, but you can:

- The herdr sidebar shows each worker's task and phase; the tab bar shows both accounts' usage.
- `pod board` opens herdr-dagr on the run: tasks, dependencies, phases, cost, open decisions.
- `pod worker-list` says what every worker is doing and what the next action for each would be.
- `pod worker-read --dispatch <id>` shows a worker's screen or transcript.

## When to use native subagents instead

If every worker is Claude and the work fits one session, use Claude Code's own subagents or agent teams and skip pod. pod is for runs that include Codex workers, span several repositories, must outlive the coordinator's session, or need proof-based completion and a durable ledger. Every vendor's multi-agent feature coordinates only its own agents; see [docs/reviews/2026-10-14-research/3-vendor-native.md](docs/reviews/2026-10-14-research/3-vendor-native.md).

## How pod compares

- **Orca**: the contract pod follows, with a browser, SSH, and a mobile app; pod gives the same workflow without the always-on Electron app.
- **Claude agent teams**: the native choice for Claude-only work, with a shared task list and hooks; pod adds Codex workers, worktree placement, acknowledged replay, and proof-based completion.
- **ClawTeam**: closest in shape (task dependencies, inboxes, worktrees, no daemon) but agent-reported completion only; pod adds dispatch fencing, ack and replay, closure reasons, and release accounting.
- **Cyclops**: careful about never inferring completion and about human drafts, with a durable mailbox; it needs a daemon and has no task graph, which pod adds.
- **OpenRig**: better at persistent fleets, restore, and adopting sessions; it runs a daemon and is not task-graph first, which pod is.
- **herdr-projects**: a polished herdr plugin with a coordinator, worker threads, and shared memory; pod adds a dependency graph, durable mail with acknowledgement, gates, and worker accounting.

Sources: [terminal orchestrators](docs/reviews/2026-10-14-research/1-terminal-orchestrators.md), [the herdr ecosystem](docs/reviews/2026-10-14-research/4-herdr-ecosystem.md), and [what sets pod apart](docs/reviews/2026-10-14-research/5-pod-distinctions.md).

## Under the hood

pod keeps [Orca](https://github.com/stablyai/orca)'s orchestration contract (command names, flags, JSON shapes), so skills and habits written for Orca carry over with `pod` in place of `orca orchestration`. A Run is the coordinator's namespace and inbox. Tasks carry specs and dependencies, and a Dispatch is one worker's attempt at one Task, with its own mailbox. Workers are herdr panes: pod writes each brief to a file and sends a one-line pointer through herdr's prompt, pasting only as a fallback. Mail is FIFO, acknowledged, and replayed until acknowledged. Each worker is observed on three axes (activity, presence, needs input) with the evidence source recorded, and a bounded wake ladder nudges a quiet worker at most five times before raising `unreachable`. Completion needs a valid `worker_done` with a closure reason, and a circuit breaker stops retries. There is no daemon clock: the waiting coordinator and the tab bar entry tick the ladder. Transcript readers exist for Claude Code and Codex, with best-effort readers for Grok and Cursor Agent, and every kind falls back to its screen. Every change rewrites the Run's `run.json` for herdr-dagr. The design spec has every rule.

Not in this version: remote placement, scheduled jobs, Orca's browser and account features, Windows, and a built-in UI.

## Documentation

- [Design spec](docs/specs/2026-10-05-pod-design.md): the single source of truth
- [Coordinator guide](docs/guides/coordinator.md): what your agent reads from `pod skills get coordinator`
- [Worker guide](docs/guides/worker.md): what a worker reads from `pod skills get worker`
- [Command reference](docs/reference/commands.md), with [worker commands](docs/reference/commands-workers.md) and [message commands](docs/reference/commands-messages.md)
- [Configuration](docs/reference/config.md): presets, limits, notifications, launch tables
- [Errors](docs/reference/errors.md) and [state machines](docs/reference/state-machines.md)
- [Troubleshooting](docs/ops/troubleshooting.md)
- [Worked example](docs/examples/full-run.md): a full run with every command and its JSON
- [Changelog](CHANGELOG.md)
- [Contributing](CONTRIBUTING.md): how to build, test, and send changes

## License

MIT. See [LICENSE](LICENSE) and [NOTICE](NOTICE). pod follows the orchestration contract of Orca (github.com/stablyai/orca, MIT).
