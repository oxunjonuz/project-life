# Project Life — the desktop app on macOS (Apple Silicon)

It is made for the moment an agent deletes or breaks something: it keeps everything — every version of every file you protect stays on your own disk and can be restored at any moment.

Author: Oxunjon Ubaydllayev <oxunjonub@gmail.com> · MIT licence · Copyright (c) 2026 Oxunjon Ubaydllayev and Aiodam

This folder builds **`Project Life.app`**, a real application bundle for arm64 Macs: three arm64
executables inside one window. Nothing else has to be installed to *run* it — not Rust, not Node.js,
not Xcode.

```
Project Life.app/Contents/
    MacOS/ProjectLife              the macOS shell: window, menu bar item, folder panel
    Resources/projectlife-ui       the interface server (the window talks to it on 127.0.0.1)
    Resources/projectlife          the core: observation, history, restore, export, import
    Resources/pl-mcp               the core's read-only MCP server (for agents)
    Resources/ProjectLife.icns     icon
    Info.plist
```

The window is rendered by the system WebKit view. It is not a browser and not a web page: there is no
server exposed to the network, only a loopback listener with a token that is generated at launch and
dies with the app. Everything the window does goes through the same `projectlife` program you can run
in a terminal — the app adds a face, not a second storage engine.

---

## 1. Open it

In the Finder, in this folder:

```
dist/Project Life.app
```

Double-click it, or in a terminal:

```sh
open "dist/Project Life.app"
```

**There is no Apple certificate behind this build.** It is not signed with a Developer ID and not
notarised, so:

* On first launch macOS may say the app is from an unidentified developer. Fix it *without* turning
  anything off: **right-click (or Control-click) the app → Open → Open**. That records your consent
  for this one app. Then it opens normally forever after.
* If the app was copied from another machine and carries the quarantine flag, either use the
  right-click → Open path, or clear just that flag for this one bundle:
  `xattr -dr com.apple.quarantine "dist/Project Life.app"`.
* **Do not disable Gatekeeper or SIP for this.** Neither is needed.

Optional, and only if you want the bundle sealed as a whole (which `codesign` does with a
certificate; here it happens ad-hoc, on your machine, with no key):

```sh
codesign --force --deep --sign - "dist/Project Life.app"
codesign --verify --verbose=2 "dist/Project Life.app"
```

Every executable already carries the linker's own ad-hoc signature, which is the shape Xcode's linker
produces and what the kernel checks before it starts an arm64 process.

## 2. First run, in seven steps

1. **Choose where the archive lives.** The first screen asks for a folder. Pick a folder on a
   *different* disk than your work if you have one; the app tells you when both are on the same disk
   and why that matters. It runs `projectlife init-archive` for you and remembers the location in
   `~/Library/Application Support/projectlife/location.json` — the same file the command-line program
   uses, so both see the same archive.
2. **Add a folder.** `Add a folder` → the native folder panel opens. Project Life never watches the
   whole computer; only folders you name are ever looked at.
3. **Choose the protected types and see what will be saved.** The next step shows what the folder
   looks like (extensions found, markers, folder hints), the preset it suggests, and — before
   anything is created — **how many files and how many bytes would be protected**, and what is left
   out (secrets, temporary files, dependencies, very large files). Untick or tick types to change the
   list. Nothing is written until you press *Start protecting*.
4. **Start protection.** The window shows the real state: whether observation is running, when it
   last looked, the interval, and — printed from the core's own words — which trigger is in use.
5. **Change a file with any editor or agent.** Versions appear in *History & restore*.
6. **Pick a moment, look at the tree, restore.** Choose a moment in the timeline (or type a date and
   time), tick files, choose a destination — the default is a *new* folder, never your working one —
   and restore. The app then hashes every restored file itself and compares it with the hash the
   archive recorded, and reports the result; that check is a second implementation, not the restore
   code grading its own work.
7. **Export and import.** *Export the history* writes a folder with a `MANIFEST.sha256`; the app
   re-hashes every file in that manifest. *Import* brings an exported folder back as its own project.

**Closing the window does not stop observation.** The first time you close it, the app says so
explicitly; the shield in the menu bar stays, with the state in its tooltip and a menu that can reopen
the window, show the protection status, stop or start observation, and quit. **Only quitting stops
observation** — and the app asks first, and says exactly what stops and what stays.

### One honest limitation of this build on macOS

Filesystem notifications (the trigger that makes a version appear within a second of a change) are
implemented here for Linux only. On macOS this build reports `unavailable` **in plain words in the
window and in the log**, and the daemon falls back to the periodic pass. So on your Mac:

* the accuracy of the promise equals the observation interval — 5 seconds by default;
* the interval is in *Settings*; saving a new value restarts observation, because the core reads its
  configuration when the daemon starts (re-reading it live is not implemented — `LIMITATIONS.md`,
  item 32).

Nothing is faked about this: the window prints the mode the core reports, not a wish.

### If the app says it cannot start (fixed in this build)

Earlier builds told you to reinstall the app when the interface server could not be found. That
sentence was wrong twice over, and both faults are fixed here:

* the lookup asked for the resource folder *inside* the resource folder — `Contents/Resources/Resources`,
  where nothing lives — so it always returned nil;
* its fallback handed the folder `Contents/MacOS` to the launcher instead of a program.

The bundle now looks in `Contents/Resources` (and then beside the main executable), refuses anything
that is not a plain executable file, and if it still cannot find them it prints **every path it
tried and what it found there** — a folder, a missing file, or a file without the execute bit. The
failure alert shows the interface server's own last words (its stderr is appended to `daemon.log`),
names the log file, and offers *Open the log*. There is no "reinstall" instruction anywhere in the
app: reinstalling the same bundle would find the same absence.

### Free space, and what the window says about it (new in this build)

* The stop threshold is **the smaller of 500 MB and 1 % of the volume**, as the specification says.
  On a large disk that means the fixed 500 MB governs — earlier builds stopped recording on a 926 GB
  volume with 1.8 GB free, which was a misreading of that sentence.
* While recording is stopped for lack of room, the window and the menu-bar shield say
  **"Recording stopped: not enough free space"** with the measured numbers, never "Protected". A
  live process is not a written archive.
* You are told **once** when it stops, then at most once every ten minutes, and once when writing
  resumes. The state lives in `<archive>/logs/space_state.json`, so a restart does not re-announce it.
* Nothing is ever deleted to make room, checking continues, and the first cycle after space returns
  records the changes made during the outage — the window shows the versions appearing.

### JavaScript dialogs (fixed in this build)

The web view has no dialog handler unless the app implements one, so `prompt` returned null and the
import stopped without a word. The window now draws its own dialog for the import name, and the shell
implements `WKUIDelegate` (alert, confirm, text input) so no future prompt can fail the same silent
way.

## 3. What was verified, and where

| Check | Where it ran | Result |
| --- | --- | --- |
| The seven steps through the window (store → add → types → limits → observation → external edit → versions → moment → tree → restore → export → import → stop → restart) | Linux, real Chromium against the same server and the same interface bytes as the app | 20/20 checks pass (`tools/ui_e2e.py`, `evidence/ui_e2e_295.txt`) |
| **The free-space transition through the window** (room → no room → repeated cycles → room again), the notification count and the "no browser dialog" rule | Linux, real Chromium, real daemon, real archive | 28/28 checks pass (`evidence/ui_e2e_296.txt`, step 10) |
| **The same transition at the command level**, with the volume measured independently by Python's `os.statvfs` and the threshold recomputed from the specification | Linux | 19/19 checks (`tools/space_transition_check.py`, `evidence/verify_296_full.txt` step 45) |
| Restored bytes compared with the archive's own hashes | by the test script, not by the app | 3/3 identical; the live file was not touched |
| Export re-hashed from its manifest | by the test script | 10/10 match |
| Core: `cargo test --release` | Linux | 116 tests, 0 failures (3 new in `tests/round296.rs`, 6 in `src/space.rs`) |
| App layer: `cargo test` in `app/` + `tools/verify_app.sh` | Linux | 41 unit tests, 28 end-to-end checks, 35 bundle checks, all green |
| Core: `tools/verify.sh` | Linux | 48 steps, 201 PASS, 0 FAIL, 1 SKIP (`evidence/verify_296_full.txt`) |
| Core: 71 deliberate faults, one at a time | Linux | 71/71 caught, 0 survivors, 0 drift (`evidence/mutations_296.txt`) |
| The `.app`: arm64 Mach-O, ad-hoc signature over every page, only system libraries | Linux, reading the built bundle as bytes | 35/35 checks (`tools/verify_macos_app.py`) |
| **Launching the app on macOS, the window, the menu bar item, the native folder panel, Finder double-click, the two dialogs** | **not executed — I have no Mac** | **unverified** |

The last row is the honest gap, and it is the row that matters most this round: **every fault fixed in
0.8.0 was found by you running the app on your Mac, not by anything in this table.** The app was built
and its bytes were checked; the parts that only macOS can run — AppKit, WebKit, the status item, the
folder panel, the JavaScript dialogs, launching through Finder — have never been executed by me. If
something there is wrong, the shell logs to
`~/Library/Application Support/ProjectLife/daemon.log`, and the same store can always be driven from a
terminal with the core inside the bundle:

```sh
"/path/to/Project Life.app/Contents/Resources/projectlife" status --json
"/path/to/Project Life.app/Contents/Resources/projectlife" scan-once --all
```

One more thing you can do without me: **the menu bar item has a *Network diagnosis…* command** that
runs the bundled interface server's own probe (sockets, binds, unix socket, signature) and writes the
result to `~/Library/Application Support/ProjectLife/diagnose.txt`. If the window ever refuses to
open again, that file and `daemon.log` are the two things worth sending.

## 4. Rebuilding it (only if you want to)

Two ways. **A. With the tools already on this machine (Linux/CI):**

```sh
sh app/macos/build_app.sh          # writes ./dist/Project Life.app
```

It downloads a macOS SDK (15.5, sha256-checked) if one is not present, needs `clang`, `lld` and
`rustup` with the `aarch64-apple-darwin` target, and produces the bundle. Set `SDK_ROOT` to use an
SDK you already have.

**B. On the Mac itself**, with Xcode command-line tools and Rust:

```sh
# once
xcode-select --install
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup target add aarch64-apple-darwin

# build (native, no cross-compiling)
cargo build --release
cargo build --release --manifest-path app/Cargo.toml

# assemble the bundle around those two binaries
mkdir -p "dist/Project Life.app/Contents/MacOS" "dist/Project Life.app/Contents/Resources"
# Info.plist is a template: fill it (the @VERSION@ and @AUTHOR@ placeholders must not ship as text)
python3 tools/fill_template.py app/macos/Info.plist "dist/Project Life.app/Contents/Info.plist" \
        --brand VERSION="$(cat VERSION)"
cp app/macos/ProjectLife.icns "dist/Project Life.app/Contents/Resources/"
cp target/release/projectlife target/release/pl-mcp "dist/Project Life.app/Contents/Resources/"
cp app/target/release/projectlife-ui "dist/Project Life.app/Contents/Resources/"
# -I app: the shell includes app/pl_brand.h, the generated header that carries the author's name,
# his address and the sentence about the purpose (tools/brand.py produces it from src/brand.rs).
clang -fobjc-arc -O2 -I app -framework Cocoa -framework WebKit -framework UniformTypeIdentifiers \
      app/macos/ProjectLife.m -o "dist/Project Life.app/Contents/MacOS/ProjectLife"
codesign --force --deep --sign - "dist/Project Life.app"
open "dist/Project Life.app"
```

## 5. Menu bar, windows and quitting, in one place

| Where | What it does |
| --- | --- |
| Menu bar shield | Tick = observing, slashed = not. Tooltip names the state |
| Menu bar menu | State and last check, *Open window*, *Start/Stop observation*, *Quit completely…* |
| App menu | About, Open window, Protection status…, Stop/Start observation, **Quit Project Life completely…** |
| Closing the window | Hides it. Observation continues. The first close says so. |
| Quitting | Asks first, then stops observation and exits. Versions already saved stay in the archive and can be restored after the app starts again. |
| Force-quitting the app | The observation process it started is left behind; the next launch shows “Observation is running, started outside this app” and lets you stop it. Nothing is silently half-stopped. |
