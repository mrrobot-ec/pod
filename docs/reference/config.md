# Configuration

pod reads one TOML file, `config.toml`. Every section and key is optional; a missing file means all defaults. pod never writes the file. Source: spec sections 3.3 and 4.

## Paths

| What | Location | Override |
|---|---|---|
| Store | `$XDG_STATE_HOME/pod/<session>/pod.db` (default `~/.local/state/pod/<session>/pod.db`) | `POD_STATE_DIR` |
| Config | `$XDG_CONFIG_HOME/pod/config.toml` (default `~/.config/pod/config.toml`) | `POD_CONFIG` |
| Board files | `<state dir>/<session>/runs/<run_id>/run.json` | none |
| Binary link | `~/.local/bin/pod`, pointing at the plugin's installed binary | none |

`<session>` is the herdr session the caller belongs to, derived from the socket path (`HERDR_SOCKET_PATH`, or herdr's default). Each herdr session is a separate server with its own pane IDs, so each gets its own store.

pod resolves these paths itself. It never relies on `HERDR_PLUGIN_STATE_DIR`, which herdr sets only for commands herdr launches.

Store and board files are owner-only: files `0600`, directories `0700`.

### Environment overrides

| Variable | Effect |
|---|---|
| `POD_STATE_DIR` | Replaces the state directory that holds `<session>/pod.db` and the board files. The `<session>` subdirectory is still appended under it, because sessions must keep separate stores. |
| `POD_CONFIG` | Absolute path of the config file to read instead of the default. A set but missing file is a usage error. An unset variable and a missing default file both mean defaults. |

Other variables pod reads: `XDG_STATE_HOME`, `XDG_CONFIG_HOME`, `HERDR_SOCKET_PATH` (session), `HERDR_PANE_ID` (caller identity), and `HERDR_ENV` (inside-herdr detection). pod sets `DAGR_RUN` only in the environment of the board tab `pod board` opens for herdr-dagr.

## Example

```toml
[defaults]
nested_depth_max = 1
heartbeat_minutes = 5
stall_minutes = 8
ask_timeout_ms = 600000
auto_release = true

[agents.codex]
command = "codex"

[presets.terra]
agent = "codex"
args = []

[presets.sol]
agent = "codex"
model = "gpt-5.6-sol"   # optional: a model name the agent accepts
effort = "xhigh"

[repos."/Users/me/code/api"]
base_branch = "dev"
setup = ["bash", "scripts/worktree-setup.sh"]
setup_policy = "wait-for-setup"
```

The `sol` preset is the reviewer: a codex agent at `xhigh` effort. `terra` is the implementer: a codex agent at default effort.

## `[defaults]`

| Key | Type | Default | Validation | Read by |
|---|---|---|---|---|
| `nested_depth_max` | integer | `1` | `>= 0`. At `1`, workers may not start workers. A start whose depth would exceed it returns `nested_worker_depth_exceeded`. | `worker-start`, `dispatch` (preflight); preamble rendering for the `=== SUB-DISPATCH ===` section |
| `heartbeat_minutes` | integer | `5` | `>= 1`. | `worker-start`, `dispatch` (the cadence printed in the preamble) |
| `stall_minutes` | integer | `8` | `>= 0`. Minutes a dispatched worker may go without progress (no heartbeat, message from the worker, `working` observation, or transcript write) before pod stores a `stalled` `escalation` for it and adds the `stalled` attention category. `0` turns stall detection off. A worker waiting on a prompt is never counted; it has its own escalation. | the stall sweep (`check --wait`, `ask`, `pod usage --tick`, mutating commands, idle hooks) |
| `ask_timeout_ms` | integer | `600000` | `>= 1`; values above `1800000` are capped at `1800000`. An explicit `--timeout-ms` on `ask` is capped the same way. | `ask` |
| `auto_release` | boolean | `true` | Acknowledging a `worker_done` (`check --ack`) releases its worker, as `worker-release` would, and `check --wait` first releases every worker of the Run whose report is already acknowledged. Both list the workers in `released`. A worker retained before the acknowledgement keeps its pane. `false` leaves every release to `worker-release` and `worker-gc`. `release_on_wait`, the 0.2.7 name of this key, is accepted as an alias. | `check --ack`, `check --wait` |

The spec fixes the defaults shown, the ask timeout cap, alias restrictions, and launch mappings. The numeric lower bounds and the defaults for omitted alias and preset fields are pod's own. A timeout above the cap is clamped to the cap, not rejected.

The 10-minute silence threshold for the `stale` warning is not configurable in version 1. `stale` (a missed heartbeat) and `stalled` (no progress of any kind for `stall_minutes`) are separate: a worker that keeps writing its transcript can be `stale` without being `stalled`.

## `[limits]`

Caps on the fleet and a usage threshold that pause `worker-start` and `dispatch`. Every key is off by default.

```toml
[limits]
max_active_workers = 20
max_active_per_kind = { codex = 15, claude = 10 }
pause_above_percent = 95
```

| Key | Type | Default | Validation | Read by |
|---|---|---|---|---|
| `max_active_workers` | integer | none (no cap) | `>= 0`. Active means a Dispatch in `dispatched` whose worker is still `active`, across every Run in this herdr session's store. A start while the count is at the cap returns `limit_reached` with `data.limit` `max_active_workers`. | `worker-start`, `dispatch` (preflight) |
| `max_active_per_kind` | table of integers | `{}` | The same count, per agent kind (`data.limit` is `max_active_per_kind:<kind>`). A kind with no entry is not capped. | `worker-start`, `dispatch` (preflight) |
| `pause_above_percent` | number | none (never pause) | `0` to `100`. A start for a Claude or Codex worker is refused with `limit_reached` (`data.limit` `pause_above_percent`, plus `data.window` and `data.usedPercent`) when the latest `pod usage` record for that vendor shows any window at or above it. A record older than 10 minutes, or none, never blocks a start: stale usage proves nothing. Other kinds are not judged. | `worker-start`, `dispatch` (preflight) |

The refusals happen before any Dispatch exists, so a refused start leaves nothing to clean up. Keep the usage records fresh with `pod usage --install`, whose tab bar entry refreshes them. A Run's own budget is not here: it is `run-create --budget-usd` ([commands](commands.md#runs)).

## `[notify]`

Where a decision or a stuck worker is announced beyond herdr's own notification.

| Key | Type | Default | Validation | Read by |
|---|---|---|---|---|
| `command` | array of strings | `[]` (nothing runs) | An argv: first element is the executable, run directly with no shell. pod sets `NOTIFY_TITLE` (the Run objective) and `NOTIFY_BODY` (the question, or the worker's next action as an argv joined by spaces) in its environment. A 5 second limit; a failure, a nonzero exit, or a timeout is one entry in the receipt's `warnings` and never fails the command. | `gate-create --for user`, `check --wait` |

herdr's own notification (`notification.show`, sound `request`) is always attempted first and needs no configuration. Use `command` for a push plugin, for example `command = ["sh", "-c", "my-push \"$NOTIFY_TITLE\" \"$NOTIFY_BODY\""]`.

## `[agents.<kind>]`

A command alias and a launch table for an agent kind.

| Key | Type | Default | Validation | Read by |
|---|---|---|---|---|
| `command` | string | the kind's own executable | A single token or an absolute path only. No interpreter arguments, environment assignments, or shell wrappers. An unconfigured name is refused even if it is on `PATH`. It is validated and recorded in `launch.effective.command`, but it cannot change which binary runs: herdr always launches the kind's own executable. | `worker-start`, `dispatch --inject` |
| `launch` | table | the built-in table for `codex` and `claude`, else none | Keys `always`, `model`, `effort`, `fast`, `yolo`, each an array of argv strings. Each key given replaces that key of the built-in table; a key left out keeps the built-in's (none for other kinds). `always` goes on every launch; for the others `[]` means unsupported. `{model}` and `{effort}` in an entry are replaced by the value, and a non-empty `model` or `effort` without its placeholder is refused at load. Any other key is refused at load. | `worker-start` |

`<kind>` is a herdr agent kind such as `codex`, `claude`, or `grok`, or any other name you give an entry: an `[agents.<kind>]` entry makes the kind known to `--agent` and to presets, so a kind herdr does not know can still get flags. An entry with neither key leaves a built-in's command and launch table unchanged, and it still makes any other name a known kind. An entry without `command` records the kind's own name, which must pass the same rules as `command` (so `[agents.sh]` is refused).

herdr's `agent.start` takes the kind and the arguments after the executable, and nothing else: it always runs the kind's own executable. pod therefore sends herdr only the launch flags (and the `--session-id` it assigns), never the command name, which would reach the agent as its first prompt. A `command` alias is checked and written to the receipt's `launch.effective`; it never makes herdr run another binary.

`launch` puts the flags in a fixed order: `always`, `model`, `effort`, `fast`, `yolo`, then the preset's `args` (an `always` entry the preset's `args` already has is not repeated). A flag the table leaves empty is refused with `launch_mapping_unsupported`, whose `nextSteps` show the snippet to add. Only `codex` and `claude` have built-in tables, written out here so you can copy and change them; `grok` is the worked example for any other kind (its flags as `grok --help` 1.0.52 lists them):

```toml
[agents.codex]
launch = { always = ["--no-daemon"], model = ["-m", "{model}"], effort = ["-c", "model_reasoning_effort=\"{effort}\""], fast = ["-c", "service_tier=\"fast\""], yolo = ["--dangerously-bypass-approvals-and-sandbox"] }

[agents.claude]
launch = { model = ["--model", "{model}"], effort = ["--effort", "{effort}"], fast = [], yolo = ["--dangerously-skip-permissions"] }

[agents.grok]
launch = { model = ["-m", "{model}"], effort = ["--reasoning-effort", "{effort}"], yolo = ["--permission-mode", "bypassPermissions"] }
```

Every Codex worker starts with `--no-daemon`: while the Codex desktop app runs its shared background server, a plain `codex` waits at a dialog asking whether to restart that server, and the brief is never typed. The flag only keeps the session out of `codex agents`, which pod does not use. A Codex too old to know the flag fails to start; give `[agents.codex]` `launch = { always = [] }` to drop it.

`pod doctor` lists, as the `launch-mapping` check, the kinds whose table maps at least one of `--model`, `--effort`, `--fast`, `--yolo`.

## `[presets.<name>]`

A pod addition: a named worker configuration. `<name>` is what you pass to `--agent`.

| Key | Type | Default | Validation | Read by |
|---|---|---|---|---|
| `agent` | string | none (required) | Must be a herdr agent kind or a kind with an `[agents.*]` entry. | `worker-start` |
| `model` | string | agent default | Passed through the kind's launch flag (`codex`: `-m <model>`; `claude`: `--model <model>`). Rejected when the kind's launch table leaves it empty. | `worker-start` |
| `effort` | string | agent default | Passed through the kind's launch flag (`codex`: `-c model_reasoning_effort="<effort>"`; `claude`: `--effort <effort>`). Rejected when the kind's launch table leaves it empty. | `worker-start` |
| `fast` | boolean | `false` | Fast mode. `codex`: `-c service_tier="fast"`. Rejected for every kind whose launch table has no `fast` entry; Claude Code has no command-line flag for its fast mode, so a person toggles it inside the session. | `worker-start` |
| `yolo` | boolean | `false` | No approval prompts. `codex`: `--dangerously-bypass-approvals-and-sandbox`; `claude`: `--dangerously-skip-permissions`. Rejected for a kind whose launch table has no `yolo` entry. Without it the agent stops at an approval prompt for every shell command, including its `pod` commands. | `worker-start` |
| `args` | array of strings | `[]` | Extra argv appended after the launch flags. | `worker-start` |

`--model` and `--effort` on the command line override the preset, and `--fast` and `--yolo` turn their modes on (they cannot turn a preset's `true` off). A `--yolo` launch first records trust for the worker's folder (and the repository a new worktree came from) in the agent's own config, `$CODEX_HOME/config.toml` for `codex` and `~/.claude.json` for `claude`, so a new worktree does not stop at the agent's folder-trust prompt; the entry is the one the agent itself writes after a person chooses "trust", existing entries and files pod cannot parse are left alone, and the receipt's `effects` list each folder trusted. The start receipt reports both `launch.requested` and `launch.effective`.

Resolution of `--agent <name>`, in order: preset name, command alias, herdr agent kind.

## `[repos."<absolute path>"]`

Settings for a repository, keyed by its absolute path.

| Key | Type | Default | Validation | Read by |
|---|---|---|---|---|
| `base_branch` | string | the repo's default branch | A ref name. Used by `--worktree new-top-level` when `--base-branch` is not passed. | `worker-start` |
| `setup` | array of strings | none (no setup) | An argv: first element is the executable. Run in the new worktree, in its own pane. | `worker-start` |
| `setup_policy` | string | `"wait-for-setup"` | `"wait-for-setup"` or `"start-immediately"`. | `worker-start` |

`--setup run | skip | inherit` on the command line selects whether setup runs for a new worktree (default `run`). Current and existing worktrees never rerun setup. With `wait-for-setup`, the prompt is held until setup exits 0 and a setup failure fails the start at stage `setup`. With `start-immediately`, setup runs beside the agent and the receipt reports `setup: running`.

## What pod executes

pod runs only: herdr, configured agent executables, configured repo `setup` argv, and the configured `[notify]` `command`. It never executes text from messages, specs, or transcripts.
