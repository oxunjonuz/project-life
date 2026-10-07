#!/usr/bin/env python3
"""Proof that the menu-contract checker can fail.

A checker that only ever passes proves nothing, and this one is the only thing standing between a
full menu and a list of entries that look like features. So: copy the tree, inject one known fault
at a time, and require `tools/menu_contract_check.py --static-only` to report FAIL for every one.
The untouched copy must pass, so the checker is not simply refusing everything.

Nothing here touches the real sources: every fault goes into a copy under /tmp.

    python3 tools/menu_control.py [--root /work/projectlife]
"""
import argparse
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

# (what the fault is, file, from, to, which checker must catch it)
#   "static" — tools/menu_contract_check.py --static-only (reads the sources)
#   "unit"   — the app crate's own unit test for the value rules (a live server would also catch it)
FAULTS = [
    ("an entry stands for a command the core does not have",
     "app/src/menu.rs", '"pl detect <path> --json"', '"pl frobnicate <path>"', "static"),
    ("a view entry names a view the page cannot draw",
     "app/src/menu.rs", '"pl version", &[], "", "about",', '"pl version", &[], "", "aboutx",', "static"),
    ("a page entry that the page does not know how to perform",
     "app/ui/app.js", "  'file.export': () => exportHistory(),\n", "", "static"),
    ("a page flow the registry never declares (a dead entry)",
     "app/ui/app.js", "  'protect.stop': () => stopWatch(),",
     "  'protect.stop': () => stopWatch(),\n  'entry.that.does.not.exist': () => stopWatch(),", "static"),
    ("the page stops reading the menu from the server",
     "app/ui/app.js", "MENU = await api('menu' + q);", "MENU = { groups: [] };", "static"),
    ("the page carries its own copy of the menu",
     "app/ui/app.js", "  const views = VIEWS();",
     "  if (MENU) MENU.groups = [{ id: 'x', title: 'Mine', items: [{ id: 'file.add', label: 'Add' }] }];\n  const views = VIEWS();",
     "static"),
    ("the macOS shell stops taking the menu from the server",
     "app/macos/ProjectLife.m", "NSDictionary *doc = [self menuDocument];", "NSDictionary *doc = nil;", "static"),
    ("the server stops checking the entry against the registry",
     "app/src/api.rs", "let item = match crate::menu::find(&id) {",
     "let item = match Some(&crate::menu::ITEMS[0]) {", "static"),
    ("the server runs a change without a confirmation",
     "app/src/api.rs", "if item.kind == crate::menu::Kind::Confirm && !confirmed {", "if false {", "static"),
    ("a value is inserted as several arguments",
     "app/src/menu.rs", "        out.push(filled);",
     "        for part in filled.split_whitespace() { out.push(part.to_string()); }\n        continue;", "unit"),
    ("a value that looks like a flag is passed on",
     "app/src/menu.rs", "        if v.starts_with('-') {", "        if false {", "unit"),
    ("the 'left to the terminal' card names a command the menu runs",
     "app/ui/app.js", "['pl panic <project>',", "['pl scan-once <project>',", "static"),
]

UNIT_TEST = "a_value_is_one_argument_and_a_flag_like_value_is_refused"
CHECK = "menu_contract_check.py"


def run_checker(root: Path) -> int:
    tool = Path(__file__).resolve().parent / CHECK
    r = subprocess.run([sys.executable, str(tool), "--root", str(root), "--static-only"],
                       capture_output=True, text=True)
    return r.returncode


def run_unit(root: Path) -> int:
    """The app crate's own test for the two value rules, in the copy."""
    import os
    env = dict(os.environ)
    env["CARGO_TARGET_DIR"] = str(root / "app" / "target")
    r = subprocess.run(["cargo", "test", "--release", f"menu::tests::{UNIT_TEST}"],
                       cwd=str(root / "app"), capture_output=True, text=True, env=env)
    out = r.stdout + r.stderr
    if "test result: ok. 1 passed" in out:
        return 0
    return 1


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", default=str(Path(__file__).resolve().parent.parent))
    args = ap.parse_args()
    src = Path(args.root).resolve()

    work = Path(tempfile.mkdtemp(prefix="pl-menu-control-"))
    bad = 0
    try:
        copy = work / "tree"
        (copy / "app").mkdir(parents=True)
        (copy / "src").mkdir()
        for rel in ["app/src", "app/ui", "app/macos", "app/Cargo.toml", "app/target", "src/cli.rs"]:
            s = src / rel
            d = copy / rel
            if s.is_dir():
                shutil.copytree(s, d)
            elif s.is_file():
                shutil.copy2(s, d)

        rc = run_checker(copy)
        if rc != 0:
            print("FAIL: the untouched copy does not pass — the control cannot say anything")
            return 2
        print("PASS: the untouched copy passes the checker")

        for what, rel, old, new, how in FAULTS:
            f = copy / rel
            text = f.read_text(encoding="utf-8")
            if text.count(old) != 1:
                print(f"FAIL: the fault '{what}' matches {text.count(old)} times in {rel} — it was "
                      f"never injected, so nothing was tested")
                bad += 1
                continue
            f.write_text(text.replace(old, new), encoding="utf-8")
            rc = run_checker(copy) if how == "static" else run_unit(copy)
            if rc == 0:
                print(f"FAIL: the checker passed on '{what}' ({rel})")
                bad += 1
            else:
                print(f"PASS: the checker caught '{what}'")
            f.write_text(text, encoding="utf-8")

        # And once more, to prove the copies were restored rather than left broken.
        if run_checker(copy) != 0:
            print("FAIL: the copy does not pass again after restoring the faults")
            bad += 1
    finally:
        shutil.rmtree(work, ignore_errors=True)

    print(f"\nMENU CONTROL: {len(FAULTS) - bad}/{len(FAULTS)} faults caught")
    return 0 if bad == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
