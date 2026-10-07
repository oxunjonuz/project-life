#!/usr/bin/env python3
"""Does `macho_inspect.py` read the load-command numbers correctly?

This exists because it did not. In round 295 the reader printed `platform: None` for a binary that
carries `LC_BUILD_VERSION`, and I nearly built a theory on it ("the binary declares no platform")
before dumping the raw command numbers by hand: the reader's table had `0x32` listed as
`LC_DYLD_CHAINED_FIXUPS` and `0x2F` as `LC_BUILD_VERSION`, when the real header says

    LC_VERSION_MIN_TVOS    0x2F
    LC_BUILD_VERSION       0x32
    LC_DYLD_INFO_ONLY     0x80000022   (not LC_DYLD_ENVIRONMENT, which is 0x27)

An instrument that mislabels what it reads is worse than no instrument, because it is believed. So
here is a Mach-O built by hand, byte by byte, and the only question asked of the reader is whether it
names what is there. Run: python3 tools/test_macho_inspect.py
"""
import json
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
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


def header(ncmds, sizeofcmds):
    return struct.pack("<IiiIIIII", 0xFEEDFACF, 0x0100000C, 0, 2, ncmds, sizeofcmds, 0xA00085, 0)


def build_version_cmd(platform=1, minos=(11, 0, 0), sdk=(15, 5, 0), tool=3, tool_version=(0, 0, 4)):
    v = (minos[0] << 16) | (minos[1] << 8) | minos[2]
    s = (sdk[0] << 16) | (sdk[1] << 8) | sdk[2]
    tv = (tool_version[0] << 16) | (tool_version[1] << 8) | tool_version[2]
    body = struct.pack("<IIII", 0x32, 32, platform, v) + struct.pack("<II", s, 1) + struct.pack("<II", tv, tool)
    return body


def dyld_info_only_cmd():
    # cmd, cmdsize, then five (offset,size) pairs — the shape that was mislabelled
    return struct.pack("<II", 0x80000022, 48) + b"\x00" * 40


def read(data):
    with tempfile.NamedTemporaryFile(suffix=".macho", delete=False) as f:
        f.write(data)
        path = f.name
    out = subprocess.run([sys.executable, str(HERE / "macho_inspect.py"), path],
                         capture_output=True, text=True)
    Path(path).unlink()
    if out.returncode != 0:
        return None, out.stderr
    return json.loads(out.stdout), ""


def main():
    print("the reader, checked against bytes built here\n")

    cmd = build_version_cmd()
    data = header(1, len(cmd)) + cmd
    j, err = read(data)
    check("the file parses at all", j is not None, err.strip()[:120])
    if j:
        check("LC_BUILD_VERSION is named as such", j["commands"] == ["LC_BUILD_VERSION"], str(j["commands"]))
        check("the platform is read from the command", j.get("platform") == "macOS", str(j.get("platform")))
        check("the minimum OS version is read", j.get("min_os") == "11.0.0", str(j.get("min_os")))
        check("the SDK version is read", j.get("sdk") == "15.5.0", str(j.get("sdk")))
        check("the number of tool entries is kept", j.get("ntools") == 1, str(j.get("ntools")))

    cmd = dyld_info_only_cmd()
    data = header(1, len(cmd)) + cmd
    j, err = read(data)
    check("0x80000022 is LC_DYLD_INFO_ONLY, not LC_DYLD_ENVIRONMENT",
          bool(j) and j["commands"] == ["LC_DYLD_INFO_ONLY"], str(j["commands"]) if j else err[:120])

    # And the same question of the real binaries, if they are here: the reader must find a platform.
    app = Path("/work/projectlife/dist/Project Life.app")
    for name in ["Contents/MacOS/ProjectLife", "Contents/Resources/projectlife",
                 "Contents/Resources/projectlife-ui", "Contents/Resources/pl-mcp"]:
        f = app / name
        if not f.exists():
            continue
        out = subprocess.run([sys.executable, str(HERE / "macho_inspect.py"), str(f)],
                             capture_output=True, text=True)
        j = json.loads(out.stdout)
        check(f"{f.name}: platform macOS, min 11.0", j["platform"] == "macOS" and j["min_os"] == "11.0.0",
              f"{j['platform']} / {j['min_os']}")

    print(f"\n{PASS} PASS, {FAIL} FAIL")
    return 1 if FAIL else 0


if __name__ == "__main__":
    sys.exit(main())
