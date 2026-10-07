#!/usr/bin/env python3
"""The moment field, tested where it will actually run — in the page's own code, under node.

`<input type="datetime-local">` is supported by Safari only from 14.1 (macOS 11.3). The bundle says
macOS 11.0 is enough, so on 11.0-11.2 that control is a plain text box. Whatever is typed there has
to be read the same way as what a picker produces — and text that is *not* a moment has to be refused
rather than silently read as the epoch (which would make "compare with 1970" look like an answer).

This pulls the function out of the shipped `app/ui/app.js` and runs it under node, so it tests the
bytes that ship rather than a copy of the idea.

    python3 tools/moment_input_check.py [--ui app/ui/app.js]
"""
import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

CASES = [
    # what the picker produces
    ("2026-10-06T16:04", True),
    # what a person types, and what a text fallback produces on macOS 11.0-11.2
    ("2026-10-06 16:04", True),
    ("2026-10-06", True),
    ("  2026-10-06 16:04  ", True),
    # not a moment
    ("", False),
    ("not a date", False),
    ("2026-13-45", False),
    ("yesterday evening", False),
]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--ui", default=str(Path(__file__).resolve().parent.parent / "app/ui/app.js"))
    args = ap.parse_args()
    text = Path(args.ui).read_text(encoding="utf-8")

    m = re.search(r"function momentFromInput\(value\) \{.*?\n\}", text, re.S)
    if not m:
        print("FAIL: momentFromInput is not in the page — the date fields have no parser")
        return 2
    fn = m.group(0)

    script = fn + "\n" + json.dumps([c for c, _ in CASES]) + ".forEach(function (v) {" \
        "  const r = momentFromInput(v);" \
        "  console.log(JSON.stringify({input: v, ok: r !== null, ms: r}));" \
        "});"
    out = subprocess.run(["node", "-e", script], capture_output=True, text=True)
    if out.returncode != 0:
        print("FAIL: node could not run the parser:\n" + out.stderr[:600])
        return 2

    got = [json.loads(line) for line in out.stdout.splitlines() if line.strip().startswith("{")]
    bad = 0
    for (value, want), row in zip(CASES, got):
        if row["ok"] != want:
            print(f"FAIL: {value!r} → read as a moment: {row['ok']}, expected {want}")
            bad += 1
        elif want and not (row["ms"] > 0):
            print(f"FAIL: {value!r} → {row['ms']}, which is not a moment in this century")
            bad += 1
    if bad:
        print(f"FAIL: {bad} of {len(CASES)} inputs read wrongly")
        return 1
    print(f"PASS: {len(CASES)} inputs read correctly — the picker's form, the typed form, "
          f"and text that is not a moment (refused, not read as the epoch)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
