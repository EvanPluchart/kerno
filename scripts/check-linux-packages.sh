#!/usr/bin/env bash
# Starts every Linux package of dist/ in clean containers and compares `kerno --version` with the
# workspace version; one line per package and distribution, exit 1 if any fails:
#   scripts/check-linux-packages.sh [aarch64] [x86_64]   both architectures when none is named
# Distributions: Ubuntu 20.04 (the glibc 2.31 floor, the build image's) and Debian 13 (current
# stable). On an arm64 host arm64 runs natively and x86_64 under emulation (docker --platform
# linux/amd64), allowed for this check only: the packages are built by scripts/bundle-linux.sh,
# never under emulation. The release workflow names one architecture per runner, each on a host of
# that architecture: an AppImage does not start under plain qemu-user (its runtime's ELF header
# carries the AppImage mark, which the kernel's qemu rule does not match).
#   .deb       apt-get install ./kerno_<v>_<arch>.deb (its Depends must resolve), then kerno --version
#   tarball    the .deb's Depends installed, unpacked, bin/kerno --version
#   AppImage   the .deb's Depends installed, --appimage-extract-and-run --version (no FUSE needed)
# The tarballs' listings are checked first with the updater's rules (one top folder named as
# artifact() names it, plain files and folders only). Containers are named kerno-check-* and
# removed; the base images this script pulled (by id, both platforms) are removed at the end.
set -euo pipefail
cd "$(dirname "$0")/.."

version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
want="kerno $version"
distros=(ubuntu:20.04 debian:13)
failed=0
archs=("$@")
[ ${#archs[@]} -gt 0 ] || archs=(aarch64 x86_64)
for arch in "${archs[@]}"; do
  case $arch in aarch64 | x86_64) ;; *) echo "usage: scripts/check-linux-packages.sh [aarch64] [x86_64]" >&2; exit 2 ;; esac
done

cleanup() { docker ps -aq --filter name=kerno-check- | xargs docker rm -f >/dev/null 2>&1 || true; }
trap cleanup EXIT

for arch in "${archs[@]}"; do
  top=kerno-$version-linux-$arch
  if tar -tzf "dist/$top.tar.gz" | grep -qv "^$top/" || tar -tvzf "dist/$top.tar.gz" | grep -q '^[^-d]'; then
    echo "FAIL tarball listing $top: an entry outside $top/ or a link"
    failed=1
  else
    echo "ok   tarball listing $top"
  fi
done

# Images are removed by id: a tag holds one platform, so pulling the other one leaves the first untagged.
before=$(docker images -aq --no-trunc)
pulled=()

for arch in "${archs[@]}"; do
  case $arch in aarch64) platform=linux/arm64 deb_arch=arm64 ;; x86_64) platform=linux/amd64 deb_arch=amd64 ;; esac
  deb=kerno_${version}_$deb_arch.deb
  for distro in "${distros[@]}"; do
    docker pull -q --platform "$platform" "$distro" >/dev/null
    id=$(docker image inspect --format '{{.Id}}' "$distro")
    grep -qx "$id" <<<"$before" || pulled+=("$id")
    install="apt-get update -qq && apt-get install -y -qq --no-install-recommends"
    # The .deb's Depends as package names: the first of each alternative, versions dropped.
    libs="\$(dpkg-deb -f /dist/$deb Depends | tr ',' '\\n' | sed 's/|.*//; s/(.*)//; s/ //g' | tr '\\n' ' ')"
    for kind in deb tarball AppImage; do
      case $kind in
        deb) run="$install /dist/$deb >/dev/null && kerno --version" ;;
        tarball) run="$install $libs >/dev/null && mkdir /opt/kerno \
          && tar -xzf /dist/kerno-$version-linux-$arch.tar.gz -C /opt/kerno --strip-components=1 \
          && /opt/kerno/bin/kerno --version" ;;
        AppImage) run="$install $libs >/dev/null \
          && /dist/Kerno-$version-$arch.AppImage --appimage-extract-and-run --version" ;;
      esac
      name=kerno-check-$kind-$arch-${distro//[:.]/-}
      got=$(docker run --rm --platform "$platform" --name "$name" -e DEBIAN_FRONTEND=noninteractive \
        -v "$PWD/dist:/dist:ro" "$distro" sh -ec "$run" 2>&1 | tail -1) || true
      if [ "$got" = "$want" ]; then
        printf 'ok   %-8s %-7s %-12s %s\n' "$kind" "$arch" "$distro" "$got"
      else
        printf 'FAIL %-8s %-7s %-12s %s\n' "$kind" "$arch" "$distro" "$got"
        failed=1
      fi
    done
  done
done

for id in ${pulled[@]+"${pulled[@]}"}; do docker image rm "$id" >/dev/null 2>&1 || true; done
[ "$failed" = 0 ] && echo "all Linux packages start" || { echo "some Linux packages failed" >&2; exit 1; }
