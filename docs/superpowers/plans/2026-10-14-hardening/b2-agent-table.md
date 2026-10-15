# Lane B2: Launch flags for more agent kinds

Spec: `docs/superpowers/specs/2026-10-14-hardening-design.md` section B2. No dependency. Read the Global Constraints in `00-index.md` first.

**Goal:** the model, effort, fast, and yolo mappings become a config table per agent kind, with the two built-in kinds expressed in the same shape, so any herdr agent kind can get flags without a code change.

**Files:**
- Modify: `src/config.rs` (`AgentAlias.launch: Option<LaunchTable>`, built-in tables, `resolve_agent`, `launch_argv`), `src/doctor.rs` (list kinds with a table), `src/engine/trust.rs` only if a kind needs a trust write (none new)
- Modify: `docs/reference/config.md`, `docs/reference/commands-workers.md`, `docs/reference/errors.md` (`launch_mapping_unsupported` next step), spec 7.1, `CHANGELOG.md`
- Test: `tests/engine_store.rs` or the config tests (`grep -ln "Config::load" tests/`), `tests/doctor.rs`

**Interfaces:**
- `pub struct LaunchTable { pub model: Vec<String>, pub effort: Vec<String>, pub fast: Vec<String>, pub yolo: Vec<String> }`; each entry is an argv fragment with `{model}` or `{effort}` substituted; an empty vector means unsupported (the current `launch_mapping_unsupported` error, whose `nextSteps` now shows the `[agents.<kind>]` snippet to add).
- `Config::launch_table(kind) -> Option<LaunchTable>`: the config's table when present, else the built-in for `codex` (`model = ["-m", "{model}"]`, `effort = ["-c", "model_reasoning_effort=\"{effort}\""]`, `fast = ["-c", "service_tier=\"fast\""]`, `yolo = ["--dangerously-bypass-approvals-and-sandbox"]`) and `claude` (`model = ["--model", "{model}"]`, `effort = ["--effort", "{effort}"]`, `fast = []`, `yolo = ["--dangerously-skip-permissions"]`).
- `launch_argv` becomes table-driven; its output for the two built-ins must not change (the existing tests pin it).

### Task 1: Table-driven argv with unchanged built-ins

- [ ] **Step 1: Write the failing test** in the config tests:

```rust
#[test]
fn a_config_launch_table_gives_any_kind_model_effort_and_yolo_flags() {
    let config = toml_config(r#"
[agents.gemini]
command = "gemini"
launch = { model = ["--model", "{model}"], effort = [], fast = [], yolo = ["--yolo"] }
"#);
    let launch = config.resolve_agent("gemini", Some("gemini-3-pro"), None, false, true).unwrap();
    assert_eq!(pod::config::launch_argv(&launch), vec!["gemini", "--model", "gemini-3-pro", "--yolo"]);
    let error = config.resolve_agent("gemini", None, Some("high"), false, false).unwrap_err();
    assert_eq!(error.code, ErrorCode::LaunchMappingUnsupported);
    assert!(error.next_steps.iter().any(|s| s.contains("[agents.gemini]")), "{error:?}");
    // Built-ins are unchanged.
    let codex = config.resolve_agent("codex", Some("gpt-5.6-terra"), Some("high"), true, true).unwrap();
    assert_eq!(pod::config::launch_argv(&codex), vec!["codex", "-m", "gpt-5.6-terra", "-c", "model_reasoning_effort=\"high\"", "-c", "service_tier=\"fast\"", "--dangerously-bypass-approvals-and-sandbox"]);
}
```

`is_builtin_kind` today rejects unknown kinds; a kind with an `[agents.<kind>]` entry (an alias or a table) must be accepted, and `pod doctor`'s `agent-integration:<kind>` check then covers it.
- [ ] **Step 2: Run:** fails. **Step 3: Implement** `LaunchTable`, `launch_table`, the substitution (`replace("{model}", model)`), the refusals built from empty vectors, and `launch_argv` over the table; keep the ordering model, effort, fast, yolo, then `args`. **Step 4: Run** the whole suite (the argv tests, trust tests, and e2e fixtures must be unchanged). **Step 5: Commit:** `git commit -am "feat(config): launch flags as a table per agent kind"`.

### Task 2: Doctor and docs

- [ ] **Step 1: Write the failing test** in `tests/doctor.rs`: with the config above, the report has a check `launch-mapping` with `status: ok` and a detail listing `claude, codex, gemini`; with the default config it lists `claude, codex`.
- [ ] **Step 2: Run:** fails. **Step 3: Implement** the check in `doctor.rs` (always `ok`; informational). **Step 4: Run** the suite. **Step 5: Docs:** `config.md` (`launch` table with the two built-ins written out so a person can copy them), `commands-workers.md` (which kinds accept which flags), `errors.md` next step, spec 7.1 sentence, README's `pod doctor` list adds `launch-mapping`, `CHANGELOG.md`; `cargo test --release --test docs_sync`; commit: `git commit -am "feat(doctor): launch-mapping check; docs for the agent table"`.
