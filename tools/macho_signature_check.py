#!/usr/bin/env python3
"""Check a Mach-O ad-hoc signature by recomputing it.

`macho_inspect.py` says *what* is in the file; this says whether the signature that is there
actually covers the bytes that are there. It recomputes every code-page hash in the CodeDirectory
from the file itself and compares, and -- for a bundle -- checks that the Info.plist entry of the
special slots matches the Info.plist next to it.

This is a second implementation of the format, written from Apple's documented layout, so it does
not share the blind spot of the linker that wrote the signature.

    python3 tools/macho_signature_check.py <binary> [--app "/path/Project Life.app"]
"""
import hashlib
import struct
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from macho_inspect import parse  # noqa: E402

CSMAGIC_EMBEDDED_SIGNATURE = 0xFADE0CC0
CSMAGIC_CODEDIRECTORY = 0xFADE0C02
CSMAGIC_BLOBWRAPPER = 0xFADE0B01
CSSLOT_INFOSLOT = 1
CSSLOT_REQUIREMENTS = 2
CSSLOT_RESOURCEDIR = 3
CSSLOT_SIGNATURESLOT = 0x10000

HASHES = {1: hashlib.sha1, 2: hashlib.sha256, 3: hashlib.sha256, 4: hashlib.sha384}


def check(path, app_dir=None):
    info = parse(str(path))
    out = {"path": str(path), "arch": info["arch"], "problems": [], "notes": []}
    if not info["has_code_signature"]:
        out["problems"].append("no code signature at all (arm64 will not run without one)")
        return out
    raw = Path(path).read_bytes()
    off = info["code_signature"]["offset"]
    size = info["code_signature"]["size"]
    super_blob = raw[off:off + size]
    magic, length, count = struct.unpack_from(">III", super_blob, 0)
    if magic != CSMAGIC_EMBEDDED_SIGNATURE:
        out["problems"].append(f"superblob magic {magic:#x} is not an embedded signature")
        return out
    if length != size:
        # Apple's own tools pad the signature area so the file can be re-signed in place: the
        # superblob states its real length and everything after it, up to the LC's datasize, is
        # zeros. Anything other than zeros there is a real fault.
        pad = super_blob[length:]
        if pad and set(pad) != {0}:
            out["problems"].append(f"superblob length {length} < datasize {size} and the gap is not zero padding")
        else:
            out["notes"].append(f"{size - length} bytes of zero padding after the signature (normal)")
    blobs = {}
    for i in range(count):
        slot_type, slot_off = struct.unpack_from(">II", super_blob, 12 + i * 8)
        blobs[slot_type] = super_blob[slot_off:]

    cd = blobs.get(0)
    if cd is None:
        out["problems"].append("no CodeDirectory in the signature")
        return out

    cd_magic, cd_len, cd_version, cd_flags = struct.unpack_from(">IIII", cd, 0)
    if cd_magic != CSMAGIC_CODEDIRECTORY:
        out["problems"].append(f"CodeDirectory magic {cd_magic:#x} is wrong")
    hash_offset, ident_offset, n_special, n_code = struct.unpack_from(">IIII", cd, 16)
    code_limit = struct.unpack_from(">I", cd, 32)[0]
    hash_size, hash_type, platform, page_log2 = struct.unpack_from(">BBBB", cd, 36)
    ident = cd[ident_offset:cd.index(b"\0", ident_offset)].decode("utf-8", "replace")
    out["identifier"] = ident
    out["hash_type"] = {1: "sha1", 2: "sha256", 3: "sha256-truncated", 4: "sha384"}.get(hash_type, hash_type)
    out["page_size"] = 1 << page_log2
    out["code_slots"] = n_code
    out["adhoc"] = not blobs.get(CSSLOT_SIGNATURESLOT)
    out["has_requirements"] = CSSLOT_REQUIREMENTS in blobs
    out["has_resource_dir_hash"] = CSSLOT_RESOURCEDIR in blobs
    out["has_info_plist_slot"] = n_special >= 1

    if hash_type not in HASHES:
        out["problems"].append(f"unknown hash type {hash_type}")
        return out
    page = 1 << page_log2
    if code_limit > len(raw):
        out["problems"].append(f"codeLimit {code_limit} is larger than the file ({len(raw)})")
        return out

    # Every page of the signed region, hashed again here.
    bad_pages = []
    data = raw[:code_limit]
    for i in range(n_code):
        chunk = data[i * page:(i + 1) * page]
        want = cd[hash_offset + i * hash_size: hash_offset + (i + 1) * hash_size]
        got = HASHES[hash_type](chunk).digest()[:hash_size]
        if got != want:
            bad_pages.append(i)
    out["pages_verified"] = n_code - len(bad_pages)
    if bad_pages:
        out["problems"].append(f"{len(bad_pages)} of {n_code} code pages do not hash to the recorded value "
                               f"(first: page {bad_pages[0]})")
    # The signature must not cover itself: codeLimit is where the signed region ends.
    out["signed_region_ends_at_signature"] = abs(code_limit - off) <= (1 << page_log2)

    if app_dir and n_special >= 1:
        plist = Path(app_dir) / "Contents" / "Info.plist"
        if plist.is_file():
            want = cd[hash_offset - hash_size: hash_offset]
            got = HASHES[hash_type](plist.read_bytes()).digest()[:hash_size]
            out["info_plist_signed"] = got == want
            if got != want:
                out["problems"].append("the Info.plist hash in the special slots does not match Info.plist")
        else:
            out["notes"].append("no Info.plist found next to the binary")
    return out


def main():
    import json
    args = sys.argv[1:]
    app = None
    if "--app" in args:
        i = args.index("--app")
        app = args[i + 1]
        del args[i:i + 2]
    rc = 0
    for path in args:
        r = check(path, app)
        print(json.dumps(r, indent=2))
        if r["problems"]:
            rc = 1
    sys.exit(rc)


if __name__ == "__main__":
    main()
