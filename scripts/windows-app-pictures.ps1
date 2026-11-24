# Drives the packaged kerno.exe from outside on a Windows runner and keeps one picture of the screen
# per step (K68, docs/DECISIONS.md D-137): the connection opened from a URL with its tree, a query and
# its result, a table's data, the connection dialog, Settings. Keys go to the focused window as a
# person's would (SendKeys); the app runs on a scratch data folder and an in-memory secret store.
# Needs the seeded PostgreSQL of scripts/windows-test-servers.sh on 127.0.0.1:55432.
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts/windows-app-pictures.ps1 <kerno.exe> <folder>
# <folder> is relative to the current folder (scripts/windows-screenshot.ps1 writes there).
param(
  [Parameter(Mandatory = $true)][string]$Exe,
  [Parameter(Mandatory = $true)][string]$Out
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms
Add-Type -Namespace Kerno -Name Window -MemberDefinition @'
[DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr window, int command);
[DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr window);
'@

New-Item -ItemType Directory -Force $Out | Out-Null
$temp = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { $env:TEMP }
$env:KERNO_DATA_DIR = Join-Path $temp 'kerno-pictures-data'
$env:KERNO_SECRET_STORE = 'memory'
$env:KERNO_NO_UPDATE_CHECK = '1'
$app = Start-Process -FilePath $Exe -ArgumentList 'postgres://kerno:kerno_test_pw@127.0.0.1:55432/kerno' -PassThru

function Picture([string]$Name) {
  & (Join-Path $PSScriptRoot 'windows-screenshot.ps1') (Join-Path $Out "$Name.png")
}
function Keys([string]$Keys, [int]$Wait = 2) {
  [Kerno.Window]::SetForegroundWindow($app.MainWindowHandle) | Out-Null
  [System.Windows.Forms.SendKeys]::SendWait($Keys)
  Start-Sleep -Seconds $Wait
}

try {
  for ($i = 0; $i -lt 60 -and $app.MainWindowHandle -eq 0; $i++) {
    if ($app.HasExited) { throw "kerno.exe stopped by itself (exit code $($app.ExitCode))" }
    Start-Sleep -Seconds 1
    $app.Refresh()
  }
  if ($app.MainWindowHandle -eq 0) { throw 'no window after 60 s' }
  [Kerno.Window]::ShowWindow($app.MainWindowHandle, 3) | Out-Null # maximized
  # The connection from the command line: connected, its tree drawn.
  Start-Sleep -Seconds 12
  Picture 'm12-windows-connection'

  # A query tab on that connection, a statement typed and run.
  Keys '^t'
  Keys 'select id, email, full_name, tags from shop.customers order by id' 1
  Keys '{ESC}' 1
  Keys '^{ENTER}' 6
  Picture 'm12-windows-query'

  # A table's data tab, through Open table (quick open).
  Keys '^p'
  Keys 'shop.products' 2
  Keys '{ENTER}' 6
  Picture 'm12-windows-table'

  Keys '^n' 3
  Picture 'm12-windows-connection-dialog'
  Keys '{ESC}'

  Keys '^,' 3
  Picture 'm12-windows-settings'

  if ($app.HasExited) { throw "kerno.exe stopped during the steps (exit code $($app.ExitCode))" }
} finally {
  if (-not $app.HasExited) { Stop-Process -Id $app.Id -Force }
  $logs = Join-Path $env:KERNO_DATA_DIR 'logs'
  if (Test-Path $logs) { Get-ChildItem $logs -File | ForEach-Object { Get-Content $_.FullName -Tail 40 } }
}
