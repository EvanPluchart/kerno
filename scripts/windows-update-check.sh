#!/usr/bin/env bash
# The update installed from the app, end to end on GitHub's Windows runner, against a fake release
# served on the runner itself (docs/DECISIONS.md D-139). Run after scripts/bundle-windows.sh, by
# the K70 probe workflow only: it points github.com at this machine through the hosts file and
# trusts a certificate made here, which it refuses to do outside GitHub Actions. The workflow
# restores the hosts file from $RUNNER_TEMP/hosts.saved and removes the certificate in a step that
# always runs.
#
# Kerno's update code runs unchanged against https://github.com/EvanPluchart/kerno/releases. The
# packages are the ones scripts/bundle-windows.sh left in dist/ (the probe's first job). The
# Kerno that updates is a second build with the test-update-without-click feature, which differs
# from the shipped one in three things: it takes the update without the click on Update, it says
# it is 0.0.0, and it does not refuse to run the installer as administrator (the runner has no
# other kind of user). What it installs is the shipped installer of dist/, unchanged.
#   1. the shipped kerno.exe (the zip's folder), as administrator: the release page, no installer
#   2. a wrong line in SHA256SUMS: refused after the download, nothing installed or run
#   3. another Kerno running from the install folder: the installer stops before replacing
#      anything, the old Kerno is started again and says why
#   4. the update: the installer replaces 0.0.0 with this version and starts the new Kerno
set -euo pipefail
cd "$(dirname "$0")/.."
[ "${GITHUB_ACTIONS:-}" = true ] && [ -n "${RUNNER_TEMP:-}" ] \
  || { echo "this check changes the hosts file and the trusted roots: GitHub's runner only" >&2; exit 1; }

version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
arch=$(rustc -vV | sed -n 's/^host: \([^-]*\)-.*/\1/p')
setup=Kerno-$version-windows-$arch-setup.exe
uninstall_key='HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\{A1106071-CFCF-4417-8D07-D209FC6B030D}_is1'
app=$(cygpath -u "$LOCALAPPDATA")/Programs/Kerno
downloads=$(cygpath -u "$LOCALAPPDATA")/Kerno/data/updates
work=$(cygpath -u "$RUNNER_TEMP")/update-check
rm -rf "$work"
mkdir -p "$work"
fail() { echo "update check: $*" >&2; exit 1; }
say() { echo "update check: $*"; }
registry() {
  powershell -NoProfile -Command "(Get-ItemProperty -ErrorAction SilentlyContinue '$uninstall_key').$1" | tr -d '\r'
}
# The ids of the Kernos running from $1.
running_from() {
  powershell -NoProfile -Command "Get-Process kerno -ErrorAction SilentlyContinue | Where-Object { \$_.Path -eq '$(cygpath -w "$1")' } | ForEach-Object { \$_.Id }" | tr -d '\r'
}
stop_from() {
  powershell -NoProfile -Command "Get-Process kerno -ErrorAction SilentlyContinue | Where-Object { \$_.Path -eq '$(cygpath -w "$1")' } | Stop-Process -Force"
}
# Waits up to $1 s for the command "${@:2}" to succeed.
until_true() {
  local seconds=$1
  shift
  for _ in $(seq "$((seconds * 2))"); do
    "$@" && return 0
    sleep 0.5
  done
  return 1
}
# Conditions for until_true, read again at each try.
has_files() { [ -n "$(ls -A "$1" 2>/dev/null)" ]; }
kerno_runs() { [ -n "$(running_from "$1")" ]; }
no_kerno() { [ -z "$(running_from "$1")" ]; }
version_is() { [ "$(registry DisplayVersion)" = "$1" ]; }
logs_say() { grep -rqF -- "$2" "$1/logs" 2>/dev/null; }
picture() { powershell -NoProfile -ExecutionPolicy Bypass -File scripts/windows-screenshot.ps1 "$1"; }

# Everything is built before github.com is changed.
[ -f "dist/$setup" ] || fail "dist/$setup is missing: run scripts/bundle-windows.sh first"
# The shipped kerno.exe: the zip's.
7z x -bso0 -bsp0 "-o$(cygpath -w "$work/zip")" "$(cygpath -w "dist/kerno-$version-windows-$arch.zip")"
shipped_dir=$work/zip/kerno-$version-windows-$arch
cp "$shipped_dir/kerno.exe" "$work/kerno-shipped.exe"
grep -aq "test-update-without-click" "$work/kerno-shipped.exe" && fail "the shipped build has the test feature"
cargo build --release --locked -p kerno --features test-update-without-click
cp target/release/kerno.exe "$work/kerno-test.exe"
grep -aq "test-update-without-click" "$work/kerno-test.exe" || fail "the test build lacks its feature"
say "the shipped kerno.exe has no test-update-without-click; the test build has it"
# A folder of the zip's shape holding the test build.
mkdir -p "$work/test/kerno"
cp "$work/kerno-test.exe" "$work/test/kerno/kerno.exe"
cp LICENSE-MIT LICENSE-APACHE THIRD_PARTY_NOTICES.md README.md "$work/test/kerno/"
cp LICENSE-MIT "$work/test/kerno/LICENSE.txt"
# The old version to update from: the test build in an installer that says 0.0.0.
iscc="/c/Program Files (x86)/Inno Setup 6/ISCC.exe"
MSYS_NO_PATHCONV=1 "$iscc" /Q "/DAppVersion=0.0.0" "/DSource=$(cygpath -aw "$work/test/kerno")" \
  "/DOutputDir=$(cygpath -aw "$work")" "$(cygpath -aw packaging/windows/kerno.iss)"
old_setup=$work/Kerno-0.0.0-windows-$arch-setup.exe
# The fake release: the shipped installer and its checksum.
mkdir "$work/release"
cp "dist/$setup" "$work/release/"
(cd "$work/release" && sha256sum "$setup" > good.sums && cp good.sums SHA256SUMS)
echo "$(printf 'not the installer' | sha256sum | cut -d' ' -f1)  $setup" > "$work/release/bad.sums"
cat "$work/release/good.sums"

# github.com is this machine from here on.
MSYS_NO_PATHCONV=1 openssl req -x509 -newkey rsa:2048 -nodes -days 2 -subj "/CN=github.com" \
  -addext "subjectAltName=DNS:github.com" -addext "basicConstraints=critical,CA:TRUE" \
  -keyout "$(cygpath -w "$work/key.pem")" -out "$(cygpath -w "$work/cert.pem")" 2>/dev/null
cp /c/Windows/System32/drivers/etc/hosts "$RUNNER_TEMP/hosts.saved"
certutil -addstore -f Root "$(cygpath -w "$work/cert.pem")" > /dev/null
printf '\r\n127.0.0.1 github.com\r\n' >> /c/Windows/System32/drivers/etc/hosts
ipconfig //flushdns > /dev/null
pids=()
trap 'for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null || true; done; stop_from "$app/kerno.exe" || true' EXIT
served() { grep -csF -- "$1" "$work/server.log" || true; }
# Whether the fake release's log holds more than $2 lines with $1.
served_more() { [ "$(served "$1")" -gt "$2" ]; }
# Starts the fake release (its requests appended to server.log); stopped, github.com is offline.
start_server() {
  local before
  before=$(served "serving Kerno")
  python scripts/fake-release.py "$(cygpath -w "$work/release")" "$version" \
    "$(cygpath -w "$work/cert.pem")" "$(cygpath -w "$work/key.pem")" 2>> "$work/server.log" &
  server=$!
  pids+=("$server")
  until_true 10 served_more "serving Kerno" "${before:-0}" || fail "the fake release did not start"
}
start_server

# 1. The shipped build, as administrator: it checks, and an update would open the release page.
data=$work/data-1
KERNO_DATA_DIR=$(cygpath -w "$data") KERNO_SECRET_STORE=memory "$shipped_dir/kerno.exe" &
pids+=("$!")
until_true 40 logs_say "$data" "Kerno runs as administrator: an update opens the release page" \
  || fail "1: the shipped Kerno did not refuse the installer as administrator"
until_true 20 served_more "HEAD /EvanPluchart/kerno/releases/latest" 0 \
  || fail "1: the shipped Kerno did not check the (fake) release"
kill "${pids[-1]}"
say "1: the shipped Kerno, as administrator, checked and keeps to the release page"

# 2. A wrong checksum: downloaded, refused, nothing run or installed.
cp "$work/release/bad.sums" "$work/release/SHA256SUMS"
data=$work/data-2
KERNO_DATA_DIR=$(cygpath -w "$data") KERNO_SECRET_STORE=memory "$work/test/kerno/kerno.exe" &
pid=$!
pids+=("$pid")
until_true 60 logs_say "$data" "the download does not match its checksum" \
  || fail "2: no refusal in the log"
[ "$(served "GET /EvanPluchart/kerno/releases/download/v$version/$setup")" -ge 1 ] \
  || fail "2: the installer was not downloaded"
[ -z "$(registry DisplayVersion)" ] || fail "2: something was installed"
[ ! -e "$app" ] || fail "2: $app exists"
ls -d "$downloads"/kerno-update-* 2>/dev/null && fail "2: a download was kept"
kill -0 "$pid" 2>/dev/null || fail "2: Kerno quit after the refusal"
kill "$pid"
say "2: a wrong SHA256SUMS line: downloaded, refused, nothing kept, run or installed; Kerno still running"

# The old version, installed.
MSYS_NO_PATHCONV=1 "$old_setup" /VERYSILENT /SUPPRESSMSGBOXES /NORESTART || fail "the 0.0.0 installer failed"
[ "$(registry DisplayVersion)" = 0.0.0 ] || fail "0.0.0 is not installed: $(registry DisplayVersion)"
cmp -s "$work/kerno-test.exe" "$app/kerno.exe" || fail "the installed 0.0.0 is not the test build"
cp "$work/release/good.sums" "$work/release/SHA256SUMS"

# 3. Another Kerno holds the install folder's kerno.exe: nothing replaced, the old one says why.
KERNO_DATA_DIR=$(cygpath -w "$work/data-other") KERNO_SECRET_STORE=memory KERNO_NO_UPDATE_CHECK=1 "$app/kerno.exe" &
other=$!
pids+=("$other")
data=$work/data-3
until_true 30 has_files "$work/data-other" || fail "3: the other Kerno did not start"
before=$(served "/download/v$version/$setup")
KERNO_DATA_DIR=$(cygpath -w "$data") KERNO_SECRET_STORE=memory "$app/kerno.exe" &
pids+=("$!")
until_true 60 served_more "GET /EvanPluchart/kerno/releases/download/v$version/$setup" "$before" \
  || fail "3: the installer was not downloaded"
# The Kerno started again would take the update again: github.com goes offline, so that what it
# says on screen is the failed install.
kill "$server"
until_true 90 logs_say "$data" "its installer stopped before it finished" \
  || { cat "$data/update-setup.log" 2>/dev/null | tail -20; fail "3: the old Kerno did not say the update failed"; }
grep -F "Another Kerno is still running" "$data/update-setup.log" || fail "3: the installer's log does not say why"
[ "$(registry DisplayVersion)" = 0.0.0 ] || fail "3: the version changed to $(registry DisplayVersion)"
cmp -s "$work/kerno-test.exe" "$app/kerno.exe" || fail "3: kerno.exe was replaced"
[ ! -e "$data/update-pending.txt" ] || fail "3: the note is still there"
sleep 6
picture dist/kerno-windows-update-failed.png
[ -n "$(running_from "$app/kerno.exe")" ] || fail "3: no Kerno runs from $app"
stop_from "$app/kerno.exe"
kill "$other" 2>/dev/null || true
until_true 20 no_kerno "$app/kerno.exe" || fail "3: a Kerno still runs from $app"
say "3: another Kerno running: the installer replaced nothing, the old Kerno started again and said why"

# 4. The update.
start_server
data=$work/data-4
before=$(served "/download/v$version/$setup")
KERNO_DATA_DIR=$(cygpath -w "$data") KERNO_SECRET_STORE=memory "$app/kerno.exe" &
pid=$!
pids+=("$pid")
until_true 60 served_more "GET /EvanPluchart/kerno/releases/download/v$version/$setup" "$before" \
  || fail "4: the installer was not downloaded"
cp "$work/release/bad.sums" "$work/release/SHA256SUMS"
until_true 120 version_is "$version" \
  || { tail -30 "$data/update-setup.log" 2>/dev/null; fail "4: the uninstall entry says $(registry DisplayVersion)"; }
kill -0 "$pid" 2>/dev/null && fail "4: the Kerno that asked did not quit"
until_true 60 kerno_runs "$app/kerno.exe" || fail "4: the new Kerno was not started"
until_true 30 test ! -e "$data/update-pending.txt" || fail "4: the new Kerno did not read the note"
cmp -s "$work/kerno-shipped.exe" "$app/kerno.exe" || fail "4: the installed kerno.exe is not the shipped one"
[ "$("$app/kerno.exe" --version)" = "kerno $version" ] || fail "4: kerno.exe --version says $("$app/kerno.exe" --version)"
logs_say "$data" "was not installed" && fail "4: the new Kerno says the update failed"
ls -d "$downloads"/kerno-update-* 2>/dev/null && fail "4: the download was kept"
sleep 6
picture dist/kerno-windows-updated.png
stop_from "$app/kerno.exe"
say "4: 0.0.0 updated to $version by the shipped installer: uninstall entry $version, kerno.exe the shipped one, the new Kerno started, the note and the download removed"
cp "$data/update-setup.log" dist/kerno-windows-update-setup.log

# Tidy: uninstall.
until_true 20 no_kerno "$app/kerno.exe" || true
MSYS_NO_PATHCONV=1 "$app/unins000.exe" /VERYSILENT /SUPPRESSMSGBOXES /NORESTART
until_true 60 version_is "" || fail "the uninstall entry is still there"
echo "--- requests to the fake release"
cat "$work/server.log"
