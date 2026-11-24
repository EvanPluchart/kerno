#!/usr/bin/env bash
# Writes packaging/macos/dmg-window.dsstore, the Finder layout of Kerno's disk image (D-142):
#   packaging/macos/make-dmg-layout.sh
# Finder writes it: a scratch image holding the same names as the release one (Kerno.app,
# Applications, .background/background.tiff) is mounted, dmg-layout.applescript arranges its
# window, and the .DS_Store Finder leaves on the volume is kept. scripts/bundle-macos.sh copies
# it into every image, so a build needs no Finder. Run it again after changing the window, the
# icons' places or the background's file name, on a Mac where this shell may control Finder
# (the release runner does: the probe workflow of K73 made the committed file there), with no
# other volume named Kerno mounted: the background is found through the volume's name.
set -euo pipefail
cd "$(dirname "$0")"

out=$PWD/dmg-window.dsstore
if [ -e /Volumes/Kerno ]; then
  echo "dmg layout: a volume named Kerno is mounted already; eject it first" >&2
  exit 1
fi
work=$(mktemp -d -t kerno-dmg-layout)
dev=
trap '[ -n "$dev" ] && hdiutil detach -quiet -force "$dev"; rm -rf "$work"' EXIT

mkdir -p "$work/stage/Kerno.app/Contents" "$work/stage/.background"
ln -s /Applications "$work/stage/Applications"
tiffutil -cathidpicheck dmg-background.png dmg-background@2x.png -out "$work/stage/.background/background.tiff"
hdiutil create -quiet -volname Kerno -srcfolder "$work/stage" -format UDRW "$work/layout.dmg"
# Browsable (no -nobrowse): Finder has to see the volume to arrange it.
attached=$(hdiutil attach -noautoopen "$work/layout.dmg")
dev=$(echo "$attached" | head -1 | cut -f1 | tr -d ' ')
mnt=$(echo "$attached" | grep /Volumes/ | cut -f3)
echo "dmg layout: $mnt on $dev"

# Finder may not answer (no session, no permission to control it): two minutes, then a clear
# failure instead of a wait.
osascript dmg-layout.applescript "$mnt" &
osa=$!
(sleep 120 && kill "$osa" 2>/dev/null) &
dog=$!
if ! wait "$osa"; then
  echo "dmg layout: Finder did not arrange the window (no answer within 2 minutes, or refused)" >&2
  exit 1
fi
kill "$dog" 2>/dev/null || true

for _ in $(seq 30); do [ -s "$mnt/.DS_Store" ] && break; sleep 1; done
[ -s "$mnt/.DS_Store" ] || { echo "dmg layout: Finder wrote no .DS_Store" >&2; exit 1; }
sleep 2
cp "$mnt/.DS_Store" "$out"
echo "dmg layout: $out ($(wc -c < "$out" | tr -d ' ') bytes)"
