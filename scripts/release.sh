#!/usr/bin/env bash
# Builds the release artifacts locally and prints the command that publishes them.
#   scripts/release.sh [version]     version defaults to the workspace version
# Steps: the tree must be clean and the version must equal the one in Cargo.toml;
# scripts/bundle-macos.sh (on macOS only) and scripts/bundle-linux.sh run; the
# artifacts are renamed to the names the update check looks for; dist/SHA256SUMS
# is written (file names only: `shasum -a 256 -c` and `sha256sum -c` read it);
# then the `gh release create` command is PRINTED for the owner to run.
# This script never runs gh, git tag or git push, and makes no network request
# (the Linux bundle on macOS builds in the local Docker daemon).
#
# Artifact names: Kerno-<version>-macos-<arch>.dmg, kerno-<version>-linux-<arch>.tar.gz,
# arch as `uname -m` names it (arm64 for the macOS dmg, the Docker host's for the tarball). They are defined by `artifact()`
# in crates/kerno-core/src/update.rs; the two go together, change both.
set -euo pipefail
cd "$(dirname "$0")/.."

version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
want=${1:-$version}
if [ "$want" != "$version" ]; then
  echo "version $want is not the workspace version $version in Cargo.toml" >&2
  exit 1
fi
if [ -n "$(git status --porcelain)" ]; then
  echo "the working tree is not clean: commit or stash first" >&2
  exit 1
fi

arch=$(uname -m)
files=()
if [ "$(uname -s)" = Darwin ]; then
  scripts/bundle-macos.sh
  mac=Kerno-$version-macos-$arch.dmg
  mv -f "dist/Kerno-$version.dmg" "dist/$mac"
  files+=("$mac")
fi
rm -f dist/kerno-"$version"-linux-*.tar.gz
scripts/bundle-linux.sh
# From macOS the tarball's <arch> is the Docker host's (aarch64 on Apple Silicon), not uname -m's.
for t in dist/kerno-"$version"-linux-*.tar.gz; do files+=("$(basename "$t")"); done

(
  cd dist
  if command -v shasum >/dev/null; then shasum -a 256 "${files[@]}"; else sha256sum "${files[@]}"; fi > SHA256SUMS
  echo "dist/SHA256SUMS:"
  cat SHA256SUMS
)

# The owner's notes when the file exists, GitHub's generated ones otherwise.
notes="--generate-notes"
[ -f "docs/release-notes/v$version.md" ] && notes="--notes-file docs/release-notes/v$version.md"
args=""
for f in "${files[@]}"; do args="$args dist/$f"; done
echo
echo "Run this yourself to publish (nothing was tagged, pushed or uploaded):"
echo "  gh release create v$version$args dist/SHA256SUMS --title \"Kerno $version\" $notes"
