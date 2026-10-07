#!/usr/bin/env python3
"""Check that the interface inside a built binary is the interface in the sources.

The window's HTML/CSS/JS are compiled into `projectlife-ui` with `include_str!`. That is what makes
"the interface that was tested is the interface that ships" a checkable statement rather than a hope:
the bytes are either in the binary or they are not.

    python3 tools/embedded_ui_check.py <binary> [<other binary> ...]
"""
import hashlib
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FILES = ["index.html", "app.css", "app.js"]


def main():
    bins = [Path(p) for p in sys.argv[1:]]
    if not bins:
        print("usage: embedded_ui_check.py <binary> [...]")
        return 2
    ok = True
    for name in FILES:
        data = (ROOT / "app/ui" / name).read_bytes()
        digest = hashlib.sha256(data).hexdigest()
        row = [f"{name:11s} sha256 {digest[:16]}… {len(data):6d} bytes"]
        for b in bins:
            present = data in b.read_bytes()
            row.append(f"{b.name}: {'yes' if present else 'NO'}")
            ok = ok and present
        print("  ".join(row))
    print("RESULT:", "the same interface bytes are in every binary" if ok else "MISMATCH")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
