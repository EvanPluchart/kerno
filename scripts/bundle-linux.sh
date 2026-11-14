#!/usr/bin/env bash
# Builds the Linux packages into dist/, for arm64 (native) and x86_64 (cross-compiled, never emulated):
#   kerno-<version>-linux-<aarch64|x86_64>.tar.gz   an install prefix; the in-app updater downloads it,
#                                                   so the name and the one top folder go with
#                                                   artifact() in crates/kerno-core/src/update.rs
#   kerno_<version>_<arm64|amd64>.deb               files under /usr, Depends from dpkg-shlibdeps
#   Kerno-<version>-<aarch64|x86_64>.AppImage       AppDir + squashfs + the AppImage type-2 runtime
#
#   scripts/bundle-linux.sh          build the image if needed, then both architectures
#   scripts/bundle-linux.sh --clean  remove the image kerno-linux-build and the volume kerno-linux-cache
#
# It runs itself in the image of packaging/linux/Dockerfile (Ubuntu 20.04 arm64, so the packages need
# glibc 2.31 or newer; docs/DECISIONS.md D-066): the source is mounted read-only, dist/ writable, and
# the cargo registry and target/ live in the volume kerno-linux-cache, so a second run only rebuilds
# what changed. The image is arm64: on an x86_64 host Docker would emulate it.
# KERNO_JOBS (default 3) and KERNO_NICE (default 10) set cargo's jobs and the niceness.
# scripts/check-linux-packages.sh starts every package in clean containers.
set -euo pipefail
cd "$(dirname "$0")/.."

jobs=${KERNO_JOBS:-3}
niceness=${KERNO_NICE:-10}
version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)

if [ -z "${KERNO_LINUX_BUILD_IMAGE:-}" ]; then
  if [ "${1:-}" = --clean ]; then
    docker image rm kerno-linux-build || true
    docker volume rm kerno-linux-cache || true
    exit 0
  fi
  toolchain=$(sed -n 's/^channel = "\(.*\)"/\1/p' rust-toolchain.toml)
  nice -n "$niceness" docker build --platform linux/arm64 -t kerno-linux-build \
    --build-arg RUST_TOOLCHAIN="$toolchain" packaging/linux
  mkdir -p dist
  exec docker run --rm --platform linux/arm64 --name kerno-linux-build \
    -v "$PWD:/src:ro" -v "$PWD/dist:/src/dist" -v kerno-linux-cache:/cache \
    -e CARGO_HOME=/cache/cargo -e CARGO_TARGET_DIR=/cache/target \
    -e KERNO_JOBS="$jobs" -e KERNO_NICE="$niceness" \
    kerno-linux-build scripts/bundle-linux.sh
fi

target_dir=${CARGO_TARGET_DIR:-target}
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

# The install prefix every package carries: bin/, share/applications, share/icons, share/doc.
prefix=$stage/prefix
mkdir -p "$prefix/bin" "$prefix/share/applications" "$prefix/share/doc/kerno"
cat > "$prefix/share/applications/kerno.desktop" <<DESKTOP
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
for icon in assets/icon/kerno-*.png; do
  size=${icon##*kerno-}
  size=${size%.png}
  mkdir -p "$prefix/share/icons/hicolor/${size}x${size}/apps"
  cp "$icon" "$prefix/share/icons/hicolor/${size}x${size}/apps/kerno.png"
done
mkdir -p "$prefix/share/icons/hicolor/scalable/apps"
cp assets/icon/kerno.svg "$prefix/share/icons/hicolor/scalable/apps/kerno.svg"
cp LICENSE-MIT LICENSE-APACHE THIRD_PARTY_NOTICES.md README.md "$prefix/share/doc/kerno/"

mkdir -p dist
for target in aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu; do
  arch=${target%%-*}
  case $arch in aarch64) deb_arch=arm64 ;; x86_64) deb_arch=amd64 ;; esac
  started=$(date +%s)
  nice -n "$niceness" cargo build --release --locked -p kerno --target "$target" -j "$jobs"
  echo "$target: built in $(($(date +%s) - started)) s"
  binary=$target_dir/$target/release/kerno
  cp "$binary" "$prefix/bin/kerno"

  # Tarball: one top folder, plain files and folders only (the updater refuses links).
  name=kerno-$version-linux-$arch
  rm -rf "${stage:?}/$name"
  cp -R "$prefix" "$stage/$name"
  tar -czf "dist/$name.tar.gz" -C "$stage" --owner=0 --group=0 "$name"

  # .deb: the prefix under /usr; Depends from the libraries the binary links, plus the two GPUI
  # opens at run time (Vulkan and the Wayland client), which dpkg-shlibdeps cannot see.
  deb=$stage/deb-$deb_arch
  rm -rf "$deb"
  mkdir -p "$deb/DEBIAN" "$deb/usr"
  cp -R "$prefix/." "$deb/usr/"
  mkdir -p "$stage/shlibs/debian"
  touch "$stage/shlibs/debian/control"
  depends=$(cd "$stage/shlibs" && DEB_HOST_ARCH=$deb_arch dpkg-shlibdeps -O "$deb/usr/bin/kerno" \
    | sed -n 's/^shlibs:Depends=//p')
  [ -n "$depends" ] || { echo "dpkg-shlibdeps found no dependency for $deb_arch" >&2; exit 1; }
  sed -e "s/@VERSION@/$version/" -e "s/@ARCH@/$deb_arch/" \
    -e "s/@SIZE@/$(du -sk "$deb/usr" | cut -f1)/" -e "s/@DEPENDS@/$depends, libvulkan1, libwayland-client0/" \
    packaging/linux/control.in > "$deb/DEBIAN/control"
  dpkg-deb --root-owner-group --build "$deb" "dist/kerno_${version}_$deb_arch.deb" >/dev/null

  # AppImage: AppRun, the desktop entry and the icon at the AppDir's root, the prefix under usr/.
  appdir=$stage/AppDir-$arch
  rm -rf "$appdir"
  mkdir -p "$appdir/usr"
  cp -R "$prefix/." "$appdir/usr/"
  cp "$prefix/share/applications/kerno.desktop" "$appdir/kerno.desktop"
  cp assets/icon/kerno-256.png "$appdir/kerno.png"
  cp assets/icon/kerno-256.png "$appdir/.DirIcon"
  cat > "$appdir/AppRun" <<'APPRUN'
#!/bin/sh
here=$(dirname "$(readlink -f "$0")")
exec "$here/usr/bin/kerno" "$@"
APPRUN
  chmod 755 "$appdir/AppRun"
  image=Kerno-$version-$arch.AppImage
  rm -f "$stage/squashfs"
  mksquashfs "$appdir" "$stage/squashfs" -all-root -noappend -comp gzip -no-progress >/dev/null
  cat "/opt/appimage/runtime-$arch" "$stage/squashfs" > "dist/$image"
  chmod 755 "dist/$image"
done

ls -la dist/kerno-"$version"-linux-*.tar.gz dist/kerno_"$version"_*.deb dist/Kerno-"$version"-*.AppImage
