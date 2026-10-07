#!/usr/bin/env python3
"""NFR-PRF measurement for Project Life: real cycles of the real binary on real files.

Two projects are built from scratch in a scratch directory:

  * ``files10k`` — 10 000 unchanged files in 100 folders. This is the NFR-PRF-2 promise:
                   "an ordinary cycle over 10 000 unchanged files takes <= 1 s".
  * ``deps50k``  — one ordinary source file plus ``node_modules`` holding 50 000 files. The
                   promise is that the ordinary cycle does not count (does not even open) a
                   skipped directory; ``scan-once --skipped`` is the mode that does count.

Three cycles are timed per project and the median reported; the first is labelled cold. Everything
is measured with the shipped binary, not with a library call, because the promise is about the
program.

Usage:
  python3 tools/perf.py --bin target/release/projectlife [--work /tmp/projectlife-perf]
                        [--json out.json]
"""

import json
import os
import shutil
import statistics
import subprocess
import sys
import time


def sh(args, env=None, cwd=None):
    e = dict(os.environ)
    if env:
        e.update(env)
    return subprocess.run(args, capture_output=True, text=True, env=e, cwd=cwd)


def build_project(root, name, files, node_modules=0):
    """Create a project folder: `files` ordinary files, plus `node_modules` files if asked."""
    p = os.path.join(root, name)
    if os.path.isdir(p):
        shutil.rmtree(p)
    for i in range(files):
        d = os.path.join(p, "src%03d" % (i // 100))
        os.makedirs(d, exist_ok=True)
        with open(os.path.join(d, "file%05d.txt" % i), "w") as f:
            f.write("content %d\n" % i)
    if node_modules:
        nm = os.path.join(p, "node_modules", "pkg")
        os.makedirs(nm, exist_ok=True)
        for i in range(node_modules):
            open(os.path.join(nm, "dep%06d.js" % i), "w").close()
    return p


def timed_cycle(binary, archive, name, extra=None, repeat=3):
    """Run `scan-once` against one project, time every run, return (times, stdout)."""
    times = []
    out = ""
    for _ in range(repeat):
        args = [binary, "--archive", archive, "scan-once", name] + list(extra or [])
        t0 = time.perf_counter()
        r = sh(args)
        dt = time.perf_counter() - t0
        if r.returncode != 0:
            raise SystemExit("scan-once failed (%d): %s%s" % (r.returncode, r.stdout, r.stderr))
        out = r.stdout
        times.append(dt)
    return times, out


def main():
    binary = None
    work = "/tmp/projectlife-perf"
    json_out = None
    args = sys.argv[1:]
    while args:
        a = args.pop(0)
        if a == "--bin":
            binary = args.pop(0)
        elif a == "--work":
            work = args.pop(0)
        elif a == "--json":
            json_out = args.pop(0)
        else:
            raise SystemExit("unknown argument: %s" % a)
    binary = os.path.abspath(binary or "target/release/projectlife")
    if not os.path.isfile(binary):
        raise SystemExit("no such binary: %s (build it first)" % binary)

    if os.path.isdir(work):
        shutil.rmtree(work)
    os.makedirs(work)
    archive = os.path.join(work, "archive")
    r = sh([binary, "init-archive", archive], env={"PROJECTLIFE_HOME": os.path.join(work, "home")})
    if r.returncode != 0:
        raise SystemExit("init-archive failed: %s%s" % (r.stdout, r.stderr))
    # The volume this runs on can be nearly full; disk thresholds are not what is measured here.
    cfg = os.path.join(archive, "config.json")
    conf = json.load(open(cfg))
    conf["stopFreePercent"] = 0
    conf["warnFreePercent"] = 0
    json.dump(conf, open(cfg, "w"), indent=2)

    home = {"PROJECTLIFE_HOME": os.path.join(work, "home")}
    result = {
        "binary": binary,
        "archive": archive,
        "filesystem": os.popen("df -h %s | tail -1" % work).read().strip(),
    }

    for label, count, deps in (("files10k", 10000, 0), ("deps50k", 1, 50000)):
        t_build = time.perf_counter()
        proj = build_project(work, label, count, deps)
        build_s = time.perf_counter() - t_build
        r = sh([binary, "--archive", archive, "add", proj, "--name", label, "--yes"], env=home)
        if r.returncode != 0:
            raise SystemExit("add failed: %s%s" % (r.stdout, r.stderr))
        entry = {
            "project": label,
            "files": count,
            "node_modules_files": deps,
            "build_seconds": round(build_s, 2),
        }
        ordinary, out1 = timed_cycle(binary, archive, label)
        entry["ordinary_cycle_seconds"] = [round(t, 4) for t in ordinary]
        entry["ordinary_median_seconds"] = round(statistics.median(ordinary), 4)
        entry["ordinary_output"] = out1.strip().splitlines()[0] if out1.strip() else ""
        if deps:
            # Positive control: the same cycle WITH counting — what the ordinary cycle must not be
            # doing. If both are equally fast, the measurement cannot tell them apart.
            counted, out2 = timed_cycle(binary, archive, label, extra=["--skipped"])
            entry["counted_cycle_seconds"] = [round(t, 4) for t in counted]
            entry["counted_median_seconds"] = round(statistics.median(counted), 4)
            entry["counted_output"] = out2.strip().splitlines()[0] if out2.strip() else ""
            entry["counted_skip_lines"] = [
                ln.strip() for ln in out2.splitlines() if "ignored_dir" in ln and "node_modules" in ln
            ][:3]
        result[label] = entry

    text = json.dumps(result, indent=2)
    print(text)
    if json_out:
        with open(json_out, "w") as f:
            f.write(text + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
