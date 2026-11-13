#!/usr/bin/env bash
# Builds dist/kerno-<version>-linux-<arch>.tar.gz: the release binary, a
# .desktop file, the icons in the hicolor layout, the licences and README,
# laid out as an install prefix:
#   tar -xzf kerno-<version>-linux-<arch>.tar.gz -C ~/.local --strip-components=1
#
# On a Linux host (the packages of scripts/install-linux-deps.sh installed):
#   scripts/bundle-linux.sh
# From macOS the same command runs itself in the Linux build image
# (Dockerfile.linux-build), so <arch> is the Docker host's. It runs:
#   nice -n 10 docker build -f Dockerfile.linux-build -t kerno-linux-build .
#   docker run --rm --name kerno-linux-build -v "$PWD/dist:/src/dist" \
#     kerno-linux-build scripts/bundle-linux.sh
# ponytail: the image rebuild is cached unless crates/ changed; then it is
# a full release build (no incremental cache kept between images).
#
# No AppImage: no AppImage tool is installed here (see docs/ROADMAP.md).
set -euo pipefail
cd "$(dirname "$0")/.."

if [ "$(uname -s)" = Darwin ]; then
  nice -n 10 docker build -f Dockerfile.linux-build -t kerno-linux-build .
  mkdir -p dist
  exec docker run --rm --name kerno-linux-build -v "$PWD/dist:/src/dist" \
    kerno-linux-build scripts/bundle-linux.sh
fi

version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
name=kerno-$version-linux-$(uname -m)

nice -n 10 cargo build --release -p kerno

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
root=$stage/$name
mkdir -p "$root/bin" "$root/share/applications" "$root/share/doc/kerno"
cp target/release/kerno "$root/bin/kerno"
cat > "$root/share/applications/kerno.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Kerno
GenericName=Database Client
Comment=A fast, native database client for PostgreSQL and MySQL
Exec=kerno
Icon=kerno
StartupWMClass=kerno
Terminal=false
Categories=Development;Database;
Keywords=database;sql;postgresql;mysql;mariadb;
DESKTOP

icons=0
for icon in assets/icon/kerno-*.png; do
  [ -f "$icon" ] || continue
  size=${icon##*kerno-}
  size=${size%.png}
  mkdir -p "$root/share/icons/hicolor/${size}x${size}/apps"
  cp "$icon" "$root/share/icons/hicolor/${size}x${size}/apps/kerno.png"
  icons=$((icons + 1))
done
[ "$icons" -gt 0 ] || echo "assets/icon/kerno-<size>.png not found: the tarball has no icon"

for doc in LICENSE-MIT LICENSE-APACHE THIRD_PARTY_NOTICES.md README.md; do
  if [ -f "$doc" ]; then cp "$doc" "$root/share/doc/kerno/"; else echo "$doc not found: left out"; fi
done

mkdir -p dist
tar -czf "dist/$name.tar.gz" -C "$stage" "$name"
echo "tarball: dist/$name.tar.gz ($(du -h "dist/$name.tar.gz" | cut -f1))"
tar -tzf "dist/$name.tar.gz"
