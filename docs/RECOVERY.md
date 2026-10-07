# Recovery scenarios

## An agent just wiped or rewrote the project

```sh
projectlife status my-app                 # was the project observed in the last minutes?
projectlife last-good my-app              # the moment just before the last mass change
projectlife panic my-app                  # offers candidates, shows a preview, restores to a new folder
```

`panic` never writes into the project folder. Look at the restored folder, and only then decide to
replace the project yourself. If the archive has no mass events, use an explicit moment:

```sh
projectlife restore my-app --at "10 minutes ago" --to ../my-app-recovered
```

An automatic notification is sent at the moment of the anomaly, with the exact restore command
inside it — that is the notification's whole purpose.

## A single file

```sh
projectlife restore my-app --path src/app.ts --at "2026-10-05 13:50" --to /tmp/one-file
# or, without the program:
python3 recover.py --archive /Volumes/backup/projectlife export-file --project my-app \
        --path src/app.ts --at "2026-10-05 13:50" --out ./app.ts
```

`pl why my-app src/app.ts` tells you whether the path is tracked at all and when its last version
was recorded. `pl why my-app --at "yesterday 22:00"` tells you why a *moment* is unavailable (a gap,
or a pruning boundary).

## "My file was never saved"

```sh
projectlife why my-app path/to/file
```

You get one of: tracked (with the last version time), or the exact reason and rule —
`secret`, `too_large`, `binary`, `hidden`, `ignored_dir`, `unreadable`, `rule`. The fix is usually to
add the path to the project's `include` list in `project.json` and run:

```sh
projectlife apply-filters my-app     # runs a cycle now and prints what started/stopped being tracked
```

## The archive disk was unplugged

The program keeps running (or the timer keeps firing) and reports `ARCHIVE_OFFLINE`; a notification is
sent once and repeated hourly. Nothing is lost except the states that changed while the disk was
away — and those changes are captured at the first cycle after the disk returns, with a `gap` event
recording the interval in which the program could not observe.

## The archive is missing or damaged

```sh
projectlife check my-app --deep      # re-reads every blob and compares sha256 with its name
projectlife doctor                   # the whole picture, with the command to fix each item
```

A corrupted blob is never deleted: it is moved to `projects/<id>/quarantine/<sha256>`, and a
`meta(blob_corrupted)` event is written. Versions that reference it are not restored; the others
are, and the exit code is 2. `pl quarantine list` shows what is in quarantine;
`pl quarantine restore <hash>` puts a blob back after you have verified it yourself.

If the journal is damaged in the middle, the project goes to `state: "error"`, writing to it stops,
and nothing is deleted. Read what is still readable with:

```sh
python3 recover.py --archive <ARCHIVE> check --project my-app
python3 recover.py --archive <ARCHIVE> events --project my-app --limit 50
```

## A four-file test of the whole promise

`pl drill <project>` does this on your own machine, with your own data:

1. takes a fresh observation so the current state is in the archive;
2. copies what is on disk right now into a sandbox (the "original");
3. restores the same moment from the archive into a second folder (as if the project was wiped);
4. compares every file byte for byte and prints the measured restore time.

```sh
projectlife drill my-app --json     # {"verdict": "PASS", "filesChecked": 412, "restoreMs": 51, ...}
```

Run it after setup and after any change to how the program runs. A promise that was never tested on
the live machine is a hope, and `drill` is the cheapest way to turn it into a measurement.

## Restoring into the project itself

Only when you mean it:

```sh
projectlife restore my-app --at "10 minutes ago" --into-project          # asks you to type the name
projectlife restore my-app --at "10 minutes ago" --into-project --clean  # also deletes newer extras
```

Before writing, the program takes a fresh observation, so the state you are about to overwrite is
also in the archive. `--clean` lists what it would delete and asks a second time when the change is
large. A preview of everything is available first: add `--preview`.
