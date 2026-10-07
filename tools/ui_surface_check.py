#!/usr/bin/env python3
"""The window's contract, checked by reading it — because a browser check cannot run in every run.

This is the checker the mutation campaign uses for the *window's* layer (page + routes). It reads
the page and the server's route table and refuses to pass when any of these stops being true:

  1. every route the page calls is a route the server answers (no dead call, no silent 404);
  2. every control the page can raise has a handler (no button that does nothing);
  3. every route that changes something asks the person first — either the page sends
     `confirm: true` or the route is one of the named exceptions (storing a policy, pausing);
  4. the project selectors remember what was chosen, so a redraw cannot silently move a destructive
     action to another project (this was a real fault, found by the acceptance test in round 297);
  5. the page never uses the browser's own prompt/alert/confirm — the macOS shell has no handler for
     them, and one that is called there dies in silence.

Every one of these has been checked against a deliberately broken copy: the checker prints FAIL for
it. A checker that cannot fail would prove nothing.

    python3 tools/ui_surface_check.py [--root /work/projectlife] [--app app]
"""
import argparse
import re
import sys
from pathlib import Path

# Routes that change something without the page having to say "I confirmed this".
# Each entry has to justify itself: the act is either reversible or it is a note, not a deletion.
CONFIRM_EXEMPT = {
    "project_note": "writing a note changes nothing about the files",
    "project_mark": "a mark is a note in the journal; nothing is deleted",
    "retention_set": "storing a policy deletes nothing — applying one is a different route",
    "project_apply_filters": "re-reading the filters cannot lose a version: earlier versions are kept",
}
# Note: relink, remove, recover and applying a policy are NOT exempt — each of them must both send
# `confirm: true` from the page and check for it in the route. Putting them here with the note
# "guarded, it does require confirm" is how this checker first failed to notice one of them losing
# its guard: an exemption list is a place where a check quietly stops checking.


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", default=str(Path(__file__).resolve().parent.parent))
    ap.add_argument("--app", default="app")
    args = ap.parse_args()

    root = Path(args.root)
    app = root / args.app
    ui_path = app / "ui" / "app.js"
    api_path = app / "src" / "api.rs"
    if not ui_path.is_file() or not api_path.is_file():
        print("FAIL: cannot read the page or the routes")
        return 2
    ui = ui_path.read_text(encoding="utf-8")
    api = api_path.read_text(encoding="utf-8")
    # Comments are not code. Round 291 taught this the hard way: a check that greps for a name finds
    # it in a comment that says the name must *not* be used, and then reports the opposite of the
    # truth. Both files are read with their comments removed first.
    def strip_comments(text: str) -> str:
        text = re.sub(r"/\*.*?\*/", "", text, flags=re.S)
        return re.sub(r"(?m)^\s*//.*$", "", text)

    ui_code = strip_comments(ui)
    api_code = strip_comments(api)

    problems = []

    # 1. every route the page calls exists
    routes = {m[1] for m in re.findall(r'\("(GET|POST)", "(/api/[^"]+)"\)', api_code)}
    for call in re.findall(r"api\('([^']+)'", ui):
        path = "/api/" + call.split("?")[0].strip("/")
        if path.startswith("/api/job"):
            path = "/api/job"
        if path not in routes:
            problems.append(f"the page calls {path}, which the server does not answer")

    # 2. every control has a handler
    acts = set(re.findall(r'data-act="([a-z0-9-]+)"', ui)) | set(re.findall(r"data-act='([a-z0-9-]+)'", ui))
    handled = set(re.findall(r"case '([a-z0-9-]+)':", ui))
    for name in ("ext", "preset", "file", "lang", "config-bool", "filter-input", "pick-project"):
        if f"act === '{name}'" in ui:
            handled.add(name)
    for a in sorted(acts - handled):
        problems.append(f"the page can raise data-act=\"{a}\" and nothing handles it")

    # 3. every route that changes something asks first.
    #
    # The route table is the authority for which handler answers which path; the handler's own body
    # is then read for its guard, and the page is read for the confirmation it sends. Both halves are
    # required: a guard the page never satisfies, or a page that sends a confirmation no route checks
    # for, is a bug either way.
    handler_paths: dict = {}
    for meth, path_, handler in re.findall(r'\("(GET|POST)", "(/api/[^"]+)"\) => (\w+)', api_code):
        handler_paths.setdefault(handler, []).append(path_)
    for name, body in re.findall(r"fn (\w+)\(ctx: &Arc<Ctx>, req: &Request\) -> Response \{(.*?)\n\}", api_code, re.S):
        if not re.search(r"core_write\(ctx", body):
            continue
        paths = handler_paths.get(name, [])
        page_sends = any(
            re.search(rf"api\('{re.escape(p[5:])}'[^;]*confirm: true", ui_code, re.S) for p in paths
        )
        guarded = "confirmed(req)" in body
        if not (page_sends and guarded) and name not in CONFIRM_EXEMPT:
            problems.append(
                f"{name} ({', '.join(paths) or 'no route'}): writes without a confirmation "
                f"(page sends one: {page_sends}, route checks for one: {guarded})"
            )

    # 4. the project selectors remember the choice
    for sid in ("ret-project", "drill-project", "recover-project"):
        m = re.search(rf"id=\"{sid}\"[^>]*>(.*?)</select>", ui_code, re.S)
        if not m:
            problems.append(f"the selector #{sid} is missing")
            continue
        if "toolProject()" not in m.group(1):
            problems.append(f"#{sid} does not show the remembered project, so a redraw would move it back to the first one")

    # 5. never the browser's own dialogs
    for bad in ("window.prompt(", "window.alert(", "window.confirm(", "prompt(", "alert("):
        if re.search(r"(?<![\w.])" + re.escape(bad), ui_code):
            problems.append(f"the page uses {bad} — the macOS shell has no handler for it")

    # 6. the About screen shows the identity the *server* sends (round 302)
    #
    # The owner asked for his name, his address and the sentence about the purpose to be wherever the
    # program describes itself. The page is where a person reads it, so the About view and the
    # sidebar footer must show what the server sends — and the page must not hold its own copy of the
    # name (that copy is what drifts; tools/brand_check.py checks the page's bytes for it).
    def body_of(name: str) -> str:
        m = re.search(r"function " + name + r"\(\) \{(.*?)\n\}\n", ui_code, re.S)
        return m.group(1) if m else ""

    about = body_of("viewAbout")
    if not about:
        problems.append("the page has no About view")
    else:
        for token in ("app.author", "app.authorEmail", "whatItIs"):
            if token not in about:
                problems.append(f"the About view does not show {token} — the identity the owner asked for is missing there")
        if "unavailable" not in about:
            problems.append("the About view cannot say that the core could not be read, so it would show an empty space")
    foot = body_of("renderSideFoot")
    if not foot:
        problems.append("the page has no sidebar footer")
    elif "author" not in foot:
        problems.append("the sidebar footer does not name the author")

    if problems:
        print("FAIL")
        for p in problems:
            print("  - " + p)
        return 1
    print(f"PASS: {len(routes)} routes, {len(acts)} controls, every one accounted for")
    print("PASS: irreversible routes ask first; selectors remember the project; no browser dialogs")
    return 0


if __name__ == "__main__":
    sys.exit(main())
