# Architecture

## Modules

| Module | Responsibility |
|---|---|
| `archive` | archive root layout, `config.json`, `project.json`, lock, heartbeat, free space, log, UUIDs, time helpers |
| `events` | the journal: read (tolerant of a half-written tail, strict about mid-file damage), append with `seq` assignment, `state_at` |
| `store` | blobs: write (tmp → fsync → rename), verified read, dangling listing, quarantine |
| `filters` | profiles, ignore rules, secret/binary/hidden/size decisions, and the reason+rule attached to every decision |
| `glob` | glob matching and gitignore-style rules |
| `scan` | one observation cycle: walk, compare, read stably, hash, `put`/`delete`/`move`/`symlink`, mass detection, cache |
| `cli` | argument parsing, command implementations, exit codes, confirmations |
| `restore` | plan (preview) and execute: verified blobs, atomic writes, partial success, last-good computation |
| `lifecycle` | `prune` with anchors, `export` (+ verification), `import`, `archive-delete` |
| `daemon` | one cycle over the archive (shared by the daemon and `scan-once`), heartbeat, gaps, notifications, adaptive interval, the two triggers and the debounce, the daemon-lifetime lock |
| `watch` | the filesystem-notification trigger: an inotify watch set over the tracked directories, one watch per directory, a queue read that reports lost notifications, and the deliberate fallback when no backend exists |
| `doctor` | OK/WARN/ERROR checks and the archive digest |
| `retention` | the retention policy: parsing, planning, and the materialised anchors it keeps; the only code in the program that applies a policy |
| `health` | heartbeat and the quiet-failure check: the exit codes a scheduler acts on |
| `quick` | the read-only conveniences (`recent`, `since`, `blame`, `suggest`, sizes, `cat`): the module contains no write call at all |
| `ops` | the operation log: what the *program* did (restore, prune, retention, export, drill), separate from what the *filesystem* did |
| `mcp` | the MCP server core: tool registry, read-only tool implementations, rate limit, request log |
| `src/bin/pl_mcp.rs` | the same server as its own process |
| `tools/recover.py` | a second, independent implementation of the format for recovery and verification |
| `tools/journal_state.py` | a third: it replays a journal by hand and prints the state at a moment, so "the state did not change" is two implementations agreeing |
| `tools/mcp_client.py` | an independent MCP client (protocol, not code): what an agent can see, and whether the archive changed |
| `tools/mcp_audit.py` | a static audit that the reading surface contains no write symbol, with its own positive control |

## Write order (the invariant everything rests on)

```
1. write the blob        tmp/ → fsync → rename → directory fsync      (FR-STO-3)
2. append journal events one write per month file + fsync               (FR-LOG-6)
3. update the state cache (project.json + cache/, round 294)             (recomputable anyway)
```

Consequences: a `put` event can never reference a blob that is not on disk; the cache is never the
only copy of anything; a crash between any two steps loses at most the unfinished version.

## One observation cycle

```
if the archive root is unreachable        -> ARCHIVE_OFFLINE, notify once, retry in 10 s
if free space is below the stop threshold -> ARCHIVE_FULL, stop writing, notify every 10 min
take the archive lock .lock (one cycle at a time, daemon or timer)
for each active project:
    if the project folder is missing      -> state path_missing, no delete events
    if the state is error                 -> skip, keep the history untouched
    if the last observation was long ago  -> write a gap event (reason = startup/observed)
    walk with filters                     -> tracked paths + skips (reason and rule each)
    compare (mtime, size) with the cache
    for changed or new files: read stably, hash, write blob, put event
    moves: by file identity (dev+ino) first, then by matching hash+size
    deletions: delete events for the rest (never for paths hidden by filters)
    skip events: only when a (path, reason) pair is new or its reason changed
    mass event: thresholds, kind, sample paths, lastGoodSeq
    persist the cache, record the measured interval (median, p95)
write the heartbeat
release the lock
```

## Two triggers, one pass (and, since round 293, one of them walks less)

```
periodic deadline (last pass end + interval)   ──┐
                                                ├──> a FULL pass (run_cycle)
notifications: 1.5 s of quiet per path,         ──┘    reason "observed"
               never later than 2 x interval

notifications, when `partialPass` is on (default, round 293)
                                                ──> a PARTIAL pass (the same cycle, scoped)
                                                     reason "changed"
```

* A notification only ever makes a pass happen **sooner**. The periodic deadline is measured from the
  end of the last pass of any kind and is never postponed by a trigger, so the interval remains a
  real bound and "notifications replace the periodic pass" cannot happen by construction. A partial
  pass counts as a pass of *some* kind here: it does observe part of the tree, so it moves the
  deadline of the next full pass, and the next full pass still comes within one interval of it.
* Since round 293 the pass a notification starts walks **only the paths the notification named**, and
  may write only what those paths cover (see "The partial pass" below). A missing, duplicated or
  wrong notification still cannot change what is *stored* — the periodic full pass is what bounds the
  promise — but it can now change how much work a notification costs.
* The watch set is the set of directories the filters let the walk descend into, so `node_modules` is
  named and never watched. New directories join the set the moment they appear; the whole set is
  re-synced every `triggerResyncCycles` passes, and a kernel queue overflow (`IN_Q_OVERFLOW`, which
  means notifications *were* lost) is reported and starts a **full** pass at once — "only what was
  mentioned" is exactly the wrong scope when notifications were lost.
* `.lock` serialises passes; `.daemon` is held for the lifetime of `daemon run`, so a second daemon
  exits with a clear message instead of quietly racing the first.

## The partial pass (round 293)

A partial pass is the ordinary cycle with a **scope**: the same loop, the same order (blob → journal →
state), the same cache, the same mass thresholds, the same batch ids. Only two things change, and
they are the two places in `src/scan.rs` that mention `scope`:

1. **What is walked.** `walk_scoped` starts the walk at every directory a notification named, plus the
   files it named — and nowhere else. A notification about a file does *not* make its directory a
   scope: listing a directory to look for a vanished entry is a different thing from deciding to read
   and store every file in it.
2. **What may be written as gone.** A tracked path may be recorded as `delete` (or matched as the
   source of a `move`) only if it is **covered** and **looked at**:
   * *covered* — `Scope::covers`: the path is the named path, or it lies under a named directory. This
     is what keeps a partial pass from touching anything the notification did not mention; every other
     tracked path is carried over from the state cache untouched, so a change nobody mentioned is not
     mistaken for a deletion and the next full pass still finds it.
   * *looked at* — `scan::may_write_delete`: the parent directory was listed in this pass, or the path
     (or an ancestor) is missing from disk. "I did not look there" must never be written as "it is
     gone": a directory the pass could not open (EACCES/EIO) protects everything under it, and the
     next full pass decides.

Everything else follows from those two rules. A rename inside a directory, between directories, or a
whole folder rename is one `move` per file in one batch, because both sides arrive as notifications
and the ordinary move detection (identity first, then contents) sees them in `cur` and in `missing`. A
deleted file whose *parent* was named is found by comparing that directory with the state, exactly as
in a full pass. A mass deletion still writes one `mass` event with `lastGoodSeq`.

What a partial pass does **not** do is claim completeness: `pl status` and the daemon's `watch_state.json`
publish what the last partial pass looked at (`scopePaths`, `dirsWalked`, `filesInScope`, `ms`), and the
log line names the paths. `partialPass=false` restores the round-290 behaviour (a notification starts a
full pass) — the switch exists so the two can be measured against each other, which is what
`tools/watch_latency.py` does.

## States

| State | Meaning | Transitions |
|---|---|---|
| `active` | observed normally | `pause`, `remove`, missing path |
| `paused` | observation stopped by the user; the pause period becomes a gap | `resume` |
| `initializing` | the initial copy was interrupted; a repeated `add` resumes it | `active` when finished |
| `path_missing` | the project folder is gone; history preserved | `relink`, `restore --to` |
| `error` | the journal is damaged in the middle; writing stopped, nothing deleted | manual inspection |
| `removed` | taken off observation; the archive is kept | `archive-delete` erases it |

Archive-level: `ARCHIVE_OFFLINE` (root unreachable) and `ARCHIVE_FULL` (below the stop threshold);
both are visible in `doctor`, `status` and the log, and both are recoverable without restarting.

## Pruning, in order

```
1. compute the state at T            (the anchors)
2. build the new journal in tmp/:    snapshot(prune-anchor) + one put per anchor + events after T
3. fsync, then rename the journal directory atomically (the old one moves to tmp/)
4. write project.json: historyStartsAt = T, lastSeq
5. delete blobs that are no longer referenced
```

`prune.journal` records each phase, so an interrupted operation is recognisable afterwards. A
dangling blob is harmless (`check` reports it, `check --fix` removes it).

## Export and import

`export` writes a standalone copy in the same format: an anchor state at `--from`, the events in the
range, every referenced blob, `export.json`, `README_RECOVERY.txt` and `MANIFEST.sha256`. Every blob
is re-read and compared with its name before the export is called successful; a failure writes
`EXPORT_FAILED.txt` into the export directory and stops any prune chain.

`import` verifies every blob against its name, skips duplicates, merges events with their original
timestamps (renumbered `seq`) and marks them `imported`. It never modifies or deletes existing data.

## The state cache, and why it is two files (round 294)

`cache/` holds `(path, hash, size, mtime, mode, dev, ino)` per tracked path to make a cycle cheap.
Nothing in it is a source of truth.

Round 293 measured what one file of that shape cost: a notification-driven pass that changed one file
read the whole journal (three times: the clock check, the gap check and the sequence allocator), parsed
the whole `state.json`, scanned every event again for the skip dedup, and wrote the whole state back.
On a 10 000-file project that was **71 ms of the pass's 91 ms**.

Round 294 splits the state by who needs it:

| Question | Where the answer is |
|---|---|
| "what did I see for this path?" | one record: `base.jsonl` (binary search) or `delta.jsonl` (the last record for that path) |
| "what is tracked under this directory?" | a seek to the prefix in `base.jsonl` and a forward read |
| "how many paths are tracked?" | `base.meta.json` + the transitions `delta.jsonl`'s records declare |
| "what is the next sequence number, and when was this last observed?" | `cache/tail.json`, checked against the last line of the newest journal file |
| "is this skip already in the journal?" | `cache/skips.jsonl`, by point lookup |

A partial pass therefore reads a few kilobytes of state, appends one record, and rewrites nothing;
a full pass (the periodic one, which is never postponed) reads the journal in full — it is the pass
that proves the journal is readable at all — rewrites `base.jsonl`, and empties the delta.

The journal is still the only truth. If the cache is missing or damaged, a pass rebuilds it from the
journal (`pl rebuild-cache`, or automatically, with a log line), which is the slow path and
always correct. `tools/cache_vs_journal.py` compares the two independently.

### Why the fold needs no journal of its own

Writing the new base and then emptying the delta is two steps, and a crash can land between them. The
fold is **idempotent**: the delta only ever says "this path is now this" or "this path is gone", and
re-applying those records to a state that already contains them changes nothing. So the order can be
base-then-reset (the safe order) with no recovery record at all, and the test kills a real process at
both sides of that window to prove it. The reverse order would lose the delta's records with the old
base still in place — that is what mutation `M51` introduces, and what the test catches.

## The retention policy (round 292)

A policy is a sentence about what to keep: `"7d:all,30d:1/day,365d:1/month"` — keep everything
younger than 7 days, one version per day between 7 and 30 days, one per month between 30 and 365
days, nothing older.

```
parse            ->  segments, ascending by age; the last one is the horizon
classify         ->  every version event falls into the first window that is wide enough
                     All     : keep the event exactly as it is
                     PerDay/PerWeek/PerMonth : the newest event of each bucket survives
materialise      ->  every surviving bucket moment becomes an ANCHOR: a snapshot event plus the
                     full state at that moment, and an explicit delete for every path that was in
                     the previous anchor but is not in this one
boundary         ->  the moment where the "keep everything" window starts is an anchor too, which
                     is what makes every moment at or after it exact rather than carried forward
swap             ->  the same crash-safe path as `prune --before` (swap_journal): build in tmp/,
                     fsync, atomic rename, metadata, delete unreferenced blobs, phase file
```

What stays exact: **every anchor moment**, and every moment at or after the boundary. What does not:
the state strictly between two anchors older than the boundary — the program carries the newer
anchor forward there and says so, both when the policy is applied and whenever such a moment is
later restored (`retention::thinned_at` is asked by the restore path for exactly this sentence).

Two rules are structural rather than promised:

* **Nothing runs by itself.** FR-LIF-6 forbids automatic cleanup, so `retention::apply` has exactly
  two callers — `prune --policy` and `retention apply` — and no path in the daemon, the cycle or the
  scanner reaches it. `settings.retentionPolicy` is a note to the human; the daemon never reads it.
  The test asserts that after two observation cycles the version count and the anchor count are
  unchanged, and a mutation that makes the cycle apply the policy is caught by it.
* **A policy that keeps nothing is refused** before anything is written, with the newest version's
  age and the policy's horizon in the message.

## The read-only MCP server (round 292)

```
pl-mcp --archive <root>            JSON-RPC over stdio, newline-delimited
    initialize  -> protocolVersion, capabilities.tools, serverInfo
    tools/list  -> the nine registered tools (Level 1 + Level 3)
    tools/call  -> one tool, rate-limited to 10 calls/second/tool
```

| Level | What | Registered? |
|---|---|---|
| 1 — reading | `pl_status`, `pl_log`, `pl_why`, `pl_tree`, `pl_diff`, `pl_last_good`, `pl_check`, `pl_doctor` | yes |
| 2 — writing | `restore`, `panic`, `prune`, `export-and-prune`, `archive-delete`, `import`, `mark`, `pause`/`resume`/`remove`, `scan-once`, `doctor --fix-lock`, `check --fix` | **no** — absent from `tools/list`, and a call is refused *as a write operation* |
| 3 — proposing | `pl_plan_restore` returns the plan as JSON and performs nothing | yes |

The boundary is enforced four ways, and only the first of them is a claim about intentions:

1. Every tool is implemented on read-only modules: `quick` (no write call in the module at all),
   `events::state_at`, `restore::plan` (plan only, never `execute`), `doctor::doctor`, the verified
   store reader, the filter decisions.
2. `tools/mcp_audit.py` reads `src/mcp.rs` and `src/bin/pl_mcp.rs`, removes comments and string
   literals, and refuses a list of write symbols; it runs a positive control on a copy with a write
   call injected by hand, so an audit that has gone blind is a failure rather than a pass.
3. `tools/mcp_client.py` exercises every tool through a real MCP session and hashes the whole
   archive before and after: exactly one file may differ, `logs/mcp.log`.
4. The acceptance test does the same thing inside the crate, and asserts that a write name is
   refused with the words "write operation" (a test that accepted "unknown tool" would pass while
   the refusal stopped explaining itself — that mutant survived once).

Content is never returned unless asked for: `pl_diff` returns paths by default, and with
`include_content: true` it still refuses any path the project's own filters call a secret, naming
the path and the rule. Parameters are logged as keys and values of the *question*; never the bytes.

## Heartbeat and healthcheck (round 292)

```
pl heartbeat-check [--max-age SEC]     0 fresh, 1 stale, 2 stale while a daemon claims to be running
pl healthcheck [--strict]              0 healthy, 1 something is wrong (--strict: warnings too)
```

`pl` opens no network connection (NFR-SEC-1): the exit code *is* the interface, and the shell decides
who to tell (`pl heartbeat-check || curl …`, where the `curl` is cron's, not the program's).
`healthcheck` = the heartbeat, the per-project observation age, pending recoveries, plus every
`doctor` finding, reduced to one code with a ready-to-run fix next to each problem.

### Whose fault it is, when nothing is recorded (round 298)

Three different states look the same from a heartbeat: nothing is scheduled, something is scheduled
and has died, and something is running and refusing to write because the disk is full. The remedy
differs, and the wrong remedy costs more than no remedy — on 2026-10-06 the owner was told to start a
daemon that was already running and to run `pl scan-once --all` on an archive where that command
cannot write a byte either.

So the heartbeat carries `storage` (the verdict from `space`, the one implementation of the rule),
and the readers use it: `heartbeat_finding()` in `src/health.rs`, `cmd_heartbeat_check()` in
`src/cli.rs` and `doctor()`'s heartbeat checks all ask why nothing is being recorded, and when the
answer is "no room" they name the disk, print the numbers, and offer only remedies that can work.
The daemon itself is unchanged: a pass that cannot store a version does not advance the state cache
(that is what keeps a change from being lost), so it does not write a heartbeat either — a fresh
heartbeat on a full disk would be the same error one layer down. `tools/no_room_words_check.py` holds
the words to the state and proves the branch is a branch, by checking that the ordinary advice comes
back when there is room again.

## The operation log (round 292)

`logs/operations.jsonl`, append-only, one JSON object per line: `restore`, `panic`, `prune`,
`retention`, `export`, `import`, `drill`. It answers "what did the program do to my files?" — a
question the project journal cannot answer, because a restore changes the disk without changing what
was observed. `recent` reads it, `undo` reads it, `suggest` reads it. A half-written trailing line is
ignored, and logging never fails the operation it describes.

## The daily commands, by what they are allowed to do (round 292)

| Kind | Commands |
|---|---|
| reads only | `recent`, `since`, `cat`, `size`, `blame`, `log --grep/--content`, `gc`, `prompt`, `suggest`, `completion`, `mcp info/tools`, `status --compact`, `list --sort`, `heartbeat-check`, `healthcheck` |
| advises, never executes | `undo` (prints the exact command and what it would change), `suggest`, `last-good`, `panic` without `--yes` |
| writes, and is a user action | `snap`/`mark` (one journal event), `prune --policy`, `retention set/clear`, `completion --install` (into the archive), `notify test` |

A read-only command that needed a write to answer would be a design error, not a shortcut: the
verification step hashes the whole archive around eight of them and requires the hash to be the same.

## The desktop window, and how it stays a subset of the program (round 297)

The window is a shell around the same binary the owner uses in a terminal. `app/` holds three things
and nothing else: a page (`app/ui/`), a small HTTP server that serves it (`app/src/`), and the macOS
frame that draws it (`app/macos/ProjectLife.m`). Every number on the screen is the output of a core
command; the app computes nothing about the archive itself.

```
window (WebKit)  ──HTTP, token──▶  projectlife-ui  ──spawn──▶  projectlife <command> --json
     ▲                                   │
     └───── the same bytes, compiled into the binary (app/src/assets.rs) ─────┘
```

**The route table is the contract.** `app/src/api.rs` maps `(method, path)` to a handler, and every
handler does one of three things: runs a core command and hands the answer over unchanged
(`core_read`, `core_write`), reads a file the core documents, or reports a job. There is no second
implementation of observation, history, restore, export or import in the window — the mutations the
window performs *are* the core's command lines.

**A command may only be missing from the window on purpose.** `tools/ui_coverage.py` reads the core's
own command list out of `src/cli.rs`, looks for each command in the page and the route table, and
refuses to pass when a command is neither wired to the window nor named in its list of deliberate
omissions with a reason. The generated result is `docs/UI_COVERAGE.md`; the same list appears in the
window itself (Diagnostics → *Deliberately left to the terminal*), so the person reading the screen
learns why a thing is not there instead of wondering.

**Two rules are enforced mechanically, not by habit.**

| rule | who checks it | how it is proven to bite |
| --- | --- | --- |
| every control has a handler; every route the page calls exists; irreversible routes ask first; selectors remember the chosen project; no browser dialogs | `tools/ui_surface_check.py` | `tools/ui_surface_control.py` injects six faults and requires all six to be caught |
| a command is in the window or documented as CLI-only | `tools/ui_coverage.py` | `verify.sh` / `verify_app.sh` fail on any unaccounted command, dead call or handler-less control |

**Writing is separated from reading by the shape of the request.** A route that changes something
refuses to act unless the page sends `confirm: true` *and* the route checks for it; the page asks the
person with a dialog it draws itself (the macOS shell has no JavaScript dialog handler — that fault
cost a silent import failure in round 296). The window's own dialogs are therefore part of the
contract, checked by the same tools.

**What the window shows of the archive's health.** Integrity: `check` (fast and deep), `drill`,
`audit-archive`, `quarantine`, `gc --dry-run`, `recover`. Storage: `size`, the free-space verdict from
`space.rs`, the stored retention policy with the plan a prune would produce, and applying it behind a
confirmation carrying the numbers. History: the moments, the versions of a single file, the content of
any version, and a comparison between two moments.

**Which build is this (round 298).** The sidebar footer, the menu-bar menu and the About panel print
`app <version> · ui <sha8> · core <sha8>`, hashed from the three things the running window is made of:
the interface server process, the core binary it calls, and the page it serves — the last from the
bytes actually served at `/app.js`, so a page that differs from the source on disk cannot claim
otherwise. The hashes are cached per (path, size, mtime), which means a binary replaced under a
running app is re-hashed rather than remembered. It exists because on 2026-10-06 the owner reported
three failures against a bundle that had been replaced hours earlier and nothing on screen said so.
`tools/build_identity_check.py` verifies the answer with Python's `hashlib` — a different
implementation from the Rust that produced it — and `shasum -a 256` on the Mac reproduces every
number.

## The menu: one list, three fronts (round 299)

Before this round the window could reach the core's functions only where a view happened to exist,
and the macOS menu bar held a hand-written dozen entries. Round 299 gave the app a full menu, and the
whole design is one decision: **the menu is data, in one place, and every entry names the core command
line it stands for.**

```
app/src/menu.rs        the registry: id, group, labels (en/ru), kind, place, core line, argv, note
      │
      ├─ GET /api/menu ─────────────▶ the window's menu bar and its ⌘K palette (drawn from the answer)
      │
      ├─ GET /api/menu?shell=1 ─────▶ the macOS menu bar and the shield's menu (built from the answer)
      │
      └─ POST /api/menu/run {id} ───▶ the server runs *the entry's own argv* with the core and
                                      returns the core's stdout/stderr verbatim
```

| kind | what happens when the entry is chosen |
|---|---|
| `view` | the window opens one of its views; the view reads the core as it always did |
| `page` | the window runs one of its own flows by id (`PAGE_ITEMS` — the same call the button makes) |
| `run` | the server runs the entry's argv and shows the core's own output |
| `ask` | the window asks one value (folder, path, moment, label, text), then a `run` |
| `confirm` | like `run`, but the server refuses with 409 unless the window has already asked |
| `info` | nothing is run: the window shows the exact command and why it stays in the terminal |

**Why this cannot become a list of buttons that look like features.**

1. `POST /api/menu/run` looks the id up in the registry (`menu::find`). An id that is not there is an
   error, not a command: the endpoint is not a command runner, and it is not a shell — the argv is
   passed as an argument vector, never through `sh -c`.
2. Values are inserted as **one argument each** (`menu::substitute`). A value that begins with `-` is
   refused before the core sees it, because the core would read it as an option: `pl snap p --json`
   saves a mark with the wrong label and changes the output shape while saying nothing.
3. An entry that changes what is stored is either a `confirm` entry or one of the six acts the window
   already performs with its own button (`scan-once`, `pause`, `resume`, `snap`, `note`, `mark`).
   `tools/menu_contract_check.py` refuses the build when that stops being true.
4. The window holds **no second copy** of the list: it draws the server's answer, and the only ids it
   names are the seven flows in `PAGE_ITEMS` — which the checker requires to be exactly the registry's
   `page` entries, in both directions.
5. The macOS menu bar is built from the same answer, and the shield's menu too; the shell keeps no
   list of its own beyond the two acts macOS puts elsewhere anyway (Quit in the application menu,
   Close in File) and the documentation folder.

**How it is checked.** `tools/menu_contract_check.py` reads the registry, the page, the route table
and the shell (static half), then starts the real server on a throw-away archive and *runs every
entry*: the `run` entries are compared with the same commands run directly by the checker, the `ask`
entries are tried without a value (refused) and with one, the `confirm` entries are tried without the
confirmation (409, and the archive must be byte-identical afterwards) and with it, an id outside the
registry and an entry the shell owns must both be refused, and a flag-shaped value must be refused.
`tools/menu_control.py` injects twelve faults into a copy and requires the checker to catch all
twelve; the window's acceptance run clicks all sixty entries in a real browser and fails if any of
them leaves an error banner behind (`tools/ui_e2e.py`, step 16).

---

## Round 300 — the repair path, the ledger, and a live configuration

Three additions, each one a place where a promise had no code behind it.

### 1. `restore --missing` — creating is the only power the mode has

`RestoreOptions.missing` is one boolean, and the difference it makes is in two places, deliberately
the same rule written twice:

* in `restore::plan`, a path that already exists is **not added to the plan at all** — it is counted in
  `RecallPlan.present` and skipped before the symlink/hash logic runs, so it cannot reach `files`;
* in `restore::execute`, before writing each file of a plan, `symlink_metadata(&dst).is_ok()` sends it
  to `skipped_present` instead. A preview is a picture of the past; the write happens in the present,
  and the rule is the rule at both moments.

Because the mode can only create, `plan.extra` (the `--clean` deletion set) is never populated, and
`--missing --clean` is refused with a sentence naming both flags. `execute` writes through the same
`store.read_verified` → `util::write_atomic` path as any restore, so the blob's sha256 is checked
before a byte is written; a file whose blob is missing is counted in `missing` and left absent.

The window adds the layer the core cannot: `app/src/api.rs::repair_job` hashes every file under the
project root **before** running the core and again afterwards, with its own sha256, and reports any
pre-existing file whose bytes changed (`overwritten`) or that vanished (`deletedByRepair`) as a
violation. A repair that damaged something is reported as damaged, not as success — a verification that
shares the code of what it verifies proves nothing.

### 2. The notification ledger

`daemon::notify_full(kind, project, title, body)` does three things: the human line in
`logs/projectlife.log` (unchanged), one JSON line appended to `logs/notifications.jsonl`, and the
platform notification if the configuration allows it. The order matters: the ledger is written even
when desktop notifications are switched off, because "notifications are off" means "do not interrupt
me", not "do not record".

`daemon::read_notifications(archive, limit)` reads at most the last 512 KiB of the file, parses what it
can and drops what it cannot (a half-written tail line is a crash artefact, not an error), then keeps
the newest `limit` rows. `pl notifications` filters by `--kind` and `--since` in the core, so a filter
that matches nothing answers zero rather than falling back to everything.

The window's screen (`viewNotifications`) draws the server's answer; it holds no copy. The repair
button on a mass line calls `POST /api/project/repair`.

### 3. FR-CFG-3

`Config::fingerprint(root)` is the sha256 of `config.json` ("absent" when there is no file). mtime is
not enough: two writes inside one second share an mtime, and a copy changes it without changing
anything. `Archive::reload_config` compares that fingerprint, reloads through `Config::load`, and
returns the list of keys whose *value* differs — a file that was reformatted reports an empty list
rather than a change that did not happen.

The daemon calls it at the top of every cycle, and `SIGHUP` (a handler beside the existing
SIGTERM/SIGINT one) sets a flag that forces the call at once. The keys the loop holds in local
variables are re-applied by `apply_live_config`, which is the only place that can reschedule the loop:
the interval family (with `next_periodic` moved to now, so a shortened interval does not wait for the
old, longer deadline), `debounceMs`, `triggerResyncCycles`, and `watchTriggers`. Everything else in
`config.json` is read by the cycle itself and needs nothing.

`Archive::write_config_state` puts the result in `<archive>/config_state.json` for a *different*
process to read: the counter of a running daemon is otherwise unreachable, and a claim about live
reloading that only exists in one process's memory cannot be checked. `daemon status` prints it, the
window can read it, and `tests/round300.rs` asserts on the file rather than on the log sentence.

## The applications: three shells, one server, one core (round 301)

```
        Project Life.app        projectlife-app        ProjectLife.exe      <- the shells
        (Objective-C,           (GTK 3 +               (Win32 +                window, tray,
         AppKit + WebKit)        WebKitGTK 4.1)         WebView2)              menu, dialogs
              |                       |                      |
              +-----------+-----------+----------------------+
                          |  HTTP on 127.0.0.1, one token per launch,
                          |  one page, one menu document, one set of routes
                  projectlife-ui                       <- the interface server
                  (a small HTTP server; drives the core, verifies what it wrote)
                          |
                          |  arguments in, JSON out — never a library call
                          v
                     projectlife                        <- the core: storage, history, restore,
                  (observation, journal, blobs,             export, import, repair, MCP server
                   cache, restore, export, import)
```

The rule this shape enforces: **a shell may not contain a second implementation of anything.** A shell
starts the server, loads the page it serves, builds its menu from the server's `GET /api/menu` answer,
and performs an action by calling a route. It has no idea how a blob is stored, what a journal line
looks like, or how a restore is verified — and it cannot get those wrong, because it does not have
them.

What a shell *does* own is exactly what the platforms differ in, and each of those lives in one file:

| file | what it holds |
|---|---|
| `app/macos/ProjectLife.m` | the AppKit window, the menu-bar shield, the native folder chooser, the `pl://` scheme handler |
| `app/linux/ProjectLife.c` | the GTK window, the appindicator, the GTK folder chooser, the `pl://` scheme handler |
| `app/windows/ProjectLife.c` | the Win32 window, `Shell_NotifyIcon`, the WebView2 callbacks, the registry entry |
| `app/src/sys.rs` | what the *server* needs from a platform: pid, parent pid, "ask it to stop", detached spawn, random bytes, the OS line, the app folder |
| `src/watch.rs` | the notification backend: inotify (Linux), `FindFirstChangeNotificationW` (Windows), periodic only (macOS) |

Three consequences worth knowing when reading the code:

* **The stop request is a file, not a signal.** `archive::request_stop` writes `<archive>/stop.request`
  with the target pid; the daemon reads it each cycle and leaves between passes. On unix `SIGTERM` is
  sent too, but the file is the mechanism every platform has, and it is what `daemon stop` waits on
  (together with the daemon-lifetime lock, because a reaped-but-unwaited child still answers
  `kill(pid, 0)`). A request naming another process is ignored, and one left behind by a dead daemon is
  removed at startup so it cannot stop the next one.
* **The menu has two serializations of one table.** `menu::json` (what the page and the macOS shell
  read) and `menu::plain` (one line per entry, what the C shell reads, because a hand-written JSON
  parser could not be tested on the machine it runs on). A test compares them field by field, and the
  route serves both (`/api/menu?plain=1`).
* **`--native` is a promise about dialogs.** When the server runs inside a shell, the page asks the
  shell for things only a native program can do (a folder chooser) and the shell answers JSON. When no
  shell is there, the page falls back to a dialog it draws itself — which is why the import flow does
  not die in silence in a browser (round 296).
