#!/usr/bin/env python3
"""Project Life — standalone recovery, without the program.

This script reads an archive directly: the journal is the truth, blobs are the contents.
It shares no code with the main program and is deliberately written from the storage format
description, so it doubles as an independent check of what the program wrote.

Commands:
    list                     show the projects in the archive
    tree --project P --at T  list the files of project P at moment T
    export-file --project P --path REL --at T --out FILE
    export-tree --project P --at T --out DIR
    check [--project P] [--deep]     verify journal references and blob hashes
    events --project P [--limit N]   print journal events

Time spec for --at: epoch ms, "YYYY-MM-DD HH:MM[:SS]", "YYYY-MM-DD", "10m ago", "2h ago",
"yesterday 14:00", "today 09:00", or "seq:NNN".

Only the Python 3 standard library is used.
"""

import argparse
import hashlib
import json
import os
import shutil
import sys
import time
from datetime import datetime, timedelta

SCHEMA_VERSION = 1


def now_ms():
    return int(time.time() * 1000)


def parse_at(spec, now=None):
    """Turn a time specification into epoch milliseconds (local time, like the program)."""
    if spec is None:
        raise SystemExit("--at is required")
    s = spec.strip().strip('"')
    now = now if now is not None else now_ms()
    low = s.lower()
    if low == "now":
        return now
    if low.startswith("seq:"):
        raise SystemExit("seq: must be resolved by the caller (use events/put lookup)")
    if low.endswith("ago"):
        rest = low[: -len("ago")].strip()
        num, unit = "", ""
        for ch in rest:
            if (ch.isdigit() or ch == ".") and not unit:
                num += ch
            elif not ch.isspace():
                unit += ch
        mult = {
            "s": 1000, "sec": 1000, "secs": 1000, "second": 1000, "seconds": 1000,
            "m": 60000, "min": 60000, "mins": 60000, "minute": 60000, "minutes": 60000,
            "h": 3600000, "hour": 3600000, "hours": 3600000,
            "d": 86400000, "day": 86400000, "days": 86400000,
        }.get(unit)
        if mult is None:
            raise SystemExit("unknown time unit: %r" % unit)
        return now - int(float(num) * mult)
    if low == "yesterday":
        return now - 86400000
    base = None
    if low.startswith("yesterday "):
        base = datetime.fromtimestamp(now / 1000.0) - timedelta(days=1)
        s = s[len("yesterday "):].strip()
    elif low.startswith("today "):
        base = datetime.fromtimestamp(now / 1000.0)
        s = s[len("today "):].strip()
    if s.isdigit() and len(s) >= 13:
        return int(s)
    for fmt in ("%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M",
                "%Y-%m-%d", "%d.%m.%Y %H:%M", "%d.%m.%Y"):
        try:
            dt = datetime.strptime(s, fmt)
            return int(dt.timestamp() * 1000)
        except ValueError:
            continue
    for fmt in ("%H:%M:%S", "%H:%M"):
        try:
            t = datetime.strptime(s, fmt)
        except ValueError:
            continue
        ref = base if base is not None else datetime.fromtimestamp(now / 1000.0)
        dt = ref.replace(hour=t.hour, minute=t.minute, second=t.second, microsecond=0)
        return int(dt.timestamp() * 1000)
    raise SystemExit("cannot parse time: %r" % spec)


class Archive:
    def __init__(self, root):
        self.root = os.path.abspath(root)
        if not os.path.isdir(os.path.join(self.root, "projects")):
            raise SystemExit("%s does not look like an archive (no projects/)" % self.root)

    def projects(self):
        base = os.path.join(self.root, "projects")
        out = []
        for name in sorted(os.listdir(base)):
            d = os.path.join(base, name)
            meta = os.path.join(d, "project.json")
            if os.path.isfile(meta):
                with open(meta, "r", encoding="utf-8") as fh:
                    out.append((name, json.load(fh), d))
        return out

    def find(self, name_or_id):
        for pid, meta, d in self.projects():
            if name_or_id in (pid, meta.get("projectId"), meta.get("name")):
                return pid, meta, d
        for pid, meta, d in self.projects():
            if meta.get("name", "").startswith(name_or_id):
                return pid, meta, d
        raise SystemExit("project not found: %s" % name_or_id)


def load_events(project_dir):
    """Read the journal. A half-written last line is ignored; mid-file garbage is reported."""
    edir = os.path.join(project_dir, "events")
    events = []
    if not os.path.isdir(edir):
        return events
    for name in sorted(os.listdir(edir)):
        if not name.endswith(".jsonl"):
            continue
        path = os.path.join(edir, name)
        with open(path, "r", encoding="utf-8", errors="replace") as fh:
            lines = fh.read().split("\n")
        for i, line in enumerate(lines):
            line = line.strip()
            if not line:
                continue
            try:
                ev = json.loads(line)
            except Exception:
                if i == len(lines) - 1 or all(not x.strip() for x in lines[i + 1:]):
                    # trailing partial line after a crash: ignored
                    break
                raise SystemExit("journal corrupted in the middle: %s line %d" % (path, i + 1))
            events.append(ev)
    events.sort(key=lambda e: e.get("seq", 0))
    return events


def state_at(events, t_ms):
    """Apply events with ts <= T in ascending seq order."""
    state = {}
    for ev in events:
        if ev.get("ts", 0) > t_ms:
            continue
        t = ev.get("type")
        if t == "put":
            state[ev["path"]] = {"hash": ev.get("hash", ""), "size": ev.get("size", 0),
                                 "mode": ev.get("mode", 0o644), "kind": "file", "target": None,
                                 "ts": ev.get("ts")}
        elif t == "symlink":
            state[ev["path"]] = {"hash": "", "size": 0, "mode": ev.get("mode", 0o777),
                                 "kind": "symlink", "target": ev.get("target"), "ts": ev.get("ts")}
        elif t == "delete":
            state.pop(ev.get("path"), None)
        elif t == "move":
            frm, to = ev.get("from"), ev.get("to")
            if frm in state and to:
                v = state.pop(frm)
                v["ts"] = ev.get("ts")
                state[to] = v
    return state


def blob_path(project_dir, h):
    return os.path.join(project_dir, "blobs", h[0:2], h[2:4], h)


def read_blob(project_dir, h):
    """Read a blob and verify its sha256 — an unverified blob is never written out."""
    p = blob_path(project_dir, h)
    with open(p, "rb") as fh:
        data = fh.read()
    got = hashlib.sha256(data).hexdigest()
    if got != h:
        raise SystemExit("blob %s is corrupted (sha256 %s)" % (h, got))
    return data


def cmd_list(args):
    arch = Archive(args.archive)
    for pid, meta, _d in arch.projects():
        print("%-24s %-12s %s" % (meta.get("name", pid), meta.get("state", "?"),
                                  meta.get("projectRoot", "")))
    return 0


def cmd_events(args):
    arch = Archive(args.archive)
    _pid, _meta, d = arch.find(args.project)
    events = load_events(d)
    if args.seq is not None:
        for ev in events:
            if ev.get("seq") == args.seq:
                print(json.dumps(ev, indent=2, ensure_ascii=False))
                return 0
        raise SystemExit("no event with seq %d" % args.seq)
    limit = args.limit or 40
    for ev in events[-limit:]:
        print(json.dumps(ev, ensure_ascii=False))
    return 0


def resolve_moment(events, spec):
    if spec and spec.startswith("seq:"):
        want = int(spec.split(":", 1)[1])
        for ev in events:
            if ev.get("seq") == want:
                return ev.get("ts"), spec
        raise SystemExit("no event with %s" % spec)
    return parse_at(spec), spec


def cmd_tree(args):
    arch = Archive(args.archive)
    _pid, _meta, d = arch.find(args.project)
    events = load_events(d)
    t, label = resolve_moment(events, args.at)
    st = state_at(events, t)
    items = sorted(st.items())
    if args.json:
        print(json.dumps([{"path": k, "size": v["size"], "hash": v["hash"], "kind": v["kind"]}
                          for k, v in items], indent=2))
        return 0
    print("%s at %s (%s)" % (args.project, datetime.fromtimestamp(t / 1000.0), label))
    for k, v in items:
        print("%10d %s %s%s" % (v["size"], v["hash"][:12], "-> " if v["kind"] == "symlink" else "", k))
    print("%d files" % len(items))
    return 0


def cmd_export_file(args):
    arch = Archive(args.archive)
    _pid, _meta, d = arch.find(args.project)
    events = load_events(d)
    t, _label = resolve_moment(events, args.at)
    st = state_at(events, t)
    rel = args.path.replace("\\", "/").lstrip("./")
    if rel not in st:
        raise SystemExit("path %s did not exist at that moment" % rel)
    data = read_blob(d, st[rel]["hash"])
    with open(args.out, "wb") as fh:
        fh.write(data)
    print("wrote %s (%d bytes, sha256 verified)" % (args.out, len(data)))
    return 0


def cmd_export_tree(args):
    arch = Archive(args.archive)
    _pid, _meta, d = arch.find(args.project)
    events = load_events(d)
    t, _label = resolve_moment(events, args.at)
    st = state_at(events, t)
    written = 0
    skipped = []
    for rel, v in sorted(st.items()):
        target = os.path.join(args.out, rel)
        if not os.path.abspath(target).startswith(os.path.abspath(args.out)):
            skipped.append(rel)
            continue
        os.makedirs(os.path.dirname(target), exist_ok=True)
        if v["kind"] == "symlink":
            skipped.append(rel)
            continue
        data = read_blob(d, v["hash"])
        with open(target, "wb") as fh:
            fh.write(data)
        try:
            os.chmod(target, v["mode"] & 0o7777)
        except OSError:
            pass
        written += 1
    print("exported %d files to %s (%d skipped)" % (written, args.out, len(skipped)))
    return 0


def cmd_check(args):
    arch = Archive(args.archive)
    worst = 0
    targets = arch.projects()
    if args.project:
        _pid, _meta, d = arch.find(args.project)
        targets = [(args.project, {}, d)]
    for pid, meta, d in targets:
        events = load_events(d)
        referenced = {}
        for ev in events:
            if ev.get("type") == "put" and ev.get("hash"):
                referenced[ev["hash"]] = referenced.get(ev["hash"], 0) + 1
        missing = []
        corrupt = []
        total = 0
        for h in sorted(referenced):
            p = blob_path(d, h)
            if not os.path.isfile(p):
                missing.append(h)
                continue
            total += os.path.getsize(p)
            if args.deep:
                with open(p, "rb") as fh:
                    data = fh.read()
                if hashlib.sha256(data).hexdigest() != h:
                    corrupt.append(h)
        present = set()
        bdir = os.path.join(d, "blobs")
        for a in os.listdir(bdir) if os.path.isdir(bdir) else []:
            pa = os.path.join(bdir, a)
            if not os.path.isdir(pa):
                continue
            for b in os.listdir(pa):
                pb = os.path.join(pa, b)
                if not os.path.isdir(pb):
                    continue
                for h in os.listdir(pb):
                    present.add(h)
        dangling = sorted(present - set(referenced))
        name = meta.get("name", pid)
        print("%s: %d versions, %d bytes" % (name, len(referenced), total))
        if missing:
            print("  MISSING BLOBS: %d (e.g. %s)" % (len(missing), missing[0]))
            worst = 1
        if corrupt:
            print("  CORRUPTED BLOBS: %d (e.g. %s)" % (len(corrupt), corrupt[0]))
            worst = 1
        if dangling:
            print("  dangling blobs: %d (harmless, safe to delete)" % len(dangling))
        if not missing and not corrupt:
            print("  every version the journal references is present%s"
                  % (" and hashes correctly" if args.deep else ""))
    return worst


def main():
    ap = argparse.ArgumentParser(description="Project Life standalone recovery (Python 3 stdlib only)")
    ap.add_argument("--archive", required=True, help="archive root")
    sub = ap.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("list")
    p.set_defaults(func=cmd_list)

    p = sub.add_parser("tree")
    p.add_argument("--project", required=True)
    p.add_argument("--at", required=True)
    p.add_argument("--json", action="store_true")
    p.set_defaults(func=cmd_tree)

    p = sub.add_parser("events")
    p.add_argument("--project", required=True)
    p.add_argument("--limit", type=int, default=40)
    p.add_argument("--seq", type=int)
    p.set_defaults(func=cmd_events)

    p = sub.add_parser("export-file")
    p.add_argument("--project", required=True)
    p.add_argument("--path", required=True)
    p.add_argument("--at", required=True)
    p.add_argument("--out", required=True)
    p.set_defaults(func=cmd_export_file)

    p = sub.add_parser("export-tree")
    p.add_argument("--project", required=True)
    p.add_argument("--at", required=True)
    p.add_argument("--out", required=True)
    p.set_defaults(func=cmd_export_tree)

    p = sub.add_parser("check")
    p.add_argument("--project")
    p.add_argument("--deep", action="store_true")
    p.set_defaults(func=cmd_check)

    args = ap.parse_args()
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
