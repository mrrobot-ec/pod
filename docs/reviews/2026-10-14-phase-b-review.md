# Phase B review, 2026-10-14

Reviewer: Opus, from `docs/superpowers/plans/2026-10-14-hardening/b4-review.md`, on `main` after lanes B1 to B3 merged (`6d8b119`). Each item has a decision and the test that pins it, or the reason there is none. Commits are on `main` after `6d8b119`.

## 1. Spec conformance, B1 to B3

| Rule | Decision | Test |
|---|---|---|
| B1.1 caps refuse `worker-start` with `limit_reached` and a literal limit | holds | `worker_start_refuses_above_the_active_caps_with_a_literal_limit` |
| B1.1 caps and budget refuse `dispatch` before a Dispatch row | holds; had no test, added | `dispatch_refuses_at_a_cap_or_a_spent_budget_before_any_dispatch` |
| B1.1 a fresh window at the threshold pauses starts; a record older than 10 minutes does not | holds | `worker_start_pauses_above_a_fresh_usage_window_and_ignores_a_stale_one` |
| Review Focus 5: missing or unreadable usage never blocks | holds; the missing and unreadable cases had no test, added | `a_missing_or_unreadable_usage_record_never_blocks_a_start` |
| B1.2 `usage --record` stores each payload by `session_id`; a matched Claude worker shows `costUsd` | holds | `a_claude_worker_shows_the_cost_its_status_line_reported`, `tests/cli_usage.rs`, `tests/usage.rs` |
| B1.2 Codex per-thread cost | Codex `app-server` 0.160.1 has no per-thread query; documented in `commands-workers.md` and spec 12.4 | none needed |
| B1.3 `run-create --budget-usd`, `run-show` `spentUsd`/`budgetUsd`, refusal at 100 percent, warning above 80 | holds | `run_create_takes_a_budget_and_run_show_reports_spent_against_it`, `worker_start_refuses_a_run_whose_budget_is_spent_and_warns_near_it`, `a_budget_with_no_matched_session_costs_nothing` |
| 12.4 a session shared by two Dispatches is spent once | held for `spentUsd`, had no test (added); the board note summed it twice, fixed (`8391de2`) | `a_session_shared_by_two_dispatches_is_spent_once`, `a_reported_cost_is_appended_to_the_task_note` |
| B2 table per kind, built-ins in the same shape, `launch-mapping` doctor check | holds after the Grok findings below | `tests/t0.rs`, `doctor_lists_the_kinds_with_a_launch_table` |
| B3.1 section 14 rows, section 10 without `BASE DRIFT` | holds in the spec; the renderer still had it, removed (`979458f`) | `preamble_matches_section_10_golden` |
| B3.2 graph validation stated in `commands.md` | holds (`task-create`) | `docs_sync` |
| B3.3 README "Why pod exists", native subagents, comparison | holds | `docs_sync` |

## 2. Store migration

A version-2 store (the 0.1.0 schema: migrations 1 and 2 are unchanged since 0.1.0, so dropping `budget_usd` and setting `user_version = 2` reproduces it) with a Run already in it opens, migrates to 3, and the Run reads back without a budget (`1e3897f`). Every other fixture builds its store through `Store::open`, which runs migrations 1 to 3, and the whole suite passes. `pod doctor`'s store check reports schema 3 (`doctor_names_the_store_and_binary_schema_versions_without_migrating`, now pinned to 3).

## 3. Cost honesty

`costUsd` was absent when unknown; it is now `null` on `worker-show` and `worker-list` rows, never `0` (`1ff667d`; contract allowance under the new section 14 row "Cost"). The board note reads `· $<cost> reported`. `worker-show` docs, `run-create`, `errors.md`, spec 12.4, and the README say the figure is the agents' own report and may differ from the bill. Test: `worker_show_and_worker_list_print_the_brief_path` (a Codex worker shows `null`), `a_reported_cost_is_appended_to_the_task_note`.

## 4. Limits never lock a person out

With every cap hit, `worker-start --terminal` reuse starts; with the budget spent, `worker-release`, `check`, and `gate-resolve` work (`caps_and_a_spent_budget_never_block_reuse_release_check_or_a_gate`). Open item, by spec: `dispatch` to an existing idle pane is checked against the caps (B1.1 names `dispatch`), while `worker-start --terminal` is not. Both reuse a live agent; the spec text was kept, and the asymmetry is documented in `commands-workers.md`.

## 5. Agent table

`claude`'s argv is byte-identical to 0.1.0. `codex`'s differs by one deliberate token: every launch adds `--no-daemon` right after the command, decided by the Phase A review (`8e05568`) for the Codex desktop app's shared-server dialog; otherwise identical. Pinned by `launch_argv_codex_and_claude`, `every_codex_launch_runs_without_the_shared_daemon`, the trust tests, and the e2e fixtures. A user table for an unknown kind is accepted by `resolve_agent` and listed by `pod doctor` (`a_config_launch_table_gives_any_kind_model_effort_and_yolo_flags`, `doctor_lists_the_kinds_with_a_launch_table`).

## 6. Docs and README

README is 182 lines; its comparison list cites research reports 1, 3, 4, and 5; it now lists limits, cost, and the agent table, and doctor's schema check. The 0.2.0 changelog entry is dated 2026-10-14 and records the review's fixes. `docs_sync`, `cli_skills`, `spec_rules` pass. A new `docs_sync` test holds the 400-line cap and the no-em-dash rule (`dc3e7b2`). The one em-dash in `src/board/contract.rs` was in a comment quoting dagr's "Schema v3 field reference" heading; no code compares that comment to dagr, so it was reworded. The vendored `vendor/herdr/schema-22.json` keeps herdr's own text.

## Grok review of lane B2

| Finding | Decision | Test |
|---|---|---|
| 1. empty `command` skips the wrapper ban | fixed: the kind's own name is validated | `an_agent_entry_without_a_command_still_passes_the_wrapper_ban` |
| 2. a `launch` table replaces the built-in; unknown keys dropped | fixed: keys merge over the built-in; `deny_unknown_fields` on `AgentAlias` and the table; spec B2 shows `launch = { ... }` | `a_launch_table_merges_over_the_builtin_and_refuses_unknown_keys`; `every_codex_launch_runs_without_the_shared_daemon` now drops `--no-daemon` with `always = []` |
| 3. spec claimed `--help`-derived defaults | spec and `config.md` say only `codex` and `claude` have built-in tables, with `[agents.grok]` as the worked example (checked against `grok --help` 1.0.52) | `the_launch_tables_in_config_md_are_the_builtins_and_a_working_grok_example` |
| 4. a fragment without its placeholder counts as supported | fixed: refused at load, and unmapped in `resolve_agent` | `a_model_or_effort_fragment_must_carry_its_placeholder` |
| 5. nit, `config.md` wording | fixed as suggested | `docs_sync` |
| 6. nit, error prose and snippet | fixed: one pasteable snippet per flag, the preset named when it supplied the value | `an_unmapped_flag_names_a_pasteable_snippet_and_its_preset` |

Also: `pod doctor`'s `launch-mapping` listed `codex` with every flag emptied and a kind with `launch = {}`; it now lists the kinds whose merged table maps a flag (`6509fac`).

## Left by lane B3

`BASE DRIFT` is gone from `src/engine/preamble.rs` (the `BaseDrift` type and the section), `tests/engine_preamble_golden.txt`, and `docs/guides/worker.md`; `cli_skills` still matches the served guide (`979458f`). The historical 0.1 plan and research reports keep their mentions.

## 7. Checks and live run

On `2fe15e4`, with the herdr variables unset: `cargo test --release` passed (447 passed, 13 ignored, 0 failed), `cargo clippy --release --all-targets -- -D warnings` and `cargo fmt --check` are clean, `shellcheck scripts/*.sh` is clean, and `POD_E2E=1 scripts/e2e.sh` passed all seven cases on herdr 0.9.4. `sh scripts/live.sh` passed all three tests (every case, both kinds, six `inject: prompt` starts) in 722 seconds; see `docs/reviews/live/2026-10-14.md`, "Rerun on main after the Phase B review".

Not verified: a store written by an actual 0.1.0 binary (the test reproduces its schema from migrations 1 and 2, which are unchanged); `[limits]` and `--budget-usd` against real agents (the live suite sets neither); Codex's `--no-daemon` with no desktop app running (harmless by `codex --help`, not exercised).

Verdict: 0.2.0 can be tagged. The one open item is the `dispatch` and `--terminal` asymmetry under caps (item 4), which follows the spec as written.
