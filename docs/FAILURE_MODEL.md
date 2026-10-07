# Failure model

What actually happens, per failure. "Nothing is lost" always means: nothing that was already
recorded, and nothing that was never observed.

## `kill -9` in the middle of a cycle

* The write order (blob → journal → state) means the worst case is an unfinished extra blob and, at
  most, a half-written last journal line.
* On the next start the reader **ignores a half-written trailing line** and reads everything before
  it. The project stays consistent; `check` reports `trailingPartialLine: true`.
* An unfinished initial copy leaves the project in `initializing`; `add` (or a daemon cycle) resumes
  it and stores only what is missing. This is tested: `tools/verify.sh` kills the process with
  SIGKILL mid-copy on 1800 files and requires the journal to stay readable and the run to resume.
* Garbage in the **middle** of a journal (not the end) is treated as damage: writing to that project
  stops, its state becomes `error`, and **nothing is deleted**. `recover.py` still reads what is
  readable and can export individual files.

## Power loss / machine switched off

Same as `kill -9`. Everything already `fsync`ed survives; the observation period that was missed
becomes a `gap` event on the next start, with the reason `startup`.

## The archive disk is unplugged

* State `ARCHIVE_OFFLINE`: the program does not exit, does not lose configuration, retries every
  10 seconds, notifies once and repeats hourly.
* Nothing is written to the project folder, and nothing is written to a wrong place in the archive.
* When the disk returns, the next cycle reconciles the disk with the last recorded state and writes
  a `gap` event with the interval that was not observed. States inside that interval do not exist.
* While the archive is away there is **no** local buffer in this version: those intermediate states
  are lost, which is exactly what the `gap` event says. (The spool is planned for 1.1.)

## The archive disk is full

* Below the warning threshold (`warnFreePercent` 10 % or `warnFreeBytes` 5 GB, whichever is smaller):
  notification and a mark in `status`.
* Below the stop threshold (`stopFreePercent` 1 % or `stopFreeBytes` 500 MB): writing stops, the
  state becomes `ARCHIVE_FULL`, a loud notification repeats every 10 minutes. Checking continues so
  that writing resumes by itself once space is freed.
* Nothing is deleted to make room, ever. `prune` only runs when you ask.

## A blob is missing or corrupted

* A missing blob: the versions that reference it are listed, `restore` writes the other files, the
  missing ones are **not** written as empty or partial files, and the exit code is 2.
* A corrupted blob: `check --deep` and any read detect the mismatch (the sha256 does not equal the
  file name). The blob is moved to `quarantine/<sha256>` — never deleted — and a
  `meta(blob_corrupted)` event is recorded. The bytes stay available for you to inspect.
* An export that meets a corrupted blob stops immediately, marks its directory
  `EXPORT_FAILED.txt`, and **no pruning follows**. Tested end to end.

## The project folder disappears

* The state becomes `path_missing`. The program deliberately does **not** write "all files were
  deleted" as ordinary events: that would be a lie about what was observed.
* The history stays intact and restorable (`restore --to <dir>`), which is the whole point.
* A renamed folder is re-attached with `relink <project> <new-path>`; the next cycle reconciles.

## The clock jumps

* A backwards jump is recorded as `meta(clock_skew)`. Ordering is by `seq`, so history cannot be
  reordered; `--at` still used `ts` and may then be misleading — the program warns when the moment
  falls into such a range.

## A file cannot be read or keeps changing

* Unreadable: `skip(reason=unreadable)`; the cycle continues, one bad file never stops the rest.
* Changing while being read: the read is retried up to three times; if still unstable the version is
  taken in the next cycle, and `skip(reason=unstable)` is written only when it fails again.
  The counter is visible in `status`.

## Two writers at once

* `.lock` is created exclusively. A second cycle — daemon or `scan-once` — refuses to start and says
  who holds the lock. A stale lock older than two minutes is reported with instructions rather than
  being stolen silently.

## A wrong `--at`

* Earlier than `historyStartsAt`: refused with the available range.
* Inside a gap: allowed, with a warning that the program was not observing then and that the nearest
  previous known state is shown.
* Unparsable: refused with the accepted formats listed.

## What the program does not defend against

* An agent running as your user deleting the archive. Mitigation: another disk, `export`,
  `audit-archive` (detection, not protection), and the external-timer mode.
* Data that was never observed: two rewrites inside one interval, a file created and deleted inside
  one interval.
* A filesystem that lies (caches, network mounts, files modified without any metadata change —
  although the hourly deep verification covers the common form of that).
