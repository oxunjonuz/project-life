#!/bin/sh
# Build the Windows application: the shell, the interface server and the core, in one portable folder.
#
#   sh app/windows/build_windows_app.sh [output-dir]
#
# What comes out:
#
#   ProjectLife.exe           the window, the tray icon and the menu (Win32 + WebView2)
#   pl.exe                    the core: observation, history, restore, export, import
#   pl-ui.exe                 the local interface server (drives the core, serves the page)
#
# The names carry a `pl-` prefix because Windows is case-insensitive: a folder cannot hold both
# `ProjectLife.exe` and `projectlife.exe`, and one of them would overwrite the other without a word.
#   pl-mcp.exe                the read-only MCP server
#   WebView2Loader.dll        Microsoft's loader for the WebView2 runtime (license beside it)
#   install.ps1               installs into %LOCALAPPDATA%\Programs, with a Start Menu entry
#   verify_windows.ps1        the checks that can only be run ON Windows — see docs/PLATFORMS.md
#
# Requirements ON THE MACHINE THAT RUNS IT: Windows 10/11 with the WebView2 Evergreen runtime, which
# ships with Windows 10/11 and with Microsoft Edge. Nothing else — no Rust, no Node.js, no Python.
#
# Environment:
#   WV2_VERSION   WebView2 SDK version to build against (default: 1.0.4078.44, sha256-checked)
#   WV2_CACHE     where the SDK is unpacked (default: /tmp/webview2-sdk)
#   NO_ZIP=1      do not build the portable zip (the folder itself is the deliverable)

set -e

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
OUT=${1:-$ROOT/dist/windows}
TARGET=x86_64-pc-windows-gnu
CC=x86_64-w64-mingw32-gcc
RC=x86_64-w64-mingw32-windres
PKG="$OUT/ProjectLife-windows-x86_64"

WV2_VERSION=${WV2_VERSION:-1.0.4078.44}
WV2_SHA256=dc4d1d9168df26b830398303e50210b6e1729f6ce5a7ac69d2c766852f489962
WV2_CACHE=${WV2_CACHE:-/tmp/webview2-sdk}
VERSION=$(cat "$ROOT/VERSION" 2>/dev/null || echo "0.9.4")
# The owner's name, his address and the one sentence about what the program is for, read once from
# src/brand.rs through the one reader — the version resource, this README and the shell's --version
# output all use these values, so they cannot say three different things.
eval "$(python3 "$ROOT/tools/brand.py" sh)"
VERNUM=$(printf '%s' "$VERSION" | awk -F. '{printf "%d,%d,%d,0", $1, $2, $3}')

say() { printf '\n== %s\n' "$1"; }

say "compilers"
command -v "$CC" >/dev/null || { echo "no $CC — install gcc-mingw-w64-x86-64"; exit 1; }
command -v "$RC" >/dev/null || { echo "no $RC — install binutils-mingw-w64-x86-64"; exit 1; }
printf '%s: %s\n' "$CC" "$($CC -dumpversion)"

say "the WebView2 SDK $WV2_VERSION (sha256-pinned)"
mkdir -p "$WV2_CACHE"
NUPKG="$WV2_CACHE/webview2-$WV2_VERSION.nupkg"
if [ ! -f "$NUPKG" ]; then
    curl -sSL -o "$NUPKG" "https://api.nuget.org/v3-flatcontainer/microsoft.web.webview2/$WV2_VERSION/microsoft.web.webview2.$WV2_VERSION.nupkg"
fi
GOT=$(sha256sum "$NUPKG" | cut -d' ' -f1)
if [ "$GOT" != "$WV2_SHA256" ]; then
    echo "the WebView2 SDK does not match the pinned sha256"
    echo "  expected $WV2_SHA256"
    echo "  got      $GOT"
    exit 1
fi
printf 'sha256 ok: %s\n' "$GOT"
SDK="$WV2_CACHE/sdk-$WV2_VERSION"
if [ ! -f "$SDK/WebView2.h" ]; then
    python3 - "$NUPKG" "$SDK" <<'PY'
import sys, zipfile
nupkg, dest = sys.argv[1], sys.argv[2]
z = zipfile.ZipFile(nupkg)
want = {
    "build/native/include/WebView2.h": "WebView2.h",
    "build/native/include/WebView2EnvironmentOptions.h": "WebView2EnvironmentOptions.h",
    "build/native/x64/WebView2Loader.dll": "WebView2Loader.dll",
    "LICENSE.txt": "WEBVIEW2-LICENSE.txt",
    "build/native/Microsoft.Web.WebView2.targets": "Microsoft.Web.WebView2.targets",
}
import os
os.makedirs(dest, exist_ok=True)
for member, name in want.items():
    with z.open(member) as src, open(os.path.join(dest, name), "wb") as out:
        out.write(src.read())
# The SDK's header includes EventToken.h, which belongs to the Windows SDK; mingw-w64 ships the same
# type in lower case. One line, and it is written here rather than patched into Microsoft's file.
with open(os.path.join(dest, "EventToken.h"), "w") as f:
    f.write("#include <eventtoken.h>\n")
print("unpacked to", dest)
PY
fi
[ -f "$SDK/WebView2.h" ] || { echo "the SDK did not unpack"; exit 1; }

say "core and interface server (cargo, release, $TARGET)"
( cd "$ROOT" && CARGO_TARGET_DIR="$ROOT/target" cargo build --release --target "$TARGET" )
( cd "$ROOT/app" && CARGO_TARGET_DIR="$ROOT/target" cargo build --release --target "$TARGET" )
REL="$ROOT/target/$TARGET/release"
for f in "$REL/projectlife.exe" "$REL/projectlife-ui.exe"; do
    [ -f "$f" ] || { echo "missing $f"; exit 1; }
done

say "icons (the design's own mark, via tools/make_icon.py)"
mkdir -p "$OUT"
python3 "$ROOT/tools/make_icon.py" --out "$OUT/ProjectLife.icns" --ico "$OUT/ProjectLife.ico" >/dev/null 2>&1 || true
# Two files whose names differ only in case cannot exist side by side in this package. Said here, at
# build time, with the names printed, rather than discovered as a mystery on the machine that runs it.
for a in ProjectLife.exe pl.exe pl-ui.exe pl-mcp.exe WebView2Loader.dll; do
    for b in ProjectLife.exe pl.exe pl-ui.exe pl-mcp.exe WebView2Loader.dll; do
        [ "$a" = "$b" ] && continue
        if [ "$(printf '%s' "$a" | tr 'A-Z' 'a-z')" = "$(printf '%s' "$b" | tr 'A-Z' 'a-z')" ]; then
            echo "REFUSING TO BUILD: $a and $b differ only in case"; exit 1
        fi
    done
done
if [ ! -f "$OUT/ProjectLife.ico" ]; then
    echo "no icon: run  python3 tools/make_icon.py --out /tmp/x.icns --ico $OUT/ProjectLife.ico"
    exit 1
fi
cp "$OUT/ProjectLife.ico" "$HERE/ProjectLife.ico"

say "the shell (Win32 + WebView2), version $VERSION ($VERNUM)"
mkdir -p "$PKG"
python3 "$ROOT/tools/fill_template.py" "$HERE/ProjectLife.rc" "$OUT/ProjectLife.rc.filled" \
    --brand VERSION="$VERSION" VERNUM="$VERNUM" || exit 1
"$RC" -I "$HERE" "$OUT/ProjectLife.rc.filled" -O coff -o "$OUT/ProjectLife.res"
printf 'shell source: %s  sha256 %s\n' "$HERE/ProjectLife.c" "$(sha256sum "$HERE/ProjectLife.c" | cut -d' ' -f1)"
"$CC" -O2 -Wall -Wno-unused-parameter -Wno-incompatible-pointer-types -Wno-unknown-pragmas -Wno-attributes \
    -I "$SDK" -I "$HERE/.." "$HERE/ProjectLife.c" "$OUT/ProjectLife.res" "$SDK/WebView2Loader.dll" \
    -o "$PKG/ProjectLife.exe" \
    -lole32 -loleaut32 -luuid -lshell32 -lwinhttp -luser32 -lgdi32 -municode -mwindows

cp "$REL/projectlife.exe" "$PKG/pl.exe"
cp "$REL/projectlife-ui.exe" "$PKG/pl-ui.exe"
[ -f "$REL/pl-mcp.exe" ] && cp "$REL/pl-mcp.exe" "$PKG/"
cp "$SDK/WebView2Loader.dll" "$PKG/"
cp "$ROOT/LICENSE" "$PKG/LICENSE.txt"
cp "$SDK/WEBVIEW2-LICENSE.txt" "$PKG/THIRD_PARTY_NOTICES.txt"
{
    echo "Project Life — third-party notices"
    echo
    echo "This package contains Microsoft's WebView2 loader:"
    echo "  WebView2Loader.dll   from the Microsoft.Web.WebView2 NuGet package $WV2_VERSION"
    echo "  build/native/x64/WebView2Loader.dll"
    echo "Its license is in WEBVIEW2-LICENSE.txt (also extracted from the same package)."
    echo "The WebView2 Evergreen runtime itself is part of Windows 10/11 and of Microsoft Edge; this"
    echo "package does not contain it and does not install it."
    echo
    echo "Everything else here is Project Life, MIT licensed: see LICENSE.txt."
} > "$PKG/THIRD_PARTY_NOTICES.txt"
cat "$SDK/WEBVIEW2-LICENSE.txt" >> "$PKG/THIRD_PARTY_NOTICES.txt"

say "installer, verifier and README"
cp "$HERE/install.ps1" "$HERE/uninstall.ps1" "$HERE/verify_windows.ps1" "$PKG/"
cp "$OUT/ProjectLife.ico" "$PKG/ProjectLife.ico"
cat > "$PKG/README.txt" <<EOF
Project Life — Windows (x86_64)
===============================

$PL_WHAT_IT_IS

By $PL_BY · $PL_LICENCE licence
$PL_COPYRIGHT

This folder is the whole application. Nothing has to be installed to try it:

    ProjectLife.exe

To install it for your user (no administrator rights, no service, no driver):

    powershell -ExecutionPolicy Bypass -File install.ps1

What each program is:

  ProjectLife.exe       the window, the tray icon and the menu. It stores nothing itself: it starts
                        the interface server below and shows what that server says.
  pl-ui.exe             the local interface server (127.0.0.1 only, one per launch, guarded by a
                        token generated at launch).
  pl.exe                the core: observation, history, restore, export, import, repair.
  pl-mcp.exe            a read-only MCP server for agents, over the same archive.
  WebView2Loader.dll    Microsoft's loader for the WebView2 runtime (see THIRD_PARTY_NOTICES.txt).

Requirement: Windows 10 or 11 with the WebView2 Evergreen runtime, which is part of Windows and of
Microsoft Edge. If it is missing, the window says so in the system's own words and names the address
to get it from.

Closing the window does not stop the protection: the window hides, the tray icon stays (beside the
clock; Windows 11 may hide it under the ^ arrow), and the observation keeps running. "Quit
completely" in the tray menu stops the observation and closes everything, and it says what it does
before it does it. The tray menu also offers "Start with Windows", which writes exactly one value in
HKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Run.

No Authenticode signature: this build is not signed and claims no publisher. Windows may therefore
show "Windows protected your PC" the first time; "More info" -> "Run anyway" starts it without
disabling any protection. To check the files instead, use verify_windows.ps1, which prints the
sha256 of every file and compares them with SHA256SUMS.txt.
EOF

say "checksums and the portable zip"
( cd "$PKG" && sha256sum ProjectLife.exe pl.exe pl-ui.exe pl-mcp.exe WebView2Loader.dll \
    install.ps1 uninstall.ps1 verify_windows.ps1 README.txt > SHA256SUMS.txt )
cat "$PKG/SHA256SUMS.txt"
if [ -z "$NO_ZIP" ]; then
    ( cd "$OUT" && rm -f ProjectLife-windows-x86_64.zip && zip -q -r ProjectLife-windows-x86_64.zip "ProjectLife-windows-x86_64" )
    printf '\nzip: %s  sha256 %s\n' "$OUT/ProjectLife-windows-x86_64.zip" \
        "$(sha256sum "$OUT/ProjectLife-windows-x86_64.zip" | cut -d' ' -f1)"
fi

say "result"
file "$PKG"/*.exe "$PKG"/*.dll | sed 's/^/  /'
printf '\nthe folder is the deliverable:\n  %s\n' "$PKG"
printf 'on Windows, the checks that need Windows:\n  powershell -ExecutionPolicy Bypass -File verify_windows.ps1\n'
