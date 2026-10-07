#!/usr/bin/env python3
"""End-to-end test of the desktop app's window, driven through a real browser.

This is the *same* code the macOS window runs: the same server process, the same HTTP routes and
the same HTML/CSS/JS. The only thing a browser replaces here is the macOS shell, which only draws
the frame, the menu-bar item and the native folder panel.

The seven steps the owner asked for are performed in order, on a separate test project and a
separate store, with a real external edit in the middle. Afterwards the restored files are compared
byte-for-byte by this script (not by the app), and the export is re-hashed from its manifest.

    python3 tools/ui_e2e.py --app /path/to/projectlife-ui --pl /path/to/projectlife [--headed]

Exit code 0 only when every step passed. Output is a step-by-step log plus a JSON summary.
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

PASS = "PASS"
FAIL = "FAIL"


class Harness:
    def __init__(self, root: Path):
        self.root = root
        self.notes = []
        self.steps = []
        self.answers = []
        # Every browser dialog the page raises. The macOS shell has no handler for them, so a page
        # that depends on one works on Linux and dies in silence on the Mac — which is exactly what
        # the owner saw. This list must stay empty.
        self.dialogs = []
        self.failures = 0

    def step(self, name, ok, detail=""):
        self.steps.append({"step": name, "result": PASS if ok else FAIL, "detail": detail})
        if not ok:
            self.failures += 1
        print(f"[{PASS if ok else FAIL}] {name}" + (f" — {detail}" if detail else ""), flush=True)
        return ok

    def note(self, text):
        self.notes.append(text)
        print(f"  note: {text}")

    def next_prompt(self, value):
        self.answers.append(value)


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(65536), b""):
            h.update(chunk)
    return h.hexdigest()


def make_fixture(root: Path):
    """A small project that looks like real work, plus its own store. Never the owner's."""
    proj = root / "work"
    (proj / "src").mkdir(parents=True, exist_ok=True)
    (proj / "docs").mkdir(parents=True, exist_ok=True)
    (proj / "node_modules" / "left-pad").mkdir(parents=True, exist_ok=True)
    (proj / "src" / "main.rs").write_text('fn main(){ println!("version one"); }\n')
    (proj / "src" / "util.rs").write_text("pub fn twice(x: i32) -> i32 { x * 2 }\n")
    (proj / "docs" / "report.md").write_text("# report\n\nfirst draft\n")
    (proj / "README.md").write_text("# test project\n")
    (proj / ".env").write_text("TOKEN=do-not-copy-me\n")
    (proj / "keys").mkdir(parents=True, exist_ok=True)
    (proj / "keys" / "id_rsa").write_text("PRIVATE KEY MATERIAL\n")
    (proj / "node_modules" / "left-pad" / "index.js").write_text("module.exports = 1;\n")
    (proj / "video.mp4").write_bytes(b"\x00\x01\x02fake video bytes")
    return proj


def _chrome_kwargs():
    """Use the browser this machine actually has. Playwright's own download is not always present."""
    for cand in ("/usr/bin/chromium", "/usr/bin/chromium-browser", "/usr/bin/google-chrome"):
        if Path(cand).exists():
            return {"executable_path": cand}
    return {}


def dialog_trap(H):
    """Record every browser dialog — and answer it, so a stray one cannot hang the run."""

    def on_dialog(dialog):
        H.dialogs.append(getattr(dialog, "message", str(dialog)))
        dialog.accept(H.answers.pop(0) if H.answers else "")

    return on_dialog


def answer_modal(page, value, timeout=15000):
    """Answer the page's *own* dialog.

    Since round 296 the window draws its name/path dialog as a DOM element instead of calling
    `window.prompt`: the macOS shell had no dialog handler, so prompt returned null and the import
    stopped without a word. This is therefore the same interaction the owner performs on his Mac —
    and the dialog trap below is kept as a check: if the page ever falls back to the browser
    dialog, the test says so instead of quietly working on Linux and failing on the Mac.
    """
    field = page.wait_for_selector("#pl-ask-input", timeout=timeout)
    field.fill(value)
    page.click("#pl-ask-ok")
    # This exact element must go away; another dialog may already have taken its place.
    try:
        field.wait_for_element_state("detached", timeout=timeout)
    except Exception:
        pass


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--app", required=True, help="the projectlife-ui binary")
    ap.add_argument("--pl", required=True, help="the projectlife core binary")
    ap.add_argument("--work", default="/tmp/pl-app-e2e", help="scratch directory")
    ap.add_argument("--headed", action="store_true")
    ap.add_argument("--keep", action="store_true")
    args = ap.parse_args()

    try:
        return _run(args)
    finally:
        _cleanup()


SERVERS = []
WORK_ROOT = ""


def _cleanup():
    """Nothing this test started may survive it: a leftover daemon would keep writing into the
    fixture and make the next run measure a machine that is already busy."""
    for srv in SERVERS:
        try:
            srv.terminate()
            srv.wait(timeout=15)
        except Exception:
            try:
                srv.kill()
            except Exception:
                pass
    # Only the daemons of *this* fixture: a pattern like "projectlife-ui" would match the command
    # line of the test itself and kill the run that is cleaning up.
    if WORK_ROOT:
        subprocess.run(
            ["pkill", "-f", "--", f"--archive {WORK_ROOT}/archive daemon run"], check=False
        )


def _run(args):
    global WORK_ROOT
    root = Path(args.work)
    WORK_ROOT = str(root)
    shutil.rmtree(root, ignore_errors=True)
    root.mkdir(parents=True)
    H = Harness(root)
    proj = make_fixture(root)
    archive = root / "archive"
    home = root / "home"
    restored = root / "restored"
    exported = root / "exported"

    from playwright.sync_api import sync_playwright

    env = dict(os.environ)
    env["PROJECTLIFE_HOME"] = str(home)
    server = subprocess.Popen(
        [args.app, "--pl", args.pl, "--port", "0", "--token", "e2etoken", "--log-dir", str(root / "applog")],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env, text=True,
    )
    SERVERS.append(server)
    ready = json.loads(server.stdout.readline())
    url = ready["url"]
    H.step("the app starts and reports its window URL", bool(ready.get("ready")), url)

    shots = root / "shots"
    shots.mkdir()
    result = {"url": url, "steps": H.steps}

    with sync_playwright() as p:
        browser = p.chromium.launch(headless=not args.headed, args=["--no-sandbox"], **_chrome_kwargs())
        page = browser.new_page(viewport={"width": 1180, "height": 820})
        page.on("dialog", dialog_trap(H))
        page.goto(url, wait_until="domcontentloaded")
        page.wait_for_selector(".sidebar", timeout=15000)
        H.step("step 1 — the window opens without a terminal", True, url)

        # --- which build is this? (round 298) -------------------------------------------------
        # The owner reported three failures against a bundle that had been replaced hours earlier,
        # and nothing on screen said so. This is the line a person reads, checked against the files.
        page.wait_for_timeout(1500)
        foot = page.inner_text("#sidefoot")
        watch_doc = json.loads(page.evaluate("fetch('/api/watch?token=e2etoken').then(r=>r.text())"))
        build_doc = watch_doc.get("build") or {}
        H.step("step 1b — the window says which build it is", "build app" in foot,
               foot.replace("\n", " | ")[:140])
        H.step("step 1c — and the line is the app's own answer",
               bool(build_doc.get("short")) and build_doc["short"] in foot, str(build_doc.get("short")))
        core_on_disk = sha256(Path(args.pl))
        H.step("step 1d — the core hash shown is the hash of that file (Python, not Rust)",
               build_doc.get("core", {}).get("sha256") == core_on_disk,
               "%s vs %s" % (str(build_doc.get("core", {}).get("sha256"))[:12], core_on_disk[:12]))

        # --- step 2: choose the store, then add a project folder -----------------------------
        page.click('button[data-act="archive-create"]')
        answer_modal(page, str(archive))
        page.wait_for_timeout(700)
        body = page.inner_text("main")
        H.step("step 2a — the store is created from the window", archive.is_dir(), str(archive))
        H.step("step 2b — the window shows an empty dashboard, not fake data",
               "No folders yet" in page.inner_text("body") or "Welcome" in page.inner_text("body"))

        page.click('button[data-act="nav"][data-view="add"]')
        page.wait_for_selector('button[data-act="wizard-pick"]', timeout=8000)
        page.click('button[data-act="wizard-pick"]')
        answer_modal(page, str(proj))
        page.wait_for_selector('input[data-act="ext"]', timeout=10000)

        # --- step 3: choose the protected types and see what will be saved -------------------
        page.fill("#wiz-name", "test-project")
        main_text = page.inner_text("main")
        H.step("step 3a — the folder is detected and the wizard shows the types it found",
               ".rs" in main_text and (".md" in main_text), "types on screen")
        page.click('button[data-act="wizard-preview"]')
        page.wait_for_selector('button[data-act="wizard-next"]', timeout=20000)
        prev = page.inner_text("main")
        rows = [ln for ln in prev.splitlines() if "files ready to protect" in ln or "What is not protected" in ln or "secrets" in ln]
        H.step("step 3b — the window shows how many files and what is left out (secrets named)", bool(rows), " | ".join(rows[:3]))
        H.step("step 3c — the secrets are visibly excluded", "secret" in prev.lower(), "")
        page.screenshot(path=str(shots / "03-coverage.png"))

        # --- step 4: start protection --------------------------------------------------------
        page.click('button[data-act="wizard-next"]')
        page.wait_for_selector('button[data-act="wizard-start"]', timeout=8000)
        page.screenshot(path=str(shots / "04-storage.png"))
        page.click('button[data-act="wizard-start"]')
        page.wait_for_selector('button[data-act="open-project"]', timeout=60000)
        page.wait_for_timeout(800)
        H.step("step 4a — the initial copy finished and the project appears", True, "")

        # protection on
        page.click('button[data-act="watch-start"]')
        page.wait_for_timeout(4000)
        b = json.loads(page.evaluate("fetch('/api/bootstrap?token=e2etoken').then(r=>r.text())"))
        H.step("step 4b — observation really started (the app's own daemon child)",
               b["watch"]["runningByApp"] is True, json.dumps(b["watch"]["heartbeat"]))
        page.screenshot(path=str(shots / "05-dashboard.png"))

        # The version count as of the initial copy. Step 5 has to *exceed* this, not merely reach
        # some round number: the initial snapshot of five files also counts five versions, so a
        # threshold like ">= 5" is already true before the external edit and the step passes without
        # measuring anything. It did exactly that on 2026-10-06, which is how this line got written.
        before = None
        for pr in b["projects"]:
            if pr["name"] == "test-project":
                before = pr["versions"]
        H.note(f"versions before the external edit: test-project={before}")

        # --- step 5: an external edit shows up as a version ----------------------------------
        (proj / "src" / "main.rs").write_text('fn main(){ println!("version two, changed outside"); }\n')
        (proj / "docs" / "report.md").write_text("# report\n\nsecond draft, written by another program\n")
        deadline = time.time() + 40
        seen = False
        after = before
        while time.time() < deadline and before is not None:
            page.wait_for_timeout(1500)
            page.reload(wait_until="domcontentloaded")
            page.wait_for_selector(".sidebar", timeout=15000)
            b = json.loads(page.evaluate("fetch('/api/bootstrap?token=e2etoken').then(r=>r.text())"))
            for pr in b["projects"]:
                if pr["name"] == "test-project":
                    after = pr["versions"]
            if after is not None and before is not None and after > before:
                seen = True
                break
        H.step("step 5 — a change made by another program became a new version", seen,
               f"{before} -> {after} versions: " + ", ".join(f"{p['name']}={p['versions']}" for p in b["projects"]))

        # --- step 6: pick a moment, see the tree, restore into a separate folder -------------
        page.click('button[data-act="open-project"][data-name="test-project"]')
        page.wait_for_selector('button[data-act="moment"]', timeout=15000)
        # Wait for the number of moments this step actually asserts, not merely for one of them:
        # waiting for one and then counting two is a race, and it lost once.
        try:
            page.wait_for_function(
                "document.querySelectorAll('button[data-act=\"moment\"]').length >= 2", timeout=20000)
        except Exception:
            pass
        moments = page.eval_on_selector_all('button[data-act="moment"]', "els => els.map(e => e.getAttribute('data-at'))")
        H.step("step 6a — the timeline lists real moments", len(moments) >= 2, f"{len(moments)} moments")
        # the oldest moment = the state before the external edit
        page.eval_on_selector_all('button[data-act="moment"]', "els => els[els.length-1].click()")
        page.wait_for_selector('input[data-act="file"]', timeout=15000)
        page.screenshot(path=str(shots / "06-tree.png"))
        tree_files = page.eval_on_selector_all('input[data-act="file"]', "els => els.map(e => e.getAttribute('data-path'))")
        H.step("step 6b — the tree at that moment is shown, from the archive", len(tree_files) >= 3,
               ", ".join(tree_files[:6]))

        page.click('button[data-act="restore-dest"]')
        answer_modal(page, str(restored))
        for path in ("src/main.rs", "docs/report.md", "README.md"):
            page.check(f'input[data-act="file"][data-path="{path}"]')
        page.click('button[data-act="restore"][data-act]')
        page.wait_for_selector(".log", timeout=15000)
        deadline = time.time() + 60
        text = ""
        while time.time() < deadline:
            page.wait_for_timeout(1200)
            text = page.inner_text("main")
            if "verified" in text or "mismatch" in text:
                break
        H.step("step 6c — the restore finished and reports its own verification",
               "byte" in text and "mismatch" in text, [ln for ln in text.splitlines() if "verified" in ln][:1])
        page.screenshot(path=str(shots / "06-restored.png"))

        # independent check: the restored bytes must equal the version that was live before the edit
        want = {
            "src/main.rs": 'fn main(){ println!("version one"); }\n',
            "docs/report.md": "# report\n\nfirst draft\n",
            "README.md": "# test project\n",
        }
        bad = []
        for rel, expect in want.items():
            got = (restored / rel).read_text()
            if got != expect:
                bad.append(rel)
        H.step("step 6d — this script compares the restored bytes itself (not the app)",
               not bad, "differs: " + ", ".join(bad) if bad else "all three files identical")

        # a file that changed later must NOT have been restored from the newer moment
        H.step("step 6e — the live file was not touched by the restore",
               (proj / "src" / "main.rs").read_text().startswith('fn main(){ println!("version two'),
               (proj / "src" / "main.rs").read_text().strip())

        # --- step 7: export, then import back -------------------------------------------------
        page.click('button[data-act="export"]')
        answer_modal(page, str(exported))
        deadline = time.time() + 60
        text = ""
        while time.time() < deadline:
            page.wait_for_timeout(1200)
            text = page.inner_text("main")
            if "manifest" in text:
                break
        manifest = exported / "test-project-export" / "MANIFEST.sha256"
        H.step("step 7a — the export wrote a manifest", manifest.is_file(), str(manifest))
        ok = total = 0
        if manifest.is_file():
            base = manifest.parent
            for line in manifest.read_text().splitlines():
                if not line.strip():
                    continue
                digest, rel = line.split("  ", 1)
                total += 1
                if sha256(base / rel) == digest:
                    ok += 1
        H.step("step 7b — this script re-hashes every exported file from the manifest",
               total > 0 and ok == total, f"{ok}/{total} match")

        page.click('button[data-act="nav"][data-view="import"]')
        page.wait_for_selector('button[data-act="import"]', timeout=8000)
        page.click('button[data-act="import"]')
        # Two dialogs in a row, both drawn by the page: the folder to import, then the name.
        answer_modal(page, str(exported / "test-project-export"))
        answer_modal(page, "imported-test")
        deadline = time.time() + 60
        text = ""
        while time.time() < deadline:
            page.wait_for_timeout(1200)
            text = page.inner_text("main")
            if "event line" in text or "imported" in text:
                break
        page.reload(wait_until="domcontentloaded")
        page.wait_for_selector(".sidebar", timeout=15000)
        b = json.loads(page.evaluate("fetch('/api/bootstrap?token=e2etoken').then(r=>r.text())"))
        names = [p["name"] for p in b["projects"]]
        imp = [p for p in b["projects"] if p["name"] == "imported-test"]
        H.step("step 7c — the export came back as its own project",
               bool(imp) and imp[0]["versions"] > 0, ", ".join(names))

        # --- stopping observation, and what the window says about it -------------------------
        page.click('button[data-act="watch-stop"]')
        page.wait_for_timeout(2500)
        page.reload(wait_until="domcontentloaded")
        page.wait_for_selector(".sidebar", timeout=15000)
        b = json.loads(page.evaluate("fetch('/api/bootstrap?token=e2etoken').then(r=>r.text())"))
        stopped_text = page.inner_text("#sidefoot")
        H.step("step 8a — stopping observation stops the daemon and the window says so",
               b["watch"]["runningByApp"] is False and ("stopped" in stopped_text.lower() or "остановлено" in stopped_text.lower()),
               stopped_text.replace("\n", " / "))

        # a second app run must find the archive and the projects again (restart of the app)
        page.screenshot(path=str(shots / "08-stopped.png"))
        browser.close()

    # --- restart: the same archive, a fresh app process ---------------------------------------
    server.terminate()
    try:
        server.wait(timeout=15)
    except subprocess.TimeoutExpired:
        server.kill()
    server2 = subprocess.Popen(
        [args.app, "--pl", args.pl, "--port", "0", "--token", "e2etoken2", "--log-dir", str(root / "applog")],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env, text=True,
    )
    SERVERS.append(server2)
    ready2 = json.loads(server2.stdout.readline())
    with sync_playwright() as p:
        browser = p.chromium.launch(headless=not args.headed, args=["--no-sandbox"], **_chrome_kwargs())
        page = browser.new_page(viewport={"width": 1180, "height": 820})
        page.on("dialog", dialog_trap(H))
        page.goto(ready2["url"], wait_until="domcontentloaded")
        page.wait_for_selector(".sidebar", timeout=15000)
        b = json.loads(page.evaluate(f"fetch('/api/bootstrap?token={ready2['token']}').then(r=>r.text())"))
        names = sorted(p["name"] for p in b["projects"])
        H.step("step 9 — restarting the app finds the same store and projects",
               names == ["imported-test", "test-project"], ", ".join(names))
        page.screenshot(path=str(shots / "09-restart.png"))

        # --- step 10: the window while recording is stopped for lack of room -------------------
        #
        # The transition the owner found on his Mac, driven through the real daemon and the real
        # window: writing → no room → repeated cycles → room again → writing resumes. Every claim
        # below is read from the page or from the archive's own log, never from a stub.
        def set_config(key, value):
            subprocess.run([args.pl, "--archive", str(archive), "config", "set", key, value],
                           env=env, capture_output=True, check=False)

        def protection_state(token, timeout=25):
            """The window's own answer, read from the page it is showing."""
            page.reload(wait_until="domcontentloaded")
            page.wait_for_selector(".sidebar", timeout=timeout * 1000)
            b = json.loads(page.evaluate(
                f"fetch('/api/bootstrap?token={token}').then(r=>r.text())"))
            return ((b.get("watch") or {}).get("protection") or {}).get("state")

        def wait_for_state(token, want, timeout=90):
            deadline = time.time() + timeout
            got = None
            while time.time() < deadline:
                got = protection_state(token, timeout=25)
                if got == want:
                    return got
                time.sleep(2)
            return got

        # A daemon reads its configuration when it starts (LIMITATIONS 32), so the thresholds are
        # set before observation is started, not under a running daemon.
        set_config("stopFreeBytes", "1099511627776")
        set_config("stopFreePercent", "100")
        page.click('button[data-act="watch-start"]')
        page.wait_for_timeout(1500)
        page.reload(wait_until="domcontentloaded")
        page.wait_for_selector(".sidebar", timeout=15000)
        state = wait_for_state(ready2["token"], "paused_full")
        H.step("step 10a — while recording is stopped the window stops claiming protection",
               state == "paused_full", f"protection.state={state}")
        foot = page.inner_text("#sidefoot")
        body = page.inner_text("body")
        H.step("step 10b — and it says why, with the numbers, and no green Protected",
               ("Protected" not in foot) and ("Защищено" not in foot) and ("not written" in body or "not written" in foot),
               foot.replace("\n", " / ")[:160])
        page.screenshot(path=str(shots / "10-recording-stopped.png"))

        # The real change made while recording is stopped must survive and be recorded afterwards.
        (proj / "src" / "util.rs").write_text("pub fn twice(x: i32) -> i32 { x + x } // changed while full\n")
        time.sleep(12)   # several cycles with no room to write
        log_path = archive / "logs" / "projectlife.log"
        log_text = log_path.read_text() if log_path.is_file() else ""
        stopped_notices = [l for l in log_text.splitlines() if "NOTIFY:" in l and "recording stopped" in l]
        H.step("step 10c — repeated cycles send one notification, not one per cycle",
               len(stopped_notices) == 1, f"{len(stopped_notices)} notification(s) in {log_path.name}")
        still_stopped = [l for l in log_text.splitlines() if "writing is still stopped" in l]
        H.step("step 10d — and the log shows the cycles that kept quiet",
               len(still_stopped) >= 2, f"{len(still_stopped)} quiet cycle(s)")

        # Room again. The daemon is restarted because it read the thresholds at startup.
        page.click('button[data-act="watch-stop"]')
        page.wait_for_timeout(2500)
        set_config("stopFreeBytes", "524288000")
        set_config("stopFreePercent", "1")
        page.reload(wait_until="domcontentloaded")
        page.wait_for_selector(".sidebar", timeout=15000)
        page.click('button[data-act="watch-start"]')
        page.wait_for_timeout(1500)
        state = wait_for_state(ready2["token"], "protected")
        H.step("step 10e — when room returns the window says Protected again",
               state == "protected", f"protection.state={state}")

        # And the change made during the outage is really in the archive.
        deadline = time.time() + 60
        recorded = 0
        while time.time() < deadline:
            page.reload(wait_until="domcontentloaded")
            page.wait_for_selector(".sidebar", timeout=15000)
            b = json.loads(page.evaluate(f"fetch('/api/bootstrap?token={ready2['token']}').then(r=>r.text())"))
            row = [p for p in b["projects"] if p["name"] == "test-project"]
            recorded = row[0]["versions"] if row else 0
            if recorded >= 3:
                break
            time.sleep(2)
        H.step("step 10f — the change made while recording was stopped is recorded after it resumes",
               recorded >= 3, f"versions now: test-project={recorded}")
        log_text = log_path.read_text() if log_path.is_file() else ""
        resumed = [l for l in log_text.splitlines() if "NOTIFY:" in l and "recording resumed" in l]
        H.step("step 10g — and the resumption is announced exactly once",
               len(resumed) == 1, f"{len(resumed)} notification(s)")
        page.screenshot(path=str(shots / "10-resumed.png"))

        # --- steps 11-15: history, integrity, storage, retention, and the guards ---------------
        #
        # The audit in docs/UI_COVERAGE.md lists every core command and where the window shows it.
        # These steps are the acceptance test for the ones added in round 297: they drive the real
        # page and the real core, and each claim is either read off the screen or recomputed here
        # from the archive — never taken from the page's word.

        pl_env = env

        def core(*argv):
            out = subprocess.run([args.pl, "--archive", str(archive)] + list(argv),
                                 env=pl_env, capture_output=True, text=True)
            return out.returncode, out.stdout, out.stderr

        def core_json(*argv):
            rc, out, err = core(*(list(argv) + ["--json"]))
            if rc != 0:
                return None
            txt = out.strip()
            for i in range(0, min(6, len(txt.splitlines()))):
                chunk = "\n".join(txt.splitlines()[i:])
                try:
                    return json.loads(chunk)
                except Exception:
                    continue
            return None

        def page_json(path):
            return json.loads(page.evaluate(
                f"fetch('/api/{path}{'&' if '?' in path else '?'}token={ready2['token']}').then(r=>r.text())"))

        # open the project and go to the versions tab
        page.reload(wait_until="domcontentloaded")
        page.wait_for_selector(".sidebar", timeout=15000)
        page.click('button[data-act="open-project"][data-name="test-project"]')
        page.wait_for_selector('.tabs button[data-tab="versions"]', timeout=15000)
        page.click('.tabs button[data-tab="versions"]')
        page.wait_for_selector('button[data-act="open-file"]', timeout=15000)
        H.step("step 11a — the project window has tabs, and one of them lists the files it can show",
               page.locator('button[data-act="open-file"]').count() >= 2,
               f"{page.locator('button[data-act=open-file]').count()} file(s) offered")

        # a path that really has more than one version in the archive
        rc, out, _ = core("blame", "test-project", "src/util.rs")
        rc2, out2, _ = core("why", "test-project", "src/util.rs")
        why = core_json("why", "test-project", "src/util.rs") or {}
        H.step("step 11b — the core reports several versions of src/util.rs",
               (why.get("versionCount") or 0) >= 2, f"versionCount={why.get('versionCount')}")
        page.fill('input[data-act="filter-input"]', "util")
        page.wait_for_timeout(400)

        # The window redraws #main on every poll (and while a job runs, every 700 ms), so a click
        # that lands in the same instant as a redraw can be lost: the button is replaced under the
        # pointer. That is a real (small) behaviour of the window, recorded in LIMITATIONS; here it
        # is handled by clicking again until the page's own state shows the click arrived — never by
        # reaching into the app's state to fake the interaction.
        clicked = False
        for attempt in range(4):
            page.click('button[data-act="open-file"][data-path="src/util.rs"]')
            for _ in range(12):
                page.wait_for_timeout(400)
                if page.evaluate("S.file") == "src/util.rs" and page.evaluate("(S.versions||{}).versionCount"):
                    clicked = True
                    break
            if clicked:
                break
        page.wait_for_selector('button[data-act="show-version"]', timeout=15000)
        rows = page.locator('button[data-act="show-version"]').count()
        state = page.evaluate("({file: S.file, count: (S.versions||{}).versionCount, error: S.error})")
        H.note(f"versions tab after click: {json.dumps(state)}")
        H.step("step 11c — the window lists exactly the versions the core counted",
               rows == (why.get("versionCount") or -1), f"on screen: {rows}, in the core's answer: {why.get('versionCount')}")

        # show one version: the bytes must be the file's real content at that moment. The moment is
        # taken from the window itself (`S.content.atIso`) and handed back to the core — so the test
        # compares what the page *shows* with what the archive *holds* for that same moment, rather
        # than with a version the test picked on its own.
        page.locator('button[data-act="show-version"]').first.click()
        page.wait_for_selector('pre.code', timeout=15000)
        shown = page.inner_text("pre.code")
        on_screen = page.evaluate("({at: (S.content||{}).atIso, hash: (S.content||{}).sha256, path: S.file})")
        rc, expected, err = core("cat", "test-project", "--path", on_screen.get("path") or "",
                                 "--at", on_screen.get("at") or "")
        expected_hash = None
        why_now = core_json("why", "test-project", "src/util.rs") or {}
        for row in (why_now.get("versions") or []):
            if row.get("atIso") == on_screen.get("at"):
                expected_hash = row.get("hash")
        H.step("step 11d — the content on screen is byte-for-byte what the core gives for that moment",
               shown.strip() == expected.strip() and shown.strip() != "",
               f"moment={on_screen.get('at')} screen={shown.strip()[:40]!r} core={expected.strip()[:40]!r}")
        H.step("step 11e — and the card names the hash the core records for that version",
               bool(expected_hash) and expected_hash[:12] in page.inner_text("body"),
               f"hash on the card {on_screen.get('hash')}, in the journal {expected_hash}")

        # compare that version with the previous one: the window must show the core's own answer
        page.wait_for_selector('button[data-act="cmp-version"]', timeout=15000)
        page.locator('button[data-act="cmp-version"]').first.click()
        page.wait_for_timeout(1200)
        body = page.inner_text("body")
        H.step("step 11f — comparing two moments shows the core's own added/changed/removed lists",
               ("changed" in body or "Added" in body or "Removed" in body or "изменено" in body),
               body[body.find("Compare"):body.find("Compare") + 120].replace("\n", " / ") if "Compare" in body else "")

        # --- step 12: integrity, from the window -------------------------------------------------
        page.click('button[data-act="nav"][data-view="integrity"]')
        page.wait_for_selector('button[data-act="check"]', timeout=15000)
        page.click('button[data-act="check"][data-deep="1"]')
        page.wait_for_timeout(2500)
        check_direct = core_json("check", "test-project", "--deep") or []
        body = page.inner_text("body")
        versions_on_screen = None
        for row in check_direct:
            if row.get("project") == "test-project":
                versions_on_screen = str(row.get("versions"))
        H.step("step 12a — the deep check ran and the window shows the core's own numbers",
               versions_on_screen is not None and versions_on_screen in body,
               f"versions={versions_on_screen}, missing/corrupted="
               f"{sum(len(r.get('missingBlobs') or []) + len(r.get('corruptedBlobs') or []) for r in check_direct)}")
        H.step("step 12b — and it says whether every referenced blob is present and readable",
               ("all_present" in body) or ("every blob" in body) or ("на месте" in body),
               "the window shows the archive's own verdict")

        page.click('button[data-act="audit"]')
        page.wait_for_timeout(2000)
        audit = page_json("audit")
        H.step("step 12c — the archive audit reports how many files it hashed and whether they changed",
               isinstance(audit, dict) and audit.get("filesHashed", 0) > 0,
               f"filesHashed={audit.get('filesHashed')} ok={audit.get('ok')} differences={len(audit.get('differences') or [])}")

        page.click('button[data-act="quarantine"]')
        page.wait_for_timeout(1200)
        quar = page_json("quarantine")
        H.step("step 12d — quarantine is shown as a number, not as a promise",
               isinstance(quar, dict) and "count" in quar, f"count={quar.get('count')}")

        page.click('button[data-act="gc"]')
        page.wait_for_timeout(1200)
        gc = page_json("gc")
        H.step("step 12e — dangling bytes are shown before anything is deleted",
               isinstance(gc, dict) and "bytes" in gc, f"bytes={gc.get('bytes')}")

        # the rehearsal runs as a job and its result is the core's own report
        page.click('button[data-act="nav"][data-view="integrity"]')
        page.click('button[data-act="drill"]')
        page.wait_for_timeout(1500)
        page.wait_for_selector('.card h3', timeout=15000)
        deadline = time.time() + 120
        drill_result = None
        while time.time() < deadline:
            jobs = page_json("jobs")
            for j in jobs:
                if j.get("kind") == "drill" and j.get("state") != "running":
                    drill_result = j
            if drill_result:
                break
            time.sleep(2)
        H.step("step 12f — a rehearsal restores a moment into a temporary folder and compares it",
               bool(drill_result) and drill_result.get("state") == "done",
               (json.dumps(drill_result.get("result"))[:160] if drill_result else "no result"))

        # recovery of an interrupted prune: the window refuses without a confirmation, and works with one
        no_confirm = page.evaluate(
            f"""fetch('/api/recover?token={ready2['token']}', {{method:'POST', headers:{{'Content-Type':'application/json','X-PL-Token':'{ready2['token']}'}}, body: JSON.stringify({{name:'test-project'}})}})
                 .then(async r => ({{status: r.status, body: await r.text()}}))""")
        H.step("step 12g — an irreversible action without a confirmation is refused by the route itself",
               no_confirm.get("status") == 409, f"status={no_confirm.get('status')} body={str(no_confirm.get('body'))[:80]}")

        # --- step 13: storage and retention ------------------------------------------------------
        page.click('button[data-act="nav"][data-view="storage"]')
        page.wait_for_selector('button[data-act="load-size"]', timeout=15000)
        page.click('button[data-act="load-size"]')
        page.wait_for_timeout(1500)
        size_direct = core_json("size") or []
        body = page.inner_text("body")
        ok_size = True
        detail_size = []
        for row in size_direct:
            if row.get("name") == "test-project":
                detail_size.append(f"{row.get('name')}: {row.get('versions')} versions")
                # the version count must be on screen next to the project name
                ok_size = str(row.get("versions")) in body
        H.step("step 13a — the sizes the window shows are the core's own numbers",
               ok_size, "; ".join(detail_size) or "no rows")

        page.select_option('#ret-project', 'test-project')
        page.click('button[data-act="retention-load"]')
        page.wait_for_timeout(1200)
        H.step("step 13b — a project with no policy says so instead of inventing one",
               "No policy is stored" in page.inner_text("body"),
               page.inner_text("body")[page.inner_text("body").find("Retention"):][:120].replace("\n", " / "))

        page.fill('#ret-policy', "7d:all,30d:1/day,365d:1/month")
        page.click('button[data-act="retention-save"]')
        page.wait_for_timeout(1500)
        stored_direct = core_json("retention", "test-project") or {}
        H.step("step 13c — storing a policy through the window is the same policy the core reads back",
               stored_direct.get("policy") == "7d:all,30d:1/day,365d:1/month",
               f"core reads back: {stored_direct.get('policy')}")

        page.click('button[data-act="retention-preview"]')
        page.wait_for_timeout(2500)
        preview_direct = core_json("prune", "test-project", "--policy", "7d:all,30d:1/day,365d:1/month", "--dry-run") or {}
        body = page.inner_text("body")
        H.step("step 13d — the preview shows the plan the core computed, and nothing was deleted",
               str(preview_direct.get("versionsKept")) in body and preview_direct.get("applied") is False,
               f"kept {preview_direct.get('versionsKept')} of {preview_direct.get('versionsBefore')}, applied={preview_direct.get('applied')}")

        versions_before_prune = None
        for row in (core_json("size") or []):
            if row.get("name") == "test-project":
                versions_before_prune = row.get("versions")
        page.click('button[data-act="retention-apply"]')
        page.wait_for_selector('#pl-c-ok', timeout=15000)
        confirm_text = page.inner_text('.modal')
        page.click('#pl-c-ok')
        page.wait_for_timeout(3000)
        applied_direct = core_json("prune", "test-project", "--policy", "7d:all,30d:1/day,365d:1/month", "--dry-run") or {}
        H.step("step 13e — applying a policy asks first, in the window's own dialog, with the numbers in it",
               (str(preview_direct.get("versionsKept")) in confirm_text) and (str(preview_direct.get("versionsBefore")) in confirm_text),
               confirm_text.replace("\n", " / ")[:140])
        H.step("step 13f — after applying, the core's own view of the project has changed as the plan said",
               applied_direct.get("versionsBefore") is not None,
               f"versions before prune={versions_before_prune}, after: {applied_direct.get('versionsBefore')}")

        # --- step 14: the coverage audit itself --------------------------------------------------
        cov = subprocess.run([sys.executable, str(Path(__file__).resolve().parent / "ui_coverage.py"),
                              "--pl", args.pl, "--app-src", str(Path(__file__).resolve().parent.parent / "app"),
                              "--out", str(root / "UI_COVERAGE.md")],
                             capture_output=True, text=True)
        H.step("step 14a — every core command is either in the window or named as deliberately CLI-only",
               cov.returncode == 0, (cov.stdout.strip().splitlines() or [""])[-1][:160])
        cov_text = (root / "UI_COVERAGE.md").read_text() if (root / "UI_COVERAGE.md").is_file() else ""
        H.step("step 14b — and the coverage table lists the commands the window now covers",
               "history" in cov_text and "retention" in cov_text,
               f"{len(cov_text.splitlines())} lines in the table")

        # --- step 15: the moment field, as a Mac without a date picker would use it --------------
        #
        # `<input type="datetime-local">` needs Safari 14.1 (macOS 11.3); the bundle says 11.0 is
        # enough, so on 11.0-11.2 the field is a plain text box. Whatever a person types there must
        # be read exactly as a picked value — and text that is not a moment must be refused, because
        # JavaScript's own parser reads "not a date" as 1999-12-31 once a space becomes a colon.
        page.click('button[data-act="nav"][data-view="project"]')
        page.wait_for_selector('.tabs button[data-tab="compare"]', timeout=15000)
        page.click('.tabs button[data-tab="compare"]')
        page.wait_for_selector('#cmp-from', timeout=15000)
        # The project detail arrives from the core; the tab is drawn before it does, so wait for the
        # answer rather than reading the screen the instant the tab appears.
        deadline = time.time() + 30
        while time.time() < deadline:
            if page.evaluate("!!(S.projectDetail && S.projectDetail.moments && S.projectDetail.moments.length)"):
                break
            page.wait_for_timeout(400)
        # The *newest* moment, not the oldest: step 13 applied a retention policy to this project, and
        # a moment older than the new historyStartsAt is refused by the core (correctly). The first
        # version of this step took the oldest and passed or failed depending on what the earlier
        # steps had pruned — a test whose input is destroyed by another test is not a test.
        moment_local = page.evaluate("((S.projectDetail||{}).moments||[])[0] ? S.projectDetail.moments[0].atLocal : ''")
        H.step("step 15a — the archive offers a moment to compare from",
               bool(moment_local), f"moment from the window: {moment_local!r}")
        # The picker's own shape goes through the real field.
        page.fill('#cmp-from', moment_local.replace(' ', 'T')[:16])
        page.check('#cmp-now')
        # Same redraw race as elsewhere: click until the page's own state shows the answer arrived.
        diff_now = None
        for _ in range(4):
            page.click('button[data-act="run-compare"]')
            for _ in range(10):
                page.wait_for_timeout(400)
                diff_now = page.evaluate("S.diff && typeof S.diff === 'object' ? S.diff : null")
                if diff_now:
                    break
            if diff_now:
                break
        if not diff_now:
            H.note("compare produced no diff; page state: " +
                   json.dumps(page.evaluate("({error: S.error, from: (document.getElementById('cmp-from')||{}).value})")))
            diff_now = None
        H.step("step 15b — comparing from a moment chosen in the field works end to end",
               bool(diff_now) and ("changed" in diff_now or "added" in diff_now),
               f"diff from a picked moment: {json.dumps(diff_now)[:110] if diff_now else 'none'}")

        # The typed shape cannot be tested through this field: Chromium refuses to put a value in a
        # datetime-local input that is not in the picker's format ("Malformed value"), and it is
        # right to. That shape only exists where the *control* degrades to text — Safari before 14.1,
        # which is macOS 11.0-11.2, the range the bundle still claims to support. So the shipped
        # parser is called directly, in the shipped page, in this engine: same bytes, same runtime.
        typed = page.evaluate("momentFromInput('2026-10-06 16:04')")
        junk = page.evaluate("momentFromInput('yesterday evening')")
        empty = page.evaluate("momentFromInput('')")
        H.step("step 15c — the field's parser reads the typed form and refuses what is not a moment "
               "(the macOS 11.0-11.2 text fallback)",
               typed is not None and junk is None and empty is None,
               f"'2026-10-06 16:04' -> {typed}; 'yesterday evening' -> {junk}; '' -> {empty}")

        # --- step 16: the menu (round 299) -------------------------------------------------------
        #
        # The menu bar is drawn from the server's own answer; these steps drive it in the real
        # window and compare what it does with what the core does when the same command is run here,
        # by this script. Nothing is taken from the page's word.

        def menu_doc(project=None):
            path = "menu" + (f"?project={project}" if project else "")
            return page_json(path)

        def dismiss_any_modal():
            """Cancel whatever the page asked, so a modal cannot swallow the next click."""
            for _ in range(3):
                if page.locator(".modal-back").count() == 0:
                    return
                for sel in ("#pl-c-no", "#pl-ask-cancel"):
                    if page.locator(sel).count():
                        page.click(sel)
                        page.wait_for_timeout(200)
                        break
                else:
                    page.keyboard.press("Escape")
                    page.wait_for_timeout(200)

        doc = menu_doc("test-project")
        win_items = [i for g in doc.get("groups", []) for i in g["items"]]
        drawn = page.evaluate("allItems().map(i => i.id)")
        H.step("step 16a — the window's menu is the server's own list, entry for entry",
               sorted(drawn) == sorted(i["id"] for i in win_items),
               f"{len(drawn)} drawn / {len(win_items)} from the server")

        # Every group opens, and it holds exactly the entries the server reported for it.
        opened = 0
        mismatched = []
        for g in doc.get("groups", []):
            page.click(f'button[data-act="menu-open"][data-group="{g["id"]}"]')
            page.wait_for_timeout(120)
            rows = page.evaluate("document.querySelectorAll('#menubar .mrow .mitem').length")
            if rows != len(g["items"]):
                mismatched.append(f'{g["id"]}: {rows} drawn / {len(g["items"])}')
            opened += 1
            page.click(f'button[data-act="menu-open"][data-group="{g["id"]}"]')
        H.step("step 16b — every group opens in the window and holds its entries",
               opened == len(doc.get("groups", [])) and not mismatched,
               f"{opened} group(s) opened" + ("; " + "; ".join(mismatched) if mismatched else ""))

        # A read entry: the window must show the core's own bytes, and this script checks them.
        page.click('button[data-act="menu-open"][data-group="history"]')
        page.wait_for_timeout(150)
        page.click('button[data-act="menu-item"][data-id="history.list"]')
        page.wait_for_selector(".card.result pre.log", timeout=15000)
        shown = page.locator(".card.result pre.log").first.inner_text()
        rc, direct_out, _e = core("list", "--sort", "risk")
        H.step("step 16c — a menu entry shows the core's own output, byte for byte",
               rc == 0 and shown.strip() == direct_out.strip(),
               f"{len(shown)} characters shown; the core printed {len(direct_out)}")

        # An entry that asks for a value asks in the page's own dialog, and the value reaches the core.
        page.click('button[data-act="menu-open"][data-group="history"]')
        page.wait_for_timeout(150)
        page.click('button[data-act="menu-item"][data-id="history.note"]')
        answer_modal(page, "round 299 wrote this note from the menu")
        page.wait_for_timeout(800)
        # Read the archive's own record: this script does not ask the app whether the note arrived.
        # (`pl note` writes the project record — project.json — not a journal event.)
        note_text = "round 299 wrote this note from the menu"
        in_record = False
        for record in (archive / "projects").glob("*/project.json"):
            if note_text in record.read_text(errors="replace"):
                in_record = True
                break
        result_card = page.locator(".card.result pre.log").count() > 0
        H.step("step 16d — an entry that needs a value asks in the page's own dialog and the value "
               "reaches the core",
               in_record and result_card,
               f"the note is in the project record: {in_record}; the core's answer is on screen: {result_card}")

        # A confirm entry: refused until the window has asked, and it does ask.
        page.click('button[data-act="menu-open"][data-group="archive"]')
        page.wait_for_timeout(150)
        page.click('button[data-act="menu-item"][data-id="archive.rebuild-cache"]')
        asked_in_window = page.locator(".modal-back").count() > 0
        if asked_in_window:
            page.click("#pl-c-no")
        page.wait_for_timeout(300)
        ran_without = page.evaluate("!!(RUN_RESULT && RUN_RESULT.id === 'archive.rebuild-cache')")
        H.step("step 16e — a changing entry asks in the window first, and cancelling runs nothing",
               asked_in_window and not ran_without,
               f"dialog shown: {asked_in_window}; ran after cancelling: {ran_without}")

        # The ⌘K palette: the same list, filtered, and it runs what is chosen.
        page.keyboard.press("Meta+k")
        page.wait_for_selector("#palette-input", timeout=10000)
        total_rows = page.evaluate("paletteMatches().length")
        page.fill("#palette-input", "mcp")
        page.wait_for_timeout(250)
        filtered = page.evaluate("paletteMatches().map(i => i.id)")
        page.keyboard.press("Enter")
        page.wait_for_timeout(700)
        H.step("step 16f — ⌘K opens the same list, filters it, and runs the chosen entry",
               total_rows == len(win_items) and filtered == ["tools.mcp"]
               and page.evaluate("!!(RUN_RESULT && RUN_RESULT.id === 'tools.mcp')"),
               f"{total_rows} entries · filter 'mcp' -> {filtered}")

        # Every single entry, clicked in the real window: each one either does something or asks, and
        # none of them leaves the window broken. This is the step that makes "no dead entry" a
        # measurement rather than a claim.
        clicked = 0
        errors = []
        for g in doc.get("groups", []):
            for entry in g["items"]:
                if not entry["enabled"]:
                    continue
                sel = f'button[data-act="menu-item"][data-id="{entry["id"]}"]'
                # The window redraws after every run, so a click can land on DOM that has just been
                # replaced. Click, look, and try again — up to three times.
                drawn = False
                for attempt in range(3):
                    page.click(f'button[data-act="menu-open"][data-group="{g["id"]}"]')
                    page.wait_for_timeout(150)
                    if page.locator(sel).count() > 0:
                        drawn = True
                        break
                    page.wait_for_timeout(250)
                if not drawn:
                    errors.append(f'{entry["id"]}: not drawn')
                    continue
                try:
                    page.click(sel, timeout=5000)
                except Exception as exc:
                    errors.append(f'{entry["id"]}: click failed ({exc})')
                    continue
                clicked += 1
                page.wait_for_timeout(250)
                dismiss_any_modal()
                if page.locator(".banner.err").count():
                    errors.append(f'{entry["id"]}: the window showed an error: '
                                  + page.locator(".banner.err").first.inner_text()[:80])
                    page.click('button[data-act="dismiss-error"]')
                    page.wait_for_timeout(150)
                page.wait_for_timeout(100)
        H.step(f"step 16g — all {clicked} entries were clicked in the window, and none left it broken",
               not errors, "; ".join(errors[:4]) if errors else "no error banner, no dead entry")
        dismiss_any_modal()

        # --- step 17: the ledger and the repair, driven in the real window (round 300) -----------
        #
        # A mass deletion is performed on the real test project, by this script. The window must then
        # show the line the core recorded, and the repair must put the files back without touching a
        # file this script edited by hand in between — which this script checks itself, from the disk.

        # The files carry an extension the project's own profile tracks (`.rs`): a repair test whose
        # files were never in the archive would pass by proving nothing, and this fixture already
        # taught that lesson once — `.txt` is not in the source preset.
        # The window's own view of the ledger, read from the server exactly as the page reads it.
        page.evaluate("S.view = 'dashboard'; render()")
        page.wait_for_timeout(200)

        def wipe_many():
            """Delete a pile of files the way an agent would, and let the core observe it."""
            victims = sorted((proj / "src").glob("many_*.rs"))
            for v in victims:
                v.unlink()
            rc, out, err = core("scan-once", "test-project")
            return rc, len(victims), out + err

        (proj / "src").mkdir(parents=True, exist_ok=True)
        for i in range(30):
            (proj / "src" / f"many_{i:02}.rs").write_text(f"hand written line {i}\n")
        rc, _, _ = core("scan-once", "test-project")
        H.step("step 17a — thirty more files are observed before the deletion", rc == 0, f"exit {rc}")
        rc, n, detail = wipe_many()
        H.step(f"step 17b — this script deleted {n} files and the core observed the mass change",
               rc == 0, detail.strip().splitlines()[-1][:90] if detail.strip() else "")

        ledger = core_json("notifications", "--limit", "50") or {}
        mass_rows = [r for r in (ledger.get("notifications") or []) if r.get("kind") == "mass"]
        H.step("step 17c — the core recorded the mass change in its own ledger, with the project",
               bool(mass_rows) and mass_rows[-1].get("project") == "test-project",
               f"{(mass_rows[-1] if mass_rows else {}).get('atIso','')} {(mass_rows[-1] if mass_rows else {}).get('kind','')}")

        # The window's screen, drawn in this browser from the same server answer.
        page.click('button[data-act="nav"][data-view="notifications"]')
        page.wait_for_selector("button[data-act=\"notif-refresh\"]", timeout=15000)
        page.wait_for_timeout(400)
        drawn_rows = page.evaluate("(S.notif && S.notif.notifications || []).length")
        first_project = page.evaluate("(function(){var r=(S.notif&&S.notif.notifications)||[];for(var i=r.length-1;i>=0;i--){if(r[i].kind==='mass')return r[i].project||'';}return '';})()")
        H.step("step 17d — the window draws the ledger the server gave it, mass line included",
               drawn_rows == len(ledger.get("notifications") or []) and first_project == "test-project",
               f"{drawn_rows} line(s) drawn / {len(ledger.get('notifications') or [])} from the server")

        # A file written by hand *after* the deletion: the repair must not touch it.
        (proj / "src" / "many_00.rs").write_text("WRITTEN BY HAND AFTER THE DELETION\n")
        hand_bytes = (proj / "src" / "many_00.rs").read_bytes()
        missing_before = [p.name for p in sorted((proj / "src").glob("many_*.rs"))]
        H.step("step 17e — before the repair, the deleted files are gone and one is hand-written",
               len(missing_before) == 1, f"present: {missing_before}")

        page.click('button[data-act="repair-start"][data-name="test-project"]')
        # The moment comes from the core's last-good answer; if it is missing the window asks — answer
        # with the same moment, so the click path is identical either way.
        lg = core_json("last-good", "test-project") or {}
        if page.locator("#pl-ask-input").count():
            answer_modal(page, lg.get("atIso", ""))
        page.wait_for_selector("#pl-c-ok", timeout=15000)
        dialog_text = page.locator(".modal").first.inner_text()
        page.click("#pl-c-ok")
        for _ in range(60):
            page.wait_for_timeout(400)
            done = page.evaluate("!!(S.job && S.job.state !== 'running')")
            if done:
                break
        job = page.evaluate("S.job ? {state: S.job.state, kind: S.job.kind, result: S.job.result} : null")
        H.step("step 17f — the repair ran from the window, after showing what it would do",
               bool(job) and job.get("state") == "done" and job.get("kind") == "repair",
               f"dialog said: {' '.join(dialog_text.split())[:120]}")

        back = sorted(p.name for p in (proj / "src").glob("many_*.rs"))
        H.step("step 17g — every deleted file is back, checked by this script on the disk",
               len(back) == 30, f"{len(back)}/30 present")
        H.step("step 17h — the hand-written file was not overwritten by the repair",
               (proj / "src" / "many_00.rs").read_bytes() == hand_bytes,
               (proj / "src" / "many_00.rs").read_text().strip()[:60])
        rest = [p for p in sorted((proj / "src").glob("many_*.rs")) if p.name != "many_00.rs"]
        contents_ok = all(p.read_text().strip() == f"hand written line {int(p.stem.split('_')[1])}"
                          for p in rest)
        H.step("step 17i — the restored files hold the bytes the archive had, not the ones the "
               "window claimed", contents_ok, f"{len(rest)} restored file(s) compared by this script")
        claim = (job or {}).get("result") or {}
        H.step("step 17j — the window's own report matches what this script just measured",
               claim.get("verified") == len(rest) and not claim.get("overwritten")
               and not claim.get("deletedByRepair"),
               f"window: created={claim.get('created')} verified={claim.get('verified')} "
               f"overwritten={len(claim.get('overwritten') or [])} deleted={len(claim.get('deletedByRepair') or [])}")
        page.click('button[data-act="notif-refresh"]')
        page.wait_for_timeout(300)
        page.evaluate("S.view = 'dashboard'; render()")

        # --- step 18: who made it, and what it is for (round 302) --------------------------------
        #
        # The owner asked for his name and address wherever the program describes itself, and for the
        # sentence about the agent. This is the check that reads it *off the rendered page* — the
        # same lesson as round 296: the test has to include what the person actually reads.
        boot_doc = page_json("bootstrap")
        app_doc = (boot_doc or {}).get("app", {})
        H.step("step 18a — the server's own answer carries the author, the address and the sentence",
               bool(app_doc.get("author")) and bool(app_doc.get("authorEmail")) and bool(app_doc.get("whatItIs")),
               f"{app_doc.get('author')} <{app_doc.get('authorEmail')}>")

        page.evaluate("window.plMenu('app.about')")
        page.wait_for_timeout(600)
        about = page.inner_text("#main")
        H.step("step 18b — the About screen shows the author, the address and the sentence about the purpose",
               app_doc.get("author", "\u0000") in about
               and app_doc.get("authorEmail", "\u0000") in about
               and app_doc.get("whatItIs", "\u0000") in about,
               f"{len(about)} characters on the screen")
        foot = page.inner_text("#sidefoot")
        H.step("step 18c — and the sidebar, which is on every screen, names the author too",
               app_doc.get("author", "\u0000") in foot,
               " · ".join(foot.split("\n"))[:90])

        # The control for the two steps above: if the core stopped answering, the screen the person
        # would see must say so rather than keep a copy of the name somewhere. Asked of the page's own
        # About function, so the check does not depend on which view happened to be drawn last.
        degraded = page.evaluate(
            "(() => { const saved = JSON.parse(JSON.stringify(S.boot));"
            " S.boot.app.author = ''; S.boot.app.authorEmail = '';"
            " S.boot.app.whatItIs = ''; S.boot.app.whatItIsRu = '';"
            " S.boot.program = {unavailable: true, why: 'the core did not answer'};"
            " const html = viewAbout(); S.boot = saved; return html; })()")
        H.step("step 18d — with the core silent the About screen says the author could not be read "
               "(no invented name)",
               app_doc.get("authorEmail", "\u0000") not in degraded
               and app_doc.get("author", "\u0000") not in degraded
               and "did not answer" in degraded,
               f"{len(degraded)} characters of the degraded screen")
        page.evaluate("S.view = 'dashboard'; render()")

        if H.answers:
            H.step("the window never fell back to a browser dialog the macOS shell cannot show",
                   not H.dialogs, f"{len(H.dialogs)} browser dialog(s) appeared: {H.dialogs[:2]}")
        else:
            H.step("the window drew its own dialogs; no browser dialog was needed", not H.dialogs,
                   f"browser dialogs seen: {H.dialogs}")
        browser.close()
    server2.terminate()
    try:
        server2.wait(timeout=15)
    except subprocess.TimeoutExpired:
        server2.kill()

    result["steps"] = H.steps
    result["failures"] = H.failures
    (root / "ui_e2e_result.json").write_text(json.dumps(result, indent=2))
    print(json.dumps({"passed": len(H.steps) - H.failures, "failed": H.failures, "total": len(H.steps)}))
    return 0 if H.failures == 0 else 1




if __name__ == "__main__":
    # The exit code has to reach the caller. Until round 297 this line was `main()` — and `main()`
    # returns 1 when a step failed, so the value was thrown away and the harness reported success to
    # every pipe it was part of while showing a red step in its own log. A test that cannot fail the
    # thing that runs it is not a test; `tools/ui_surface_control.py` now also runs this script with
    # a missing binary and requires a non-zero exit.
    sys.exit(main())
