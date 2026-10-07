#!/bin/sh
# Build the Linux application: the shell, the interface server and the core, in one portable tree.
#
#   sh app/linux/build_linux_app.sh [output-dir]
#
# What comes out (nothing here needs Rust, Node.js or a compiler to *run* — those are needed only to
# build it):
#
#   bin/projectlife-app     the window, the tray icon and the menu (GTK 3 + WebKitGTK 4.1)
#   bin/projectlife-ui      the local interface server: it drives the core and serves the page
#   bin/projectlife         the core: observation, history, restore, export, import
#   bin/pl-mcp              the read-only MCP server, the same one the macOS bundle ships
#   share/applications/     the desktop entry, so it appears in the applications menu
#   share/icons/            the design's own mark, in the sizes a Linux icon theme wants
#   systemd/projectlife.service   background observation that starts with the session
#   install.sh              installs into ~/.local (no root needed) and says what it changed
#   uninstall.sh            removes exactly what install.sh added
#
# Environment:
#   WEBKIT_PC   pkg-config name for WebKitGTK (default: webkit2gtk-4.1, then webkit2gtk-4.0)
#   NO_APPINDICATOR=1   build without the tray icon (the window menu bar then carries the same list)

set -e

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
OUT=${1:-$ROOT/dist/linux}
ARCH=$(uname -m)
PKG="$OUT/ProjectLife-linux-$ARCH"

say() { printf '\n== %s\n' "$1"; }

# The owner's name, his address and the one sentence about what the program is for: read once from
# src/brand.rs through the one reader (tools/brand.py), so the desktop entry, this tree's README and
# the shell's own --version output cannot say three different things.
eval "$(python3 "$ROOT/tools/brand.py" sh)"
VERSION=$(cat "$ROOT/VERSION" 2>/dev/null || echo "0.9.4")

say "libraries"
GTK=$(pkg-config --modversion gtk+-3.0)
WEBKIT_PC=${WEBKIT_PC:-}
if [ -z "$WEBKIT_PC" ]; then
    if pkg-config --exists webkit2gtk-4.1; then WEBKIT_PC=webkit2gtk-4.1
    elif pkg-config --exists webkit2gtk-4.0; then WEBKIT_PC=webkit2gtk-4.0
    else echo "no WebKitGTK development files found (libwebkit2gtk-4.1-dev)"; exit 1; fi
fi
WK=$(pkg-config --modversion "$WEBKIT_PC")
printf 'gtk+-3.0 %s\n%s %s\n' "$GTK" "$WEBKIT_PC" "$WK"

APPIND_PC=""
APPIND_VER="none"
if [ -z "$NO_APPINDICATOR" ]; then
    for p in ayatana-appindicator3-0.1 appindicator3-0.1; do
        if pkg-config --exists "$p"; then APPIND_PC=$p; APPIND_VER=$(pkg-config --modversion "$p"); break; fi
    done
fi
if [ -n "$APPIND_PC" ]; then
    printf '%s %s (tray icon available)\n' "$APPIND_PC" "$APPIND_VER"
else
    echo "no appindicator development files: building without the tray icon (the menu bar carries the same list)"
fi

say "core and interface server (cargo, release)"
( cd "$ROOT" && cargo build --release )
( cd "$ROOT/app" && cargo build --release )
CORE="$ROOT/target/release/projectlife"
UISRV="$ROOT/app/target/release/projectlife-ui"
MCP="$ROOT/target/release/pl-mcp"
for f in "$CORE" "$UISRV"; do [ -f "$f" ] || { echo "missing $f"; exit 1; }; done

say "the shell (GTK + WebKitGTK)"
mkdir -p "$PKG/bin"
PKGS="gtk+-3.0 $WEBKIT_PC libsoup-3.0 json-glib-1.0"
[ -n "$APPIND_PC" ] && PKGS="$PKGS $APPIND_PC"
DEFS="-DPL_GTK_VERSION=\"$GTK\" -DPL_WEBKIT_VERSION=\"$WK\" -DPL_APPINDICATOR_VERSION=\"$APPIND_VER\""
[ -n "$APPIND_PC" ] && DEFS="$DEFS -DPL_HAVE_APPINDICATOR"
# shellcheck disable=SC2086
cc -O2 -Wall -Wextra -Wno-unused-parameter -I "$HERE/.." -o "$PKG/bin/projectlife-app" "$HERE/ProjectLife.c" \
    $DEFS $(pkg-config --cflags $PKGS) $(pkg-config --libs $PKGS) -lcairo

cp "$CORE" "$PKG/bin/projectlife"
cp "$UISRV" "$PKG/bin/projectlife-ui"
chmod 755 "$PKG/bin/projectlife" "$PKG/bin/projectlife-ui"
[ -f "$MCP" ] && { cp "$MCP" "$PKG/bin/pl-mcp"; chmod 755 "$PKG/bin/pl-mcp"; }

say "desktop entry, icons and the background service"
mkdir -p "$OUT"
python3 "$ROOT/tools/make_icon.py" --out "$OUT/ProjectLife.icns" --png-dir "$OUT/icons"
mkdir -p "$PKG/share/applications" "$PKG/share/icons/hicolor" "$PKG/systemd"
python3 "$ROOT/tools/fill_template.py" "$HERE/project-life.desktop.in" \
    "$PKG/share/applications/project-life.desktop" --brand || exit 1
printf '  desktop entry: %s — %s\n' "$PL_PRODUCT" "$PL_AUTHOR"
for size in 16 24 32 48 64 128 256 512; do
    mkdir -p "$PKG/share/icons/hicolor/${size}x${size}/apps"
    cp "$OUT/icons/icon-$size.png" "$PKG/share/icons/hicolor/${size}x${size}/apps/project-life.png"
done
# Background observation that starts with the session: the same unit `projectlife daemon install`
# writes, kept beside the app so a person can read it before installing it.
cat > "$PKG/systemd/projectlife.service" <<EOF
[Unit]
Description=Project Life observer (background file history)
Documentation=file:$PKG/README.txt

[Service]
ExecStart=%h/.local/bin/projectlife daemon run
Restart=always
RestartSec=5
Nice=10
IOSchedulingClass=idle

[Install]
WantedBy=default.target
EOF

say "install / uninstall"
cat > "$PKG/install.sh" <<'EOF'
#!/bin/sh
# Install Project Life for one user, into ~/.local. No root, and every file this touches is named.
set -e
HERE=$(cd "$(dirname "$0")" && pwd)
PREFIX=${PREFIX:-$HOME/.local}
BIN="$PREFIX/bin"
APP="$PREFIX/share/applications"
ICONS="$PREFIX/share/icons/hicolor"
echo "installing into $PREFIX"
mkdir -p "$BIN" "$APP" "$ICONS"
for f in projectlife projectlife-ui projectlife-app pl-mcp; do
    [ -f "$HERE/bin/$f" ] || continue
    cp "$HERE/bin/$f" "$BIN/$f"
    chmod 755 "$BIN/$f"
    echo "  $BIN/$f"
done
cp "$HERE/share/applications/project-life.desktop" "$APP/" && echo "  $APP/project-life.desktop"
for size in 16 24 32 48 64 128 256 512; do
    [ -d "$HERE/share/icons/hicolor/${size}x${size}/apps" ] || continue
    mkdir -p "$ICONS/${size}x${size}/apps"
    cp "$HERE/share/icons/hicolor/${size}x${size}/apps/project-life.png" "$ICONS/${size}x${size}/apps/"
done
echo "  icons in $ICONS"
if command -v update-desktop-database >/dev/null 2>&1; then update-desktop-database "$APP" || true; fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then gtk-update-icon-cache -q "$ICONS" 2>/dev/null || true; fi
echo
echo "start it from your applications menu (\"Project Life\"), or:"
echo "  $BIN/projectlife-app"
echo
echo "background observation at every login (optional):"
echo "  projectlife daemon install      # writes ~/.config/systemd/user/projectlife.service"
echo "  systemctl --user enable --now projectlife.service"
EOF
cat > "$PKG/uninstall.sh" <<'EOF'
#!/bin/sh
# Remove exactly what install.sh added. The archive is NOT touched: it is yours, wherever you put it,
# and this program never deletes it.
set -e
PREFIX=${PREFIX:-$HOME/.local}
for f in projectlife projectlife-ui projectlife-app pl-mcp; do
    rm -f "$PREFIX/bin/$f" && echo "removed $PREFIX/bin/$f"
done
rm -f "$PREFIX/share/applications/project-life.desktop"
for size in 16 24 32 48 64 128 256 512; do
    rm -f "$PREFIX/share/icons/hicolor/${size}x${size}/apps/project-life.png"
done
echo "removed the desktop entry and the icons"
echo "your archive and its history were not touched"
EOF
chmod 755 "$PKG/install.sh" "$PKG/uninstall.sh"

say "README"
cat > "$PKG/README.txt" <<EOF
Project Life — Linux ($ARCH)
============================

$PL_WHAT_IT_IS

By $PL_BY · $PL_LICENCE licence
$PL_COPYRIGHT

This folder is the whole application. Nothing has to be installed to try it:

    ./bin/projectlife-app

Install it for your user (into ~/.local, no root):

    ./install.sh

What each program is:

  bin/projectlife-app   the window, the tray icon and the menu. It stores nothing itself: it starts
                        the interface server below and shows what that server says.
  bin/projectlife-ui    the local interface server (127.0.0.1, one per launch, guarded by a token
                        generated at launch).
  bin/projectlife       the core: observation, history, restore, export, import, repair.
  bin/pl-mcp            a read-only MCP server for agents, over the same archive.

Requirements on the machine that RUNS it: GTK 3, WebKitGTK 4.1 (libwebkit2gtk-4.1-0) and, for the
tray icon, libayatana-appindicator3. These are the libraries this build was linked against:

  gtk+-3.0 $GTK
  $WEBKIT_PC $WK
  appindicator: $APPIND_VER

If a library is missing the window says so when it starts, in its own words, and
~/.local/state/projectlife-app/daemon.log carries the same sentence.

Closing the window does not stop the protection: the window hides, the tray icon stays, and the
observation keeps running. "Quit completely" (in the tray menu and in the window's own menu) stops
observation and closes everything; it says what it does before it does it.
EOF

say "result"
find "$PKG" -type f | sort | sed "s|$PKG|  $PKG|"
printf '\nrun it with:\n  %s/bin/projectlife-app\n' "$PKG"
