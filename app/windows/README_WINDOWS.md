# The Windows application

It is made for the moment an agent deletes or breaks something: it keeps everything — every version of every file you protect stays on your own disk and can be restored at any moment.

Author: Oxunjon Ubaydllayev <oxunjonub@gmail.com> · MIT licence · Copyright (c) 2026 Oxunjon Ubaydllayev and Aiodam

`ProjectLife.exe` is the Windows shell: a Win32 window with **WebView2**, a tray icon, and the
server's menu on both fronts. It stores nothing. It starts `pl-ui.exe` beside it, shows the page that
server serves, and performs every action by calling that server's routes — the same routes the page
uses.

```
sh app/windows/build_windows_app.sh       # cross-compiled here with mingw-w64; WebView2 SDK sha256-pinned
powershell -ExecutionPolicy Bypass -File verify_windows.ps1   # the checks that need Windows
python3 tools/windows_package_check.py    # what can be checked here, without Windows
```

## Read this first: nothing here has been run

There is no Windows machine in the environment this was built in. The package was **compiled, linked,
packaged and read** — not executed. Every claim below is about bytes:

* `tools/windows_package_check.py` (30 checks, all passing) reads the delivered files: the shell is a
  Windows GUI PE importing `WebView2Loader.dll`, `WINHTTP.dll`, `SHELL32.dll` and `ADVAPI32.dll`; the
  words it promises (`Quit completely`, `Start with Windows`, `selftest:`,
  `menu?shell=1&plain=1`, …) are in the binary; the resource section holds the icon and the version
  block, and the version block says in words that the file is **unsigned**;
  `SHA256SUMS.txt` matches the files it names; the zip is byte-identical to the folder; and no two
  names in the package differ only in case.
* `tools/embedded_ui_check.py` proves the same `index.html`, `app.css` and `app.js` (same sha256) are
  inside the macOS, Linux and Windows interface servers — the interface that was exercised in the
  Linux window is the interface in this package.
* The `--selftest` output *format* is the same one the Linux shell produces and that the Linux check
  parses, so the Windows verifier is not reading a different dialect.

`verify_windows.ps1` is what closes the gap: it verifies the delivered checksums, runs the shell's
`--selftest` (real WebView2, real page) and then drives the whole scenario on a scratch archive —
observation through the app's own routes, an external edit becoming a version, a restore compared
byte by byte, an export/import round trip, and "quit completely" really stopping the daemon.

**The PowerShell scripts have never been executed here, and were not even parsed by a PowerShell
interpreter** (there is none in this environment). What is checked is weaker and is stated as such:
they are present, their braces balance, they name executables that exist in the package, and the
verifier checks both the selftest result and the delivered checksums. A mistake in them is possible;
its failure mode is visible, because the verifier prints every step and counts them.

## What was deliberately solved rather than assumed

* **No signals.** A detached Windows process cannot be asked to stop with a signal, so
  `projectlife daemon stop` writes a one-line *request file* into the archive naming the daemon's pid;
  the daemon reads it on each cycle and leaves between passes. `TerminateProcess` is the last resort
  and the command says when it uses it. Tested here on Linux (`tests/round301.rs`), because it is the
  same code on both platforms.
* **No JSON parser in C.** The menu arrives in a line format (`GET /api/menu?plain=1`) that the shell
  splits on tabs, rather than a hand-written JSON parser that could not be tested on the machine it
  runs on. The two formats are compared field by field in the app's own tests.
* **Names that cannot collide.** Windows is case-insensitive, so the package ships `ProjectLife.exe`
  with `pl.exe`, `pl-ui.exe`, `pl-mcp.exe`. This was not theoretical: the first build lost the shell
  binary to a file whose name differed only in case (`FAILURES_301.md`, F301-3).
* **The WebView2 runtime is a real dependency and is named as one.** It ships with Windows 10/11 and
  with Edge. If it is missing, the window says so in the system's own words (the HRESULT) and names
  the page to get it from — it does not show an empty frame.
* **Autostart is a decision, not a side effect.** The tray entry writes exactly one value in
  `HKEY_CURRENT_USER\...\Run`, says what it wrote, and `uninstall.ps1` removes it.

## Files

| file | what it is |
|---|---|
| `ProjectLife.c` | the shell: window, tray, menu, WebView2 callbacks, registry entry, `--selftest` |
| `ProjectLife.rc` | icon and version resource (a template: `@VERSION@`/`@VERNUM@` filled by the build) |
| `build_windows_app.sh` | the cross-build: mingw-w64, cargo for `x86_64-pc-windows-gnu`, the sha256-pinned WebView2 SDK, the zip |
| `install.ps1` / `uninstall.ps1` | per-user install into `%LOCALAPPDATA%\Programs`, with a Start Menu shortcut |
| `verify_windows.ps1` | the checks that need Windows (see above) |
