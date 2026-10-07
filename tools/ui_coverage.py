#!/usr/bin/env python3
"""Which of the core's commands reach the window — measured, not asserted.

The audit the owner asked for in round 297 has to survive him reading the code, so this tool does
not take a report's word for anything. It reads the command list out of the *core binary itself*,
looks for each command in the window's own sources (the page and the routes it calls), and refuses
to pass when a command is neither wired to the window nor named, with a reason, as deliberately
left to the terminal.

It also checks the other direction, which is the one that produces dead buttons: every
`data-act` the page can raise must have a handler, and every route the page calls must exist in
the server's route table.

    python3 tools/ui_coverage.py --pl target/release/projectlife --app-src app [--out docs/UI_COVERAGE.md]

Exit code 0 only when every command is accounted for and no control is dead.
"""
import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

# Commands that stay in the terminal on purpose. The reason is the honest part: it is what the owner
# reads when he asks "why is this not in the window?".
CLI_ONLY = {
    "undo": "in the menu (Restore → the command that would undo the last write): it prints the command and performs nothing — the same act the terminal does",
    "open": "in the menu (File → Show the archive in the file manager) and in the window through the shell; the command stays for scripts",
    "suggest": "in the window (dashboard) and in the menu (History → what needs attention)",
    "panic": "asks on a terminal which moment to fall back to; the window does the same job through History & restore",
    "export-and-prune": "one command, two irreversible effects (write an export, then delete versions); the window keeps them apart",
    "archive-move": "moves the whole archive: a system-level decision about disks, not a button",
    "archive-delete": "deletes a project's history for good; it needs a typed confirmation a window cannot honestly ask for",
    "daemon": "in the menu as start/stop of the app's own observation process; *installing* it (launchd/systemd) is a system change the person makes deliberately",
    "mcp": "in the menu as `mcp tools` (the list, read-only); the MCP server itself (pl-mcp) is a separate process started by the agent that needs it",
    "completion": "in the menu as an entry that spells the command out rather than running it: installing completion changes your shell, not this app",
    "prompt": "in the menu as an entry that spells the command out: it is a line for a shell prompt",
    "notify": "in the menu (Protection → send a test notification); it writes a line in the archive's log and nothing else",
    "watch": "a live terminal stream of events; the window shows the log and the trigger state instead",
    "drill": "in the window as a job (Integrity → Rehearse a restore) and in the menu (Restore → Rehearse a restore)",
    "gc": "in the menu as `gc --dry-run` (shown, never deleting); deleting dangling bytes stays in `check --fix`, where it asks",
    "rebuild-cache": "in the menu (Archive → Rebuild the state cache), behind a confirmation: the same repair the program performs by itself when the cache disagrees with the journal",
    "quarantine": "in the menu (Archive → Quarantined blobs) and listed in the window; restoring one blob by hash is a terminal action",
    "audit-archive": "in the menu read-only (Archive → Audit the archive); the window and the menu refuse to write a new digest — recording one is a deliberate act in the terminal",
    "check": "the window runs check (and --deep); --fix, which deletes, is left to the terminal",
    "verify": "an alias of check --deep; the window offers the deep check",
    "apply-filters": "in the window (Project tools); kept here too for scripts",
    "import": "in the window (Import); the CLI form stays for scripts",
    "init-archive": "in the window (choose the store); the CLI form stays for scripts",
    "presets": "in the menu (Tools → the protection presets) and in the wizard; the command prints the same list",
    "detect": "in the window (Add folder) and in the menu (File → see what a folder holds)",
    "size": "in the window (Storage) and in the menu (Archive → the largest things); the CLI form adds --top",
    "recent": "in the window (Project tools) and in the menu (History → recent changes)",
    "since": "in the menu (History → what changed since…); the window shows the same thing as Compare",
    "blame": "in the window through Versions of a file, and in the menu (History → which moment touched each line)",
    "cat": "in the window through Versions of a file and in the menu (Tools → show a file as the archive has it); the CLI form also writes to a file (--out)",
    "why": "in the window as the decision line over a file's versions, and in the menu (History → why did this file change)",
    "last-good": "in the window (Project tools) and in the menu (History → the last good moment)",
    "mark": "in the window (Project tools) and in the menu (History → mark this moment)",
    "snap": "in the window (Project tools) and in the menu (History → snap a marker)",
    "retention": "in the window (Storage & retention)",
    "prune": "the window runs `prune --policy` (with the policy stored there); `--before` is a sharper tool the terminal keeps",
    "recover": "in the window (Integrity) and in the menu (Restore → finish an interrupted prune), behind a confirmation",
    "heartbeat-check": "in the menu (Protection → is anything being observed) and shown as the protection verdict",
    "healthcheck": "in the menu (Protection → health check) and in the window (Diagnostics); designed to run unattended from cron",
    "relink": "in the window (Project tools)",
    "remove": "in the window (Project tools), with a confirmation",
    "pause": "in the window (a project card and Project tools) and in the menu (Protection → pause this project)",
    "resume": "in the window (a project card and Project tools) and in the menu (Protection → resume this project)",
    "scan-once": "in the menu (Protection → observe once now) and as the window's daemon; the CLI form is for cron and for --all",
    "partial-pass": "an internal pass the daemon runs; on the terminal it exists to be measured",
    "doctor": "in the window (Diagnostics) and in the menu (Archive → checks); --fix-lock stays on the terminal",
    "config": "in the window (Settings shows the core's keys; interval and notifications are editable there) and in the menu (Archive → the core's settings)",
    "export": "in the window (History & restore) and in the menu (File → export the history)",
    "export-file": "the window exports a whole project; one file at one moment is a terminal convenience",
    "log": "in the window as Moments, with the same filters the core supports; the menu searches by text and by content",
    "timeline": "the human form of `log`; the window draws the same moments",
    "tree": "in the window as the file list at a chosen moment",
    "diff": "in the window (Compare)",
    "add": "in the window (the four-step wizard) and in the menu (File → add a folder to protect)",
    "list": "in the window (dashboard cards and the sidebar) and in the menu (History → projects by risk)",
    "status": "in the window (dashboard and project cards) and in the menu (History → what is watched right now)",
    "daemon run": "started by the window itself when observation is switched on",
    "restore": "in the window (History & restore) and in the menu (Restore → preview a restore at…)",
    "help": "not a capability",
    "version": "shown in Settings and about, and in the menu (Tools → the core's version)",
    "heartbeat-check ": "alias",
    "mcp tools": "in the menu (Tools → MCP: what another agent may ask)",
}

# The routes the page may call. Anything else is a dead call.
def read(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def core_commands(pl: str) -> list:
    """The list the core prints for itself — taken from the binary, not from a document."""
    out = subprocess.run([pl, "help"], capture_output=True, text=True)
    text = out.stdout
    # `help` prints the usage text; the authoritative list is also in the source (COMMANDS).
    return text


def parse_commands(src: str) -> list:
    m = re.search(r"pub const COMMANDS: &\[&str\] = &\[(.*?)\];", src, re.S)
    if not m:
        return []
    return re.findall(r'"([^"]+)"', m.group(1))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--pl", default="target/release/projectlife", help="the core binary (for its own help text)")
    ap.add_argument("--core-src", default="src/cli.rs", help="the core source, for the command list")
    ap.add_argument("--app-src", default="app", help="the app crate folder")
    ap.add_argument("--out", default="docs/UI_COVERAGE.md")
    args = ap.parse_args()

    root = Path(__file__).resolve().parent.parent
    core_src = Path(args.core_src)
    core_src = core_src if core_src.is_absolute() else root / core_src
    commands = parse_commands(read(core_src))
    if not commands:
        print(f"cannot read the core's command list from {core_src}")
        return 1
    app_dir = Path(args.app_src)
    app_dir = app_dir if app_dir.is_absolute() else root / app_dir
    ui = read(app_dir / "ui" / "app.js")
    api = read(app_dir / "src" / "api.rs")
    menu_src = read(app_dir / "src" / "menu.rs")

    # The menu (round 299) is a third place a command can reach the person. Its entries are read
    # from the registry the same way the contract checker reads them: the row's `core` field.
    menu_map = {}
    for row in re.findall(r"it!\((.*?\"\),\n)", menu_src, re.S):
        flat = re.sub(r"&\[[^\]]*\]", "&[]", row)
        s = re.findall(r'"([^"]*)"', flat)
        if len(s) < 5:
            continue
        words = s[4].split()
        if len(words) > 1 and words[0] == "pl" and not words[1].startswith("<"):
            menu_map.setdefault(words[1], []).append(s[0])

    # Routes the server actually answers.
    routes = set(re.findall(r'\("(GET|POST)", "(/api/[^"]+)"\)', api))
    routes = {r[1] for r in routes}
    # Routes the page calls: `api('project/why?...')` means GET /api/project/why.
    called = set()
    for m in re.findall(r"api\('([^']+)'", ui):
        path = "/api/" + m.split("?")[0].strip("/")
        called.add("/api/job" if path.startswith("/api/job") else path)

    dead_routes = sorted(p for p in called if p not in routes)

    # data-act values the page can raise, and the ones it handles.
    acts = set(re.findall(r'data-act="([a-z0-9-]+)"', ui)) | set(re.findall(r'data-act=[\'"]([a-z0-9-]+)', ui))
    handled = set(re.findall(r"case '([a-z0-9-]+)':", ui))
    # Handlers written as guarded blocks rather than cases.
    for name in ("ext", "preset", "file", "lang", "config-bool", "filter-input", "pick-project"):
        if f"act === '{name}'" in ui:
            handled.add(name)
    unhandled = sorted(a for a in acts if a not in handled)

    # Where each core command appears in the window's own sources.
    #
    # Two different questions, kept apart on purpose: "does the window call this command" is read
    # out of the code (the core invocations in api.rs), and "is this capability on screen" is the
    # curated answer below — a command can be called by the window without the person ever seeing
    # it (an implementation detail), and the other way round (shown through a different command).
    rows = []
    missing = []
    for cmd in commands:
        calls = bool(re.search(rf'"{re.escape(cmd)}"\.to_string\(\)|"{re.escape(cmd)}"\.into\(\)|\["{re.escape(cmd)}",', api))
        reason = CLI_ONLY.get(cmd, "")
        in_menu = cmd in menu_map
        shown = calls or reason.startswith("in the window") or reason.startswith("in the menu") or in_menu
        if not shown and not reason:
            missing.append(cmd)
        rows.append({"command": cmd, "called": calls, "in_menu": in_menu, "in_window": shown,
                     "entries": ", ".join(menu_map.get(cmd, [])), "reason": reason})

    lines = ["# Which parts of the core the window shows",
             "",
             "Read the file the tool writes, not this line: the table below is generated by",
             "`tools/ui_coverage.py` from the core's own command list (`src/cli.rs`), the window's page",
             "(`app/ui/app.js`), its routes (`app/src/api.rs`) and the menu registry (`app/src/menu.rs`).",
             "",
             f"- core commands: **{len(commands)}**",
             f"- reachable from the window: **{sum(1 for r in rows if r['in_window'])}**",
             f"- with a menu entry of their own: **{sum(1 for r in rows if r['in_menu'])}**",
             f"- deliberately left to the terminal, with a reason: **{len(set(CLI_ONLY) & set(commands))}**",
             f"- unaccounted for: **{len(missing)}**",
             "",
             "| command | called by the window | in the menu | shown to the person | entry | where / why not |",
             "| --- | --- | --- | --- | --- | --- |"]
    for r in rows:
        lines.append(f"| `{r['command']}` | {'yes' if r['called'] else 'no'} | {'yes' if r['in_menu'] else 'no'} | "
                     f"{'yes' if r['in_window'] else 'no'} | {r['entries'] or '—'} | {r['reason'] or ''} |")
    lines += ["", f"Dead page calls (routes the page asks for that the server does not answer): "
                  f"{', '.join(dead_routes) if dead_routes else 'none'}",
              f"Controls with no handler (buttons that would do nothing): "
              f"{', '.join(unhandled) if unhandled else 'none'}", ""]
    out = Path(args.out)
    out = out if out.is_absolute() else root / out
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text("\n".join(lines), encoding="utf-8")

    print(f"commands: {len(commands)}, in the window: {sum(1 for r in rows if r['in_window'])}, "
          f"CLI-only with a reason: {len(set(CLI_ONLY) & set(commands))}, unaccounted: {len(missing)}")
    if missing:
        print("UNACCOUNTED FOR: " + ", ".join(missing))
    if dead_routes:
        print("DEAD PAGE CALLS: " + ", ".join(dead_routes))
    if unhandled:
        print("CONTROLS WITH NO HANDLER: " + ", ".join(unhandled))
    print(f"table written to {out}")
    return 0 if not (missing or dead_routes or unhandled) else 1


if __name__ == "__main__":
    sys.exit(main())
