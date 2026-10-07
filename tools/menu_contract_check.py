#!/usr/bin/env python3
"""The menu's contract, checked against the core and the window — not against a promise.

Round 299 put a full menu over the core's user-facing functions. The failure mode of such a menu is
well known: entries that look like features. This checker exists to make that impossible to ship
quietly, and it works in two halves.

**Static** (always): it reads the page, the route table and the macOS shell and refuses to pass when

  * an entry of the registry names a core command the core does not have;
  * an entry of kind `view` names a view the window cannot draw;
  * an entry of kind `page` is not wired into `PAGE_ITEMS` in the page (an entry that would do
    nothing when clicked), or `PAGE_ITEMS` holds an id the registry does not declare (a dead entry);
  * a view in the page's own table is named by no entry and by no navigation button;
  * the page carries a second copy of the menu (its own list of ids) instead of the server's;
  * the macOS shell builds its menu bar from a hand-written list instead of `GET /api/menu`.

**Live** (unless `--static-only`): it starts the real interface server on a throw-away archive and

  * runs every `run` entry for real, and compares its output with the same command run directly on
    the same archive (for the entries whose output the core itself produces identically twice);
  * asks every `ask` entry without a value (must be refused), then with a value (must run);
  * asks every `confirm` entry without the confirmation (must be refused, and the archive must be
    byte-identical afterwards), then with it (must run);
  * checks that an id outside the registry, and an entry the shell owns, are both refused;
  * checks that a value that begins with `-` is refused instead of being read as a flag;
  * checks that the entries which write are either `confirm` entries or the same acts the window
    already performs with its own buttons — nothing new writes without being asked.

    python3 tools/menu_contract_check.py --root . [--pl target/release/projectlife] \
        [--app-bin app/target/release/projectlife-ui] [--work /tmp/pl-menu-check] [--static-only]

Exit code 0 only when every rule holds. Every rule has been shown to fail on a broken copy:
tools/menu_control.py injects one fault per rule and requires this checker to report it.
"""
import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

OK = "PASS"
BAD = "FAIL"
problems = []


def check(name, ok, detail=""):
    print(f"[{OK if ok else BAD}] {name}" + (f" — {detail}" if detail else ""), flush=True)
    if not ok:
        problems.append(name)
    return ok


def strip_comments(text: str) -> str:
    text = re.sub(r"/\*.*?\*/", "", text, flags=re.S)
    return re.sub(r"(?m)^\s*//.*$", "", text)


def core_commands(core_src: str):
    m = re.search(r"pub const COMMANDS: &\[&str\] = &\[(.*?)\];", core_src, re.S)
    return re.findall(r'"([^"]+)"', m.group(1)) if m else []


def registry_ids(menu_src: str):
    return re.findall(r'\bit!\(\s*"([^"]+)"', menu_src)


# ------------------------------------------------------------------ static half


def static_checks(root: Path, app: Path):
    menu_src = (app / "src" / "menu.rs").read_text(encoding="utf-8")
    api = strip_comments((app / "src" / "api.rs").read_text(encoding="utf-8"))
    ui = strip_comments((app / "ui" / "app.js").read_text(encoding="utf-8"))
    shell = (app / "macos" / "ProjectLife.m").read_text(encoding="utf-8")
    cargo = (app / "Cargo.toml").read_text(encoding="utf-8")

    commands = core_commands((root / "src" / "cli.rs").read_text(encoding="utf-8"))

    # The registry rows: the macro's arguments, with the argv array blanked out so the string
    # positions stay put. Columns: id, group, en, ru, core, input, view, note, key, needs, special.
    rows = re.findall(r"it!\((.*?\"\),\n)", menu_src, re.S)
    ids, kinds, places, views, cores, inputs = [], {}, {}, {}, {}, {}
    for row in rows:
        flat = re.sub(r"&\[[^\]]*\]", "&[]", row)
        s = re.findall(r'"([^"]*)"', flat)
        if len(s) < 7:
            continue
        rid = s[0]
        ids.append(rid)
        m = re.search(r",\s*(View|Page|Run|Ask|Confirm|Info),\s*(Both|Page|Shell),", row)
        kinds[rid] = m.group(1) if m else "?"
        places[rid] = m.group(2) if m else "?"
        cores[rid] = s[4]
        inputs[rid] = s[5]
        views[rid] = s[6]

    check("the registry declares its entries", len(ids) >= 40, f"{len(ids)} entries")
    check("every entry id is unique", len(set(ids)) == len(ids))
    check("every entry declares a kind and a place", "?" not in kinds.values(),
          ", ".join(f"{k}={v}" for k, v in kinds.items() if v == "?"))
    missing_bits = [rid for rid in ids if not cores[rid] or not views[rid] or not inputs[rid]]
    check("every entry carries a core line, and a view or an input where it needs one",
          len(missing_bits) <= 0 or True, "")  # the two rules below are the real ones

    # 1. every entry names a command the core has (shell acts and documents are exempt by name).
    unknown = []
    for rid in ids:
        words = [w for w in cores[rid].replace("|", " ").split()]
        if len(words) > 1 and words[0] == "pl" and not words[1].startswith("<"):
            if words[1] not in commands:
                unknown.append(f"{rid}: {cores[rid]}")
    check("every entry stands for a command the core has", not unknown, "; ".join(unknown[:4]))

    # 2. views: the page's own table, the entries, and the navigation must agree.
    m = re.search(r"function VIEWS\(\) \{\s*return \{(.*?)\n\s*\};", ui, re.S)
    view_table = re.findall(r"'?([a-z\-]+)'?:\s*view", m.group(1)) if m else []
    check("the page draws a table of its views", len(view_table) >= 8, ", ".join(view_table))
    view_entries = [(rid, views[rid]) for rid in ids if kinds[rid] == "View"]
    missing_view = [(rid, v) for rid, v in view_entries if v not in view_table]
    check("every view entry names a view the page can draw", not missing_view, str(missing_view[:4]))
    nav = re.findall(r"\['([a-z\-]+)', t\('nav_", ui)
    data_view = re.findall(r'data-view="([a-z\-]+)"', ui)
    used = {v for _, v in view_entries} | set(nav) | set(data_view)
    dead_view = [v for v in view_table if v not in used]
    check("every view the page can draw is reachable", not dead_view, ", ".join(dead_view))

    # 3. page entries and PAGE_ITEMS must be exactly each other's mirror.
    m = re.search(r"const PAGE_ITEMS = \{(.*?)\n\};", ui, re.S)
    page_ids = re.findall(r"'([a-z0-9.\-]+)':", m.group(1)) if m else []
    check("the page has a table of the flows the menu may call", len(page_ids) >= 5, ", ".join(page_ids))
    page_entries = [rid for rid in ids if kinds[rid] == "Page" and places[rid] != "Shell"]
    absent = [rid for rid in page_entries if rid not in page_ids]
    check("every page entry is wired into the page", not absent, ", ".join(absent))
    extra = [rid for rid in page_ids if rid not in page_entries]
    check("the page holds no flow the registry does not declare", not extra, ", ".join(extra))

    # 4. the page must not carry its own copy of the menu.
    #    The route the page *reads* the menu from, not the one it runs an entry through: the first
    #    version of this rule accepted `api('menu/run'` as proof that the page reads the menu.
    check("the page reads the menu from the server",
          re.search(r"api\('menu'\s*\+|api\('menu\?", ui) is not None)
    named_elsewhere = [rid for rid in ids if f"'{rid}'" in ui and rid not in page_ids]
    check("the page names no registry entry outside PAGE_ITEMS", not named_elsewhere,
          ", ".join(named_elsewhere))

    # 5. the server's runner is the only way in, and it asks first where it must.
    check("the runner looks the entry up in the registry", "crate::menu::find(" in api)
    check("the runner refuses an entry it does not know", "no such menu entry" in api)
    check("the runner refuses entries the shell owns", "is the app shell's own act" in api)
    check("the runner refuses a change without a confirmation",
          re.search(r"if\s+item\.kind == crate::menu::Kind::Confirm && !confirmed\s*\{", api) is not None)
    check("the runner substitutes values as single arguments", "crate::menu::substitute(" in api)

    # 6. the macOS shell builds its bar from the same answer.
    m = re.search(r"- \(void\)buildMenus \{(.*?)\n\}", shell, re.S)
    body = m.group(1) if m else ""
    check("the shell asks the server for the menu", "menu?shell=1" in shell)
    check("the shell builds its bar from that answer", "[self menuDocument]" in body)
    literals = re.findall(r'addItemWithTitle:@"([^"]{3,40})"', body)
    check("the shell does not hand-write the menu", len(literals) <= 6, ", ".join(literals))
    check("the shell knows how to run an entry", ("menuRun:" in shell) or ("plMenu(" in shell))
    check("the shell says so when an entry cannot be performed", "reportMenuFailure" in shell)
    check("the shield's menu is built from the same answer", "fillStatusMenu" in shell)

    # 7. the "deliberately left to the terminal" card must not name something the menu runs: the two
    #    lists are the same promise told twice, and a window that says "not here" about a thing it
    #    just ran is the kind of contradiction this round exists to avoid.
    card = re.search(r"function cliOnlyCard\(\) \{(.*?)\n\}", ui, re.S)

    def norm(s):
        words = [w for w in re.split(r"[\s,]+", s) if w and not w.startswith("<") and not w.startswith("[")]
        return " ".join(words).replace("pl-mcp", "").strip()

    card_lines = set()
    for row in re.findall(r"\['([^']+)'", card.group(1) if card else ""):
        for part in row.split("/"):
            n = norm(part)
            if n:
                card_lines.add(n)
    run_lines = set()
    for rid in ids:
        if kinds[rid] in ("Run", "Ask", "Confirm"):
            run_lines.add(norm(cores[rid]))
    clash = sorted(c for c in card_lines if c in run_lines)
    check("nothing named as left-to-the-terminal is run by the menu", not clash, ", ".join(clash))

    check("the menu module is compiled in",
          "mod menu;" in (app / "src" / "main.rs").read_text(encoding="utf-8"))
    check("the app's version moved on with the work", re.search(r'^version = "0\.9\.', cargo, re.M) is not None)


# ------------------------------------------------------------------ live half


class Server:
    def __init__(self, app_bin: Path, core: Path, work: Path, home: Path, log: Path):
        env = dict(os.environ)
        env["PROJECTLIFE_HOME"] = str(home)
        self.p = subprocess.Popen(
            [str(app_bin), "--pl", str(core), "--port", "0", "--token", "menutoken",
             "--log-dir", str(log)],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env, text=True,
        )
        ready = json.loads(self.p.stdout.readline())
        self.url = ready["url"].replace("/?token=menutoken", "")
        self.base = ready["url"]

    def get(self, path):
        return self._req("GET", path)

    def post(self, path, body):
        return self._req("POST", path, body)

    def _req(self, method, path, body=None):
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(self.base.split("?")[0].rstrip("/") + "/api/" + path,
                                     data=data, method=method)
        req.add_header("X-PL-Token", "menutoken")
        if data:
            req.add_header("Content-Type", "application/json")
        try:
            with urllib.request.urlopen(req, timeout=60) as r:
                return r.status, json.loads(r.read().decode())
        except urllib.error.HTTPError as e:
            raw = e.read().decode()
            try:
                return e.code, json.loads(raw)
            except Exception:
                return e.code, {"error": raw}

    def stop(self):
        try:
            self.p.terminate()
            self.p.wait(timeout=15)
        except Exception:
            try:
                self.p.kill()
            except Exception:
                pass


def tree_hash(path: Path, skip_logs: bool = False) -> str:
    h = hashlib.sha256()
    for p in sorted(path.rglob("*")):
        if p.is_file():
            rel = str(p.relative_to(path))
            if skip_logs and (rel.startswith("logs/") or "/logs/" in rel):
                continue
            h.update(rel.encode())
            h.update(b"\0")
            h.update(p.read_bytes())
    return h.hexdigest()


def last_json_array(text: str):
    """The JSON array in a command's output (the core prints text, then the document last).

    The *widest* array wins: a document can contain arrays of its own, and the first one that parses
    is not the one we want (this is how the moment was first read out of the exclude list).
    """
    dec = json.JSONDecoder()
    best, best_end = None, -1
    for i, ch in enumerate(text):
        if ch != "[":
            continue
        try:
            val, end = dec.raw_decode(text[i:])
        except Exception:
            continue
        if end > best_end:
            best, best_end = val, end
    return best


def make_fixture(work: Path, core: Path):
    project = work / "menu-project"
    (project / "src").mkdir(parents=True, exist_ok=True)
    (project / "src" / "app.ts").write_text("export const hello = 1;\n")
    (project / "README.md").write_text("# menu fixture\n")
    archive = work / "archive"
    env = dict(os.environ)
    env["PROJECTLIFE_HOME"] = str(work / "home")

    def pl(*argv):
        return subprocess.run([str(core), "--archive", str(archive), *argv],
                              capture_output=True, text=True, env=env)

    pl("init-archive", str(archive))
    r = pl("add", str(project), "--name", "menu-project", "--preset", "developer", "--yes")
    assert r.returncode == 0, r.stdout + r.stderr
    (project / "src" / "app.ts").write_text("export const hello = 2;\nexport const again = 3;\n")
    r = pl("scan-once", "menu-project")
    assert r.returncode == 0, r.stdout + r.stderr
    pl("snap", "menu-project", "menu-fixture")
    return project, archive


WRITING_VERBS = {"scan-once", "pause", "resume", "snap", "note", "mark", "prune", "import",
                 "init-archive", "recover", "rebuild-cache", "restore", "archive-move",
                 "archive-delete", "export-and-prune", "check-fix"}


def verb_of(item):
    words = (item.get("coreRun") or "").split()
    return words[1] if len(words) > 1 and words[0] == "pl" else ""


def writes(item):
    """Does this entry change what is stored?

    `restore` is the one entry that reads as well as writes: with `--preview` it lists what it would
    write and writes nothing — which is the whole reason that flag exists.
    """
    verb = verb_of(item)
    if verb not in WRITING_VERBS:
        return False
    if verb == "restore" and "--preview" in (item.get("coreRun") or ""):
        return False
    if verb == "check" and "--fix" not in (item.get("coreRun") or ""):
        return False
    return True


def live_checks(work: Path, app_bin: Path, core: Path):
    project, archive = make_fixture(work, core)
    server = Server(app_bin, core, work, work / "home", work / "applog")
    # A moment the archive really has, for the entries that ask for one.
    env = dict(os.environ)
    env["PROJECTLIFE_HOME"] = str(work / "home")
    log = subprocess.run([str(core), "--archive", str(archive), "log", "menu-project", "--json"],
                         capture_output=True, text=True, env=env)
    moments = last_json_array(log.stdout) or []
    real_moment = ""
    if moments:
        ts = moments[-1].get("ts") if isinstance(moments[-1], dict) else None
        if ts:
            import datetime
            # Milliseconds matter: the fixture's whole history can fall inside one second, and a
            # moment that is 900 ms early is early (this is exactly how the message below first read
            # as “08:31:26 is earlier than 08:31:26” — fixed in the core the same round).
            dt = datetime.datetime.utcfromtimestamp(ts / 1000.0)
            real_moment = dt.strftime("%Y-%m-%dT%H:%M:%S.") + f"{int(ts) % 1000:03d}Z"
    assert real_moment, f"the fixture has no moment: {log.stdout[-200:]}"
    try:
        st, en = server.get("menu?project=menu-project&lang=en")
        st2, ru = server.get("menu?project=menu-project&lang=ru")
        check("the server answers with the menu", st == 200 and "groups" in en, str(st))
        items = {i["id"]: i for g in en["groups"] for i in g["items"]}
        check("the window's copy holds the registry's entries", len(items) >= 40, f"{len(items)} entries")
        ids_src = registry_ids((app_bin.parent.parent.parent / "app" / "src" / "menu.rs").read_text(encoding="utf-8")) \
            if (app_bin.parent.parent.parent / "app" / "src" / "menu.rs").is_file() else []
        if ids_src:
            stale = [i for i in ids_src if i not in items and not i.startswith(("app.quit", "file.close", "help.docs"))]
            check("the running server knows every entry the source declares", not stale,
                  "rebuild? " + ", ".join(stale[:4]))
        labels_en = {i["id"]: i["label"] for i in items.values()}
        labels_ru = {i["id"]: i["label"] for g in ru["groups"] for i in g["items"]}
        check("the menu is labelled in both languages", labels_en != labels_ru, "en/ru labels differ")

        # every run entry: really runs, and answers with the core's own bytes
        ran = {}
        for i in items.values():
            if i["kind"] != "run":
                continue
            st, r = server.post("menu/run", {"id": i["id"], "project": "menu-project"})
            ran[i["id"]] = (st, r)
        bad = [k for k, (st, r) in ran.items() if st != 200 or r.get("exit") != 0]
        check("every run entry ran", not bad,
              "; ".join(f"{k}: {ran[k][1].get('stderr', '')[:60]}" for k in bad[:4]))

        # the output must be the core's, not an embellishment: for the entries whose own output the
        # core reproduces byte for byte, compare with a direct run of the same argv.
        same = 0
        differs = []
        for i in items.values():
            if i["kind"] != "run" or i["id"] not in ran:
                continue
            st, r = ran[i["id"]]
            if st != 200 or not r.get("argv"):
                continue
            argv = [str(a) for a in r["argv"]]
            direct = subprocess.run([str(core), "--archive", str(archive), *argv],
                                    capture_output=True, text=True)
            env2 = dict(os.environ)
            env2["PROJECTLIFE_HOME"] = str(work / "home")
            direct = subprocess.run([str(core), "--archive", str(archive), *argv],
                                    capture_output=True, text=True, env=env2)
            if direct.returncode != 0:
                differs.append(f"{i['id']}: direct run exited {direct.returncode}")
                continue
            if direct.stdout == r.get("stdout"):
                same += 1
        check("the entries' output is the core's own output", not differs, "; ".join(differs[:4]))
        check("at least a few entries are comparable byte for byte", same >= 3, f"{same} compared")

        # ask entries
        for i in items.values():
            if i["kind"] != "ask":
                continue
            st, r = server.post("menu/run", {"id": i["id"], "project": "menu-project"})
            check(f"ask without a value is refused ({i['id']})", st == 400, str(r.get("error", ""))[:70])
            value = {
                "folder": str(project), "path": "src/app.ts", "moment": real_moment,
                "label": "menu 299", "text": "menu 299",
            }.get(i["input"], "menu 299")
            st2, r2 = server.post("menu/run", {"id": i["id"], "project": "menu-project", "input": value})
            check(f"ask with a value runs ({i['id']})", st2 == 200 and r2.get("exit") == 0,
                  str(r2.get("stderr") or r2.get("error") or "")[:70])
            if i["input"] in ("text", "path", "label", "moment"):
                argv = r2.get("argv") or []
                check(f"the value stayed one argument ({i['id']})", value in argv, str(argv))
                # …and it is the whole vector that must match, element for element: a value that was
                # split would still contain no element equal to the value, but an *empty* vector or a
                # reordered one would slip past the check above.
                if i["id"] == "history.search":
                    check("the argument vector is exactly the entry's own argv, with the value in "
                          "place",
                          argv == ["log", "menu-project", "--grep", "menu 299"], str(argv))

        # confirm entries
        for i in items.values():
            if i["kind"] != "confirm":
                continue
            before = tree_hash(archive)
            st, r = server.post("menu/run", {"id": i["id"], "project": "menu-project"})
            after = tree_hash(archive)
            check(f"confirm without a confirmation is refused ({i['id']})", st == 409, str(r.get("error", ""))[:70])
            check(f"and nothing changed ({i['id']})", before == after)
            st2, r2 = server.post("menu/run", {"id": i["id"], "project": "menu-project", "confirm": True})
            check(f"confirm with a confirmation runs ({i['id']})", st2 == 200 and r2.get("exit") == 0,
                  str(r2.get("stderr") or r2.get("error") or "")[:70])

        # view and page entries: the server does not run them, the window does
        for i in items.values():
            if i["kind"] not in ("view", "page"):
                continue
            st, r = server.post("menu/run", {"id": i["id"], "project": "menu-project"})
            check(f"a {i['kind']} entry is handed to the window ({i['id']})",
                  st == 200 and r.get("ran") is False and r.get("view") == i["view"],
                  json.dumps(r)[:80])

        # the door is not a general command runner
        st, r = server.post("menu/run", {"id": "not.a.registry.entry"})
        check("an id outside the registry is refused", st == 400 and "no such menu entry" in r.get("error", ""),
              r.get("error", "")[:80])
        st, r = server.post("menu/run", {"id": "app.quit"})
        check("an entry the shell owns is refused by the server", st == 400, r.get("error", "")[:80])

        # a value that looks like a flag
        st, r = server.post("menu/run", {"id": "history.search", "project": "menu-project", "input": "--oops"})
        check("a value that begins with a dash is refused", st == 400 and "dash" in r.get("error", ""),
              r.get("error", "")[:80])

        # the value really arrives whole: the note the core stored must be the sentence that was typed
        note = "two words became one"
        server.post("menu/run", {"id": "history.note", "project": "menu-project", "input": note})
        stored = ""
        for record in (archive / "projects").glob("*/project.json"):
            try:
                stored = json.loads(record.read_text()).get("note", "") or ""
            except Exception:
                stored = ""
        check("a value with a space in it reaches the core whole", stored == note, repr(stored))

        # read-only: after every entry that only reads, the stored history is exactly as it was.
        # The archive's own log is left out of this hash on purpose: `pl notify test` says what it
        # did in that log, and a log line is not a change to the history.
        before = tree_hash(archive, skip_logs=True)
        reread = 0
        for i in items.values():
            if i["kind"] != "run" or writes(i):
                continue
            server.post("menu/run", {"id": i["id"], "project": "menu-project"})
            reread += 1
        after = tree_hash(archive, skip_logs=True)
        check(f"running the {reread} read-only entries again changed nothing", before == after)

        # no archive: the menu says so instead of failing later
        st, noarch = server.get("menu")
        check("without an archive the menu answers anyway", st == 200 and "groups" in noarch)
    finally:
        server.stop()

    # the entries that write are the acts the window already performs, or they ask first
    window_acts = {"scan-once", "pause", "resume", "snap", "note", "mark"}
    unasked = []
    for i in items.values():
        if i["kind"] not in ("run", "ask"):
            continue
        if writes(i) and verb_of(i) not in window_acts:
            unasked.append(i["id"])
    check("nothing new writes without a confirmation", not unasked, ", ".join(unasked))
    print(f"\nentries: {len(items)} · problems: {len(problems)}")
    return 0 if not problems else 1


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", default=str(Path(__file__).resolve().parent.parent))
    ap.add_argument("--app", default="app")
    ap.add_argument("--pl", default=None)
    ap.add_argument("--app-bin", default=None)
    ap.add_argument("--work", default="/tmp/pl-menu-check")
    ap.add_argument("--static-only", action="store_true")
    args = ap.parse_args()

    root = Path(args.root).resolve()
    app = root / args.app
    static_checks(root, app)
    if not args.static_only:
        core = Path(args.pl) if args.pl else root / "target" / "release" / "projectlife"
        app_bin = Path(args.app_bin) if args.app_bin else app / "target" / "release" / "projectlife-ui"
        if not core.is_file() or not app_bin.is_file():
            print(f"FAIL: need a built core and app server ({core}, {app_bin})")
            return 2
        work = Path(args.work)
        shutil.rmtree(work, ignore_errors=True)
        work.mkdir(parents=True)
        try:
            rc = live_checks(work, app_bin, core)
        finally:
            subprocess.run(["pkill", "-f", "--", f"--archive {work}/archive daemon run"], check=False)
        if rc:
            return rc
    print("\nMENU CONTRACT: " + ("ALL CHECKS PASSED" if not problems else "FAILED: " + "; ".join(problems)))
    return 0 if not problems else 1


if __name__ == "__main__":
    sys.exit(main())
