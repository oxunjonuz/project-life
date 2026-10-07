#!/usr/bin/env python3
"""Which build is this, and is the answer the files' own? (round 298)

The owner's manual macOS test of 2026-10-06 was performed on a bundle that had been replaced hours
before, and nothing on screen said which build he had opened: three of the failures he reported were
three that had already been fixed, and a round went into working that out. The app now answers the
question itself, from the files it is running with.

This check does not trust that answer either. It asks the running interface server for its build
document and then hashes the same files with Python's `hashlib` — a different implementation from
the Rust `sha2` that produced them — and compares:

  * `server.sha256`  with the interface server binary that was launched;
  * `core.sha256`    with the core binary it will call;
  * `page.sha256`    with the page source on disk *and* with the bytes actually served at /app.js.

A number that agrees with itself proves nothing; these agree with the bytes.

    python3 tools/build_identity_check.py --app app/target/release/projectlife-ui \\
        --pl target/release/projectlife --page app/ui/app.js [--shell app/macos/ProjectLife.m]
"""
import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path
from urllib.request import urlopen

PASS = 0
FAIL = 0


def check(name, ok, detail=""):
    global PASS, FAIL
    if ok:
        PASS += 1
        print(f"  PASS: {name}" + (f" — {detail}" if detail else ""))
    else:
        FAIL += 1
        print(f"  FAIL: {name}" + (f" — {detail}" if detail else ""))


def sha256_file(path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(65536), b""):
            h.update(chunk)
    return h.hexdigest()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--app", required=True)
    ap.add_argument("--pl", required=True)
    ap.add_argument("--page", default="")
    ap.add_argument("--shell", default="")
    ap.add_argument("--work", default="/tmp/pl-build-identity")
    ap.add_argument("--token", default="idcheck")
    args = ap.parse_args()

    work = Path(args.work)
    shutil.rmtree(work, ignore_errors=True)
    work.mkdir(parents=True)
    home = work / "home"
    home.mkdir()
    env = dict(os.environ)
    env["PROJECTLIFE_HOME"] = str(home)

    server = subprocess.Popen(
        [args.app, "--pl", args.pl, "--port", "0", "--token", args.token, "--log-dir", str(work / "applog")],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env, text=True,
    )
    try:
        line = server.stdout.readline()
        ready = json.loads(line) if line.strip().startswith("{") else {}
        check("the interface server starts on this machine", ready.get("ready") is True, line.strip()[:120])
        if not ready.get("ready"):
            return 1
        base = f"http://127.0.0.1:{ready['port']}"

        def get(path):
            with urlopen(base + path, timeout=20) as r:
                return r.read()

        watch = json.loads(get(f"/api/watch?token={args.token}"))
        build = watch.get("build") or {}
        check("the running server has a build document of its own", bool(build.get("short")),
              str(build.get("short")))

        core_file = os.path.realpath(args.pl)
        server_file = os.path.realpath(args.app)
        check("the core it reports is the core it was given",
              os.path.realpath(build.get("core", {}).get("path", "")) == core_file,
              build.get("core", {}).get("path", ""))
        check("the core hash equals Python's hash of that file",
              build.get("core", {}).get("sha256") == sha256_file(core_file),
              f"{str(build.get('core', {}).get('sha256'))[:12]}… vs {sha256_file(core_file)[:12]}…")
        check("the server hash equals Python's hash of this binary",
              build.get("server", {}).get("sha256") == sha256_file(server_file),
              f"{str(build.get('server', {}).get('sha256'))[:12]}… vs {sha256_file(server_file)[:12]}…")

        # The page: the hash the server reports, the bytes it serves now, and the file on disk.
        served = get("/app.js")
        served_hash = hashlib.sha256(served).hexdigest()
        check("the page hash is the hash of the bytes served at /app.js",
              build.get("page", {}).get("sha256") == served_hash,
              f"{str(build.get('page', {}).get('sha256'))[:12]}…")
        if args.page:
            on_disk = sha256_file(args.page)
            check("and those bytes are the page source on disk",
                  on_disk == served_hash, f"{on_disk[:12]}…")
        check("the one-line identity carries all three", 
              all(build.get("short", "").count(x) == 1 for x in ("app ", "ui ", "core ")),
              str(build.get("short")))
        check("the window is told how to check it from outside",
              "sha256" in str(build.get("check", "")).lower() or "shasum" in str(build.get("check", "")),
              str(build.get("check"))[:100])

        # The window must actually show the line, and the shell must be able to say it in its menus:
        # a fact that exists only in an API response is not "the app says which build it is".
        check("the page shows the line in the sidebar footer",
              "S.boot.build" in served.decode("utf-8", "replace"), "side-foot wiring")
        if args.shell:
            shell = Path(args.shell).read_text()
            check("the macOS shell keeps the build for the About panel and the menu",
                  "buildShort" in shell and shell.count("buildShort") >= 3,
                  f"{shell.count('buildShort')} references")
            check("the shell's About panel prints it",
                  "This build:" in shell, "")
    finally:
        server.terminate()
        try:
            server.wait(timeout=15)
        except Exception:
            server.kill()
        subprocess.run(["pkill", "-f", "--", f"--archive {work}"], check=False)

    print(f"\n{PASS} PASS, {FAIL} FAIL")
    return 0 if FAIL == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
