#!/usr/bin/env python3
"""Does the state cache say what the journal says? An independent reader of both.

Written from the storage-format description, sharing no code with the program (the journal is JSON
Lines, `cache/base.jsonl` is the same shape sorted by `path`, and `cache/delta.jsonl` holds records
applied in order, last record per path wins, `{"op":"del"}` meaning "no longer tracked").

Four checks, in the order a fault would show up:

  1. the base file is sorted by path and its line count equals `base.meta.json`'s `entries`;
  2. the delta's records are well formed and every one of them carries a path;
  3. after applying the delta over the base, the tracked state must equal the journal's own state
     (the journal is replayed here, independently of the program: snapshot/put/delete/move/symlink);
  4. the count the program would report (`base entries + the transitions the delta declares`) must
     equal the number of paths in the cache.

Exit 0 when the two agree, 1 when they do not (`--quiet` prints only the verdict).
"""
import argparse
import glob
import json
import os
import sys


def read_journal(project_dir):
    events = []
    for f in sorted(glob.glob(os.path.join(project_dir, "events", "*.jsonl"))):
        with open(f) as fh:
            for line in fh:
                line = line.strip()
                if not line:
                    continue
                try:
                    events.append(json.loads(line))
                except Exception:
                    # a half-written trailing line is normal after a crash
                    continue
    events.sort(key=lambda e: e.get("seq", 0))
    return events


def state_at(events):
    """The journal's own view: path -> (kind, hash/target)."""
    st = {}
    for e in events:
        t = e.get("type")
        p = e.get("path")
        if t == "put" and p:
            st[p] = ("file", e.get("hash", ""))
        elif t == "symlink" and p:
            st[p] = ("symlink", e.get("target", ""))
        elif t == "delete" and p:
            st.pop(p, None)
        elif t == "move":
            src, dst = e.get("from"), e.get("to")
            if src in st:
                st[dst] = st.pop(src)
    return st


def read_base(project_dir):
    path = os.path.join(project_dir, "cache", "base.jsonl")
    entries, problems = {}, []
    if not os.path.isfile(path):
        return entries, problems
    prev = None
    with open(path) as fh:
        for n, line in enumerate(fh, 1):
            line = line.strip()
            if not line:
                continue
            try:
                rec = json.loads(line)
            except Exception as ex:
                problems.append("base.jsonl line %d is not JSON: %s" % (n, ex))
                continue
            p = rec.get("path")
            if p is None:
                problems.append("base.jsonl line %d has no path" % n)
                continue
            if prev is not None and p < prev:
                problems.append("base.jsonl is not sorted: %r after %r" % (p, prev))
            prev = p
            entries[p] = rec
    return entries, problems


def read_delta(project_dir):
    path = os.path.join(project_dir, "cache", "delta.jsonl")
    recs, problems = [], []
    if not os.path.isfile(path):
        return recs, problems
    with open(path) as fh:
        for n, line in enumerate(fh, 1):
            line = line.strip()
            if not line:
                continue
            try:
                rec = json.loads(line)
            except Exception:
                # a half-written last line after a crash is tolerated, as in the journal
                continue
            if "path" not in rec:
                problems.append("delta.jsonl line %d has no path" % n)
                continue
            recs.append(rec)
    return recs, problems


def check(project_dir):
    events = read_journal(project_dir)
    want = state_at(events)
    base, problems = read_base(project_dir)
    delta, dproblems = read_delta(project_dir)
    problems.extend(dproblems)

    cache = {}
    transitions = 0
    for rec in delta:
        p = rec["path"]
        if rec.get("op") == "del":
            was = p in cache if p in cache else p in base
            cache.pop(p, None)
            if was:
                transitions -= 1
        else:
            was = p in cache if p in cache else p in base
            cache[p] = rec
            if not was:
                transitions += 1
    merged = dict(base)
    merged.update(cache)

    def kind_hash(rec):
        if rec.get("kind") == "symlink":
            return ("symlink", rec.get("target", ""))
        return ("file", rec.get("hash", ""))

    discrepancies = []
    for p, rec in sorted(merged.items()):
        got = kind_hash(rec)
        exp = want.get(p)
        if exp is None:
            discrepancies.append("%s is in the cache but the journal does not have it" % p)
        elif got != exp:
            discrepancies.append("%s: cache %s, journal %s" % (p, got[1][:12], exp[1][:12]))
    missing = [p for p in want if p not in merged]
    for p in sorted(missing)[:20]:
        discrepancies.append("%s is in the journal but not in the cache" % p)

    meta_path = os.path.join(project_dir, "cache", "base.meta.json")
    entries_meta = None
    if os.path.isfile(meta_path):
        try:
            entries_meta = json.load(open(meta_path)).get("entries")
        except Exception as ex:
            problems.append("base.meta.json is unreadable: %s" % ex)
    if entries_meta is not None and entries_meta != len(base):
        problems.append("base.meta.json says %s entries, the file holds %s" % (entries_meta, len(base)))
    declared = (len(base) if entries_meta is None else entries_meta) + transitions
    if declared != len(merged):
        discrepancies.append("the declared count %d does not equal the number of tracked paths %d" % (declared, len(merged)))

    return {
        "journal_events": len(events),
        "journal_paths": len(want),
        "base_entries": len(base),
        "delta_records": len(delta),
        "tracked": len(merged),
        "problems": problems,
        "discrepancies": discrepancies,
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--archive", required=True)
    ap.add_argument("--project", default="")
    ap.add_argument("--quiet", action="store_true")
    a = ap.parse_args()

    projects = []
    for d in sorted(glob.glob(os.path.join(a.archive, "projects", "*"))):
        if not os.path.isdir(d):
            continue
        name = ""
        try:
            name = json.load(open(os.path.join(d, "project.json"))).get("name", "")
        except Exception:
            pass
        if a.project and name != a.project:
            continue
        projects.append((name, d))
    if not projects:
        print("no project found in %s" % a.archive)
        return 1

    bad = 0
    for name, d in projects:
        r = check(d)
        print("%-12s journal: %d event(s), %d path(s) | cache: %d base entry(ies), %d delta record(s), %d tracked"
              % (name, r["journal_events"], r["journal_paths"], r["base_entries"], r["delta_records"], r["tracked"]))
        for p in r["problems"]:
            print("   FORMAT: %s" % p)
            bad += 1
        for p in r["discrepancies"]:
            print("   DISAGREE: %s" % p)
            bad += 1
    if bad == 0:
        print("CACHE AND JOURNAL AGREE")
    else:
        print("CACHE AND JOURNAL DISAGREE (%d finding(s))" % bad)
    return 0 if bad == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
