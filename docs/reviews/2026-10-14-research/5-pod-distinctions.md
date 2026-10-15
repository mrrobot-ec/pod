# pod: what distinguishes it, and where it falls short

Scope: pod 0.1.0 as of 2026-10-14, read from the repository at `~/Documents/pod`.
Paths are relative to that repository. "Spec" is `docs/specs/2026-10-05-pod-design.md`;
"Notes" is `docs/reviews/2026-10-12-implementation-notes.md`; "Guide" is `docs/guides/coordinator.md`.
Claims about what is implemented were checked against `src/engine/` where noted; otherwise they
rest on the docs, which say so.

## Distinctions

Each item gives the principle, where it lives, and why it matters to a person running many agents.

### 1. Completion is proof, never absence (`worker_done` is the only way an attempt ends by report)

- The rule: "act on proof, never on absence" (Spec:821; README:115; Guide:13). A worker that looks
  idle has not finished. A worker-reported outcome needs a valid `worker_done` from the active
  Dispatch (Spec:394-396; Guide:15).
- Enforced in the engine: `settle_dispatch` in `src/engine/tasks.rs:140-165` is the single place a
  worker report changes a Task (Notes:24-28). A repeat with the same outcome is `settled`
  with `duplicate: true`; a conflicting one is `inactive_dispatch` (Spec:490-491;
  `src/engine/tasks.rs:140-144`).
- Silence is never auto-failed: `stale` after 10 minutes is a warning only (Spec:451, 829).
  `unverifiable` presence never authorizes stop, abandon, retry or release (Spec:470-472).
- Why it matters: the commonest failure of an agent fleet is the false "done" or the false
  "dead". A coordinator that kills a quiet-but-working agent loses an hour; one that trusts an idle
  prompt ships nothing. Making the agent's own report the only completion signal, and herdr's state
  the only death signal, removes both guesses.
- Honest caveat: the report is still a claim. The guide tells the coordinator to verify it
  before building on it (Guide:112); pod does not check acceptance criteria.

### 2. Closure reasons: a failure always says where the work went

- `worker_done --outcome failed` requires `--reason` (one of seven); `handed_off`, `blocked_on`,
  `superseded` also require `--reason-target` (Spec:489-499; README:19). A target pod cannot find
  is accepted with a warning, not refused (CHANGELOG:26).
- The reason is stored on the Dispatch and shown by `worker-show`, `task-list`, `log` (Spec:497-499).
  `denied` and `canceled` skip the circuit breaker and fail the Task directly, because a retry is
  pointless (Spec:419-422; `src/engine/tasks.rs:149-151`).
- Why it matters: "failed" alone forces the coordinator to read a pane to learn whether to retry,
  unblock, or replan. A typed reason turns a dead end into a routing decision, and it lets the
  breaker treat "a human refused" differently from "the agent got it wrong".
- Source idea is OpenRig (NOTICE, per CHANGELOG:66); the breaker interaction is pod's own.

### 3. Three-axis observation instead of one status

- `activity` (`working | idle_at_prompt | unknown`), `presence` (`present | exited | absent`,
  nullable), `needs_input` (a count with reasons). `liveness` is derived, never stored as the truth
  (Spec:427-441; README:20).
- Each observation records its `evidence_source`: `self_report` proves activity, never presence;
  `herdr_state` is authoritative for presence (Spec:436-441; CHANGELOG:29). Implemented as separate
  writers: `record_self_report` and `record_herdr_state` in `src/engine/observe.rs:64,87`.
- pod deliberately does not read screen text to infer state (Spec:440-441).
- A bare herdr `blocked` status counts as `needs_input`, which was found live when Codex approval
  prompts were invisible (Notes:319; CHANGELOG:14).
- Why it matters: "the pane is open" is not "the agent is alive", and "the agent is alive" is not
  "it is making progress". Collapsing them is how fleets hang quietly. Separate axes let
  `worker-list` say exactly one literal next command per worker (`nextAction`, Spec:454-458), so
  a coordinator agent does not have to interpret state.

### 4. Durable mailboxes with acknowledged, replayed deliveries

- Every message is a SQLite row first; "success means durably stored, nothing more" (Spec:479).
  The coordinator receives FIFO batches (up to 50) that replay until `check --ack` (Spec:563;
  Guide:54).
- A `run-use` bumps `consumer_generation`; older deliveries become `fenced` and are re-offered to
  the new consumer (Spec:262-265). So a coordinator crash or context reset loses nothing
  (Spec:837; Guide:26).
- Mutations carry `--retry-request` receipts keyed by caller fingerprint (Spec:265-267, 836), so a
  lost reply can be recovered with `request-show` instead of a blind repeat.
- Why it matters: agents' contexts get reset, compacted and crashed far more often than servers.
  A durable inbox that survives the coordinator is what lets a 100-task run outlive a single
  coordinator conversation (success criterion 1, Spec:20-22).

### 5. Safe pane input and a bounded wake ladder

- Every line pod types into an agent pane (nudge, preamble, setup command) is pasted without
  submitting, confirmed visible on screen, and only then followed by Enter (Spec:525-532;
  `src/engine/safe_input.rs:112,189`). "pod never sends a bare Enter it cannot attribute to text it
  just confirmed."
- Nudges are bounded: at most 5 per mailbox (`MAX_ATTEMPTS`, `src/engine/wake.rs:20`), the first at
  commit, later ones only on an idle hook or a mutating command at least 60 s apart
  (`wake.rs:21,64-70`), then attention `unreachable`. The position is computed from `wake_attempts`
  and `events`, so a restart does not reset it (Spec:549-551). There is no timer.
- A nudge points at a stored message; it never retypes the body (Spec:510-512, 831).
- Why it matters: typing into a live agent is the riskiest thing an orchestrator does; a blind
  Enter can submit half a paste or answer a prompt. And an unbounded retry loop either spams the
  agent or stalls silently. A bounded, restart-safe policy ending in a visible "unreachable" gives a
  person a defined point at which to look.
- Validated by real use: the first live run withheld Enter on every start until the paste check
  learned Codex's soft-wrapped composer (Notes:317; CHANGELOG:13), and a Claude paste lost its head
  until the settle wait was added (CHANGELOG:35).

### 6. Circuit breaker on cumulative failures

- `failure_count` is cumulative per Task across retries; below 3 the Task returns to `ready`, at 3
  the Dispatch is `circuit_broken` and the Task `failed` (Spec:414-418;
  `src/engine/tasks.rs:152-158`). A failed start and a failed `dispatch --inject` count too
  (Spec:423; Guide:118).
- Why it matters: it caps the money and time an unattended loop can spend on one bad Task. It is
  the only cost guard pod has in the engine (see Weaknesses).

### 7. No daemon; herdr is the only long-lived process

- Every invocation runs one short transaction or one bounded wait, then exits (Spec:89-91; README:11).
  Waiting uses a per-run signal file with an OS file watch and a 1 s `data_version` fallback
  (Spec:94-97; `src/store/signal.rs`). When no one waits, herdr runs `pod hook` on
  `pane.exited`, `pane.closed`, `pane.agent_status_changed` and at startup (Spec:98-104, 144-146).
  After a herdr restart, startup reconciliation marks missing panes `exited`, never `failed`
  (Spec:838; `src/engine/reconcile.rs:22`).
- Why it matters: nothing extra to start, supervise, upgrade or leave orphaned. State lives in one
  SQLite file (WAL, 0600; Spec:204, 867); upgrading the binary mid-run is safe because migrations
  only add (Spec:205-206, 842). For a person this means "the multi-agent system" is just the
  terminal they already have open.
- Cost: no process can do work on a timer (see Weaknesses).

### 8. Escalation that reaches a person once, with evidence

- `ask --to dispatch:<id>` routes a question to a specialist worker, copying the coordinator; the
  first reply wins (Spec:636-645). Escalations must carry `--evidence` and `--recommendation`
  (`escalation_evidence_required`, Spec:648-652). A user gate needs evidence, a nonempty options
  list, and a recommendation that is one of the options (Spec:653-659).
- Gates are for coordinator-owned DAG decisions, with the dependency rule intact: a resolved gate
  returns the Task to `ready` only if all dependencies are complete (CHANGELOG:22).
- Why it matters: the person is the scarcest resource. What reaches them is a decision with a
  default, not a transcript. The routing rule (name the blocker, approval prompts are the
  coordinator's job, only product decisions go to the user) is in the guide and is explicitly not
  enforced by pod (Spec:667; Guide:186).

### 9. Honest accounting of every worker

- After an accepted `worker_done` the coordinator must reuse, retain or release; the turn is not
  finished while `worker-list --terminal-state reclaimable` returns rows (Spec:397-411; Guide:88-96,
  192-198).
- Release archives the final screen text for every agent kind, and closes only a tab pod created,
  and only the worker's own pane if a person split another in (Spec:401-404; CHANGELOG:24, 71).
  pod never removes, cleans or commits in a worktree (Spec:378-379).
  A pane the user types into or closes becomes user-owned (Spec:407-408), though typing into a
  live pane is not detected in 0.1.0 (Notes:310).
- Why it matters: leaked panes, orphan worktrees and agents burning tokens in forgotten tabs are
  the daily cost of many-agent work. A checkable "nothing is left" query closes that loop.

### 10. Folder trust handled for unattended workers (`--yolo`)

- Before an unattended launch pod records trust for the worker's folder (and the source repository
  for a new worktree) in the agent's own config: Codex `config.toml`, Claude `~/.claude.json`
  (Spec:188-195; `src/engine/trust.rs:1-30`). pod never answers the prompt itself.
- Found live: a Codex worker in a new worktree stopped at "Trust and continue", and a `-c` override
  did not clear it (Notes:321; Spec:959). Files are rewritten atomically with their mode kept;
  an unparseable file is left untouched (`trust.rs:17-19`).
- Why it matters: this is the classic "ten workers are all waiting at the same dialog" failure.
  The design keeps the safety property that pod does not impersonate the person at a prompt, and
  instead performs the one durable consent the person already gave by choosing `--yolo`.
- Caveat: `--yolo` also disables approval prompts entirely, so the real safety boundary shifts to
  the sandbox and the spec's ownership clause (Guide:42, 71).

### 11. Board export and account usage bar (visibility without a UI)

- Every commit rewrites `run.json` in herdr-dagr contract v3 through a temp file and rename
  (Spec:773; README:24). Task state mapping never contradicts the attempt record (Spec:793-797).
  Evidence is labelled `reported`, `heuristic`, or `asserted` (Spec:784-786), so the board shows
  how much each fact is trusted. Also writes `<task> · <phase>` to each worker workspace's sidebar.
- `pod usage` puts both accounts' rate-limit windows, Claude session cost and Codex credits on
  herdr's tab bar, collapsible with `prefix+u` (Spec:807-817; README:23;
  `src/usage/mod.rs:127,136`).
- Why it matters: someone supervising ten agents needs a single glance at "who is blocked on me"
  and "how close am I to a rate limit". pod gets this by exporting data to existing herdr
  surfaces, not by building a UI (Spec:34).
- Caveat: the board pane is a separate program (herdr-dagr) and the notes record that only a
  stand-in dagr was exercised in tests at the final review (Notes:367); CHANGELOG:15 later says the
  export was brought to `dagr check --strict` against dagr 0.3.1.

### 12. Orca contract compatibility, with tested differences

- Command names, flags and JSON shapes follow Orca (README:9), checked by 35 contract samples with
  252 allowed differences, each with a reason (Notes:98-100, 213-215). The differences table is
  Spec:849-863.
- Why it matters: existing habits and skills transfer; leaving Orca costs a rename, not a retraining.
  This is mainly a migration property rather than a general design strength.

## Weaknesses

Compared with what a 2026 user expects. A brief web search (Appendix) found the common 2026 picture
to be: parallel agents in separate git worktrees with the human as coordinator, vendor-native
multi-agent features (Claude Code Agent Teams, Codex parallel agents, Devin sandboxed sessions), and
cost framed as wasted tokens from uncoordinated agents. It found no dedicated cost-control dashboard
product and little on remote orchestration, so some expectations below are inferred, and are marked.

### W1. Scale: the design is single-machine and untested beyond tens of agents

- No cap on concurrent workers (Spec:34; CHANGELOG:89). That is stated as a v1 omission, not a
  feature: nothing stops forty `worker-start`s from exhausting rate limits, RAM or herdr's
  tab bar. The 3-retry breaker bounds failure per Task, not fleet size.
- Performance targets are for a 100-worker Run on one machine (Spec:881), and
  `worker-list` is measured in the engine, not through the CLI (Notes:297-298).
- One `check --wait` waiter per Run (Spec:572); parallel coordinators on one Run are not supported.
- The only real runs so far are ten Codex researchers and one worktree test (Notes:315-323). The
  spec's own success criterion (about 100 tasks, 5 worktrees, several repos; Spec:20-22) has not
  been demonstrated.
- Linux x86-64 native and macOS beyond arm64 are unverified; Linux was last run in wave 2 and not at
  the final review (Notes:127, 281, 365).

### W2. No remote or cloud placement

- `--on` is refused with `remote_unsupported` (Spec:301-302; README:13). A user who expects
  to push work to a remote box, a cloud sandbox, or a second laptop (inferred expectation; the
  search found little concrete evidence of how common this is) cannot.
- The store is a local SQLite file per herdr session (Spec:114-116); there is no sharing between
  machines or teammates. Caller identity relies on local process ancestry (Spec:128-131), which has no
  remote analogue.

### W3. Cost control is thin

- What exists: the 3-failure breaker (`src/engine/tasks.rs:152-158`) and the passive usage bar
  (README:23). Both are advisory or per-Task.
- What is missing for a person worried about spend: a per-Run or per-worker token or dollar budget,
  an automatic pause on a rate-limit threshold, a cost column per Task on the board, a record of what
  each Dispatch cost. `pod usage` shows account-wide windows and Claude session cost, not per-worker
  cost, and nothing acts on it (Spec:807-817).
- The documented recommendation is `--yolo` for every unattended worker (Guide:42), which makes
  runaway cost a policy question for the coordinator agent, not something pod limits.
- No model-selection policy: presets are static and the coordinator chooses.

### W4. Heavy reliance on the coordinator agent's discipline

- Several important rules are guide text, not engine rules: the routing rule (Spec:667), the task
  spec contract of target/change/constraints/ownership/acceptance (Spec:617-618, "pod does not
  parse specs"), verification before accepting a success (Guide:112), and the reclaimable check
  before ending the turn (Guide:192). A weaker or tired coordinator model can skip any of them.
- Specs carry no machine-checkable acceptance; pod cannot run the acceptance command or refuse an
  overlap of file ownership between parallel workers. Merging, conflict resolution and review
  gating are the coordinator's job, which is the same gap that 2026 write-ups describe in other
  worktree-based orchestrators.
- The Orca-like model also assumes a coordinator that waits in a blocking `check --wait`. There is
  no automatic coordinator loop (Spec:863), so if the coordinator agent stops, nothing continues.

### W5. Everything proactive depends on events; there is no timer

- The wake ladder advances only on an idle hook or a mutating command (Spec:553-555). Read-only
  commands never advance it. If nobody runs a mutating command and herdr emits no idle event, an
  unreachable worker stays at attempt N indefinitely. By design "exhaustion is a count, not an
  elapsed time".
- The 3-second quiet check made nudges skip when proof was unavailable; the final review flagged
  mail sent within 3 s of a recipient going idle as waiting for the next trigger (Notes:334).
- No scheduled jobs (README:13), so recurring work (nightly, "re-run until green") is out of scope.
- The `stale` warning at 10 minutes is a warning only and no action is attached (Spec:451, 829).

### W6. UI and observability are second-hand

- No built-in UI; the board is the optional herdr-dagr pane (Spec:34, 799-803). No remote or
  web view, no notifications to a phone or chat (the preamble forbids workers from posting to
  Slack/GitHub, Spec:691). A person away from the terminal learns nothing.
- The sidebar `DECISION` status and a board card for an untasked user gate are deferred, so a
  pending decision for the person can be invisible on the board (Spec:660-662; CHANGELOG:90;
  Notes:311).
- The attention categories `guidance` and `interruption` are defined but never produced
  (Notes:308; Guide:135, 139).
- "Detecting a person typing into a live worker pane" is not implemented (Notes:310), so a person
  who types into a worker can collide with pod's own safe input.
- Sidebar token rendering was never seen (the call returns ok; Notes:367).

### W7. Agent coverage is narrow and vendor-brittle

- Launch mappings exist for Codex and Claude only; other kinds reject `--model`, `--effort`,
  `--yolo`, and `--fast` works only for Codex (Spec:195-198).
- Several behaviours are pinned to specific versions with fixtures: Codex 0.160.1 composer
  rendering (Spec:960), herdr 0.9.3 `blocked` semantics (Spec:961), trust file formats (Spec:959,
  `src/engine/trust.rs`). Each vendor update is a possible regression, and the first live
  pilot already hit three such mismatches (Notes:315-321; CHANGELOG:35).
- Transcript readers for Grok and Cursor are best-effort and untested live (Spec:761; Notes:366).
  Claude Code's trust entry "was not exercised live" (Notes:323).
- The vendor-native multi-agent features mentioned in 2026 coverage (Claude Agent Teams, Codex
  parallel agents) are not integrated; pod orchestrates top-level agent CLIs, which is complementary
  but means a worker cannot spawn its own sub-team (`nested_depth_max = 1`, Spec:155).

### W8. Verification gaps in what shipped

- `start_unknown` is specified but has no code path (Notes:307); the spec's recovery table still
  lists it (Spec:834).
- The `=== BASE DRIFT ===` preamble section is never rendered (Notes:309).
- Live worker-from-worktree `worker_done` was not proven end to end because of an isolated-harness
  state-directory issue (Notes:323).
- At the final review: Linux not run, real agents' quiet-window proof only with a scripted agent,
  seven contract samples hand-authored, install script release path and CI untested because there is
  no remote (Notes:365-370). Both repository-based installs in README:46-51 are therefore
  forward-looking.
- A `worker-stop` on a worker whose pane a person closed is handled; a pane the person typed into
  is not (see W6).

### W9. Distribution and bus factor

- Personal tool: no GitHub repository, no release pipeline, a local-checkout install (README:34-53;
  Spec:913). It depends on herdr plugin hooks and on herdr-dagr, both small ecosystems, and on
  Orca's contract, which pod follows but does not control.
- macOS and Linux only (README:13, 28). Windows is out.

### W10. Security model is mostly "trust the agent"

- Identity is terminal ID plus process ancestry, which blocks accidental cross-talk but is not a
  security boundary against a hostile agent in the same user account (Spec:128-131; spec section 19 says it may
  fall back to terminal ID alone under the Codex sandbox, Spec:954).
- Message bodies and specs are treated as data and never executed (Spec:870-871), which is the right
  stance, but nothing sandboxes the agents themselves; that is left to Codex/Claude settings and
  `--yolo` disables them (Spec:198-199, 191-195).
- `store_access_denied` advice is fixed per kind and may not name the installed version's key
  (Notes:369).

## What would close the most valuable gaps

Ordered by value to a person running many agents (a judgment, not from the docs):

1. A per-Run budget and concurrency cap that `worker-start` checks (W1, W3). The breaker and usage
   bar already provide the data model; the missing step is acting on it.
2. Surface "needs a person" outside the terminal (W6): the untasked user gate board card and the
   `DECISION` sidebar status that the spec already defers.
3. A live multi-day run at the success-criterion scale, plus Linux and Claude trust verification
   (W1, W8).
4. Remote placement (W2), only after the single-machine path is proven.

## Why pod exists

Spec section 1 (Spec:5-16) says pod gives herdr the supervised multi-agent workflow Orca provides:
a coordinator agent creates a Run, defines Tasks with dependencies, starts workers in panes or new
worktrees, exchanges durable messages with them, and accounts for every worker until it is reused,
retained or released. Orca does this today but needs its Electron app as the long-lived process;
the goal is "to stop using Orca entirely while keeping the same workflow", so pod follows Orca's
command names, flags and JSON shapes so habits and skills carry over with `pod` in place of
`orca orchestration`, while making herdr the only long-lived process and adding the rules that
real multi-agent runs showed to matter: proof-based completion, closure reasons, separate
observation axes, safe pane input and a bounded wake ladder. Its success criteria are concrete:
coordinate a roughly 100-task, five-worktree, multi-repository build with Orca uninstalled; run the
implementer-then-reviewer loop in the same pane; install with one command and have `pod doctor`
explain the rest; and keep non-waiting commands under 50 ms (Spec:18-28).

## Sources

Pod repository (all under `~/Documents/pod`):

- `README.md` (150 lines; esp. 7-24, 76-115)
- `docs/specs/2026-10-05-pod-design.md` (963 lines; esp. 5-34, 89-104, 128-131, 188-199, 204-277,
  394-422, 427-473, 479-557, 563-587, 606-674, 773-817, 821-844, 849-863, 867-873, 947-963)
- `docs/guides/coordinator.md` (202 lines; esp. 11-20, 42, 54, 88-118, 147-158, 169-186, 192-202)
- `docs/reviews/2026-10-12-implementation-notes.md` (386 lines; esp. 24-28, 303-337, 363-370)
- `CHANGELOG.md` (90 lines; esp. 11-35, 81-90)
- Code skimmed: `src/engine/tasks.rs:120-165` (breaker, settlement), `src/engine/wake.rs:20-23, 50-110,
  482` (ladder constants and triggers), `src/engine/safe_input.rs:112, 189, 251, 306, 371` (paste,
  confirm, Enter), `src/engine/observe.rs:64, 87, 135` (two evidence sources, projection),
  `src/engine/trust.rs:1-30` (folder trust), `src/engine/reconcile.rs:22` (startup reconciliation),
  `src/usage/mod.rs:127, 136` (usage bar). Module sizes: `src/engine/` is 7,369 lines across 19 files,
  `lifecycle.rs` being 2,491. I did not audit every branch; behaviour I did not read in code is cited
  to the docs and is labelled as such.

External (brief search, 2026-10-14; the search tool returned a summary and no page bodies, so only
claims in its summary are used and I did not read these pages):

- [open source agent orchestrators](https://www.augmentcode.com/tools/open-source-agent-orchestrators)
- [Multi-Agent Orchestration for Coding (2026)](https://www.morphllm.com/multi-agent-orchestration)
- [Orchestration topology is the new bottleneck in agentic coding](https://dev.to/terminalblog/orchestration-topology-is-the-new-bottleneck-in-agentic-coding-487g)
- [Multi-Agent AI Coding Workflows (2026)](https://www.tembo.io/blog/multi-agent-ai-coding-workflows)
- [git worktrees parallel AI coding agents guide 2026](https://noqta.tn/en/blog/git-worktrees-parallel-ai-coding-agents-guide-2026)
- [Agentmaxxing: Parallel Multi-CLI Orchestration](https://codex.danielvaughan.com/2026/04/11/agentmaxxing-parallel-multi-cli-orchestration/)
- [Parallel coding agents guide](https://superset.sh/blog/parallel-coding-agents-guide)

Limits of the external evidence: the search found no cost-dashboard or remote-orchestration product
details, so W2 and W3's "what users expect" is partly an inference; the weaknesses themselves are
facts about pod and stand on the repository citations.
