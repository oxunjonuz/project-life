#!/usr/bin/env python3
"""Proof that the window-contract checker can fail.

A check that only ever passes proves nothing. This script copies the window's sources, injects one
known fault at a time, and requires `tools/ui_surface_check.py` to report FAIL for every one of them.
If the checker passes on any injected fault, this script exits non-zero and names it.

It also proves the opposite direction: the untouched copy must pass, so the checker is not simply
refusing everything.

    python3 tools/ui_surface_control.py [--root /work/projectlife]

Nothing here touches the real sources: every fault is injected into a copy under /tmp.
"""
import argparse
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

# One fault per rule the checker claims to enforce.
FAULTS = [
    ("a route the page calls that the server does not answer",
     "app/ui/app.js", "api('check?deep='", "api('check-that-does-not-exist?deep='"),
    ("a control with no handler (a button that would do nothing)",
     "app/ui/app.js", "case 'load-suggest': S.suggest = await api('suggest'); break;", ""),
    ("a project selector that forgets the choice on every redraw",
     "app/ui/app.js", "'<option' + (toolProject() === p.name ? ' selected' : '') + '>'", "'<option>'"),
    ("a confirmation asked for in the window but not checked by the route",
     "app/src/api.rs",
     """    if !confirmed(req) {
        return err(409, "this stops observing the folder; the window must ask first");
    }
""",
     ""),
    ("a guard the page never satisfies (the confirmation is never sent)",
     "app/ui/app.js", "body: { name: name, confirm: true }", "body: { name: name }"),
    ("a browser dialog the macOS shell cannot draw",
     "app/ui/app.js", "const back = document.createElement('div');",
     "window.prompt('name?'); const back = document.createElement('div');"),
    ("an About screen that stops showing the address the owner asked for",
     "app/ui/app.js", "const email = app.authorEmail || p.authorEmail || '';", "const email = '';"),
]


def run_checker(root: Path) -> int:
    return subprocess.run([sys.executable, str(Path(__file__).resolve().parent / "ui_surface_check.py"),
                           "--root", str(root)], capture_output=True, text=True).returncode


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", default=str(Path(__file__).resolve().parent.parent))
    args = ap.parse_args()
    src = Path(args.root)
    failures = []

    with tempfile.TemporaryDirectory(prefix="pl-surface-control-") as tmp:
        base = Path(tmp) / "tree"
        # The checker reads only these two files; copy exactly what it reads.
        (base / "app" / "ui").mkdir(parents=True)
        (base / "app" / "src").mkdir(parents=True)
        for rel in ("app/ui/app.js", "app/src/api.rs"):
            shutil.copy2(src / rel, base / rel)

        if run_checker(base) != 0:
            print("FAIL: the checker refuses the untouched sources — it cannot be trusted either way")
            return 2
        print("ok: the untouched copy passes")

        for i, (label, rel, old, new) in enumerate(FAULTS):
            work = Path(tmp) / f"fault{i}"
            shutil.copytree(base, work)
            path = work / rel
            text = path.read_text(encoding="utf-8")
            count = text.count(old)
            if count == 0:
                failures.append(f"{label}: the fault could not be injected (pattern absent)")
                print(f"FAIL: {label} — pattern not found in {rel}")
                continue
            path.write_text(text.replace(old, new), encoding="utf-8")
            code = run_checker(work)
            if code == 0:
                failures.append(label)
                print(f"FAIL: the checker did not notice — {label}")
            else:
                print(f"ok: caught — {label}")

    print()
    if failures:
        print(f"CONTROL FAILED: {len(failures)} of {len(FAULTS)} faults went unnoticed")
        return 1
    print(f"CONTROL PASSED: the checker caught {len(FAULTS)} of {len(FAULTS)} injected faults, "
          f"and passed the untouched copy")
    return 0


if __name__ == "__main__":
    sys.exit(main())
