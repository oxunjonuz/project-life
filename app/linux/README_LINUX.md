# The Linux application

It is made for the moment an agent deletes or breaks something: it keeps everything — every version of every file you protect stays on your own disk and can be restored at any moment.

Author: Oxunjon Ubaydllayev <oxunjonub@gmail.com> · MIT licence · Copyright (c) 2026 Oxunjon Ubaydllayev and Aiodam

`projectlife-app` is the Linux shell: a GTK 3 window with WebKitGTK 4.1, a tray icon
(libayatana-appindicator3), and the server's menu on both fronts. It stores nothing. It starts
`projectlife-ui` beside it, shows the page that server serves, and performs every action by calling
that server's routes — the same routes the page uses.

```
sh app/linux/build_linux_app.sh          # builds the core, the server, the shell; assembles a portable tree
sh tools/make_linux_packages.sh          # turns that tree into a .tar.gz and a .deb
python3 tools/linux_shell_check.py       # drives the real window under Xvfb, end to end
```

## What was verified here, by running it

`tools/linux_shell_check.py` runs the shipped binary under `Xvfb` (a container has no display) against
a scratch archive, and every check below passed — 22 of them:

* the page loads in the real WebKit and renders its own UI (`sidebar=yes`, 750 characters of text in
  the window; a PNG written through WebKit's **own snapshot API**, 69 KB, and read back with an
  independent pixel tool);
* the menu is the server's document, rendered twice: 8 groups, 8 menu-bar items, 17 tray items;
* closing the window hides it and the server keeps answering (the same signal a window manager sends,
  emitted in-process — there is no window manager under Xvfb, and that is stated, not hidden);
* observation started through the app's own route, and the app then reports real protection
  (`protected`);
* **an edit made from outside** (a plain file write, as an agent or an editor would do it) becomes a
  new version in the archive — measured afterwards by the *core*, in another process — and costs
  exactly one new blob, not a copy of the project;
* "quit completely" stops the observation: the shell exits 0, the daemon is gone, the daemon lock is
  released, and no process of that run is left behind;
* a second launch does not start a second window (it asks the first one to come forward), a handshake
  file naming a dead process is ignored, and a handshake file naming a live one is respected.

The `.deb` was extracted with `dpkg-deb -x` and the shell **from the extracted package** was run
(`--selftest` → `RESULT=PASS`), so the packaged layout is verified too, not just the build tree.

## What was not verified here

* The tray icon's *visibility*: a StatusNotifier host (KDE, GNOME with an extension, …) is what shows
  it, and a headless X server has none. The app detects the absence, says so on the first close, and
  the window's own menu carries the same entries.
* `pl://reveal`: the Linux shell shows the path in the window instead of launching a file manager
  (opening a program is a decision for the person, not a side effect of a click).
* Signing: there is none, on any platform, and nothing here asks for a system protection to be turned
  off.

## Files

| file | what it is |
|---|---|
| `ProjectLife.c` | the shell: window, tray, menu, `pl://` bridge, control channel, `--selftest` |
| `build_linux_app.sh` | the build: cargo for the core and the server, `cc` for the shell, then the tree |
| `../../tools/linux_shell_check.py` | the end-to-end check that runs the shell and reads the archive afterwards |
| `../../tools/make_linux_packages.sh` | `.tar.gz` + `.deb`, whose `Depends:` is read from `ldd` on the shipped binary |
