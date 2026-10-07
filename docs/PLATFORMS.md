# The three applications, and what has actually been run on each platform

Project Life — a local flight recorder for project files.
It is made for the moment an agent deletes or breaks something: it keeps everything — every version
of every file you protect stays on your own disk and can be restored at any moment.
Author: Oxunjon Ubaydllayev ⟨oxunjonub@gmail.com⟩ · MIT licence.

One core (`projectlife`), one interface server (`projectlife-ui`), three shells. The shells hold no
storage logic at all: each starts the server that ships beside it on `127.0.0.1`, shows the page the
server serves, and builds its menu from the server's own `GET /api/menu` answer. Adding a platform
means writing a shell and a build script — never a second copy of the mechanisms.

| | macOS | Linux | Windows |
|---|---|---|---|
| shell | `app/macos/ProjectLife.m` (Objective-C, AppKit + WebKit) | `app/linux/ProjectLife.c` (GTK 3 + WebKitGTK 4.1) | `app/windows/ProjectLife.c` (Win32 + WebView2) |
| build | `sh app/macos/build_app.sh` (cross-compiled, SDK sha256-pinned) | `sh app/linux/build_linux_app.sh` | `sh app/windows/build_windows_app.sh` (mingw-w64; WebView2 SDK sha256-pinned) |
| package | `Project Life.app` (arm64, ad-hoc signed) | portable `tar.gz` + `.deb` (`tools/make_linux_packages.sh`) | portable `zip` + `install.ps1` / `uninstall.ps1` |
| run here? | **no** — no Mac | **yes** — the real window under Xvfb | **no** — no Windows |
| checks here | bytes: Mach-O arm64, signature, imports, embedded UI (`tools/verify_macos_app.py`, 41 checks) | the whole scenario in the running app (`tools/linux_shell_check.py`, 22 checks) | bytes: PE shape, imports, resources, strings, checksums, zip (`tools/windows_package_check.py`, 30 checks) + the same UI bytes inside `pl-ui.exe` |
| checks on your machine | open it; `README_MACOS.md` | `sh tools/linux_shell_check.py` | `powershell -ExecutionPolicy Bypass -File verify_windows.ps1` |

**The single most important sentence in this file:** the macOS and Windows packages were *built and
read*, not *run*. Nothing in either of them has been executed — there is no Mac and no Windows in the
build environment. The Linux application was run, end to end, against a real archive. Every document
in this delivery keeps that distinction; `FAILURES_301.md` records what it cost to learn it.

## What each shell promises, and how each one keeps it

1. **The window is the real interface.** All three load the same page bytes that are inside their own
   interface server (`tools/embedded_ui_check.py` proves the three servers carry identical
   `index.html`, `app.css`, `app.js`; `verify.sh` compares the macOS, Linux and Windows binaries with
   each other).
2. **Closing the window does not stop the protection.** The window hides, the tray icon stays, the
   observation keeps running. The first close says so: a tray balloon on Windows, a dialog on Linux,
   a dialog on macOS.
3. **Quitting is explicit and names its consequences.** "Quit completely" stops the observation *this
   app started*, says what stays in the archive, and closes everything. On Linux and Windows the same
   action is also reachable by writing `quit.request` into the app folder, which is how the automated
   checks quit the app the way a person would.
4. **No control is decoration.** Every menu entry comes from one table in the core (`app/src/menu.rs`),
   names the core command it runs, and is disabled *with the server's reason* when it cannot run.

## Platform differences the code actually handles

* **Notification backend.** Linux uses inotify (per directory). Windows uses
  `FindFirstChangeNotificationW` — one recursive handle per project root, because
  `WaitForMultipleObjects` takes at most 64 handles and a change notification cannot name the file
  that changed. macOS has **no** backend in this build: it runs on the periodic pass alone, which is
  the same promise honored more slowly (`src/watch.rs`, and limitation 92).
* **Stopping a background process.** macOS and Linux send `SIGTERM`. Windows has no signal to send to
  a detached process, so `projectlife daemon stop` writes a one-line *request file* into the archive
  that names the daemon's pid; the daemon reads it each cycle and leaves between passes.
  `TerminateProcess` exists as a last resort and says so when it is used. A request that names another
  process is ignored and the stale file is removed at startup — otherwise a request left behind by a
  dead daemon would stop the next one.
* **Configuration without a restart.** `SIGHUP` on macOS/Linux; on every platform the daemon also
  re-reads `config.json` when its bytes change, so Windows gets the same behaviour without signals.
* **Paths.** Every relative path inside the archive is normalized to forward slashes
  (`scan::normalize_rel`), so an archive written on Windows is readable on Linux and macOS. Tested:
  `notification_paths_written_with_backslashes_address_the_same_file`.
* **App folders.** `~/Library/Application Support/ProjectLife` (macOS),
  `$XDG_STATE_HOME/projectlife-app` (Linux), `%LOCALAPPDATA%\ProjectLife` (Windows),
  `PROJECTLIFE_APP_HOME` overriding all three.
* **Pids and locks.** `pid_alive` is `kill(pid, 0)` on unix and
  `OpenProcess` + `GetExitCodeProcess` on Windows. After a crash the lock is taken over; the
  *liveness* of the holder is what decides, on both platforms.
* **Names that differ only in case.** Windows cannot hold `ProjectLife.exe` and `projectlife.exe` in
  one folder: one would overwrite the other, silently. The Windows package therefore ships
  `ProjectLife.exe` (shell), `pl.exe` (core), `pl-ui.exe` (server), `pl-mcp.exe`. The build script
  refuses to produce a package with colliding names, `verify.sh` checks the delivered folder for them,
  and this is not theoretical: the volume this delivery was built on is case-insensitive — writing
  `casetest.txt` overwrote `CaseTest.txt` — and it silently destroyed the shell binary once
  (`FAILURES_301.md`, F301-3).

## Building on your machine

```sh
# Linux (what the packaged binaries were built with)
sudo apt install build-essential libgtk-3-dev libwebkit2gtk-4.1-dev libayatana-appindicator3-dev \
                 libsoup-3.0-dev libjson-glib-dev
cargo build --release && ( cd app && cargo build --release )
sh app/linux/build_linux_app.sh

# Windows, from Linux (what the .zip was built with)
sudo apt install gcc-mingw-w64-x86-64 binutils-mingw-w64-x86-64
rustup target add x86_64-pc-windows-gnu
sh app/windows/build_windows_app.sh          # downloads the WebView2 SDK, sha256-pinned

# Windows, natively (no cross-compiling, no mingw)
#   requires Rust (MSVC toolchain) + the WebView2 SDK from NuGet (the same pinned version)
#   build the core and app with `cargo build --release --target x86_64-pc-windows-msvc`
#   and compile app/windows/ProjectLife.c with MSVC against build/native/include/WebView2.h
#   (the only source change needed is nothing: the file is C, and the SDK's header is C-compatible)

# macOS, from Linux (cross-compiled against the real SDK, sha256-pinned)
sh app/macos/build_app.sh
# or on a Mac: the commands in app/macos/README_MACOS.md
```

## Checking it on your machine

```sh
# Linux: the real window, a real archive, and the whole scenario (needs Xvfb for a headless run)
sh tools/linux_shell_check.py

# Windows: run inside the package folder
powershell -ExecutionPolicy Bypass -File verify_windows.ps1
#   verifies the delivered checksums, runs the window's own --selftest, then drives observation
#   through the app's own routes on a scratch archive: a change becomes a version, a restore is
#   compared byte by byte, an export/import round trip, and "quit completely" really stops it.

# macOS: open "Project Life.app" and watch the footer line `build app … · ui … · core …`
```

The Windows selftest writes `<app folder>\shell-selftest.txt` as well as printing to the console it
can attach, because a GUI process has no stdout of its own. That file is what `verify_windows.ps1`
reads.

## Not done, and why

* **No FSEvents backend on macOS.** The core's watcher falls back to off + periodic passes there, as
  it did in 0.9.3. Adding it means another platform's watch machinery beside the Windows one; it is
  a round of its own, and this round's job was not to change what the Mac has.
* **No tray icon without a StatusNotifier host (Linux).** `libayatana-appindicator3` needs a host
  (KDE, GNOME with an extension, XFCE with one, and so on). Where there is none the app says so on
  the first close and the window's own menu carries the same entries.
* **No code signing anywhere.** No Apple certificate, no Authenticode. The macOS binaries carry the
  linker's ad-hoc signature (without it arm64 does not start at all); the Windows executable says in
  its own version block that it is unsigned. Instructions for running them anyway are in each
  README, and neither requires disabling a protection.
* **`pl://reveal` on Linux does not open a file manager.** Clicking a path shows the path in the
  window instead of launching `xdg-open`: opening an external program is a decision for the person,
  not a side effect of a click.
