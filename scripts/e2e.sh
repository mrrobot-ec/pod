#!/usr/bin/env bash
set -euo pipefail

if ! command -v herdr >/dev/null 2>&1; then
  printf 'herdr 0.9.3 or newer is required on PATH\n' >&2
  exit 1
fi

version="$(herdr --version | awk '{print $NF}')"
if ! printf '%s\n' "$version" | awk -F. '{ exit !($1 > 0 || ($1 == 0 && $2 > 9) || ($1 == 0 && $2 == 9 && $3 >= 3)) }'; then
  printf 'herdr 0.9.3 or newer is required; found %s\n' "$(herdr --version)" >&2
  exit 1
fi

# The suite starts its own herdr server; variables from the herdr pane this
# script runs in would point pod at the live session and its store.
for name in $(env | grep '^HERDR_' | cut -d= -f1); do
  unset "$name"
done

cargo build --bin pod
POD_E2E=1 cargo test --test e2e -- --ignored --test-threads=1
