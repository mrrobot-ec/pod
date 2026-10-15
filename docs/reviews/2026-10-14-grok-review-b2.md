# Review: merge d495d2d (lane B2, agent launch table)

Reviewed `git diff 135b8cb..d495d2d` hunk by hunk against `docs/superpowers/plans/2026-10-14-hardening/b2-agent-table.md` and section B2 of `docs/superpowers/specs/2026-10-14-hardening-design.md`. Line numbers are from `d495d2d`. No repository file was modified.

## Rules

- Built-in `codex` and `claude` argv is unchanged. `builtin_launch_table` (`src/config.rs:61-76`) reproduces the old match arms: codex `-m {model}`, `-c model_reasoning_effort="{effort}"`, `-c service_tier="fast"`, `--dangerously-bypass-approvals-and-sandbox`; claude `--model {model}`, `--effort {effort}`, empty `fast`, `--dangerously-skip-permissions`. Order stays model, effort, fast, yolo, then args (`src/config.rs:327-349`).
- A config `[agents.<kind>]` `launch` table gives that kind its flags. `resolve_agent` plus `launch_argv` substitute `{model}` and `{effort}` (`src/config.rs:176-210`, `320-350`). The gemini test pins `gemini --model gemini-3-pro --yolo`.
- An empty fragment refuses with `launch_mapping_unsupported` and a `nextSteps` entry that contains `[agents.<kind>]` (`src/config.rs:188-208`).
- `pod doctor` lists the kinds. `launch_mapping_check` (`src/doctor.rs:197-210`) is always `ok` and joins `claude`, `codex`, plus every agent whose `launch` is `Some`, in sorted order.

## Findings

### 1. bug , an empty `command` skips the shell-wrapper ban

File: `src/config.rs:233-235`, resolved command at `src/config.rs:170-175`, ban at `src/config.rs:307-312`.

`validate` calls `validate_command` only when `alias.command` is non-empty. `resolve_agent` then uses the kind name when the command is empty. `validate_command` rejects a basename of `sh`, `bash`, `env`, `sudo`, and the rest of that list. `[agents.sh]` (or `[agents.env]`) has an empty command, so load succeeds, the kind is known (`src/config.rs:223-225`), and the executable is `sh`. The same binary written as `command = "sh"` is refused. Before this merge, `[agents.sh]` failed load because the key was not a herdr kind.

Suggested fix: in `validate`, run `validate_command` on the command that will actually launch, using the kind name when `command` is empty.

### 2. risk , a `launch` table replaces the built-in, and unknown keys are dropped

File: `src/config.rs:215-220`, `src/config.rs:47-53`, `src/config.rs:36-43`. Spec shape: `docs/superpowers/specs/2026-10-14-hardening-design.md:188-193`.

`launch_table` uses the config table whenever `alias.launch` is `Some`, and does not merge it over the built-in. `LaunchTable` is `#[serde(default)]`, so a missing key becomes `[]`, which means unsupported. `AgentAlias` does not use `deny_unknown_fields` (nothing in the crate does).

Evidence: this loads and then disables codex effort, fast, and yolo, with no config error:

```toml
[agents.codex]
launch = { model = ["--model", "{model}"] }
```

The TOML in spec B2 is not the shape the code reads. Those keys are ignored, `launch` stays `None`, the kind becomes known, and `--model` / `--effort` / `--yolo` come back as `launch_mapping_unsupported`:

```toml
[agents.gemini]
model = ["--model", "{model}"]
effort = []
yolo = ["--yolo"]
```

`pod doctor` still lists `codex` (it is hardcoded) and does not list this gemini, because `launch` is not `Some` (`src/doctor.rs:198-204`). A `launch = {}` on codex is `Some` of an all-empty table, so doctor keeps listing codex while every flag is refused.

Suggested fix: `deny_unknown_fields` on `AgentAlias` and `LaunchTable`. Merge only the keys that appear in the TOML over the built-in table. Update spec B2 so its example is the `launch = { ... }` table.

### 3. risk , spec B2's per-kind `--help` defaults are not shipped

File: `src/config.rs:61-76`. Spec: `docs/superpowers/specs/2026-10-14-hardening-design.md:195-197`.

The spec says the implementation ships defaults for every kind whose installed `--help` on this machine documents the flags. `builtin_launch_table` matches only `codex` and `claude`. `grok`, `cursor`, `opencode`, and the other `HERDR_AGENT_KINDS` (`src/config.rs:273-292`) stay `launch_mapping_unsupported` until a person writes a table. The plan's interface section narrowed the built-ins to those two and made every other kind config-only. The code matches the plan. The spec sentence is unmet.

Suggested fix: edit spec B2 so it states that only `codex` and `claude` have built-in tables, or add a table for each kind whose `--help` documents the flags.

### 4. risk , a fragment that omits the placeholder still counts as supported

File: `src/config.rs:188-196` and `src/config.rs:327-333`.

Refusal is only `requested && fragment.is_empty()`. `launch_argv` does `part.replace("{model}", model)` and appends whatever that returns. `model = ["--model"]` is non-empty, so `--model gpt-5.6` resolves successfully and the argv is `cmd --model` with the value dropped. The same applies to `{effort}`.

Suggested fix: when model or effort is requested, refuse with `launch_mapping_unsupported` unless the fragment contains `{model}` or `{effort}`. Do that in `validate` for the config table and in `resolve_agent` before building argv.

### 5. nit , `config.md` contradicts the resolver

File: `docs/reference/config.md:79`.

The sentence "so a CLI herdr has no kind for can still get flags" does not parse. The next clause, "An entry with neither key changes nothing," is false for a new name. `[agents.gemini]` with no keys makes `gemini` known (`src/config.rs:223-225`) and adds `agent-integration:gemini` and `sandbox:gemini` (`src/doctor.rs:214-216`). It changes nothing only for a built-in whose command and launch table stay on their defaults.

Suggested fix: "so a kind herdr does not know can still get flags. An entry with neither key leaves a built-in's command and launch table unchanged, and it still makes any other name a known kind."

### 6. nit , the error prose is not the snippet it describes

File: `docs/reference/commands-workers.md:76`, `docs/reference/errors.md:31`, text built at `src/config.rs:202-206`.

`commands-workers.md` says `` `--model`/`--effort`/`--fast`/`--yolo` the kind's launch table leaves empty ``, which drops the noun. `errors.md` says `nextSteps` shows `launch = { model = ["--model", "{model}"], yolo = ["--yolo"] }`. The live step is `launch = { effort = [...] }`, which is not valid to paste, and it always says "Remove the effort flag" even when the value came from a preset. The previous fast and yolo errors named the preset.

Suggested fix: write "flags the kind's launch table leaves empty." Emit one concrete fragment per flag, with `{model}` or `{effort}` filled in, and name the preset when `preset_model`, `preset_effort`, `preset_fast`, or `preset_yolo` supplied the value.

## Tests

`sh -c 'unset $(env | grep "^HERDR_" | cut -d= -f1); cargo test --release --test t0 --test doctor'` in the working tree failed to compile: `src/engine/safe_input.rs` contains `<<<<<<< HEAD` / `>>>>>>> a4-attention`. That marker is not in `d495d2d` (`git grep` on that commit is clean). The working tree picked up an in-progress merge during this review. It was left as it was.

The same command was run on a read-only `git archive` of `d495d2d` with `CARGO_TARGET_DIR=/tmp/pod-b2-target`. Result: `tests/t0.rs` 28 passed, `tests/doctor.rs` 6 passed, exit 0. That includes `a_config_launch_table_gives_any_kind_model_effort_and_yolo_flags`, `launch_argv_codex_and_claude`, `config_fast_is_codex_only_and_parses_from_preset`, `config_yolo_maps_to_each_agents_bypass_flag_and_parses_from_preset`, and `doctor_lists_the_kinds_with_a_launch_table`.

Verdict: approve with reservations
