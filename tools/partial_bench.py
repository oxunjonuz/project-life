#!/usr/bin/env python3
"""What does a partial pass cost, next to the full pass it replaces?

Round 293's claim is not "it is faster in theory" but a number: on a project of 10 000 files, a
notification-driven pass that was told about one changed file must cost a fraction of an ordinary
cycle. This tool measures both, on the same fixture, on the same filesystem, with the shipped
binary — the full pass through `pl scan-once`, the partial pass through `pl partial-pass` (the same
entry point the daemon uses when inotify reports a change).

Every run of the partial pass first rewrites the files it is about to be told about, so the pass
really reads, hashes and stores them: the number is the cost of the work, not of finding nothing.

Usage:
  python3 tools/partial_bench.py --bin target/release/projectlife [--work DIR] [--files 10000]
                                [--runs 5] [--json out.json]
"""

import argparse
import json
import os
import shutil
import statistics
import subprocess
import sys
import time


def run(args, env=None):
    e = dict(os.environ)
    if env:
        e.update(env)
    return subprocess.run(args, capture_output=True, text=True, env=e)


def build_project(root, files):
    p = os.path.join(root, "project")
    if os.path.isdir(p):
        shutil.rmtree(p)
    for i in range(files):
        d = os.path.join(p, "src%03d" % (i // 100))
        os.makedirs(d, exist_ok=True)
        with open(os.path.join(d, "file%05d.txt" % i), "w") as f:
            f.write("content %d\n" % i)
    return p


def pct(values, p):
    if not values:
        return None
    v = sorted(values)
    i = min(len(v) - 1, int(round((len(v) - 1) * p)))
    return v[i]


def summarise(times):
    return {
        "samples_ms": [round(t * 1000.0, 1) for t in times],
        "median_ms": round(statistics.median(times) * 1000.0, 1) if times else None,
        "p95_ms": round(pct(times, 0.95) * 1000.0, 1) if times else None,
        "max_ms": round(max(times) * 1000.0, 1) if times else None,
        "min_ms": round(min(times) * 1000.0, 1) if times else None,
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default="/work/projectlife/target/release/projectlife")
    ap.add_argument("--work", default="/work/projectlife/tmp/partial-bench")
    ap.add_argument("--files", type=int, default=10000)
    ap.add_argument("--runs", type=int, default=5)
    ap.add_argument("--json", default="")
    args = ap.parse_args()

    binary = os.path.abspath(args.bin)
    work = os.path.abspath(args.work)
    # A fresh fixture every run: the archive holds the state of the previous fixture, and "a project
    # is already being observed here" would end the measurement before it starts.
    if os.path.isdir(work):
        shutil.rmtree(work)
    os.makedirs(work)
    home = os.path.join(work, "home")
    os.makedirs(home, exist_ok=True)
    archive = os.path.join(work, "archive")
    env = {"PROJECTLIFE_HOME": home}

    proj = build_project(work, args.files)
    r = run([binary, "--archive", archive, "init-archive", archive], env)
    if r.returncode != 0:
        print("init-archive failed: %s %s" % (r.stdout, r.stderr))
        return 2
    for k, v in (("stopFreePercent", "0"), ("warnFreePercent", "0")):
        run([binary, "--archive", archive, "config", "set", k, v], env)
    r = run([binary, "--archive", archive, "add", proj, "--name", "bench", "--yes"], env)
    if r.returncode != 0:
        print("add failed: %s %s" % (r.stdout, r.stderr))
        return 2

    result = {
        "bin": binary,
        "work": work,
        "files": args.files,
        "runs": args.runs,
        "filesystem": subprocess.run(["stat", "-f", "-c", "%T", work], capture_output=True, text=True).stdout.strip(),
    }

    def all_paths():
        out = []
        for dirpath, _dirnames, filenames in os.walk(proj):
            for f in sorted(filenames):
                out.append(os.path.relpath(os.path.join(dirpath, f), proj))
        return sorted(out)

    everything = all_paths()

    # --- the full pass on 10 000 unchanged files ------------------------------------------------
    times = []
    for i in range(3):
        t0 = time.time()
        r = run([binary, "--archive", archive, "scan-once", "bench", "--json"], env)
        times.append(time.time() - t0)
        if r.returncode != 0:
            print("scan-once failed: %s %s" % (r.stdout, r.stderr))
            return 2
    result["full_cycle"] = summarise(times)
    result["full_cycle"]["note"] = "pl scan-once over %d unchanged files, 3 runs, no warm-up drop" % args.files

    # --- the partial pass for 1, 10 and 100 changed files ---------------------------------------
    result["partial"] = {}
    for n in (1, 10, 100):
        targets = everything[:n]
        times = []
        refusals = 0
        for run_no in range(args.runs):
            for rel in targets:
                with open(os.path.join(proj, rel), "w") as f:
                    f.write("changed by run %d\n" % run_no)
            t0 = time.time()
            r = run([binary, "--archive", archive, "partial-pass", "bench", *targets, "--json"], env)
            times.append(time.time() - t0)
            if r.returncode != 0:
                refusals += 1
                print("partial-pass failed: %s %s" % (r.stdout, r.stderr))
        s = summarise(times)
        s["changed_files"] = n
        s["refusals"] = refusals
        s["times_the_full_cycle"] = (
            round(s["median_ms"] / result["full_cycle"]["median_ms"], 4)
            if s["median_ms"] and result["full_cycle"]["median_ms"]
            else None
        )
        result["partial"]["%d" % n] = s

    # --- the same partial pass, but handed the whole directory (the scope a directory event has) --
    times = []
    for run_no in range(args.runs):
        rel = everything[0]
        with open(os.path.join(proj, rel), "w") as f:
            f.write("directory scope run %d\n" % run_no)
        t0 = time.time()
        r = run([binary, "--archive", archive, "partial-pass", "bench", os.path.dirname(rel)], env)
        times.append(time.time() - t0)
    s = summarise(times)
    s["changed_files"] = 1
    s["note"] = "the scope was one directory of %d files (100 per directory)" % 100
    result["partial_one_directory"] = s

    # --- the fixed cost: a pass whose scope is empty. Nothing is walked, nothing is read, nothing is
    #     stored — what is left is opening the archive, reading the state and the journal, locking,
    #     and writing the state back. Measured with a path that does not exist (a notification about
    #     a file that was created and removed again looks exactly like this).
    times = []
    for _ in range(args.runs):
        t0 = time.time()
        r = run([binary, "--archive", archive, "partial-pass", "bench", "src/does-not-exist.txt", "--json"], env)
        times.append(time.time() - t0)
        if r.returncode != 0:
            print("partial-pass (empty scope) failed: %s %s" % (r.stdout, r.stderr))
    s = summarise(times)
    s["note"] = "scope: a path that does not exist — no file was walked, read or stored"
    result["partial_empty_scope"] = s
    result["state_bytes"] = {}
    for base, _dirs, files in os.walk(os.path.join(archive, "projects")):
        for f in files:
            p = os.path.join(base, f)
            if f == "state.json" or f.endswith(".jsonl") or f == "project.json":
                key = f if f != "state.json" else "state.json"
                result["state_bytes"][key] = result["state_bytes"].get(key, 0) + os.path.getsize(p)

    print("filesystem: %s   fixture: %d files   runs per measurement: %d" % (result["filesystem"], args.files, args.runs))
    print("%-34s %10s %10s %10s %10s" % ("pass", "median ms", "p95 ms", "max ms", "min ms"))
    print("%-34s %10s %10s %10s %10s" % ("full (scan-once, unchanged)", result["full_cycle"]["median_ms"],
                                         result["full_cycle"]["p95_ms"], result["full_cycle"]["max_ms"],
                                         result["full_cycle"]["min_ms"]))
    for n in (1, 10, 100):
        s = result["partial"]["%d" % n]
        print("%-34s %10s %10s %10s %10s   %.4f x full" % ("partial, %d file(s) notified" % n, s["median_ms"], s["p95_ms"],
                                                           s["max_ms"], s["min_ms"], s["times_the_full_cycle"] or 0))
    s = result["partial_one_directory"]
    print("%-34s %10s %10s %10s %10s   (scope: one directory)" % ("partial, directory notified", s["median_ms"],
                                                                  s["p95_ms"], s["max_ms"], s["min_ms"]))
    s = result["partial_empty_scope"]
    print("%-34s %10s %10s %10s %10s   (the fixed cost of any pass)" % ("partial, empty scope", s["median_ms"],
                                                                        s["p95_ms"], s["max_ms"], s["min_ms"]))
    print("archive state on disk: %s" % ", ".join("%s %d B" % (k, v) for k, v in sorted(result["state_bytes"].items())))
    print("samples, full:    %s" % result["full_cycle"]["samples_ms"])
    for n in (1, 10, 100):
        print("samples, partial %-3d %s" % (n, result["partial"]["%d" % n]["samples_ms"]))
    print("samples, empty scope %s" % result["partial_empty_scope"]["samples_ms"])

    if args.json:
        with open(args.json, "w") as f:
            json.dump(result, f, indent=2)
        print("raw: %s" % args.json)
    return 0


if __name__ == "__main__":
    sys.exit(main())
