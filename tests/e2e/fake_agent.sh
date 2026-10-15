#!/usr/bin/env bash
set -euo pipefail

pod=${POD_E2E_POD:?POD_E2E_POD is required}
trace_dir=${POD_E2E_TRACE_DIR:?POD_E2E_TRACE_DIR is required}
herdr=${HERDR_BIN_PATH:-herdr}
pane=${HERDR_PANE_ID:?HERDR_PANE_ID is required}
seq=$(date +%s)

report() {
  local agent=$1
  local state=$2
  seq=$((seq + 1))
  "$herdr" pane report-agent "$pane" --source pod-e2e --agent "$agent" --state "$state" --seq "$seq" >/dev/null
}

finish() {
  "$herdr" pane release-agent "$pane" --source pod-e2e --agent pi --seq "$((seq + 1))" >/dev/null 2>&1 || true
}
trap finish EXIT

report pi idle
printf '> '
IFS= read -r first
pointer='^The full worker brief is stored at (.+)\. Read that file, then carry out the task\.$'
if [[ $first =~ $pointer ]]; then
  # herdr's `agent.prompt` typed the one-line pointer and Enter together;
  # like Claude and Codex, the turn starts at once, which is what herdr's
  # prompt wait looks for.
  report pi working
  preamble=$(cat "${BASH_REMATCH[1]}")
  # A real agent spends seconds reading the brief before its first command;
  # pod marks the Dispatch `dispatched` only after herdr's prompt returns.
  sleep 1
else
  # The safe-input fallback pastes the brief and confirms it before Enter.
  preamble=$first
  # macOS ships Bash 3.2, whose `read -t` accepts whole seconds only.
  while IFS= read -r -t 1 line; do
    preamble+=$'\n'
    preamble+=$line
    if [[ $line == POD_E2E_MODE=* ]]; then
      # Mirror the size-matched collapsed-paste proof used by Claude and Codex.
      # The trailing newline is part of render_preamble's submitted text.
      preamble+=$'\n'
      printf '\033[2J\033[H> [Pasted Content %s chars]' "${#preamble}"
      break
    fi
  done
  # Safe input sends this Enter only after it confirms the summary above.
  IFS= read -r _submitted
  # Like Claude and Codex under herdr: working during the turn, idle once
  # back at the prompt, so herdr's idle event follows the screen it ends on.
  report pi working
fi

task=$(sed -n 's/^Your task ID is: //p' <<<"$preamble" | head -n 1)
dispatch=$(sed -n 's/^Your dispatch ID is: //p' <<<"$preamble" | head -n 1)
worker=$(sed -n 's/^Your worker handle is: //p' <<<"$preamble" | head -n 1)
mode=$(sed -n 's/^POD_E2E_MODE=//p' <<<"$preamble" | tail -n 1)
mkdir -p "$trace_dir"
printf '%s\n' "$preamble" >"$trace_dir/$dispatch.preamble"

done_message() {
  "$pod" send --from "$worker" --type worker_done --subject "fake agent done" \
    --body "The fake agent completed its scripted flow. The orchestration state matched the scenario. Nothing is left." \
    --task-id "$task" --dispatch-id "$dispatch" --outcome succeeded --json \
    >"$trace_dir/$dispatch.done.json"
  printf '> '
  report pi idle
  while sleep 60; do :; done
}

case "$mode" in
  happy)
    "$pod" ask --from "$worker" --question "Proceed with the e2e task?" --options "approved,rejected" \
      --timeout-ms 30000 --json >"$trace_dir/$dispatch.ask.json"
    "$pod" send --from "$worker" --type heartbeat --subject alive --task-id "$task" \
      --dispatch-id "$dispatch" --phase implementing --json >"$trace_dir/$dispatch.heartbeat.json"
    "$pod" check --terminal "$worker" --json >"$trace_dir/$dispatch.check.json"
    done_message
    ;;
  nudge)
    printf '> '
    report pi idle
    IFS= read -r nudge
    report pi working
    printf '%s\n' "$nudge" >"$trace_dir/$dispatch.nudge"
    # Spec 8.2 and A3: the nudge names the worker's handle and its brief.
    # shellcheck disable=SC2016 # the backticks are literal text
    wanted='^You have [0-9]+ orchestration message\(s\)\. Run `pod check --terminal [^`]+`\. Your brief: .+\.$'
    if [[ ! "$nudge" =~ $wanted ]]; then
      exit 64
    fi
    "$pod" check --terminal "$worker" --json >"$trace_dir/$dispatch.check.json"
    done_message
    ;;
  specialist)
    specialist_dispatch=$(sed -n '/^=== SPECIALISTS ===$/,/^===/s/^\(ctx_[0-9a-f]*\) .*/\1/p' <<<"$preamble" | head -n 1)
    "$pod" ask --from "$worker" --question "Which specialist answer wins?" --options "coordinator,specialist" \
      --to "dispatch:$specialist_dispatch" --timeout-ms 30000 --json >"$trace_dir/$dispatch.ask.json"
    done_message
    ;;
  hold)
    printf '> '
    while sleep 60; do :; done
    ;;
  *)
    printf 'unknown POD_E2E mode: %s\n' "$mode" >&2
    exit 64
    ;;
esac
