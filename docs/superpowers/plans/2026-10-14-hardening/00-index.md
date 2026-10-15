# pod 0.2 Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close every single-machine gap the 2026-10-14 research found: herdr-native injection, a clock for the wake ladder without a daemon, briefs that survive compaction, the `interruption` and `guidance` categories, decisions visible away from the terminal, a live suite, then caps, cost, budget, more agent kinds, and the docs.

**Architecture:** pod stays one Rust binary with no daemon; herdr is the only long-lived process. Each lane below is one worker Task in its own child worktree with exclusive file ownership, merged by the coordinator, then reviewed by an Opus worker per phase. Every rule is pinned by an engine test on `FakeHerdr` before the code exists.

**Tech Stack:** Rust 2024 edition, rusqlite, clap 4, serde_json, toml_edit, herdr 0.9.3 socket API, cargo test/clippy/fmt.

**Spec:** `docs/superpowers/specs/2026-10-14-hardening-design.md`, companion to `docs/specs/2026-10-05-pod-design.md`.

## Global Constraints

- herdr 0.9.3 or newer; pod never runs `git` itself (spec 7.2); the worker may run `git add` and `git commit` in its own worktree only, never `git worktree`, `push`, rebase, or branch deletion, and never `gh`.
- `cargo test --release`, `cargo clippy --release --all-targets -- -D warnings`, and `cargo fmt --check` pass at every commit; run tests with the herdr variables unset: `sh -c 'unset $(env | grep "^HERDR_" | cut -d= -f1); cargo test --release'`.
- Non-waiting commands return within 50 ms (spec 16): no sleeps or pane reads on the critical path of a plain command.
- Every file under `docs/` stays at or under 400 lines except `docs/specs/2026-10-05-pod-design.md`; no em-dashes anywhere in docs or code comments; `tests/docs_sync.rs` must pass (every clap flag and error code mentioned in docs).
- Conventional commit messages, small commits.
- Error codes are snake_case in JSON (`ErrorCode` in `src/error.rs`, documented in `docs/reference/errors.md`); receipts use camelCase keys as the existing ones do.
- `CHANGELOG.md`: add lines under a new `## [0.2.0] - Unreleased` heading at the top (create it if absent); never edit the 0.1.0 entries.
- Tests create their engine through `tests/support/mod.rs` (`TestEnv::new()`), which already sets `input_settle` to zero; add new tunables there the same way.

## Review Focus

Inputs the spec implies but no lane's tests exercise, most likely to bite first. Each line names the owning lane, which adds the test.

1. herdr answers `agent.prompt` with `agent_prompt_stalled` after the text went in: pod must not paste again (a duplicate brief) but record `dispatched_unobserved`. Lane A1.
2. The tab bar tick runs every second while the coordinator also waits: two processes must not both nudge the same mailbox inside one minute (the per-mailbox lock and the 60-second rule hold across processes). Lane A2.
3. A nudge for the Run mailbox (the coordinator) has no brief file: the nudge text must omit the brief sentence rather than print a path that does not exist. Lane A3.
4. A person types into a worker pane, then deletes the text: `interruption` must clear on the next bare read, and a pending nudge must then go through. Lane A4.
5. `pause_above_percent` with a usage record older than 10 minutes or absent: starts must proceed, with no error, because stale usage proves nothing. Lane B1.

## Lanes

| Lane | Plan file | Depends on |
|---|---|---|
| A1 prompt injection | `a1-prompt-injection.md` | none |
| A2 clock | `a2-clock.md` | none |
| A3 briefs | `a3-briefs.md` | A1 merged |
| A4 attention | `a4-attention.md` | A1 merged |
| A5 decisions | `a5-decisions.md` | none |
| A6 live suite | `a6-live-suite.md` | A1 to A5 merged |
| B1 limits, cost, budget | `b1-limits-cost-budget.md` | A2 merged |
| B2 agent table | `b2-agent-table.md` | none |
| B3 spec hygiene and README | `b3-docs.md` | A and B merged |

## Execution

1. The coordinator creates one pod Task per lane with the plan file as the brief, dependencies as above, and starts each worker with `--agent claude --model claude-sonnet-5-5 --effort high --yolo --worktree new-child --name <lane>` so each lane has its own branch and build directory.
2. A worker reads its plan file and the spec, follows the steps in order, commits on its branch, runs the full checks, and reports `worker_done` with the diff stat and any step it could not complete.
3. After the Phase A lanes report, the coordinator merges the branches into `main`, runs the full checks, and starts an Opus reviewer (`claude-opus-5-5`, effort `high`) with `a7-review.md` as its brief. The same for Phase B with `b4-review.md`.
