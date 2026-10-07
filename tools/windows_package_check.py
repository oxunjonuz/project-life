#!/usr/bin/env python3
"""What can be checked about the Windows build without a Windows machine.

This is the honest half of the Windows deliverable. There is no Windows here, so nothing in the
package has been *run*: this tool reads the delivered bytes and answers the questions that can be
answered by reading them — the shape of each executable, what it imports, what words it carries,
which resources it has, whether the package's own checksums agree with the files, and whether the
package's names would survive a case-insensitive filesystem (Windows and, measured, this project's
own volume: writing `casetest.txt` overwrote `CaseTest.txt`).

The checks that need Windows are in the package itself, as `verify_windows.ps1`, which the owner runs
there. This tool says so at the end rather than implying the two are the same thing.

    python3 tools/windows_package_check.py [--pkg DIR] [--json]
"""

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import zipfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DEFAULT = os.path.join(ROOT, "dist/windows/ProjectLife-windows-x86_64")

RESULTS = {"passed": 0, "failed": 0}


def ok(msg):
    RESULTS["passed"] += 1
    print(f"  PASS  {msg}")


def bad(msg):
    RESULTS["failed"] += 1
    print(f"  FAIL  {msg}")


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def objdump(path, *args):
    tool = "x86_64-w64-mingw32-objdump"
    try:
        return subprocess.run([tool, *args, path], capture_output=True, text=True, timeout=120).stdout
    except FileNotFoundError:
        return ""


def wide_strings(path):
    try:
        out = subprocess.run(["strings", "-a", "-e", "l", path], capture_output=True, text=True,
                             timeout=120).stdout
    except FileNotFoundError:
        return ""
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--pkg", default=DEFAULT)
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()
    pkg = args.pkg
    report = {"package": pkg}

    print(f"package: {pkg}")
    if not os.path.isdir(pkg):
        print("no such folder — build it first: sh app/windows/build_windows_app.sh")
        return 2

    names = sorted(os.listdir(pkg))
    print(f"files: {', '.join(names)}")
    report["files"] = names

    print("\n== 1. the package can survive a case-insensitive filesystem")
    lower = {}
    clash = []
    for n in names:
        k = n.lower()
        if k in lower:
            clash.append((lower[k], n))
        lower[k] = n
    if clash:
        bad(f"two names differ only in case and would collapse into one: {clash}")
    else:
        ok(f"{len(names)} names, none differing only in case (Windows is case-insensitive)")
    if "ProjectLife.exe" in names and "projectlife.exe" in names:
        bad("the shell and the core share a name modulo case — one would overwrite the other")
    if all(x in names for x in ["ProjectLife.exe", "pl.exe", "pl-ui.exe", "WebView2Loader.dll"]):
        ok("the shell (ProjectLife.exe) and the helpers (pl.exe, pl-ui.exe) are distinct names")

    print("\n== 2. the shell is a GUI program that asks for WebView2 and WinHTTP")
    shell = os.path.join(pkg, "ProjectLife.exe")
    head = objdump(shell, "-p")
    m = re.search(r"Subsystem\s+(\w+)\s+\((.*?)\)", head)
    if m and "Windows GUI" in m.group(2):
        ok(f"ProjectLife.exe is a Windows GUI executable ({m.group(2)})")
    else:
        bad(f"ProjectLife.exe is not a GUI executable: {m.group(0) if m else 'no subsystem line'}")
    imports = set(re.findall(r"DLL Name: (\S+)", head))
    for want, why in [("WebView2Loader.dll", "the WebView2 loader it ships beside itself"),
                      ("WINHTTP.dll", "the local HTTP client it talks to the server with"),
                      ("SHELL32.dll", "the tray icon"),
                      ("ADVAPI32.dll", "the registry entry for autostart")]:
        if want in imports:
            ok(f"imports {want} ({why})")
        else:
            bad(f"does not import {want} ({why})")
    unexpected = {i for i in imports if not re.match(r"^[A-Za-z0-9_.-]+\.dll$", i)}
    if unexpected:
        bad(f"imports something that is not a DLL name: {unexpected}")
    report["shell_imports"] = sorted(imports)

    print("\n== 3. the words the window promises are in the binary")
    ws = wide_strings(shell)
    for phrase, why in [("Quit completely", "the entry that stops the observation"),
                        ("Start with Windows", "the autostart entry"),
                        ("Open window", "bringing the window back from the tray"),
                        ("Protection status", "the state in a person's terms")]:
        if phrase in ws:
            ok(f"carries {phrase!r} ({why})")
        else:
            bad(f"is missing {phrase!r} ({why})")
    ascii_strings = subprocess.run(["strings", "-a", shell], capture_output=True, text=True).stdout
    for phrase, why in [("selftest:", "the selftest the owner runs on Windows"),
                        ("menu?shell=1&plain=1", "the menu it reads from the server"),
                        ("watch/start", "the route that starts observation"),
                        ("shutdown", "the route that stops it")]:
        if phrase in ascii_strings:
            ok(f"carries {phrase!r} ({why})")
        else:
            bad(f"is missing {phrase!r} ({why})")

    print("\n== 4. what a person sees in Explorer: the icon and the version block")
    sections = objdump(shell, "-h")
    if ".rsrc" in sections:
        size = 0
        for line in sections.splitlines():
            if ".rsrc" in line:
                parts = line.split()
                for p in parts:
                    if re.match(r"^[0-9a-f]{8}$", p):
                        size = int(p, 16)
                        break
        ok(f"has a resource section ({size} bytes) — the icon and the version block live there")
    else:
        bad("has no resource section: no icon and no version block in Explorer")
    for phrase in ["FileVersion", "ProductName", "LegalCopyright", "OriginalFilename"]:
        if phrase in ws:
            ok(f"the version block carries {phrase}")
        else:
            bad(f"the version block has no {phrase}")
    if "not signed" in ws or "Authenticode" in ws:
        ok("the version block says in words that the file is unsigned")
    else:
        bad("nothing in the version block says the file is unsigned")

    print("\n== 5. the package's own checksums agree with its own files")
    sums = os.path.join(pkg, "SHA256SUMS.txt")
    if os.path.exists(sums):
        checked = 0
        for line in open(sums):
            line = line.strip()
            if not line:
                continue
            want, name = line.split(None, 1)
            path = os.path.join(pkg, name.strip())
            if not os.path.exists(path):
                bad(f"SHA256SUMS.txt names a file that is not here: {name}")
                continue
            got = sha256(path)
            if got != want:
                bad(f"{name}: SHA256SUMS.txt says {want}, the file is {got}")
            else:
                checked += 1
        if checked:
            ok(f"{checked} file(s) match SHA256SUMS.txt exactly")
        report["checksummed"] = checked
    else:
        bad("no SHA256SUMS.txt in the package")

    print("\n== 6. the zip carries the same bytes as the folder")
    zip_path = os.path.join(os.path.dirname(pkg.rstrip("/")), os.path.basename(pkg.rstrip("/")) + ".zip")
    if os.path.exists(zip_path):
        with zipfile.ZipFile(zip_path) as z:
            inside = {n.split("/", 1)[1]: z.read(n) for n in z.namelist() if "/" in n and not n.endswith("/")}
        same, differ = 0, []
        for name, data in inside.items():
            disk = os.path.join(pkg, name)
            if not os.path.exists(disk):
                differ.append(name)
                continue
            if hashlib.sha256(data).hexdigest() == sha256(disk):
                same += 1
            else:
                differ.append(name)
        if differ:
            bad(f"the zip and the folder disagree about: {differ}")
        else:
            ok(f"the zip's {same} file(s) are byte-identical to the folder's")
        rep = [n for n in z.namelist()]
        zl = [n.lower() for n in rep]
        if len(set(zl)) != len(zl):
            bad("the zip would collapse two names on a case-insensitive filesystem")
        else:
            ok("no two names in the zip differ only in case")
    else:
        bad(f"no zip beside the folder ({zip_path})")

    print("\n== 7. the scripts that run on Windows are at least readable here")
    for name in ["install.ps1", "uninstall.ps1", "verify_windows.ps1", "README.txt"]:
        p = os.path.join(pkg, name)
        if not os.path.exists(p):
            bad(f"missing {name}")
            continue
        text = open(p, encoding="utf-8", errors="replace").read()
        if text.count("{") != text.count("}"):
            bad(f"{name}: braces do not balance ({text.count('{')} open, {text.count('}')} close)")
        else:
            ok(f"{name} is present and its braces balance ({len(text)} bytes)")
    vw = os.path.join(pkg, "verify_windows.ps1")
    if os.path.exists(vw):
        text = open(vw, encoding="utf-8", errors="replace").read()
        if "RESULT=PASS" in text and "SHA256SUMS" in text:
            ok("verify_windows.ps1 checks the selftest result and the delivered checksums")
        else:
            bad("verify_windows.ps1 does not check both the selftest and the checksums")
        if re.search(r"\bprojectlife\.exe\b", text.replace("pl.exe", "")):
            bad("verify_windows.ps1 calls an executable name the package does not have")
        else:
            ok("verify_windows.ps1 names only executables that are in this package")

    print("\n== 8. what this tool cannot say")
    print("  note  nothing in this package has been RUN: there is no Windows machine here. The window,")
    print("        the tray icon, the registry entry, the WebView2 runtime and the PowerShell scripts")
    print("        were compiled, linked and read — not executed. verify_windows.ps1 is the check that")
    print("        finishes the job on the machine that has Windows (see docs/PLATFORMS.md).")

    print(f"\nWINDOWS PACKAGE: {RESULTS['passed']} passed, {RESULTS['failed']} failed")
    report.update(RESULTS)
    report["executed_on_windows"] = False
    out = os.path.join(ROOT, "evidence")
    os.makedirs(out, exist_ok=True)
    with open(os.path.join(out, "windows_package_check_301.json"), "w") as f:
        json.dump(report, f, indent=2)
    if args.json:
        print(json.dumps(report, indent=2))
    return 1 if RESULTS["failed"] else 0


if __name__ == "__main__":
    sys.exit(main())
