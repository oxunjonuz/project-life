#!/bin/sh
# Turn the portable Linux tree into the two things people actually install from: a tarball and a .deb.
#
#   sh tools/make_linux_packages.sh [dist-dir]
#
# Both are built from the same files, and the checksums come from the files on disk — a hand-typed
# list cannot disagree with what shipped.
#
# The .deb is built with dpkg-deb from a folder that this script writes: no debhelper, no build
# system, and nothing is installed on this machine. Its control file names the libraries the window
# links against, so apt can tell a person what is missing instead of the app failing to start.

set -e
ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT=${1:-$ROOT/dist/linux}
ARCH=$(dpkg --print-architecture 2>/dev/null || echo amd64)
TREE="$OUT/ProjectLife-linux-$(uname -m)"
PKG="$OUT/projectlife"

[ -d "$TREE" ] || { echo "no portable tree at $TREE — run sh app/linux/build_linux_app.sh first"; exit 1; }

say() { printf '\n== %s\n' "$1"; }
VERSION=$(cat "$ROOT/VERSION" 2>/dev/null || echo "0.9.4")
# The author, his address and the sentence about what the program is for, read from src/brand.rs
# through the one reader (tools/brand.py) — the Debian control file must not hold a second copy.
eval "$(python3 "$ROOT/tools/brand.py" sh)"

say "the portable tarball"
( cd "$OUT" && tar --sort=name --owner=0 --group=0 --numeric-owner -czf "ProjectLife-linux-$(uname -m).tar.gz" "ProjectLife-linux-$(uname -m)" )
printf '%s  %s\n' "$(sha256sum "$OUT/ProjectLife-linux-$(uname -m).tar.gz" | cut -d' ' -f1)" \
    "ProjectLife-linux-$(uname -m).tar.gz" > "$OUT/tarball.sha256"
cat "$OUT/tarball.sha256"

say "the .deb"
rm -rf "$PKG"
mkdir -p "$PKG/DEBIAN" "$PKG/usr/bin" "$PKG/usr/share/applications" \
         "$PKG/usr/share/icons/hicolor" "$PKG/lib/systemd/user" "$PKG/usr/share/doc/projectlife"
for f in projectlife projectlife-ui projectlife-app pl-mcp; do
    [ -f "$TREE/bin/$f" ] || continue
    cp "$TREE/bin/$f" "$PKG/usr/bin/$f"
    chmod 755 "$PKG/usr/bin/$f"
done
cp "$TREE/share/applications/project-life.desktop" "$PKG/usr/share/applications/"
for size in 16 24 32 48 64 128 256 512; do
    mkdir -p "$PKG/usr/share/icons/hicolor/${size}x${size}/apps"
    cp "$TREE/share/icons/hicolor/${size}x${size}/apps/project-life.png" \
       "$PKG/usr/share/icons/hicolor/${size}x${size}/apps/"
done
cp "$TREE/systemd/projectlife.service" "$PKG/lib/systemd/user/projectlife.service"
cp "$ROOT/LICENSE" "$PKG/usr/share/doc/projectlife/copyright"
cp "$TREE/README.txt" "$PKG/usr/share/doc/projectlife/README.txt"
gzip -9 -c "$ROOT/docs/CHANGELOG.md" > "$PKG/usr/share/doc/projectlife/changelog.gz"

# The dependencies are the libraries the shell was linked against — read from the binary, not typed
# from memory: `ldd` on the delivered file names the sonames this package cannot work without.
LIBDEPS=$(ldd "$TREE/bin/projectlife-app" | awk '/=> \// {print $3}' | while IFS= read -r library; do
    resolved=$(readlink -f "$library")
    owner=$(dpkg-query -S "$resolved" 2>/dev/null | head -1)
    [ -n "$owner" ] || { echo "Cannot determine Debian package for $resolved" >&2; exit 1; }
    printf '%s\n' "${owner%%:*}"
done | sort -u | paste -sd ',' -)
[ -n "$LIBDEPS" ] || { echo "No Debian dependencies were determined" >&2; exit 1; }
cat > "$PKG/DEBIAN/control" <<EOF
Package: projectlife
Version: $VERSION
Section: utils
Priority: optional
Architecture: $ARCH
Maintainer: $PL_AUTHOR <$PL_AUTHOR_EMAIL>
Installed-Size: $(du -sk "$PKG" | cut -f1)
Depends: $LIBDEPS
Recommends: libayatana-appindicator3-1
Description: $PL_PRODUCT — file history for your projects, kept locally
 Project Life keeps the history of the files in a project folder: one initial copy,
 then only the new content of the files that change, on the machine that owns them.
 No network, no telemetry, no account.
 .
 It is made for the moment an agent deletes or breaks something: it keeps everything.
 .
 It ships a desktop window, a tray icon, a background observer and a command line.
 Closing the window does not stop the observation; "Quit completely" does, and says so.
 .
 Author: $PL_AUTHOR <$PL_AUTHOR_EMAIL>. $PL_LICENCE licence.
EOF
cat > "$PKG/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
if command -v update-desktop-database >/dev/null 2>&1; then update-desktop-database -q /usr/share/applications || true; fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then gtk-update-icon-cache -q /usr/share/icons/hicolor 2>/dev/null || true; fi
echo "Project Life installed. Start it from your applications menu, or: projectlife-app"
echo "Background observation at every login (optional):  systemctl --user enable --now projectlife.service"
EOF
chmod 755 "$PKG/DEBIAN/postinst"
( cd "$OUT" && dpkg-deb --build "$PKG" "projectlife_${VERSION}_${ARCH}.deb" >/dev/null )
printf '%s  %s\n' "$(sha256sum "$OUT/projectlife_${VERSION}_${ARCH}.deb" | cut -d' ' -f1)" \
    "projectlife_${VERSION}_${ARCH}.deb" > "$OUT/deb.sha256"
cat "$OUT/deb.sha256"

say "what the package says it is"
dpkg-deb -I "$OUT/projectlife_${VERSION}_${ARCH}.deb" | sed 's/^/  /'
say "and what is inside"
dpkg-deb -c "$OUT/projectlife_${VERSION}_${ARCH}.deb" | awk '{print "  " $1 " " $6}' | sort -u | head -20
