#!/usr/bin/env python3
"""One threshold, three readers, and the machine as the fourth.

Round 296 came from the owner's Mac: the archive was declared full on a disk with 1.8 GB free, the
notification centre filled with the same sentence, and the window kept saying "Protected" while the
daemon wrote nothing. The fix is one implementation of the rule (`src/space.rs`) read by the daemon,
the doctor, `heartbeat-check` and the window. This script is the check that does *not* share their
blind spot: it measures the volume with `os.statvfs` — Python's own view of the same disk — and
compares it with the numbers the program reports, then drives the whole transition at the command
level and counts the notifications in the archive's own log.

    python3 tools/space_transition_check.py --pl /path/to/projectlife [--work DIR]

Exit code 0 only when every check passed.
"""
import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

PASS = 0
FAIL = 0


def check(name, ok, detail=""):
    global PASS, FAIL
    if ok:
        PASS += 1
        print(f"  PASS: {name}" + (f" — {detail}" if detail else ""))
    else:
        FAIL += 1
        print(f"  FAIL: {name}" + (f" — {detail}" if detail else ""))


def run(binary, home, args, timeout=180):
    env = dict(os.environ)
    env["PROJECTLIFE_HOME"] = str(home)
    r = subprocess.run([binary] + args, capture_output=True, text=True, env=env, timeout=timeout)
    return r.returncode, r.stdout, r.stderr


def json_doc(text):
    """The program prints human lines first in some commands; take the last JSON document."""
    start = text.rfind("\n{")
    start = start + 1 if start >= 0 else text.find("{")
    return json.loads(text[start:])


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--pl", required=True)
    ap.add_argument("--work", default="/tmp/pl-space-check")
    args = ap.parse_args()
    root = Path(args.work)
    shutil.rmtree(root, ignore_errors=True)
    (root / "home").mkdir(parents=True)
    proj = root / "work"
    (proj / "src").mkdir(parents=True)
    (proj / "src" / "main.rs").write_text("fn main() {}\n")
    arch = root / "archive"
    home = root / "home"

    code, out, err = run(args.pl, home, ["init-archive", str(arch)])
    check("the archive is created", code == 0, err.strip()[:120])
    code, out, err = run(args.pl, home, ["add", str(proj), "--yes", "--preset", "developer"])
    check("the project is added", code == 0, err.strip()[:120])

    # ---- 1. the numbers the program reports against the numbers this script measures -----------
    code, out, _ = run(args.pl, home, ["heartbeat-check", "--json"])
    hb = json_doc(out)
    st = hb["storage"]
    vfs = os.statvfs(str(arch))
    py_total = vfs.f_blocks * vfs.f_frsize
    py_free = vfs.f_bavail * vfs.f_frsize
    check("the program's volume size is the one this script measures with statvfs",
          st["totalBytes"] == py_total, f"program {st['totalBytes']} vs statvfs {py_total}")
    drift = abs(st["freeBytes"] - py_free) / max(py_free, 1)
    check("its free space agrees with statvfs to within the writing between the two calls",
          drift < 0.01, f"program {st['freeBytes']} vs statvfs {py_free} ({drift * 100:.3f} %)")
    print(f"   the two thresholds on this volume: stop {st['stopBytes']} B, warn {st['warnBytes']} B, "
          f"state {st['state']}")

    # The rule: min(fixed, percentage). Stated here independently, from the same two numbers.
    cfg_total = st["totalBytes"]
    expected_stop = min(500 * 1024 * 1024, cfg_total // 100)
    check("the stop threshold is the smaller of the fixed floor and one percent",
          st["stopBytes"] == expected_stop, f"program {st['stopBytes']}, this script expects {expected_stop}")

    # ---- 2. not enough room, said once --------------------------------------------------------
    for key, value in (("stopFreeBytes", "1099511627776"), ("stopFreePercent", "100")):
        code, _out, err = run(args.pl, home, ["config", "set", key, value])
        check(f"config {key} is set", code == 0, err.strip()[:120])

    (proj / "src" / "main.rs").write_text("fn main() { /* changed while full */ }\n")
    states = []
    for _ in range(5):
        _c, out, _e = run(args.pl, home, ["scan-once", "work", "--json"])
        states.append(json_doc(out)["archiveState"])
    check("every cycle while there is no room reports ARCHIVE_FULL",
          all(s == "ARCHIVE_FULL" for s in states), ", ".join(states))
    log = (arch / "logs" / "projectlife.log").read_text()
    stopped = [l for l in log.splitlines() if "NOTIFY:" in l and "recording stopped" in l]
    check("five cycles produced exactly one notification", len(stopped) == 1,
          f"{len(stopped)} notification(s)")
    quiet = [l for l in log.splitlines() if "writing is still stopped" in l]
    check("and the log shows the cycles that kept quiet", len(quiet) >= 3, f"{len(quiet)} line(s)")
    notice = json.loads((arch / "logs" / "space_state.json").read_text())
    check("the archive remembers that it already said so",
          notice.get("state") == "full" and notice.get("lastNotifyMs", 0) > 0, json.dumps(notice))

    _c, out, _e = run(args.pl, home, ["heartbeat-check", "--json"])
    hb = json_doc(out)
    check("the window's own document says the archive is full", hb["storage"]["state"] == "full",
          hb["storage"]["reason"][:100])
    reason = hb["storage"]["reason"]
    check("and the sentence carries both numbers",
          "1.0 TB" in reason and "100 %" in reason, reason[:140])
    code, _out, _err = run(args.pl, home, ["healthcheck"])
    check("healthcheck fails, so a scheduler notices", code == 1, f"exit {code}")

    # ---- 3. ten minutes later the reminder comes ----------------------------------------------
    notice["lastNotifyMs"] = int(time.time() * 1000) - 11 * 60 * 1000
    (arch / "logs" / "space_state.json").write_text(json.dumps(notice))
    run(args.pl, home, ["scan-once", "work", "--json"])
    log = (arch / "logs" / "projectlife.log").read_text()
    stopped = [l for l in log.splitlines() if "NOTIFY:" in l and "recording stopped" in l]
    check("the reminder arrives after the interval, once", len(stopped) == 2,
          f"{len(stopped)} notification(s)")

    # ---- 4. room again: it says so once, and the lost change is recorded -----------------------
    for key, value in (("stopFreeBytes", "524288000"), ("stopFreePercent", "1")):
        run(args.pl, home, ["config", "set", key, value])
    _c, out, _e = run(args.pl, home, ["scan-once", "work", "--json"])
    doc = json_doc(out)
    check("the cycle records again", doc["archiveState"] != "ARCHIVE_FULL", json.dumps(doc)[:160])
    changed = doc["projects"][0]["changed"] if doc.get("projects") else 0
    check("the change made during the outage is recorded afterwards", changed == 1,
          f"changed={changed}")
    log = (arch / "logs" / "projectlife.log").read_text()
    resumed = [l for l in log.splitlines() if "NOTIFY:" in l and "recording resumed" in l]
    check("the resumption is announced exactly once", len(resumed) == 1, f"{len(resumed)} notification(s)")
    for _ in range(3):
        run(args.pl, home, ["scan-once", "work", "--json"])
    log = (arch / "logs" / "projectlife.log").read_text()
    resumed = [l for l in log.splitlines() if "NOTIFY:" in l and "recording resumed" in l]
    check("and never announced again afterwards", len(resumed) == 1, f"{len(resumed)} notification(s)")

    print(f"\n{PASS} PASS, {FAIL} FAIL")
    return 1 if FAIL else 0


if __name__ == "__main__":
    sys.exit(main())
