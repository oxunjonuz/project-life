# The checks that can only be run ON Windows.
#
#   powershell -ExecutionPolicy Bypass -File verify_windows.ps1
#
# The build that produced this package ran on Linux: it cross-compiled these binaries and checked
# their bytes, but it could not start a single one of them. These are the checks that finish the job
# on a machine that has Windows.
#
# What it does: verifies the delivered files against SHA256SUMS.txt, runs the window's own selftest,
# then drives the full path on a scratch archive in %TEMP% — observation through the app's own route,
# an external change becoming a version, a restore compared byte by byte, and an export/import round
# trip. Your real archive is never touched: everything happens under a folder this script creates and
# deletes.
#
# Written without PowerShell 7 features, because Windows 10 ships Windows PowerShell 5.1 and this
# script is meant to run there.

$ErrorActionPreference = "Continue"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$here = $PSScriptRoot
$passed = 0
$failed = 0

function Ok($msg) {
    $script:passed = $script:passed + 1
    Write-Host "  PASS  $msg"
}
function Bad($msg) {
    $script:failed = $script:failed + 1
    Write-Host "  FAIL  $msg"
}
function Note($msg) {
    Write-Host "  note  $msg"
}

function Sha256($path) {
    return (Get-FileHash -Algorithm SHA256 -Path $path).Hash.ToLower()
}

Write-Host "== 0. this machine"
Write-Host ("     " + [System.Environment]::OSVersion.VersionString)
try {
    $rt = Get-ItemProperty "HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" -ErrorAction SilentlyContinue
    if ($rt -and $rt.pv) {
        Write-Host ("     WebView2 runtime: " + $rt.pv)
        Ok "the WebView2 Evergreen runtime is installed ($($rt.pv))"
    } else {
        Note "the WebView2 runtime was not found in the registry; the window will say so itself if it is missing"
    }
} catch {
    Note "the WebView2 runtime version could not be read from the registry"
}
Write-Host ("     PowerShell " + $PSVersionTable.PSVersion.ToString())

Write-Host ""
Write-Host "== 1. the delivered bytes are the delivered bytes"
$sums = Join-Path $here "SHA256SUMS.txt"
if (Test-Path $sums) {
    $lines = Get-Content $sums
    $checked = 0
    foreach ($line in $lines) {
        if ($line.Trim().Length -eq 0) { continue }
        $parts = $line -split '\s+', 2
        $want = $parts[0].ToLower()
        $name = $parts[1].TrimStart('*').Trim()
        $file = Join-Path $here $name
        if (-not (Test-Path $file)) {
            Bad "missing file: $name"
            continue
        }
        $got = Sha256 $file
        if ($got -eq $want) {
            $checked = $checked + 1
        } else {
            Bad "$name does not match SHA256SUMS.txt (want $want, got $got)"
        }
    }
    if ($checked -gt 0 -and $failed -eq 0) {
        Ok "$checked file(s) match SHA256SUMS.txt exactly"
    }
} else {
    Bad "no SHA256SUMS.txt beside this script"
}

Write-Host ""
Write-Host "== 2. the window's own selftest (real WebView2, real page, real server)"
$appData = Join-Path $env:LOCALAPPDATA "ProjectLife"
$scratch = Join-Path $env:TEMP ("pl-verify-" + (Get-Date -Format "yyyyMMdd-HHmmss"))
New-Item -ItemType Directory -Force -Path $scratch | Out-Null
$archive = Join-Path $scratch "archive"
$project = Join-Path $scratch "proj"
New-Item -ItemType Directory -Force -Path (Join-Path $project "src") | Out-Null
Set-Content -Path (Join-Path $project "src\app.ts") -Value "export const v = 1;" -Encoding UTF8

$env:PROJECTLIFE_APP_HOME = Join-Path $scratch "app-home"
New-Item -ItemType Directory -Force -Path $env:PROJECTLIFE_APP_HOME | Out-Null

$shell = Join-Path $here "ProjectLife.exe"
$core = Join-Path $here "pl.exe"

& $shell --selftest | Out-Null
$selftestFile = Join-Path $env:PROJECTLIFE_APP_HOME "shell-selftest.txt"
if (Test-Path $selftestFile) {
    $report = Get-Content $selftestFile
    foreach ($line in $report) { Write-Host ("     " + $line) }
    $result = $report | Where-Object { $_ -match "RESULT=" }
    if ($result -match "RESULT=PASS") {
        Ok "the window loaded the shipped page in WebView2 and reported PASS"
    } else {
        Bad "the window's selftest did not pass: $result"
    }
} else {
    Bad "no shell-selftest.txt was written to $env:PROJECTLIFE_APP_HOME"
}

Write-Host ""
Write-Host "== 3. a scratch archive: nothing here is your history"
& $core init-archive $archive | Out-Null
& $core --archive $archive config set stopFreePercent 0 | Out-Null
& $core --archive $archive config set warnFreePercent 0 | Out-Null
& $core --archive $archive add $project --profile all --yes | Out-Null
if ($LASTEXITCODE -eq 0) {
    Ok "a scratch archive and project exist under $scratch"
} else {
    Bad "could not create the scratch archive (exit $LASTEXITCODE)"
}

Write-Host ""
Write-Host "== 4. the app protects: observation through the app's own route"
$logFile = Join-Path $env:PROJECTLIFE_APP_HOME "daemon.log"
$proc = Start-Process -FilePath $shell -ArgumentList @("--archive", $archive) -PassThru
$ready = $null
$tries = 0
while ($tries -lt 60 -and -not $ready) {
    Start-Sleep -Milliseconds 500
    $tries = $tries + 1
    if (Test-Path $logFile) {
        $line = Get-Content $logFile | Where-Object { $_ -match "server said: " } | Select-Object -Last 1
        if ($line) {
            $json = $line.Substring($line.IndexOf("server said: ") + 13)
            try {
                $obj = $json | ConvertFrom-Json
                if ($obj.ready) { $ready = $obj }
            } catch {
                $ready = $null
            }
        }
    }
}
if ($ready) {
    Ok "the window is up; its server answers on 127.0.0.1:$($ready.port)"
    $token = $ready.token
    $base = "http://127.0.0.1:$($ready.port)/api"
    $start = Invoke-RestMethod -Method Post -Uri "$base/watch/start?token=$token" -ContentType "application/json" -Body '{}'
    if ($start.started) {
        Ok "observation started through the app's own route (daemon pid $($start.pid))"
    } else {
        Bad "observation did not start: $($start | ConvertTo-Json -Compress)"
    }
    $state = ""
    $tries = 0
    while ($tries -lt 60 -and $state -ne "protected" -and $state -ne "protected_low_space") {
        Start-Sleep -Milliseconds 500
        $tries = $tries + 1
        $watch = Invoke-RestMethod -Uri "$base/watch?token=$token"
        if ($watch.protection) { $state = $watch.protection.state }
    }
    if ($state -eq "protected" -or $state -eq "protected_low_space") {
        Ok "the app reports real protection ($state)"
    } else {
        Bad "the app does not report protection (state: $state)"
    }

    $before = (& $core --archive $archive status proj --json | ConvertFrom-Json)
    $versionBefore = 0
    if ($before -is [array]) { $versionBefore = $before[0].versions } else { $versionBefore = $before.versions }
    Set-Content -Path (Join-Path $project "src\app.ts") -Value "export const v = 2;" -Encoding UTF8
    $versionAfter = $versionBefore
    $tries = 0
    while ($tries -lt 60 -and $versionAfter -le $versionBefore) {
        Start-Sleep -Milliseconds 500
        $tries = $tries + 1
        $now = (& $core --archive $archive status proj --json | ConvertFrom-Json)
        if ($now -is [array]) { $versionAfter = $now[0].versions } else { $versionAfter = $now.versions }
    }
    if ($versionAfter -gt $versionBefore) {
        Ok "an external change became a version in the archive ($versionBefore -> $versionAfter), measured by the core"
    } else {
        Bad "the external change was not stored ($versionBefore -> $versionAfter)"
    }

    # Quit completely, exactly as the tray menu does it.
    Set-Content -Path (Join-Path $env:PROJECTLIFE_APP_HOME "quit.request") -Value "quit"
    $waited = 0
    while ($waited -lt 60 -and -not $proc.HasExited) {
        Start-Sleep -Milliseconds 500
        $waited = $waited + 1
    }
    if ($proc.HasExited) {
        Ok "the window left on ""quit completely"""
    } else {
        Bad "the window did not leave after quit completely"
        $proc | Stop-Process -Force
    }
    $lockGone = -not (Test-Path (Join-Path $archive ".daemon"))
    if ($lockGone) {
        Ok "the observation really stopped: the daemon lock was released"
    } else {
        Bad "the daemon lock was left behind: a daemon may still be running"
    }
} else {
    Bad "the window never reported a ready server; see $logFile"
}

Write-Host ""
Write-Host "== 5. restore, compared byte by byte"
$restoreDir = Join-Path $scratch "restored"
$moment = (& $core --archive $archive status proj --json | ConvertFrom-Json)
if ($moment -is [array]) { $moment = $moment[0] }
& $core --archive $archive restore proj --at $moment.lastObservedAt --to $restoreDir | Out-Null
$restored = Join-Path $restoreDir "src\app.ts"
if (Test-Path $restored) {
    $a = Sha256 (Join-Path $project "src\app.ts")
    $b = Sha256 $restored
    if ($a -eq $b) {
        Ok "the restored file is byte-identical to the file on disk (sha256 $a)"
    } else {
        Note "the restored file differs from the current file (that is correct if the change came after the moment restored)"
        Write-Host "     restored $b"
        Write-Host "     current  $a"
    }
} else {
    Bad "the restore did not produce $restored"
}

Write-Host ""
Write-Host "== 6. export and import"
$exp = Join-Path $scratch "export.plx"
& $core --archive $archive export proj --out $exp | Out-Null
if (Test-Path $exp) {
    Ok "the history exported to a file ($([Math]::Round((Get-Item $exp).Length / 1KB, 1)) KB)"
} else {
    Bad "the export produced no file"
}
$archive2 = Join-Path $scratch "archive2"
& $core init-archive $archive2 | Out-Null
& $core --archive $archive2 import $exp --name imported | Out-Null
if ($LASTEXITCODE -eq 0) {
    Ok "the export imported into a second archive as its own project"
} else {
    Bad "the import failed (exit $LASTEXITCODE)"
}

Write-Host ""
Write-Host "== 7. the tray and the window's own behaviour"
Note "the tray icon, the first-close balloon, the registry autostart entry and the WebView2 window"
Note "cannot be checked by a script: they need a person at the screen. What to look at:"
Note "  1. close the window (X) -> it hides, and a balloon says protection continues"
Note "  2. right-click the shield beside the clock -> the state, the window, stop/start, quit"
Note "  3. the tray menu's last group is the server's own menu, the same list the window shows"
Note "  4. 'Quit completely' -> the observation stops and the window closes"
Note "if the icon is not beside the clock, look under the ^ arrow (Windows 11 hides new icons there)"

Write-Host ""
Write-Host "WINDOWS VERIFY: $passed passed, $failed failed"
Write-Host "scratch folder (safe to delete): $scratch"
if ($failed -eq 0) {
    Write-Host "nothing was left running; your own archive was never touched"
    exit 0
} else {
    exit 1
}
