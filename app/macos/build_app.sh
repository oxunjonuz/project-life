#!/bin/sh
# Build "Project Life.app" for Apple Silicon (arm64) on a Linux host.
#
# Everything the app needs is produced here: three arm64 Mach-O executables (the macOS shell, the
# interface server and the core), the bundle around them, and an ad-hoc code signature. Nobody who
# runs the app needs Rust, Node.js or any developer tool — those are needed only to *build* it.
#
#   sh macos/build_app.sh [output-dir]
#
# Environment (all optional):
#   SDK_ROOT     an already extracted MacOSX*.sdk          (default: download 15.5, sha256-checked)
#   SDK_VERSION  which SDK to download                      (default: 15.5)
#   MACOS_MIN    deployment target for the binaries         (default: 11.0)
#   CC           clang to use                               (default: clang-19, clang, then LLVM)
#   RCodesign    path to rcodesign (https://github.com/indygreg/apple-platform-rs) if available
#   NO_SIGN=1    skip signing (the app will NOT start on Apple Silicon without a signature)

set -e

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
# Absolute on purpose: the Rust builds below run with `cd` in a subdirectory, and a relative output
# folder made the linker path in $OUT/tools unreachable — "linker not found" for a build that was
# otherwise fine. Measured on 2026-10-07; the same trap the Linux and Windows scripts avoid.
OUT=$(cd "$(dirname "${1:-$ROOT/dist}")" 2>/dev/null && pwd)/$(basename "${1:-$ROOT/dist}")
SDK_VERSION=${SDK_VERSION:-15.5}
MACOS_MIN=${MACOS_MIN:-11.0}
TARGET=aarch64-apple-darwin
TOOLS="$OUT/tools"
CACHE=${SDK_CACHE:-/tmp/macos-sdk}

say() { printf '\n== %s\n' "$1"; }

# ------------------------------------------------------------------ 1. the linker
# Rust and clang both need a Mach-O linker. LLVM's lld has a Mach-O port; it is invoked under its
# ld64 name so the flavour is chosen by argv[0].
find_lld() {
    for c in /usr/lib/llvm-19/bin/ld64.lld /usr/lib/llvm/bin/ld64.lld /usr/bin/ld64.lld; do
        [ -x "$c" ] && { echo "$c"; return; }
    done
    command -v ld64.lld 2>/dev/null || true
}

find_cc() {
    for c in "${CC:-}" clang-19 clang /usr/lib/llvm-19/bin/clang; do
        [ -n "$c" ] && command -v "$c" >/dev/null 2>&1 && { echo "$c"; return; }
    done
    echo clang
}

# ------------------------------------------------------------------ 2. the SDK
if [ -z "$SDK_ROOT" ]; then
    SDK_ROOT="$CACHE/MacOSX$SDK_VERSION.sdk"
fi
if [ ! -d "$SDK_ROOT" ]; then
    say "downloading the macOS SDK $SDK_VERSION"
    mkdir -p "$CACHE"
    URL="https://github.com/joseluisq/macosx-sdks/releases/download/$SDK_VERSION/MacOSX$SDK_VERSION.sdk.tar.xz"
    SUM_URL="https://github.com/joseluisq/macosx-sdks/releases/download/$SDK_VERSION/sha256sum.txt"
    [ -f "$CACHE/MacOSX$SDK_VERSION.sdk.tar.xz" ] || curl -sSL -o "$CACHE/MacOSX$SDK_VERSION.sdk.tar.xz" "$URL"
    if curl -sSL -o "$CACHE/sha256sum.txt" "$SUM_URL"; then
        ( cd "$CACHE" && sha256sum -c sha256sum.txt ) || { echo "SDK checksum does not match — refusing"; exit 1; }
    fi
    tar -xJf "$CACHE/MacOSX$SDK_VERSION.sdk.tar.xz" -C "$CACHE"
fi
[ -d "$SDK_ROOT" ] || { echo "no SDK at $SDK_ROOT"; exit 1; }

CC_BIN=$(find_cc)
LLD_BIN=$(find_lld)
[ -n "$LLD_BIN" ] || { echo "no ld64.lld found (install lld)"; exit 1; }
say "using CC=$CC_BIN  LLD=$LLD_BIN  SDK=$SDK_ROOT  min macOS $MACOS_MIN"

mkdir -p "$TOOLS"
ln -sf "$LLD_BIN" "$TOOLS/ld64.lld"

# A clang driver that Rust can use as its linker: same flags, but always aimed at the SDK.
cat > "$TOOLS/clang-mac" <<EOF
#!/bin/sh
exec "$CC_BIN" --target=arm64-apple-macos$MACOS_MIN -isysroot "$SDK_ROOT" -fuse-ld=lld --ld-path="$TOOLS/ld64.lld" "\$@"
EOF
chmod +x "$TOOLS/clang-mac"

# ------------------------------------------------------------------ 3. Rust targets
say "building the core and the interface server for $TARGET"
if command -v rustup >/dev/null 2>&1; then
    rustup target add "$TARGET" >/dev/null 2>&1 || true
fi

export CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER="$TOOLS/clang-mac"
export RUSTFLAGS="-C link-arg=-Wl,-adhoc_codesign"
export SDKROOT="$SDK_ROOT"

( cd "$ROOT" && cargo build --release --target "$TARGET" )
( cd "$ROOT/app" && cargo build --release --target "$TARGET" )

CORE="$ROOT/target/$TARGET/release/projectlife"
UISRV="$ROOT/app/target/$TARGET/release/projectlife-ui"
MCP="$ROOT/target/$TARGET/release/pl-mcp"
for f in "$CORE" "$UISRV"; do [ -f "$f" ] || { echo "missing $f"; exit 1; }; done

# ------------------------------------------------------------------ 4. the macOS shell
say "compiling the shell (AppKit + WebKit)"
# The bundle lookup is the one line that decides whether the app starts at all, and on 2026-10-06 it
# was wrong once: `pathForResource:ofType:inDirectory:@"Resources"` asks the resource folder for a
# folder *inside* the resource folder, which is where nothing lives, and the window then said
# "reinstall". The build refuses to produce a bundle with that mistake again.
if grep -q 'inDirectory:' "$HERE/ProjectLife.m"; then
    echo "REFUSING TO BUILD: $HERE/ProjectLife.m uses pathForResource:inDirectory: — the helper"
    echo "programs live directly in Contents/Resources, so no directory may be named a second time."
    exit 1
fi
printf 'shell source: %s  sha256 %s\n' "$HERE/ProjectLife.m" "$(shasum -a 256 "$HERE/ProjectLife.m" 2>/dev/null | cut -d' ' -f1 || sha256sum "$HERE/ProjectLife.m" | cut -d' ' -f1)"
SHELL_OBJ="$OUT/ProjectLife.o"
"$CC_BIN" --target=arm64-apple-macos$MACOS_MIN -isysroot "$SDK_ROOT" -fobjc-arc \
    -I "$HERE/.." \
    -Wno-deprecated-declarations -O2 -c "$HERE/ProjectLife.m" -o "$SHELL_OBJ"
"$CC_BIN" --target=arm64-apple-macos$MACOS_MIN -isysroot "$SDK_ROOT" -fuse-ld=lld \
    --ld-path="$TOOLS/ld64.lld" -Wl,-adhoc_codesign -Wl,-platform_version,macos,$MACOS_MIN,$SDK_VERSION \
    "$SHELL_OBJ" -framework Cocoa -framework WebKit -framework UniformTypeIdentifiers \
    -o "$OUT/ProjectLife"

# ------------------------------------------------------------------ 5. the bundle
say "assembling Project Life.app"
APP="$OUT/Project Life.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$OUT/ProjectLife" "$APP/Contents/MacOS/ProjectLife"
cp "$CORE" "$APP/Contents/Resources/projectlife"
cp "$UISRV" "$APP/Contents/Resources/projectlife-ui"
[ -f "$MCP" ] && cp "$MCP" "$APP/Contents/Resources/pl-mcp"
chmod 755 "$APP/Contents/MacOS/ProjectLife" "$APP/Contents/Resources/projectlife" \
    "$APP/Contents/Resources/projectlife-ui"
[ -f "$APP/Contents/Resources/pl-mcp" ] && chmod 755 "$APP/Contents/Resources/pl-mcp"
# The Info.plist is a template: the version comes from the delivery's VERSION file and the author,
# address and purpose from src/brand.rs (through tools/brand.py). A token left unfilled fails the
# build — before round 302 the version here was typed by hand and had drifted a release behind.
VERSION=$(cat "$ROOT/VERSION" 2>/dev/null || echo "0.9.4")
python3 "$ROOT/tools/fill_template.py" "$HERE/Info.plist" "$APP/Contents/Info.plist" \
    --brand VERSION="$VERSION" || exit 1
# A filled plist that does not parse is a bundle macOS will not start — and XML comments may not
# contain a doubled hyphen, which is exactly how the first version of this template was ruined.
python3 -c "import plistlib,sys;plistlib.load(open(sys.argv[1],'rb'))" "$APP/Contents/Info.plist" \
    || { echo "REFUSING TO BUILD: $APP/Contents/Info.plist is not a parsable property list"; exit 1; }
printf 'Info.plist: version %s, author %s\n' "$VERSION" "$(python3 "$ROOT/tools/brand.py" get AUTHOR)"
if [ -f "$HERE/ProjectLife.icns" ]; then cp "$HERE/ProjectLife.icns" "$APP/Contents/Resources/ProjectLife.icns"; fi
printf 'APPL????' > "$APP/Contents/PkgInfo"

# ------------------------------------------------------------------ 6. signature
# Apple Silicon refuses to run unsigned arm64 code, so every executable carries at least an ad-hoc
# signature — and it is the linker above that writes it, in Apple's own "linker-signed" form: a
# CodeDirectory with CS_ADHOC|CS_LINKER_SIGNED and no CMS blob, byte for byte the shape Xcode's ld
# produces. That signature is what the kernel checks before it starts the process.
#
# Sealing the *bundle* (so that Info.plist and the resources are part of one signature) is what
# `codesign` does when a certificate is present. rcodesign can do it on Linux, but it writes an empty
# CMS blob into the signature slot, which is not what Apple's own ad-hoc signature looks like; on a
# machine we cannot test on, the safer choice is the signature shape Apple itself produces. Sign
# fully on the Mac instead — one command, in README_MACOS.md:
#     codesign --force --deep --sign - "Project Life.app"
if [ -n "$RCodesign" ] && [ -x "$RCodesign" ] && [ -n "$SIGN_BUNDLE" ]; then
    say "signing the bundle with rcodesign"
    "$RCodesign" sign "$APP" || echo "rcodesign could not sign the bundle; the per-binary signatures remain"
else
    say "signatures: linker-signed (ad-hoc) per executable; bundle sealing is left to codesign on the Mac"
fi

# ------------------------------------------------------------------ 7. report
say "done: $APP"
printf 'executables:\n'
for f in "$APP/Contents/MacOS/ProjectLife" "$APP/Contents/Resources/projectlife" "$APP/Contents/Resources/projectlife-ui"; do
    printf '  %s  %s\n' "$(basename "$f")" "$(file -b "$f")"
done
printf '\nopen it with:\n  open "%s"\n' "$APP"
