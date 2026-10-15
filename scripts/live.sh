#!/usr/bin/env bash
# Runs the live suite: real claude and codex workers on an isolated herdr
# session (tests/live.rs). It spends model tokens and needs both agents signed
# in. Extra arguments are test name filters; POD_LIVE_KINDS=claude (or codex)
# limits the kinds. Results are written to docs/reviews/live/<date>.md, or POD_LIVE_RESULTS.
set -euo pipefail

for program in herdr claude codex pod; do
  if ! command -v "$program" >/dev/null 2>&1; then
    printf '%s is required on PATH\n' "$program" >&2
    exit 1
  fi
done

# The suite starts its own herdr server; variables from the herdr pane this
# script runs in would point pod at the wrong session.
for name in $(env | grep '^HERDR_' | cut -d= -f1); do
  unset "$name"
done

cargo build --release --bin pod
POD_LIVE=1 cargo test --release --test live -- --ignored --test-threads=1 --nocapture "$@"

printf 'results: %s\n' "${POD_LIVE_RESULTS:-docs/reviews/live/$(date +%F).md}"
