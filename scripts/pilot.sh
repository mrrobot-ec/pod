#!/usr/bin/env bash
set -euo pipefail

cat <<'EOF'
pod live pilot (spec 17 item 6): terra implements, sol reviews, and the
findings go back to terra in the same pane, coordinated from a Claude session.

Before you start:
  - pod is installed (sh scripts/install.sh, then herdr plugin link) and
    ~/.local/bin is on PATH; codex and claude have their herdr integrations.
  - Either ~/.config/pod/config.toml defines the presets (model is optional;
    add model = "<name>" only to pin a model codex accepts):
      [presets.terra]
      agent = "codex"
      [presets.sol]
      agent = "codex"
      effort = "xhigh"
    or you skip presets and write the launch inline in each start, for example
    "--agent codex --effort xhigh --fast" in place of "--agent sol" (--fast is
    Codex fast mode).
  - A disposable real repository with a small, bounded change in mind
    (one function and its test). Open a Claude session in a herdr pane at its
    root; that session is the coordinator and runs every command below.

Coordinator commands, in order (keep the IDs each one prints):

  pod doctor --json                      # no check may be "fail"
  pod run-create --objective "Pilot: terra implements, sol reviews, terra applies the findings" --json

  # 1. terra implements.
  pod worker-start --agent terra --task-title "Implement" --spec "Target: <file>. Change: <one bounded change>. Constraints: no new dependencies; do not commit. Ownership: <file> and its test only. Acceptance: <test command> passes." --json
  pod check --wait --types worker_done,escalation,question,decision_gate --timeout-ms 600000 --json
  #    Answer a question with: pod reply --id <message_id> --body "<answer>"
  #    then acknowledge every batch you handled:
  pod check --ack <delivery_id> --json
  #    Keep terra's pane: do not release its Dispatch yet.

  # 2. sol reviews in its own pane.
  pod worker-start --agent sol --task-title "Review" --spec "Target: the uncommitted change to <file>. Change: none; review only, edit nothing. Constraints: do not commit. Ownership: read-only. Acceptance: worker_done whose --body lists each finding with file and line, or says there are none." --json
  pod check --wait --types worker_done,escalation,question,decision_gate --timeout-ms 600000 --json
  pod check --ack <delivery_id> --json
  pod worker-release --dispatch <sol_ctx_id> --json

  # 3. The findings go back to terra, in the same pane (reuse).
  pod worker-start --terminal <terra_handle> --task-title "Apply review" --spec "Target: <file>. Change: apply these review findings: <paste sol's worker_done body>. Constraints: do not commit. Ownership: <file> and its test only. Acceptance: <test command> passes." --json
  pod check --wait --types worker_done,escalation,question,decision_gate --timeout-ms 600000 --json
  pod check --ack <delivery_id> --json
  pod worker-release --dispatch <terra_second_ctx_id> --json

  # 4. Account and inspect.
  pod worker-list --terminal-state reclaimable --json
  pod log --run <run_id> --json
  pod worker-show --dispatch <ctx_id> --json     # once for each of the three Dispatches
  pod board --run <run_id> --json
  #    Optional, when herdr-dagr is installed: dagr check --strict <boardFile>

Checklist after the pilot:
  [ ] worker-list --terminal-state reclaimable returns an empty workers array.
  [ ] log shows run.created, three task.created, and dispatch.dispatched
      followed by dispatch.completed for Implement, Review, and Apply review.
  [ ] worker-show: both terra Dispatches have the same assigneeTerminalId and
      assigneeHandle; terra's first Dispatch is terminalState "released"
      (reuse moved its pane to the second) and the other two are "released"
      after worker-release (release writes no log event; the worker row is
      the record).
  [ ] each worker sent worker_done exactly once with its dispatched IDs, and
      worker-show reports preambleInput "delivered" for the two fresh starts.
  [ ] if a nudge was needed, worker-show's wakeLadder shows a "delivered"
      attempt and the worker's consuming check cleared it.
  [ ] the boardFile that pod board printed exists; with dagr installed, it
      opened in a "board" tab and passes dagr check --strict.
  [ ] no pane outside the pilot's work was closed; the change is uncommitted.
EOF
