# Storage format

Principle: **history is ordinary files; everything else is a hint.** All data can be recovered
without the program (`recover.py`, or by hand with a text editor and `sha256sum`).

## Layout

```
<ARCHIVE_ROOT>/
├── README_RECOVERY.txt                 how to recover without the program
├── config.json                         global settings
├── heartbeat                           time of the last observation cycle (epoch ms)
├── .lock                               held during a cycle (daemon or scan-once)
├── logs/projectlife.log                human-readable operation log
├── manifest/YYYY-MM-DD.jsonl           archive digest (audit-archive)
├── tmp/                                scratch space (drills and temporary work)
└── projects/
    └── <project-id>/                   a UUID v4, never derived from the path
        ├── project.json                project passport
        ├── blobs/<hh>/<hh>/<sha256>    version contents, byte for byte
        ├── events/YYYY-MM.jsonl        event journal (the truth)
        ├── cache/base.jsonl            rebuildable cache: every tracked path, sorted (safe to delete)
        ├── cache/base.meta.json        entry count, validated against the base's size
        ├── cache/delta.jsonl           cache records written since the last base write (append-only)
        ├── cache/skips.jsonl           (path, reason) pairs already written as `skip` events
        ├── cache/tail.json             the journal's fast index: last seq, last ts, last observation
        ├── cache/state.json.v1-*       a format-1 cache, kept aside after migration (if any)
        ├── quarantine/<sha256>         corrupted blobs (never cleaned automatically)
        ├── tmp/                        temporary files during writes and pruning
        └── prune.journal               present only while pruning
```

`recover.py` (shipped with the program) lives in the archive root as well, so a stranger who finds
the disk can recover a file without installing anything.

## `project.json`

```json
{
  "schemaVersion": 1,
  "projectId": "6b1a7c0e-5a3e-4c35-9a0f-1c2e8f3d9b77",
  "name": "my-app",
  "projectRoot": "/Users/uc/work/my-app",
  "createdAt": "2026-10-05T08:12:44.120Z",
  "historyStartsAt": "2026-10-05T08:12:44.120Z",
  "profile": "source",
  "settings": { "maxFileSizeKb": 2048, "includeSecrets": false, "useGitignore": false },
  "state": "active",
  "lastSeq": 18423,
  "lastObservedAt": "2026-10-05T14:31:07.552Z",
  "observedIntervalMs": { "median": 5120, "p95": 9400, "samples": 200 },
  "note": "", "tags": [], "lastGapAt": "2026-10-05T09:02:00.000Z"
}
```

`state` is one of `active`, `paused`, `path_missing`, `removed`, `initializing`, `error`.
Times are UTC ISO 8601 with milliseconds; internal times are epoch milliseconds.

## The state cache (`cache/`)

The cache is **derived**: every entry in it can be recomputed from the journal, and deleting the
whole directory costs time at the next pass and nothing else. It exists so that a cycle does not have
to read the journal to know what the project looked like.

Two files, because a pass must be able to read and write only the part it touched:

* **`cache/base.jsonl`** — one JSON record per tracked path, **sorted by `path`**, one per line, so
  a record is found by a binary search over byte offsets and a directory's subtree by a seek and a
  forward read. Written by a full pass, by a compaction, or by a rebuild; never by an ordinary
  partial pass. `cache/base.meta.json` carries `entries` (the count), `bytes` (the size the count
  was true for) and `cause` (`full`, `compaction`, `rebuild`, `rebuild-cache`, `migration-from-v1`).
* **`cache/delta.jsonl`** — append-only records written since the last base write, in order. The
  **last record for a path wins**; `{"op":"del"}` is a tombstone ("no longer tracked"). A record is
  `{"path":…,"at":…,"was":<bool>,…entry…}` where `was` says whether the path was tracked when the
  record was written, which is what makes the count of tracked paths a sum over the file instead of
  a search in the base.

  Folding the delta into a new base — writing `base.jsonl`, then emptying `delta.jsonl` — **is
  idempotent**: applying the same records again to a state that already contains them changes
  nothing. That is why no swap journal is needed for it, and why a crash between the two writes
  costs nothing.

A record is the same shape as a journal `put` plus the identity of the file:

```json
{"path":"src/lib/mod0.ts","hash":"9f2c…","size":41,"mtime":1791250429441,"mode":420,
 "kind":"file","target":null,"dev":43,"ino":20902200}
```

Rules a reader can rely on: the base is sorted by path; a line that does not parse is a damaged file
(the program then rebuilds the cache from the journal); a half-written **last** line of the delta is
ignored, and the next append either closes it (if it parses) or cuts it off (if it does not);
`cache/tail.json` is an index of the journal and is only believed when its `seqLast`/`tsLast` equal
the last line of the newest journal file.

`tools/cache_vs_journal.py` is an independent reader of this format (written from this description,
sharing no code with the program). It replays the journal, applies the delta over the base and
reports any path where the two disagree — which is how this round's changes were checked.

## Events (`events/YYYY-MM.jsonl`)

One JSON object per line, append-only, one file per calendar month. Common fields:

| Field | Meaning |
|---|---|
| `seq` | monotonic counter inside the project; **the order of truth** |
| `ts` | UTC epoch milliseconds — used by `--at` and by `state_at` |
| `type` | see below |
| `batchId` | groups the events of one cycle (`b-<first seq of the cycle>`) |
| `reason` | `initial`, `observed`, `startup`, `pre_restore`, `filters_changed`, `deep_verify`, … |

| Type | Fields | Meaning |
|---|---|---|
| `snapshot` | `reason`, `files` | initial copy (`initial`), pruning boundary (`prune-anchor`), export boundary (`export-anchor`) |
| `put` | `path`, `hash`, `size`, `mtime`, `mode` | a new observed version of a file |
| `delete` | `path` | the file disappeared |
| `move` | `from`, `to`, optional `unreadable` | rename/move, detected by file identity or by matching contents. `unreadable` is present when the bytes at the new path could not be read in that cycle: the move is real but no version is written, the last known version is carried over, and the next cycle records the real contents |
| `symlink` | `path`, `target`, `mode` | a symlink, recorded and never dereferenced |
| `skip` | `path`, `reason`, `rule` | a path that is not tracked, with the exact reason and rule |
| `gap` | `from`, `to`, `reason` | the program was not observing during that interval |
| `pause`, `resume` | | observation paused and resumed by the user |
| `mass` | `kind`, `files`, `deleted`, `changed`, `created`, `percent`, `samplePaths`, `lastGoodSeq`, `batchId` | an anomaly: `mass_delete`, `mass_change`, `mass_create`, `suspicious_rewrite` |
| `mark` | `label` | a named moment you can restore to (`--mark`) |
| `filters` | old/new values | the profile or the rules changed |
| `meta` | `reason`, … | `mode_changed`, `blob_corrupted`, `clock_skew`, `path_conflict`, `relink` |
| `restore` | `at`, `target`, `files` | a restore was performed |
| `prune` | `before` | history was pruned; `historyStartsAt` moved |
| `import` | `from`, `events`, `blobs` | data was merged from an export |

Unknown fields are ignored by readers, so the format can grow. A format change bumps
`schemaVersion`; the program reads all previous versions.

## The state at moment T (the only algorithm that matters)

```
state = {}
for event in journal sorted by seq:
    if event.ts > T: continue
    switch event.type:
        put:     state[path] = {hash, size, mode}
        symlink: state[path] = {target}
        delete:  remove state[path]
        move:    state[to] = state.pop(from)
# anything else (snapshot, mass, gap, skip, mark, meta) does not change the file set
```

Two consequences worth stating plainly:

* `seq` decides the order; `ts` decides inclusion. A backwards clock jump therefore cannot reorder
  history — it is recorded as `meta(clock_skew)` and the state computation stays consistent.
* A half-written last line after a crash is ignored (the reader stops at it); garbage in the
  **middle** of a journal is an error and the project goes to `state: "error"`. Nothing is deleted
  in that case.

## Blobs

* Path: `blobs/<first 2 hex chars>/<next 2 hex chars>/<full sha256 hex>`.
* Contents: the original bytes, no compression, no header, no metadata of ours.
* Verified on every read: a blob that does not hash to its own name is never written out as a file.
* Write order is always **blob → journal event → state**. An event can never reference a blob that
  is not on disk, which is why a crash cannot produce a journal entry without contents.
* Deduplication is per project: the same contents are stored once, however many times they are
  observed.

## `prune.journal` (transient, per project)

`prune` is the only operation that rewrites the journal. It does so in a fixed order and writes one
line to `projects/<id>/prune.journal` before each phase, so that a crash can be told apart from a
finished operation:

```
{"phase":"start","stamp":<ms>,"before":<ms>,"project":"<id>","extra":""}
```

| phase | what is already true on disk when this line is written |
|---|---|
| `start` | nothing has been touched |
| `journal_built` | the new journal exists in `tmp/events.new-<stamp>`, not yet swapped in |
| `journal_swapped` | `events/` is the new journal; `project.json` still describes the old history |
| `meta_saved` | `project.json` has the new `lastSeq` and `historyStartsAt`; unreferenced blobs remain |
| `blobs_removed` | the unreferenced blobs are gone; the file is deleted right after |

Recovery (`recover_prune`, run before any operation that writes or reports state, and explicitly by
`pl recover <project>`):

* `start`, `journal_built` → **roll back**: the new journal is discarded. If the crash landed between
  the two directory renames (the events directory is missing), `events.old-<stamp>` is renamed back.
* `journal_swapped` → **complete**: keep the new journal, recompute `lastSeq` and `historyStartsAt`
  from it, then delete the unreferenced blobs.
* `meta_saved`, `blobs_removed` → **complete**: delete the unreferenced blobs (idempotent).

The journal is never rewritten by the recovery, and no phase can lose a version: either the old
journal is back in place, or the new one already contains the anchor state at `before` plus every
event after it.

## Recovery without the program

1. Open `events/YYYY-MM.jsonl` in a text editor. Find the last `put` for the path you want whose
   `ts` is not later than the moment you need.
2. Copy `blobs/<hh>/<hh>/<hash>` under the name you want.
3. Check it: `sha256sum <file>` must equal the blob's own file name.

Or let `recover.py` do it:

```sh
python3 recover.py --archive /Volumes/backup/projectlife tree --project my-app --at "today 09:00"
python3 recover.py --archive /Volumes/backup/projectlife export-file --project my-app \
        --path src/app.ts --at "yesterday 18:00" --out /tmp/app.ts
python3 recover.py --archive /Volumes/backup/projectlife export-tree --project my-app \
        --at "2026-10-05 14:00" --out /tmp/recovered
python3 recover.py --archive /Volumes/backup/projectlife check --deep
```
