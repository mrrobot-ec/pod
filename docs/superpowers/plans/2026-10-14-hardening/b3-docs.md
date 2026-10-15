# Lane B3: Spec hygiene and positioning

Spec: `docs/superpowers/specs/2026-10-14-hardening-design.md` section B3. Depends on every other lane merged. Read the Global Constraints in `00-index.md` first. This lane changes documentation only; `cargo test --release --test docs_sync --test cli_skills --test spec_rules` and the 400-line cap are its tests.

**Files:**
- Modify: `docs/specs/2026-10-05-pod-design.md` (sections 1, 10, 14, 19), `README.md`, `docs/reference/commands.md`, `docs/reviews/2026-10-12-implementation-notes.md` (deferrals), `CHANGELOG.md` (0.2.0 entry tidied)

### Task 1: Spec text

- [ ] **Step 1:** Section 10 (preamble template): remove the `[=== BASE DRIFT === ...]` line and any sentence describing it; add one sentence in section 19 or 14 recording the decision: "`BASE DRIFT` was dropped on 2026-10-14: counting commits needs git, which pod does not run."
- [ ] **Step 2:** Section 14 (Orca comparison): remove the "automatic coordinator loop" row (Orca v1.4.219 dropped its own); update the row that mentions Orca's dispatch capability (dropped in v1.4.221) and the `blocked` rules (widened in v1.4.221) with the version numbers from `docs/reviews/2026-10-14-research/2-orca-openrig.md`; add the row "Runtime: Orca's app is always on; pod ticks inside the waiting coordinator and the tab bar entry".
- [ ] **Step 3:** Section 1 success criteria: leave the numbers; add "cross-vendor supervision (a Claude coordinator over Codex workers) with proof-based completion is the reason pod exists" as the first sentence of "Why", citing the research folder.
- [ ] **Step 4:** `spec_rules.rs` and `docs_sync.rs` must still pass; commit: `git commit -am "docs(spec): hygiene after the 2026-10-14 research"`.

### Task 2: README and commands

- [ ] **Step 1:** README "Why pod exists": rewrite from `docs/reviews/2026-10-14-research/5-pod-distinctions.md` section "Why pod exists" in at most 12 lines: Orca's workflow without the app, herdr as the only long-lived process, completion by proof, closure reasons, durable acknowledged mail, cross-vendor coordination, no daemon.
- [ ] **Step 2:** New README section "When to use native subagents instead" (at most 10 lines): Claude-only fan-out inside one session belongs to Claude Code subagents or agent teams; pod is for runs with Codex workers, several repositories, or work that must outlive the coordinator; cite `3-vendor-native.md`.
- [ ] **Step 3:** New README section "How pod compares" as a six-row list (Orca, Claude agent teams, ClawTeam, Cyclops, OpenRig, herdr-projects), one line each with what it does better and what pod adds, from `1-terminal-orchestrators.md` and `4-herdr-ecosystem.md`. README must stay at or under 400 lines.
- [ ] **Step 4:** `docs/reference/commands.md` `task-create`: "a dependency must exist in the Run; cycles cannot form because dependencies are fixed at creation; a dependency already `failed` leaves the new Task `pending` under `waiting_on_failed_dependency`".
- [ ] **Step 4b:** Left by other lanes: `docs/ops/troubleshooting.md` (the two places that say typing into a worker pane makes it user-owned or cannot be seen) and `docs/reference/herdr-adapter.md` item 10 now describe `interruption` (lane A4: text pod did not type is seen through the styled screen read, the pane is left alone until bare, and it is not user-owned by that alone); spec 8.2 item 3 shows the new nudge text with the handle and brief parts (lane A3).
- [ ] **Step 5:** Implementation notes: the "Deferred past v1" list keeps only `start_unknown` (with the reason) and states that items 2 to 5 shipped in 0.2.0 with the lane names. `CHANGELOG.md`: order the 0.2.0 lines by lane, one per lane, and set the date.
- [ ] **Step 6:** run `cargo test --release --test docs_sync --test cli_skills --test spec_rules`, `wc -l` on every doc, grep for em-dashes, commit: `git commit -am "docs: README positioning and command notes after 0.2"`.
