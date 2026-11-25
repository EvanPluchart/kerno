#!/usr/bin/env bash
# Builds the Windows packages into dist/, on Windows (Git Bash, as GitHub's Windows runners have it):
#   kerno-<version>-windows-x86_64.zip         one top folder of that name: kerno.exe (its icon and
#                                              the C runtime inside), the licences, the notices,
#                                              the README
#   Kerno-<version>-windows-x86_64-setup.exe   the installer (packaging/windows/kerno.iss, Inno
#                                              Setup 6): the same files, for this user only
#   scripts/bundle-windows.sh
# Before packing: `kerno.exe --version` must print the workspace version, the executable must not be
# a test build (the test-update-without-click feature), and the app is launched once on a scratch
# data directory and an in-memory secret store: it must still run after it wrote its data, a
# picture of the screen is kept in dist/kerno-windows-launch.png, then it is stopped.
# After packing, the install check (docs/DECISIONS.md D-139), on GitHub's runners or when asked
# for by name (KERNO_INSTALL_CHECK=1), never by surprise on someone's own Windows: the installer
# is run silently for this user, its files, Start menu shortcut and uninstall entry are checked,
# the installed kerno.exe is launched once (dist/kerno-windows-installed.png), then the
# uninstaller runs silently: the folder, the shortcut and the entry must be gone, Kerno's data
# folder still there. It refuses to run where it could touch the user's own Kerno
# (scripts/windows-install-guard.sh: KERNO_DATA_DIR set, Kerno's data, local data or install
# folder, its uninstall entry or a failed read of it, a running kerno.exe), and on a failure it
# uninstalls only what it installed. KERNO_SKIP_LAUNCH_CHECK=1 skips both launches and the install
# check (no desktop). The names go with what the release workflow lists in SHA256SUMS and with
# kerno_core::update::artifact.
set -euo pipefail
cd "$(dirname "$0")/.."

version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
arch=$(rustc -vV | sed -n 's/^host: \([^-]*\)-.*/\1/p')
name=kerno-$version-windows-$arch
setup=Kerno-$version-windows-$arch-setup

cargo build --release --locked -p kerno
exe=target/release/kerno.exe
printed=$("$exe" --version)
# A test build says so in its version (crates/kerno/src/update.rs, VERSION_LINE): never packaged.
case $printed in
  *+test-update-without-click*)
    echo "$exe is a test build ($printed): never packaged" >&2
    exit 1
    ;;
esac
if [ "$printed" != "kerno $version" ]; then
  echo "kerno.exe --version printed \"$printed\", not \"kerno $version\"" >&2
  exit 1
fi
# And the feature's own log line (crates/kerno/src/workspace/updates.rs), as a second check.
if grep -aq "test-update-without-click" "$exe"; then
  echo "$exe is a test build (test-update-without-click): never packaged" >&2
  exit 1
fi

pids=()
temps=()
installed_here=
cleanup() {
  for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null || true; done
  # Only what this run installed (the guard saw no Kerno before it).
  if [ -n "$installed_here" ] && [ -f "$app/unins000.exe" ]; then
    MSYS_NO_PATHCONV=1 "$app/unins000.exe" /VERYSILENT /SUPPRESSMSGBOXES /NORESTART || true
  fi
  rm -rf "${temps[@]}" dist/.stage-windows
}
trap cleanup EXIT

# Starts $1 with the environment in "${@:2}", waits until folder $data holds something and the app
# still runs 8 s later, keeps a picture of the screen as $picture, stops it.
launch_once() {
  local program=$1 pid ready=
  shift
  env "$@" "$program" &
  pid=$!
  pids+=("$pid")
  for _ in $(seq 40); do
    sleep 1
    if ! kill -0 "$pid" 2>/dev/null; then
      echo "launch check: $program stopped by itself; what it left in $data:" >&2
      find "$data" -type f | head -20 >&2 || true
      find "$data" -name '*.log*' -exec tail -n 40 {} + >&2 || true
      exit 1
    fi
    if [ -n "$(ls -A "$data" 2>/dev/null)" ]; then ready=1; break; fi
  done
  [ -n "$ready" ] || { echo "launch check: no data written in $data after 40 s" >&2; exit 1; }
  # Time for the first frames, then the picture.
  sleep 8
  kill -0 "$pid" 2>/dev/null || { echo "launch check: $program stopped after its first frames" >&2; exit 1; }
  powershell -NoProfile -ExecutionPolicy Bypass -File scripts/windows-screenshot.ps1 "$picture"
  echo "launch check: $program running, data written: $(ls "$data" | tr '\n' ' ')"
  kill "$pid"
  wait "$pid" 2>/dev/null || true
}

mkdir -p dist
if [ "${KERNO_SKIP_LAUNCH_CHECK:-}" != 1 ]; then
  data=$(mktemp -d)
  temps+=("$data")
  picture=dist/kerno-windows-launch.png
  launch_once "$exe" KERNO_DATA_DIR="$(cygpath -w "$data")" KERNO_SECRET_STORE=memory KERNO_NO_UPDATE_CHECK=1
fi

# Staged under dist/, so that 7z is given relative paths only.
stage=dist/.stage-windows
rm -rf "$stage" "dist/$name.zip" "dist/$setup.exe"
mkdir -p "$stage/$name"
cp "$exe" LICENSE-MIT LICENSE-APACHE THIRD_PARTY_NOTICES.md README.md "$stage/$name/"
(cd "$stage" && 7z a -tzip -bso0 -bsp0 "../$name.zip" "$name")
7z l "dist/$name.zip" | tail -n 12
ls -la "dist/$name.zip"

# The installer, from the same files; its licence page shows both licences.
{
  printf 'Kerno is licensed under either of the two licences below, at your option:\r\n'
  printf 'the MIT licence, or the Apache License, Version 2.0.\r\n\r\n'
  sed 's/$/\r/' LICENSE-MIT
  printf '\r\n\r\n'
  sed 's/$/\r/' LICENSE-APACHE
} > "$stage/LICENSE.txt"
iscc="/c/Program Files (x86)/Inno Setup 6/ISCC.exe"
[ -x "$iscc" ] || iscc=$(command -v iscc || command -v ISCC.exe || true)
[ -n "$iscc" ] || { echo "Inno Setup 6 (ISCC.exe) was not found" >&2; exit 1; }
cp "$stage/LICENSE.txt" "$stage/$name/"
MSYS_NO_PATHCONV=1 "$iscc" /Q "/DAppVersion=$version" "/DSource=$(cygpath -aw "$stage/$name")" \
  "/DOutputDir=$(cygpath -aw dist)" "$(cygpath -aw packaging/windows/kerno.iss)"
ls -la "dist/$setup.exe"

[ "${KERNO_SKIP_LAUNCH_CHECK:-}" != 1 ] || exit 0
if [ "${GITHUB_ACTIONS:-}" != true ] && [ "${KERNO_INSTALL_CHECK:-}" != 1 ]; then
  echo "install check: skipped (it installs and uninstalls Kerno for this user: GitHub's runners, or KERNO_INSTALL_CHECK=1)"
  exit 0
fi

# The install check.
. scripts/windows-install-guard.sh
registry() { # the value $1 of the uninstall entry, or nothing
  powershell -NoProfile -Command "(Get-ItemProperty -ErrorAction SilentlyContinue '$uninstall_key').$1" | tr -d '\r'
}
app=$(cygpath -u "$LOCALAPPDATA")/Programs/Kerno
start_menu=$(cygpath -u "$APPDATA")/Microsoft/Windows/"Start Menu"/Programs/Kerno.lnk
desktop=$(cygpath -u "$USERPROFILE")/Desktop/Kerno.lnk
user_data=$(cygpath -u "$APPDATA")/Kerno
why=$(install_check_refusal)
if [ -n "$why" ]; then
  echo "install check: not run: $why" >&2
  exit 1
fi
fail() { echo "install check: $*" >&2; exit 1; }
installed_here=1
MSYS_NO_PATHCONV=1 "dist/$setup.exe" /VERYSILENT /SUPPRESSMSGBOXES /NORESTART \
  "/LOG=$(cygpath -aw dist/kerno-windows-install.log)" || fail "the installer failed ($?)"
for file in kerno.exe LICENSE-MIT LICENSE-APACHE THIRD_PARTY_NOTICES.md README.md unins000.exe; do
  [ -f "$app/$file" ] || fail "$app/$file is missing"
done
cmp -s "$exe" "$app/kerno.exe" || fail "the installed kerno.exe is not the one built"
[ -f "$start_menu" ] || fail "no Start menu shortcut at $start_menu"
target=$(powershell -NoProfile -Command "(New-Object -ComObject WScript.Shell).CreateShortcut('$(cygpath -w "$start_menu")').TargetPath" | tr -d '\r')
[ "$(cygpath -u "$target")" = "$app/kerno.exe" ] || fail "the shortcut starts $target"
[ ! -e "$desktop" ] || fail "a desktop shortcut was made, though it is off by default"
for value in DisplayName DisplayVersion Publisher InstallLocation UninstallString; do
  echo "install check: uninstall entry $value = $(registry "$value")"
done
[ "$(registry DisplayVersion)" = "$version" ] || fail "the uninstall entry says version $(registry DisplayVersion)"
[ "$(registry Publisher)" = "Evan Pluchart" ] || fail "the uninstall entry's publisher is $(registry Publisher)"
echo "install check: installed in $app, Start menu shortcut to kerno.exe, uninstall entry $version"

# Launched from where it was installed, on the user's own data folder (%APPDATA%\Kerno).
data=$user_data
picture=dist/kerno-windows-installed.png
launch_once "$app/kerno.exe" KERNO_SECRET_STORE=memory KERNO_NO_UPDATE_CHECK=1

uninstaller=$(registry UninstallString)
MSYS_NO_PATHCONV=1 "$app/unins000.exe" /VERYSILENT /SUPPRESSMSGBOXES /NORESTART || fail "the uninstaller failed ($?)"
# The uninstaller goes on from a copy of itself: wait until it is done.
for _ in $(seq 60); do
  [ -e "$app" ] || [ -e "$start_menu" ] || [ -n "$(registry DisplayVersion)" ] || break
  sleep 1
done
[ ! -e "$app" ] || fail "$app is still there after uninstalling: $(ls -A "$app" | tr '\n' ' ')"
[ ! -e "$start_menu" ] || fail "the Start menu shortcut is still there"
[ -z "$(registry DisplayVersion)" ] || fail "the uninstall entry is still there"
installed_here=
[ -n "$(ls -A "$user_data/data" 2>/dev/null)" ] || fail "Kerno's data folder $user_data/data is gone"
echo "install check: uninstalled ($uninstaller): folder, shortcut and entry gone; data kept: $(ls "$user_data/data" | tr '\n' ' ')"
# This check's own data, made by its launch.
rm -rf "$user_data"
