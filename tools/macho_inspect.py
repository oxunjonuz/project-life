#!/usr/bin/env python3
"""Read a Mach-O file without Apple tools: architecture, load commands, the libraries it
asks for, and whether it carries a code signature.

Written from the Mach-O format description (loader.h), not from lld's writer, so it is an
independent reader of what the cross-linker produced.  It does not verify the signature
cryptographically -- `rcodesign verify` does that -- it reports what is inside the file.
"""
import struct
import sys

MH_MAGIC_64 = 0xFEEDFACF
LC_SEGMENT_64 = 0x19
LC_CODE_SIGNATURE = 0x1D
LC_LOAD_DYLIB = 0xC
LC_LOAD_WEAK_DYLIB = 0x80000018
LC_RPATH = 0x8000001C
LC_ID_DYLIB = 0xD
LC_MAIN = 0x80000028
LC_BUILD_VERSION = 0x32
LC_VERSION_MIN_MACOSX = 0x24
LC_VERSION_MIN_IPHONEOS = 0x25
LC_VERSION_MIN_TVOS = 0x2F
LC_VERSION_MIN_WATCHOS = 0x30

CPU_TYPES = {0x0100000C: "arm64", 0x01000007: "x86_64"}
PLATFORMS = {1: "macOS", 2: "iOS", 3: "tvOS", 4: "watchOS", 6: "macCatalyst"}
LC_NAMES = {
    0x01: "LC_SEGMENT", 0x02: "LC_SYMTAB", 0x0B: "LC_DYSYMTAB", 0x0C: "LC_LOAD_DYLIB",
    0x0D: "LC_ID_DYLIB", 0x0E: "LC_LOAD_DYLINKER", 0x0F: "LC_ID_DYLINKER",
    0x1B: "LC_UUID", 0x1D: "LC_CODE_SIGNATURE", 0x20: "LC_UNIXTHREAD",
    0x22: "LC_DYLD_INFO", 0x80000022: "LC_DYLD_INFO_ONLY",
    0x24: "LC_VERSION_MIN_MACOSX", 0x25: "LC_VERSION_MIN_IPHONEOS",
    0x26: "LC_FUNCTION_STARTS", 0x27: "LC_DYLD_ENVIRONMENT",
    0x29: "LC_DATA_IN_CODE", 0x2A: "LC_SOURCE_VERSION", 0x2B: "LC_DYLIB_CODE_SIGN_DRS",
    0x2F: "LC_VERSION_MIN_TVOS", 0x30: "LC_VERSION_MIN_WATCHOS", 0x31: "LC_NOTE",
    0x32: "LC_BUILD_VERSION",
    0x80000018: "LC_LOAD_WEAK_DYLIB", 0x8000001C: "LC_RPATH",
    0x80000028: "LC_MAIN", 0x80000033: "LC_DYLD_EXPORTS_TRIE",
    0x80000034: "LC_DYLD_CHAINED_FIXUPS",
}



def cstr(buf, off):
    end = buf.index(b"\0", off)
    return buf[off:end].decode("utf-8", "replace")


def parse(path):
    with open(path, "rb") as fh:
        buf = fh.read()
    magic, cputype, cpusub, filetype, ncmds, sizeofcmds, flags, _res = struct.unpack_from(
        "<IiiIIIII", buf, 0)
    if magic != MH_MAGIC_64:
        raise SystemExit("not a 64-bit little-endian Mach-O: magic=%08x" % magic)
    out = {
        "path": path,
        "arch": CPU_TYPES.get(cputype, hex(cputype)),
        "cpusubtype": cpusub,
        "filetype": filetype,
        "ncmds": ncmds,
        "sizeofcmds": sizeofcmds,
        "flags": flags,
        "dylibs": [],
        "rpaths": [],
        "segments": [],
        "has_code_signature": False,
        "code_signature": None,
        "platform": None,
        "min_os": None,
        "entry": None,
        "commands": [],
    }
    off = 32
    for _ in range(ncmds):
        cmd, cmdsize = struct.unpack_from("<II", buf, off)
        body = buf[off:off + cmdsize]
        out["commands"].append(LC_NAMES.get(cmd, hex(cmd)))
        if cmd == LC_SEGMENT_64:
            segname = cstr(body, 8)
            _vmaddr, vmsize, _fileoff, filesize = struct.unpack_from("<QQQQ", body, 24)
            out["segments"].append({"name": segname, "vmsize": vmsize, "filesize": filesize})
        elif cmd in (LC_LOAD_DYLIB, LC_LOAD_WEAK_DYLIB, LC_ID_DYLIB):
            out["dylibs"].append(cstr(body, struct.unpack_from("<I", body, 8)[0]))
        elif cmd == LC_RPATH:
            out["rpaths"].append(cstr(body, struct.unpack_from("<I", body, 8)[0]))
        elif cmd == LC_CODE_SIGNATURE:
            dataoff, datasize = struct.unpack_from("<II", body, 8)
            out["has_code_signature"] = True
            out["code_signature"] = {"offset": dataoff, "size": datasize}
            blob = buf[dataoff:dataoff + datasize]
            if len(blob) >= 12:
                # Every code-signature structure is big-endian, unlike the Mach-O header itself.
                bmagic, blen, bcount = struct.unpack_from(">III", blob, 0)
                out["code_signature"]["magic"] = hex(bmagic)
                out["code_signature"]["declared_length"] = blen
                out["code_signature"]["count"] = bcount
                indices = []
                for i in range(bcount):
                    slot_type, slot_off = struct.unpack_from(">II", blob, 12 + i * 8)
                    if not slot_off:
                        continue
                    sd_magic, sd_len = struct.unpack_from(">II", blob, slot_off)
                    entry = {"type": slot_type, "magic": hex(sd_magic), "len": sd_len}
                    if slot_type == 0:  # CodeDirectory
                        cd = blob[slot_off:slot_off + sd_len]
                        cd_version, cd_flags = struct.unpack_from(">II", cd, 8)
                        ident_off = struct.unpack_from(">I", cd, 20)[0]
                        n_special, n_code = struct.unpack_from(">II", cd, 24)
                        code_limit = struct.unpack_from(">I", cd, 32)[0]
                        hash_size, hash_type, platform, page_log2 = struct.unpack_from(">BBBB", cd, 36)
                        entry.update({
                            "version": cd_version,
                            "flags": cd_flags,
                            "identifier": cstr(cd, ident_off),
                            "special_slots": n_special,
                            "code_slots": n_code,
                            "code_limit": code_limit,
                            "hash_size": hash_size,
                            "hash_type": {1: "sha1", 2: "sha256", 3: "sha256-truncated", 4: "sha384"}.get(hash_type, hash_type),
                            "platform": platform,
                            "page_size_log2": page_log2,
                        })
                    indices.append(entry)
                out["code_signature"]["blobs"] = indices
        elif cmd in (LC_VERSION_MIN_MACOSX, LC_VERSION_MIN_IPHONEOS, LC_VERSION_MIN_TVOS, LC_VERSION_MIN_WATCHOS):
            ver, sdk = struct.unpack_from("<II", body, 8)
            plats = {LC_VERSION_MIN_MACOSX: "macOS", LC_VERSION_MIN_IPHONEOS: "iOS", LC_VERSION_MIN_TVOS: "tvOS", LC_VERSION_MIN_WATCHOS: "watchOS"}
            out["platform"] = plats.get(cmd, hex(cmd))
            out["min_os"] = "%d.%d.%d" % ((ver >> 16) & 0xFFFF, (ver >> 8) & 0xFF, ver & 0xFF)
            out["sdk"] = "%d.%d.%d" % ((sdk >> 16) & 0xFFFF, (sdk >> 8) & 0xFF, sdk & 0xFF)
            out["build_version_command"] = LC_NAMES.get(cmd, hex(cmd))
        elif cmd == LC_BUILD_VERSION:
            plat, minos, sdk, ntools = struct.unpack_from("<IIII", body, 8)
            out["platform"] = PLATFORMS.get(plat, plat)
            out["ntools"] = ntools
            out["build_version_command"] = "LC_BUILD_VERSION"
            out["min_os"] = "%d.%d.%d" % ((minos >> 16) & 0xFFFF, (minos >> 8) & 0xFF, minos & 0xFF)
            out["sdk"] = "%d.%d.%d" % ((sdk >> 16) & 0xFFFF, (sdk >> 8) & 0xFF, sdk & 0xFF)
        elif cmd == LC_MAIN:
            out["entry"] = struct.unpack_from("<Q", body, 8)[0]
        off += cmdsize
    return out


def main():
    import json
    for path in sys.argv[1:]:
        print(json.dumps(parse(path), indent=2))


if __name__ == "__main__":
    main()
