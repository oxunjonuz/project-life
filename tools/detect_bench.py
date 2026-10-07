#!/usr/bin/env python3
"""How long does `pl detect` take on a big folder — and does it really stay metadata-only?

The promise (SPEC §6.17, deliverable 9) is: 100 000 entries in under 2 seconds. This tool builds a
fixture of that size, runs the *shipped* binary on it three times, and prints every measurement
rather than an average, because a single number hides the spread.

  python3 tools/detect_bench.py [--files 100000] [--bin PATH] [--keep]

Exit code 0 only if every run finished inside the budget. Nothing in the real project is touched:
the fixture lives in a scratch directory and is removed unless --keep is given.
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time

BUDGET_MS = 2000


def build_fixture(root, files):
    os.makedirs(root, exist_ok=True)
    stamp = os.urandom(8)
    for i in range(files):
        name = "file_%06d.%s" % (i, ("txt", "md", "json", "csv")[i % 4])
        path = os.path.join(root, name)
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o644)
        os.write(fd, stamp)
        os.close(fd)


def floor_probe(root, files):
    """Where the time actually goes: the same directory, once without and once with a metadata call
    per file. This is the kernel's share, and it is usually the whole story."""
    t0 = time.time()
    n = 0
    with os.scandir(root) as it:
        for _ in it:
            n += 1
    readdir_ms = int((time.time() - t0) * 1000)
    t0 = time.time()
    with os.scandir(root) as it:
        for e in it:
            try:
                e.stat(follow_symlinks=False)
            except OSError:
                pass
    with_stat_ms = int((time.time() - t0) * 1000)
    print(
        "floor on this disk: readdir alone %d ms, readdir + one metadata call per file %d ms "
        "(%d files) -> the program cannot be faster than that" % (readdir_ms, with_stat_ms, n)
    )


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--files", type=int, default=100000)
    ap.add_argument("--bin", default=os.environ.get("PROJECTLIFE_BIN", "/work/projectlife/target/release/projectlife"))
    ap.add_argument("--runs", type=int, default=3)
    ap.add_argument("--target-ms", type=int, default=2000, help="the brief's target for 100 000 files")
    ap.add_argument("--cap-ms", type=int, default=5000, help="the design cap (FR-PRE-8)")
    ap.add_argument("--keep", action="store_true")
    args = ap.parse_args()

    if not os.path.exists(args.bin):
        print("no binary at %s" % args.bin)
        return 1

    root = tempfile.mkdtemp(prefix="projectlife-detect-bench-")
    data = os.path.join(root, "Data")
    t0 = time.time()
    build_fixture(data, args.files)
    build_s = time.time() - t0

    print("fixture: %d files in %s (built in %.1f s)" % (args.files, data, build_s))
    floor_probe(data, args.files)
    worst = 0
    results = []
    for run in range(args.runs):
        t0 = time.time()
        p = subprocess.run([args.bin, "detect", data, "--json"], capture_output=True, text=True)
        wall_ms = int((time.time() - t0) * 1000)
        if p.returncode != 0:
            print("run %d: the binary failed: %s" % (run + 1, p.stderr.strip()[:200]))
            shutil.rmtree(root, ignore_errors=True)
            return 1
        d = json.loads(p.stdout)
        results.append(wall_ms)
        worst = max(worst, wall_ms)
        print(
            "run %d: %6d ms (binary reports %6d ms) files=%d entries=%d unsized=%d truncated=%s profile=%s"
            % (
                run + 1,
                wall_ms,
                d["elapsedMs"],
                d["files"],
                d["entriesScanned"],
                d["unsizedFiles"],
                d["truncated"],
                d["detectedProfile"],
            )
        )

    ordered = sorted(results)
    median = ordered[len(ordered) // 2]
    over_target = sum(1 for r in results if r > args.target_ms)
    print("median %d ms, worst %d ms, best %d ms" % (median, worst, ordered[0]))
    print("target %d ms: %d of %d runs inside it" % (args.target_ms, len(results) - over_target, len(results)))
    print("cap %d ms: %s" % (args.cap_ms, "held" if worst < args.cap_ms else "EXCEEDED"))
    if not args.keep:
        shutil.rmtree(root, ignore_errors=True)
        print("fixture removed")
    else:
        print("fixture kept at %s" % data)
    if worst >= args.cap_ms:
        print("FAIL: the design cap was exceeded")
        return 1
    if over_target:
        print("NOTE: the target was missed in %d of %d runs — reported, not hidden" % (over_target, len(results)))
        return 2
    print("OK: every run stayed inside the target")
    return 0


if __name__ == "__main__":
    sys.exit(main())
