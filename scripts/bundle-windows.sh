#!/usr/bin/env bash
# Builds the Windows package into dist/, on Windows (Git Bash, as GitHub's Windows runners have it):
#   kerno-<version>-windows-x86_64.zip   one top folder of that name: kerno.exe (its icon and the
#                                        C runtime inside), the licences, the notices, the README
#   scripts/bundle-windows.sh
# Checks that `kerno.exe --version` prints the workspace version before packing. The package is a
# preview (docs/DECISIONS.md D-131, D-133): no installer, and an update only opens the release
# page; the name goes with what the release workflow lists in SHA256SUMS.
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

# Staged under dist/, so that 7z is given relative paths only.
stage=dist/.stage-windows
rm -rf "$stage" "dist/$name.zip"
mkdir -p "$stage/$name"
trap 'rm -rf "$stage"' EXIT
cp "$exe" LICENSE-MIT LICENSE-APACHE THIRD_PARTY_NOTICES.md README.md "$stage/$name/"
(cd "$stage" && 7z a -tzip -bso0 -bsp0 "../$name.zip" "$name")
7z l "dist/$name.zip" | tail -n 12
ls -la "dist/$name.zip"
