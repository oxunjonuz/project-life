# Remove what install.ps1 added, and nothing else.
#
#   powershell -ExecutionPolicy Bypass -File uninstall.ps1
#
# Your archive is NOT touched: it lives wherever you put it when the app asked, and this program has
# never deleted it. Your project folders are not touched either.

$ErrorActionPreference = "Continue"
$dest = Join-Path $env:LOCALAPPDATA "Programs\Project Life"
$shortcut = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\Project Life.lnk"

if (Test-Path $shortcut) {
    Remove-Item $shortcut -Force
    Write-Host "removed $shortcut"
}

$run = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
if (Test-Path $run) {
    $value = (Get-ItemProperty -Path $run -Name "ProjectLife" -ErrorAction SilentlyContinue).ProjectLife
    if ($value) {
        Remove-ItemProperty -Path $run -Name "ProjectLife"
        Write-Host "removed HKCU\...\Run\ProjectLife (it was: $value)"
    }
}

# The app's own folder (logs, the window's handshake). The ARCHIVE is not here, and is not touched.
$appData = Join-Path $env:LOCALAPPDATA "ProjectLife"
if (Test-Path $appData) {
    Write-Host ""
    Write-Host "the app's own folder was left in place: $appData"
    Write-Host "(it holds daemon.log and the window's handshake; delete it yourself if you want it gone)"
}

if (Test-Path $dest) {
    Get-Process -Name "ProjectLife", "pl-ui", "pl" -ErrorAction SilentlyContinue |
        ForEach-Object { Write-Host "stopping $($_.ProcessName) ($($_.Id))"; $_ | Stop-Process -Force }
    Start-Sleep -Milliseconds 400
    Remove-Item $dest -Recurse -Force
    Write-Host "removed $dest"
}

Write-Host ""
Write-Host "your archive and your history were not touched."
Write-Host "if a scheduled task was created earlier, remove it with:"
Write-Host "  schtasks /delete /tn ProjectLifeDaemon /f"
