#!/usr/bin/env bash
# Builds dist/Kerno.app and dist/Kerno-<version>.dmg.
#   scripts/bundle-macos.sh
# Signed with the identity in KERNO_SIGN_IDENTITY when it is set (a name or
# hash `security find-identity -v -p codesigning` lists), ad hoc otherwise.
# An ad-hoc signature changes with every build, so the keychain asks again
# for Kerno's saved passwords after each rebuild; a stable identity keeps
# "Always allow" across builds.
# Then launches the bundle once on a scratch data directory and an
# in-memory secret store, waits for its window and quits it.
# KERNO_SKIP_LAUNCH_CHECK=1 skips that check (no display).
set -euo pipefail
cd "$(dirname "$0")/.."

version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
app=dist/Kerno.app
dmg=dist/Kerno-$version.dmg

nice -n 10 cargo build --release -p kerno
binary=${CARGO_TARGET_DIR:-target}/release/kerno
# The oldest macOS the binary runs on, as the linker recorded it.
minos=$(otool -l "$binary" | awk '/LC_BUILD_VERSION/ {found = 1} found && $1 == "minos" {print $2; exit}')

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$binary" "$app/Contents/MacOS/kerno"
if [ -f assets/icon/kerno.icns ]; then
  cp assets/icon/kerno.icns "$app/Contents/Resources/kerno.icns"
else
  echo "assets/icon/kerno.icns not found: the bundle gets the generic app icon"
fi
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Kerno</string>
  <key>CFBundleDisplayName</key><string>Kerno</string>
  <key>CFBundleIdentifier</key><string>dev.kerno.Kerno</string>
  <key>CFBundleExecutable</key><string>kerno</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>CFBundleIconFile</key><string>kerno</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>LSMinimumSystemVersion</key><string>$minos</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSHumanReadableCopyright</key><string>MIT OR Apache-2.0</string>
</dict>
</plist>
PLIST
plutil -lint "$app/Contents/Info.plist"

if [ -n "${KERNO_SIGN_IDENTITY:-}" ]; then
  codesign --force --deep --options runtime --sign "$KERNO_SIGN_IDENTITY" "$app"
  echo "signed with the identity \"$KERNO_SIGN_IDENTITY\""
else
  codesign --force --deep --sign - "$app"
  echo "signed ad hoc (set KERNO_SIGN_IDENTITY to sign with a stable identity)"
fi
codesign --verify --deep --strict --verbose=2 "$app"
echo "bundle: $app ($(du -sh "$app" | cut -f1))"

if [ "${KERNO_SKIP_LAUNCH_CHECK:-}" != 1 ]; then
  # The bundle's own process is found by its path: the user's own Kerno may
  # be open, and is neither waited for nor touched.
  exe="$(cd "$app" && pwd -P)/Contents/MacOS/kerno"
  data=$(mktemp -d -t kerno-bundle-check)
  pid=
  trap '[ -n "$pid" ] && kill "$pid" 2>/dev/null; rm -rf "$data"' EXIT
  open -n --env KERNO_DATA_DIR="$data" --env KERNO_SECRET_STORE=memory "$app"
  window=
  for _ in $(seq 30); do
    pid=$(pgrep -f "^$exe" | head -1 || true)
    if [ -n "$pid" ] && window=$(swift scripts/window-id.swift kerno "$pid" 2>/dev/null); then break; fi
    sleep 1
  done
  if [ -z "$window" ]; then
    echo "launch check: no Kerno window after 30 s" >&2
    exit 1
  fi
  echo "launch check: pid $pid, window $window, binary $(ps -o comm= -p "$pid")"
  # Nothing is read from the source tree: fonts, theme and logos are compiled in.
  if lsof -p "$pid" 2>/dev/null | grep -E '/crates/|/assets/'; then
    echo "launch check: the app opened files from the source tree" >&2
    exit 1
  fi
  [ -n "$(ls -A "$data")" ] || { echo "launch check: the scratch data directory is empty" >&2; exit 1; }
  echo "launch check: data written to the scratch directory: $(ls "$data" | tr '\n' ' ')"
  kill "$pid"
  for _ in $(seq 10); do kill -0 "$pid" 2>/dev/null || break; sleep 1; done
  if kill -0 "$pid" 2>/dev/null; then
    echo "launch check: kerno did not quit" >&2
    exit 1
  fi
  pid=
  echo "launch check: quit cleanly"
fi

# The disk image: the app and a link to /Applications to drag it onto, in the window Finder opens
# on (D-142): the background picture, the icons' places and the window's size are Finder's
# .DS_Store, written once by packaging/macos/make-dmg-layout.sh and copied in here, so no Finder
# is needed; the volume's icon is Kerno's. Made read-write, completed while mounted out of
# Finder's sight, then compressed.
# hdiutil now and then fails on a busy machine ("Resource busy", seen on GitHub's runner): five
# tries for each of its steps, its own words shown at each failure.
hdi() {
  for try in 1 2 3 4 5; do
    if hdiutil "$@"; then return 0; fi
    echo "disk image: hdiutil $1 failed (try $try of 5)" >&2
    [ "$try" = 5 ] || sleep 5
  done
  return 1
}
work=$(mktemp -d -t kerno-dmg)
mnt=$work/mnt
trap '[ -n "${pid:-}" ] && kill "$pid" 2>/dev/null; [ -d "$mnt" ] && hdiutil detach -quiet -force "$mnt"; rm -rf "$work" "${data:-}"' EXIT
stage=$work/stage
mkdir -p "$stage/.background"
cp -R "$app" "$stage/"
ln -s /Applications "$stage/Applications"
tiffutil -cathidpicheck packaging/macos/dmg-background.png packaging/macos/dmg-background@2x.png \
  -out "$stage/.background/background.tiff"
cp assets/icon/kerno.icns "$stage/.VolumeIcon.icns"
hdi create -ov -volname Kerno -srcfolder "$stage" -format UDRW "$work/rw.dmg"
mkdir "$mnt"
hdi attach -nobrowse -noautoopen -mountpoint "$mnt" "$work/rw.dmg"
# hdiutil leaves out a .DS_Store of the source folder: it goes in on the mounted volume.
cp packaging/macos/dmg-window.dsstore "$mnt/.DS_Store"
# The volume shows .VolumeIcon.icns once its root carries the custom-icon Finder flag.
xattr -wx com.apple.FinderInfo 0000000000000000040000000000000000000000000000000000000000000000 "$mnt"
rm -rf "$mnt/.fseventsd"
hdi detach "$mnt"
rmdir "$mnt"
rm -f "$dmg"
hdi convert -ov "$work/rw.dmg" -format UDZO -o "$dmg"
hdiutil verify "$dmg"

# What ships is checked as a user mounts it: the layout, the background and the volume icon are
# there, and the app is the one signed and launched above.
fail() {
  echo "disk image: $1" >&2
  exit 1
}
mkdir "$mnt"
hdi attach -nobrowse -noautoopen -readonly -mountpoint "$mnt" "$dmg"
cmp -s packaging/macos/dmg-window.dsstore "$mnt/.DS_Store" || fail "the window layout is missing"
[ -s "$mnt/.background/background.tiff" ] || fail "the background is missing"
[ -s "$mnt/.VolumeIcon.icns" ] || fail "the volume icon is missing"
[ "$(xattr -px com.apple.FinderInfo "$mnt" | tr -d ' \n' | cut -c17-18)" = 04 ] \
  || fail "the volume icon is not switched on"
codesign --verify --deep --strict "$mnt/Kerno.app"
cmp -s "$app/Contents/MacOS/kerno" "$mnt/Kerno.app/Contents/MacOS/kerno" \
  || fail "the app inside is not the one built"
hdi detach "$mnt"
rm -rf "$work"
echo "disk image: $dmg ($(du -h "$dmg" | cut -f1)), opens on Kerno's window"
