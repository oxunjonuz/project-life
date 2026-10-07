#!/usr/bin/env python3
"""One delivery, one version — checked, because the version is written in five files by hand.

Round 302, found while doing something else: the delivery announced itself as 0.9.4 (VERSION file,
the three shells' `SHELL_VERSION`) while the Rust crates still said 0.9.3 and the macOS `Info.plist`
had been typed as 0.9.3 a round earlier. Three answers to a question with one answer.

Where the version is written, and how each one is read:

| place | how |
|---|---|
| `VERSION` | the file the build scripts read |
| `Cargo.toml`, `app/Cargo.toml` | the crate version — compiled into `pl version` |
| `app/macos/ProjectLife.m`, `app/linux/ProjectLife.c`, `app/windows/ProjectLife.c` | `SHELL_VERSION` |
| `app/macos/Info.plist` | the template's `@VERSION@` token, filled from `VERSION` at build time |

Built artefacts, if they are on disk, must carry the same string.

Usage:

    python3 tools/version_check.py [--root DIR] [--expect X.Y.Z] [--quiet]

`--expect` is the control: it pretends the VERSION file says something else, and the run must then
go red. Exit codes: 0 one version everywhere, 1 a disagreement (printed), 2 usage.
"""

import argparse
import re
import sys
from pathlib import Path

SHELL_VERSION = re.compile(r'#define\s+SHELL_VERSION\s+(?:L)?"([0-9][0-9A-Za-z.\-]*)"')
CARGO_VERSION = re.compile(r'^version\s*=\s*"([0-9][0-9A-Za-z.\-]*)"', re.M)


def read(path):
    try:
        return path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return None


def main(argv):
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", default=str(Path(__file__).resolve().parent.parent))
    ap.add_argument("--expect", default=None, help="control: pretend VERSION says this")
    ap.add_argument("--quiet", action="store_true")
    args = ap.parse_args(argv[1:])

    root = Path(args.root).resolve()
    rows = []          # (name, value or None if the file is absent, path)

    version = args.expect or (read(root / "VERSION") or "").strip()
    rows.append(("VERSION", version, root / "VERSION"))
    if not version:
        print("version_check: the VERSION file is empty or missing", file=sys.stderr)
        return 1

    for name, path in [("Cargo.toml", root / "Cargo.toml"), ("app/Cargo.toml", root / "app" / "Cargo.toml")]:
        text = read(path)
        if text is None:
            rows.append((name, None, path))
            continue
        m = CARGO_VERSION.search(text)
        rows.append((name, m.group(1) if m else "?", path))

    for name in ["app/linux/ProjectLife.c", "app/windows/ProjectLife.c"]:
        path = root / name
        text = read(path)
        if text is None:
            rows.append((name, None, path))
            continue
        m = SHELL_VERSION.search(text)
        rows.append((name + " (SHELL_VERSION)", m.group(1) if m else "?", path))

    # The macOS shell has no version constant of its own: it reads both versions from the bundle's
    # Info.plist, which is filled from VERSION at build time. So what is checked is the token — in the
    # key that matters, not merely somewhere in the file (the file mentions it twice, and a check that
    # only asked "is @VERSION@ present anywhere" passed while the version string itself had been
    # typed as a literal).
    plist = root / "app" / "macos" / "Info.plist"
    ptext = read(plist)
    if ptext is not None:
        m = re.search(r"<key>CFBundleShortVersionString</key>\s*<string>([^<]*)</string>", ptext)
        got = m.group(1) if m else "?"
        rows.append(("app/macos/Info.plist CFBundleShortVersionString",
                     version if got == "@VERSION@" else f"a literal ({got})", plist))

    # Built artefacts, if this tree has them: the version must be inside the bytes that ship.
    binaries = [
        ("the core binary", root / "target" / "release" / "projectlife", version),
        ("the interface server", root / "app" / "target" / "release" / "projectlife-ui", version),
    ]
    for label, path, want in binaries:
        if path.is_file():
            rows.append((label, want if want.encode() in path.read_bytes() else "not in the bytes", path))

    bad = [(n, v, p) for n, v, p in rows if v != version]
    if not args.quiet:
        for n, v, p in rows:
            mark = "PASS" if v == version else "FAIL"
            print(f"  {mark}  {n}: {v}")
        print(f"\n  the version in VERSION is {version}")
        for n, v, p in bad:
            print(f"  FAILED: {n} says {v!r} — {p}")
        print(f"  {len(rows) - len(bad)} of {len(rows)} place(s) agree")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
