# Limitations

Everything here is either a measured property of this build or a boundary the program will not
cross. If a claim is not in this file, it is not promised.

## The promise and its window

> While a project is added, the program is running and the archive is reachable, any **observed**
> state of the project can be restored to within one observation interval (5 seconds by default).
> States that appeared and disappeared between two observations are not recoverable. The archive is
> kept until you clear or delete it yourself.

1. **Observation window.** A file changed several times between two checks leaves only the last read
   state. The intermediate bytes no longer exist anywhere by the time of the check — this is not a
   bug, it is physics of a polling observer.
2. **Created and removed between two checks** means no version at all.
3. **Observation gaps** (program not running, machine off, archive disk unplugged, project paused)
   are recorded as `gap` events and shown by `restore`, `tree` and `why`. States inside a gap are
   lost. `pl drill` prints the window actually achieved on your machine.
4. **The window is measured, not assumed.** `status --risk` prints median and p95 of the interval
   actually achieved; under load the interval grows (up to `maxIntervalSeconds`).
5. **One initial copy.** Adding a project stores the current state of matching files once, with an
   estimate and a confirmation. After that only changes are stored. Unchanged files are never copied
   again, and the program never writes anything inside the project folder (except an explicitly
   requested `restore --into-project`).

## What is deliberately not copied

6. **Secrets** (`.env`, keys, credentials) are skipped by default, with reason and rule. If you turn
   `includeSecrets` on, remember: **the archive is not encrypted**.
7. **Dependencies, build output, `.git` contents, binaries (in the `source` profile), files above
   the size limit** are skipped. Every skip is recorded as a `skip` event and explainable with
   `pl why <project> <path>`.
8. **Symlinks are recorded, never dereferenced.** Their target contents are never copied. On restore
   a link is recreated only when its target is inside the project; otherwise you get a warning.
9. **Hard links** are treated as ordinary files by path; contents may be deduplicated by hash.
10. **Empty directories are not restored.** Files inside them are.

## Data and disk

11. **The archive is not encrypted** in this version.
12. **Losing the archive disk loses the history.** Mitigations that exist today: a different disk,
    `pl export`, `pl audit-archive`, and running from an external timer rather than a daemon.
13. **An agent running as your user can delete the archive too.** The program does not pretend
    otherwise. What it does provide is *detection*: `pl audit-archive` compares a digest of every
    archive file and reports what was removed or changed inside the archive.
14. **No automatic deletion, ever.** `prune` and `archive-delete` only run when you ask, and both
    require typing the project name. Nothing is pruned on a schedule.
15. **Nothing is written to the network.** No telemetry, no accounts, no update checks.

## Restore

16. Restoring writes into a **new folder by default**. Writing into the project requires
    `--into-project` and a confirmation, and the program first takes a fresh observation so the
    current state is in the archive too.
17. `--clean` (deleting files that did not exist at that moment) is only allowed together with
    `--into-project`, lists what it will delete, and asks twice when the change is large.
18. If a blob is missing or corrupted, the affected files are reported and **no empty or partial
    file is written**; the rest is restored and the exit code is 2 (partial success).
19. Moments earlier than `historyStartsAt` are refused with the available range. After `prune`, the
    state at the boundary survives; everything superseded before it is gone.

## Performance, measured (not promised)

The promise is NFR-PRF-2: **an ordinary cycle over 10 000 unchanged files takes at most 1 second.**
`tools/perf.py` builds the projects from scratch, runs the shipped binary three times and reports the
median. It was run repeatedly on **two filesystems** on 2026-10-05 (aarch64 Linux, 10 cores), and
repeated runs of the same command differ by up to ~25 % — so the table quotes the run that is shipped
as evidence, and says so.

| filesystem | 10 000 unchanged files | 1 file + 50 000 in `node_modules` | the same with `--skipped` |
|---|---|---|---|
| the owner's mounted disk (`/run/host_mark/Users`, 100 % full) | **0.555 s** (0.907 / 0.546 / 0.555) | **0.007 s** | **1.614 s** (1.61 / 1.06 / 2.11) |
| the container's overlay filesystem (`/tmp`) | **0.501 s** (0.768 / 0.493 / 0.501) | **0.010 s** | **0.085 s** (0.085 / 0.083 / 0.092) |

Evidence: `evidence/perf_289_hostdisk.json` and `evidence/perf_289_overlay.json` are those two runs,
verbatim. Earlier runs gave 0.343–0.524 s for the 10 000-file case and 0.092–3.961 s for the counted
case; the counted column is the noisiest because it is the one that reads every entry of a 50 000-file
directory on an almost-full disk.

**Round 291 re-measured the same promise and it improved by roughly a factor of two**, from the same
command on the same container: **0.277 s** median for 10 000 unchanged files (`tools/verify.sh`, step
22, this round). The reason is worth writing down: the glob matcher ran the whole secret, temporary
and profile pattern list over every file through a matcher that allocated two `Vec<char>` per call,
which cost more than reading the file's metadata did. The matcher now takes a byte-wise path for
ASCII patterns and names and allocates nothing (`src/glob.rs`), so detection of 100 000 files fell from
1.46–2.36 s to **0.46–1.63 s** (median 0.49 s) on the same fixture. The following detection is the
measurement the brief asked for: `tools/detect_bench.py --files 100000 --runs 3`, on the container's
overlay filesystem, 2026-10-05 — 459 ms / 711 ms / 1634 ms, every run inside the 2 s target, the 5 s
design cap (FR-PRE-8) never in question. The same tool prints the floor: readdir alone 29 ms for
100 000 entries and readdir plus one metadata call per file 221 ms, so the program's own work is now a
small part of its own measurement rather than the whole of it. On a busy container the same fixture
has been seen at 4.7 s before this change and 2.4 s after it — the spread belongs to the shared disk,
and it is printed by the tool rather than averaged away.

Read this as: the promise holds on both filesystems with a margin of about 2x, it does not depend on
which of the two the archive lives on, and the cost of an ordinary cycle is dominated by walking the
project and parsing the state cache rather than by the archive. A network share has not been measured
and nothing is claimed for it. The `--skipped` column is also the positive control for item 20:
counting what was skipped is **~10x to ~230x** slower than not counting it, so the two modes cannot be
confused — and that is exactly the work the ordinary cycle no longer does.

## The partial pass, measured (round 293)

Round 293 added the partial pass: a notification-driven pass walks only the paths the notification
named. `pl partial-pass` runs exactly the pass the daemon runs, so the cost can be measured from
outside the process, and `tools/partial_bench.py` measures it next to the full pass on the same
fixture with the shipped binary — 10 000 files, 5 runs per measurement, the owner's mounted disk,
2026-10-05.

| pass | median | p95 | max | min | vs the full pass |
|---|---|---|---|---|---|
| full (`scan-once`, unchanged) | 296.3 ms | 327.5 | 327.5 | 291.4 | 1.00 x |
| partial, 1 file notified | 90.8 ms | 130.4 | 130.4 | 87.2 | **0.31 x** |
| partial, 10 files notified | 103.1 ms | 111.7 | 111.7 | 91.2 | 0.35 x |
| partial, 100 files notified | 159.2 ms | 221.5 | 221.5 | 130.1 | 0.54 x |
| partial, one directory notified (100 files in scope) | 121.9 ms | 139.4 | 139.4 | 98.1 | 0.41 x |
| **partial, empty scope** (a path that does not exist) | **70.7 ms** | 80.2 | 80.2 | 69.0 | 0.24 x |

**The expectation "a one-file pass is much faster than a full pass" is only partly met, and the last
row is why.** The walk and the storage of one file cost about 20 ms; the other ~71 ms is the *fixed
cost of any pass*, and it grows with the size of the project rather than with the scope: on the same
10 000-file fixture the pass reads a 1.94 MB `state.json` and a 2.41 MB journal and rewrites the state
cache whole. The scaling run says the same: at 1 000 files the empty-scope pass costs 17.3 ms and the
one-file pass 18.5 ms against a 36.7 ms full pass (0.50 x). So:

* the partial pass is **~3x cheaper** than the full pass at 10 000 files for a one-file change, and
  ~1.9x for a hundred-file change;
* its cost is **not** independent of the size of the project (task-level expectation: "the load
  depends on the number of affected directories, not on the number of files in the project" — that
  holds for the *walk*, which is the part that changed, and not for the bookkeeping, which did not);
* at a few hundred files the difference all but disappears (1 000-file fixture: 100 notified files
  cost 46.2 ms, more than the 36.7 ms full pass over an unchanged tree), because a cheap walk cannot
  pay for an expensive state load.

The fix is known and not done in this round: a partial pass needs the last sequence number, the last
observed time and the state cache, not the whole journal and not a full rewrite of the cache. Making
that path exist means the state can no longer be "read whole, written whole", which is a change to
the format's assumptions and deserves its own round with its own campaign. Until then the numbers
above are the ones this build delivers, and `tools/partial_bench.py --json` reproduces them.

**Latency is unaffected by which kind of pass a notification starts** (`tools/watch_latency.py`,
same machine, one fixture per phase, each write placed at a uniform random offset inside the
interval — see `evidence/latency_293.json`):

| interval | trigger | latency median | p95 | max |
|---|---|---|---|---|
| 5 s | periodic only | 3419.7 ms | 4574.5 | 4574.5 |
| 5 s | notify, **partial** pass | 1721.1 ms | 2036.8 | 2036.8 |
| 5 s | notify, full pass | 1624.0 ms | 2068.2 | 2068.2 |
| 30 s | periodic only | 19456.3 ms | 29441.3 | 29441.3 |
| 30 s | notify, **partial** pass | 1573.4 ms | 1810.0 | 1810.0 |

The trigger's counters in the same run say the partial path was the one taken: at 5 s,
`partialCycles = triggerCycles = 9`; with `partialPass=false`, `partialCycles = 0` and
`triggerCycles = 8`. The latency is the debounce (1.5 s of quiet per path), and it is the same for
both kinds of pass: on a 21-file fixture neither the walk nor the bookkeeping is visible in it.

## This build specifically (0.6.0)

46. **The partial pass does not replace the periodic full pass, and cannot.** A notification-driven
    pass observes part of the project; the periodic pass is what bounds the promise, and the interval
    is measured from the end of the last pass of *any* kind. A notification can only make the next
    full pass come sooner, never later (FR-WCH-13).
47. **A lost notification still costs a version's *lateness* and never the version.** The change is
    stored by the next full pass, which no notification can cancel — the round-290 proof
    (`lost_notifications_do_not_lose_versions`, `a_queue_overflow_is_reported_and_costs_no_version`)
    still holds, and round 293 added the case where a partial pass runs in the same window
    (`lost_notification_and_partial_pass_do_not_postpone_the_periodic_pass`).
48. **A rename can be missed by a partial pass when only one of its two paths was notified** (the
    source's disappearance is then a `delete`, and the destination is stored by the next full pass as
    a new file). The current state after the full pass is correct; the history carries a delete and a
    create instead of a move. Both halves are asserted in
    `partial_pass_matches_a_rename_between_two_directories`.
49. **The load of a partial pass is now proportional to the scope, and the reading is logarithmic
    in the project** (round 294; this item replaced the round-293 measurement of the same quantity).
    A one-file pass reads a few kilobytes: measured on this machine, 43 KB of a 57 KB base on a
    300-file project and 58 KB of a 583 KB base on a 3000-file project — ten times the state,
    1.35 times the reading. The journal is not read in full at all (`journalFullBytesRead` is 0 and
    the pass completes while a journal file is unreadable), and the base is not rewritten
    (`cacheBaseRewrites` is 0, and the file's sha256 is unchanged). What is left of the fixed cost is
    the process itself (~3 ms on this machine, printed as the control in the measurement) and the
    durability writes of a version-storing pass: the blob, the journal, the delta record and
    `project.json`, each a real fsync. The wall clock through the CLI therefore sits close to 10 ms
    rather than comfortably under it; the pass measures itself at 4–9 ms.
50. **A partial pass is conservative about directories it could not open.** If a notified directory
    exists but cannot be listed (EACCES/EIO), nothing under it is written as deleted; the next full
    pass decides. The rule is `scan::may_write_delete` and it is unit-tested; the end-to-end case
    cannot be produced by this test suite because it runs as root, and that is stated rather than
    pretended.
51. **On this machine's shared filesystem the daemon's own read-only pass is sometimes reported to
    inotify as a change** (`/work` is a `fakeowner` host share; the container's overlay is not —
    measured by `tools/inotify_probe.py`, and seen once as 31 spurious paths in the daemon's log and
    once as none). The consequence is bounded and measured: the extra notifications widen the scope of
    one notification-driven pass (a directory walk instead of a single file), store nothing, and do
    not loop (18 s of observation: `triggerCycles` stayed at 1). The versions are unaffected; the cost
    is one cheap pass. Exactly when the noise appears was not determined — an honest unknown.
52. **`pl partial-pass` is not a scheduler.** It runs one scoped pass and exits; it is the measurement
    surface for the feature (and the way to exercise a scope by hand), not a timer. `watchTriggers`
    and the periodic pass are unchanged.
53. **The delta is read in full by every pass, and it is capped for that reason.** It is the newest
    half of the state, so a pass that consults it reads the whole file (bounded by
    `cacheDeltaMaxBytes`, 128 KB by default; past the cap the pass folds it into a new base and says
    so: `cacheCompactions`). In the steady state the delta is empty, because the periodic full pass
    empties it every interval; a long run of notification-only passes is what fills it. Sharding the
    delta by path would make a one-file pass read one shard instead of the file — designed, not
    built, and not needed for the numbers in item 49.
54. **A pass that finds no cache reads the journal in full.** That is the documented fallback (it is
    the only case in which a partial pass reads the journal at all): the state is rebuilt from the
    events, the base is written, and the log says so. The property in item 49 is about an archive
    whose cache exists — which is every archive after its first pass.
55. **The bookkeeping of a pass is measured from inside it as well** (`PROJECTLIFE_TIMING=1` prints
    the stage timings to stderr, and the pass report carries `cacheBytesRead`, `journalFullBytesRead`,
    `journalTailBytesRead`, `cacheBaseRewrites`, `cacheDeltaRecords`, `cacheCompactions`). A number a
    program prints about itself is a claim, which is why every one of them is also measured from
    outside: by tools/cache_vs_journal.py (a second reader), by the sha256 of the base file, and by
    running the pass while a journal file is unreadable.

## Round 294 — the same measurement, after the bookkeeping was rebuilt (2026-10-06)

The round-293 table above is kept as it was measured, and it is superseded by this one: `state.json`
is now `base.jsonl` + `delta.jsonl`, the journal is read by its tail, and the skip dedup is a file.
`tools/partial_bench.py`, 10 000 files, 9 runs per measurement, the same disk, the shipped binary:

| pass | median | p95 | max | min | vs the full pass |
|---|---|---|---|---|---|
| full (`scan-once`, unchanged) | 341.0 ms | — | — | 329.3 | 1.00 x |
| partial, 1 file notified | **14.7 ms** | 18.7 | 18.7 | 11.4 | **0.043 x** |
| partial, 10 files notified | 20.0 ms | 54.0 | 54.0 | 17.7 | 0.059 x |
| partial, 100 files notified | 49.2 ms | 160.3 | 160.3 | 40.5 | 0.144 x |
| partial, one directory notified | 21.1 ms | 55.5 | 55.5 | 18.6 | 0.062 x |
| partial, empty scope | 12.6 ms | 14.7 | 14.7 | 7.5 | 0.037 x |

And the counters of one of those one-file passes, printed by the binary itself:

```
ms 9   cacheBytesRead 111494   journalFullBytesRead 0   journalTailBytesRead 8192
skipMapBytesRead 0   cacheBaseRewrites 0   cacheDeltaRecords 1   cacheCompactions 0
trackedBefore 10000   changed 1
```

What is left is not bookkeeping: of the ~15 ms wall clock on this machine, 2–5 ms is starting a
process (`pl version` measured in the same session prints the floor) and 4–6 ms is the four durable
writes a pass that stores a version must make — the blob, the journal record, the delta record and
`project.json`. The verify step that prints all of it is step 42 of `tools/verify.sh`.

## This build specifically (0.3.0)

20. **`count_files_below` never runs in the ordinary cycle.** A skipped directory is *named*, not
    opened and not counted; the file count is produced only for the estimate before `add` and for
    `scan-once --skipped`. That is what the two rows above measure, and the difference between them
    is the price of the old behaviour.
21. **Filesystem notification triggers: implemented on Linux, and only there.** `inotify` is the
    backend; macOS and Windows report themselves *unavailable* rather than pretending (the daemon
    logs it and falls back), so on those platforms the promise is still exactly the interval. Three
    further limits worth knowing:
    * **The trigger starts a full pass, not a pass over the changed paths only.** FR-WCH-4 says the
      notification is a trigger "for the changed paths"; this build names those paths in the log and
      in `watch_state.json`, but the pass they start is the ordinary full pass. That is deliberate:
      a targeted partial scan is a different promise (it must prove it cannot miss a file that moved
      or was renamed), and a wrong trigger can never lose a change here. The consequence is a full
      walk per trigger, which is the same work the periodic pass does.
    * **The binding bound is the interval, not the 2 x interval cap.** A notification can only make a
      pass happen *sooner* — it never postpones the periodic one — so FR-WCH-6's "never longer than
      2 x interval" holds because the interval itself holds. Measured in `tools/verify.sh`: with
      triggers on, a change is stored in ~1.5 s; with them off, at an interval of 5 s the sample
      spread 1.7-4.6 s and at 30 s it reached 29.4 s.
    * **The watch set covers tracked directories only.** An excluded directory (`node_modules` and
      the rest) is not watched at all: 200 dependency files add zero watches in the acceptance test.
      If the kernel's watch limit (`fs.inotify.max_user_watches`) is reached, one `ENOSPC` is enough
      to switch the trigger off for the whole run and say so in the log — the periodic pass carries
      the promise from then on.
22. **Compression is not implemented** (`compression` stays `none`).
23. **Mirroring and the local spool are not implemented** (decided for 1.1).
24. **No graphical interface.** CLI and OS notifications only; if the notification tool is missing,
    messages go to `logs/projectlife.log` and to a banner in `status`/`doctor`.
25. **Windows: compiles, never executed.** `cargo check --target x86_64-pc-windows-gnu` passes (it
    is step 24 of `tools/verify.sh`), and the places that used to be unix-only now have Windows
    answers: free space via `GetDiskFreeSpaceExW`, local time via
    `SystemTimeToTzSpecificLocalTime`, console detection via `GetConsoleMode`, Ctrl-C via
    `SetConsoleCtrlHandler`. **Nothing was run on Windows** — there is no Windows machine here. What
    remains unverified there: long paths, reserved names, case-insensitive filesystems, and
    `file_id` (device/inode identity), which is `None` on Windows, so a rename is matched by content
    instead of by identity. macOS is unchanged and likewise unexecuted.
26. **Import is refused into a project that already has a journal.** Imported events would carry
    older timestamps with newer sequence numbers, and the state is reconstructed in sequence order,
    so an old `put` could resurrect a file deleted after the export range. `import --new NAME`
    (a project of its own) is the supported shape; a chronological merge is decided for 1.1.
27. **A rename whose file could not be read stably.** If a file is renamed *and* unreadable in the
    same cycle, the move is recorded (so the old path is not reported as deleted) but **no new
    version is written**; the last known version is carried to the new path and the next cycle
    records the real contents. This branch is not covered by a test: making a read fail
    deterministically for a regular file as root is not possible in this container, and the branch is
    reported here rather than tested around.
28. **Clock jumps backwards** are recorded as a `meta(clock_skew)` event; ordering is by `seq`, but
    `--at` times can then be misleading and the program warns.
29. **Network filesystems** are not a supported scenario: they may work, nothing is promised.
30. **A project folder that disappears** becomes `path_missing`; the program deliberately does not
    record "all files deleted" as ordinary events. Use `restore --to` or `relink` after renaming.

31. **`watchTriggers=false` and `daemon run --no-watch` are the same thing**: the periodic pass is the
    only trigger. `daemon status` prints which mode is in force, and `watch_state.json` records it, so
    "notifications are on" is never an assumption.
32. **Configuration is re-read while a daemon runs (round 300, FR-CFG-3 is implemented).** Once per
    cycle the daemon hashes `<archive>/config.json`; when the bytes differ it reloads, logs the keys
    that changed, and re-applies the ones the loop holds in local variables — `intervalSeconds` and its
    family, `debounceMs`, `triggerResyncCycles`, and `watchTriggers` (which turns the notification
    backend on or off live, keeping its counters). Everything the cycle reads by itself —
    `notifications`, `partialPass`, the mass thresholds, the filters, `deepVerifyIntervalMinutes`, the
    space thresholds, `lowPriority`, `maxConcurrentReads` — is in force from the next cycle with no
    extra work. `SIGHUP` forces the re-read at once. `daemon status` and `<archive>/config_state.json`
    say how many times the running process re-read, when, and which keys changed.
    What is *not* live: the app's own server, which reads the configuration when it starts — a change
    made while the window is open reaches the observation immediately and the window's own copy of the
    settings at its next refresh.
33. **Auto-detection uses metadata only (names, sizes, extensions).** It cannot determine file type by
    content: a `.txt` file holding binary data is treated as text, and a mislabelled file is judged by
    its label. This is a promise, not an oversight — reading content to classify a folder would touch
    every byte of it (LIMITATIONS 3 is about the same idea for files).
34. **Presets are suggestions.** Confidence is a number produced by the formula in SPEC §6.17, not a
    probability: two folders with the same shapes get the same number. Anything a preset picks can be
    overridden (`--preset <name>`, `--preset custom`, `--edit-add`/`--edit-remove`, or editing
    `project.json` and running `pl apply-filters`).
35. **`settings.include` means "allow this list" only when `filterMode` is `"allow"`** — what a preset
    writes. Hand-written settings keep the older meaning (add paths on top of the profile), so a
    project added before this round behaves exactly as it did. Two consequences of the same round:
    an include list no longer lifts the size limit (`includeLargeFiles` does), and the secret and
    temporary rules now decide before the include list, so their reasons are what `pl why` prints.
36. **Detection has caps: 100 000 entries or 5 seconds.** When a cap is hit the report says the scan
    was partial and the numbers describe only the part that was seen. Dependency and hidden
    directories are named and not descended into — so a folder whose only evidence lives inside
    `node_modules` is judged without it.
37. **The interactive `edit` branch of `pl add --preset auto` is not covered by an automated test.**
    It needs a terminal, and the test environment has none. The path it takes is the same function
    (`apply_edit`) that `--edit-add` / `--edit-remove` call, and those are tested
    (`at46_user_edits_preset`, verify.sh step "Universal mode: the preset is written down").
38. **A retention policy thins between anchors.** Every retained moment is materialised and every
    moment at or after the keep-everything window is exact, but the state strictly between two older
    anchors is the newer anchor carried forward: a restore at such a moment returns the last retained
    moment, not the bytes that were there. The plan says so when the policy is applied, and the
    restore path says so again every time such a moment is asked for. There is no way to have both
    "one version per day" and "any moment in between": the versions in between are gone.
39. **A stored policy is never applied by anything but a human.** `pl` prints it, `pl` applies it when
    asked, and the daemon does not read it at all. There is therefore no "automatic retention" and no
    timer that thins an archive by itself (FR-LIF-6). If a user expects the stored policy to keep the
    archive small on its own, it will not — the estimate is printed so the decision stays visible.
40. **`pl gc` reports and never deletes.** Removing unreferenced blobs is `pl check --fix`, which
    confirms first. `gc --dry-run` exists so the number can be seen without touching anything; a
    deletion path was deliberately not added for a command whose name suggests it is safe.
41. **The MCP server is stdio only, and its rate limit is per process.** There is no HTTP transport,
    no authentication and no multi-client arbitration: whoever can start the process can read the
    archive through it. What it *cannot* do is write — that is enforced and verified (SPEC §6.20,
    FR-MCP-8) — and its request log is the only file it touches, so "who read my archive" is
    answerable. A second client in the same process shares the 10-per-second limiter of the first.
42. **`pl undo` never executes anything.** It prints the command and the moment the previous state is
    restorable from. Undoing a restore that wrote into the project folder is a restore of that older
    moment (`--into-project --clean`), and undoing a prune is impossible: the versions were deleted.
    A command that performed this on its own would be a command that can destroy the thing it is
    meant to protect.
43. **`pl watch` and `pl open` do not need a terminal, but `pl open --launch` does need a desktop.**
    `watch` polls the journal (`--interval`, default 500 ms) rather than subscribing to anything;
    `open` prints the path and only hands it to `xdg-open`/`open` when `--launch` is given, so a
    headless machine gets the path and no error.
44. **FR-CFG-3 was implemented in round 300** (see item 32), after being open since the
    specification was written. The remaining edge: a *cycle already running* finishes with the
    configuration it began with; the change is in force from the next one. On a machine where one pass
    takes a minute, "applied without a restart" therefore means "applied within a pass plus a cycle",
    not "applied inside the pass that is running".
45. **Not implemented from the round-292 brief, and named as such:** `pl mount` (a read-only FUSE
    view), a TUI browser (`pl browse`), `pl restore --interactive`, `pl restore --diff`,
    `pl export --human`/`--format zip|tar|folder` as a first-class option, project groups, tags and
    bookmarks beyond `mark`/`note`, quiet hours and notification `--sound`, git-hook anchors,
    `pl protect <project> --until`, `pl install --timer` as a one-shot installer, and a local web
    view. Each of these is a product decision of its own; building half of one would be worse than
    leaving it out. The command surface that *was* added is in SPEC §6.21.

74. **The menu is only as complete as the registry.** An entry exists because someone added it to
    `app/src/menu.rs`; the app does not discover new core commands at runtime. A command added to the
    core without an entry stays in the terminal — and `tools/ui_coverage.py` reports it as
    unaccounted-for unless it is named there with a reason, so the omission is visible rather than
    silent.
75. **The menu asks for at most one value per entry.** Multi-path restores, two-ended moment ranges
    (`log --from … --until …`) and the `--preset custom` extension editor still belong to the window's
    own screens or to the terminal.
76. **The palette's filter is a substring match** over the label, the id, the command line, the note
    and the group — not fuzzy. A word that appears in none of those finds nothing, and the palette
    says so instead of showing a near miss.
77. **The macOS menu bar is compiled and never run.** Rounds 295–298 already say this of the shell as
    a whole; this round adds the menu bar and the shield's submenus to that list. What is checked here
    is that the shell asks the server for the menu (`menu?shell=1`), builds its bar from that answer
    rather than a hand-written list, and says so when an entry cannot be performed
    (`reportMenuFailure`) — compiled for arm64 against the real AppKit/WebKit headers, read by checks
    shown to fail when the property is removed, and not executed.
78. **`info` entries run nothing, on purpose.** `pl completion … --install` and `pl prompt` are shown
    with their reason rather than run: installing completion changes the person's shell, not this app.
    A reader who wants them must type them — the menu is there to make the command visible, not to
    pretend it performed it.
79. **The result card shows the core's output verbatim, however long it is.** `history.content` over a
    large archive prints a lot; the card scrolls rather than truncating, because a truncated answer is
    a different answer. The command line the card shows is the one that ran, so it can be repeated in
    a terminal to the same effect.
80. **The window's menu bar is a drawing of the macOS one.** On the Mac the real menu bar is the
    system's, built by the shell from the same answer; in a browser (and in the acceptance run) the
    window draws its own row so the application is usable and readable there too. The two are built
    from one answer, so they cannot disagree about what exists — but only the drawn one has been
    clicked by a machine.
81. **Entries that need a project are disabled in both fronts, and the reason is the server's.**
    The shell does not know which project the window has open (it passes none), so an entry chosen
    from the macOS menu without a project in the window is performed by the window, which says
    "no project is open" in the page's own words rather than the menu's greyed-out tooltip.
82. **A boundary message now carries milliseconds when the second alone would repeat itself.** The
    comparison was always millisecond-precise; the sentence was not, and printing the same second
    twice made the refusal look like a bug in the reader's head rather than in the moment. The
    boundary itself is unchanged: a moment 1 ms before the start of the history is still refused.

## The desktop app (round 295)

46. **The app is a shell around this program, and it says so.** `Project Life.app` contains three
    executables: the macOS shell, the interface server (`projectlife-ui`) and this program. Every
    mutation the window performs is a command line of this program (`add --dry-run`, `add`, `restore`,
    `export`, `import`, `pause`/`resume`, `config set`), and every number it shows comes from this
    program's `--json` output or from files this program writes. If the three are separated — the
    helper binaries moved or deleted — the app reports that it cannot start rather than showing a
    window over nothing.
47. **On macOS this build has no filesystem-notification backend** (item 3): the window prints
    `unavailable` and the promise's accuracy equals the observation interval — 5 s by default, set in
    *Settings*. Saving a new interval reaches a running observation without a restart (round 300,
    FR-CFG-3), so the number in *Settings* is the number the daemon will use from its next cycle; the
    app no longer restarts observation to make a saved interval take effect.
48. **Nothing macOS-only has been executed by the author of this round.** There is no Mac in the
    environment that built the app. What was verified is: the bundle's bytes (arm64 Mach-O, ad-hoc
    signature over every page, only system libraries), and the whole seven-step scenario through the
    *same* interface server and the *same* interface bytes in a real browser on Linux. Not executed:
    launching the app, the AppKit shell (window, menu-bar item and menu, native folder panel, quit
    confirmation, close-window alert), WebKit rendering, and this program under macOS.
49. **No Apple certificate, no notarisation.** Every executable carries the linker's ad-hoc signature
    (the shape Xcode's linker produces). A bundle-level signature is one command on the Mac
    (`codesign --force --deep --sign - "Project Life.app"`). On first launch macOS may require
    right-click → *Open*; **do not disable Gatekeeper or SIP for this**, and the documentation says so.
50. **The window's server is a loopback HTTP listener with a per-launch token.** The three static
    interface files are served without the token (they are the bytes compiled into the binary and can
    do nothing by themselves); every `/api` route requires it. There is no other network code in the
    app, and this program's own rule (NFR-SEC-1, no connections) is untouched.
51. **A restore is verified by the app, not by the restore code**: after restoring, the app hashes
    every restored file and compares it with the hash the archive recorded, and reports per file.
    Symlinks are not content-compared (they are counted and named as such in the report), and an
    export is re-checked the same way from its own `MANIFEST.sha256`.
52. **`pl add --dry-run` is an estimate, not a promise about the future.** It computes exactly what
    the real run would compute for the folder as it is at that moment, and writes nothing at all (no
    project, no journal, no blob, no config, no lock). If the folder changes between the estimate and
    the real add, the real add stores what is there *then*.
53. **Two screens of the Figma design are deliberately not built**: the notification centre and the
    settings screens for filters/advanced. Building them would have meant showing data the core does
    not produce. The tray menu of the design *is* built — as the real menu-bar item, with the state
    the core reports.
54. **The app never kills a process it did not start.** If observation runs from a terminal or from
    launchd, *Stop* refuses and names the holder (this program's `daemon stop` on macOS is
    `launchctl unload`, which is not what a foreground daemon needs); the window shows "started
    outside this app" instead of taking credit for it.

## Free space, and what a warning may cost a person (round 296)

55. **The threshold is the smaller of the two numbers, and that is a change of behaviour.** FR-DSK-3
    says "1 % or 500 MB, whichever is smaller". Until round 296 the program asked `free < 500 MB ||
    free % < 1`, i.e. "whichever trips first", and on the owner's 926 GB volume with 1.8 GB free it
    declared the archive full and stopped recording. Now the threshold is `min(500 MB, 1 % of the
    volume)`: on a big volume the fixed floor governs, on a small one the percentage does, and the
    sentence in the log states which of the two won and both numbers it chose between.
56. **While the archive is full it speaks once, then at most every ten minutes, then once when it
    resumes.** The state is kept in `<archive>/logs/space_state.json`, so cycles *and restarts* share
    the memory of having already said it. What this costs: a person who looks only at notifications
    and not at the window may see one message and then silence for up to ten minutes while nothing is
    recorded — the window, the menu bar and `pl healthcheck` say the state the whole time.
57. **`ARCHIVE_FULL` stops writing and nothing else.** Checking continues, the archive is never
    deleted from, and the change made during the outage is recorded by the first cycle after space
    returns (tested at the command level and in the window).
58. **The window's protection state comes from this program, not from process liveness.** It is one
    function (`protection` in the app's server, fed by `heartbeat-check --json`), and a live daemon
    that cannot write is reported as *Recording stopped: not enough free space* — never as
    "Protected". Four states are distinguished: `protected`, `protected_low_space` (writing continues
    below the warning threshold), `paused_full`, `stale`, plus `stopped` and `unknown`.
59. **The app's own server, its window and its menu bar have still never run on macOS.** The
    round-296 faults in the shell — a lookup that named the resource folder twice, and a
    `window.prompt` with no WKUIDelegate behind it — were found by the owner running the app on his
    Mac, not by anything in this repository. The fixes are compiled for arm64 and read by a source
    check that is itself checked for being able to fail; they are not executed anywhere.
60. **Space thresholds now move with the configuration.** `config set stopFreeBytes …` reaches a
    running daemon at its next cycle (round 300, FR-CFG-3), so the transition test can change them and
    watch the same process resume writing. Before this round that needed a restart, and the test said
    so.

61. **The window shows what the core can answer as JSON, and nothing more.** Commands that only
    print prose were given `--json` in round 297 rather than being scraped: a screen that parses
    sentences breaks the first time a sentence is reworded. `docs/UI_COVERAGE.md` is generated from
    the core's own command list, so it cannot quietly fall behind the program.
62. **Nine commands stay in the terminal on purpose, each with a reason** (the list is in the window
    itself, under Diagnostics): `undo` (it prints what would undo the last write rather than doing
    it), `panic`, `export-and-prune` (two irreversible effects in one command), `archive-move`,
    `archive-delete`, `daemon install|start|stop`, `mcp`, `completion`, `prompt`, `notify test`,
    `watch`, `gc`'s deletion half (`check --fix`), `audit-archive --update` (recording a digest is a
    deliberate act), and `prune --before` (a sharper cut than a policy). This is a choice, not an
    omission: a button that performs an irreversible act on one click is worse than no button.
63. **The window redraws its main area on every poll (3 s) and every 700 ms while a job runs.** A
    click that lands in the same instant as a redraw can be lost — the button is replaced under the
    pointer. It is rare and harmless (click again), and the acceptance test hits it often enough to
    have needed a retry loop; it is recorded here rather than hidden. The fix, when it is worth
    doing, is a targeted update of the changed region instead of a full redraw.
64. **`cat` in the window sends at most 256 KiB and marks binary bytes as binary.** A version larger
    than that is shown truncated *and said to be truncated*; its hash and size in the card describe
    the whole version, not the part shown. A file whose extension says text but whose bytes are not
    UTF-8 is reported as `binary` — the filter decides by name, the window must not pretend.
65. **The versions and the diff are read from the journal on every request.** Opening a file's
    versions re-reads the project's journal; there is no index by path. On the measured 10 000-file
    fixture that is milliseconds, but a project with a very long history will show the cost on the
    screen rather than hiding it in a background task. Nothing is cached between requests, so nothing
    can go stale.
66. **Applying a retention policy from the window deletes versions, and the dialog says so with the
    numbers** (`kept / before`, `dropped`). Storing a policy deletes nothing — the two are separate
    routes and separate buttons, because one is a note and the other is not.
67. **The app's window, its menu bar and its native panels have still never run on macOS.** Round 297
    adds no new macOS-only code (the page runs in WebKit unchanged), but the claim "it works on a
    Mac" remains untested here for the same reason as before: there is no Mac. The four faults found
    in rounds 295–296 were all found by the owner running it.
68. **A mutation campaign may not run inside the project tree.** `tools/mutations.py` now refuses
    `--work` inside the sources, because a campaign copy contains deliberately broken code: on
    2026-10-06 one such copy at `tmp/verify296/mut/app/macos/ProjectLife.m:631` was read as the
    current source by the owner's other agent, and reported to me as a defect in the delivered app.
    The stale trees are gone; the rule is enforced by the tool.
69. **`heartbeat-check`'s exit code speaks about the heartbeat, not about writing.** 0 means a cycle
    finished recently, 1 that none has, 2 that none has while a daemon claims to be running. A disk
    that fills just after a successful cycle leaves the code at 0 for up to a minute and the reason
    in the JSON's `storage` block; `healthcheck` and `doctor` report the stopped write as an error
    immediately. The code was left as documented rather than redefined, because a scheduler's exit
    codes are a contract and changing what they mean is worse than a minute of latency.
70. **A refused pass writes no heartbeat, by design.** The heartbeat means "a pass completed", and a
    pass that cannot store a version must not advance the state — that is how a change made during
    the stop is still recorded afterwards (measured in `tests/round298.rs` and in
    `tools/no_room_words_check.py`). The consequence is that on a full archive the heartbeat ages
    while the daemon is alive and cycling; the diagnostics now say so in those words instead of
    implying a dead daemon.
71. **`doctor`'s remedy for a full archive names a command that deletes** (`pl prune …` without
    `--dry-run`), because nothing else frees space. It asks for confirmation unless `--yes` is
    passed, and the dry run remains the suggestion while there is only a warning. A remedy that
    cannot work is worse than none; a remedy that deletes should be read before it is run.
72. **The build line describes what is installed and running, not what was intended.** Two copies of
    the app in two folders look alike in the Finder until one is opened: what the window can do is
    name the bytes it is, and hash them again on demand. Nothing here can tell a person which of two
    bundles they *meant* to open.
73. **The macOS shell remains compiled and unrun.** Round 298 adds to it two properties, one menu
    item and one sentence in the About panel; all of it is cross-compiled for arm64 and read by
    checks that have been shown to fail when the property is removed, and none of it has been
    executed, because there is still no Mac here. Every fault found in the shell so far (rounds
    295–297) was found by the owner running it.

## Round 300 — a repair that cannot overwrite, a ledger that survives, a configuration that moves (0.9.3)

86. **`restore --missing` cannot invent what the archive never stored.** It creates the files that are
    absent now from a chosen moment; if the archive has no version of a path at that moment (a filter
    excluded it, the moment is before the project was added, a blob is quarantined), the file stays
    absent and the report names it. It is a repair of what was observed, not a promise about what was
    not — item 3 and the promise's window still bound it.
87. **`restore --missing` does not recreate empty directories** (item on empty directories stands):
    files inside them come back, the directory itself only exists because a file is in it.
88. **The notification ledger grows by one line per message and is never rotated.** Messages are rare
    by construction (a mass change, a stop for space and its resume, an unreachable archive, a cycle
    error, a lifecycle line), and the reader is bounded — `pl notifications` reads at most the last
    512 KiB of the file, so a decade of messages cannot be pulled into memory to show twenty. There is
    no retention policy for the ledger itself.
89. **The ledger records what the program said, not what the person read.** "Unread" does not exist in
    the core: the window shows the newest lines and the person scrolls. A read marker belongs to a
    window, and a window can be replaced; a claim of unread messages in the archive would be a claim
    the core cannot support.
90. **A repair is verified by the window, not by the core.** The core says how many files it wrote;
    the app hashes the folder before and after and reports any pre-existing file whose bytes changed or
    vanished as a violation. A repair run from the terminal gets no such check — the terminal has
    `pl drill` and `pl check` for the same purpose, run deliberately.
91. **`--missing` and `--clean` are mutually exclusive on purpose**, and the refusal names both. There
    is no mode that repairs and prunes in one gesture: a command that can delete while someone is
    repairing damage is not a command this program offers.
92. **macOS has no notification backend in this build.** The core watches directories with inotify on
    Linux and with `FindFirstChangeNotificationW` on Windows; on macOS it reports `unavailable` and
    the periodic pass is the only trigger, exactly as in 0.9.3. The promise (nothing is lost) holds;
    the latency is the interval instead of a debounce.
93. **On Windows a notification names a project root, not a file.** A change notification says
    "something under this root changed". The label it produces is `<project>:` with no path, so a
    notification-driven pass on Windows is a pass over that project, not over one directory — and
    `describe()` says "project root(s) watched recursively" rather than pretending to the
    per-directory detail inotify gives on Linux. One handle per root also means at most 64 projects
    are watched (the `WaitForMultipleObjects` limit); beyond that the periodic pass carries the rest.
94. **The Windows notification backend has never been executed.** It was compiled for
    `x86_64-pc-windows-gnu` and read; there is no Windows machine in the build environment. The
    periodic pass is not affected by its absence: if the handles cannot be opened, the daemon logs
    the system's own words and runs on the periodic pass alone, which is the same path it takes on a
    machine whose inotify watch limit is reached.
95. **The Windows and macOS applications have never been run here.** Their bytes were checked
    (executable shape, imports, resources, embedded interface, checksums, packaged layout) and the
    same interface bytes were compared with the Linux build's; their *behaviour* — the window, the
    tray icon, the first-close notice, the registry entry, the folder chooser — has been executed by
    nobody. `docs/PLATFORMS.md` names the command that checks each of them on a machine that has the
    operating system, and both packages carry it.
96. **The PowerShell scripts (`install.ps1`, `uninstall.ps1`, `verify_windows.ps1`) were never
    executed, and were not even parsed by a PowerShell interpreter.** No PowerShell exists in the
    build environment. What is checked here is weaker and is stated as such: the files are present,
    their braces balance, they name executables that exist in the package, and the verifier checks the
    selftest result and the delivered checksums (`tools/windows_package_check.py`). A mistake in them
    is possible; the failure mode is visible rather than silent (the verifier prints each step).
97. **A case-insensitive filesystem cannot hold two programs whose names differ only in case.** This
    is why the Windows package ships `ProjectLife.exe`, `pl.exe`, `pl-ui.exe`: the first draft shipped
    `ProjectLife.exe` beside `projectlife.exe`, and on the volume the delivery was built on the second
    silently replaced the first. The build script now refuses such a package, and `verify.sh` checks
    the delivered folder for it. An archive itself is unaffected: its own paths are normalized
    (`scan::normalize_rel`), but two files inside one project folder whose names differ only in case
    remain ambiguous on Windows and are treated as the single file the filesystem reports.
98. **`projectlife daemon stop` waits up to 15 s, and the wait is two answers, not one.** The pid
    being gone *or* the daemon-lifetime lock being released counts as stopped: on unix a child that has
    exited but not been reaped still answers `kill(pid, 0)` with success, so a pid alone would report
    a running daemon that is already gone. `FAILURES_301.md` (F301-2) is the measurement.
99. **Nothing about the three applications is code-signed.** No Apple certificate, no Authenticode.
    macOS gets the linker's ad-hoc signature because arm64 will not start without one; the Windows
    executable carries the sentence in its own version block. Running them therefore involves the
    operating system's own warning the first time, and neither README asks for a protection to be
    disabled.

## Round 302 — who made it, what it is for, and one version

100. **The name and the address are the owner's own, and one of them is unusual.** They are written
     exactly as he wrote them on 2026-10-07: `Oxunjon Ubaydllayev`, `oxunjonub@gmail.com`. The
     surname differs by one letter from the spelling `LICENSE` carried before this round
     (`Ubaydullayev`), and the domain is `gmmail.com`, not `gmail.com`. Both are his words, so both
     are kept; `docs/BRAND.md` records them and the two-step change if either was a typo. A program
     that silently "corrected" the name of its author would be a worse program.
101. **Four files are checked, not generated.** `Cargo.toml`, `LICENSE`, `README.md` and the `docs/`
     files are written by hand, so `tools/brand_check.py` checks them rather than producing them: a
     name changed in `src/brand.rs` and not here fails the check. That is weaker than generating
     them, and it is what is actually enforced.
102. **The Windows shell prints the English sentence, not the Russian one.** It is compiled as wide
     strings, and this build cannot run on Windows to see what the compiler does with a UTF-8 Cyrillic
     literal. The English sentence is ASCII and certain; the Russian one is in the version *resource*
     (which Explorer shows) and in every other front.
103. **The macOS and Windows shells have not been run.** Unchanged from round 301: they are compiled,
     linked and read as bytes; the About panels, the menu-bar items and the folder panels open on a
     machine that has the operating system. The four end-to-end steps added this round read the
     identity off the *rendered page* in Chromium on Linux, which is the same page and the same
     server all three shells show.
104. **The author line in the window comes from the core, so a broken core shows a broken About
     screen.** If `projectlife version --json` cannot be run, the About screen says the author could
     not be read and names the reason; it does not fall back to a name written into the page. The
     end-to-end run has that as an explicit step, because an alternative would have been a second
     copy of the name.
