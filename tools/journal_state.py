"""An independent reader of a Project Life journal: replay it by hand and print the state at a moment.

This is a verification tool, not part of the program. It is written from the storage-format
description (blob -> journal event -> state) and shares no code with it, so when it agrees with the
program about the state at a moment, that is two implementations agreeing rather than one repeating
itself.

Usage: python3 tools/journal_state.py <project-dir> <path|*> <ts-ms> [<ts-ms> ...]
Prints one line per moment: "<ts> <sha256-prefix|symlink:target|ABSENT>".
"""
import json
import os
import sys

proj = sys.argv[1]
path = sys.argv[2]
moments = [int(x) for x in sys.argv[3:]]

evs = []
d = os.path.join(proj, "events")
for name in sorted(os.listdir(d)):
    if not name.endswith(".jsonl"):
        continue
    for line in open(os.path.join(d, name), encoding="utf-8"):
        line = line.strip()
        if line:
            evs.append(json.loads(line))
evs.sort(key=lambda e: e["seq"])


def state_at(t):
    st = {}
    for e in evs:
        if e["ts"] > t:
            continue
        kind = e["type"]
        if kind == "put":
            st[e["path"]] = (e.get("hash", ""), e.get("size", 0))
        elif kind == "symlink":
            st[e["path"]] = ("symlink:" + (e.get("target") or ""), 0)
        elif kind == "delete":
            st.pop(e.get("path"), None)
        elif kind == "move":
            if e.get("from") in st:
                st[e["to"]] = st.pop(e["from"])
    return st


def show(value):
    if value is None:
        return "ABSENT"
    head, size = value
    if head.startswith("symlink:"):
        return head
    return "%s/%s" % (head[:12], size)


for T in moments:
    st = state_at(T)
    if path == "*":
        blob = " ".join(sorted(st.keys()))
        print("%d %d %s" % (T, len(st), blob))
    else:
        print("%d %s" % (T, show(st.get(path))))
