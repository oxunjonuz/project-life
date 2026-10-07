#!/usr/bin/env python3
"""Measure the observation interval of Project Life with filesystem notifications and without.

Two questions are answered by measurement, not by a promise:

  * LATENCY — how long does a change take to become a stored version? The clock starts when the file
    is written and stops when a `put` event for exactly those bytes is in the journal.
  * INTERVAL — how far apart the observation passes themselves are, taken from the project's own
    `observedIntervalMs` (median/p95), which the program keeps for `status`.

Both are measured in the same fixture, on the same machine, one after the other, changing only the
trigger:

  * `periodic-only`   — `watchTriggers=false`: the periodic pass alone.
  * `notify-partial`  — `watchTriggers=true`, `partialPass=true`: as shipped since round 293, a
                        notification-driven pass walks only the paths that were named.
  * `notify-full`     — `watchTriggers=true`, `partialPass=false`: the round-290 behaviour, kept as a
                        switch, where a notification starts the ordinary full pass.

Every individual latency is printed, not only the summary, and every phase reports the trigger's own
counters, so "which kind of pass stored this version" is read from the program and not assumed.

Usage:
  python3 tools/watch_latency.py --bin target/release/projectlife --work /tmp/pl-latency [--quick]
"""

import argparse
import glob
import hashlib
import json
import os
import random
import shutil
import subprocess
import time

BIN = "projectlife"
SEED = 290


def sha256_bytes(b):
    return hashlib.sha256(b).hexdigest()


def journal_dir(archive):
    d = glob.glob(os.path.join(archive, "projects", "*", "events"))
    return d[0] if d else None


def puts(archive):
    """All (path, hash) pairs in the journal."""
    out = []
    d = journal_dir(archive)
    if not d:
        return out
    for f in sorted(glob.glob(os.path.join(d, "*.jsonl"))):
        with open(f) as fh:
            for line in fh:
                line = line.strip()
                if not line:
                    continue
                try:
                    e = json.loads(line)
                except Exception:
                    continue
                if e.get("type") == "put":
                    out.append((e.get("path", ""), e.get("hash", "")))
    return out


def watch_state(archive):
    p = os.path.join(archive, "watch_state.json")
    try:
        with open(p) as f:
            return json.load(f)
    except Exception:
        return {}


def project_meta(archive):
    for p in glob.glob(os.path.join(archive, "projects", "*", "project.json")):
        try:
            with open(p) as f:
                return json.load(f)
        except Exception:
            pass
    return {}


def run(bin_path, home, archive, *args, env=None):
    e = dict(os.environ)
    e["PROJECTLIFE_HOME"] = home
    if env:
        e.update(env)
    return subprocess.run([bin_path, "--archive", archive, *args], capture_output=True, text=True, env=e)


def build_fixture(work):
    """A fresh fixture per phase: the observed-interval window is a rolling list inside the project,
    so reusing one project would let an earlier phase's samples answer for a later one."""
    if os.path.isdir(work):
        shutil.rmtree(work)
    home = os.path.join(work, "home")
    proj = os.path.join(work, "project")
    arch = os.path.join(work, "archive")
    os.makedirs(os.path.join(proj, "src"), exist_ok=True)
    os.makedirs(home, exist_ok=True)
    for i in range(20):
        with open(os.path.join(proj, "src", "seed%02d.txt" % i), "w") as f:
            f.write("seed %d\n" % i)
    with open(os.path.join(proj, "src", "target.txt"), "w") as f:
        f.write("start\n")
    return home, proj, arch


def set_config(bin_path, home, archive, **kv):
    for k, v in kv.items():
        run(bin_path, home, archive, "config", "set", k, json.dumps(v) if isinstance(v, bool) else str(v))


def wait_ready(archive, pid, expect_watch, timeout=20.0):
    t0 = time.time()
    while time.time() - t0 < timeout:
        st = watch_state(archive)
        if st.get("pid") == pid and st.get("periodicCycles", 0) >= 1:
            if not expect_watch or st.get("watchedDirs", 0) > 0:
                return st
        time.sleep(0.05)
    return watch_state(archive)


def phase(bin_path, work, interval, mode, runs, label):
    """One phase: a fresh fixture, a fixed interval, one trigger mode, `runs` measured changes.

    The moment of each write is drawn uniformly from the interval, with a declared seed. Writing at a
    fixed offset after the previous detection would lock onto the daemon's cycle phase and report a
    latency the interval happens to produce for that one offset — a measurement of the instrument, not
    of the program.
    """
    home, proj, arch = build_fixture(os.path.join(work, "phase-" + label))
    r = run(bin_path, home, arch, "init-archive", arch)
    if r.returncode != 0:
        raise SystemExit("init-archive failed: %s %s" % (r.stdout, r.stderr))
    set_config(bin_path, home, arch, stopFreePercent=0, warnFreePercent=0)
    r = run(bin_path, home, arch, "add", proj, "--name", "demo", "--yes")
    if r.returncode != 0:
        raise SystemExit("add failed: %s %s" % (r.stdout, r.stderr))
    rng = random.Random(SEED)
    target = os.path.join(proj, "src", "target.txt")
    notify = mode != "periodic-only"
    set_config(bin_path, home, arch, watchTriggers=notify, partialPass=(mode != "notify-full"),
               intervalSeconds=interval, autoInterval=False, debounceMs=1500)
    env = dict(os.environ)
    env["PROJECTLIFE_HOME"] = home
    if not notify:
        env["PROJECTLIFE_DROP_EVENTS"] = "0"
    proc = subprocess.Popen([bin_path, "--archive", arch, "daemon", "run"], stdout=subprocess.DEVNULL,
                            stderr=subprocess.DEVNULL, env=env)
    st = wait_ready(arch, proc.pid, notify)
    latencies = []
    try:
        for k in range(runs):
            # a uniform offset inside the interval: the honest sampling of "a change at any moment"
            time.sleep(rng.random() * interval)
            body = ("%s change %d at %.3f\n" % (label, k, time.time())).encode()
            t0 = time.time()
            with open(target, "wb") as f:
                f.write(body)
            want = sha256_bytes(body)
            # the write is the clock start; the stop is the moment the version is in the journal
            deadline = t0 + interval + 15
            while time.time() < deadline:
                if any(p == "src/target.txt" and h == want for p, h in puts(arch)):
                    break
                time.sleep(0.02)
            else:
                latencies.append(None)
                continue
            latencies.append(round((time.time() - t0) * 1000.0, 1))
            # do not let the next change hide behind the previous one's debounce window
            time.sleep(min(0.4, interval / 4.0))
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=15)
        except Exception:
            proc.kill()
            proc.wait()
    st = watch_state(arch)
    meta = project_meta(arch)
    iv = meta.get("observedIntervalMs") or {}
    good = sorted(x for x in latencies if x is not None)

    def pct(v, p):
        if not v:
            return None
        i = min(len(v) - 1, int(round((len(v) - 1) * p)))
        return v[i]

    return {
        "label": label,
        "intervalSeconds": interval,
        "trigger": mode,
        "latencies_ms": latencies,
        "latency_median_ms": pct(good, 0.5),
        "latency_p95_ms": pct(good, 0.95),
        "latency_max_ms": good[-1] if good else None,
        "lost_samples": sum(1 for x in latencies if x is None),
        "observed_interval_ms": {
            "median": iv.get("median"),
            "p95": iv.get("p95"),
            "samples": iv.get("samples"),
        },
        "trigger_state": {k: st.get(k) for k in
                          ("mode", "watchedDirs", "events", "triggerCycles", "partialCycles",
                           "periodicCycles", "overflows", "lostEvents", "watchRefusals")},
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default="/work/projectlife/target/release/projectlife")
    ap.add_argument("--work", default="/tmp/pl-latency")
    ap.add_argument("--quick", action="store_true", help="fewer samples (for a smoke run)")
    ap.add_argument("--no-slow", action="store_true",
                    help="skip the 30 s periodic-only phase (the expensive one): the quick+no-slow "
                         "combination still measures both trigger modes and proves the latency does "
                         "not scale with the interval")
    args = ap.parse_args()

    global BIN
    BIN = args.bin
    n_short = 4 if args.quick else 8
    n_long = 2 if args.quick else 4
    result = {"bin": args.bin, "work": args.work, "seed": SEED, "phases": []}
    plan = [(5, "periodic-only", n_short), (5, "notify-partial", n_short), (5, "notify-full", n_short),
            (30, "periodic-only", n_long), (30, "notify-partial", n_long)]
    if args.no_slow:
        plan = [p for p in plan if not (p[0] == 30 and p[1] == "periodic-only")]
    for interval, mode, runs in plan:
        label = "%ds-%s" % (interval, mode)
        ph = phase(args.bin, args.work, interval, mode, runs, label)
        ph["write_offset"] = "uniform in [0, interval), seed %d" % SEED
        result["phases"].append(ph)
        print("%-22s latency median %8s ms  p95 %8s ms  max %8s ms   observed interval median %8s ms  %s"
              % (label, ph["latency_median_ms"], ph["latency_p95_ms"], ph["latency_max_ms"],
                 ph["observed_interval_ms"]["median"], ph["trigger_state"]))
        print("                   samples: %s" % ph["latencies_ms"])

    with open(os.path.join(args.work, "latency.json"), "w") as f:
        json.dump(result, f, indent=2)
    print("raw: %s" % os.path.join(args.work, "latency.json"))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
