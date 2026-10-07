"""An MCP client for the Project Life server, written from the protocol, not from the server.

It is deliberately a second implementation: it speaks newline-delimited JSON-RPC to `pl-mcp` over
stdio as an ordinary MCP client does, and it checks three things the server promises.

  1. `tools/list` advertises Level 1 and Level 3 only. Every Level 2 write name must be absent.
  2. A call to a write name is refused (isError), and a call to an unknown name is refused too.
  3. Exercising every registered tool leaves the archive byte-identical except for logs/mcp.log:
     the client hashes every file before and after and prints the difference. This is the check
     that "the reading surface cannot write" is measured, not asserted.

Usage:
  python3 tools/mcp_client.py --archive ARCH --binary BIN [--check-readonly] [--rate-test]

Exit code 0 when every expectation holds, 1 otherwise.
"""
import argparse
import hashlib
import json
import os
import subprocess
import sys
import time

WRITE_NAMES = [
    "pl_restore", "pl_panic", "pl_prune", "pl_export_and_prune", "pl_archive_delete", "pl_import",
    "pl_mark", "pl_pause", "pl_resume", "pl_remove", "pl_scan_once", "pl_doctor_fix_lock", "pl_check_fix",
]


class Client:
    def __init__(self, binary, archive):
        self.p = subprocess.Popen(
            [binary, "--archive", archive],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=True, bufsize=1,
        )
        self._id = 0

    def _send(self, obj):
        self.p.stdin.write(json.dumps(obj) + "\n")
        self.p.stdin.flush()

    def request(self, method, params=None):
        self._id += 1
        rid = self._id
        msg = {"jsonrpc": "2.0", "id": rid, "method": method}
        if params is not None:
            msg["params"] = params
        self._send(msg)
        line = self.p.stdout.readline()
        if not line:
            err = self.p.stderr.read()
            raise RuntimeError("server closed the connection: %s" % err)
        reply = json.loads(line)
        if reply.get("id") != rid:
            raise RuntimeError("out of order reply: %s" % reply)
        return reply

    def notify(self, method, params=None):
        msg = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            msg["params"] = params
        self._send(msg)

    def close(self):
        try:
            self.p.stdin.close()
        except Exception:
            pass
        try:
            self.p.wait(timeout=5)
        except Exception:
            self.p.kill()


def manifest(root):
    out = {}
    for dirpath, dirnames, filenames in os.walk(root):
        for f in filenames:
            p = os.path.join(dirpath, f)
            rel = os.path.relpath(p, root)
            try:
                with open(p, "rb") as fh:
                    out[rel] = hashlib.sha256(fh.read()).hexdigest()
            except OSError:
                out[rel] = "unreadable"
    return out


def first_project(archive):
    d = os.path.join(archive, "projects")
    for name in sorted(os.listdir(d)):
        pj = os.path.join(d, name, "project.json")
        if os.path.isfile(pj):
            with open(pj) as f:
                return json.load(f)["name"], os.path.join(d, name)
    return None, None


def text_of(reply):
    res = reply.get("result", {})
    if isinstance(res, dict) and "content" in res:
        return res["content"][0]["text"]
    return json.dumps(res)[:400]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--archive", required=True)
    ap.add_argument("--binary", default="target/release/pl-mcp")
    ap.add_argument("--check-readonly", action="store_true")
    ap.add_argument("--rate-test", action="store_true")
    args = ap.parse_args()

    failures = []
    before = manifest(args.archive) if args.check_readonly else None

    c = Client(args.binary, args.archive)
    init = c.request("initialize", {"protocolVersion": "2024-11-05", "clientInfo": {"name": "mcp_client.py", "version": "1"}, "capabilities": {}})
    print("initialize -> protocolVersion=%s serverInfo=%s" % (init["result"].get("protocolVersion"), init["result"].get("serverInfo")))
    c.notify("notifications/initialized")

    listing = c.request("tools/list")["result"]["tools"]
    names = [t["name"] for t in listing]
    print("tools/list -> %d tools" % len(names))
    for t in listing:
        print("   %-18s %s" % (t["name"], t["description"][:60]))

    no_write_code = [n for n in names if n in WRITE_NAMES]
    if no_write_code:
        failures.append("tools/list registers write operations: %s" % no_write_code)
    missing = [n for n in ("pl_status", "pl_log", "pl_why", "pl_tree", "pl_diff", "pl_last_good", "pl_check", "pl_doctor", "pl_plan_restore") if n not in names]
    if missing:
        failures.append("tools/list is missing Level 1/3 names: %s" % missing)

    project, proj_dir = first_project(args.archive)
    if project is None:
        print("no project in the archive: only the read-only invariants were tested")
    else:
        calls = [
            ("pl_status", {}),
            ("pl_status", {"project": project}),
            ("pl_log", {"project": project, "limit": 5}),
            ("pl_log", {"project": project, "type": "mass,gap,mark"}),
            ("pl_tree", {"project": project}),
            ("pl_diff", {"project": project, "at": "1h ago"}),
            ("pl_diff", {"project": project, "at": "1h ago", "include_content": True}),
            ("pl_last_good", {"project": project}),
            ("pl_check", {"project": project, "deep": True}),
            ("pl_doctor", {}),
            ("pl_plan_restore", {"project": project, "at": "10m ago"}),
            ("pl_why", {"project": project, "path": "src/app.ts"}),
        ]
        for tool, params in calls:
            reply = c.request("tools/call", {"name": tool, "arguments": params})
            res = reply.get("result", {})
            ok = not res.get("isError", False)
            body = text_of(reply).replace("\n", " ")[:110]
            print("call %-17s %-40s %s  %s" % (tool, json.dumps(params)[:40], "ok" if ok else "ERROR", body))
            if tool == "pl_plan_restore" and ok:
                try:
                    plan = json.loads(res["content"][0]["text"])
                    if plan.get("performed") is not False:
                        failures.append("pl_plan_restore did not report performed=false")
                    if not plan.get("target"):
                        failures.append("pl_plan_restore returned no target")
                except Exception as e:
                    failures.append("pl_plan_restore did not return JSON: %s" % e)

    # 2. a write name must be refused, and an invented name too
    for bad in ("pl_restore", "pl_prune", "definitely_not_a_tool"):
        reply = c.request("tools/call", {"name": bad, "arguments": {"project": project or "x", "before": "30d ago"}})
        res = reply.get("result", {})
        refused = res.get("isError", False)
        print("call %-22s refused=%s  %s" % (bad, refused, text_of(reply)[:80]))
        if not refused:
            failures.append("a call to %s was NOT refused" % bad)

    if args.rate_test:
        # A burst is only a test of the limiter if this client can actually outrun it. If the client
        # itself is slower than 10 calls/second, no refusal can happen and saying "failed" would be
        # measuring the harness, not the server. So the burst is measured and the outcome reported
        # honestly; the deterministic proof lives in the in-crate test
        # `mcp_rate_limiter_bites_and_recovers`, which calls the server in-process at full speed.
        burst = 25
        t0 = time.monotonic()
        got = []
        for i in range(burst):
            reply = c.request("tools/call", {"name": "pl_status", "arguments": {"project": project or "x"}})
            got.append(reply.get("result", {}).get("isError", False))
        dt = time.monotonic() - t0
        rate = burst / dt if dt > 0 else float("inf")
        refused = [i + 1 for i, r in enumerate(got) if r]
        print("rate test: %d calls in %.2f s (%.1f calls/s), refusals at positions %s"
              % (burst, dt, rate, refused or "none"))
        if refused:
            print("   the limiter refused calls inside a burst faster than its limit")
        elif rate < 10.0:
            print("   the limiter was NOT exercised: this client is slower (%.1f calls/s) than the 10/s limit" % rate)
        else:
            failures.append("the client sustained %.1f calls/s and the limiter refused none of %d calls" % (rate, burst))
        time.sleep(1.1)
        reply = c.request("tools/call", {"name": "pl_status", "arguments": {"project": project or "x"}})
        if reply.get("result", {}).get("isError", False):
            failures.append("the rate limiter did not recover after a second")
        else:
            print("   the limiter accepts calls again one second later")

    c.close()

    if args.check_readonly:
        # The one file the server may touch.
        after = manifest(args.archive)
        changed = sorted(set(k for k in set(before) | set(after) if before.get(k) != after.get(k)))
        allowed = [c for c in changed if c == os.path.join("logs", "mcp.log")]
        unexpected = [c for c in changed if c not in allowed]
        print("read-only check: %d file(s) changed, allowed: %s, unexpected: %s" % (len(changed), allowed, unexpected or "none"))
        if unexpected:
            failures.append("tools changed the archive: %s" % unexpected)
        if not changed:
            failures.append("the read-only check saw no change at all, not even logs/mcp.log: it is not measuring")

    print()
    if failures:
        print("MCP CLIENT: FAILED")
        for f in failures:
            print("  - %s" % f)
        return 1
    print("MCP CLIENT: ALL EXPECTATIONS HELD")
    return 0


if __name__ == "__main__":
    sys.exit(main())
