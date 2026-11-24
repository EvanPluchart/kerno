# Saves a picture of the whole screen as a PNG: scripts/bundle-windows.sh keeps one of Kerno's
# window at its launch check, the only look at the Windows app a runner gives.
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts/windows-screenshot.ps1 <file.png>
param([Parameter(Mandatory = $true)][string]$Path)
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
$screen = [System.Windows.Forms.SystemInformation]::VirtualScreen
$bitmap = New-Object System.Drawing.Bitmap $screen.Width, $screen.Height
$graphics = [System.Drawing.Graphics]::FromImage($bitmap)
$graphics.CopyFromScreen($screen.Left, $screen.Top, 0, 0, $bitmap.Size)
$target = Join-Path (Get-Location) $Path
$bitmap.Save($target, [System.Drawing.Imaging.ImageFormat]::Png)
$graphics.Dispose()
$bitmap.Dispose()
Write-Output "screen $($screen.Width)x$($screen.Height) saved to $target"
