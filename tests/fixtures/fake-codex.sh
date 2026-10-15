#!/bin/sh
# A stand-in for `codex app-server`: answers initialize and account/rateLimits/read.
[ "$1" = "app-server" ] || { echo "unexpected args: $*" >&2; exit 2; }
FIXTURE="$(dirname "$0")/codex-rate-limits.json"
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*) printf '{"id":1,"result":{"userAgent":"fake","codexHome":"/tmp"}}\n' ;;
    *'"method":"account/rateLimits/read"'*) printf '{"id":2,"result":%s}\n' "$(cat "$FIXTURE")"; exit 0 ;;
  esac
done
