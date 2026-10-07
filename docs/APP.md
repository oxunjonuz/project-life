# Project Life — the desktop app (`app/`)

Author: Oxunjon Ubaydllayev ⟨oxunjonub@gmail.com⟩ · MIT licence ·
`Copyright (c) 2026 Oxunjon Ubaydllayev and Aiodam`.
Where every one of those words is written, and which check keeps them from drifting, is
`docs/BRAND.md`; the values themselves live only in `src/brand.rs`.

The desktop app is a **shell around the existing core**. It adds a window, a menu-bar item and a
set of screens; it does not add a second implementation of observation, history, restore, export or
import. The rule that made this possible: *every mutation is a command line of the same program, and
every number on screen comes from that program or from the archive's own files.*

```
app/
    Cargo.toml            package projectlife-app, binary projectlife-ui
    src/main.rs           starts the local server, prints one line with the port and token, waits for SIGTERM
    src/http.rs           a small HTTP/1.1 server on 127.0.0.1, one thread per request
    src/api.rs            the routes: state, detect, add, history, restore, export, import, watch, doctor
    src/pl.rs             running the core binary, reading its JSON, hashing files, space, local time
    src/jobs.rs           long work (add, restore, export, import, a pass) as poll-able jobs
    src/assets.rs         the window's HTML/CSS/JS, compiled into the binary
    ui/                   index.html, app.css, app.js  (the design in the Figma export's own tokens)
    macos/ProjectLife.m   the macOS shell: NSWindow + WKWebView + NSStatusItem + NSOpenPanel
    macos/Info.plist
    macos/build_app.sh    builds the whole .app, including the two Rust binaries, for aarch64-apple-darwin
    macos/README_MACOS.md how to open it, what is verified, how to rebuild it
```

## Why a shell and not a new engine

The core is a single Rust program with a documented command line and JSON output. Two consequences
shaped this layer:

1. **The UI has no storage code.** Adding a folder runs `projectlife add … --yes`; the coverage step
   runs `projectlife add … --dry-run --json`, which computes the same estimate and writes nothing;
   a restore runs `projectlife restore … --yes`. If the core's behaviour changes, the app follows it
   without a second implementation drifting out of step.
2. **Reads go through the same door.** Status, moments, trees, heartbeat and the trigger state come
   from `projectlife status|log|tree|heartbeat-check --json`, `watch_state.json` and `config.json` —
   all of them readable from a terminal, all of them printed by the app in the logs it shows.

The one thing the app does itself is *check*, not *do*: after a restore it hashes every restored file
and compares it with the hash the archive recorded, and after an export it re-hashes every entry of
`MANIFEST.sha256`. A verification that shares the code of what it verifies is not a verification.

## Screen → core, operation by operation

| Screen / control | Core operation | What the screen shows |
| --- | --- | --- |
| *Choose the archive* (first run, Settings) | `projectlife init-archive <dir>` (which also writes `location.json`) or `pl add`-free validate for an existing archive | path, free space, whether it is already an archive |
| *Add a folder*, step 1 | `projectlife detect <dir> --json` — metadata only, no file is opened | extensions found, markers, folder hints, confidence, the preset it suggests |
| step 2 (coverage) | `projectlife presets --json` plus `projectlife add <dir> --preset <id> [--edit-add E] [--edit-remove E] --dry-run --json` | **the estimate before anything exists**: files, bytes, what is skipped and why (secrets, temporary, dependencies, size), same-disk warning |
| step 3 (storage) | `projectlife config`/`init-archive` for the destination; the same-disk question is computed from the two paths | free space, same-disk warning |
| step 4 + *Start protecting* | `projectlife add <dir> --preset <id> --yes` | the core's own output, streamed line by line into the job log |
| Dashboard | `projectlife status --json`, `heartbeat-check --json` | per project: state, versions, last observed, archive size, skipped reasons |
| *Pause* / *Resume* | `projectlife pause|resume <project>` | the project state |
| *Start / Stop observation* | the app starts `projectlife daemon run` as its own child, and stops it with SIGTERM | whether *this app* started it, the heartbeat mode the core reports, the mode's own sentence, the last check and its age |
| *History & restore* | `projectlife log <project> --json` (grouped by the timestamp each pass stamps on its events) | moments with their change counts, gaps |
| tree at a moment | `projectlife tree <project> --at <ISO> --json` | path, size, kind, hash of every file at that moment |
| *Restore selected* | `projectlife restore <project> --at <ISO> --path … --to <new dir> --yes` | the job log, then the app's **own** byte comparison per file |
| *Export the history* | `projectlife export <project> --out <dir>` | the job log, then the app's own re-hash of `MANIFEST.sha256` |
| *Import* | `projectlife import <dir> --new <name>` | exported event lines on disk vs the project the core created |
| Diagnostics | `projectlife doctor --json`, `healthcheck --json`, and the daemon's log file | the core's checks verbatim, each with the command that fixes it |
| Settings | `projectlife config set <key> <value>` | interval, notifications — and the note that the interval is read when the daemon starts |

`pl://pick-folder` and `pl://reveal` are the only two things the macOS shell provides to the window;
everything else is the app's own server. There is no third path into the archive.

## The window's files

The interface is HTML/CSS/JS rendered by the system WebKit view, compiled into `projectlife-ui`. Its
colours, sizes and radii come from the Figma export in `design/figma-PL` (ink `#202833`, secondary
`#606B79`, hairlines `#DDE2E8`, surfaces `#F8F9FB`/`#F0F3F6`, accent `#245FC4`, ok `#23704C`,
warn `#855B0A`, error `#AC3838`; 10 px cards, 7 px buttons; Inter at 11–30 px). The fonts named by
the design (Inter, Roboto Mono) are not shipped with the export, so the stack falls back to the
system UI font on macOS when Inter is absent — the app never pretends to have a font it lacks.

Two screens of the design are deliberately **not** built yet, because building them would mean
inventing data: the notification centre and the settings screens for filters/advanced. The tray menu
of the design *is* built, as the real menu-bar item.

## The menu (round 299)

The window has a menu bar of its own, a command palette behind **⌘K** (Ctrl+K elsewhere), and the
macOS menu bar holds the same structure — all three drawn from one list the app server publishes
(`GET /api/menu`). The list lives in `app/src/menu.rs`; nothing in the page or in the shell repeats it.

Eight groups, in the order the list defines:

| menu | what is in it |
|---|---|
| Project Life | About, Protection status…, Settings…, Diagnostics…, Network diagnosis…, Quit completely… |
| File | Add a folder to protect…, see what a folder holds…, create/use an archive, export the history…, import an export…, show the archive in the file manager, close the window |
| Protection | start/stop observation, observe once now, pause/resume this project, is anything being observed?, health check, send a test notification |
| History | the moments and the file tree, recent changes, search the history…, search inside file contents…, why did this file change…, which moment touched each line…, what changed since…?, the last good moment, snap a marker…, write a note…, mark this moment…, what needs attention?, what is watched right now, projects by risk |
| Restore | the history view, preview a restore at…, rehearse a restore, finish an interrupted prune, the command that would undo the last write, restore a moment into a folder… |
| Archive | integrity, check now, deep check, checks (doctor), audit the archive, the largest things, dangling bytes, quarantined blobs, rebuild the state cache, storage & retention…, the core's settings |
| Tools | the protection presets, show a file as the archive has it…, MCP: what another agent may ask, the core's version, shell completion (shown, not run), a prompt line (shown, not run), what is not in the window and why, every command… |
| Help | the quick guide, the documentation folder (opens in the file manager), what the menu deliberately leaves out |

**Every entry names the core command it stands for**, and the palette shows that command next to it —
so the menu teaches the terminal instead of hiding it. Choosing an entry either opens a view, runs one
of the window's existing flows, or runs the core command and shows its output verbatim in a result
card (the exact command line, the exit code, the duration and the core's own bytes).

Three details worth knowing when using it:

* an entry that needs a value (a folder, a path, a moment, a label, a line of text) asks for it in the
  window's own dialog, then runs the command with that value as **one** argument;
* an entry that changes what is stored asks first, and the dialog carries the command it is about to
  run;
* entries that need a project are shown greyed out **with the reason** ("no project is open") instead
  of failing after the click.

The machine-readable version of all of this is `docs/UI_COVERAGE.md`, and the rules are enforced by
`tools/menu_contract_check.py` (static and live) plus `tools/menu_control.py`, which proves the
checker can fail.

### The notification centre and the repair (round 300)

The design's notification centre was left unbuilt in round 295 with the reason written down: building
it would have meant inventing data. The data exists now — the core writes one structured line per
message it raises — so the screen is a view over `<archive>/logs/notifications.jsonl` and nothing else.

| Screen / control | Core operation | What the screen shows |
| --- | --- | --- |
| *What happened* (sidebar, `History → What happened?`, ⌘3) | `projectlife notifications --json` | every message the program raised, newest last, with its moment, kind, project and body; filter buttons for all / mass changes / disk space / errors / observation |
| *The last twenty messages, as text* (menu) | `projectlife notifications --limit 20` | the same ledger as a result card, so the command the screen stands on is visible |
| *Give back the missing files…* (button on any line naming a project) | `projectlife restore <project> --at <moment> --missing --into-project` | first the plan as data (`--missing --preview --json`: how many to create, how many already on disk and left alone, how many to delete — always zero), then, after the confirmation, the repair as a job |
| the repair's result card | the app's own hashing | how many files were created and verified byte for byte, and — computed by the app, not claimed by the core — how many pre-existing files changed and how many were deleted. Anything other than 0/0 is shown as **a violation**, in red |

Three things are worth knowing when using it:

* the moment defaults to the **last good state** (the moment before the last mass event), which is what
  a person wants after a wipe — but it is shown before anything is written, and can be overridden by
  the window asking for a moment instead;
* cancelling the confirmation runs nothing at all;
* a repair is the one write the window performs that deliberately *adds* to a project folder, so its
  result card is the only place where the window's own verification is the thing you read.

The window's *Settings* screen no longer claims that a saved interval needs a restart: since this round
the observation re-reads its configuration (`FR-CFG-3`), and `config_state.json` in the archive records
every re-read for anyone who wants to check.

## What the app deliberately does not do

* It never copies, moves or rewrites anything in a project or an archive by itself.
* It never shows a placeholder, a sample or a "coming soon" control.
* It never claims protection it cannot see: "Protected" needs a live observation, and a fresh
  heartbeat right after the app stopped its own daemon is reported as *Observation is stopped*.
* It never kills a process it did not start: if a daemon is running from a terminal or from launchd,
  Stop reports that, and leaves it alone (`daemon stop` on macOS is `launchctl unload`, which is not
  what a foreground daemon needs).
* It never opens a network port except the loopback one for its own window, and the Core's
  `NFR-SEC-1` (the core itself makes no connections) is untouched.
* Its menu contains no entry whose command the core does not have, and no entry that does nothing
  when it is chosen — checked by `tools/menu_contract_check.py`, which *runs* every entry, and by the
  window's acceptance run, which clicks all of them in a real browser.

## What the menu deliberately leaves in the terminal

The menu is not a claim that everything belongs in a window. The commands it does *not* run are named
in the window itself (Tools → *What is not in the window, and why*, and Help → the same list), and the
reason is next to each one. The short version:

| command | why it stays a terminal action |
|---|---|
| `pl panic` | it asks on a terminal which moment to fall back to; the window does the same job through History & restore |
| `pl export-and-prune` | two irreversible effects in one command; the window keeps them apart |
| `pl archive-move`, `pl archive-delete` | they move or delete the whole archive — a system-level decision about disks, not a button |
| `pl daemon install` | installing a background service (`launchd`/`systemd`) is a change to the system; the menu starts and stops the app's own process instead |
| `pl-mcp` (the server) | a read-only server for other agents, started by the agent that needs it; the menu lists what it registers |
| `pl watch --for` | a live terminal stream; the window has the log and the trigger state |
| `pl partial-pass` | the pass the daemon runs by itself; on the terminal it exists to be measured |
| `pl check --fix`, `pl audit-archive --update`, `pl prune --before` | the sharp forms: each deletes or freezes something, and each asks a question a window cannot honestly ask. The menu runs the read-only forms |
| `pl completion --install`, `pl prompt` | shell furniture: the menu shows the command rather than running it |

