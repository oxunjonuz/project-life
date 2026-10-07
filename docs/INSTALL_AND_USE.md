# Install and use

## Build

```sh
cargo build --release        # requires a Rust toolchain; no runtime dependencies in the result
target/release/projectlife --help
```

The binary is self-contained. `tools/recover.py` (Python 3 standard library only) is an optional
bonus that recovers without the program; copy it into the archive root if you want it there.

## First run

```sh
projectlife init-archive /Volumes/backup/projectlife      # a DIFFERENT disk than your projects
projectlife add ~/work/my-app --name my-app
```

`add` prints an estimate (files, size, skips by reason), warns if the archive is on the same disk as
the project, and stores one initial snapshot. Nothing is copied without that single confirmation
(`--yes` skips the question; `--no-initial` skips the snapshot and warns).

## Running it

**Option A — a resident daemon**

```sh
projectlife daemon run          # foreground, for debugging and service managers
projectlife daemon install      # writes a systemd user unit / LaunchAgent / scheduled task
projectlife daemon start|stop|status
```

**Option B — an external timer (recommended on machines that see agents)**

```sh
projectlife scan-once --all     # exactly one observation cycle; cron it
```

`daemon install` writes both: the resident unit and an equivalent timer entry. A timer runs a
short-lived process that takes the same archive lock, so two cycles can never overlap. Its accuracy
of the promise equals the timer period — the program states that in its own output.

Example crontab entry (every minute):

```
* * * * * /usr/local/bin/projectlife scan-once --all >> ~/.local/share/projectlife/timer.log 2>&1
```

## Universal mode: protecting any files

Project Life does not need your folder to be code. Point it at a folder and it looks at **names,
sizes and extensions only** — it never opens a file to decide what the folder is.

```sh
projectlife detect ~/Documents            # read-only: what is this folder, and why
projectlife detect ~/Documents --json     # the same answer as data
projectlife presets                       # the ten built-in profiles and their limits

projectlife add ~/Documents --preset auto        # detect, show the evidence, ask
projectlife add ~/work/app  --preset developer   # force a profile
projectlife add ~/Design    --preset custom       # start from what is actually there
```

`--preset auto` prints what it found, how confident it is, what it would include and exclude, the
estimated archive size and anything the safety rules are holding back, then asks
`Proceed? [Y/n/edit]`. Type `edit` to change the include list, the size limit or the profile; or use
`--edit-add .md --edit-remove .pptx` to do the same thing non-interactively (and `--yes` to answer
the question upfront, which is what scripts and tests do).

Three examples:

```sh
# an office worker's documents: never a video, never an archive, never .env
projectlife add ~/Documents --preset office --yes

# a designer: .psd/.ai/.fig are the work, so media is protected rather than excluded
projectlife add ~/Design --preset designer --yes

# a photographer: raw files and the Lightroom catalogue; a 2 GB limit, not 2 MB
projectlife add ~/Pictures/2026 --preset auto --yes
```

**What a preset can never do.** Secrets (`.env`, `id_rsa`, `*.pem`, `credentials.json`, `.ssh/*`…),
system locations (`/etc`, `/usr`, `/Windows`…), dependency folders (`node_modules`, `dist`, `build`,
`target`, `.venv`…) and temporary files (`*.tmp`, `~$*`, `.DS_Store`…) are never included
automatically — not even when the profile's own list would match them. `pl detect /etc` and
`pl add /etc --preset auto` are refused outright. Two flags are the only way past a hard rule, and
both are explicit:

```sh
projectlife add ~/notes --preset writer --yes --include-secrets     # warns: the archive is not encrypted
projectlife add ~/video --preset video  --yes --include-large-files # warns: sizes, not a promise of speed
```

Everything the preset decides is written into the project, not kept in the tool: `project.json` gets
the `preset` block with the evidence (extension counts, markers, folders, entries scanned) plus the
`include`/`exclude`/`maxFileSizeKb` the filter actually uses, and the journal gets a `filters` event
with `reason: "preset_applied"`. Change your mind later by editing those globs and running
`pl apply-filters <project>`.

## Commands

```
Archive     init-archive <path> | archive-move <new-path> | audit-archive [--update]
Projects    add <path> [--name N] [--profile source|all|ask] [--yes] [--no-initial]
            add <path> --preset auto|<profile>|custom [--yes] [--edit-add EXT] [--edit-remove EXT]
              [--include-secrets] [--include-large-files]
            detect <path> [--json] | presets [--json]
            list | status [project] [--skipped] [--risk] [--json]
            pause | resume | remove (alias detach) | relink <project> <new-path>
            note <project> "text"
History     log <project> [--path P] [--since T] [--until T] [--type …] [--json] [--pretty]
            tree <project> --at T | diff <project> --at T [--to T2|--current]
              [--path P] [--name-status|--stat|--files-only|--content]
Restore     restore <project> --at T | --mark M | --last-good
              [--path P …] [--to DIR] [--into-project [--clean]] [--preview] [--yes]
            panic <project> [--to DIR] | last-good <project> | mark <project> "label"
Answers     why <project> <path> | why <project> --at T
Lifecycle   prune <project> --before T [--dry-run] [--yes]
            export <project> [--from T] [--to T2] --out DIR [--pack]
            export-and-prune <project> --before T --out DIR
            import <export-dir> [--into P | --new] | archive-delete <project> [--export-first DIR]
Checks      check [project] [--deep] [--fix] | rebuild-cache [project]
            doctor [--json] | quarantine list|restore <hash>
Running     scan-once [--all | <project>] | daemon run|install|uninstall|start|stop|status
Promise     drill <project> [--json]
Other       config get|set <key> [value] | version | help
```

Global flags: `--archive <path>`, `--json`, `--yes`, `--lang <en|ru>`.
If the archive path is not given, it is read from `~/.config/projectlife/location.json`
(`~/Library/Application Support/projectlife/location.json` on macOS,
`%APPDATA%\projectlife\location.json` on Windows), then from `PROJECTLIFE_ARCHIVE`.
`PROJECTLIFE_HOME` overrides the location file (useful for tests).

## Time specifications for `--at`

`now` · `10m ago` · `2h ago` · `3d ago` · `1 minute ago` · `yesterday 14:00` · `today 09:00` ·
`2026-10-05 14:30` · `2026-10-05T14:30:00+05:00` · `2026-10-05` · `5.10.2026 14:30` · `seq:12345` ·
a mark label with `--mark` · `--last-good`.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | success |
| 1 | error (nothing was changed unless the message says so) |
| 2 | partial success — for example some versions could not be restored, or a restore finished with failures |
| 3 | cancelled by the user |

## Configuration

```sh
projectlife config get
projectlife config set intervalSeconds 5
projectlife config set massChangeFiles 50
projectlife config set language ru
```

Keys: `intervalSeconds`, `minIntervalSeconds`, `maxIntervalSeconds`, `autoInterval`, `debounceMs`,
`watchTriggers`, `triggerResyncCycles`, `deepVerifyIntervalMinutes`, `profile`, `warnFreePercent`,
`warnFreeBytes`, `stopFreePercent`, `stopFreeBytes`, `massChangeFiles`, `massChangePercent`,
`minMassFiles`, `lowPriority`, `maxConcurrentReads`, `notifications`, `language`.

With `watchTriggers` on (the default on Linux) a change is normally stored about `debounceMs`
(1.5 s) after the writing stops, while the periodic pass keeps its own rhythm as the safety net.
`pl daemon run --no-watch` does the same run with the periodic pass as the only trigger, and
`pl daemon status` prints which mode is in force. Per-project settings live in
`project.json` → `settings` and override the global ones.

## First useful commands after a week
```sh
projectlife status --risk          # state, measured observation window, risk items
projectlife doctor                 # the whole picture plus the fixing command per item
projectlife audit-archive --update # a digest of the archive; compare later to detect tampering
projectlife drill my-app           # re-test the promise on this machine
```

## Registering the MCP server (an agent reads, never writes)

The server is a second binary; it needs no configuration beyond the archive root:

```sh
target/release/pl-mcp --archive /Volumes/backup/projectlife      # or PROJECTLIFE_ARCHIVE
```

A client that speaks MCP over stdio registers it like this (the same snippet `pl mcp info` prints):

```json
{
  "mcpServers": {
    "projectlife": {
      "command": "/path/to/pl-mcp",
      "args": ["--archive", "/Volumes/backup/projectlife"]
    }
  }
}
```

What the agent gets: `pl_status`, `pl_log`, `pl_why`, `pl_tree`, `pl_diff`, `pl_last_good`,
`pl_check`, `pl_doctor`, and `pl_plan_restore`. What it cannot get, ever: `restore`, `panic`,
`prune`, `import`, `export-and-prune`, `archive-delete`, `mark`, `pause`/`resume`/`remove`,
`scan-once`, `doctor --fix-lock`, `check --fix`. Those names are not advertised and a call to one is
refused as a write operation, so the agent's only way to change anything is to give you the command
and let you run it (`pl_plan_restore` returns exactly that command).

Two knobs, both honest: at most 10 calls per second per tool, and file contents only when the caller
asks with `include_content: true` — and even then never for a path the project's filters call a
secret. Every request is written to `<archive>/logs/mcp.log`: who asked (the client name), which
tool, which parameters, the outcome, the duration. That file is the only thing this process writes.

## Running the checks from a scheduler

`pl` never opens a network connection, so it cannot tell anyone it stopped. It can tell a *scheduler*,
and the shell can pass that on. In a crontab:

```cron
# every 5 minutes: one cycle, then a ping only if the cycle produced a fresh heartbeat
*/5 * * * * /path/to/projectlife --archive /Volumes/backup/projectlife scan-once --all >/dev/null 2>&1
*/5 * * * * /path/to/projectlife --archive /Volumes/backup/projectlife heartbeat-check || curl -fsS -o /dev/null https://hc-ping.com/<uuid>

# once a day: everything doctor knows, mailed only when something is wrong
30 8 * * * /path/to/projectlife --archive /Volumes/backup/projectlife healthcheck --strict || /path/to/notify-me.sh
```

Exit codes: `heartbeat-check` — 0 fresh, 1 stale or absent, 2 stale **and** a live process holds the
daemon lock (the two disagree). `healthcheck` — 0 healthy, 1 something is wrong; `--strict` also
fails on warnings. With systemd, the same two commands are `ExecStart` of a timer's service; with
launchd, a `StartInterval` job. The archive does not care which one you use, and `pl daemon status`
prints which mode it is in.

## The daily commands

| Command | What it is for |
|---|---|
| `pl recent [--limit N]` | the last mass events, gaps, marks and program operations, with the command each implies |
| `pl since <project> [--at T] [--path P] [--stat]` | what changed since the last mark, by content, not by timestamp |
| `pl cat <project> --path P [--at T] [--out F]` | one file's bytes at one moment, straight out of the archive |
| `pl blame <project> <path>` | every event that touched a path, newest first |
| `pl size [--top N]` | what each project costs and which files weigh the most |
| `pl list --sort name\|size\|age\|risk`, `pl status --compact` | the archive at a glance, with the next command per row |
| `pl suggest [project]` | one to three actions that follow from what is wrong right now |
| `pl snap <project> [label]` | a mark in one word, before something risky |
| `pl watch <project> [--for SEC]` | new events in the terminal as they are recorded |
| `pl gc [--dry-run]` | how many blobs nothing references (it never deletes; `check --fix` does) |
| `pl undo <project>` | what the last program-write to your project was, and the command to reverse it — it does not run it |
| `pl prompt [--space]` | one line for a shell prompt: `pl:2p ok 3s` |
| `pl open <project> [--archive] [--launch]` | where the folders are |
| `pl completion bash\|zsh\|fish [--install]` | a completion script (installed into the archive, and it prints how to source it) |
| `pl notify test` | whether the notification channel actually reaches you |
| `pl mcp info\|tools` | what the agent surface offers, and how to register it |

