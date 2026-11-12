#!/usr/bin/env bash
# Runs Kerno, captures its window to a PNG, and quits it. For checks of the
# real app: uses a scratch data directory and the in-memory secret store, so
# the user's own Kerno data and keychain are never touched.
#
#   scripts/capture.sh <output.png> [light|dark] [seconds-to-wait]
#
# KERNO_BIN selects the binary (default: target/release/kerno, else debug).
set -euo pipefail
cd "$(dirname "$0")/.."

out="${1:?usage: scripts/capture.sh <output.png> [light|dark] [seconds]}"
appearance="${2:-light}"
wait_seconds="${3:-3}"

bin="${KERNO_BIN:-}"
if [ -z "$bin" ]; then
  if [ -x target/release/kerno ]; then bin=target/release/kerno; else bin=target/debug/kerno; fi
fi
if pgrep -x kerno >/dev/null; then
  echo "another Kerno is already running; quit it first" >&2
  exit 1
fi

data_dir="${KERNO_DATA_DIR:-$(mktemp -d)}"
helper="${TMPDIR:-/tmp}/kerno-window-id"
if [ ! -x "$helper" ] || [ scripts/window-id.swift -nt "$helper" ]; then
  swiftc -O -o "$helper" scripts/window-id.swift
fi

KERNO_DATA_DIR="$data_dir" KERNO_SECRET_STORE=memory KERNO_APPEARANCE="$appearance" \
  nice -n 10 "$bin" >"$data_dir/stdout.log" 2>&1 &
pid=$!
trap 'kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true' EXIT

# Wait for the window, then for the UI to settle.
for _ in $(seq 1 50); do
  if window_id="$("$helper" kerno 2>/dev/null)"; then break; fi
  if ! kill -0 "$pid" 2>/dev/null; then
    echo "Kerno exited before showing a window:" >&2
    cat "$data_dir/stdout.log" >&2
    exit 1
  fi
  perl -e 'select(undef, undef, undef, 0.2)'
done
perl -e "select(undef, undef, undef, $wait_seconds)"
mkdir -p "$(dirname "$out")"
screencapture -x -o -l "${window_id:?no Kerno window appeared}" "$out"
echo "captured $out (window $window_id, $appearance)"
