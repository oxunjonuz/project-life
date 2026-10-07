#!/bin/sh
# Independent verification of Project Life.
#
# The checks are deliberately made not to share a blind spot with the program:
#   * the archive is read back by tools/recover.py — a separate Python implementation written from
#     the storage format description, sharing no code with the program;
#   * the restored tree is compared byte for byte with the original files, and the two independent
#     restores (Rust and Python) are compared with each other;
#   * corruption is injected by hand into a blob: the program must refuse, and recover.py must see it;
#   * an export with a corrupted blob must fail, and no deletion may follow it;
#   * the crash test kills the process with SIGKILL in the middle of the initial copy and then
#     requires the journal to stay readable and the run to resume.
#
# Usage: sh tools/verify.sh [work-dir]

set -u

BIN=${PROJECTLIFE_BIN:-/work/projectlife/target/release/projectlife}
PY=${PROJECTLIFE_PY:-python3}
TOOLS=$(cd "$(dirname "$0")" && pwd)
WORK=${1:-/work/projectlife/tmp/verify}
ARCH="$WORK/archive"
PROJ="$WORK/project"
FAILED=0
STEP=0

demo_dir() {
  for d in "$ARCH"/projects/*/; do
    n=$(python3 -c "import json,sys;print(json.load(open(sys.argv[1])).get('name',''))" "$d/project.json" 2>/dev/null)
    if [ "$n" = "demo" ]; then printf '%s' "$d"; return; fi
  done
}

say()  { printf '%s\n' "$*"; }
step() { STEP=$((STEP + 1)); printf '\n== %s. %s\n' "$STEP" "$*"; }
ok()   { printf '   PASS: %s\n' "$*"; }
bad()  { printf '   FAIL: %s\n' "$*"; FAILED=$((FAILED + 1)); }
need() { if [ "$1" = "0" ]; then ok "$2"; else bad "$2"; fi; }

[ -x "$BIN" ] || { say "no binary at $BIN (build with: cargo build --release)"; exit 1; }

rm -rf "$WORK"
mkdir -p "$WORK"
export PROJECTLIFE_HOME="$WORK/home"
mkdir -p "$PROJECTLIFE_HOME"

step "Building a project with awkward content"
mkdir -p "$PROJ/src/lib" "$PROJ/docs" "$PROJ/node_modules/dep" "$PROJ/dir with space"
i=0
while [ $i -lt 40 ]; do
  printf 'file %s\nline two\n' "$i" > "$PROJ/src/lib/mod$i.ts"
  i=$((i + 1))
done
printf 'CRLF\r\nline\r\n'          > "$PROJ/src/crlf.txt"
printf '\357\273\277BOM content\n' > "$PROJ/src/bom.txt"
: > "$PROJ/src/empty.txt"
printf 'unicode\n'                 > "$PROJ/dir with space/файл-имя.txt"
printf 'SECRET=1\n'                > "$PROJ/.env"
printf 'dependency\n'              > "$PROJ/node_modules/dep/index.js"
printf 'docs v1\n'                 > "$PROJ/docs/guide.md"
head -c 4000 /dev/urandom          > "$PROJ/blob.png"
mkdir -p "$WORK/outside" && printf 'outside target\n' > "$WORK/outside/target.txt"
ln -s ../src/empty.txt "$PROJ/link_inside" 2>/dev/null || true
ln -s "$WORK/outside/target.txt" "$PROJ/link_outside" 2>/dev/null || true
say "   project files (excluding node_modules): $(find "$PROJ" -type f -not -path '*/node_modules/*' | wc -l | tr -d ' ')"

step "init-archive + add (one initial snapshot, with confirmation)"
"$BIN" init-archive "$ARCH" >/dev/null 2>&1
need $? "init-archive"
"$BIN" --archive "$ARCH" config set stopFreePercent 0 >/dev/null
"$BIN" --archive "$ARCH" config set warnFreePercent 0 >/dev/null
"$BIN" --archive "$ARCH" add "$PROJ" --name demo --yes > "$WORK/add.txt" 2>&1
need $? "add with initial snapshot"
grep -q 'initial snapshot' "$WORK/add.txt" || bad "add did not report the initial snapshot"
grep -q 'secret' "$WORK/add.txt" || bad "add did not report the skipped secret"

step "Changes: modify, rename, delete, create"
printf 'file 0 changed\n' > "$PROJ/src/lib/mod0.ts"
mv "$PROJ/src/lib/mod1.ts" "$PROJ/src/lib/mod1-renamed.ts"
rm "$PROJ/docs/guide.md"
printf 'brand new\n'      > "$PROJ/src/new.txt"
"$BIN" --archive "$ARCH" scan-once demo > "$WORK/scan.txt" 2>&1
need $? "scan-once"
grep -qE 'moved [1-9]' "$WORK/scan.txt" && ok "the rename was recognised as a move (no new blob)" || bad "no move detected for the rename"
"$BIN" --archive "$ARCH" log demo --type move 2>/dev/null | grep -q 'mod1.ts -> src/lib/mod1-renamed.ts' \
  && ok "the journal records the move as from -> to" || bad "no move event in the journal"

step "Rust restore vs Python recover.py export-tree"
"$BIN" --archive "$ARCH" restore demo --at now --to "$WORK/rust-out" --yes > "$WORK/rust-restore.txt" 2>&1
need $? "restore via the program"
"$PY" "$TOOLS/recover.py" --archive "$ARCH" export-tree --project demo --at now --out "$WORK/py-out" > "$WORK/py-restore.txt" 2>&1
need $? "export-tree via recover.py"
if diff -r "$WORK/rust-out" "$WORK/py-out" > "$WORK/diff.txt" 2>&1; then
  ok "the two independent restores are byte-identical"
else
  bad "the restores differ:"; sed -n '1,20p' "$WORK/diff.txt"
fi

step "Restored bytes vs the original files"
MISMATCH=0
for f in src/lib/mod0.ts src/lib/mod1-renamed.ts src/crlf.txt src/bom.txt src/empty.txt src/new.txt; do
  cmp -s "$PROJ/$f" "$WORK/rust-out/$f" || { bad "byte mismatch: $f"; MISMATCH=1; }
done
[ $MISMATCH -eq 0 ] && ok "every checked file matches the original byte for byte"

step "Drill: the program tests its own promise on this machine"
"$BIN" --archive "$ARCH" drill demo --json > "$WORK/drill.json" 2>&1
need $? "drill verdict (exit code)"
grep -q '"verdict": "PASS"' "$WORK/drill.json" && ok "drill verdict: PASS (byte-for-byte, with the measured time inside)" || { bad "drill did not pass"; sed -n '1,25p' "$WORK/drill.json"; }

step "What must NOT be in the archive: secrets, dependencies, binaries, symlink targets"
FOUND=0
for needle in 'SECRET=1' 'dependency' 'outside target'; do
  if [ -n "$(find "$ARCH/projects" -path '*blobs*' -type f -exec grep -l "$needle" {} \; 2>/dev/null | head -1)" ]; then
    bad "found forbidden content in the archive: $needle"; FOUND=1
  fi
done
[ $FOUND -eq 0 ] && ok "none of the forbidden contents reached the blob store"
"$PY" "$TOOLS/recover.py" --archive "$ARCH" events --project demo --limit 400 > "$WORK/events.txt" 2>&1
grep -q '"reason": "secret"' "$WORK/events.txt" && ok "the secret was skipped with a stated reason" || bad "no skip event for the secret"
grep -q '"type": "symlink"' "$WORK/events.txt" && ok "the symlink was recorded as an event" || bad "no symlink event"

step "Integrity: both implementations agree the archive is intact"
"$BIN" --archive "$ARCH" check demo --deep > "$WORK/check-rust.txt" 2>&1
need $? "program deep check"
"$PY" "$TOOLS/recover.py" --archive "$ARCH" check --project demo --deep > "$WORK/check-py.txt" 2>&1
need $? "recover.py deep check"

step "Export, manifest check, import into a new project"
"$BIN" --archive "$ARCH" export demo --out "$WORK/export" > "$WORK/export.txt" 2>&1
need $? "export"
( cd "$WORK/export" && sha256sum -c MANIFEST.sha256 > "$WORK/manifest.txt" 2>&1 )
need $? "sha256sum -c MANIFEST.sha256 inside the export"
"$BIN" --archive "$ARCH" import "$WORK/export" --new --name imported > "$WORK/import.txt" 2>&1
need $? "import into a new project"
"$PY" "$TOOLS/recover.py" --archive "$ARCH" check --project imported --deep > "$WORK/import-check.txt" 2>&1
need $? "recover.py agrees with the imported project"

step "Corruption: a blob of this project is damaged by hand"
DEMO_DIR=$(demo_dir)
BLOB=$(find "$DEMO_DIR/blobs" -type f | head -1)
say "   damaging a blob that project demo really references"
cp "$BLOB" "$WORK/blob.before"
chmod 644 "$BLOB"
printf 'X' | dd of="$BLOB" bs=1 seek=0 conv=notrunc 2>/dev/null
chmod 444 "$BLOB"
"$BIN" --archive "$ARCH" restore demo --at now --to "$WORK/corrupt-out" --yes > "$WORK/corrupt-restore.txt" 2>&1
CORRUPT_RC=$?
"$PY" "$TOOLS/recover.py" --archive "$ARCH" check --project demo --deep > "$WORK/corrupt-check-py.txt" 2>&1
PY_CORRUPT=$?
grep -q 'CORRUPTED BLOBS' "$WORK/corrupt-check-py.txt" && ok "recover.py detected the damaged blob" || bad "recover.py missed the damaged blob"
[ $CORRUPT_RC -eq 2 ] && ok "the program reported a partial restore (exit 2)" || bad "expected exit 2 for a partial restore, got $CORRUPT_RC"
[ $PY_CORRUPT -ne 0 ] && ok "recover.py exits non-zero on a corrupted archive" || bad "recover.py exited 0 with a corrupted blob"
grep -q 'could not be restored' "$WORK/corrupt-restore.txt" && ok "the affected file was reported, not silently written" || bad "the affected file was not reported"

step "An export with a corrupted blob must fail, and nothing may be deleted"
say "   (the blob damaged above belongs to demo, so the export of demo cannot verify)"
# Make sure the export range really contains observations: without this wait the range can be empty
# and the step would prove nothing while appearing to pass.
sleep 3
VERSIONS_BEFORE=$("$BIN" --archive "$ARCH" check demo 2>/dev/null | head -1 | awk '{print $2}')
"$BIN" --archive "$ARCH" export-and-prune demo --before "2 seconds ago" --out "$WORK/exp-bad" --yes > "$WORK/exp-bad.txt" 2>&1
BAD_RC=$?
say "   export-and-prune exit code: $BAD_RC"
grep -qi 'export' "$WORK/exp-bad.txt" && ok "the chain reported the export step" || bad "no export message in the output"
if grep -q 'export verified (0 blobs' "$WORK/exp-bad.txt"; then
  bad "the export range was empty: this step did not exercise a corrupted blob"
elif [ -f "$WORK/exp-bad/EXPORT_FAILED.txt" ]; then
  ok "the export stopped at the corrupted blob and is marked EXPORT_FAILED.txt"
else
  bad "the failed export was not marked"
fi
VERSIONS_AFTER=$("$BIN" --archive "$ARCH" check demo 2>/dev/null | head -1 | awk '{print $2}')
if [ "$VERSIONS_BEFORE" = "$VERSIONS_AFTER" ]; then
  ok "no history was deleted after a failed export ($VERSIONS_AFTER versions)"
else
  bad "versions changed from $VERSIONS_BEFORE to $VERSIONS_AFTER after a failed export"
fi

step "Prune keeps the boundary state"
FILES_BEFORE=$("$BIN" --archive "$ARCH" tree demo --at now 2>/dev/null | tail -1 | awk '{print $1}')
"$BIN" --archive "$ARCH" prune demo --before now --yes > "$WORK/prune.txt" 2>&1
PRUNE_RC=$?
[ $PRUNE_RC -eq 0 ] && ok "prune completed" || { bad "prune failed (exit $PRUNE_RC)"; sed -n '1,10p' "$WORK/prune.txt"; }
FILES_AFTER=$("$BIN" --archive "$ARCH" tree demo --at now 2>/dev/null | tail -1 | awk '{print $1}')
say "   files known before prune: ${FILES_BEFORE:-?}, after: ${FILES_AFTER:-?}"
if [ -n "$FILES_AFTER" ] && [ "$FILES_AFTER" != "0" ]; then ok "the current state is still restorable after pruning"; else bad "pruning lost the current state"; fi
"$BIN" --archive "$ARCH" prune demo --before "1 hour ago" --yes > "$WORK/prune-old.txt" 2>&1
if [ $? -ne 0 ]; then ok "pruning before the history boundary is refused with an explanation"; else bad "an out-of-range prune was accepted"; fi

step "audit-archive notices a change made inside the archive"
"$BIN" --archive "$ARCH" audit-archive --update > /dev/null 2>&1
VICTIM=$(find "$ARCH"/projects -path '*blobs*' -type f | tail -1)
chmod 644 "$VICTIM"
printf 'Y' | dd of="$VICTIM" bs=1 seek=0 conv=notrunc 2>/dev/null
chmod 444 "$VICTIM"
"$BIN" --archive "$ARCH" audit-archive > "$WORK/audit.txt" 2>&1
grep -q 'CHANGED inside the archive' "$WORK/audit.txt" && ok "the digest reported the replaced data" || { bad "the digest did not report the change"; sed -n '1,10p' "$WORK/audit.txt"; }

step "External timer mode: cycles without a daemon"
"$BIN" --archive "$ARCH" scan-once --all > "$WORK/timer1.txt" 2>&1
need $? "scan-once --all"
sleep 1
"$BIN" --archive "$ARCH" scan-once --all > "$WORK/timer2.txt" 2>&1
need $? "second scan-once --all"
[ -f "$ARCH/heartbeat" ] && ok "heartbeat written by scan-once" || bad "no heartbeat file"
"$BIN" --archive "$ARCH" status demo --risk > "$WORK/status.txt" 2>&1
grep -q 'Observed window' "$WORK/status.txt" && ok "status reports the measured observation window" || bad "status does not report the measured window"
"$BIN" --archive "$ARCH" doctor > "$WORK/doctor.txt" 2>&1
if grep -q '^ERROR' "$WORK/doctor.txt"; then bad "doctor reports errors:"; sed -n '1,10p' "$WORK/doctor.txt"; else ok "doctor reports no errors"; fi

step "The lock is released after a cycle"
"$BIN" --archive "$ARCH" scan-once demo > "$WORK/lock1.txt" 2>&1 &
LOCK_PID=$!
sleep 0.05
"$BIN" --archive "$ARCH" scan-once demo > "$WORK/lock2.txt" 2>&1
LOCK_RC=$?
wait $LOCK_PID 2>/dev/null
say "   a concurrent cycle exited with $LOCK_RC (1 would mean the lock refused it; both cycles can also finish fast enough not to overlap)"
[ -f "$ARCH/.lock" ] && bad "the lock was left behind" || ok "no lock is left behind"

step "Crash test: SIGKILL in the middle of an initial copy"
BIG="$WORK/big"
mkdir -p "$BIG/sub"
i=0
while [ $i -lt 900 ]; do
  head -c 4096 /dev/urandom > "$BIG/sub/f$i.bin"
  printf 'text %s\n' "$i"        > "$BIG/sub/t$i.txt"
  i=$((i + 1))
done
say "   created $(find "$BIG" -type f | wc -l | tr -d ' ') files"
PROJECTS_BEFORE=$(ls "$ARCH/projects" | wc -l | tr -d ' ')
"$BIN" --archive "$ARCH" add "$BIG" --name big --yes --profile all > "$WORK/big-add.txt" 2>&1 &
ADD_PID=$!
i=0
while [ $i -lt 200 ]; do
  NOW=$(ls "$ARCH/projects" | wc -l | tr -d ' ')
  if [ "$NOW" -gt "$PROJECTS_BEFORE" ]; then break; fi
  if ! kill -0 "$ADD_PID" 2>/dev/null; then break; fi
  sleep 0.05
  i=$((i + 1))
done
sleep 0.15
kill -9 "$ADD_PID" 2>/dev/null
wait "$ADD_PID" 2>/dev/null
rm -f "$ARCH/.lock"
say "   killed the initial copy with SIGKILL after $i polls"
if [ "$(ls "$ARCH/projects" | wc -l | tr -d ' ')" -gt "$PROJECTS_BEFORE" ]; then
  ok "the crash landed after the project was registered (a real half-copy)"
  "$BIN" --archive "$ARCH" check big > "$WORK/crash-check.txt" 2>&1
  need $? "the journal is readable and consistent right after SIGKILL"
  say "   project state after the crash: $("$BIN" --archive "$ARCH" status big 2>/dev/null | sed -n 's/^State: *//p')"
else
  bad "the project was never registered: the crash did not land where it was meant to"
fi
"$BIN" --archive "$ARCH" add "$BIG" --name big --yes --profile all > "$WORK/big-add2.txt" 2>&1
need $? "the interrupted initial copy resumed"
"$BIN" --archive "$ARCH" scan-once big > "$WORK/big-scan.txt" 2>&1
need $? "a normal cycle after the crash"
"$BIN" --archive "$ARCH" check big --deep > "$WORK/crash-check2.txt" 2>&1
need $? "deep check after the resumed copy"
"$PY" "$TOOLS/recover.py" --archive "$ARCH" check --project big --deep > "$WORK/crash-check-py.txt" 2>&1
need $? "recover.py agrees after the crash"

step "Symlinks: a link is a link (no content, no blob, skip into the archive)"
SECRET="$WORK/outside-secret.txt"
printf 'TOP-SECRET-OUTSIDE-42\n' > "$SECRET"
ln -sf "$SECRET" "$PROJ/link_to_secret"
ln -sf "$ARCH/config.json" "$PROJ/link_to_archive"
"$BIN" --archive "$ARCH" scan-once demo > "$WORK/symlink-scan.txt" 2>&1
need $? "a cycle with two symlinks in the project"
# Journal objects are written with sorted keys, so the path comes before the type on the line.
if grep -q '"path":"link_to_secret".*"type":"symlink"' "$ARCH"/projects/*/events/*.jsonl; then
  ok "the outside symlink is recorded as a symlink event"
else
  bad "no symlink event for link_to_secret"
fi
if grep -q '"reason":"symlink_to_archive"' "$ARCH"/projects/*/events/*.jsonl; then
  ok "the archive symlink is recorded as skipped (symlink_to_archive)"
else
  bad "the archive symlink was not skipped"
fi
if grep -rq 'TOP-SECRET-OUTSIDE' "$ARCH/projects"/*/blobs 2>/dev/null; then
  bad "the symlink target's bytes are inside the archive"
else
  ok "the symlink target's bytes are in no blob"
fi
if grep -q '"path":"link_to_secret".*"type":"put"' "$ARCH"/projects/*/events/*.jsonl; then
  bad "the symlink was stored as a version of itself"
else
  ok "no version event was written for the symlink"
fi
"$PY" "$TOOLS/recover.py" --archive "$ARCH" check --project demo > "$WORK/symlink-py.txt" 2>&1
need $? "recover.py agrees the archive is intact with symlinks in the project"
rm -f "$PROJ/link_to_secret" "$PROJ/link_to_archive"

step "Renames: one move event per file, no version for unchanged contents"
mkdir -p "$PROJ/mvdir"
printf 'rename me\n' > "$PROJ/mvdir/one.txt"
printf 'and me\n' > "$PROJ/mvdir/two.txt"
"$BIN" --archive "$ARCH" scan-once demo > "$WORK/mv0.txt" 2>&1
mv "$PROJ/mvdir" "$PROJ/mvdir_renamed"
"$BIN" --archive "$ARCH" scan-once demo > "$WORK/mv1.txt" 2>&1
need $? "a cycle after renaming a folder with two files"
DEMO_DIR2=$(demo_dir)
MOVES=$(cat "$DEMO_DIR2"/events/*.jsonl | grep -c '"type":"move"')
if [ "$MOVES" -ge 2 ]; then ok "moves are written for a renamed folder ($MOVES move events)"; else bad "expected move events in demo, found $MOVES"; fi
# After a rename the old path must appear nowhere as the subject of an event: a delete event would
# carry it in a "path" field (the put that first stored the file legitimately has that path too, so
# the check is on the event type, not on the bare path).
if cat "$DEMO_DIR2"/events/*.jsonl | grep -q '"path":"mvdir/one.txt".*"type":"delete"'; then
  bad "the renamed file's old path was reported as deleted"
else
  ok "no delete was written for the renamed file"
fi
BATCHES=$(cat "$DEMO_DIR2"/events/*.jsonl | grep '"type":"move"' | sed -n 's/.*"batchId":"\([^"]*\)".*/\1/p' | sort -u | tail -1)
if [ -n "$BATCHES" ]; then ok "moves carry a batch id ($BATCHES)"; else bad "no batch id on the move events"; fi
rm -rf "$PROJ/mvdir_renamed"

step "An interrupted prune is recovered, not left half-done"
# A project of its own: the deliberate blob damage that verify.sh inflicts on `demo` earlier would
# otherwise be measured here, and this step is about the prune.
PRUNEPROJ="$WORK/pruneproj"
mkdir -p "$PRUNEPROJ"
printf 'first\n' > "$PRUNEPROJ/p.txt"
printf 'keep\n' > "$PRUNEPROJ/q.txt"
"$BIN" --archive "$ARCH" add "$PRUNEPROJ" --name prunetest --yes > "$WORK/prune-add.txt" 2>&1
need $? "a project for the prune crash test"
sleep 1
printf 'second\n' > "$PRUNEPROJ/p.txt"
"$BIN" --archive "$ARCH" scan-once prunetest > /dev/null 2>&1
BOUNDARY=$(date +%s%3N)
sleep 1
printf 'third\n' > "$PRUNEPROJ/p.txt"
"$BIN" --archive "$ARCH" scan-once prunetest > /dev/null 2>&1
PROJECTLIFE_CRASH_AFTER=journal_swapped "$BIN" --archive "$ARCH" prune prunetest --before "$BOUNDARY" --yes > "$WORK/prune-crash.txt" 2>&1
RC=$?
if [ $RC -gt 128 ] || [ $RC -eq 137 ]; then ok "the prune process was killed by a signal (rc=$RC)"; else bad "the crash point did not kill the process (rc=$RC)"; fi
if find "$ARCH"/projects -name prune.journal | grep -q .; then ok "the phase file survived the crash"; else bad "no prune.journal after the crash"; fi
"$BIN" --archive "$ARCH" recover prunetest > "$WORK/recover.txt" 2>&1
need $? "recover finishes or rolls back the interrupted prune"
if find "$ARCH"/projects -name prune.journal | grep -q .; then bad "prune.journal is still there after recovery"; else ok "the phase file is gone after recovery"; fi
"$BIN" --archive "$ARCH" check prunetest --deep > "$WORK/post-prune-check.txt" 2>&1
need $? "deep check after the recovered prune"
if grep -q 'dangling' "$WORK/post-prune-check.txt"; then bad "the recovered prune left unreferenced blobs behind"; else ok "no unreferenced blob is left after recovery"; fi
"$PY" "$TOOLS/recover.py" --archive "$ARCH" check --project prunetest --deep > "$WORK/post-prune-py.txt" 2>&1
need $? "recover.py agrees after the recovered prune"
# The current state must have survived: restore the project as it is now and compare byte for byte.
"$BIN" --archive "$ARCH" restore prunetest --at now --to "$WORK/prune-restored" --yes > "$WORK/prune-restore.txt" 2>&1
need $? "the current state is restorable after the interrupted-and-recovered prune"
if cmp -s "$PRUNEPROJ/p.txt" "$WORK/prune-restored/p.txt"; then ok "the recovered history holds the current bytes of p.txt"; else bad "p.txt differs after the recovered prune"; fi
"$BIN" --archive "$ARCH" restore prunetest --at "$BOUNDARY" --to "$WORK/prune-boundary" --yes > "$WORK/prune-boundary.txt" 2>&1
need $? "the moment of the interrupted prune is still restorable"
if [ "$(cat "$WORK/prune-boundary/p.txt" 2>/dev/null)" = "second" ]; then ok "the boundary moment restores the contents it had"; else bad "the boundary moment restored '$(
cat "$WORK/prune-boundary/p.txt" 2>/dev/null)' instead of 'second'"; fi

step "A stale lock is taken over; a live one is not"
rm -f "$ARCH/.lock"
sleep 0 &
DEAD=$!
wait "$DEAD" 2>/dev/null
printf 'cycle %s %s\n' "$DEAD" "$(date +%s%3N)" > "$ARCH/.lock"
"$BIN" --archive "$ARCH" scan-once demo > "$WORK/lock-stale.txt" 2>&1
need $? "a cycle takes over a lock whose owner is gone"
printf 'cycle %s %s\n' "$$" "$(date +%s%3N)" > "$ARCH/.lock"
"$BIN" --archive "$ARCH" scan-once demo > "$WORK/lock-live.txt" 2>&1
if [ $? -ne 0 ]; then ok "a cycle refuses to displace a live holder (this shell)"; else bad "a live lock was stolen"; fi
"$BIN" --archive "$ARCH" doctor --fix-lock > "$WORK/lock-fix-refused.txt" 2>&1
if [ $? -ne 0 ]; then ok "doctor --fix-lock refuses to remove a live holder's lock"; else bad "doctor --fix-lock removed a live lock"; fi
printf 'cycle %s %s\n' "$DEAD" "$(date +%s%3N)" > "$ARCH/.lock"
"$BIN" --archive "$ARCH" doctor --fix-lock > "$WORK/lock-fix.txt" 2>&1
need $? "doctor --fix-lock removes a lock whose owner is gone"
[ -f "$ARCH/.lock" ] && bad "the stale lock file is still there" || ok "the stale lock file is gone"
rm -f "$ARCH/.lock"

step "Counting skipped directories costs time; the ordinary cycle does not pay it"
mkdir -p "$PROJ/node_modules/pkg"
i=0
while [ $i -lt 5000 ]; do : > "$PROJ/node_modules/pkg/d$i.js"; i=$((i + 1)); done
"$BIN" --archive "$ARCH" scan-once demo > "$WORK/count-plain.txt" 2>&1
need $? "an ordinary cycle with 5000 files in node_modules"
T0=$(date +%s%3N)
"$BIN" --archive "$ARCH" scan-once demo > "$WORK/count-plain2.txt" 2>&1
T1=$(date +%s%3N)
ORD=$((T1 - T0))
T0=$(date +%s%3N)
"$BIN" --archive "$ARCH" scan-once demo --skipped > "$WORK/count-skipped.txt" 2>&1
T1=$(date +%s%3N)
CNT=$((T1 - T0))
say "   ordinary cycle ${ORD} ms; with --skipped ${CNT} ms"
if grep -qE 'ignored_dir\] node_modules \([0-9]+ files\)' "$WORK/count-skipped.txt"; then
  ok "--skipped reports the counted size of the skipped directory ($(grep -oE 'node_modules \([0-9]+ files\)' "$WORK/count-skipped.txt" | head -1))"
else
  bad "--skipped did not report the file count"
fi
if grep -q 'files)' "$WORK/count-plain2.txt"; then
  bad "the ordinary cycle reported a count for a skipped directory"
else
  ok "the ordinary cycle reported the skip without counting it"
fi
if [ "$ORD" -lt 1000 ]; then ok "the ordinary cycle stays under a second (${ORD} ms)"; else bad "the ordinary cycle took ${ORD} ms"; fi
if [ "$CNT" -gt "$ORD" ]; then ok "counting is measurably slower than not counting (${CNT} ms vs ${ORD} ms)"; else bad "counting and not counting are indistinguishable here"; fi
rm -rf "$PROJ/node_modules"

step "NFR-PRF on a built-from-scratch 10 000-file project (numbers land in LIMITATIONS.md)"
"$PY" "$TOOLS/perf.py" --bin "$BIN" --work "$WORK/perf" > "$WORK/perf.json" 2>&1
need $? "the performance measurement ran"
MED=$(sed -n 's/.*"ordinary_median_seconds": \([0-9.]*\).*/\1/p' "$WORK/perf.json" | head -1)
say "   10 000 unchanged files: median ordinary cycle ${MED} s (promise: <= 1 s)"
if [ -n "$MED" ]; then
  if [ "$(python3 -c "print(1 if float('$MED') <= 1.0 else 0)")" = "1" ]; then ok "NFR-PRF-2 holds on this machine"; else bad "NFR-PRF-2 violated on this machine (${MED} s)"; fi
else
  bad "no median found in the performance output"
fi

step "The filesystem-notification trigger: measured latency with and without it"
# The point of this step is a NUMBER, not a claim: the same change is made in the same fixture with
# triggers on and off, and the two distributions are compared. With triggers the latency must sit at
# the debounce and must not grow with the interval; without them it must show the interval itself.
if [ "$(uname -s)" = "Linux" ]; then
  "$PY" "$TOOLS/watch_latency.py" --bin "$BIN" --work "$WORK/latency" --quick --no-slow > "$WORK/latency.txt" 2>&1
  need $? "the latency measurement ran"
  grep -E '^[0-9]+s-' "$WORK/latency.txt" | while read -r line; do say "   $line"; done
  LJSON="$WORK/latency/latency.json"
  check=$(python3 - "$LJSON" <<'PYEOF'
import json, sys
d = json.load(open(sys.argv[1]))
ph = {p["label"]: p for p in d["phases"]}
def need(label):
    if label not in ph:
        print("MISSING %s" % label); raise SystemExit(0)
    return ph[label]
bad = []
p5 = need("5s-periodic-only")
n5p = need("5s-notify-partial"); n5f = need("5s-notify-full")
n30p = need("30s-notify-partial")
if n5p["latency_max_ms"] is None or n5p["latency_max_ms"] > 4000:
    bad.append("with notifications at 5 s the slowest change took %s ms (debounce 1500, interval 5000): it must beat the interval" % n5p["latency_max_ms"])
if n5f["latency_max_ms"] is None or n5f["latency_max_ms"] > 4000:
    bad.append("with notifications and partialPass=false at 5 s the slowest change took %s ms" % n5f["latency_max_ms"])
if p5["latency_max_ms"] is None or p5["latency_max_ms"] < 2500:
    bad.append("without notifications at 5 s no sample waited near the interval (max %s ms): the measurement cannot tell the two modes apart" % p5["latency_max_ms"])
if n30p["latency_median_ms"] is None or n30p["latency_median_ms"] > 3000:
    bad.append("with notifications (partial pass) at 30 s the median latency is %s ms — it must not scale with the interval" % n30p["latency_median_ms"])
iv = p5["observed_interval_ms"]["median"]
if iv is None or not (0.8 * 5000 <= iv <= 1.2 * 5000):
    bad.append("without notifications the measured observation interval is %s ms (configured 5000)" % iv)
# and the counters say which kind of pass stored those versions
tp = n5p["trigger_state"]; tf = n5f["trigger_state"]
if not (tp.get("partialCycles", 0) > 0 and tp.get("triggerCycles") == tp.get("partialCycles")):
    bad.append("at 5 s with partialPass on the notification passes were not all partial: %s" % tp)
if tf.get("partialCycles", 0) != 0 or tf.get("triggerCycles", 0) < 1:
    bad.append("at 5 s with partialPass=false a notification still claimed to be a partial pass: %s" % tf)
print("; ".join(bad) if bad else "OK")
PYEOF
)
  if [ "$check" = "OK" ]; then
    ok "with notifications every change landed within the debounce — partial pass and full pass alike"
    ok "the partial path was the one taken (triggerCycles == partialCycles), and partialPass=false turns it off"
    ok "without notifications the wait reached the interval, and the measured interval matched the configured one"
  else
    bad "$check"
  fi
else
  say "   SKIP (not a pass): this build has no notification backend on $(uname -s), so only the fallback exists"
fi

step "daemon run at the command level: the trigger, the lock, and a second start"
"$BIN" --archive "$ARCH" config set watchTriggers true >/dev/null
"$BIN" --archive "$ARCH" config set autoInterval false >/dev/null
"$BIN" --archive "$ARCH" config set intervalSeconds 60 >/dev/null
"$BIN" --archive "$ARCH" daemon run > "$WORK/daemon.log" 2>&1 &
DPID=$!
for i in $(seq 1 100); do
  [ -f "$ARCH/watch_state.json" ] && grep -q '"watchedDirs": [1-9]' "$ARCH/watch_state.json" && break
  sleep 0.1
done
"$BIN" --archive "$ARCH" daemon status > "$WORK/daemon-status.txt" 2>&1
grep -q 'trigger: inotify' "$WORK/daemon-status.txt" && ok "daemon status reports the trigger" || bad "daemon status says: $(head -3 "$WORK/daemon-status.txt" | tr '\n' ' ')"
printf 'trigger demo change\n' > "$PROJ/docs/guide.md"
T0=$(date +%s%3N)
for i in $(seq 1 200); do
  grep -q 'cycle triggered by filesystem notifications' "$ARCH/logs/projectlife.log" 2>/dev/null && break
  sleep 0.05
done
T1=$(date +%s%3N)
TRIG=$((T1 - T0))
if [ "$TRIG" -lt 8000 ]; then ok "a change started a pass ${TRIG} ms later (interval is 60 s)"; else bad "the trigger took ${TRIG} ms with a 60 s interval"; fi
"$BIN" --archive "$ARCH" daemon run > "$WORK/daemon-second.txt" 2>&1
if [ $? -ne 0 ] && grep -q 'another daemon' "$WORK/daemon-second.txt"; then
  ok "a second daemon start was refused with a clear message"
else
  bad "a second daemon start was not refused: $(head -3 "$WORK/daemon-second.txt" | tr '\n' ' ')"
fi
kill "$DPID" 2>/dev/null
for i in $(seq 1 100); do kill -0 "$DPID" 2>/dev/null || break; sleep 0.1; done
[ -f "$ARCH/.daemon" ] && bad "the daemon lock survived a clean stop" || ok "the daemon lock is released on a clean stop"
"$BIN" --archive "$ARCH" daemon status > "$WORK/daemon-status2.txt" 2>&1
# The heartbeat stays fresh for up to 60 s after a stop by design (FR-WCH-13), so the exact signal
# that no daemon is left is the daemon lock, not "not running".
grep -q 'daemon lock: held by' "$WORK/daemon-status2.txt" && bad "daemon status still reports a live daemon lock" || ok "daemon status reports no daemon holder after the stop"

step "Universal mode: a folder is detected without reading a byte of it"
UNIV="$WORK/universal"
mkdir -p "$UNIV/Documents"
printf 'report\n'   > "$UNIV/Documents/report.docx"
printf 'table\n'    > "$UNIV/Documents/table.xlsx"
printf 'notes\n'    > "$UNIV/Documents/notes.txt"
printf 'slides\n'   > "$UNIV/Documents/slides.pptx"
printf 'SECRET=1\n' > "$UNIV/Documents/.env"
printf 'cache\n'    > "$UNIV/Documents/cache.tmp"
printf 'clip\n'     > "$UNIV/Documents/clip.mp4"
# An old atime on every document: any content read moves it (relatime), nothing else does.
for f in "$UNIV"/Documents/* "$UNIV"/Documents/.env; do touch -a -t 200101010000 "$f"; done
AT_BEFORE=$(stat -c '%X' "$UNIV/Documents/report.docx")
"$BIN" detect "$UNIV/Documents" --json > "$WORK/detect.json" 2>&1
need $? "pl detect runs and prints JSON"
AT_AFTER=$(stat -c '%X' "$UNIV/Documents/report.docx")
[ "$AT_BEFORE" = "$AT_AFTER" ] && ok "detection moved no atime (still $AT_BEFORE)" || bad "detect read report.docx (atime $AT_BEFORE -> $AT_AFTER)"
cat "$UNIV/Documents/table.xlsx" > /dev/null
AT_CTRL=$(stat -c '%X' "$UNIV/Documents/table.xlsx")
[ "$AT_CTRL" != "978307200" ] && ok "control: a real read does move the atime here, so the check above can go red" || bad "atime never moves on this filesystem — the check above proves nothing"
check=$(python3 - "$WORK/detect.json" <<'PYEOF'
import json, sys
d = json.load(open(sys.argv[1]))
bad = []
if d["detectedProfile"] != "office": bad.append("profile is %s" % d["detectedProfile"])
if d["confidence"] < 80: bad.append("confidence %s" % d["confidence"])
inc = d["suggestedInclude"]
for want in ["*.docx", "*.xlsx", "*.txt"]:
    if want not in inc: bad.append("%s not suggested" % want)
for none in ["*.mp4", "*.exe", "*.env", "*.tmp", "*.zip"]:
    if none in inc: bad.append("%s must never be suggested automatically" % none)
ex = [e["path"] for e in d["safetyExcluded"]]
for must in [".env", "cache.tmp"]:
    if must not in ex: bad.append("%s was not reported as excluded" % must)
if not d["suggestCustom"] and d["candidates"][0]["extensionScore"] <= 0:
    bad.append("the best candidate claims nothing")
print("; ".join(bad) if bad else "OK")
PYEOF
)
[ "$check" = "OK" ] && ok "office detected: documents suggested, secret/media/temp never suggested" || bad "$check"
"$BIN" detect /etc > "$WORK/detect-etc.txt" 2>&1
[ $? -ne 0 ] || bad "detecting a system path succeeded — it should never be silent"
"$BIN" --archive "$ARCH" add /etc --preset auto --yes > "$WORK/add-etc.txt" 2>&1
if [ $? -ne 0 ] && grep -q 'system location' "$WORK/add-etc.txt"; then
  ok "adding a system path is refused with a reason"
else
  bad "adding /etc was not refused: $(head -2 "$WORK/add-etc.txt" | tr '\n' ' ')"
fi

step "Universal mode: the preset is written down, and what it says is obeyed"
UNIV2="$WORK/universal2"
mkdir -p "$UNIV2/Documents"
printf 'report\n' > "$UNIV2/Documents/report.docx"
printf 'notes\n'  > "$UNIV2/Documents/notes.txt"
printf 'readme\n' > "$UNIV2/Documents/README.md"
printf 'slides\n' > "$UNIV2/Documents/slides.pptx"
printf 'SECRET=1\n' > "$UNIV2/Documents/.env"
"$BIN" --archive "$ARCH" add "$UNIV2/Documents" --name universal --preset auto --yes --edit-add .md --edit-remove .pptx > "$WORK/add-universal.txt" 2>&1
need $? "add --preset auto with a hand edit"
UNIVDIR=$(python3 - "$ARCH" <<'PYEOF'
import json, glob, os, sys
for p in glob.glob(os.path.join(sys.argv[1], "projects", "*", "project.json")):
    m = json.load(open(p))
    if m.get("name") == "universal":
        print(os.path.dirname(p)); break
PYEOF
)
[ -n "$UNIVDIR" ] && ok "found the project directory written by the run" || bad "no project named universal in the archive"
check=$(python3 - "$UNIVDIR/project.json" <<'PYEOF'
import json, sys
m = json.load(open(sys.argv[1]))
bad = []
p = m.get("preset") or {}
if p.get("id") != "office": bad.append("preset.id is %r" % p.get("id"))
if p.get("source") != "auto": bad.append("preset.source is %r" % p.get("source"))
if m.get("presetSource") != "auto": bad.append("presetSource is %r" % m.get("presetSource"))
if p.get("modifiedByUser") is not True: bad.append("preset.modifiedByUser is %r" % p.get("modifiedByUser"))
if m.get("presetModifiedByUser") is not True: bad.append("presetModifiedByUser is %r" % m.get("presetModifiedByUser"))
if not p.get("evidence", {}).get("extensions"): bad.append("the evidence is empty")
s = m.get("settings") or {}
inc = s.get("include") or []
if "*.md" not in inc: bad.append("*.md missing from include")
if "*.pptx" in inc: bad.append("*.pptx was not removed")
if s.get("filterMode") != "allow": bad.append("filterMode is %r" % s.get("filterMode"))
if s.get("presetId") != "office": bad.append("settings.presetId is %r" % s.get("presetId"))
print("; ".join(bad) if bad else "OK")
PYEOF
)
[ "$check" = "OK" ] && ok "project.json records the preset, the evidence and the hand edit" || bad "$check"
check=$(python3 - "$UNIVDIR" <<'PYEOF'
import glob, json, os, sys
# Read the journal as a journal, not as text: a path that merely appears in a `skip` event was
# NOT stored. (The first version of this check grepped for the path and "found" skips.)
evs = []
for f in glob.glob(os.path.join(sys.argv[1], "events", "*.jsonl")):
    for line in open(f):
        line = line.strip()
        if line:
            evs.append(json.loads(line))
bad = []
puts = set(e["path"] for e in evs if e.get("type") == "put")
if "README.md" not in puts:
    bad.append("README.md was not stored after --edit-add .md")
if "slides.pptx" in puts:
    bad.append("slides.pptx was stored although --edit-remove .pptx took it out")
if ".env" in puts:
    bad.append("the secret was stored")
if not any(e.get("type") == "filters" and e.get("reason") == "preset_applied" for e in evs):
    bad.append("no preset_applied filters event")
if not any(e.get("type") == "skip" and e.get("path") == ".env" and e.get("reason") == "secret" for e in evs):
    bad.append("no skip event showing the secret rule fired")
if not any(e.get("type") == "skip" and e.get("path") == "slides.pptx" and e.get("reason") == "not_in_preset" for e in evs):
    bad.append("no skip event showing the removed extension is left out")
print("; ".join(bad) if bad else "OK")
PYEOF
)
[ "$check" = "OK" ] && ok "journal: preset applied, hand edits obeyed, secret skipped by the hard rule" || bad "$check"
if grep -rq 'SECRET=1' "$UNIVDIR/blobs" 2>/dev/null; then
  bad "secret bytes are in the blob store"
else
  ok "no secret bytes anywhere in the blob store"
fi
if grep -rq 'report' "$UNIVDIR/blobs" 2>/dev/null; then
  ok "control: the same grep does find an ordinary file's content in the blob store"
else
  bad "control failed: the blob store does not contain a stored file, so the grep above proves nothing"
fi
# `--preset custom` is the "these files, exactly" path: it starts from the extensions that are
# actually present (as *.abc, not as a bare name — the first version of this branch wrote names that
# matched nothing and protected zero files).
UNIV3="$WORK/universal3"
mkdir -p "$UNIV3/odd"
printf 'x' > "$UNIV3/odd/file.abc"
printf 'x' > "$UNIV3/odd/file.xyz"
printf 'SECRET=1\n' > "$UNIV3/odd/.env"
"$BIN" --archive "$ARCH" add "$UNIV3/odd" --name universal-custom --preset custom --yes --edit-remove .xyz > "$WORK/add-custom.txt" 2>&1
need $? "add --preset custom with --edit-remove"
CUSTOMDIR=$(python3 - "$ARCH" <<'PYEOF'
import json, glob, os, sys
for p in glob.glob(os.path.join(sys.argv[1], "projects", "*", "project.json")):
    m = json.load(open(p))
    if m.get("name") == "universal-custom":
        print(os.path.dirname(p)); break
PYEOF
)
check=$(python3 - "$CUSTOMDIR" <<'PYEOF'
import glob, json, os, sys
d = sys.argv[1]
m = json.load(open(os.path.join(d, "project.json")))
bad = []
inc = m.get("settings", {}).get("include")
if inc != ["*.abc"]:
    bad.append("include is %r, expected ['*.abc']" % (inc,))
if m.get("preset", {}).get("id") != "custom":
    bad.append("preset id is %r" % m.get("preset", {}).get("id"))
if m.get("presetModifiedByUser") is not True:
    bad.append("presetModifiedByUser is %r" % m.get("presetModifiedByUser"))
puts = set()
skips = {}
for f in glob.glob(os.path.join(d, "events", "*.jsonl")):
    for line in open(f):
        line = line.strip()
        if not line:
            continue
        e = json.loads(line)
        if e.get("type") == "put":
            puts.add(e["path"])
        if e.get("type") == "skip":
            skips[e["path"]] = e.get("reason")
if puts != {"file.abc"}:
    bad.append("stored %r, expected exactly file.abc" % (sorted(puts),))
if skips.get("file.xyz") != "not_in_preset":
    bad.append("file.xyz skip reason is %r" % skips.get("file.xyz"))
if skips.get(".env") != "secret":
    bad.append(".env skip reason is %r" % skips.get(".env"))
print("; ".join(bad) if bad else "OK")
PYEOF
)
[ "$check" = "OK" ] && ok "custom protects exactly the extensions it was told to, and the secret still goes" || bad "$check"


step "Universal mode: pl detect on 100 000 files (the brief's 2 s target)"
"$PY" "$TOOLS/detect_bench.py" --files 100000 --runs 3 > "$WORK/detect-bench.txt" 2>&1
BENCH=$?
sed -n 's/^\(floor on this disk\|median\|target\|cap\|run [0-9]\)/   &/p' "$WORK/detect-bench.txt"
if [ "$BENCH" = "0" ]; then
  ok "every run of pl detect over 100 000 files stayed inside the 2 s target"
elif [ "$BENCH" = "2" ]; then
  ok "the 5 s design cap held, but a run missed the 2 s target — reported, not hidden"
else
  bad "pl detect exceeded the 5 s design cap on 100 000 files"
fi

# The three round-301 steps run *before* the mutation campaign on purpose: the campaign builds many
# copies of the program in a row, and on this machine the memory it leaves behind was enough for the
# kernel to kill the WebKit process in the middle of the application check (measured: step 58 "Killed",
# the check's own log empty). Order is not a matter of taste when one step's leftovers decide whether
# the next one can run at all.
# ---------------------------------------------------------------------------------------------
# Round 301: the Windows and Linux applications.
#
# The three steps below are deliberately unequal in strength, and they say which is which:
#   * the Linux step RUNS the real window (GTK + WebKitGTK) under Xvfb, against a real archive;
#   * the Windows step READS the delivered bytes — there is no Windows here to run anything on;
#   * the cross-platform step compares the interface inside all three builds, byte for byte, which is
#     the one thing that can be checked here about a build that runs somewhere else.
# ---------------------------------------------------------------------------------------------

step "The Linux application: the real window, under Xvfb, against a real archive (round 301)"
# The source folder is derived later in this script for other steps; these four run before that, and
# `set -u` refused a variable that was not set yet the first time they were moved here.
PKGSRC=$(cd "$TOOLS/.." && pwd)

LINUX_APP="$PKGSRC/dist/linux/ProjectLife-linux-$(uname -m)/bin/projectlife-app"
if [ -x "$LINUX_APP" ] && command -v Xvfb >/dev/null 2>&1; then
  ( cd "$PKGSRC" && "$PY" tools/linux_shell_check.py --app "$LINUX_APP" --keep > "$WORK/linux_shell.txt" 2>&1 )
  need $? "the Linux shell passed its own end-to-end check"
  grep -E "^  (PASS|FAIL)" "$WORK/linux_shell.txt" | tail -24 | sed 's/^/   /'
  tail -1 "$WORK/linux_shell.txt" | sed 's/^/   /'
  say "   full log: $WORK/linux_shell.txt"
else
  say "   SKIP (not a pass): no Linux shell built, or no Xvfb here"
  say "   build it with: sh app/linux/build_linux_app.sh"
fi

step "The Windows package: everything that can be checked without Windows (round 301)"
WIN_PKG="$PKGSRC/dist/windows/ProjectLife-windows-x86_64"
if [ -d "$WIN_PKG" ]; then
  ( cd "$PKGSRC" && "$PY" tools/windows_package_check.py --pkg "$WIN_PKG" > "$WORK/win_pkg.txt" 2>&1 )
  need $? "the Windows package passed the checks that do not need Windows"
  grep -E "^  (PASS|FAIL)" "$WORK/win_pkg.txt" | tail -8 | sed 's/^/   /'
  tail -2 "$WORK/win_pkg.txt" | sed 's/^/   /'
  say "   SKIP (not a pass): nothing in that package has been RUN — there is no Windows machine here."
  say "   the checks that need Windows are in the package: verify_windows.ps1"
  say "   full log: $WORK/win_pkg.txt"
else
  say "   SKIP (not a pass): no Windows package built"
fi

step "One interface, three builds: the same page bytes inside every interface server (round 301)"
UI_MAC="$PKGSRC/dist/macos/Project Life.app/Contents/Resources/projectlife-ui"
UI_LINUX="$PKGSRC/dist/linux/ProjectLife-linux-$(uname -m)/bin/projectlife-ui"
UI_WIN="$PKGSRC/dist/windows/ProjectLife-windows-x86_64/pl-ui.exe"
FOUND=0
for f in "$UI_MAC" "$UI_LINUX" "$UI_WIN"; do [ -f "$f" ] && FOUND=$((FOUND + 1)); done
if [ "$FOUND" -eq 3 ]; then
  ( cd "$PKGSRC" && "$PY" tools/embedded_ui_check.py "$UI_MAC" "$UI_LINUX" "$UI_WIN" > "$WORK/ui_three.txt" 2>&1 )
  need $? "the interface bytes are identical in the macOS, Linux and Windows servers"
  cat "$WORK/ui_three.txt" | sed 's/^/   /'
else
  say "   SKIP (not a pass): $FOUND of the 3 built applications are present here"
fi

step "The installable packages: checksums, and names that survive a case-insensitive filesystem"
LINUX_DIR="$PKGSRC/dist/linux"
if [ -f "$LINUX_DIR/tarball.sha256" ] && [ -f "$LINUX_DIR/deb.sha256" ]; then
  ( cd "$LINUX_DIR" && sha256sum -c tarball.sha256 && sha256sum -c deb.sha256 ) > "$WORK/pkgs.txt" 2>&1
  need $? "the tarball and the .deb match the checksums written when they were built"
  sed 's/^/   /' "$WORK/pkgs.txt"
else
  say "   SKIP (not a pass): the Linux packages have not been built (sh tools/make_linux_packages.sh)"
fi
# A package whose names collapse on Windows is a package that loses a program without a word. The
# volume this project is delivered on is case-insensitive (measured), which made this a real trap
# rather than a theoretical one: `casetest.txt` overwrote `CaseTest.txt` in the round that built the
# Windows package, and the shell's own name had to change because of it.
if [ -d "$WIN_PKG" ]; then
  DUP=$(ls "$WIN_PKG" | tr 'A-Z' 'a-z' | sort | uniq -d)
  if [ -z "$DUP" ]; then
    ok "no two files in the Windows package differ only in case ($(ls "$WIN_PKG" | wc -l) files)"
  else
    bad "the Windows package has names that differ only in case: $DUP"
  fi
fi

step "The suite actually bites: one fault at a time (tools/mutations.py)"
# The campaign copy holds deliberately broken code, so it may not live inside the project — a copy
# left in tmp/ was read by the owner's other agent as the current source on 2026-10-06 (FAILURES_297
# F297-12). The tool refuses such a path now; the work tree goes outside the tree it checks.
"$PY" "$TOOLS/mutations.py" --src "$(cd "$TOOLS/.." && pwd)" --work "${PL_MUT_WORK:-/tmp/pl-verify-mut}" > "$WORK/mutations.txt" 2>&1
need $? "every injected fault was caught by its test"
say "   $(sed -n 's/^  every mutation.*/&/p' "$WORK/mutations.txt")"
say "   $(sed -n 's/^  SURVIVORS.*/&/p' "$WORK/mutations.txt")"

step "Windows target: the port compiles"
# The build products go into $WORK: a check must not leave anything behind in the tree it checks
# (the first version of this step put a whole target/ directory inside the delivery package).
SRC_DIR=$(cd "$TOOLS/.." && pwd)
HAD_TARGET=0
[ -d "$SRC_DIR/target" ] && HAD_TARGET=1
if rustup target list --installed 2>/dev/null | grep -q '^x86_64-pc-windows-gnu$'; then
  (cd "$SRC_DIR" && CARGO_TARGET_DIR="$WORK/win-target" cargo check --release --target x86_64-pc-windows-gnu > "$WORK/win-check.txt" 2>&1)
  need $? "cargo check --target x86_64-pc-windows-gnu"
  say "   SKIP (not a pass): compiled for Windows, never executed there"
else
  say "   SKIP: the x86_64-pc-windows-gnu target is not installed here (rustup target add x86_64-pc-windows-gnu)"
fi
if [ "$HAD_TARGET" = "0" ] && [ -d "$SRC_DIR/target" ]; then
  bad "the verification left a build directory behind in $SRC_DIR"
else
  ok "the verification left no build products in the tree it verified"
fi

step "Retention policy: what it keeps stays restorable, and nothing runs by itself"
# The promise of a policy is a set of MOMENTS you can still restore. The comparison below is made
# with tools/journal_state.py, an independent reader written from the storage format, and every
# moment newer than the boundary of the keep-everything window must be identical before and after.
RP="$WORK/retention"
mkdir -p "$RP/proj/src"
printf 'v0\n'  > "$RP/proj/src/app.ts"
printf 'b0\n'  > "$RP/proj/src/other.txt"
"$BIN" --archive "$ARCH" add "$RP/proj" --name retpol --profile source --yes > /dev/null 2>&1
i=1
while [ $i -le 24 ]; do
  printf 'v%s\n' "$i" > "$RP/proj/src/app.ts"
  "$BIN" --archive "$ARCH" scan-once retpol > /dev/null 2>&1
  i=$((i + 1))
done
RPDIR=$("$PY" - "$ARCH" <<'PYEOF'
import json, os, sys
arch = sys.argv[1]
for d in sorted(os.listdir(os.path.join(arch, "projects"))):
    p = os.path.join(arch, "projects", d, "project.json")
    if os.path.isfile(p) and json.load(open(p)).get("name") == "retpol":
        print(os.path.dirname(p))
        break
PYEOF
)
VERSIONS_BEFORE=$("$PY" -c "import json,os,sys;d=sys.argv[1];print(sum(1 for n in os.listdir(os.path.join(d,'events')) for l in open(os.path.join(d,'events',n)) if l.strip() and json.loads(l)['type']=='put'))" "$RPDIR")
"$PY" "$TOOLS/backdate.py" "$RPDIR" 60 > /dev/null
POLICY="7d:all,30d:1/day,365d:1/month"
"$PY" - "$RPDIR" "$RP/moments.txt" <<'PYEOF'
import json, os, sys, time
proj, out = sys.argv[1], sys.argv[2]
evs = []
for n in sorted(os.listdir(os.path.join(proj, "events"))):
    for l in open(os.path.join(proj, "events", n)):
        if l.strip():
            evs.append(json.loads(l))
now = int(time.time() * 1000)
ms = set(e["ts"] for e in evs)
for d in (3, 1, 0):
    ms.add(now - d * 86_400_000)
open(out, "w").write(" ".join(str(int(x)) for x in sorted(ms)))
PYEOF
MOMENTS=$(cat "$RP/moments.txt")
"$PY" "$TOOLS/journal_state.py" "$RPDIR" src/app.ts $MOMENTS > "$RP/before.txt"
"$BIN" --archive "$ARCH" prune retpol --policy "$POLICY" --dry-run > "$RP/dry.txt" 2>&1
need $? "the policy plan runs end to end (pl prune --policy ... --dry-run)"
grep -q "dry run" "$RP/dry.txt" && ok "the plan says it changed nothing" || bad "the dry run does not say that nothing was changed"
grep -q "1/day" "$RP/dry.txt" && ok "the plan prints the windows it applies" || bad "the plan does not print the windows"
sed -n 's/^\(Versions\|Blobs\|Size\|History will start\)/   &/p' "$RP/dry.txt"
"$BIN" --archive "$ARCH" prune retpol --policy "$POLICY" --yes > "$RP/apply.txt" 2>&1
need $? "applying the policy (pl prune --policy ... --yes)"
"$PY" "$TOOLS/journal_state.py" "$RPDIR" src/app.ts $MOMENTS > "$RP/after.txt"
"$PY" - "$RP/before.txt" "$RP/after.txt" <<'PYEOF'
import sys, time
before = dict(l.split()[:2] for l in open(sys.argv[1]) if l.strip())
after = dict(l.split()[:2] for l in open(sys.argv[2]) if l.strip())
boundary = int(time.time() * 1000) - 7 * 86_400_000
kept = old = bad = 0
for t, v in after.items():
    if int(t) >= boundary:
        kept += 1
        if before.get(t) != v:
            bad += 1
            print("   FAIL: the state at %s changed: %s -> %s" % (t, before.get(t), v))
    else:
        old += 1
print("   moments at or after the boundary: %d checked, %d differing (must be 0)" % (kept, bad))
print("   moments older than the boundary: %d (thinned by the policy, not claimed to be intact)" % old)
sys.exit(1 if bad else 0)
PYEOF
need $? "every moment the policy keeps restores to the same bytes as before the prune"
"$BIN" --archive "$ARCH" check retpol --deep > "$RP/check.txt" 2>&1
need $? "the archive is consistent after the policy prune (no missing, no dangling blobs)"
grep -q "every blob re-read and matched its name" "$RP/check.txt" && ok "deep verification passed" || bad "deep verification did not pass"
POL_STORED=$("$PY" -c "import json,sys;print(json.load(open(sys.argv[1])).get('settings',{}).get('retentionPolicy',''))" "$RPDIR/project.json")
[ "$POL_STORED" = "$POLICY" ] && ok "the policy is stored in project.json -> settings -> retentionPolicy" || bad "the stored policy is '$POL_STORED'"
ANCHORS_BEFORE=$("$PY" -c "import json,os,sys;d=sys.argv[1];print(sum(1 for n in os.listdir(os.path.join(d,'events')) for l in open(os.path.join(d,'events',n)) if l.strip() and json.loads(l).get('reason','') in ('retention-anchor','retention-boundary')))" "$RPDIR")
HSTART_BEFORE=$("$PY" -c "import json,sys;print(json.load(open(sys.argv[1])).get('historyStartsAt',''))" "$RPDIR/project.json")
"$BIN" --archive "$ARCH" scan-once retpol > /dev/null 2>&1
"$BIN" --archive "$ARCH" scan-once retpol > /dev/null 2>&1
ANCHORS_AFTER=$("$PY" -c "import json,os,sys;d=sys.argv[1];print(sum(1 for n in os.listdir(os.path.join(d,'events')) for l in open(os.path.join(d,'events',n)) if l.strip() and json.loads(l).get('reason','') in ('retention-anchor','retention-boundary')))" "$RPDIR")
HSTART_AFTER=$("$PY" -c "import json,sys;print(json.load(open(sys.argv[1])).get('historyStartsAt',''))" "$RPDIR/project.json")
PUTS_AFTER=$("$PY" -c "import json,os,sys;d=sys.argv[1];print(sum(1 for n in os.listdir(os.path.join(d,'events')) for l in open(os.path.join(d,'events',n)) if l.strip() and json.loads(l)['type']=='put'))" "$RPDIR")
say "   put events: $VERSIONS_BEFORE before the policy (original versions), $PUTS_AFTER after the policy and two cycles (anchors are put events as well)"
[ "$ANCHORS_BEFORE" = "$ANCHORS_AFTER" ] && ok "two further observation cycles created no anchors: a stored policy is never applied by itself" || bad "an observation cycle applied the policy ($ANCHORS_BEFORE -> $ANCHORS_AFTER anchors)"
[ "$HSTART_BEFORE" = "$HSTART_AFTER" ] && ok "two further observation cycles left historyStartsAt untouched ($HSTART_AFTER)" || bad "an observation cycle moved the start of history ($HSTART_BEFORE -> $HSTART_AFTER)"

step "Heartbeat and healthcheck: exit codes a scheduler can act on"
"$BIN" --archive "$ARCH" scan-once --all > /dev/null 2>&1
"$BIN" --archive "$ARCH" heartbeat-check > "$WORK/hb-fresh.txt" 2>&1
HB_FRESH=$?
"$BIN" --archive "$ARCH" heartbeat-check > /dev/null 2>&1
say "   $(sed -n '1p' "$WORK/hb-fresh.txt")"
[ "$HB_FRESH" = "0" ] && ok "a fresh heartbeat exits 0" || bad "a fresh heartbeat exited $HB_FRESH"
"$BIN" --archive "$ARCH" healthcheck > "$WORK/hc-ok.txt" 2>&1
[ $? = "0" ] && ok "healthcheck exits 0 on a healthy archive" || bad "healthcheck reported a problem on a healthy archive"
HB_FILE="$ARCH/heartbeat"
SAVED_HB=$(cat "$HB_FILE" 2>/dev/null || echo 0)
printf '%s\n' "$(( $(date +%s) * 1000 - 600000 ))" > "$HB_FILE"
"$BIN" --archive "$ARCH" heartbeat-check > "$WORK/hb-stale.txt" 2>&1
HB_STALE=$?
say "   $(sed -n '1p' "$WORK/hb-stale.txt")"
[ "$HB_STALE" = "1" ] && ok "a stale heartbeat exits 1" || bad "a stale heartbeat exited $HB_STALE (expected 1)"
"$BIN" --archive "$ARCH" healthcheck > "$WORK/hc-bad.txt" 2>&1
HC_BAD=$?
say "   $(sed -n '1p' "$WORK/hc-bad.txt")"
[ "$HC_BAD" = "1" ] && ok "healthcheck exits 1 when the promise is not being kept" || bad "healthcheck exited $HC_BAD on a stale archive (expected 1)"
grep -q "fix:" "$WORK/hc-bad.txt" && ok "the report names the command that fixes it" || bad "the report does not name a fix"
printf '%s\n' "$SAVED_HB" > "$HB_FILE"

step "The MCP surface: nine read-only tools, and the archive is not touched"
MCP_BIN="$(dirname "$BIN")/pl-mcp"
if [ ! -x "$MCP_BIN" ]; then
  bad "the MCP server binary is missing ($MCP_BIN) — build it with: cargo build --release"
else
  "$PY" "$TOOLS/mcp_client.py" --archive "$ARCH" --binary "$MCP_BIN" --check-readonly --rate-test > "$WORK/mcp-client.txt" 2>&1
  MCP_RC=$?
  sed -n 's/^\(tools\/list\|   pl_\|rate test\|read-only check\|call pl_restore\|call pl_prune\|call definitely\)/   &/p' "$WORK/mcp-client.txt"
  need $MCP_RC "an outside MCP client sees only reading tools, is refused write ones, and finds the archive unchanged"
fi
"$PY" "$TOOLS/mcp_audit.py" --root "$(cd "$TOOLS/.." && pwd)" > "$WORK/mcp-audit.txt" 2>&1
MCP_AUDIT=$?
sed -n 's/^\(read surface\|no write symbol\|control ok\|WRITE PATH\|MCP AUDIT\)/   &/p' "$WORK/mcp-audit.txt"
need $MCP_AUDIT "the reading surface contains no write symbol (and the control was caught)"

step "The daily commands answer without changing anything"
DC_BEFORE=$("$PY" - "$ARCH" <<'PYEOF'
import hashlib, os, sys
arch = sys.argv[1]
h = hashlib.sha256()
for dirpath, dirnames, filenames in os.walk(arch):
    for f in sorted(filenames):
        p = os.path.join(dirpath, f)
        rel = os.path.relpath(p, arch)
        if rel.startswith("logs"):
            continue
        h.update(rel.encode())
        with open(p, "rb") as fh:
            h.update(fh.read())
print(h.hexdigest())
PYEOF
)
for c in "recent --limit 3" "status --compact" "size --top 3" "gc" "suggest" "prompt --space" "list --sort risk" "log retpol --grep app.ts"; do
  "$BIN" --archive "$ARCH" $c > /dev/null 2>&1 || bad "pl $c failed"
done
"$BIN" --archive "$ARCH" cat retpol --path src/app.ts > "$WORK/cat.txt" 2>/dev/null
CAT_RC=$?
DC_AFTER=$("$PY" - "$ARCH" <<'PYEOF'
import hashlib, os, sys
arch = sys.argv[1]
h = hashlib.sha256()
for dirpath, dirnames, filenames in os.walk(arch):
    for f in sorted(filenames):
        p = os.path.join(dirpath, f)
        rel = os.path.relpath(p, arch)
        if rel.startswith("logs"):
            continue
        h.update(rel.encode())
        with open(p, "rb") as fh:
            h.update(fh.read())
print(h.hexdigest())
PYEOF
)
need $CAT_RC "pl cat reads one file's bytes out of the archive"
if [ "$DC_BEFORE" = "$DC_AFTER" ]; then
  ok "eight read-only commands left the archive byte-identical ($(printf '%s' "$DC_BEFORE" | cut -c1-12)…)"
else
  bad "a read-only command changed the archive"
fi

step "The partial pass: a notification-driven pass walks only what was named"
# A fixture of its own: 60 tracked files, so "walked one file instead of sixty" is a measurement and
# not a claim. Everything goes through the shipped binary — the same entry point the daemon uses
# when inotify reports a change (`pl partial-pass` calls `scan::scan_project_partial`).
PP="$WORK/partial"
rm -rf "$PP"; mkdir -p "$PP/home" "$PP/project/src"
i=0
while [ $i -lt 60 ]; do printf 'seed %s\n' "$i" > "$PP/project/src/seed$i.txt"; i=$((i + 1)); done
PROJECTLIFE_HOME="$PP/home" "$BIN" --archive "$PP/archive" init-archive "$PP/archive" > /dev/null 2>&1
PROJECTLIFE_HOME="$PP/home" "$BIN" --archive "$PP/archive" config set stopFreePercent 0 > /dev/null 2>&1
PROJECTLIFE_HOME="$PP/home" "$BIN" --archive "$PP/archive" config set warnFreePercent 0 > /dev/null 2>&1
PROJECTLIFE_HOME="$PP/home" "$BIN" --archive "$PP/archive" add "$PP/project" --name part --yes > /dev/null 2>&1
PPD=""
for d in "$PP/archive"/projects/*/; do
  n=$("$PY" -c "import json,sys;print(json.load(open(sys.argv[1])).get('name',''))" "$d/project.json" 2>/dev/null)
  [ "$n" = "part" ] && PPD="$d"
done
[ -n "$PPD" ] && ok "a 60-file project is being observed" || bad "the fixture project was not created"
printf 'changed 3\n' > "$PP/project/src/seed3.txt"
printf 'changed 7 (never mentioned)\n' > "$PP/project/src/seed7.txt"
PROJECTLIFE_HOME="$PP/home" "$BIN" --archive "$PP/archive" partial-pass part src/seed3.txt --json > "$WORK/partial-1.json" 2>&1
PARTIAL_RC=$?
sed -n 's/^  \("partial"\|"scopePaths"\|"dirsWalked"\|"filesInScope"\|"trackedBefore"\|"changed"\|"deleted"\|"ms"\):/   \1:/p' "$WORK/partial-1.json"
need $PARTIAL_RC "pl partial-pass runs one pass over the named path"
"$PY" - "$WORK/partial-1.json" "$PPD" "$PP" <<'PYEOF'
import hashlib, json, os, sys
rep = json.load(open(sys.argv[1])); proj = sys.argv[2]; root = sys.argv[3]
ev = []
for n in sorted(os.listdir(os.path.join(proj, "events"))):
    for line in open(os.path.join(proj, "events", n)):
        if line.strip(): ev.append(json.loads(line))
puts = [e for e in ev if e["type"] == "put"]
bad = 0
def check(cond, msg):
    global bad
    print("   %s: %s" % ("PASS" if cond else "FAIL", msg))
    if not cond: bad += 1
def versions(path):
    return [e["hash"] for e in puts if e.get("path") == path]
def h(p):
    return hashlib.sha256(open(p, "rb").read()).hexdigest()
check(rep.get("partial") is True, "the pass reports itself as partial")
check(rep.get("scopePaths") == 1, "one notification path was handed in")
check(rep.get("filesInScope") == 1, "exactly one file was in scope")
check(rep.get("dirsWalked") == 0, "no directory was opened (the notification named a file)")
check(rep.get("trackedBefore") == 60, "the project itself tracks 60 files")
check(rep.get("changed") == 1, "exactly one file was stored")
check(rep.get("deleted") == 0, "nothing was deleted")
check(rep.get("moved") == 0, "nothing was moved")
check(len(versions("src/seed3.txt")) == 2, "the notified file has two versions (initial + change)")
check(versions("src/seed3.txt")[-1] == h(os.path.join(root, "project/src/seed3.txt")),
      "the stored version is the hash of the bytes on disk")
check(len(versions("src/seed7.txt")) == 1, "the file nobody mentioned was not read at all")
check([e for e in ev if e["type"] == "delete"] == [], "no delete event was written")
sys.exit(1 if bad else 0)
PYEOF
need $? "the pass stored exactly what was named, and nothing outside it"
puts_of() { "$PY" -c "import json,os,sys;d=sys.argv[1];print(sum(1 for n in os.listdir(os.path.join(d,'events')) for l in open(os.path.join(d,'events',n)) if l.strip() and json.loads(l)['type']=='put'))" "$1"; }
PUTS_ONE=$(puts_of "$PPD")
PROJECTLIFE_HOME="$PP/home" "$BIN" --archive "$PP/archive" partial-pass part src/seed3.txt > /dev/null 2>&1
PUTS_TWO=$(puts_of "$PPD")
[ "$PUTS_ONE" = "$PUTS_TWO" ] && ok "a second partial pass over an unchanged path wrote nothing" || bad "the partial pass stored a version for unchanged contents ($PUTS_ONE -> $PUTS_TWO)"
PROJECTLIFE_HOME="$PP/home" "$BIN" --archive "$PP/archive" scan-once part --json > "$WORK/partial-full.json" 2>&1
CHANGED_BY_FULL=$("$PY" -c "import json,sys;print(json.load(open(sys.argv[1]))['projects'][0]['changed'])" "$WORK/partial-full.json")
[ "$CHANGED_BY_FULL" = "1" ] && ok "the periodic full pass that follows finds the file no notification mentioned" || bad "the full pass found $CHANGED_BY_FULL changed files (expected 1)"
PROJECTLIFE_HOME="$PP/home" "$BIN" --archive "$PP/archive" check part > /dev/null 2>&1
need $? "the archive is consistent after the partial pass and the full pass"

step "The partial pass: a cross-directory rename and a folder rename, one batch each"
mkdir -p "$PP/project/lib"
mv "$PP/project/src/seed5.txt" "$PP/project/lib/seed5moved.txt"
PROJECTLIFE_HOME="$PP/home" "$BIN" --archive "$PP/archive" partial-pass part src/seed5.txt lib/seed5moved.txt --json > "$WORK/partial-move.json" 2>&1
"$PY" - "$WORK/partial-move.json" "$PPD" <<'PYEOF'
import json, os, sys
rep = json.load(open(sys.argv[1])); proj = sys.argv[2]
ev = []
for n in sorted(os.listdir(os.path.join(proj, "events"))):
    for line in open(os.path.join(proj, "events", n)):
        if line.strip(): ev.append(json.loads(line))
moves = [e for e in ev if e["type"] == "move"]
bad = 0
def check(cond, msg):
    global bad
    print("   %s: %s" % ("PASS" if cond else "FAIL", msg))
    if not cond: bad += 1
check(rep.get("moved") == 1, "one move was recorded for the cross-directory rename")
check(rep.get("deleted") == 0, "a rename is not a deletion")
check(rep.get("blobs_new", rep.get("newBlobs")) == 0, "a rename creates no new bytes")
check(len(moves) == 1 and moves[0]["from"] == "src/seed5.txt" and moves[0]["to"] == "lib/seed5moved.txt",
      "the move names both sides: %s" % [(m["from"], m["to"]) for m in moves])
sys.exit(1 if bad else 0)
PYEOF
need $? "a rename between two directories is one move event, with no version and no new blob"
mkdir -p "$PP/project/tree/deep"
printf 't1\n' > "$PP/project/tree/a.txt"; printf 't2\n' > "$PP/project/tree/deep/b.txt"
PROJECTLIFE_HOME="$PP/home" "$BIN" --archive "$PP/archive" partial-pass part tree > /dev/null 2>&1
mv "$PP/project/tree" "$PP/project/grove"
PROJECTLIFE_HOME="$PP/home" "$BIN" --archive "$PP/archive" partial-pass part tree grove --json > "$WORK/partial-dir.json" 2>&1
"$PY" - "$WORK/partial-dir.json" "$PPD" <<'PYEOF'
import json, os, sys
rep = json.load(open(sys.argv[1])); proj = sys.argv[2]
ev = []
for n in sorted(os.listdir(os.path.join(proj, "events"))):
    for line in open(os.path.join(proj, "events", n)):
        if line.strip(): ev.append(json.loads(line))
moves = [e for e in ev if e["type"] == "move" and e.get("from", "").startswith("tree/")]
bad = 0
def check(cond, msg):
    global bad
    print("   %s: %s" % ("PASS" if cond else "FAIL", msg))
    if not cond: bad += 1
check(rep.get("moved") == 2, "both files of the renamed folder were moved")
check(len(moves) == 2, "two move events in the journal")
check(len({m.get("batchId") for m in moves}) == 1, "both moves carry one batch id")
check(sorted((m["from"], m["to"]) for m in moves) == [("tree/a.txt", "grove/a.txt"), ("tree/deep/b.txt", "grove/deep/b.txt")],
      "the two moves name the right paths")
sys.exit(1 if bad else 0)
PYEOF
need $? "a folder rename through a partial pass is one batch of moves"
PROJECTLIFE_HOME="$PP/home" "$BIN" --archive "$PP/archive" check part > /dev/null 2>&1
need $? "the archive is still consistent after the renames"

step "The daemon really uses the partial pass (end to end, with inotify)"
PD="$WORK/partial-daemon"
rm -rf "$PD"; mkdir -p "$PD/home" "$PD/project/src"
i=0
while [ $i -lt 30 ]; do printf 'seed %s\n' "$i" > "$PD/project/src/seed$i.txt"; i=$((i + 1)); done
PROJECTLIFE_HOME="$PD/home" "$BIN" --archive "$PD/archive" init-archive "$PD/archive" > /dev/null 2>&1
PROJECTLIFE_HOME="$PD/home" "$BIN" --archive "$PD/archive" config set stopFreePercent 0 > /dev/null 2>&1
PROJECTLIFE_HOME="$PD/home" "$BIN" --archive "$PD/archive" config set warnFreePercent 0 > /dev/null 2>&1
PROJECTLIFE_HOME="$PD/home" "$BIN" --archive "$PD/archive" config set intervalSeconds 60 > /dev/null 2>&1
PROJECTLIFE_HOME="$PD/home" "$BIN" --archive "$PD/archive" config set autoInterval false > /dev/null 2>&1
PROJECTLIFE_HOME="$PD/home" "$BIN" --archive "$PD/archive" add "$PD/project" --name pdem --yes > /dev/null 2>&1
PDD=""
for d in "$PD/archive"/projects/*/; do
  n=$("$PY" -c "import json,sys;print(json.load(open(sys.argv[1])).get('name',''))" "$d/project.json" 2>/dev/null)
  [ "$n" = "pdem" ] && PDD="$d"
done
PROJECTLIFE_HOME="$PD/home" "$BIN" --archive "$PD/archive" daemon run > "$WORK/pd-daemon.log" 2>&1 &
DPID=$!
"$PY" - "$PD/archive" "$DPID" <<'PYEOF'
import json, os, sys, time
arch, pid = sys.argv[1], int(sys.argv[2])
t0 = time.time()
while time.time() - t0 < 20:
    try:
        st = json.load(open(os.path.join(arch, "watch_state.json")))
    except Exception:
        st = {}
    if st.get("pid") == pid and st.get("watchedDirs", 0) > 0 and st.get("periodicCycles", 0) >= 1:
        print("   watch set installed: %s director(ies), trigger %s" % (st.get("watchedDirs"), st.get("mode")))
        sys.exit(0)
    time.sleep(0.05)
print("   FAIL: the daemon never installed a watch set")
sys.exit(1)
PYEOF
need $? "the daemon installs its watch set and completes its first pass"
printf 'created by the watchdog\n' > "$PD/project/src/new.txt"
"$PY" - "$PD/archive" "$PD/project/src/new.txt" <<'PYEOF'
import glob, hashlib, json, os, sys, time
arch, target = sys.argv[1], sys.argv[2]
want = hashlib.sha256(open(target, "rb").read()).hexdigest()
t0 = time.time()
while time.time() - t0 < 25:
    for f in glob.glob(os.path.join(arch, "projects", "*", "events", "*.jsonl")):
        for line in open(f):
            if line.strip():
                e = json.loads(line)
                if e.get("type") == "put" and e.get("path") == "src/new.txt" and e.get("hash") == want:
                    print("   the required bytes are in the journal after %.1f s (interval: 60 s)" % (time.time() - t0))
                    sys.exit(0)
    time.sleep(0.05)
print("   FAIL: the created file was not stored within 25 s with a 60 s interval")
sys.exit(1)
PYEOF
need $? "a created file is stored by the notification, long before the 60 s interval"
kill -TERM "$DPID" 2>/dev/null
wait "$DPID" 2>/dev/null
"$PY" - "$PD/archive" <<'PYEOF'
import json, os, sys
st = json.load(open(os.path.join(sys.argv[1], "watch_state.json")))
bad = 0
def check(cond, msg):
    global bad
    print("   %s: %s" % ("PASS" if cond else "FAIL", msg))
    if not cond: bad += 1
check(st.get("partialCycles", 0) >= 1, "at least one notification-driven pass was PARTIAL")
check(st.get("triggerCycles", 0) == st.get("partialCycles", 0), "no notification pass fell back to the full pass")
check(st.get("periodicCycles", 0) == 1, "the periodic pass is still only the startup one (interval 60 s)")
lp = (st.get("lastPartial") or {}).get("projects") or []
check(bool(lp), "the published state says what the partial pass did: %s" % lp)
if lp:
    p0 = lp[0]
    # What is asserted here is the *shape* of the report, not a size: on this machine's own
    # filesystem the daemon's read-only pass is itself reported as a change (measured by
    # tools/inotify_probe.py), so the scope of a notification-driven pass is legitimately wider than
    # the one path that was written by hand. The exact scope semantics are proved deterministically
    # by the two steps above, which hand the paths in themselves.
    check(p0.get("scopePaths", 0) >= 1, "the pass was handed at least the path that was written")
    check(p0.get("filesInScope", 0) >= 1, "at least that file was in scope")
    check(p0.get("filesInScope", 0) <= 31, "the scope never exceeded the project (%s tracked)" % 31)
    check(p0.get("created") == 1, "exactly the created file was stored")
sys.exit(1 if bad else 0)
PYEOF
need $? "the published trigger state proves the pass was partial and what its scope cost"

step "A notification that reports no real change costs nothing and stores nothing"
# This machine's /work is a host share whose reads are reported to inotify as modifications (and
# whose directory entries are re-created); the container's overlay filesystem does neither. The
# requirement is the same for both: a notification about a file that did not change may cost a look
# and must never cost a version. The journal is what is asserted; whether the filesystem is noisy is
# printed, not assumed.
NT="$WORK/noisy"
rm -rf "$NT"; mkdir -p "$NT/home" "$NT/project/src"
i=0
while [ $i -lt 5 ]; do printf 'quiet %s\n' "$i" > "$NT/project/src/q$i.txt"; i=$((i + 1)); done
PROJECTLIFE_HOME="$NT/home" "$BIN" --archive "$NT/archive" init-archive "$NT/archive" > /dev/null 2>&1
PROJECTLIFE_HOME="$NT/home" "$BIN" --archive "$NT/archive" config set stopFreePercent 0 > /dev/null 2>&1
PROJECTLIFE_HOME="$NT/home" "$BIN" --archive "$NT/archive" config set warnFreePercent 0 > /dev/null 2>&1
PROJECTLIFE_HOME="$NT/home" "$BIN" --archive "$NT/archive" config set intervalSeconds 60 > /dev/null 2>&1
PROJECTLIFE_HOME="$NT/home" "$BIN" --archive "$NT/archive" config set autoInterval false > /dev/null 2>&1
PROJECTLIFE_HOME="$NT/home" "$BIN" --archive "$NT/archive" add "$NT/project" --name quiet --yes > /dev/null 2>&1
PROJECTLIFE_HOME="$NT/home" "$BIN" --archive "$NT/archive" daemon run > "$WORK/quiet-daemon.log" 2>&1 &
QPID=$!
"$PY" - "$NT/archive" "$QPID" <<'PYEOF'
import json, os, sys, time
arch, pid = sys.argv[1], int(sys.argv[2])
t0 = time.time()
while time.time() - t0 < 20:
    try:
        st = json.load(open(os.path.join(arch, "watch_state.json")))
    except Exception:
        st = {}
    if st.get("pid") == pid and st.get("periodicCycles", 0) >= 1:
        sys.exit(0)
    time.sleep(0.05)
sys.exit(1)
PYEOF
need $? "the daemon completed its first pass"
sleep 6
kill -TERM "$QPID" 2>/dev/null
wait "$QPID" 2>/dev/null
"$PY" - "$NT" <<'PYEOF'
import glob, json, os, sys
root = sys.argv[1]
ev = []
for f in glob.glob(os.path.join(root, "archive", "projects", "*", "events", "*.jsonl")):
    for line in open(f):
        if line.strip():
            ev.append(json.loads(line))
after = [e for e in ev if e.get("reason") != "initial"]
st = json.load(open(os.path.join(root, "archive", "watch_state.json")))
print("   the filesystem reported %s notification path(s) in %s pass(es) (%s of them partial); the journal has %d event(s) after the initial snapshot"
      % (st.get("events"), st.get("triggerCycles", 0) + st.get("periodicCycles", 0), st.get("partialCycles", 0), len(after)))
print("   %s" % ("this filesystem reports reads as changes: every notification-driven pass here is\n   the daemon's own reading being echoed back — measured by tools/inotify_probe.py"
                 if st.get("events", 0) > 0 else
                 "this filesystem reports no change for a read, so no pass was triggered by noise"))
sys.exit(0 if not after else 1)
PYEOF
need $? "a read-only daemon stores nothing, however noisy the filesystem's notifications are"

step "The bookkeeping of a partial pass: the base is not rewritten, the journal is not read"
SP="$WORK/spread"
rm -rf "$SP"
i=0
while [ $i -lt 300 ]; do
  sub=$(printf 's%02d' $((i / 25)))
  mkdir -p "$SP/$sub"
  printf 'file %s\n' "$i" > "$SP/$sub/f$i.txt"
  i=$((i + 1))
done
# A second fixture ten times the size: it is what makes "the reads do not grow with the project"
# a measurement instead of an assertion.
SPBIG="$WORK/spreadbig"
rm -rf "$SPBIG"
i=0
while [ $i -lt 3000 ]; do
  sub=$(printf 'b%03d' $((i / 25)))
  mkdir -p "$SPBIG/$sub"
  printf 'file %s\n' "$i" > "$SPBIG/$sub/f$i.txt"
  i=$((i + 1))
done
"$BIN" --archive "$ARCH" add "$SP" --name spread --yes > "$WORK/spread-add.txt" 2>&1
need $? "a project of 300 files is observed (its first pass writes the cache base)"
"$BIN" --archive "$ARCH" add "$SPBIG" --name spreadbig --yes > "$WORK/spreadbig-add.txt" 2>&1
need $? "and one of 3000 files"
proj_dir_of() {
  for d in "$ARCH"/projects/*/; do
    n=$("$PY" -c "import json,sys;print(json.load(open(sys.argv[1])).get('name',''))" "$d/project.json" 2>/dev/null)
    [ "$n" = "$1" ] && printf '%s' "$d"
  done
}
PDIR_SP=$(proj_dir_of spread)
PDIR_BIG=$(proj_dir_of spreadbig)
[ -n "$PDIR_SP" ] && [ -n "$PDIR_BIG" ] && ok "both projects are on disk" || bad "a fixture project is missing"
BASE_BEFORE=$(sha256sum "$PDIR_SP/cache/base.jsonl" 2>/dev/null | cut -d' ' -f1)
DELTA_BEFORE=$(wc -l < "$PDIR_SP/cache/delta.jsonl" 2>/dev/null | tr -d ' ')
BASE_BYTES=$(stat -c%s "$PDIR_SP/cache/base.jsonl" 2>/dev/null)

printf 'changed by the verifier\n' > "$SP/s00/f0.txt"
"$BIN" --archive "$ARCH" partial-pass spread s00/f0.txt --json > "$WORK/pp294.json" 2>"$WORK/pp294.err"
need $? "the partial pass ran"
BASE_AFTER=$(sha256sum "$PDIR_SP/cache/base.jsonl" 2>/dev/null | cut -d' ' -f1)
DELTA_AFTER=$(wc -l < "$PDIR_SP/cache/delta.jsonl" 2>/dev/null | tr -d ' ')

if [ "$BASE_BEFORE" = "$BASE_AFTER" ]; then
  ok "the cache base is byte-identical after a partial pass (sha256 $(printf '%s' "$BASE_BEFORE" | cut -c1-12))"
else
  bad "the partial pass rewrote the cache base ($BASE_BEFORE -> $BASE_AFTER)"
fi
if [ "$((DELTA_AFTER - DELTA_BEFORE))" -eq 1 ]; then
  ok "the delta grew by exactly one record (the changed path, and nothing else)"
else
  bad "the delta grew by $((DELTA_AFTER - DELTA_BEFORE)) records for one changed file"
fi
printf 'changed by the verifier\n' > "$SPBIG/b000/f0.txt"
"$BIN" --archive "$ARCH" partial-pass spreadbig b000/f0.txt --json > "$WORK/pp294big.json" 2>/dev/null
BASE_BIG=$(stat -c%s "$PDIR_BIG/cache/base.jsonl" 2>/dev/null)
"$PY" - "$WORK/pp294.json" "$BASE_BYTES" "$WORK/pp294big.json" "$BASE_BIG" <<'PY294'
import json, sys
small, sbase, big, bbase = json.load(open(sys.argv[1])), int(sys.argv[2]), json.load(open(sys.argv[3])), int(sys.argv[4])
print("   one-file pass on 300 files:   %s ms, read %s B of a %s B base (%s journal byte(s) in full, %s base rewrite(s), %s delta record(s))"
      % (small.get("ms"), small.get("cacheBytesRead"), sbase, small.get("journalFullBytesRead"),
         small.get("cacheBaseRewrites"), small.get("cacheDeltaRecords")))
print("   one-file pass on 3000 files:  %s ms, read %s B of a %s B base" % (big.get("ms"), big.get("cacheBytesRead"), bbase))
ok = (small.get("journalFullBytesRead") == 0 and big.get("journalFullBytesRead") == 0
      and small.get("cacheBaseRewrites") == 0 and big.get("cacheBaseRewrites") == 0
      and small.get("cacheDeltaRecords") == 1 and big.get("cacheDeltaRecords") == 1
      and (small.get("journalTailBytesRead") or 0) > 0 and (big.get("journalTailBytesRead") or 0) > 0
      # ten times the state, nowhere near ten times the reading: the cost is the log of the base
      and (big.get("cacheBytesRead") or 0) < 0.25 * bbase
      and (big.get("cacheBytesRead") or 0) < 3 * max(small.get("cacheBytesRead") or 1, 1))
sys.exit(0 if ok else 1)
PY294
need $? "no journal read in full, no base rewritten, and the reading does not grow with the project"

# The control that makes the line above mean something: with a journal file no reader can read, a
# full pass must fail and a partial pass must not even notice.
mkdir -p "$PDIR_SP/events/2020-01.jsonl"
"$BIN" --archive "$ARCH" scan-once spread > "$WORK/spread-full-broken.txt" 2>&1
if grep -q "spread: " "$WORK/spread-full-broken.txt"; then
  ok "a full pass refuses to run while a journal file is unreadable (the control bites)"
else
  bad "a full pass did not report the unreadable journal: $(head -3 "$WORK/spread-full-broken.txt" | tr '\n' ' ')"
fi
printf 'changed while the journal was unreadable\n' > "$SP/s00/f0.txt"
"$BIN" --archive "$ARCH" partial-pass spread s00/f0.txt --json > "$WORK/pp294b.json" 2>"$WORK/pp294b.err"
need $? "the partial pass stores the change while that journal file is still unreadable"
grep -q '"changed": 1' "$WORK/pp294b.json" && ok "it stored exactly the changed file" || bad "the change was not stored: $(cat "$WORK/pp294b.json" | tr '\n' ' ')"
grep -q '"journalFullBytesRead": 0' "$WORK/pp294b.json" && ok "and it read no journal file in full" || bad "it read the journal in full: $(grep journalFull "$WORK/pp294b.json")"
rmdir "$PDIR_SP/events/2020-01.jsonl"

step "The cache and the journal agree — checked by a reader written from the format"
"$PY" "$TOOLS/cache_vs_journal.py" --archive "$ARCH" --project spread
need $? "every cache entry matches the state the journal implies (independent Python reader)"
"$PY" "$TOOLS/cache_vs_journal.py" --archive "$ARCH" --project spreadbig
need $? "and the same on the 3000-file project"

# The control: a cache that has drifted must be caught. One hash is changed by hand, on a path the
# delta does NOT mention (a delta record would hide the base entry — the first version of this
# control tampered with exactly such a path and the reader was right not to notice).
cp "$PDIR_SP/cache/base.jsonl" "$WORK/base294.bak"
"$PY" - "$PDIR_SP" <<'PY294'
import io, json, os, sys
d = sys.argv[1]
delta_paths = set()
with open(os.path.join(d, "cache", "delta.jsonl")) as fh:
    for line in fh:
        if line.strip():
            delta_paths.add(json.loads(line)["path"])
p = os.path.join(d, "cache", "base.jsonl")
lines = io.open(p).read().splitlines()
target = None
for i, l in enumerate(lines):
    rec = json.loads(l)
    if rec["path"] not in delta_paths:
        j = l.find('"hash":"')
        k = j + 8
        lines[i] = l[:k] + "0" * 64 + l[k + 64:]
        target = rec["path"]
        break
io.open(p, "w").write("\n".join(lines) + "\n")
print("   (the hash of %s was replaced with zeros by hand; it is not mentioned by the delta, so the\n    cache really is the only place that says it)" % target)
PY294
"$PY" "$TOOLS/cache_vs_journal.py" --archive "$ARCH" --project spread > "$WORK/cache-tampered.txt" 2>&1
[ $? -ne 0 ] && ok "the reader catches a cache that drifted from the journal (the control bites)" || bad "the drift was not caught: $(tail -2 "$WORK/cache-tampered.txt" | tr '\n' ' ')"
cp "$WORK/base294.bak" "$PDIR_SP/cache/base.jsonl"
"$PY" "$TOOLS/cache_vs_journal.py" --archive "$ARCH" --project spread > /dev/null 2>&1
need $? "and it agrees again once the file is put back"

step "The delta past its cap is folded into the base, and the state does not change"
"$BIN" --archive "$ARCH" config set cacheDeltaMaxBytes 400 > /dev/null 2>&1
i=0
COMP=0
while [ $i -lt 12 ]; do
  printf 'cap version %s\n' "$i" > "$SP/s01/f25.txt"
  "$BIN" --archive "$ARCH" partial-pass spread s01/f25.txt --json > "$WORK/pp294c.json" 2>/dev/null
  c=$("$PY" -c "import json;print(json.load(open('$WORK/pp294c.json')).get('cacheCompactions',0))" 2>/dev/null || echo 0)
  COMP=$((COMP + c))
  i=$((i + 1))
done
[ "$COMP" -ge 1 ] && ok "the cap was reached and a pass folded the delta ($COMP compaction(s))" || bad "the delta was never folded past its cap"
DELTA_BYTES=$(stat -c%s "$PDIR_SP/cache/delta.jsonl" 2>/dev/null || echo 9999)
[ "$DELTA_BYTES" -le 400 ] && ok "the folded delta is under the cap ($DELTA_BYTES B)" || bad "the delta is $DELTA_BYTES B, over the 400 B cap"
"$PY" "$TOOLS/cache_vs_journal.py" --archive "$ARCH" --project spread > /dev/null 2>&1
need $? "the cache still agrees with the journal after the folding"
"$BIN" --archive "$ARCH" config set cacheDeltaMaxBytes 131072 > /dev/null 2>&1

step "Stress: ten partial passes in a row, and ten runs of the round-294 suite"
i=0
FAILP=0
while [ $i -lt 10 ]; do
  printf 'stress %s\n' "$i" > "$SP/s02/f50.txt"
  "$BIN" --archive "$ARCH" partial-pass spread s02/f50.txt --json > "$WORK/pp294d.json" 2>/dev/null || FAILP=$((FAILP + 1))
  i=$((i + 1))
done
[ "$FAILP" -eq 0 ] && ok "ten consecutive partial passes, none failed" || bad "$FAILP of ten partial passes failed"
"$PY" "$TOOLS/cache_vs_journal.py" --archive "$ARCH" --project spread > /dev/null 2>&1
need $? "the cache still agrees with the journal after the tenth pass"
VERSIONS=$("$PY" -c "
import glob, json, sys
puts = 0
for f in glob.glob(sys.argv[1] + '/events/*.jsonl'):
    for line in open(f):
        if line.strip() and json.loads(line).get('type') == 'put':
            puts += 1
print(puts)" "$PDIR_SP")
say "   the journal holds $VERSIONS put event(s): 300 initial versions + the distinct contents of the passed files"
SUITE_FAILS=0
i=0
while [ $i -lt 10 ]; do
  (cd "${PROJECTLIFE_SRC:-/work/projectlife}" && cargo test --release --test round294 > /dev/null 2>&1) || SUITE_FAILS=$((SUITE_FAILS + 1))
  i=$((i + 1))
done
[ "$SUITE_FAILS" -eq 0 ] && ok "the round-294 suite is green ten times in a row (a test that needs a quiet machine is not a test)" || bad "$SUITE_FAILS of ten runs of the round-294 suite failed"

step "What a partial pass costs now, with the process floor as the control"
"$PY" - "$BIN" "$ARCH" "$SPBIG" <<'PY294'
import json, os, statistics, subprocess, sys, time
binary, arch, proj = sys.argv[1], sys.argv[2], sys.argv[3]
env = dict(os.environ)

def timed(args):
    t0 = time.monotonic()
    r = subprocess.run([binary, "--archive", arch] + args, capture_output=True, text=True, env=env)
    return (time.monotonic() - t0) * 1000.0, r

def med(xs):
    return round(statistics.median(xs), 1)

floor, part, empty, full, inner = [], [], [], [], []
for k in range(9):
    floor.append(timed(["version"])[0])
    part.append(timed(["partial-pass", "spreadbig", "b000/f0.txt", "--json"])[0])
    empty.append(timed(["partial-pass", "spreadbig", "no-such-path.txt", "--json"])[0])
    full.append(timed(["scan-once", "spreadbig"])[0])
    inner.append(json.loads(timed(["partial-pass", "spreadbig", "b000/f0.txt", "--json"])[1].stdout).get("ms"))
print("   process floor (pl version):            median %6.1f ms   min %5.1f ms" % (med(floor), min(floor)))
print("   partial pass, 1 file notified:          median %6.1f ms   min %5.1f ms   (the pass measures itself: %s ms)" % (med(part), min(part), med(inner)))
print("   partial pass, scope names nothing:      median %6.1f ms" % med(empty))
print("   full pass over 3000 files:              median %6.1f ms" % med(full))
print("   round 293 measured the same pass at ~91 ms on a 10 000-file project, of which ~71 ms was bookkeeping")
ok = med(part) * 4 < med(full) and med(part) < med(floor) + 30
sys.exit(0 if ok else 1)
PY294
need $? "a one-file partial pass costs a fraction of a full pass (the floor is printed, not assumed)"

step "Archive layout (what a stranger would see on disk)"
say "   $(find "$ARCH" -maxdepth 1 | sort | tr '\n' ' ')"
say "   blobs: $(find "$ARCH/projects" -path '*blobs*' -type f 2>/dev/null | wc -l | tr -d ' ') files; journal files: $(find "$ARCH" -path '*events*' -name '*.jsonl' 2>/dev/null | wc -l | tr -d ' ')"

step "The interface server's socket: the ladder, the refusal, and who binds on whose behalf"
SRC=${PROJECTLIFE_SRC:-/work/projectlife}
APP_BIN=${PROJECTLIFE_APP_BIN:-$SRC/app/target/release/projectlife-ui}
if [ ! -x "$APP_BIN" ]; then
  say "   building the interface server (cargo build --release --manifest-path app/Cargo.toml)"
  (cd "$SRC" && cargo build --release --manifest-path app/Cargo.toml > "$WORK/app_build.txt" 2>&1)
fi
if [ ! -x "$APP_BIN" ]; then
  bad "no interface server binary at $APP_BIN"
else
  DENY="$WORK/bind_deny"
  cc -O2 -o "$DENY" "$TOOLS/bind_deny.c" > "$WORK/deny_cc.txt" 2>&1
  if [ ! -x "$DENY" ]; then
    bad "the seccomp launcher could not be built: $(head -2 "$WORK/deny_cc.txt" | tr '\n' ' ')"
  else
    # The launcher refuses to be used as evidence until the kernel has refused bind() in its own
    # process: the errno in every case below comes from the kernel, not from a stub of mine.
    "$DENY" -- /bin/true 2> "$WORK/deny_self.txt"
    if grep -q "refused by the kernel with errno 1" "$WORK/deny_self.txt"; then
      ok "the kernel itself refuses bind() with EPERM here: $(head -1 "$WORK/deny_self.txt")"
    else
      bad "the seccomp launcher did not report a kernel refusal: $(head -1 "$WORK/deny_self.txt")"
    fi
    "$PY" "$TOOLS/ui_bind_test.py" --app "$APP_BIN" --pl "$BIN" --deny "$DENY" > "$WORK/bind_test.txt" 2>&1
    BIND_RC=$?
    grep -cE "^  PASS" "$WORK/bind_test.txt" | sed 's/^/   case(s) passed: /'
    tail -1 "$WORK/bind_test.txt" | sed 's/^/   /'
    say "   full log: $WORK/bind_test.txt"
    need $BIND_RC "the port ladder, the refusal, the handoff and --diagnose all behave as promised"

    # And once outside every harness: one plain run under the kernel's refusal, where the only
    # things checked are the exit code, the words, and the absence of the sentence round 295 shipped.
    "$DENY" "$APP_BIN" --pl "$BIN" --port 7717 --port-range 1 --log-dir "$WORK/refusal" --no-ipc \
        > "$WORK/refusal_out.txt" 2> "$WORK/refusal_err.txt"
    REFUSAL_RC=$?
    if [ "$REFUSAL_RC" = "3" ] && grep -q '"kind":"permission"' "$WORK/refusal_out.txt" \
        && grep -q "Operation not permitted" "$WORK/refusal_err.txt" \
        && grep -q "no port number will help" "$WORK/refusal/daemon.log" \
        && ! grep -q "Reinstall" "$WORK/refusal_out.txt" "$WORK/refusal_err.txt" "$WORK/refusal/daemon.log"; then
      ok "one plain run under a kernel refusal: exit 3, the errno in the JSON, the stderr and the log, and no generic sentence"
    else
      bad "the refusal path outside any harness (exit $REFUSAL_RC)"
      head -4 "$WORK/refusal_err.txt" | sed 's/^/   | /'
    fi
  fi
fi

step "There is no room: one rule, three readers, and the machine as the fourth (round 296)"
"$PY" "$TOOLS/space_transition_check.py" --pl "$BIN" --work "$WORK/space" > "$WORK/space_check.txt" 2>&1
SPACE_RC=$?
sed 's/^/   /' "$WORK/space_check.txt" | grep -E "PASS|FAIL|thresholds on this volume" | head -8
tail -1 "$WORK/space_check.txt" | sed 's/^/   /'
need $SPACE_RC "the threshold is the smaller of the two numbers, the disk is measured independently with statvfs, and the notifications are one per transition"

step "The Mach-O reader is checked against bytes built by hand"
"$PY" "$TOOLS/test_macho_inspect.py" > "$WORK/macho_reader.txt" 2>&1
MACHO_RC=$?
tail -2 "$WORK/macho_reader.txt" | sed 's/^/   /'
need $MACHO_RC "the reader names the load commands it finds (its constant table was wrong once: LC_BUILD_VERSION is 0x32, not 0x2F)"

step "The macOS shell keeps the promises the window depends on, and the check can fail"
"$PY" "$TOOLS/shell_contract_check.py" > "$WORK/shell_contract.txt" 2>&1
CONTRACT_RC=$?
tail -1 "$WORK/shell_contract.txt" | sed 's/^/   /'
need $CONTRACT_RC "the shell reads the child's stderr into daemon.log, shows the server's own reason, and hands a socket over"
sed 's/\[self captureChildText:text\];/(void)text;/' "$SRC/app/macos/ProjectLife.m" > "$WORK/PL_control.m"
if "$PY" "$TOOLS/shell_contract_check.py" --file "$WORK/PL_control.m" > "$WORK/shell_control.txt" 2>&1; then
  bad "the shell check passed on a copy with the child's stderr reader removed — a check that cannot fail proves nothing"
else
  ok "the same check fails when that reader is removed ($(grep -c FAIL "$WORK/shell_control.txt") failed check(s) in the control)"
fi

step "The window's contract, and the proof that the checker can fail (round 297)"
if python3 "$SRC/tools/ui_surface_check.py" --root "$SRC" > "$WORK/ui_surface.txt" 2>&1 \
   && python3 "$SRC/tools/ui_surface_control.py" --root "$SRC" > "$WORK/ui_surface_control.txt" 2>&1; then
  ok "$(head -1 "$WORK/ui_surface.txt")"
  ok "$(tail -1 "$WORK/ui_surface_control.txt")"
else
  bad "the window's contract check failed — see $WORK/ui_surface.txt and $WORK/ui_surface_control.txt"
fi

step "The moment field reads what a Mac without a date picker produces (round 297)"
if "$PY" "$SRC/tools/moment_input_check.py" --ui "$SRC/app/ui/app.js" > "$WORK/moment_input.txt" 2>&1; then
  ok "$(tail -1 "$WORK/moment_input.txt")"
else
  bad "the moment field misreads its input — see $WORK/moment_input.txt"
fi

step "Every core command is in the window, or named with a reason as deliberately CLI-only"
if python3 "$SRC/tools/ui_coverage.py" --pl "$SRC/target/release/projectlife" --app-src "$SRC/app" \
     --out "$SRC/docs/UI_COVERAGE.md" > "$WORK/ui_coverage.txt" 2>&1; then
  while read -r line; do say "   $line"; done < "$WORK/ui_coverage.txt"
  ok "the coverage table was regenerated from the core's own command list"
else
  bad "the coverage audit found an unaccounted command, a dead page call or a button with no handler"
  cat "$WORK/ui_coverage.txt"
fi

step "There is no room: the words name the disk, and the remedies can work (round 298)"
"$PY" "$TOOLS/no_room_words_check.py" --pl "$BIN" --work "$WORK/no_room" > "$WORK/no_room.txt" 2>&1
NO_ROOM_RC=$?
grep -cE "^  PASS" "$WORK/no_room.txt" | sed 's/^/   check(s) passed: /'
grep -E "^  FAIL" "$WORK/no_room.txt" | sed 's/^/   /' | head -5
tail -1 "$WORK/no_room.txt" | sed 's/^/   /'
need $NO_ROOM_RC "a stopped write is reported as a stopped write by heartbeat-check, healthcheck and doctor, with remedies that can work"

step "A repair creates only what is missing, and nothing else moves (round 300)"
# The failure mode the research is full of: an agent deletes part of a project, and the person has
# since written something by hand. A full restore would overwrite that work; a repair must not.
R="$WORK/repair"
mkdir -p "$R/proj/src"
printf 'alpha\n'  > "$R/proj/a.txt"
printf 'beta\n'   > "$R/proj/src/b.txt"
printf 'gamma\n'  > "$R/proj/src/c.txt"
"$BIN" --archive "$R/arch" init-archive "$R/arch" > /dev/null 2>&1
"$BIN" --archive "$R/arch" config set stopFreePercent 0 > /dev/null 2>&1
"$BIN" --archive "$R/arch" config set warnFreePercent 0 > /dev/null 2>&1
"$BIN" --archive "$R/arch" add "$R/proj" --profile all --yes > /dev/null 2>&1
M=$("$BIN" --archive "$R/arch" status proj --json | "$PY" -c 'import json,sys;print(json.load(sys.stdin)[0]["lastObservedAt"])')
sleep 1.1
rm "$R/proj/src/b.txt" "$R/proj/src/c.txt"
printf 'GAMMA BY HAND\n' > "$R/proj/src/c.txt"
printf 'ALPHA BY HAND\n' > "$R/proj/a.txt"
"$BIN" --archive "$R/arch" restore proj --at "$M" --into-project --missing --preview --json > "$WORK/repair-plan.json" 2>&1
need $? "a repair can be previewed as data"
PLAN=$("$PY" -c 'import json,sys;d=json.load(open(sys.argv[1]));print("%s/%s/%s/%s"%(d["create"],d["overwrite"],d["present"],d["deleteExtra"]))' "$WORK/repair-plan.json")
say "   plan: create/overwrite/present/delete = $PLAN"
[ "$PLAN" = "1/0/2/0" ] && ok "the plan creates one file, overwrites none, deletes none, and counts the two it leaves alone" \
                        || bad "the plan is not create=1 overwrite=0 present=2 delete=0: $PLAN"
"$BIN" --archive "$R/arch" restore proj --at "$M" --into-project --missing --yes --json > "$WORK/repair-done.json" 2>&1
need $? "the repair runs"
if [ "$(cat "$R/proj/src/b.txt")" = "beta" ]; then ok "the deleted file is back with its archived bytes"; else bad "src/b.txt is not the archived content"; fi
if [ "$(cat "$R/proj/src/c.txt")" = "GAMMA BY HAND" ]; then
  ok "the file the person wrote by hand after the deletion is untouched"
else
  bad "the repair replaced hand-written work (src/c.txt)"
fi
if [ "$(cat "$R/proj/a.txt")" = "ALPHA BY HAND" ]; then
  ok "a file edited after the moment is not reverted by a repair"
else
  bad "the repair reverted an edited file (a.txt)"
fi
"$BIN" --archive "$R/arch" restore proj --at "$M" --into-project --missing --clean --yes > "$WORK/repair-refuse.txt" 2>&1
[ $? -ne 0 ] && ok "a repair refuses to delete, and says which two flags disagree" || bad "--missing accepted --clean"
grep -q -- "--missing only creates" "$WORK/repair-refuse.txt" && ok "the refusal names the flags and what each does" \
                                                     || bad "the refusal does not explain itself"

step "Every message the program raised is still readable later (round 300)"
# A toast that vanished is not a record. One line per message, machine-readable, and the mass-change
# line has to carry the project and the command that repairs it.
MASS="$WORK/ledger"
mkdir -p "$MASS/proj/src"
i=0
while [ $i -lt 25 ]; do printf 'body %s\n' "$i" > "$MASS/proj/src/f$i.txt"; i=$((i + 1)); done
"$BIN" --archive "$MASS/arch" init-archive "$MASS/arch" > /dev/null 2>&1
"$BIN" --archive "$MASS/arch" config set stopFreePercent 0 > /dev/null 2>&1
"$BIN" --archive "$MASS/arch" config set warnFreePercent 0 > /dev/null 2>&1
"$BIN" --archive "$MASS/arch" add "$MASS/proj" --profile all --yes > /dev/null 2>&1
rm -f "$MASS/proj/src/"f*.txt
"$BIN" --archive "$MASS/arch" scan-once proj > /dev/null 2>&1
"$BIN" --archive "$MASS/arch" notifications --json > "$WORK/ledger.json" 2>&1
need $? "the ledger can be read as data"
"$PY" - "$WORK/ledger.json" <<'PYEOF'
import json,sys
v=json.load(open(sys.argv[1]))
rows=v.get("notifications",[])
mass=[r for r in rows if r.get("kind")=="mass"]
assert mass, "no mass line in the ledger"
m=mass[-1]
assert m.get("project")=="proj", "the mass line does not name the project"
assert "--missing --into-project" in m.get("body",""), "the mass line does not carry the repair command"
assert m.get("atIso","").count("T")==1, "the mass line has no ISO moment"
print("   mass line: project=%s kind=%s at=%s" % (m["project"], m["kind"], m["atIso"]))
PYEOF
need $? "the mass deletion left one line naming the project, the moment and the repair command"
"$PY" - "$MASS/arch/logs/notifications.jsonl" <<'PYEOF'
import json,sys
n=0
for line in open(sys.argv[1]):
    if line.strip():
        json.loads(line); n+=1
assert n>0
print("   %d line(s), every one of them JSON" % n)
PYEOF
need $? "the ledger file is JSON lines, not prose with quotes in it"

step "A configuration change reaches a running daemon without a restart (FR-CFG-3)"
# The requirement the specification has carried, unbuilt, for many rounds. The proof is not a flag
# somewhere: it is the interval in force changing in a process that was not restarted, plus a record
# another process can read.
CF="$WORK/cfg"
mkdir -p "$CF/proj"
printf 'x\n' > "$CF/proj/f.txt"
"$BIN" --archive "$CF/arch" init-archive "$CF/arch" > /dev/null 2>&1
"$BIN" --archive "$CF/arch" config set stopFreePercent 0 > /dev/null 2>&1
"$BIN" --archive "$CF/arch" config set warnFreePercent 0 > /dev/null 2>&1
"$BIN" --archive "$CF/arch" config set intervalSeconds 5 > /dev/null 2>&1
"$BIN" --archive "$CF/arch" add "$CF/proj" --profile all --yes > /dev/null 2>&1
"$BIN" --archive "$CF/arch" daemon run > "$WORK/cfg-daemon.txt" 2>&1 &
CFPID=$!
i=0
while [ $i -lt 60 ]; do
  grep -q "daemon started" "$CF/arch/logs/projectlife.log" 2>/dev/null && break
  sleep 0.2; i=$((i + 1))
done
say "   daemon pid $CFPID, interval 5 s"
"$BIN" --archive "$CF/arch" config set intervalSeconds 1 > /dev/null 2>&1
i=0
while [ $i -lt 60 ]; do
  grep -q "5000 ms -> 1000 ms" "$CF/arch/logs/projectlife.log" 2>/dev/null && break
  sleep 0.2; i=$((i + 1))
done
grep -q "configuration re-read without a restart" "$CF/arch/logs/projectlife.log" && \
  ok "the running daemon re-read its configuration when the file changed" || \
  bad "the daemon did not notice the change"
grep -q "5000 ms -> 1000 ms" "$CF/arch/logs/projectlife.log" && \
  ok "the interval in force changed from 5 s to 1 s in a process that was never restarted" || \
  bad "the interval was logged as re-read but not applied"
kill -HUP $CFPID 2>/dev/null
i=0
while [ $i -lt 40 ]; do
  grep -q "SIGHUP: the configuration file is unchanged" "$CF/arch/logs/projectlife.log" 2>/dev/null && break
  sleep 0.2; i=$((i + 1))
done
grep -q "SIGHUP: the configuration file is unchanged" "$CF/arch/logs/projectlife.log" && \
  ok "SIGHUP with an unchanged file is answered with the truth, not with a pretend re-apply" || \
  bad "SIGHUP was not answered"
"$PY" - "$CF/arch/config_state.json" <<'PYEOF'
import json,sys
v=json.load(open(sys.argv[1]))
assert v["reloads"]==1, "expected exactly one re-read, got %r" % v["reloads"]
assert v["intervalSeconds"]==1, "the record must show the interval now in force"
assert v["changedKeys"]==["intervalSeconds"], "the record must name what changed"
print("   %s" % json.dumps({"reloads": v["reloads"], "intervalSeconds": v["intervalSeconds"], "changedKeys": v["changedKeys"], "reason": v["reason"]}))
PYEOF
need $? "the re-read is recorded where another process can check it"
kill -TERM $CFPID 2>/dev/null
i=0
while [ $i -lt 60 ]; do
  kill -0 $CFPID 2>/dev/null || break
  sleep 0.2; i=$((i + 1))
done
kill -9 $CFPID 2>/dev/null
wait $CFPID 2>/dev/null
grep -q "daemon stopped on signal" "$CF/arch/logs/projectlife.log" && ok "the daemon stopped cleanly on SIGTERM" || bad "the daemon did not stop cleanly"


step "The app says which build it is, and the answer is the files' own (round 298)"
APP_BIN=${APP_BIN:-$SRC/app/target/release/projectlife-ui}
if [ -x "$APP_BIN" ]; then
  "$PY" "$TOOLS/build_identity_check.py" --app "$APP_BIN" --pl "$BIN" --page "$SRC/app/ui/app.js" \
      --shell "$SRC/app/macos/ProjectLife.m" --work "$WORK/build_id" > "$WORK/build_id.txt" 2>&1
  BUILD_RC=$?
  grep -cE "^  PASS" "$WORK/build_id.txt" | sed 's/^/   check(s) passed: /'
  grep -E "one-line identity" "$WORK/build_id.txt" | sed 's/^/   /' | head -2
  tail -1 "$WORK/build_id.txt" | sed 's/^/   /'
  need $BUILD_RC "the window's build line is the sha256 of the binaries it runs, hashed again by Python"
else
  say "   SKIP (not a pass): no interface server binary, so the build line could not be asked for"
fi

step "The macOS shell compiles for arm64 (or says plainly that it was not checked)"
SDKM=$(ls -d /tmp/macos-sdk/MacOSX*.sdk 2>/dev/null | head -1)
if [ -n "$SDKM" ] && command -v clang-19 > /dev/null 2>&1; then
  clang-19 --target=arm64-apple-macos11.0 -isysroot "$SDKM" -fobjc-arc -I "$SRC/app" \
      -Wno-deprecated-declarations -O2 -c "$SRC/app/macos/ProjectLife.m" -o "$WORK/ProjectLife.o" \
      > "$WORK/shell_cc.txt" 2>&1
  need $? "the shell compiles for arm64 against $SDKM"
  say "   $(file -b "$WORK/ProjectLife.o" | cut -c1-70)"
  say "   not executed: there is no Mac here, so the window, AppKit and WebKit are compiled and never run"
else
  say "   SKIP (not a pass): no macOS SDK and no clang-19 here, so the shell was not compiled in this run"
fi

step "The menu: one list, three fronts, and every entry driven for real (round 299)"
# The registry is read, then driven. The static half refuses a menu that names a command the core
# does not have, a view the window cannot draw, or an entry the page does not know how to perform;
# the live half starts the real interface server on a throw-away archive and runs every entry,
# comparing the output with the core's own and trying every confirmation twice — without it and with
# it. The control proves the checker can fail (eleven faults, eleven caught).
if [ -x "$APP_BIN" ]; then
  "$PY" "$TOOLS/menu_contract_check.py" --root "$SRC" --static-only > "$WORK/menu_static.txt" 2>&1
  need $? "the menu's static contract holds (registry, views, page flows, the shell)"
  grep -cE "^\[PASS\]" "$WORK/menu_static.txt" | sed 's/^/   static rule(s) passed: /'

  "$PY" "$TOOLS/menu_control.py" --root "$SRC" > "$WORK/menu_control.txt" 2>&1
  need $? "the menu checker itself can fail (a checker that cannot fail proves nothing)"
  tail -1 "$WORK/menu_control.txt" | sed 's/^/   /'

  "$PY" "$TOOLS/menu_contract_check.py" --root "$SRC" --pl "$BIN" --app-bin "$APP_BIN" \
      --work "$WORK/menu_live" > "$WORK/menu_live.txt" 2>&1
  need $? "every menu entry does what it says on the real server"
  grep -E "^\[PASS\]" "$WORK/menu_live.txt" | wc -l | sed 's/^/   live rule(s) passed: /'
  tail -1 "$WORK/menu_live.txt" | sed 's/^/   /'
  say "   full log: $WORK/menu_live.txt"
else
  say "   SKIP (not a pass): no interface server binary, so the menu could not be driven"
fi

step "Who made it, and what it is for: one source, and the shipped bytes agree (round 302)"
# The values live in src/brand.rs and nowhere else. This tool re-reads that file itself, asks the
# built core for its own answer, and then looks for those bytes in every file and binary that must
# carry them — and requires the page and the three shells to hold no hand-typed copy.
"$PY" "$TOOLS/brand_check.py" --root "$SRC" --pl "$BIN" --app "$APP_BIN" > "$WORK/brand.txt" 2>&1
BRAND_RC=$?
grep -cE "^  PASS" "$WORK/brand.txt" | sed 's/^/   check(s) passed: /'
grep -E "^  FAIL" "$WORK/brand.txt" | sed 's/^/   /' | head -5
tail -1 "$WORK/brand.txt" | sed 's/^/   /'
need $BRAND_RC "the author, the address and the sentence about the purpose are in every place that must carry them — and nowhere else"
# The control: the same check, on a copy of README.md with the address replaced, must go red — and
# must go red for that reason, not for some other one.
cp "$SRC/README.md" "$WORK/README.address-removed.md"
sed 's/oxunjonub@gmail.com/author@example.invalid/g' "$SRC/README.md" > "$WORK/README.address-removed.md"
if "$PY" "$TOOLS/brand_check.py" --root "$SRC" --pl "$BIN" --app "$APP_BIN" --control \
     --override "$SRC/README.md=$WORK/README.address-removed.md" > "$WORK/brand_control.txt" 2>&1; then
  bad "the brand check passed on a copy with the address removed — a check that cannot fail proves nothing"
else
  if grep -q "README.md carries 'oxunjonub@gmail.com'" "$WORK/brand_control.txt"; then
    ok "the same check fails when the address is removed from one file ($(grep -c '^  FAIL' "$WORK/brand_control.txt") failed check(s) in the control)"
  else
    bad "the control went red, but not for the removed address — see $WORK/brand_control.txt"
  fi
fi
# The generated header the three shells include must be exactly what tools/brand.py produces: a
# stale header would ship a different name from the one the core prints.
if "$PY" "$TOOLS/brand.py" check-fresh "$SRC/app/pl_brand.h" > "$WORK/brand_header.txt" 2>&1; then
  ok "$(tail -1 "$WORK/brand_header.txt")"
else
  bad "app/pl_brand.h is out of date with src/brand.rs — see $WORK/brand_header.txt"
fi

step "One delivery, one version — the version is typed in five files, so it is checked (round 302)"
"$PY" "$TOOLS/version_check.py" --root "$SRC" > "$WORK/version.txt" 2>&1
VERSION_RC=$?
tail -3 "$WORK/version.txt" | sed 's/^/   /'
need $VERSION_RC "VERSION, both Cargo.toml files, both SHELL_VERSION constants, the Info.plist token and the built binaries all name the same version"
# The control: pretending the VERSION file says something else must go red.
if "$PY" "$TOOLS/version_check.py" --root "$SRC" --expect 9.9.9 --quiet > "$WORK/version_control.txt" 2>&1; then
  bad "the version check passed while the VERSION file disagreed with everything else — it cannot fail"
else
  ok "the same check fails when the VERSION file disagrees with the rest"
fi

printf '\n================\n'
if [ $FAILED -eq 0 ]; then
  printf 'VERIFY: ALL CHECKS PASSED (%s steps)\n' "$STEP"
  exit 0
fi
printf 'VERIFY: %s CHECK(S) FAILED of %s steps\n' "$FAILED" "$STEP"
exit 1
