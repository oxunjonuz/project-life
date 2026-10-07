#!/usr/bin/env python3
"""Ask the core itself which commands can be spoken to in JSON - the raw material for the
UI-coverage table. Not a report: every line is a real run with its real exit code."""
import json, os, shutil, subprocess, sys, datetime

ROOT = "/work/projectlife/tmp/audit297"
BIN = "/work/projectlife/target/release/projectlife"
ARCH = f"{ROOT}/archive"
FILES = f"{ROOT}/files"

def run(args, timeout=120):
    p = subprocess.run([BIN] + args, capture_output=True, text=True, timeout=timeout,
                       env={**os.environ, "PROJECTLIFE_ARCHIVE": ARCH})
    return p.returncode, p.stdout, p.stderr

def jshape(out):
    t = out.strip()
    if not t:
        return "empty"
    lines = t.split("\n")
    for i in range(0, min(len(lines), 8)):
        chunk = "\n".join(lines[i:])
        try:
            v = json.loads(chunk)
        except Exception:
            continue
        if isinstance(v, list):
            return f"json-array[{len(v)}]"
        if isinstance(v, dict):
            return "json-object{" + ",".join(list(v.keys())[:6]) + "}"
        return f"json-{type(v).__name__}"
    return "not-json"

def main():
    if os.path.isdir(ROOT):
        shutil.rmtree(ROOT)
    os.makedirs(f"{FILES}/src", exist_ok=True)
    os.makedirs(f"{FILES}/docs", exist_ok=True)
    open(f"{FILES}/src/a.txt", "w").write("one\n")
    open(f"{FILES}/docs/b.md", "w").write("# two\n")

    print("SETUP:", run(["init-archive", ARCH])[0], run(["add", FILES, "--name", "t", "--preset", "auto", "--yes"])[0])
    open(f"{FILES}/src/a.txt", "w").write("one changed\n")
    print("SCAN :", run(["scan-once", "t"])[0])
    open(f"{FILES}/src/a.txt", "w").write("one changed again\n")
    open(f"{FILES}/src/c.txt", "w").write("three\n")
    print("SCAN :", run(["scan-once", "t"])[0])

    rc, out, _ = run(["log", "t", "--json"])
    events = json.loads(out)
    ts = sorted({e["ts"] for e in events if e.get("type") in ("put", "delete", "move")})
    m1 = ts[0]; m2 = ts[-1]
    def iso(ms):
        return datetime.datetime.utcfromtimestamp(ms / 1000).strftime("%Y-%m-%dT%H:%M:%S.") + f"{ms % 1000:03d}Z"

    probes = [
        ("status --json", ["status", "--json"]),
        ("list --json", ["list", "--json"]),
        ("log <p> --json", ["log", "t", "--json"]),
        ("log <p> --grep --json", ["log", "t", "--grep", "changed", "--json"]),
        ("log <p> --content --json", ["log", "t", "--content", "changed", "--json"]),
        ("tree --at --json", ["tree", "t", "--at", iso(m2), "--json"]),
        ("diff --at --to --json", ["diff", "t", "--at", iso(m1), "--to", iso(m2), "--json"]),
        ("diff --at --current --json", ["diff", "t", "--at", iso(m1), "--current", "--json"]),
        ("cat --path --json", ["cat", "t", "--path", "src/a.txt", "--json"]),
        ("cat --path --at --json", ["cat", "t", "--path", "src/a.txt", "--at", iso(m1), "--json"]),
        ("why <p> <path> --json", ["why", "t", "src/a.txt", "--json"]),
        ("why --at --json", ["why", "t", "--at", iso(m2), "--json"]),
        ("last-good --json", ["last-good", "t", "--json"]),
        ("blame <p> <path> --json", ["blame", "t", "src/a.txt", "--json"]),
        ("since --json", ["since", "t", "--json"]),
        ("recent --json", ["recent", "--json"]),
        ("size --json", ["size", "--json"]),
        ("gc --dry-run --json", ["gc", "--dry-run", "--json"]),
        ("retention <p> --json", ["retention", "t", "--json"]),
        ("check --json", ["check", "--json"]),
        ("check <p> --json", ["check", "t", "--json"]),
        ("check --deep --json", ["check", "t", "--deep", "--json"]),
        ("rebuild-cache --json", ["rebuild-cache", "--json"]),
        ("doctor --json", ["doctor", "--json"]),
        ("healthcheck --json", ["healthcheck", "--json"]),
        ("heartbeat-check --json", ["heartbeat-check", "--json"]),
        ("audit-archive --json", ["audit-archive", "--json"]),
        ("drill <p> --json", ["drill", "t", "--json"]),
        ("recover <p> --json", ["recover", "t", "--json"]),
        ("quarantine --json", ["quarantine", "--json"]),
        ("apply-filters --json", ["apply-filters", "t", "--json"]),
        ("restore --preview --json", ["restore", "t", "--at", iso(m2), "--preview", "--json"]),
        ("panic --json", ["panic", "t", "--json"]),
        ("mark <p> L --json", ["mark", "t", "L1", "--json"]),
        ("snap <p> --json", ["snap", "t", "--json"]),
        ("note <p> txt --json", ["note", "t", "hello", "--json"]),
        ("undo <p> --json", ["undo", "t", "--json"]),
        ("suggest --json", ["suggest", "t", "--json"]),
        ("prompt --json", ["prompt", "--json"]),
        ("presets --json", ["presets", "--json"]),
        ("detect --json", ["detect", FILES, "--json"]),
        ("mcp tools", ["mcp", "tools"]),
        ("config get --json", ["config", "get", "--json"]),
        ("export-and-prune (no args)", ["export-and-prune", "--json"]),
        ("archive-delete (no args)", ["archive-delete", "--json"]),
    ]
    rows = []
    for label, args in probes:
        try:
            rc, out, e = run(args)
        except subprocess.TimeoutExpired:
            rows.append((label, "TIMEOUT", "-", "")); print(f"{label:32} TIMEOUT"); continue
        shape = jshape(out)
        rows.append((label, rc, shape, (e or out).strip().split("\n")[0][:90]))
        print(f"{label:32} rc={rc:<3} {shape:52} {rows[-1][3]}")

    with open(f"{ROOT}/probe.json", "w") as f:
        json.dump({"json_support": [{"cmd": r[0], "exit": r[1], "shape": r[2], "first_line": r[3]} for r in rows],
                   "moments": {"m1": iso(m1), "m2": iso(m2)}}, f, indent=1)
    print("\nwrote", f"{ROOT}/probe.json")

if __name__ == "__main__":
    main()
