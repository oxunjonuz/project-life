"""Spread a project's journal over time, so that an age-based policy has something to thin.

A verification tool, not part of the program: it rewrites the `ts` of every event (order preserved)
and moves `historyStartsAt` with it, because a project whose events predate its own start of history
rightly refuses to restore them.

Usage: python3 tools/backdate.py <project-dir> <span-days> [<ago-days>]
"""
import json
import os
import sys
import time

DAY = 86_400_000
proj_dir = sys.argv[1]
span_days = float(sys.argv[2])
ago_days = float(sys.argv[3]) if len(sys.argv) > 3 else 0.0
now = int(time.time() * 1000) - int(ago_days * DAY)
events_dir = os.path.join(proj_dir, "events")


def month_name(ms):
    t = time.localtime(ms / 1000.0)
    return "%04d-%02d" % (t.tm_year, t.tm_mon)


def iso(ms):
    t = time.gmtime(ms / 1000.0)
    return "%04d-%02d-%02dT%02d:%02d:%02d.%03dZ" % (t.tm_year, t.tm_mon, t.tm_mday, t.tm_hour, t.tm_min, t.tm_sec, ms % 1000)


evs = []
for name in sorted(os.listdir(events_dir)):
    if not name.endswith(".jsonl"):
        continue
    with open(os.path.join(events_dir, name), encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line:
                evs.append(json.loads(line))
evs.sort(key=lambda e: e["seq"])
n = len(evs)
if n == 0:
    raise SystemExit("no events in %s" % events_dir)
for i, e in enumerate(evs):
    frac = i / max(1, n - 1)
    e["ts"] = now - int((span_days * DAY) * (1.0 - frac))

for name in os.listdir(events_dir):
    if name.endswith(".jsonl"):
        os.remove(os.path.join(events_dir, name))
by_month = {}
for e in evs:
    by_month.setdefault(month_name(e["ts"]), []).append(json.dumps(e))
for m, lines in by_month.items():
    with open(os.path.join(events_dir, m + ".jsonl"), "w", encoding="utf-8") as f:
        f.write("\n".join(lines) + "\n")

pj = os.path.join(proj_dir, "project.json")
with open(pj, encoding="utf-8") as f:
    meta = json.load(f)
oldest = min(e["ts"] for e in evs)
meta["historyStartsAt"] = iso(oldest)
with open(pj, "w", encoding="utf-8") as f:
    json.dump(meta, f, indent=2)
print("rewrote %d events over %s days; historyStartsAt=%s" % (n, span_days, meta["historyStartsAt"]))
