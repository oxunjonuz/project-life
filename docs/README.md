# Project Life

A local flight recorder for the file history of a project.

**Author: Oxunjon Ubaydllayev ⟨oxunjonub@gmail.com⟩** · MIT licence

It is made for the moment an agent deletes or breaks something: it keeps everything — every version of every file you protect stays on your own disk and can be restored at any moment.

AI agents and scripts can rewrite or delete a whole project in seconds. Git does not save
uncommitted work; editor rollbacks cover only their own operations. Project Life watches the projects
you add and stores every **observed** state of their files in a **central archive on a separate
disk**. Source code weighs kilobytes, so the archive is kept until you decide to clear it.

**The promise** (also in `LIMITATIONS.md`, verbatim):

> While a project is added, the program is running and the archive is reachable, any **observed**
> state of the project can be restored to within one observation interval (5 seconds by default).
> States that appeared and disappeared between two observations are not recoverable. The archive is
> kept until you clear or delete it yourself.

This is not git and not a classic backup. It does not know what is "correct"; it honestly keeps what
it managed to read.

## Two more windows onto the same archive

```sh
pl recent                       # the last few things that matter, each with the command that follows
pl heartbeat-check              # 0 fresh, 1 stale, 2 stalled daemon — for a scheduler
pl healthcheck                  # the same, plus space, project states and pending recoveries
pl prune my-app --policy "7d:all,30d:1/day,365d:1/month" --dry-run
pl retention my-app             # the stored policy and what applying it would do
pl suggest                      # one to three actions that follow from the state right now
```

**An agent may read the archive, never write it.** `pl-mcp --archive <archive-root>` is a separate
process speaking MCP over stdio. It registers nine read-only tools, refuses every write operation by
name, hides file contents unless asked and never for something the filters call a secret, and writes
exactly one file: its request log. `pl mcp info` prints the registration snippet and the two lists.

**A retention policy is a sentence you write, not a rule the program follows by itself.** It is
stored in `project.json`, applied only when you run `pl prune --policy`, and every moment it keeps
stays restorable exactly; the plan says what is dropped, how much space comes back, and which moments
remain — before anything is touched. Between two retained moments older than the keep-everything
window the state is the newer one carried forward, and the program tells you so at restore time.

## Quick start

```sh
cargo build --release                    # one static binary, no runtime dependencies
B=target/release/projectlife

$B init-archive /Volumes/backup/projectlife      # create the archive (a separate disk!)
$B add ~/work/my-app --name my-app               # estimate, confirmation, one initial snapshot
$B scan-once my-app                              # or: $B daemon run / an external timer
$B status my-app --risk                          # state, measured observation window, risk items
$B log my-app                                    # what changed
$B restore my-app --at "10 minutes ago" --to ../my-app-recovered   # never writes into the project
```

If an agent just wiped the project:

```sh
$B panic my-app            # offers last-good / 5 min / 10 min / last mark, restores to a new folder
$B drill my-app            # tests the promise on YOUR machine and prints the measured time
```

## The two things it does that a backup tool does not

1. **It says what it did not save.** Every skip has a reason and a rule (`pl why`, `status --skipped`),
   every observation gap is a journal event, and `pl drill` re-tests the promise on your machine
   instead of asserting it.
2. **Two equal ways to run it.** A resident daemon (`pl daemon run`) *or* an external timer
   (`pl scan-once --all` from cron/launchd/systemd-timer). A daemon can be killed and forgotten; a
   scheduler starts a fresh process every time. In timer mode the accuracy of the promise equals the
   timer period, and the program says so.

## Documentation

| File | Contents |
|---|---|
| `SPEC.md` | the complete technical specification (merged from the two source documents) |
| `LIMITATIONS.md` | everything the program does not promise, including the measured numbers |
| `STORAGE_FORMAT.md` | archive layout, event types, the state-at-T algorithm, recovery by hand |
| `RECOVERY.md` | step-by-step recovery scenarios |
| `INSTALL_AND_USE.md` | build, install, autostart, commands, exit codes |
| `ARCHITECTURE.md` | modules, write order, state machine |
| `FAILURE_MODEL.md` | what happens on a crash, a full disk, a corrupted journal |
| `OPERATIONS.md` | move the archive, free space, integrity, no daemon running |
| `SECURITY.md` | threat model: what is protected and what is not |
| `VERIFY.md` | a ten-minute manual verification on a real machine |
| `CHANGELOG.md` | what is in this build and what is deferred |

## Independence of the checks

`tools/recover.py` is a second implementation of the archive format (Python 3 standard library
only). It is written from the format description, shares no code with the program, and is used two
ways: as the documented "recover without the program" path, and as an independent verifier.
`tools/verify.sh` checks the program end to end: it compares a restore made by the program with one
made by `recover.py` byte for byte, injects corruption by hand, kills the process with SIGKILL in
the middle of an initial copy, and requires the journal to stay readable and the run to resume.

## What changed in 0.3.0

Filesystem notifications are implemented as a **trigger** for an out-of-band full pass (inotify;
macOS and Windows report themselves unavailable rather than pretending). The periodic pass stays
mandatory and is never postponed, the debounce is 1.5 s of quiet per path, a kernel queue overflow is
reported and starts a pass, a second daemon start is refused by a daemon-lifetime lock, and an
excluded directory is never watched. Measured on the machine this build was made on: with
notifications a change is stored in ~1.55 s at a 5 s interval **and** ~1.53 s at 30 s; without them,
3.4 s and 19.5 s. Two new `tools/verify.sh` steps and eight new mutations cover it.

## What changed in 0.2.0

Eight gaps reported against 0.1.0 are closed, each with a test that fails if the fix is taken out
(`tools/mutations.py` proves that one fault at a time). The short version: a symlink can never be
read through; a rename always links the old path to the new one; an interrupted `prune` is completed
or rolled back (`pl recover`); importing old history into an existing project is refused instead of
corrupting the current state; a lock left behind by a killed process is taken over (`pl doctor
--fix-lock` available too); a skipped `node_modules` is no longer traversed on every cycle; the
observed window is no longer truncated by value; and Windows `free_space` is implemented (compiled,
not executed there). The owner's seven decisions are recorded in `SPEC.md` §14.2 and the licence is
MIT.

## Status of this build

0.3.0: M0–M3 of `SPEC.md` are implemented and tested (observation, CLI, restore, lifecycle,
export/import, quarantine, digest, drill, doctor, timer mode, daemon, and now the
filesystem-notification trigger on Linux). Not in this build: compression, mirroring, spool, a UI,
the trigger on macOS and Windows, and verification on Windows and macOS (see `CHANGELOG.md` and
`LIMITATIONS.md` for the honest list).
