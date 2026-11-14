#!/usr/bin/env bash
# Builds the release artifacts locally and prints the command that publishes them.
#   scripts/release.sh [version]               version defaults to the workspace version
#   scripts/release.sh --linux-only [version]  the Linux packages alone, on a tree that may be dirty
#   scripts/release.sh --clean                 remove the Linux build image and its cache volume
# Steps: the tree must be clean (not with --linux-only) and the version must equal the one in
# Cargo.toml; scripts/bundle-macos.sh (on macOS, not with --linux-only) and scripts/bundle-linux.sh
# run, then scripts/check-linux-packages.sh starts every Linux package in clean containers;
# dist/SHA256SUMS is written (file names only: `shasum -a 256 -c` and `sha256sum -c` read it);
# then the `gh release create` command is PRINTED for the owner to run.
# This script never runs gh, git tag or git push. The Linux packages build in the local Docker
# daemon; the build image and the check containers download their base images and build tools.
#
# Artifact names: Kerno-<version>-macos-<arch>.dmg (arm64 on Apple Silicon),
# kerno-<version>-linux-<aarch64|x86_64>.tar.gz, kerno_<version>_<arm64|amd64>.deb,
# Kerno-<version>-<aarch64|x86_64>.AppImage. The dmg and tarball names are the ones `artifact()` in
# crates/kerno-core/src/update.rs looks for; the two go together, change both.
set -euo pipefail
cd "$(dirname "$0")/.."

linux_only=0
case "${1:-}" in
  --clean) exec scripts/bundle-linux.sh --clean ;;
  --linux-only) linux_only=1; shift ;;
esac
version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
want=${1:-$version}
if [ "$want" != "$version" ]; then
  echo "version $want is not the workspace version $version in Cargo.toml" >&2
  exit 1
fi
if [ "$linux_only" = 0 ] && [ -n "$(git status --porcelain)" ]; then
  echo "the working tree is not clean: commit or stash first" >&2
  exit 1
fi

arch=$(uname -m)
files=()
if [ "$linux_only" = 0 ] && [ "$(uname -s)" = Darwin ]; then
  scripts/bundle-macos.sh
  mac=Kerno-$version-macos-$arch.dmg
  mv -f "dist/Kerno-$version.dmg" "dist/$mac"
  files+=("$mac")
fi
rm -f dist/kerno-"$version"-linux-*.tar.gz dist/kerno_"$version"_*.deb dist/Kerno-"$version"-*.AppImage
scripts/bundle-linux.sh
scripts/check-linux-packages.sh
for arch in aarch64 x86_64; do
  deb_arch=$([ "$arch" = aarch64 ] && echo arm64 || echo amd64)
  files+=("kerno-$version-linux-$arch.tar.gz" "kerno_${version}_$deb_arch.deb" "Kerno-$version-$arch.AppImage")
done

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
[ "$linux_only" = 1 ] && echo "--linux-only: no macOS dmg here; run without --linux-only on a clean tree for a release."
echo "Run this yourself to publish (nothing was tagged, pushed or uploaded):"
echo "  gh release create v$version$args dist/SHA256SUMS --title \"Kerno $version\" $notes"
