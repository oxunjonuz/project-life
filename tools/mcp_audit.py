"""Does the agent-facing code contain a write path?

The MCP server promises that the reading surface cannot write. That claim is worth exactly as much
as the check behind it, so this script reads the source of the reading surface and looks for the
symbols that write: journal appends, blob puts, metadata saves, restores, prunes, scans.

It also runs a positive control on itself: a copy of each file with a write call injected by hand
must be flagged, otherwise the check is measuring nothing.

Usage:
  python3 tools/mcp_audit.py [--root .]
Exit code 0 = clean (and the control was caught), 1 = a write path was found or the control was missed.
"""
import argparse
import os
import re
import sys
import tempfile

# The files the agent-facing process is allowed to consist of.
READ_SURFACE = ["src/mcp.rs", "src/bin/pl_mcp.rs"]

# Calls that write to the archive or to a project. A reading surface must contain none of them.
FORBIDDEN = [
    "events::append",
    "save_meta",
    "util::write_atomic",
    "write_atomic",
    "swap_journal",
    "recover_prune",
    "lifecycle::prune",
    "prune_plan",
    "retention::apply",
    "store::blob_path",
    "restore::execute",
    "lifecycle::import",
    "lifecycle::export",
    "lifecycle::archive_delete",
    "scan::scan_project",
    "daemon::run_cycle",
    "daemon::run_cycle_triggered",
    "daemon::run_daemon",
    "fs::write",
    "fs::remove_file",
    "fs::remove_dir_all",
    "fs::rename",
    "File::create",
    "open_writer",
]

# The single file this process may write, and the only two calls allowed to make it.
ALLOWED_WRITE_FILE = os.path.join("logs", "mcp.log")
ALLOWED_CALLS = ["create_dir_all", "OpenOptions"]


def strip_code(text):
    """Remove comments and string literals so that prose cannot trip (or hide) a match."""
    out = []
    i = 0
    n = len(text)
    while i < n:
        c = text[i]
        if c == '/' and i + 1 < n and text[i + 1] == '/':
            while i < n and text[i] != '\n':
                i += 1
        elif c == '/' and i + 1 < n and text[i + 1] == '*':
            i += 2
            while i + 1 < n and not (text[i] == '*' and text[i + 1] == '/'):
                i += 1
            i += 2
        elif c == '"':
            i += 1
            while i < n and text[i] != '"':
                i += 2 if text[i] == '\\' else 1
            i += 1
            out.append('""')
        else:
            out.append(c)
            i += 1
    return "".join(out)


def violations(path, text):
    code = strip_code(text)
    src_lines = text.splitlines()
    found = []
    for sym in FORBIDDEN:
        for m in re.finditer(re.escape(sym), code):
            line = code[: m.start()].count("\n") + 1
            found.append("%s:%d uses %s" % (path, line, sym))
    # The request log is the one file this process may write. The two calls that do it are allowed
    # only where the log file is named in the neighbourhood.
    for call in ALLOWED_CALLS:
        for m in re.finditer(re.escape(call), code):
            line = code[: m.start()].count("\n") + 1
            window = "\n".join(src_lines[max(0, line - 8) : line + 8])
            if "mcp.log" not in window:
                found.append("%s:%d uses %s outside the request log" % (path, line, call))
    return found


def run(root, files):
    problems = []
    for rel in files:
        p = os.path.join(root, rel)
        with open(p, encoding="utf-8") as f:
            text = f.read()
        problems.extend(violations(rel, text))
    return problems


def self_test(root):
    """Inject one write call into a copy of each file; the auditor must catch it."""
    ok = True
    for rel in READ_SURFACE:
        with open(os.path.join(root, rel), encoding="utf-8") as f:
            text = f.read()
        injected = text + "\nfn _injected_for_the_control(){ let _ = crate::restore::execute; }\n"
        found = violations(rel, injected)
        if not found:
            print("CONTROL FAILED for %s: an injected restore::execute was not noticed" % rel)
            ok = False
        else:
            print("control ok for %s: %s" % (rel, found[0]))
    return ok


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", default=".")
    args = ap.parse_args()
    problems = run(args.root, READ_SURFACE)
    print("read surface audited: %s" % ", ".join(READ_SURFACE))
    if problems:
        print("WRITE PATH FOUND in the read surface:")
        for p in problems:
            print("  - %s" % p)
    else:
        print("no write symbol from the forbidden list appears in the read surface")
    control = self_test(args.root)
    if problems or not control:
        print("MCP AUDIT: FAILED")
        return 1
    print("MCP AUDIT: CLEAN (and the control was caught)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
