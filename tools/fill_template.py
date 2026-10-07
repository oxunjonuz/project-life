#!/usr/bin/env python3
"""Fill @TOKEN@ placeholders in a build template, and refuse to leave one behind.

Three file formats in this project are filled at build time from values that already exist somewhere
else, and all three used to be filled with `sed`:

  * `app/macos/Info.plist`      — the version, the author, the address, the sentence about purpose
  * `app/windows/ProjectLife.rc` — the same, inside the Windows version resource
  * `app/linux/.../project-life.desktop` — the same, inside the desktop entry

`sed` has one failure mode that matters and that nobody notices: a token it does not know about is
left in the file *as text*. A shipped `Info.plist` containing `@VERSION@` looks fine to every script
and wrong to every user. So this tool replaces what it knows and then **fails the build** if any
`@TOKEN@` is still there, naming them.

Usage:

    fill_template.py TEMPLATE OUT [--brand] [KEY=VALUE ...]
    fill_template.py --selftest

`--brand` adds the values from `src/brand.rs` (through `tools/brand.py`, the single reader) as the
tokens @PRODUCT@ @AUTHOR@ @AUTHOR_EMAIL@ @BY@ @COPYRIGHT@ @LICENCE@ @TAGLINE@ @TAGLINE_RU@
@WHAT_IT_IS@ @WHAT_IT_IS_RU@ @ANSWERS@.

Exit codes: 0 written, 1 a token was left unfilled or a value was malformed, 2 usage.
"""

import re
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent

TOKEN = re.compile(r"@([A-Z][A-Z0-9_]*)@")

BRAND_TOKENS = [
    "PRODUCT",
    "AUTHOR",
    "AUTHOR_EMAIL",
    "BY",
    "COPYRIGHT",
    "LICENCE",
    "TAGLINE",
    "TAGLINE_RU",
    "WHAT_IT_IS",
    "WHAT_IT_IS_RU",
    "ANSWERS",
]


def brand_values():
    """The brand values, read through the one reader — never a second copy of the words."""
    out = subprocess.run(
        [sys.executable, str(HERE / "brand.py"), "json"],
        capture_output=True,
        text=True,
        cwd=ROOT,
    )
    if out.returncode != 0:
        raise SystemExit(f"fill_template.py: brand.py failed: {out.stderr.strip()}")
    import json

    b = json.loads(out.stdout)
    return {
        "PRODUCT": b["product"],
        "AUTHOR": b["author"],
        "AUTHOR_EMAIL": b["authorEmail"],
        "BY": b["by"],
        "COPYRIGHT": b["copyright"],
        "LICENCE": b["licence"],
        "TAGLINE": b["tagline"],
        "TAGLINE_RU": b["taglineRu"],
        "WHAT_IT_IS": b["whatItIs"],
        "WHAT_IT_IS_RU": b["whatItIsRu"],
        "ANSWERS": b["answers"],
    }


def fill(text, values):
    """The filled text, and the tokens nobody supplied."""
    def sub(m):
        k = m.group(1)
        return values[k] if k in values else m.group(0)

    filled = TOKEN.sub(sub, text)
    left = sorted({m.group(1) for m in TOKEN.finditer(filled)})
    return filled, left


def selftest():
    """Prove the refusal works: a token with no value must fail, and a known one must be replaced."""
    filled, left = fill("v=@VERSION@ author=@AUTHOR@ nope=@NOT_A_VALUE@", {"VERSION": "1.2.3", "AUTHOR": "X"})
    ok = True
    if left != ["NOT_A_VALUE"]:
        print(f"selftest FAIL: unfilled tokens were {left}, expected ['NOT_A_VALUE']")
        ok = False
    if "v=1.2.3 author=X" not in filled:
        print(f"selftest FAIL: known tokens were not replaced: {filled!r}")
        ok = False
    # The control in the other direction: with no unknown token, nothing may be reported.
    _, left2 = fill("v=@VERSION@", {"VERSION": "1.2.3"})
    if left2:
        print(f"selftest FAIL: a fully filled template reported {left2}")
        ok = False
    print("fill_template selftest: " + ("PASS" if ok else "FAIL"))
    return 0 if ok else 1


def main(argv):
    if len(argv) == 2 and argv[1] == "--selftest":
        return selftest()
    if len(argv) < 3:
        print(__doc__.strip().splitlines()[0])
        print("usage: fill_template.py TEMPLATE OUT [--brand] [KEY=VALUE ...]")
        return 2
    tpl_path, out_path = Path(argv[1]), Path(argv[2])
    values = {}
    for arg in argv[3:]:
        if arg == "--brand":
            values.update(brand_values())
        elif "=" in arg:
            k, v = arg.split("=", 1)
            values[k] = v
        else:
            print(f"fill_template.py: {arg!r} is neither --brand nor KEY=VALUE", file=sys.stderr)
            return 2
    try:
        text = tpl_path.read_text(encoding="utf-8")
    except OSError as e:
        print(f"fill_template.py: cannot read {tpl_path}: {e}", file=sys.stderr)
        return 1
    filled, left = fill(text, values)
    if left:
        print(
            f"fill_template.py: {tpl_path} still asks for {', '.join('@' + t + '@' for t in left)} — "
            "the build script must pass a value for it (a shipped placeholder is a lie)",
            file=sys.stderr,
        )
        return 1
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(filled, encoding="utf-8")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
