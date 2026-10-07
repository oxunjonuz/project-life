#!/usr/bin/env python3
"""What does this filesystem report to inotify, and for what?

The partial-pass tests behaved differently in /work than in /tmp: files created BEFORE the watch was
installed produced notifications in /work. This probe asks the kernel directly, without Project Life
in the picture: install a watch on a fresh directory, then read a file, stat it, read the directory,
and create a file, and print the raw masks that arrive.

Usage: python3 tools/inotify_probe.py [directory]
"""
import ctypes
import ctypes.util
import os
import struct
import sys
import time

libc = ctypes.CDLL(ctypes.util.find_library("c") or "libc.so.6", use_errno=True)

IN_CREATE = 0x00000100
IN_DELETE = 0x00000200
IN_MODIFY = 0x00000002
IN_ATTRIB = 0x00000004
IN_CLOSE_WRITE = 0x00000008
IN_MOVED_FROM = 0x00000040
IN_MOVED_TO = 0x00000080
IN_ACCESS = 0x00000001
IN_OPEN = 0x00000020
IN_ISDIR = 0x40000000

NAMES = {
    IN_ACCESS: "IN_ACCESS", IN_MODIFY: "IN_MODIFY", IN_ATTRIB: "IN_ATTRIB",
    IN_CLOSE_WRITE: "IN_CLOSE_WRITE", IN_OPEN: "IN_OPEN", IN_MOVED_FROM: "IN_MOVED_FROM",
    IN_MOVED_TO: "IN_MOVED_TO", IN_CREATE: "IN_CREATE", IN_DELETE: "IN_DELETE",
    IN_ISDIR: "IN_ISDIR",
}
MASK = IN_CREATE | IN_MODIFY | IN_CLOSE_WRITE | IN_DELETE | IN_MOVED_FROM | IN_MOVED_TO | IN_ATTRIB | IN_OPEN | IN_ACCESS | IN_ISDIR


def names(mask):
    return "|".join(n for bit, n in sorted(NAMES.items()) if mask & bit)


def drain(fd):
    out = []
    while True:
        try:
            data = os.read(fd, 8192)
        except BlockingIOError:
            break
        if not data:
            break
        off = 0
        while off < len(data):
            wd, mask, cookie, length = struct.unpack("iIII", data[off:off + 16])
            name = data[off + 16:off + 16 + length].split(b"\0")[0].decode("utf-8", "replace")
            out.append((wd, mask, name))
            off += 16 + length
    return out


def main():
    d = sys.argv[1] if len(sys.argv) > 1 else "/tmp/inotify-probe"
    os.makedirs(d, exist_ok=True)
    old = os.path.join(d, "old.txt")
    with open(old, "w") as f:
        f.write("created before the watch\n")
    time.sleep(0.2)

    fd = libc.inotify_init1(os.O_NONBLOCK | os.O_CLOEXEC)
    if fd < 0:
        raise SystemExit("inotify_init1 failed: %s" % os.strerror(ctypes.get_errno()))
    wd = libc.inotify_add_watch(fd, d.encode(), MASK)
    if wd < 0:
        raise SystemExit("inotify_add_watch failed: %s" % os.strerror(ctypes.get_errno()))
    drain(fd)

    print("watching %s (mask %s)" % (d, names(MASK)))
    steps = []
    with open(old, "rb") as f:
        f.read()
    steps.append("read an existing file")
    os.stat(old)
    steps.append("stat an existing file")
    os.listdir(d)
    steps.append("list the directory")
    with open(old, "ab") as f:
        f.write(b"more\n")
    steps.append("append to an existing file")
    new = os.path.join(d, "new.txt")
    with open(new, "w") as f:
        f.write("created under the watch\n")
    steps.append("create a new file")
    os.rename(new, os.path.join(d, "renamed.txt"))
    steps.append("rename it")
    os.remove(os.path.join(d, "renamed.txt"))
    steps.append("delete it")
    time.sleep(0.3)

    got = drain(fd)
    print("events, in order (%d):" % len(got))
    for wd_, mask, name in got:
        print("   wd=%s mask=0x%08x %-60s name=%r" % (wd_, mask, names(mask), name))
    print()
    for s in steps:
        print("   step: %s" % s)
    os.close(fd)


if __name__ == "__main__":
    main()
