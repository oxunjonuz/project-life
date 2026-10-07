#!/usr/bin/env python3
"""Inspect a built "Project Life.app" the way a person on the Mac would receive it.

Nothing here needs macOS: it reads the bundle as bytes and checks the things that decide whether the
app can start at all on an Apple Silicon Mac —

  * the bundle layout and Info.plist that launchd reads;
  * every executable is an arm64 Mach-O with a valid ad-hoc signature over its own bytes;
  * every library it asks for is one that ships with macOS (so nothing else must be installed);
  * the interface files are actually inside the interface binary;
  * the shell knows where its two helper binaries are.

It prints a verdict per check and exits non-zero if any of them fails.

    python3 tools/verify_macos_app.py "/path/Project Life.app"
"""
import json
import plistlib
import struct
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from macho_inspect import parse                      # noqa: E402
from macho_signature_check import check as sig_check  # noqa: E402

SYSTEM_PREFIXES = ("/usr/lib/", "/System/Library/", "/System/iOSSupport/")


class Report:
    def __init__(self):
        self.rows = []
        self.bad = 0

    def add(self, name, ok, detail=""):
        self.rows.append((name, bool(ok), detail))
        if not ok:
            self.bad += 1
        print(f"[{'PASS' if ok else 'FAIL'}] {name}" + (f" — {detail}" if detail else ""))
        return ok


def main():
    if len(sys.argv) < 2:
        print("usage: verify_macos_app.py <path to Project Life.app>")
        return 2
    app = Path(sys.argv[1])
    r = Report()
    r.add("the bundle exists", app.is_dir(), str(app))
    if not app.is_dir():
        return 1

    contents = app / "Contents"
    macos = contents / "MacOS"
    res = contents / "Resources"
    r.add("Contents/MacOS and Contents/Resources exist", macos.is_dir() and res.is_dir())

    # ---- Info.plist
    plist_path = contents / "Info.plist"
    plist = {}
    if plist_path.is_file():
        try:
            plist = plistlib.loads(plist_path.read_bytes())
            r.add("Info.plist parses", True, f"{len(plist)} keys")
        except Exception as e:
            r.add("Info.plist parses", False, str(e))
    else:
        r.add("Info.plist exists", False)

    exe_name = plist.get("CFBundleExecutable", "")
    main_exe = macos / exe_name if exe_name else None
    r.add("CFBundleExecutable points at a file in Contents/MacOS",
          bool(main_exe and main_exe.is_file()), exe_name)
    r.add("CFBundlePackageType is APPL", plist.get("CFBundlePackageType") == "APPL",
          str(plist.get("CFBundlePackageType")))
    r.add("LSMinimumSystemVersion is set", bool(plist.get("LSMinimumSystemVersion")),
          str(plist.get("LSMinimumSystemVersion")))
    ats = plist.get("NSAppTransportSecurity", {}) or {}
    r.add("the window may load its loopback server (NSAllowsLocalNetworking)",
          bool(ats.get("NSAllowsLocalNetworking")), json.dumps(ats))
    icon = plist.get("CFBundleIconFile")
    r.add("the icon named in Info.plist is present",
          bool(icon) and (res / icon).is_file(), str(icon))

    # ---- the three executables
    binaries = [p for p in [main_exe,
                            res / "projectlife",
                            res / "projectlife-ui"] if p]
    for b in binaries:
        if not b.is_file():
            r.add(f"{b.name} is inside the bundle", False, str(b))
            continue
        r.add(f"{b.name} is present and executable", bool(b.stat().st_mode & 0o111), str(b.relative_to(app)))
        info = parse(str(b))
        r.add(f"{b.name} is an arm64 Mach-O executable",
              info["arch"] == "arm64" and info["filetype"] == 2,
              f"{info['arch']} type {info['filetype']}")
        sig = sig_check(str(b))
        r.add(f"{b.name}: every signed page re-hashes to the recorded value",
              not sig["problems"] and sig.get("pages_verified", 0) > 0,
              f"{sig.get('pages_verified')} pages, identifier {sig.get('identifier')}, ad-hoc {sig.get('adhoc')}")
        foreign = [d for d in info["dylibs"] if not d.startswith(SYSTEM_PREFIXES)]
        r.add(f"{b.name} asks only for libraries that ship with macOS",
              not foreign, ", ".join(info["dylibs"]) or "none")
        r.add(f"{b.name} has no rpaths that would need a private copy",
              not info["rpaths"], ", ".join(info["rpaths"]) or "none")

    # ---- the interface is inside the interface binary
    uisrv = res / "projectlife-ui"
    if uisrv.is_file():
        blob = uisrv.read_bytes()
        for needle, what in ((b"<title>Project Life</title>", "the window's HTML"),
                             (b"--accent: #245FC4", "the design's colours"),
                             (b"/api/", "the route prefix the window calls"),
                             (b"watch/start", "the observation routes"),
                             (b"pl://pick-folder", "the native folder panel bridge")):
            r.add(f"the interface binary carries {what}", needle in blob, needle.decode("utf-8", "replace"))

    # ---- the shell knows where the helpers are
    shell = main_exe
    if shell and shell.is_file():
        text = shell.read_bytes()
        for needle in (b"projectlife-ui", b"projectlife", b"pick-folder", b"forURLScheme",
                       b"Application Support/ProjectLife", b"NSStatusBar"):
            r.add(f"the shell references {needle.decode()}", needle in text)

    # ---- the shell's own code, read out of the delivered bytes
    #
    # The window "could not start" on the owner's Mac because the shell asked the resource folder
    # for a folder *inside* the resource folder. A reader of the source can miss that line, and a
    # check of the source cannot see a bundle that was built from an older copy of it. So it is
    # checked here, in the bytes that ship: the selector naming a second "Resources" must be absent,
    # and the sentences the fixed lookup can print must be present. They are stored as UTF-16
    # NSString constants — searching for them as ASCII bytes finds nothing, which is a mistake I
    # made myself on this very file, so the check reads both ways and says which one hit.
    if shell and shell.is_file():
        blob = shell.read_bytes()
        r.add("the shell does not ask Resources for a folder called Resources",
              b"pathForResource:ofType:inDirectory:" not in blob,
              "the selector that named a second Resources is absent from the shipped bytes")
        utf16 = blob.decode("utf-16-le", "ignore")
        markers = ["Contents/Resources", "no such file", "a folder, not a program", "not executable"]
        found = [m for m in markers if (m in utf16) or (m.encode() in blob)]
        r.add("the shipped shell carries the fixed helper lookup, not an older one",
              len(found) == len(markers),
              f"{len(found)}/{len(markers)} markers in the arm64 shell: {', '.join(found)}")

    # ---- the shell can say which build it is (round 298)
    #
    # The owner reported three failures against a bundle that had been replaced hours earlier, and
    # nothing on screen said so. The line exists in the shell's own menu and About panel; it is
    # checked here in the delivered bytes, for the same reason as the helper lookup above — a check
    # of the source cannot see a bundle built from an older copy of it.
    if shell and shell.is_file():
        blob = shell.read_bytes()
        utf16 = blob.decode("utf-16-le", "ignore")
        for marker in ["This build:", "build %@"]:
            r.add(f"the shipped shell carries {marker!r}",
                  (marker in utf16) or (marker.encode() in blob))
        r.add("the shipped shell keeps the build line for the menu and the About panel",
              b"buildShort" in blob and b"buildCheck" in blob)
        core_bin = res / "projectlife"
        if core_bin.is_file():
            # The version is read from the delivery's VERSION file rather than typed here: a check
            # that hardcodes the version is a check that has to be edited every round, and round 301
            # left this one saying 0.9.3 while the delivery said 0.9.4.
            want = (Path(__file__).resolve().parent.parent / "VERSION").read_text().strip().encode()
            r.add("the shipped core carries the delivery's own version",
                  want in core_bin.read_bytes(), f"VERSION file says {want.decode()}")

    # ---- nothing else is required
    r.add("no node_modules / npm / rust toolchain is shipped",
          not any("node_modules" in str(p) for p in app.rglob("*")))

    print()
    print(json.dumps({"checks": len(r.rows), "failed": r.bad}, indent=2))
    return 1 if r.bad else 0


if __name__ == "__main__":
    sys.exit(main())
