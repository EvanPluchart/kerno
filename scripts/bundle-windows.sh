#!/usr/bin/env bash
# Builds the Windows package into dist/, on Windows (Git Bash, as GitHub's Windows runners have it):
#   kerno-<version>-windows-x86_64.zip   one top folder of that name: kerno.exe (its icon and the
#                                        C runtime inside), the licences, the notices, the README
#   scripts/bundle-windows.sh
# Before packing: `kerno.exe --version` must print the workspace version, and the app is launched
# once on a scratch data directory and an in-memory secret store: it must still run after it wrote
# its data, a picture of the screen is kept in dist/kerno-windows-launch.png, then it is stopped.
# KERNO_SKIP_LAUNCH_CHECK=1 skips the launch (no desktop). The package is a preview
# (docs/DECISIONS.md D-131, D-133): no installer, and an update only opens the release page; the
# name goes with what the release workflow lists in SHA256SUMS.
set -euo pipefail
cd "$(dirname "$0")/.."

version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
arch=$(rustc -vV | sed -n 's/^host: \([^-]*\)-.*/\1/p')
name=kerno-$version-windows-$arch

cargo build --release --locked -p kerno
exe=target/release/kerno.exe
printed=$("$exe" --version)
if [ "$printed" != "kerno $version" ]; then
  echo "kerno.exe --version printed \"$printed\", not \"kerno $version\"" >&2
  exit 1
fi

mkdir -p dist
if [ "${KERNO_SKIP_LAUNCH_CHECK:-}" != 1 ]; then
  data=$(mktemp -d)
  pid=
  stop() { [ -n "$pid" ] && kill "$pid" 2>/dev/null; rm -rf "$data" "${stage:-}"; }
  trap stop EXIT
  KERNO_DATA_DIR=$(cygpath -w "$data") KERNO_SECRET_STORE=memory KERNO_NO_UPDATE_CHECK=1 "$exe" &
  pid=$!
  ready=
  for _ in $(seq 40); do
    sleep 1
    if ! kill -0 "$pid" 2>/dev/null; then
      echo "launch check: kerno.exe stopped by itself; what it left in its data directory:" >&2
      find "$data" -type f | head -20 >&2
      find "$data" -name '*.log*' -exec tail -n 40 {} + >&2 || true
      pid=
      exit 1
    fi
    if [ -n "$(ls -A "$data")" ]; then ready=1; break; fi
  done
  [ -n "$ready" ] || { echo "launch check: no data written after 40 s" >&2; exit 1; }
  # Time for the first frames, then the picture.
  sleep 8
  kill -0 "$pid" 2>/dev/null || { echo "launch check: kerno.exe stopped after its first frames" >&2; pid=; exit 1; }
  powershell -NoProfile -ExecutionPolicy Bypass -File scripts/windows-screenshot.ps1 dist/kerno-windows-launch.png
  echo "launch check: running, data written: $(ls "$data" | tr '\n' ' ')"
  kill "$pid"
  pid=
fi

# Staged under dist/, so that 7z is given relative paths only.
stage=dist/.stage-windows
rm -rf "$stage" "dist/$name.zip"
mkdir -p "$stage/$name"
trap 'rm -rf "$stage" "${data:-}"' EXIT
cp "$exe" LICENSE-MIT LICENSE-APACHE THIRD_PARTY_NOTICES.md README.md "$stage/$name/"
(cd "$stage" && 7z a -tzip -bso0 -bsp0 "../$name.zip" "$name")
7z l "dist/$name.zip" | tail -n 12
ls -la "dist/$name.zip"
