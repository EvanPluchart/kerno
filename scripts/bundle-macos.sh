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
binary=target/release/kerno
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

# The disk image: the app and a link to /Applications to drag it onto.
stage=$(mktemp -d -t kerno-dmg)
cp -R "$app" "$stage/"
ln -s /Applications "$stage/Applications"
rm -f "$dmg"
hdiutil create -quiet -volname Kerno -srcfolder "$stage" -format UDZO "$dmg"
rm -rf "$stage"
echo "disk image: $dmg ($(du -h "$dmg" | cut -f1))"
