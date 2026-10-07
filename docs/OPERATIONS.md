# Operations

## Move the archive to another disk

```sh
cp -a /old/archive /new/archive           # or rsync -a; the archive is portable by design
projectlife archive-move /new/archive     # rewrites the location file and verifies it looks like an archive
projectlife check --deep                  # optional but cheap confidence
```

All paths inside the archive are relative, so nothing else changes. Verify the copy with
`cd /new/archive && find . -type f -exec sha256sum {} \; > /tmp/x` if you want certainty, or use
`pl export` + `pl import` for a verified move.

## Free space without losing what matters

```sh
projectlife status --risk                  # how big is it, how much free space is left
projectlife prune my-app --before "2026-09-01"        # shows the plan, then asks for confirmation
projectlife export my-app --from "2026-08-01" --to "2026-09-01" --out /backup/august
projectlife export-and-prune my-app --before "2026-09-01" --out /backup/august
```

`prune` keeps the state at the boundary (the anchors), so you can always restore *to* that moment;
everything superseded before it is removed and the blobs are freed. It never runs on a schedule.

## Integrity check

```sh
projectlife check my-app            # fast: references, sizes, dangling blobs, journal parsing
projectlife check my-app --deep     # re-reads every blob and compares sha256 with its name
projectlife check my-app --deep --fix --yes   # remove dangling blobs and temporary files
python3 recover.py --archive <ARCHIVE> check --project my-app --deep   # independent second opinion
```

`--fix` deletes only blobs that no event references and files in `tmp/`. Journal records are never
deleted, and corrupted blobs go to `quarantine/` instead of being removed.

## Detecting tampering with the archive

```sh
projectlife audit-archive --update      # write today's digest of every archive file
# ...later, or from another machine...
projectlife audit-archive               # nothing is written; differences are reported
```

Output names the exact files that were removed or changed inside the archive. This is detection, not
protection: an agent with your rights can delete the archive, but it cannot do so unnoticed once a
digest exists somewhere else (keep a copy of `manifest/` off the archive disk if you care).

## The daemon is not running

```sh
projectlife doctor            # says whether the heartbeat is fresh, and what to do
projectlife daemon start      # systemd user unit / LaunchAgent
projectlife daemon install    # writes both a resident unit and an equivalent timer entry
```

Any cycle is better than no cycle. If a resident process is unwanted (or keeps being killed), use
the timer mode: `projectlife scan-once --all` from cron, launchd or a systemd timer. The lock makes
overlap impossible, and the promise's accuracy becomes the timer period — the program prints that.

## A prune was interrupted

`prune` writes its phase to `projects/<id>/prune.journal`. If the machine died in the middle, the
next cycle finishes or rolls it back automatically, and `doctor` reports the pending file. To do it
now, or to see what was decided:

```
pl recover <project>
```

The journal is never rewritten by the recovery, and either the old journal is put back or the new one
is kept with its metadata corrected. `pl check <project> --deep` afterwards is the confirmation.

## The daemon will not start: a lock file is left

The lock carries the pid of the process holding it. A lock left by a process that is gone (`kill -9`,
a power cut) is taken over by the next cycle by itself; a live holder is never displaced. If you want
it gone without waiting for a cycle:

```
pl doctor                 # tells you whether the holder is alive
pl doctor --fix-lock      # removes it only if the holder is provably gone
pl doctor --fix-lock --force   # removes it whatever it says (you must be sure)
```

## Observation triggers: what is watching and what it costs

```sh
pl daemon status                     # the trigger line: mode, watched directories, events, cycles
pl daemon run --no-watch             # periodic pass only (same as config watchTriggers=false)
pl config set watchTriggers false    # permanent: notifications off
pl config set debounceMs 1500        # quiet time per path before a pass starts
pl config set triggerResyncCycles 12 # how often the watch set is rebuilt (self-healing)
```

`daemon status` reads `<archive>/watch_state.json`, which the daemon rewrites as it works. What it
shows and what it means:

| Field | Meaning |
|---|---|
| `mode` | `inotify` (a real backend), `off` (you asked for it), `unavailable` (the reason is in `describe`) |
| `watchedDirs` | directories with a live watch — tracked ones only; excluded folders are not watched |
| `events` | paths that changed, as reported by the kernel |
| `triggerCycles` / `periodicCycles` | which trigger started each pass |
| `overflows` | `IN_Q_OVERFLOW` — notifications **were** lost and a full pass was started because of it |
| `lostEvents` | notifications thrown away by the test injector (`PROJECTLIFE_DROP_EVENTS=1`); always 0 in normal use |

On a filesystem where the kernel cannot report (macOS and Windows in this build), the daemon logs
`trigger: unavailable — …` and the interval is the promise again. Nothing else changes.

## Recovery without the program

```sh
python3 recover.py --archive <ARCHIVE> list
python3 recover.py --archive <ARCHIVE> tree --project my-app --at "2026-10-05 14:00"
python3 recover.py --archive <ARCHIVE> export-tree --project my-app --at "2026-10-05 14:00" --out /tmp/rec
```

By hand: the journal (`events/YYYY-MM.jsonl`) is text; a `put` line names the `hash`; the file
`blobs/<hh>/<hh>/<hash>` **is** the contents. Copy it, rename it, verify with `sha256sum`.

## A project's path changed

```sh
projectlife relink my-app /new/path/to/my-app
projectlife scan-once my-app
```

## Taking a project off observation

```sh
projectlife pause my-app      # temporary; the pause is recorded as a gap
projectlife resume my-app
projectlife remove my-app     # history is kept; nothing is deleted
projectlife archive-delete my-app --export-first /backup/my-app   # erases the history, asks for the name
```

## Weekly five-minute routine

```sh
projectlife doctor                 # archive reachable, space, heartbeat, errors
projectlife status --risk          # measured window, gaps, risk items
projectlife audit-archive --update # keep a rolling digest
projectlife drill my-app           # prove the promise still holds on this machine
```

## Schedulers: making "it stopped" visible

A recorder that fails quietly is worse than no recorder, so the promise has an exit code:

```sh
pl heartbeat-check            # 0 fresh (60 s by default), 1 stale, 2 stale while a daemon claims to run
pl heartbeat-check --max-age 300 --json
pl healthcheck [--strict]     # heartbeat + mode + per-project ages + pending recoveries + all doctor findings
```

The checks are meant for cron, systemd timers and launchd: `pl` opens no socket (NFR-SEC-1), and the
shell decides who to tell. Every problem is printed with the command that fixes it next to it, so the
message in an inbox is actionable:

```
ERROR heartbeat is 3661 s old (limit 60 s), mode: stopped — nothing has been observed recently
      fix: pl scan-once --all   # or: pl daemon status / pl daemon start
WARN  demo: archive and project share one volume — losing the volume loses both
      fix: move the archive to another disk: pl archive-move <new-path>
```

`healthcheck` is safe to run while a daemon runs: it takes no lock, reads no project, and writes
nothing (the verifier hashes the whole archive around eight read-only commands and requires the same
hash back).

## Retention as an operation

Thinning is a lifecycle operation, so it goes through the same door as `prune`:

```
plan      pl prune <project> --policy "7d:all,30d:1/day,365d:1/month" --dry-run
apply     pl prune <project> --policy "…" --yes            (or: pl retention <project> apply)
check     pl check <project> --deep
recover   pl recover <project>        # only if something was interrupted; it says so when asked
```

Order of operations that matters: the plan is printed first and it names the moments that will stay
restorable. A policy is refused outright when it would keep nothing at all, and `--before` may not be
combined with `--policy` because they keep different promises. `settings.retentionPolicy` is a note —
the daemon never reads it — so an archive is never thinned while you sleep unless a scheduler of
*yours* runs the command.

## What the operation log is for

`<archive>/logs/operations.jsonl` records what the *program* did (restore, panic, prune, retention,
export, import, drill) in one JSON object per line. The project journal records what the *filesystem*
did; a restore changes the disk without changing what was observed, so "what did it do to my files?"
needs the second file. `pl recent` shows both side by side, `pl undo` reads the last restore, and
`pl suggest` uses it to notice a rehearsal that has never happened.

