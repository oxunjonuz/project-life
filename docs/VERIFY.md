# Manual verification on a real machine (about ten minutes)

Everything here can be automated (`tools/verify.sh` does exactly these checks end to end and prints
PASS/FAIL per step), but a person should run it once on the machine that will actually use it —
because that is the only place where the promise is worth anything.

Prepare an archive on a **different disk** than the project, and a throwaway project folder.

```sh
B=projectlife                     # or the full path to the binary
A=/Volumes/backup/projectlife     # the archive
P=~/tmp/pl-test                   # a project to test with
mkdir -p $P && echo "hello" > $P/a.txt && mkdir -p $P/sub && echo "x" > $P/sub/b.txt
```

## 1. Setup

```sh
$B init-archive $A
$B add $P --name pltest
$B status pltest --skipped
```

Expect: an estimate, a warning if the archive is on the same disk, one initial snapshot line, the
project state `active`, and the skipped list with reasons and rules.

## 2. A change, an undo

```sh
echo "changed" > $P/a.txt
mv $P/sub/b.txt $P/sub/b-renamed.txt
rm $P/a.txt
$B scan-once pltest
$B log pltest
```

Expect a `put`, a `move` and a `delete` in the log, and no `mass` event for a small change.

## 3. Restore — by the program and without it

```sh
$B restore pltest --at now --to /tmp/pl-restored-by-program
python3 tools/recover.py --archive $A export-tree --project pltest --at now --out /tmp/pl-restored-by-python
diff -r /tmp/pl-restored-by-program /tmp/pl-restored-by-python
cmp $P/sub/b-renamed.txt /tmp/pl-restored-by-program/sub/b-renamed.txt
```

Expect: no differences between the two restores, and no difference against the file on disk.

## 4. The promise, measured on your machine

```sh
$B drill pltest
```

Expect `VERDICT: PASS` and a plausible restore time. This is the number to trust: it was measured
here, on this disk, with your data.

## 5. What must not be in the archive

```sh
echo "SECRET=1" > $P/.env
mkdir -p $P/node_modules/dep && echo "dep" > $P/node_modules/dep/i.js
$B scan-once pltest
$B why pltest .env
grep -rl "SECRET=1" $A 2>/dev/null | head        # expect: nothing
```

Expect `why` to name `secret` and the exact rule, and no occurrence of the secret in the archive.

## 6. A simulated disaster

```sh
$B mark pltest "before the disaster"
rm -rf $P/sub $P/a.txt
$B scan-once pltest
$B last-good pltest
$B panic pltest --yes
```

Expect a mass event (`mass_delete`), a notification containing a ready command, `last-good` pointing
just before the deletion, and `panic` restoring into a **separate** folder — the project folder must
be untouched.

## 7. Integrity and tampering, in one minute

```sh
$B check pltest --deep
$B audit-archive --update
BLOB=$(find $A/projects -path '*blobs*' -type f | head -1)
chmod u+w "$BLOB" && printf 'X' | dd of="$BLOB" bs=1 seek=0 conv=notrunc && chmod u-w "$BLOB"
$B audit-archive                # expect: CHANGED inside the archive
$B check pltest --deep          # expect: the blob is reported as corrupted
$B restore pltest --at now --to /tmp/pl-after-damage ; echo "exit=$?"
```

Expect exit code 2, a list of the files that could not be restored, no empty files, and the other
files restored.

## 8. The timer mode (no daemon)

```sh
$B scan-once --all
sleep 2
$B scan-once --all
$B status pltest --risk
```

Expect both cycles to succeed, a heartbeat file in the archive, and `status` to print the measured
observation window (median and p95) plus the risk list.

## 9. The notification trigger, timed on your machine

```sh
$B daemon run &                      # leave the interval at its default (5 s)
sleep 2
$B daemon status                     # expect: trigger: inotify (N directories watched)
date +%s%3N ; echo "changed" >> $P/a.txt
$B log pltest --path a.txt | tail -3
```

Expect the new version to appear about 1.5 s after you wrote it, and `daemon status` to show
`triggerCycles` growing while `periodicCycles` keeps moving as well. Then start the same run with
notifications off and repeat — the wait becomes up to one interval:

```sh
kill %1
$B daemon run --no-watch &
sleep 2
$B daemon status                     # expect: trigger: off (watchTriggers=false)
```

`tools/watch_latency.py` measures both cases with many samples and prints every one of them. On the
machine this build was made on: with notifications the median was 1.55 s at a 5 s interval and
1.53 s at a 30 s interval; without them it was 3.4 s at 5 s (samples 1.7-4.6 s) and 19.5 s at 30 s
(samples 13.7-29.4 s). If nothing is listening on the filesystem, `daemon status` says so in the
`trigger:` line instead of pretending.

## 10. Universal mode: a folder that is not code (two minutes)

```sh
mkdir -p /tmp/pltest-docs && cd /tmp/pltest-docs
printf x > report.docx; printf x > table.xlsx; printf x > notes.txt
printf x > archive.zip; printf x > clip.mp4; printf 'SECRET=1\n' > .env
printf x > ~$draft.docx          # a Word lock file

$B detect /tmp/pltest-docs                    # expect: office, confidence > 80 %
                                             #   .env and ~$draft.docx under "Excluded by the
                                             #   hard-coded safety rules"
$B detect /tmp/pltest-docs --json | head -20  # the same answer as data
$B presets                                    # the ten profiles and their limits

$B add /tmp/pltest-docs --name pltest-docs --preset auto --yes
$B status pltest-docs
$B why pltest-docs report.docx                # tracked
$B why pltest-docs archive.zip                # NOT tracked — reason not_in_preset (rule: office)
$B why pltest-docs .env                       # NOT tracked — reason secret
```

Then look at what was recorded, not at what was printed:

```sh
python3 - <<'EOF'
import glob, json
d = glob.glob("/path/to/archive/projects/*/project.json")
m = [json.load(open(p)) for p in d]
m = [x for x in m if x["name"] == "pltest-docs"][0]
print(m["preset"]["id"], m["preset"]["source"], m["preset"]["confidence"])
print(m["preset"]["evidence"]["extensions"])
print(m["settings"]["include"], m["settings"]["filterMode"])
EOF
```

Expect `office auto`, an evidence block naming the extensions it saw, and `filterMode: allow`. The
journal holds the same decision as an event: `grep preset_applied .../events/*.jsonl`.

## 11. Clean up the test

```sh
$B archive-delete pltest            # asks you to type the name
$B list
```

## Recording the result

Write down, in one line each: the restore time from step 4, the number of files checked, and
anything that surprised you. A promise that is not written down is not a promise; the numbers from
this page are the ones worth keeping.

## 12. The round-292 checks (retention, schedulers, the agent window)

These are the steps `tools/verify.sh` runs as steps 30–33 of 34, written out so they can be repeated
by hand. Each one uses an instrument that does not share a blind spot with the program.

### 12.1 A retention policy keeps what it says it keeps

```sh
# a project with a spread-out history (tools/backdate.py moves historyStartsAt with it)
python3 tools/backdate.py <archive>/projects/<id> 60
python3 tools/journal_state.py <archive>/projects/<id> src/app.ts <ts> [<ts> …] > before.txt

$B prune demo --policy "7d:all,30d:1/day,365d:1/month" --dry-run     # read the plan first
$B prune demo --policy "7d:all,30d:1/day,365d:1/month" --yes
python3 tools/journal_state.py <archive>/projects/<id> src/app.ts <ts> [<ts> …] > after.txt

diff before.txt after.txt     # 0 differences for every moment >= now - 7 days
$B check demo --deep          # every referenced blob present, no unreferenced blobs left
```

`journal_state.py` is a second reader written from the storage format: when it agrees with the
program about the state at a moment, that is two implementations agreeing. The verification step also
requires that two further observation cycles create **no** anchors (a stored policy is never applied
by itself) and that `project.json → settings → retentionPolicy` holds the policy.

### 12.2 The heartbeat answers a scheduler

```sh
$B scan-once --all && $B heartbeat-check ; echo $?      # 0  (fresh)
printf '%s\n' "$(( $(date +%s) * 1000 - 600000 ))" > <archive>/heartbeat
$B heartbeat-check ; echo $?                            # 1  (stale)
$B healthcheck ; echo $?                                # 1, with the fix printed next to the problem
```

### 12.3 An agent can read the archive and cannot write it

```sh
python3 tools/mcp_client.py --archive <archive> --binary target/release/pl-mcp \
        --check-readonly --rate-test
python3 tools/mcp_audit.py --root .
```

The client is an independent implementation of the protocol side: it asks for the tool list (nine
names, no write operation among them), calls every tool, tries to call three write names and requires
a refusal, presses the rate limiter, and hashes the whole archive before and after — exactly one file
may differ (`logs/mcp.log`), and a run where *nothing* differs fails, so the check cannot pass by
being blind. The audit reads the two files that make up the reading surface, removes comments and
string literals, and refuses a list of write symbols; it also runs a positive control on a copy with
a write call injected by hand.

### 12.4 Eight read-only commands change nothing

The verification step hashes the whole archive (minus `logs/`) before and after `recent`,
`status --compact`, `size`, `gc`, `suggest`, `prompt`, `list --sort risk` and `log --grep`, and
requires the same hash. A read-only command that needs to write to answer would be a design error.

## Round 294 — the bookkeeping (steps 38–42 of `tools/verify.sh`)

| Step | What it checks | What makes it able to fail |
|---|---|---|
| 38 | A partial pass leaves `cache/base.jsonl` byte-identical, appends exactly one delta record, reads no journal file in full and rewrites no base; the reading grows 1.35× when the state grows 10× (a 300-file and a 3000-file project, both measured) | a journal file that no reader can read is in place: a full pass must fail, the partial pass must still store its change |
| 39 | `tools/cache_vs_journal.py` — a second implementation of the format — agrees with the journal on both projects | one entry's hash is replaced with zeros by hand, on a path the delta does not mention: the reader must catch it |
| 40 | The delta is folded when it passes its cap, the folded file is under the cap, and the state does not change | the cap is set to 400 bytes, so the folding really happens (6 compactions) |
| 41 | Ten partial passes in a row, and ten runs of `tests/round294.rs` | a leaked daemon or a flaky ordering test fails the step |
| 42 | What one file costs now: the process floor, the partial pass, an empty scope and a full pass over 3 000 files, all measured in the same run | the gate is a ratio against the full pass in the same session, so a slow machine moves both numbers |

The proof that the tests themselves bite is `tools/mutations.py`: 51 mutations, each one a fault the
suite must catch, 0 survivors and 0 source drift (step 28).
