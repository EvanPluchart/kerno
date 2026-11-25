# Sourced by scripts/bundle-windows.sh (and the K70 probe's dry test of it): whether the install
# check may install, launch and uninstall Kerno for this Windows user. It may not touch anything
# that is the user's (docs/DECISIONS.md D-139, review V2).
#   why=$(install_check_refusal)   # empty: it may run; else the reason, in one line

# packaging/windows/kerno.iss's AppId: Windows' uninstall entry is named after it.
uninstall_key='HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\{A1106071-CFCF-4417-8D07-D209FC6B030D}_is1'

install_check_refusal() {
  local appdata localappdata app entry running
  if [ -n "${KERNO_DATA_DIR:-}" ]; then
    echo "KERNO_DATA_DIR is set: the installed Kerno would open that data"
    return
  fi
  appdata=$(cygpath -u "${APPDATA:?}")/Kerno
  localappdata=$(cygpath -u "${LOCALAPPDATA:?}")/Kerno
  app=$(cygpath -u "$LOCALAPPDATA")/Programs/Kerno
  for folder in "$appdata" "$localappdata" "$app"; do
    if [ -e "$folder" ]; then
      echo "$folder exists: Kerno's data or install for this user"
      return
    fi
  done
  # A read that fails is a refusal, not an absence.
  entry=$(powershell -NoProfile -Command "if (Test-Path '$uninstall_key') { 'present' } else { 'absent' }" 2>/dev/null | tr -d '\r') \
    || entry="unreadable"
  if [ "$entry" != absent ]; then
    echo "Kerno's uninstall entry is ${entry:-unreadable}"
    return
  fi
  running=$(powershell -NoProfile -Command "@(Get-Process kerno -ErrorAction SilentlyContinue).Count" 2>/dev/null | tr -d '\r') \
    || running="unknown"
  if [ "$running" != 0 ]; then
    echo "kerno.exe processes running: ${running:-unknown}"
    return
  fi
}
