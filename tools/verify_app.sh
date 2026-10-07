#!/bin/sh
# Every check of the desktop app, in one command.
#
#   sh tools/verify_app.sh [--app "<path to Project Life.app>"]
#
# It runs, in order:
#   1. the app crate's own unit tests,
#   2. the end-to-end scenario through the real interface in a real browser (separate store),
#   3. the same interface bytes inside the arm64 binary and inside the tested one,
#   4. the built bundle: layout, Info.plist, arm64 Mach-O, libraries, signatures over every page.
#
# Exit code 0 only if all of them pass.

set -e
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/.." && pwd)
APP="$ROOT/dist/Project Life.app"
if [ "$1" = "--app" ] && [ -n "$2" ]; then APP="$2"; fi
WORK=${APP_WORK:-/tmp/pl-app-verify}
mkdir -p "$WORK"

say() { printf '\n== %s\n' "$1"; }
fail=0

say "1. the app crate's own tests"
( cd "$ROOT/app" && cargo test --release ) || fail=1

say "2. the steps through the window (real browser, separate store)"
( cd "$ROOT" && python3 tools/ui_e2e.py \
    --app "$ROOT/app/target/release/projectlife-ui" --pl "$ROOT/target/release/projectlife" \
    --work "$WORK/e2e" ) || fail=1

# The harness has to be able to go red. Until round 297 it could not: its __main__ threw the exit
# code away, so a failing step left the pipeline's status at 0. Here the same harness is asked to
# start an app that does not exist, and must return non-zero.
say "2b. the acceptance harness itself can fail"
if ( cd "$ROOT" && python3 tools/ui_e2e.py --app "$ROOT/app/target/release/does-not-exist" \
        --pl "$ROOT/target/release/projectlife" --work "$WORK/e2e-control" >/dev/null 2>&1 ); then
    echo "FAIL: the harness reported success while it could not start anything"
    fail=1
else
    echo "PASS: with no runnable app the harness exits non-zero"
fi

say "2c. the window's contract, and the proof that the checker can fail"
python3 "$HERE/ui_surface_check.py" --root "$ROOT" || fail=1
python3 "$HERE/ui_surface_control.py" --root "$ROOT" || fail=1

say "2d. the moment field, as a Mac without a date picker would use it"
python3 "$HERE/moment_input_check.py" --ui "$ROOT/app/ui/app.js" || fail=1

say "2e. every core command is in the window or named, with a reason, as CLI-only"
python3 "$HERE/ui_coverage.py" --pl "$ROOT/target/release/projectlife" --app-src "$ROOT/app" \
    --out "$ROOT/docs/UI_COVERAGE.md" || fail=1

say "2f. the menu: the contract, and the proof that the checker can fail"
python3 "$HERE/menu_contract_check.py" --root "$ROOT" --static-only || fail=1
python3 "$HERE/menu_control.py" --root "$ROOT" || fail=1

say "2g. the menu: every entry driven against the real server (the checker, live)"
python3 "$HERE/menu_contract_check.py" --root "$ROOT" \
    --pl "$ROOT/target/release/projectlife" --app-bin "$ROOT/app/target/release/projectlife-ui" \
    --work "$WORK/menu" || fail=1

say "3. the interface inside the binaries is the interface in the sources"
if [ -f "$APP/Contents/Resources/projectlife-ui" ]; then
    python3 "$HERE/embedded_ui_check.py" "$APP/Contents/Resources/projectlife-ui" \
        "$ROOT/app/target/release/projectlife-ui" || fail=1
else
    echo "SKIP: no built app at $APP"
fi

say "4. the built bundle"
if [ -d "$APP" ]; then
    python3 "$HERE/verify_macos_app.py" "$APP" || fail=1
    for f in "$APP/Contents/MacOS/ProjectLife" "$APP/Contents/Resources/projectlife" \
             "$APP/Contents/Resources/projectlife-ui"; do
        [ -f "$f" ] || continue
        python3 "$HERE/macho_signature_check.py" "$f" >/dev/null || { echo "FAIL: signature of $(basename "$f")"; fail=1; }
        printf '  signature of %-16s re-derived and matched\n' "$(basename "$f")"
    done
else
    echo "SKIP: no built app at $APP"
fi

say "result"
if [ "$fail" = "0" ]; then
    echo "VERIFY_APP: ALL CHECKS PASSED"
else
    echo "VERIFY_APP: SOMETHING FAILED"
fi
exit $fail
