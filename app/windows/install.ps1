# Install Project Life for the current user. No administrator rights, no service, no driver.
#
#   powershell -ExecutionPolicy Bypass -File install.ps1 [-Autostart]
#
# What it touches, and nothing else:
#   %LOCALAPPDATA%\Programs\Project Life\            the programs themselves
#   %APPDATA%\Microsoft\Windows\Start Menu\Programs\Project Life.lnk
#   HKCU\...\CurrentVersion\Run\ProjectLife         only with -Autostart (removed by uninstall.ps1)
#
# It does not touch your archive, your projects, or any system setting.

param(
    [switch]$Autostart
)

$ErrorActionPreference = "Stop"
$src = $PSScriptRoot
$dest = Join-Path $env:LOCALAPPDATA "Programs\Project Life"
$startMenu = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs"
$shortcut = Join-Path $startMenu "Project Life.lnk"

Write-Host "installing from $src"
Write-Host "             to $dest"

New-Item -ItemType Directory -Force -Path $dest | Out-Null
foreach ($f in @("ProjectLife.exe", "pl.exe", "pl-ui.exe", "pl-mcp.exe",
                 "WebView2Loader.dll", "ProjectLife.ico", "README.txt", "LICENSE.txt",
                 "THIRD_PARTY_NOTICES.txt", "uninstall.ps1", "verify_windows.ps1")) {
    $from = Join-Path $src $f
    if (Test-Path $from) {
        Copy-Item $from (Join-Path $dest $f) -Force
        Write-Host "  copied $f"
    }
}

# The Start Menu entry. A shortcut, not a service: the app starts when you start it.
$shell = New-Object -ComObject WScript.Shell
$link = $shell.CreateShortcut($shortcut)
$link.TargetPath = Join-Path $dest "ProjectLife.exe"
$link.WorkingDirectory = $dest
$link.IconLocation = Join-Path $dest "ProjectLife.ico"
$link.Description = "Project Life — a local flight recorder for your project files"
$link.Save()
Write-Host "  start menu: $shortcut"

if ($Autostart) {
    $run = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
    New-Item -Path $run -Force | Out-Null
    Set-ItemProperty -Path $run -Name "ProjectLife" -Value "`"$(Join-Path $dest 'ProjectLife.exe')`" --autostart"
    Write-Host "  autostart: HKCU\...\Run\ProjectLife"
} else {
    Write-Host "  autostart: not set (pass -Autostart, or use the tray menu's own entry)"
}

Write-Host ""
Write-Host "start it from the Start Menu (""Project Life""), or:"
Write-Host "  & `"$(Join-Path $dest 'ProjectLife.exe')`""
Write-Host ""
Write-Host "background observation at every logon (optional, and reversible):"
Write-Host "  & `"$(Join-Path $dest 'pl.exe')`" daemon install    # prints the exact schtasks command"
Write-Host ""
Write-Host "to remove all of it:   powershell -ExecutionPolicy Bypass -File `"$(Join-Path $dest 'uninstall.ps1')`""
Write-Host "your archive and your project folders are not touched by any of this."
