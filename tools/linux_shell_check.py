#!/usr/bin/env python3
"""Drive the real Linux shell and check what it actually does.

Not a mock, and not a web page in a browser: the shipped `projectlife-app` binary, its real GTK
window, its real WebKitGTK, its real interface server and the real core underneath — under Xvfb,
because a container has no display.

Three phases:

  A. `--selftest`: the shell loads the shipped page, asks the DOM what it found, writes a PNG of the
     window through WebKit's own snapshot API and prints one machine-readable line per fact.
  B. The window stays open and protects: the script starts observation through the app's own route,
     edits a file *from outside* (as an agent or an editor would), and then asks the archive — through
     the core, not through the app — whether a new version exists. Then it asks the app to quit
     completely and checks that the observation really stopped and that nothing was left running.
  C. The handshake: a second launch must not start a second window, and a handshake file left behind
     by a shell that is gone must not refuse to start.

What this cannot check, and says so in its output: there is no window manager under Xvfb, so the
close path is exercised by emitting the same signal a window manager sends; and a tray icon needs a
StatusNotifier host on the desktop, which a headless X server does not provide.

    python3 tools/linux_shell_check.py [--app DIR] [--json]
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import time
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
RESULTS = {"passed": 0, "failed": 0, "notes": []}


def ok(msg):
    RESULTS["passed"] += 1
    print(f"  PASS  {msg}")


def bad(msg):
    RESULTS["failed"] += 1
    print(f"  FAIL  {msg}")


def note(msg):
    RESULTS["notes"].append(msg)
    print(f"  note  {msg}")


def run(cmd, env=None, timeout=180, cwd=None):
    return subprocess.run(cmd, capture_output=True, text=True, env=env, timeout=timeout, cwd=cwd)


def xvfb(cmd, env, timeout=180):
    return run(["xvfb-run", "-a", "-s", "-screen 0 1280x900x24"] + cmd, env=env, timeout=timeout)


def api(port, token, path, method="GET", body=None, timeout=20):
    url = f"http://127.0.0.1:{port}/api/{path}{'&' if '?' in path else '?'}token={token}"
    data = None
    if body is not None:
        data = json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, method=method)
    if data is not None:
        req.add_header("Content-Type", "application/json")
    with urllib.request.urlopen(req, timeout=timeout) as r:
        raw = r.read().decode()
    try:
        return json.loads(raw)
    except json.JSONDecodeError:
        return {"raw": raw}


def core(binary, archive, args, home=None, timeout=120):
    env = dict(os.environ)
    if home:
        env["PROJECTLIFE_HOME"] = home
    out = run([binary, "--archive", archive] + args, env=env, timeout=timeout)
    return out.returncode, out.stdout, out.stderr


def last_json(text):
    """The last complete JSON document in a program's output."""
    lines = text.splitlines()
    for start in range(max(0, len(lines) - 60), len(lines)):
        chunk = "\n".join(lines[start:])
        if not chunk.strip():
            continue
        try:
            return json.loads(chunk)
        except json.JSONDecodeError:
            continue
    return None


def shell_env(tmp, archive):
    env = dict(os.environ)
    env["HOME"] = os.path.join(tmp, "home")
    env["XDG_STATE_HOME"] = os.path.join(tmp, "state")
    env["XDG_CONFIG_HOME"] = os.path.join(tmp, "config")
    env["XDG_CACHE_HOME"] = os.path.join(tmp, "cache")
    env["XDG_RUNTIME_DIR"] = os.path.join(tmp, "run")
    env["PROJECTLIFE_HOME"] = os.path.join(tmp, "pl-home")
    env["PROJECTLIFE_APP_HOME"] = os.path.join(tmp, "app-home")
    env["GDK_BACKEND"] = "x11"
    env["NO_AT_BRIDGE"] = "1"
    for key in ("HOME", "XDG_STATE_HOME", "XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_RUNTIME_DIR",
                "PROJECTLIFE_HOME", "PROJECTLIFE_APP_HOME"):
        os.makedirs(env[key], exist_ok=True)
    os.chmod(env["XDG_RUNTIME_DIR"], 0o700)
    env["PL_ARCHIVE"] = archive
    return env


def read_log(env):
    p = os.path.join(env["PROJECTLIFE_APP_HOME"], "daemon.log")
    try:
        with open(p, encoding="utf-8", errors="replace") as f:
            return f.read()
    except OSError:
        return ""


def ready_line(log):
    m = None
    for line in log.splitlines():
        if "server said: " in line:
            m = line.split("server said: ", 1)[1]
    if not m:
        return None
    try:
        return json.loads(m)
    except json.JSONDecodeError:
        return None


def wait_ready(env, timeout=40, since=0):
    """The ready line of *this* run: the log is appended to across runs, and a stale line from a
    server that has since exited would send the check to a port nothing is listening on."""
    t0 = time.time()
    while time.time() - t0 < timeout:
        r = ready_line(read_log(env)[since:])
        if r and r.get("ready"):
            return r
        time.sleep(0.3)
    return None


def phase_a(app, tmp, arch, env, report):
    print("\n== A. --selftest: the real window loads the real page")
    shot = os.path.join(tmp, "window.png")
    out = xvfb([app, "--selftest", "--selftest-close", "--screenshot", shot,
                "--app-home", env["PROJECTLIFE_APP_HOME"], "--archive", arch], env, timeout=120)
    text = out.stdout + out.stderr
    report["selftest_stdout"] = out.stdout
    for line in out.stdout.splitlines():
        if line.startswith("selftest:"):
            print("   ", line)
    if out.returncode == 0 and "RESULT=PASS" in out.stdout:
        ok("the shell's selftest passed (page loaded, DOM answered, window closed cleanly)")
    else:
        bad(f"the shell's selftest failed (exit {out.returncode}):\n{text[-1500:]}")
        return False
    m = re.search(r"page=loaded title=\"([^\"]*)\" sidebar=(\w+) textLen=(\d+)", out.stdout)
    if m and m.group(2) == "yes" and int(m.group(3)) > 200:
        ok(f"the page rendered its own UI in the window (title {m.group(1)!r}, {m.group(3)} chars of text)")
    else:
        bad(f"the page did not render a sidebar: {m.group(0) if m else out.stdout[-300:]}")
    m = re.search(r"shell-version=([\w.]+) tray=(\w+) url-port=(\d+)", out.stdout)
    if m:
        ok(f"the shell reports itself: version {m.group(1)}, tray {m.group(2)}, port {m.group(3)}")
    else:
        bad("the shell did not report its own version and the port it uses")
    m = re.search(r"menu-groups=(\d+) menu-bar-items=(\d+) tray-items=(\d+)", out.stdout)
    if m and int(m.group(1)) >= 5 and int(m.group(2)) >= 5:
        ok(f"the menu came from the server as data: {m.group(1)} groups, {m.group(2)} menu bar items, "
           f"{m.group(3)} tray items")
    else:
        bad(f"the menu was not built from the server's document: {m.group(0) if m else 'no line'}")
    m = re.search(r"close window-visible=(\w+) server-still-answering=(\w+)", out.stdout)
    if m and m.group(1) == "no" and m.group(2) == "yes":
        ok("closing the window hides it and the server keeps answering (the protection is a separate process)")
    else:
        bad(f"the close path did not behave: {m.group(0) if m else 'no line'}")
    m = re.search(r"screenshot=(\S+) bytes=(-?\d+)", out.stdout)
    if m and int(m.group(2)) > 5000 and os.path.exists(shot):
        ok(f"a real PNG of the window was written by WebKit's own snapshot API ({m.group(2)} bytes)")
        report["screenshot"] = shot
    else:
        bad(f"no usable screenshot: {m.group(0) if m else 'no line'}")
    m = re.search(r"screenshot-size=(\d+)x(\d+)", out.stdout)
    if m:
        report["screenshot_size"] = m.group(0).split("=")[1]
        ok(f"the snapshot has real dimensions ({report['screenshot_size']})")
    return True


def phase_b(app, tmp, arch, env, report):
    print("\n== B. the window stays open and protects")
    work = os.path.join(tmp, "proj")
    os.makedirs(os.path.join(work, "src"), exist_ok=True)
    with open(os.path.join(work, "src", "app.ts"), "w") as f:
        f.write("export const v = 1;\n")

    core_bin = os.path.join(os.path.dirname(app), "projectlife")
    code, out, err = core(core_bin, arch, ["init-archive", arch], home=env["PROJECTLIFE_HOME"])
    if code != 0:
        bad(f"could not create a test archive: {out}{err}")
        return False
    core(core_bin, arch, ["config", "set", "stopFreePercent", "0"], home=env["PROJECTLIFE_HOME"])
    core(core_bin, arch, ["config", "set", "warnFreePercent", "0"], home=env["PROJECTLIFE_HOME"])
    code, out, err = core(core_bin, arch, ["add", work, "--profile", "all", "--yes"], home=env["PROJECTLIFE_HOME"])
    if code != 0:
        bad(f"could not add the test project: {out}{err}")
        return False
    ok("a test archive and project exist (separate from any real history)")

    # The shell is started without --selftest and left running.
    log_at = len(read_log(env))
    proc = subprocess.Popen(["xvfb-run", "-a", "-s", "-screen 0 1280x900x24",
                             app, "--app-home", env["PROJECTLIFE_APP_HOME"], "--archive", arch],
                            env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
                            start_new_session=True)
    try:
        r = wait_ready(env, since=log_at)
        if not r:
            bad("the shell never reported a ready server:\n" + read_log(env)[-800:])
            return False
        port, token = r["port"], r["token"]
        ok(f"the window is up and its server answers on 127.0.0.1:{port} ({r.get('how')})")

        boot = api(port, token, "bootstrap")
        if boot.get("app", {}).get("platform") == "linux" and boot.get("app", {}).get("native"):
            ok("the server knows it is running inside the Linux shell (native=true, platform=linux)")
        else:
            bad(f"the server does not report the Linux shell: {json.dumps(boot.get('app', {}))[:200]}")

        # Nothing is copied by merely starting the window: count the archive's blobs first.
        blobs_before = count_blobs(arch)

        start = api(port, token, "watch/start", "POST", {"intervalSeconds": 1})
        ok(f"observation started through the app's own route: {json.dumps(start)[:120]}")
        state = wait_for_state(port, token, 30)
        report["state_after_start"] = state
        if state in ("protected", "protected_low_space"):
            ok(f"the app says protection is real ({state})")
        else:
            bad(f"the app does not report protection after starting observation ({state})")

        # An external change, exactly as an editor or an agent would make it.
        versions_before = versions_of(core_bin, arch, "proj", env)
        with open(os.path.join(work, "src", "app.ts"), "w") as f:
            f.write("export const v = 2;\n")
        versions_after = wait_for_version(core_bin, arch, "proj", env, versions_before, 30)
        report["versions"] = [versions_before, versions_after]
        if versions_after > versions_before:
            ok(f"the external change became a new version in the archive ({versions_before} -> {versions_after}), "
               "measured by the core, not by the app")
        else:
            bad(f"the external change was not stored ({versions_before} -> {versions_after})")
        blobs_after = count_blobs(arch)
        if blobs_after > blobs_before:
            ok(f"the change costs one new blob, not a copy of the project ({blobs_before} -> {blobs_after})")
        else:
            bad(f"no new blob was written ({blobs_before} -> {blobs_after})")

        # Quit completely: the app says what it does, and then the observation really stops.
        # "Quit completely" as the menu performs it: the app's own control channel.
        with open(os.path.join(env["PROJECTLIFE_APP_HOME"], "quit.request"), "w") as f:
            f.write("quit\n")
        code = wait_exit(proc, 40)
        report["shell_exit"] = code
        if code == 0:
            ok("the shell exited cleanly on \"quit completely\"")
        else:
            bad(f"the shell exited with {code}")
        stopped = wait_daemon_gone(core_bin, arch, env, 20)
        if stopped:
            ok("the observation this app started really stopped (no daemon, no lock, heartbeat frozen)")
        else:
            bad("the daemon is still running after quit completely")
        if os.path.exists(os.path.join(arch, ".daemon")):
            bad("the daemon lock file was left behind")
        else:
            ok("the daemon lock was released")
        leftover = all_matching(tmp)
        if leftover:
            bad(f"processes were left behind: {leftover}")
        else:
            ok("no projectlife process was left behind by this run")
        return True
    finally:
        if proc.poll() is None:
            kill_leftovers(tmp)
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()


def count_blobs(arch):
    """The archive stores content per project, under projects/<name>/blobs: count them there rather
    than guessing a path that happens to be empty."""
    n = 0
    root = os.path.join(arch, "projects")
    for dirpath, _dirs, files in os.walk(root):
        if "/blobs" in dirpath:
            n += len(files)
    return n


def count_files(path):
    n = 0
    for _root, _dirs, files in os.walk(path):
        n += len(files)
    return n


def versions_of(core_bin, arch, name, env):
    code, out, err = core(core_bin, arch, ["status", name, "--json"], home=env["PROJECTLIFE_HOME"])
    v = last_json(out)
    rows = v if isinstance(v, list) else [v]
    for row in rows:
        if isinstance(row, dict) and row.get("name") == name:
            return int(row.get("versions") or row.get("versionCount") or 0)
    return 0


def wait_for_version(core_bin, arch, name, env, before, seconds):
    t0 = time.time()
    v = before
    while time.time() - t0 < seconds:
        v = versions_of(core_bin, arch, name, env)
        if v > before:
            return v
        time.sleep(0.5)
    return v


def wait_for_state(port, token, seconds):
    """Wait for a verdict that is about *writing*, not for the first answer. `stale` is what a daemon
    looks like in the second before its first heartbeat — returning on it would call a working
    observation broken."""
    t0 = time.time()
    state = "?"
    while time.time() - t0 < seconds:
        try:
            w = api(port, token, "watch")
            state = w.get("protection", {}).get("state", "?")
            if state in ("protected", "protected_low_space", "paused_full"):
                return state
        except Exception:
            pass
        time.sleep(0.5)
    return state


def wait_exit(proc, seconds):
    t0 = time.time()
    while time.time() - t0 < seconds:
        if proc.poll() is not None:
            return proc.returncode
        time.sleep(0.3)
    return None


def wait_daemon_gone(core_bin, arch, env, seconds):
    """Gone means: the lock is released AND the pid that held it is not alive."""
    t0 = time.time()
    while time.time() - t0 < seconds:
        code, out, _ = core(core_bin, arch, ["daemon", "status"], home=env["PROJECTLIFE_HOME"])
        lock = os.path.exists(os.path.join(arch, ".daemon"))
        if not lock and "daemon lock: held by" not in out:
            return True
        time.sleep(0.5)
    return False


# The programs this check starts and may therefore clean up. Matching on the *path* alone once killed
# the check itself: `--app /work/projectlife/dist/linux/ProjectLife-linux-aarch64/bin/projectlife-app`
# puts that path in the check's own command line, so the cleanup found the check, killed it with
# SIGKILL, and the step reported "Killed" with an empty log — three times, before the cause was found
# by reading the process list instead of the log.
OWN_PROGRAMS = ("projectlife-app", "projectlife-ui", "bin/projectlife", "pl-ui", "Xvfb", "xvfb-run")


def all_matching(marker):
    """Processes of *this* check that mention `marker`, excluding this process and its parent.

    A leftover from an earlier run is not this run's business — and mistaking one for another sent this
    check hunting a bug in the wrong place once already."""
    mine = {os.getpid(), os.getppid()}
    out = run(["ps", "-eo", "pid,pgid,cmd", "--no-headers"], timeout=30).stdout
    rows = []
    for line in out.splitlines():
        if marker not in line or "ps -eo" not in line:
            continue
        if not any(name in line for name in OWN_PROGRAMS):
            continue
        parts = line.strip().split(None, 2)
        if len(parts) == 3 and int(parts[0]) not in mine and int(parts[1]) not in mine:
            rows.append(parts)
    return rows


def kill_leftovers(marker):
    """Nothing of ours may be left running before a check starts: a shell from an earlier run holds
    the port the ladder wants first, and then the next run is measuring the wrong thing."""
    killed = []
    for pid, _pgid, cmd in all_matching(marker):
        try:
            os.kill(int(pid), 9)
            killed.append(pid)
        except ProcessLookupError:
            pass
    if killed:
        time.sleep(0.5)
    return killed


def phase_c(app, tmp, arch, env, report):
    print("\n== C. the handshake: a second launch, and a file left behind")
    sh = os.path.join(env["PROJECTLIFE_APP_HOME"], "shell.json")
    with open(sh, "w") as f:
        json.dump({"pid": os.getpid(), "port": 1, "version": "test"}, f)
    out = xvfb([app, "--app-home", env["PROJECTLIFE_APP_HOME"], "--archive", arch], env, timeout=60)
    text = out.stdout + out.stderr
    if out.returncode == 0 and "already running" in text:
        ok("a second launch does not start a second window; it asks the first one to come forward")
    else:
        bad(f"the second launch did not behave (exit {out.returncode}): {text[-300:]}")
    if os.path.exists(os.path.join(env["PROJECTLIFE_APP_HOME"], "show.request")):
        ok("the request to show the window was left where the running shell looks for it")
    else:
        bad("no show.request was written")

    # A handshake file whose process is gone must not stop the app from starting.
    os.remove(sh)
    log_at = len(read_log(env))
    proc = subprocess.Popen(["xvfb-run", "-a", "-s", "-screen 0 1280x900x24",
                             app, "--app-home", env["PROJECTLIFE_APP_HOME"], "--archive", arch],
                            env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    try:
        r = wait_ready(env, timeout=40, since=log_at)
        if r:
            ok("with no live handshake file, the app starts normally and gets a server")
        else:
            bad("the app did not start a server")
        api(r["port"], r["token"], "shutdown", "POST", {}) if r else None
        wait_exit(proc, 20)
    finally:
        if proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()
    with open(sh, "w") as f:
        json.dump({"pid": 999999, "port": 1, "version": "test"}, f)
    out = xvfb([app, "--selftest", "--app-home", env["PROJECTLIFE_APP_HOME"], "--archive", arch], env, timeout=90)
    if out.returncode == 0 and "RESULT=PASS" in out.stdout:
        ok("a handshake file naming a dead process is ignored, not obeyed")
    else:
        bad(f"a stale handshake file blocked the app (exit {out.returncode})")
    return True


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--app", default=os.path.join(ROOT, "dist/linux",
                    "ProjectLife-linux-" + os.uname().machine, "bin/projectlife-app"))
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--keep", action="store_true")
    args = ap.parse_args()

    if not os.path.exists(args.app):
        print(f"no shell at {args.app} — build it first: sh app/linux/build_linux_app.sh")
        return 2

    killed = kill_leftovers("dist/linux/ProjectLife-linux-")
    if killed:
        print(f"cleaned up {len(killed)} process(es) left by an earlier run: {killed}")
    tmp = f"/tmp/pl-linux-shell-{os.getpid()}"
    shutil.rmtree(tmp, ignore_errors=True)
    os.makedirs(tmp)
    arch = os.path.join(tmp, "archive")
    env = shell_env(tmp, arch)
    report = {"app": args.app, "tmp": tmp}

    print(f"shell: {args.app}")
    print(f"work:  {tmp}")

    phase_a(args.app, tmp, arch, env, report)
    # A fresh archive per phase: phase A needs one to exist, phase B creates its own.
    shutil.rmtree(arch, ignore_errors=True)
    os.makedirs(arch, exist_ok=True)
    phase_b(args.app, tmp, arch, env, report)
    phase_c(args.app, tmp, arch, env, report)

    print(f"\n{SYMBOL} {RESULTS['passed']} passed, {RESULTS['failed']} failed")
    for n in RESULTS["notes"]:
        print(f"  note: {n}")
    print("  note: there is no window manager under Xvfb, so the close path is the same signal a")
    print("        window manager sends, emitted in-process; a tray icon needs a StatusNotifier host")
    print("        on the desktop, which a headless X server does not provide.")
    report["passed"] = RESULTS["passed"]
    report["failed"] = RESULTS["failed"]
    out = os.path.join(ROOT, "evidence")
    os.makedirs(out, exist_ok=True)
    with open(os.path.join(out, "linux_shell_check_301.json"), "w") as f:
        json.dump(report, f, indent=2)
    if args.json:
        print(json.dumps(report, indent=2))
    if not args.keep:
        shutil.rmtree(tmp, ignore_errors=True)
    return 0 if RESULTS["failed"] == 0 else 1


SYMBOL = "LINUX SHELL:"

if __name__ == "__main__":
    sys.exit(main())
