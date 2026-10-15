# Phase B review (Opus)

You are the reviewer for Phase B of `docs/superpowers/specs/2026-10-14-hardening-design.md`, lanes B1 to B3, merged on this branch after Phase A. Read the spec, `00-index.md`, and the lane plans `b1` to `b3`.

1. **Spec conformance** for B1 to B3, rule by rule, test by test, as in the Phase A review; Review Focus item 5 (stale usage never blocks a start) must have its test.
2. **Store migration.** A version-2 store from 0.1.0 opens, migrates to 3, and every existing test fixture still loads; `pod doctor`'s store check reports the new version.
3. **Cost honesty.** Costs come from the agents' own reports and may differ from the bill; `worker-show`, `run-show`, the board note, and the docs say so. A Codex worker without a cost shows `null`, never `0`.
4. **Limits never lock a person out.** With caps hit, `worker-start --terminal <handle>` (reuse) of a pane that is already counted must still work; with a budget exceeded, `worker-release`, `check`, and `gate-resolve` are unaffected. Add tests where missing.
5. **Agent table.** The built-in `codex` and `claude` argv are byte-identical to 0.1.0 (the e2e fixtures and trust tests pin them); a user table for an unknown kind is accepted by `resolve_agent` and by `pod doctor`.
6. **Docs and README.** README at or under 400 lines, the comparison list cites the research reports, the 0.2.0 changelog entry is complete and dated; `docs_sync`, `cli_skills`, `spec_rules` pass.
7. Run the full checks and `sh scripts/live.sh` once more (Phase B touches the start preflight). Fix what fails; commit in small pieces.

Report with `worker_done`: fixes, open items, and whether 0.2.0 can be tagged.

## Findings from the Grok review of lane B2

A Grok reviewer (grok 1.0.52, started through pod with the new agent table) reviewed the B2 merge; its report is `docs/reviews/2026-10-14-grok-review-b2.md`. Resolve each of its six findings in this review, with a test where it names a bug or risk: (1) an empty `command` skips the shell-wrapper ban; (2) a `launch` table replaces the built-in wholesale and unknown keys are silently dropped, so merge the given keys over the built-in, add `deny_unknown_fields`, and fix the spec B2 example to the `launch = { ... }` shape; (3) the spec's "defaults for every kind whose --help documents the flags" claim is not what shipped, so state that only `codex` and `claude` have built-in tables and that `[agents.grok]` is the documented example (its table: `model = ["-m", "{model}"]`, `effort = ["--reasoning-effort", "{effort}"]`, `yolo = ["--permission-mode", "bypassPermissions"]`); (4) a fragment that omits the placeholder still counts as supported; and the two nits in the report.

## Left by lane B3

`docs/guides/worker.md` (the BASE DRIFT line near 142), `tests/engine_preamble_golden.txt`, and `src/engine/preamble.rs` (the drift section it can still render, near line 118) still carry `BASE DRIFT`, which the spec dropped on 2026-10-14. Remove the rendering and the mentions, keep the golden test in step, and confirm `cli_skills` still matches the served guide.
