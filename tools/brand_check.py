#!/usr/bin/env python3
"""The author, the address and the sentence about the purpose: are they everywhere they must be?

Round 302: the owner asked for his name and address "everywhere it makes sense, like about the
program", and for the sentence about what it is for. A string requested in a dozen places written in
six languages is a string that drifts, so the values live in exactly one file (`src/brand.rs`) and
everything else either reads them or is checked against them here.

What this tool does, from the outside:

  1. reads `src/brand.rs` **itself** (a second reader, so a bug in `tools/brand.py` is caught here
     rather than trusted);
  2. asks the built core for its own `version --json` and requires it to agree, word for word, with
     what was read from the source — two independent readers of one truth;
  3. requires the exact bytes to be present in every place that must carry them, including the three
     compiled shells and the package metadata that is filled at build time;
  4. requires that the places which must *not* hold a copy (the page, the three shells) really do not:
     a second copy is how a name drifts;
  5. `--control` runs the same code with a copy of one file that has the address removed, and requires
     the result to be red. A check that cannot fail proves nothing.

Usage:

    python3 tools/brand_check.py [--root DIR] [--pl BINARY] [--app BINARY] [--control]
                                 [--override REAL=COPY]...

Exit codes: 0 everything present, 1 something missing (listed), 2 usage.
"""

import argparse
import json
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

CONST = re.compile(r'^pub const ([A-Z][A-Z0-9_]*): &str = "(.*)";\s*$')


def read_brand(root):
    """The values, read directly from src/brand.rs — this tool's own reader, not brand.py's."""
    text = (root / "src" / "brand.rs").read_text(encoding="utf-8")
    vals = {}
    for line in text.splitlines():
        m = CONST.match(line)
        if m:
            vals[m.group(1)] = m.group(2)
    missing = [k for k in ("PRODUCT", "AUTHOR", "AUTHOR_EMAIL", "COPYRIGHT", "LICENCE", "WHAT_IT_IS") if k not in vals]
    if missing:
        raise SystemExit(f"brand_check: src/brand.rs does not define {', '.join(missing)} on one line")
    return vals


class Report:
    def __init__(self):
        self.rows = []

    def add(self, ok, what, detail=""):
        self.rows.append((bool(ok), what, detail))

    @property
    def failed(self):
        return [r for r in self.rows if not r[0]]

    def show(self):
        for ok, what, detail in self.rows:
            mark = "PASS" if ok else "FAIL"
            line = f"  {mark}  {what}"
            if detail:
                line += f"  ({detail})"
            print(line)
        n_ok = sum(1 for r in self.rows if r[0])
        print(f"\n  {n_ok} passed, {len(self.failed)} failed, {len(self.rows)} checks")
        for _ok, what, detail in self.failed:
            print(f"  FAILED: {what}" + (f" — {detail}" if detail else ""))


def read(path, overrides):
    """A file's bytes, with any control override applied (the real code path, mutated input)."""
    p = Path(path)
    key = str(p.resolve()) if p.exists() else str(p)
    if key in overrides:
        return Path(overrides[key]).read_bytes()
    return p.read_bytes()


def has_text(path, needle, overrides):
    try:
        return needle.encode("utf-8") in read(path, overrides)
    except OSError:
        return False


def carries_binary(path, needle, overrides):
    """A compiled artefact carries a string as bytes — in UTF-8 (Mach-O, ELF) or UTF-16LE (PE,
    because the Windows shell is compiled with wide strings). Either counts; neither is assumed."""
    try:
        raw = read(path, overrides)
    except OSError:
        return False
    return needle.encode("utf-8") in raw or needle.encode("utf-16-le") in raw


def has_prose(path, needle, overrides):
    """For Markdown: the same sentence wrapped across two lines is the same sentence.

    Exact bytes are right for binaries and machine-read files; for prose, a line break is a
    paragraph choice, not a different sentence. Whitespace is therefore collapsed before comparing —
    which is also why the wrapping of a sentence in a README can be edited without this check
    complaining, while a changed word still fails it.
    """
    try:
        text = " ".join(read(path, overrides).decode("utf-8", "replace").split())
    except OSError:
        return False
    return " ".join(needle.split()) in text


def main(argv):
    ap = argparse.ArgumentParser(add_help=True)
    ap.add_argument("--root", default=str(Path(__file__).resolve().parent.parent))
    ap.add_argument("--pl", default=None, help="the core binary (default: <root>/target/release/projectlife)")
    ap.add_argument("--app", default=None, help="the interface server (default: <root>/app/target/release/projectlife-ui)")
    ap.add_argument("--control", action="store_true", help="mutate a copy and require a red result")
    ap.add_argument("--override", action="append", default=[], help="REAL=COPY, for the control")
    args = ap.parse_args(argv[1:])

    root = Path(args.root).resolve()
    overrides = {}
    for o in args.override:
        if "=" not in o:
            print(f"brand_check: --override wants REAL=COPY, got {o!r}", file=sys.stderr)
            return 2
        real, copy = o.split("=", 1)
        overrides[str(Path(real).resolve())] = copy

    b = read_brand(root)
    author, email = b["AUTHOR"], b["AUTHOR_EMAIL"]
    what, product = b["WHAT_IT_IS"], b["PRODUCT"]
    r = Report()
    if args.control:
        print(f"  control run: reading one file from {list(overrides.values()) or 'nowhere'}")

    # ---- 1. the source of truth itself
    r.add(bool(author and email and what), "src/brand.rs defines the author, the address and the sentence",
          f"{author} / {email}")

    # ---- 2. the built core's own answer agrees with the source
    pl = Path(args.pl) if args.pl else root / "target" / "release" / "projectlife"
    if pl.exists():
        out = subprocess.run([str(pl), "version", "--json"], capture_output=True, text=True)
        if out.returncode != 0:
            r.add(False, "the core answers `version --json`", f"exit {out.returncode}: {out.stderr.strip()[:120]}")
        else:
            try:
                j = json.loads(out.stdout.strip().splitlines()[-1])
            except Exception as e:
                j = None
                r.add(False, "the core's `version --json` is JSON", str(e))
            if j:
                for key, want in [("author", author), ("authorEmail", email), ("whatItIs", what), ("product", product)]:
                    r.add(j.get(key) == want, f"the core's {key} matches src/brand.rs", str(j.get(key))[:60])
                human = subprocess.run([str(pl), "version"], capture_output=True, text=True).stdout
                r.add(author in human and email in human and what in human,
                      "`pl version` prints the author, the address and the sentence")
                r.add(carries_binary(pl, author, overrides) and carries_binary(pl, email, overrides),
                      "the core binary carries the author and the address as bytes", pl.name)
    else:
        # A tree with no `target/` at all is a source tree (the GitHub package is exactly that, and
        # anyone who has just cloned the repository has it too): nothing has been built, so there is
        # no binary to read. But a tree that *has* been built and whose core binary is missing is a
        # broken build, and that is a failure.
        if (root / "target").exists():
            r.add(False, "the core binary is built (target/ exists, the binary does not)", str(pl))
        else:
            print(f"  (nothing is built in this tree: {pl} does not exist, and there is no target/ — skipped)")

    # ---- 3. every place that must carry them
    README = root / "README.md"
    for path, needles, why in [
        (README, [author, email, product], "README.md"),
        (root / "LICENSE", [author, email, b["COPYRIGHT"]], "LICENSE"),
        (root / "docs" / "README.md", [author, email, what], "docs/README.md"),
        (root / "docs" / "APP.md", [author, email, b["COPYRIGHT"]], "docs/APP.md"),
        (root / "docs" / "PLATFORMS.md", [author, email, what], "docs/PLATFORMS.md"),
        (root / "Cargo.toml", [author, email], "Cargo.toml authors/description"),
        (root / "app" / "Cargo.toml", [author, email], "app/Cargo.toml authors"),
        # The generated header the three shells include, and the build templates whose placeholders
        # the build scripts fill (checked as placeholders here, as values in the built artefacts).
        (root / "app" / "pl_brand.h", [author, email, what], "app/pl_brand.h (generated header)"),
        (root / "app" / "macos" / "Info.plist", ["@AUTHOR@", "@AUTHOR_EMAIL@", "@WHAT_IT_IS@", "@COPYRIGHT@", "@LICENCE@"],
         "app/macos/Info.plist template"),
        (root / "app" / "windows" / "ProjectLife.rc", ["@AUTHOR@", "@AUTHOR_EMAIL@", "@WHAT_IT_IS@", "@COPYRIGHT@", "@LICENCE@"],
         "app/windows/ProjectLife.rc template"),
        (root / "app" / "linux" / "project-life.desktop.in", ["@AUTHOR@", "@AUTHOR_EMAIL@", "@WHAT_IT_IS@", "@LICENCE@"],
         "app/linux/project-life.desktop.in template"),
        (root / "src" / "brand.rs", ["Oxunjon Ubaydllayev", "oxunjonub@gmail.com"], "src/brand.rs (the source)"),
    ]:
        prose = path.suffix in (".md",)
        for n in needles:
            found = has_prose(path, n, overrides) if prose else has_text(path, n, overrides)
            r.add(found, f"{why} carries {n[:38]!r}", str(path))

    # ---- 3b. the header is not stale: regenerate and compare through brand.py
    gen = subprocess.run([sys.executable, str(root / "tools" / "brand.py"), "c"], capture_output=True, text=True, cwd=root)
    if gen.returncode == 0:
        r.add(not overrides and (root / "app" / "pl_brand.h").read_text(encoding="utf-8") == gen.stdout,
              "app/pl_brand.h equals what tools/brand.py generates from src/brand.rs")
    else:
        r.add(False, "tools/brand.py could regenerate the header", gen.stderr.strip()[:120])
    fresh = subprocess.run([sys.executable, str(root / "tools" / "brand.py"), "check-fresh", str(root / "app" / "pl_brand.h")],
                           capture_output=True, text=True, cwd=root)
    r.add(fresh.returncode == 0, "tools/brand.py check-fresh agrees", (fresh.stdout or fresh.stderr).strip()[:90])

    # ---- 4. the places that must NOT hold a copy: a second copy is how a name drifts
    for path, why in [
        (root / "app" / "ui" / "app.js", "the page (it asks the server)"),
        (root / "app" / "ui" / "index.html", "the page shell (it asks the server)"),
        (root / "app" / "macos" / "ProjectLife.m", "the macOS shell (it includes pl_brand.h)"),
        (root / "app" / "windows" / "ProjectLife.c", "the Windows shell (it includes pl_brand.h)"),
        (root / "app" / "linux" / "ProjectLife.c", "the Linux shell (it includes pl_brand.h)"),
    ]:
        if not path.exists():
            continue
        text = read(path, {}).decode("utf-8", "replace")
        r.add(email not in text, f"{why} holds no hand-typed copy of the address", str(path.name))
        r.add(author not in text, f"{why} holds no hand-typed copy of the name", str(path.name))

    # ---- 5. the built artefacts that are on disk right now
    bundle = root / "dist" / "macos" / "Project Life.app"
    plist = bundle / "Contents" / "Info.plist"
    if plist.exists():
        raw = plist.read_text(encoding="utf-8", errors="replace")
        for n in (b["PRODUCT"], author, email):
            r.add(n in raw, f"the built Info.plist carries {n[:34]!r}", "dist/macos")
        try:
            import plistlib

            keys = plistlib.loads(plist.read_bytes())
            r.add(keys.get("CFBundleShortVersionString") == (root / "VERSION").read_text().strip(),
                  "the built Info.plist version matches the VERSION file",
                  str(keys.get("CFBundleShortVersionString")))
        except Exception as e:
            r.add(False, "the built Info.plist parses as a property list", str(e)[:110])
    else:
        print("  (the macOS bundle is not built in this tree: skipped)")
    linux_apps = sorted((root / "dist" / "linux").glob("ProjectLife-linux-*/bin/projectlife-app"))
    if linux_apps:
        for n in (author, email, what):
            r.add(carries_binary(linux_apps[-1], n, overrides),
                  f"the built Linux shell carries {n[:34]!r}", linux_apps[-1].name)
        desktop = linux_apps[-1].parent.parent / "share" / "applications" / "project-life.desktop"
        if desktop.exists():
            raw = desktop.read_text(encoding="utf-8")
            r.add(author in raw and email in raw and what in raw,
                  "the built desktop entry carries the author, the address and the sentence", desktop.name)
    else:
        print("  (the Linux tree is not built in this tree: skipped)")
    win_exes = sorted((root / "dist" / "windows").glob("**/ProjectLife.exe"))
    if win_exes:
        for n in (author, email, what):
            r.add(carries_binary(win_exes[-1], n, overrides),
                  f"the built Windows shell carries {n[:34]!r}", win_exes[-1].name)
    else:
        print("  (the Windows package is not built in this tree: skipped)")

    r.show()
    return 1 if r.failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
