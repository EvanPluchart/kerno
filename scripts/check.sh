#!/usr/bin/env bash
# Every check of the project, run locally (there is no CI), in this order; stops at the first failure
# and prints one summary line per step:
#   scripts/check.sh              fmt, clippy, deny, tests
#   scripts/check.sh --no-tests   fmt, clippy, deny
# Refuses to start when the 1-minute load average is above the number of cores.
# The tests need the kerno-* containers: they are started (scripts/test-env.sh
# up) only if they are not running, and stopped again only if this script
# started them. It never touches a container that is not named kerno-*.
set -euo pipefail
cd "$(dirname "$0")/.."

tests=1
case "${1:-}" in
  "") ;;
  --no-tests) tests=0 ;;
  *) echo "usage: scripts/check.sh [--no-tests]" >&2; exit 2 ;;
esac

cores=$(sysctl -n hw.ncpu 2>/dev/null || nproc)
load=$(uptime | sed 's/.*load averages\{0,1\}: *//' | tr -d ',' | awk '{print $1}')
if awk -v l="$load" -v c="$cores" 'BEGIN { exit !(l > c) }'; then
  echo "load average $load is above $cores cores: wait and retry" >&2
  exit 1
fi

step() { # step <label> <command...>
  local label=$1
  shift
  if "$@"; then
    echo "ok   $label"
  else
    echo "FAIL $label" >&2
    exit 1
  fi
}

step "cargo fmt --all --check" cargo fmt --all --check
step "cargo clippy" nice -n 10 cargo clippy --workspace --all-targets -- -D warnings
step "cargo deny check" nice -n 10 cargo deny check

if [ "$tests" = 1 ]; then
  started=0
  running=$(docker ps --filter name=kerno- --format '{{.Names}}' | grep -c '^kerno-' || true)
  if [ "$running" -lt 4 ]; then
    step "test containers up" scripts/test-env.sh up
    started=1
  fi
  stop() { [ "$started" = 1 ] && scripts/test-env.sh down >/dev/null 2>&1 || true; }
  trap stop EXIT
  step "cargo test (containers required)" env KERNO_REQUIRE_CONTAINERS=1 \
    nice -n 10 cargo test --workspace -- --test-threads=3
fi
echo "all checks passed"
