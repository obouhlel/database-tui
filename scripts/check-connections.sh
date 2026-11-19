#!/usr/bin/env bash
# Tests every connection of config.toml (or the ones named) and prints one
# line per connection. Exit code 1 when one of them fails, so it can run
# from cron or CI:
#
#   scripts/check-connections.sh                 # all connections
#   scripts/check-connections.sh prod recette    # only these
#   scripts/check-connections.sh --config ~/autre.toml
#
# Uses pgtui from PATH, else builds it from this repository.
set -euo pipefail

config=()
names=()
while [ $# -gt 0 ]; do
  case "$1" in
    --config) config=(--config "$2"); shift 2 ;;
    --config=*) config=(--config "${1#--config=}"); shift ;;
    -h|--help) sed -n '2,10p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) names+=("$1"); shift ;;
  esac
done

if command -v pgtui >/dev/null 2>&1; then
  bin=pgtui
else
  root=$(cd "$(dirname "$0")/.." && pwd)
  bin="$root/target/release/pgtui"
  if [ ! -x "$bin" ]; then
    echo "pgtui introuvable dans le PATH : compilation de $root…" >&2
    cargo build --release --quiet --manifest-path "$root/Cargo.toml"
  fi
fi

exec "$bin" ${config[@]+"${config[@]}"} --check-connections ${names[@]+"${names[@]}"}
