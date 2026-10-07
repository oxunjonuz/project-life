# Which build am I running?

On 2026-10-06 the owner ran a bundle that had been replaced hours earlier, and nothing on screen
said so: the three failures he reported were three that had already been fixed, and working that out
took a round. These are the three ways to answer the question, in the order they cost the least.

## 1. The window says it

The sidebar footer, under the protection pill, shows one line:

```
build app 0.9.5 · ui <the bytes in your bundle> · core <likewise>
```

Hover it to see the command that checks it. The same line is in the menu-bar item's menu, and in the
About panel (`Project Life` → **About Project Life**), which also prints the command. The About panel
and the About screen also carry the author and his address — and where those come from is
`docs/BRAND.md`.

* `app` — the version of the interface server compiled into the bundle;
* `ui` — the first eight hex digits of the sha256 of `Contents/Resources/projectlife-ui`;
* `core` — the first eight hex digits of the sha256 of `Contents/Resources/projectlife`.

## 2. The Finder says it

`Project Life.app` → **Get Info** shows the version from `Info.plist`. Round 295 shipped `0.1.0`,
round 297 `0.9.0`, round 298 `0.9.1`, round 299 `0.9.2`, round 300 `0.9.3`, round 301 `0.9.4`,
round 302 `0.9.5`. A bundle that says `0.1.0` is the one the owner's report of 2026-10-06
described. (Until round 302 that version was typed into `Info.plist` by hand and had already drifted
one release behind the VERSION file; it is filled at build time now, and
`tools/version_check.py` fails if any of the five places disagrees.)

## 3. The terminal says it, from the bytes

```sh
cd "/path/to/Project Life.app/Contents/Resources"
shasum -a 256 projectlife projectlife-ui
```

These two numbers must be what the window's build line says (in full; the line shows the first eight
digits). If they differ from what the window shows, the window is not the bundle you are looking at,
or one of the binaries was replaced after the app started — the app re-hashes on demand, but the
comparison is yours to make.

The same check is automated here: `tools/build_identity_check.py` asks the running server and hashes
the files with Python's `hashlib`, i.e. with a different implementation than the Rust that produced
the numbers. `tools/ui_e2e.py` reads the line off the rendered page in a real browser and compares it
with the server's own answer.

## Round 299 — the menu says which build it is too

The build line is in one more place now: the window's own menu bar and the About entry
(`Project Life → About Project Life…`), which also prints the core's version and the command that
checks the hashes. And the menu's entries are the server's own list, so a window whose menu shows an
entry the server does not know is a window from another bundle — a difference a person can see.

## Why not just a version number?

A version number is a claim about a bundle; a hash is the bundle. Two copies of `Project Life.app`
in two folders look identical in the Finder until one of them is opened and read — so the app prints
the hashes of the files it is actually running, and prints the command that lets anyone check them
without trusting it.
