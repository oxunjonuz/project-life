#!/usr/bin/env python3
"""What the interface server does when it cannot have a socket — measured, not asserted.

The owner's Mac refused it the only socket it asked for:

    projectlife-ui: cannot listen on 127.0.0.1:0: Operation not permitted

Every path that round 296 added is exercised here, on Linux, against the real binary:

  1. the port ladder — an explicit port first, then its neighbours, then the kernel's own choice
  2. a busy port and a *refused* port are told apart, in the report and in the log
  3. the plain refusal: exit 3, JSON on stdout, the reason in the log, no generic sentence anywhere
  4. the handoff: the app binds in its own process and passes the descriptor over a unix socket;
     the child then serves real HTTP on it (checked with a real request, not with a log line)
  5. the same refusal produced by the *kernel*: `tools/bind_deny` installs a seccomp filter that
     makes bind() fail with EPERM, so the errno is not mine and not simulated by my own code
  6. `--diagnose` names the layer that refused, and says whether the handoff is available
  7. an inherited descriptor is verified before it is used, and a bad one is refused loudly

Usage:
    python3 tools/ui_bind_test.py --app app/target/release/projectlife-ui \
                                  --pl target/release/projectlife [--deny tools/bind_deny]
Exit code 0 only if every case passed.
"""

import argparse
import http.client
import json
import os
import select
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

PASS = 0
FAIL = 0
NOTES = []


def check(name, ok, detail=""):
    global PASS, FAIL
    if ok:
        PASS += 1
        print(f"  PASS: {name}" + (f" — {detail}" if detail else ""))
    else:
        FAIL += 1
        print(f"  FAIL: {name}" + (f" — {detail}" if detail else ""))
    return ok


def read_line(fd, timeout=20.0):
    """One line from a pipe, or '' if it never comes. No threads, no hangs."""
    buf = b""
    deadline = time.time() + timeout
    while b"\n" not in buf:
        left = deadline - time.time()
        if left <= 0:
            return buf.decode("utf-8", "replace")
        r, _, _ = select.select([fd], [], [], min(0.5, left))
        if not r:
            continue
        chunk = os.read(fd, 65536)
        if not chunk:
            break
        buf += chunk
    return buf.decode("utf-8", "replace")


def wait_exit(proc, timeout=25.0):
    try:
        proc.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()
        NOTES.append("a child had to be killed after the timeout")
    return proc.returncode


def start(cmd, env=None, pass_fds=(), cwd=None):
    e = dict(os.environ)
    if env:
        e.update(env)
    return subprocess.Popen(
        cmd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=e,
        pass_fds=pass_fds,
        cwd=cwd,
        text=False,
    )


def http_get(port, path, token=None, method="GET", body=None):
    c = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
    headers = {}
    if token:
        headers["X-PL-Token"] = token
    if body is not None:
        headers["Content-Type"] = "application/json"
    c.request(method, path, body=body, headers=headers)
    r = c.getresponse()
    data = r.read()
    c.close()
    return r.status, data


def hold_port(start_at=7717, count=1):
    """Occupy the first `count` ports of the ladder, exactly as another program would."""
    held = []
    p = start_at
    while len(held) < count:
        s = socket.socket()
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        try:
            s.bind(("127.0.0.1", p))
            s.listen(4)
            held.append(s)
        except OSError:
            s.close()
            start_at = p + 1
        p += 1
    return held


class FakeApp:
    """Plays the part of Project Life.app: it opens the unix socket the child will ask on, binds a
    TCP listener in *its own* process, and hands the descriptor over with SCM_RIGHTS — the mechanism
    the Objective-C shell uses. Note the order: the app listens first, the child only connects. On the
    machine where this went wrong, `bind` is the refused call, so the refused process is the last
    process that should be asked to bind anything."""

    def __init__(self, ipc_path, grant=True):
        self.path = Path(ipc_path)
        self.grant = grant
        self.sock = None
        self.unix = None
        self.port = None
        self.request_line = None
        self.error = None
        self.refusal = "the app could not bind either: Operation not permitted (errno 1)"

    def bind(self):
        s = socket.socket()
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        s.bind(("127.0.0.1", 0))
        s.listen(16)
        self.sock = s
        self.port = s.getsockname()[1]
        return self.port

    def listen_unix(self):
        if self.path.exists():
            self.path.unlink()
        self.path.parent.mkdir(parents=True, exist_ok=True)
        u = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        u.bind(str(self.path))
        u.listen(1)
        u.settimeout(25)
        self.unix = u
        return self.path

    def serve_once(self, timeout=25.0):
        if self.unix is None:
            self.listen_unix()
        try:
            conn, _ = self.unix.accept()
        except OSError as e:
            self.error = f"the child never asked for a socket: {e}"
            return
        conn.settimeout(timeout)
        self.request_line = conn.recv(4096).decode("utf-8", "replace")
        if self.grant:
            if self.sock is None:
                self.bind()
            body = json.dumps({"grant": True, "port": self.port}).encode() + b"\n"
            conn.sendmsg([body], [(socket.SOL_SOCKET, socket.SCM_RIGHTS, self.sock.fileno().to_bytes(4, sys.byteorder))])
        else:
            conn.sendmsg([json.dumps({"grant": False, "reason": self.refusal}).encode() + b"\n"])
        conn.close()

def close_all(socks):
    for s in socks:
        try:
            s.close()
        except OSError:
            pass


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--app", required=True, help="the projectlife-ui binary")
    ap.add_argument("--pl", required=True, help="the projectlife core binary")
    ap.add_argument("--deny", default=str(Path(__file__).with_name("bind_deny")),
                    help="the seccomp launcher that makes bind() fail with EPERM")
    args = ap.parse_args()
    tmp = Path(tempfile.mkdtemp(prefix="pl-bind-test-"))
    app, core = str(Path(args.app).resolve()), str(Path(args.pl).resolve())
    deny = str(Path(args.deny).resolve())

    print(f"interface server: {app}")
    print(f"workdir: {tmp}\n")

    # ---------------------------------------------------------------- 1. the ladder
    print("1. An explicit port is asked for first, and a busy one is stepped over")
    held = hold_port(7717, 2)
    log1 = tmp / "log1"
    p = start([app, "--pl", core, "--port", "7717", "--port-range", "3", "--log-dir", str(log1)])
    line = read_line(p.stdout.fileno())
    ready = json.loads(line.strip().splitlines()[-1]) if line.strip() else {}
    check("the server came up on a neighbouring port", ready.get("ready") is True, line.strip()[:160])
    check("it says the ladder chose it", ready.get("how") == "ladder", str(ready.get("how")))
    check("the port is inside the ladder", ready.get("port") in (7719, 7720), str(ready.get("port")))
    busy = [a for a in ready.get("attempts", []) if a.get("kind") == "in_use"]
    check("the busy ports are recorded as busy, with the system's words", len(busy) >= 2,
          json.dumps(busy[:2]))
    if ready.get("port"):
        st, body = http_get(ready["port"], "/")
        check("a real request over the ladder's port is answered", st == 200 and b"<html" in body.lower(),
              f"status {st}")
    p.terminate()
    wait_exit(p)
    close_all(held)

    # ---------------------------------------------------------------- 2. an explicit free port
    print("\n2. With the first port free it is used as asked (no kernel guess)")
    log2 = tmp / "log2"
    p = start([app, "--pl", core, "--port", "7721", "--port-range", "2", "--log-dir", str(log2)])
    line = read_line(p.stdout.fileno())
    ready = json.loads(line.strip().splitlines()[-1]) if line.strip() else {}
    check("the asked-for port is the one served", ready.get("port") == 7721, str(ready.get("port")))
    check("and it is called explicit", ready.get("how") == "explicit", str(ready.get("how")))
    p.terminate()
    wait_exit(p)

    # ---------------------------------------------------------------- 3. the plain refusal
    print("\n3. Every address denied, nobody to ask: exit 3, the real reason in three places")
    log3 = tmp / "log3"
    p = start([deny, app, "--pl", core, "--port", "7717", "--port-range", "2", "--log-dir", str(log3),
               "--no-ipc"])
    out = p.stdout.read().decode()
    err = p.stderr.read().decode()
    rc = wait_exit(p)
    check("it leaves with its own status", rc == 3, f"exit {rc}")
    j = {}
    for l in out.strip().splitlines():
        try:
            j = json.loads(l)
        except ValueError:
            pass
    check("stdout carries a structured refusal", j.get("ready") is False and "error" in j, out.strip()[:120])
    attempts = (j.get("error") or {}).get("attempts", [])
    check("every address is listed with its errno", len(attempts) >= 3 and all(a.get("kind") == "permission" for a in attempts),
          json.dumps(attempts[:1]))
    check("the advice names the difference from a busy port",
          "no port number will help" in (j.get("error") or {}).get("advice", ""),
          (j.get("error") or {}).get("advice", "")[:100])
    check("stderr repeats the system's own words", "Operation not permitted" in err, err.strip().splitlines()[-1][:120])
    logtext = (log3 / "daemon.log").read_text() if (log3 / "daemon.log").exists() else ""
    check("the log has every attempt", logtext.count("refused:") >= 3, f"{logtext.count('refused:')} lines")
    check("the log has the advice and the next step",
          "no port number will help" in logtext and "diagnose" in err, "")
    check("nothing anywhere says 'Reinstall'", "Reinstall" not in (out + err + logtext), "")

    # ---------------------------------------------------------------- 4. the handoff
    print("\n4. Every address denied, the app binds and hands the socket over")
    log4 = tmp / "log4"
    ipc = tmp / "ipc4.sock"
    app_side = FakeApp(ipc)
    app_side.bind()
    app_side.listen_unix()
    p = start([deny, app, "--pl", core, "--port", "7717", "--port-range", "2", "--token", "tok296",
               "--log-dir", str(log4), "--ipc", str(ipc)])
    first = read_line(p.stdout.fileno())
    need = {}
    try:
        need = json.loads(first.strip().splitlines()[-1])
    except ValueError:
        pass
    check("the child asks before it gives up", need.get("needSocket") is True, first.strip()[:160])
    check("it asks on the path it was given", need.get("sock") == str(ipc), str(need.get("sock")))
    check("the request carries the addresses already refused", len(need.get("attempts", [])) >= 3, "")
    app_side.serve_once()
    second = read_line(p.stdout.fileno())
    ready = {}
    try:
        ready = json.loads(second.strip().splitlines()[-1])
    except ValueError:
        pass
    check("the child serves on the app's socket", ready.get("ready") is True and ready.get("how") == "handed-over",
          second.strip()[:160])
    check("the port is the app's port", ready.get("port") == app_side.port,
          f"{ready.get('port')} vs {app_side.port}")
    check("the app saw a request, not a bare connect", "socket" in (app_side.request_line or ""),
          (app_side.request_line or "").strip()[:120])
    st, body = http_get(ready.get("port"), "/")
    check("a real page is served over the handed-over socket", st == 200 and b"<html" in body.lower(), f"status {st}")
    st, body = http_get(ready.get("port"), "/app.js")
    check("and the interface's own script too", st == 200 and b"function" in body, f"status {st}")
    st, _ = http_get(ready.get("port"), "/api/lang", method="POST", body=b'{"lang":"ru"}')
    check("a wrong token over that socket is refused", st == 403, f"status {st}")
    st, body = http_get(ready.get("port"), "/api/lang", token="tok296", method="POST", body=b'{"lang":"ru"}')
    check("the right token is accepted", st == 200 and b"ru" in body, f"status {st}")
    logtext = (log4 / "daemon.log").read_text() if (log4 / "daemon.log").exists() else ""
    check("the log says which path was used", "handed over" in logtext, "")
    p.terminate()
    wait_exit(p)
    app_side.sock.close()

    # ---------------------------------------------------------------- 5. the app refuses too
    print("\n5. When the app cannot bind either, its own words come back")
    log5 = tmp / "log5"
    ipc5 = tmp / "ipc5.sock"
    app_side = FakeApp(ipc5, grant=False)
    app_side.listen_unix()
    p = start([deny, app, "--pl", core, "--port", "7717", "--port-range", "2", "--log-dir", str(log5),
               "--ipc", str(ipc5)])
    read_line(p.stdout.fileno())
    app_side.serve_once()
    out = p.stdout.read().decode()
    rc = wait_exit(p)
    j = {}
    for l in out.strip().splitlines():
        try:
            j = json.loads(l)
        except ValueError:
            pass
    check("the refusal is still reported, with exit 3", rc == 3 and j.get("ready") is False, f"exit {rc}")
    check("the app's own reason is carried, not replaced", 
          "the app could not bind either" in json.dumps(j.get("error", {}).get("handoff", "")), 
          json.dumps(j.get("error", {}).get("handoff", ""))[:120])

    # ---------------------------------------------------------------- 6. the injector
    print("\n6. The same refusal, injected in-process (the route verify.sh uses)")
    log6 = tmp / "log6"
    p = start([app, "--pl", core, "--port", "7717", "--port-range", "1", "--log-dir", str(log6), "--no-ipc"],
              env={"PROJECTLIFE_UI_BIND_DENY": "1"})
    out = p.stdout.read().decode()
    rc = wait_exit(p)
    check("the injected denial takes the same path", rc == 3 and '"kind":"permission"' in out.replace(" ", ""),
          f"exit {rc}")
    logtext = (log6 / "daemon.log").read_text() if (log6 / "daemon.log").exists() else ""
    check("and it says in the log that it was injected, not measured",
          "injected by PROJECTLIFE_UI_BIND_DENY" in logtext, "")

    # ---------------------------------------------------------------- 7. diagnose
    print("\n7. --diagnose names the layer")
    log7 = tmp / "log7"
    p = start([app, "--pl", core, "--log-dir", str(log7), "--diagnose"])
    out = p.stdout.read().decode()
    rc = wait_exit(p)
    check("it reports instead of failing", rc == 0, f"exit {rc}")
    j = {}
    for l in out.strip().splitlines():
        if l.startswith("{"):
            try:
                j = json.loads(l)
            except ValueError:
                pass
    check("the verdict is printed for a person", "verdict:" in out, "")
    d = j.get("diagnose", {})
    check("here TCP listening works and it says so",
          "TCP listening works" in d.get("verdict", ""), d.get("verdict", "")[:120])
    check("and the handoff probe reports both halves working",
          d.get("handoff", {}).get("transport") == "ok" and d.get("handoff", {}).get("descriptor") == "arrived",
          json.dumps(d.get("handoff", {}))[:160])
    check("the report file is written", Path(j.get("file", "/nonexistent")).exists(), str(j.get("file")))
    p = start([deny, app, "--pl", core, "--log-dir", str(log7), "--diagnose"])
    out = p.stdout.read().decode()
    wait_exit(p)
    j = {}
    for l in out.strip().splitlines():
        if l.startswith("{"):
            try:
                j = json.loads(l)
            except ValueError:
                pass
    d = j.get("diagnose", {})
    check("under a kernel refusal it says which call was refused",
          "errno Some(1)" in json.dumps(d.get("binds", {})) or "Operation not permitted" in json.dumps(d.get("binds", {})),
          json.dumps(d.get("binds", {}))[:160])
    # Under a blanket {bind} refusal in *this* process neither the app's half nor the child's
    # path half can be opened here, and the report must not pretend otherwise: it says the receive
    # half works (measured over a socketpair, where no bind is involved) and that whether the app
    # may bind is not measurable from inside this process.
    check("it says this process cannot open any listening socket",
          "cannot open a unix socket either" in d.get("verdict", ""), d.get("verdict", "")[:220])
    check("it refuses to claim the app can or cannot bind",
          "not measurable" in d.get("verdict", ""), d.get("verdict", "")[:220])
    check("the receive half is measured, with no bind involved anywhere in it",
          d.get("pair", {}).get("transport") == "ok" and d.get("pair", {}).get("descriptor") == "arrived",
          json.dumps(d.get("pair", {}))[:160])
    check("the app's half is reported as refused here, with the system's own errno",
          d.get("handoff", {}).get("transport") == "failed" and "Operation not permitted" in json.dumps(d.get("handoff", {})),
          json.dumps(d.get("handoff", {}))[:200])
    check("socket() itself is fine — so the refusal is at bind, not at socket",
          d.get("sockets", {}).get("inet") == "created", json.dumps(d.get("sockets", {})))

    # ---------------------------------------------------------------- 8. an inherited descriptor
    print("\n8. A descriptor the app already bound is verified before it is used")
    log8 = tmp / "log8"
    srv = socket.socket()
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("127.0.0.1", 0))
    srv.listen(8)
    os.set_inheritable(srv.fileno(), True)
    p = start([app, "--pl", core, "--port", "0", "--log-dir", str(log8), "--listen-fd", str(srv.fileno())],
              pass_fds=(srv.fileno(),))
    line = read_line(p.stdout.fileno())
    ready = json.loads(line.strip().splitlines()[-1]) if line.strip() else {}
    check("the inherited socket is used", ready.get("how") == "inherited", str(ready.get("how")))
    check("and it is the app's port", ready.get("port") == srv.getsockname()[1], str(ready.get("port")))
    st, _ = http_get(ready.get("port"), "/")
    check("it serves over it", st == 200, f"status {st}")
    p.terminate()
    wait_exit(p)
    srv.close()

    p = start([app, "--pl", core, "--port", "7722", "--port-range", "1", "--log-dir", str(log8),
               "--listen-fd", "99"])
    line = read_line(p.stdout.fileno())
    ready = json.loads(line.strip().splitlines()[-1]) if line.strip() else {}
    check("a descriptor that is not there is refused, not served on",
          ready.get("how") in ("explicit", "ladder"), str(ready.get("how")))
    logtext = (log8 / "daemon.log").read_text() if (log8 / "daemon.log").exists() else ""
    check("and the log says why the inherited one was dropped",
          "inherited descriptor 99 is not a usable local listener" in logtext, "")
    p.terminate()
    wait_exit(p)

    print(f"\n{PASS} PASS, {FAIL} FAIL")
    if NOTES:
        print("notes: " + "; ".join(NOTES))
    print(f"workdir kept for inspection: {tmp}")
    return 1 if FAIL else 0


if __name__ == "__main__":
    sys.exit(main())
