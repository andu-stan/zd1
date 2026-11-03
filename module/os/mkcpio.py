#!/usr/bin/env python3
"""A directory as a newc initramfs, reproducibly: entries sorted, owner root, mtime 0, permissions from the tree,
symlinks kept; plus the device nodes the kernel needs before mdev runs (2.6.24 has no devtmpfs). No root needed.

    mkcpio.py ROOTDIR OUT.cpio          an initramfs
    mkcpio.py --slot DIR OUT.cpio       a system slot's root: /dev/console and /dev/null its only device nodes (the
                                        kernel opens the console before init mounts /dev)
"""

import os
import stat
import sys

NODES = [
    ("dev/console", 5, 1),
    ("dev/null", 1, 3),
    ("dev/zero", 1, 5),
    ("dev/ttyS0", 4, 64),
    ("dev/ttyS1", 4, 65),
    ("dev/urandom", 1, 9),
    ("dev/random", 1, 8),
    ("dev/mem", 1, 1),
    ("dev/tty", 5, 0),
]


def entry(name, mode, data, ino, rdev=(0, 0)):
    nb = name.encode() + b"\0"
    h = b"070701" + b"".join(
        b"%08X" % x for x in (ino, mode, 0, 0, 1, 0, len(data), 0, 0, rdev[0], rdev[1], len(nb), 0)
    )
    b = h + nb
    b += b"\0" * (-len(b) % 4) + data
    return b + b"\0" * (-len(b) % 4)


def main():
    slot = "--slot" in sys.argv
    root, out = [a for a in sys.argv[1:] if a != "--slot"][:2]
    parts, ino, names = [], 1, set()
    for dp, dns, fs in os.walk(root):
        dns.sort()
        for n in sorted(dns) + sorted(fs):
            full = os.path.join(dp, n)
            name = os.path.relpath(full, root)
            st = os.lstat(full)
            if stat.S_ISLNK(st.st_mode):
                parts.append(entry(name, 0o120777, os.readlink(full).encode(), ino))
            elif stat.S_ISDIR(st.st_mode):
                parts.append(entry(name, 0o40000 | (st.st_mode & 0o7777), b"", ino))
            elif stat.S_ISREG(st.st_mode):
                parts.append(entry(name, 0o100000 | (st.st_mode & 0o7777), open(full, "rb").read(), ino))
            else:
                continue
            names.add(name)
            ino += 1
    for name, maj, mi in [n for n in NODES if n[0] in ("dev/console", "dev/null")] if slot else NODES:
        if name not in names:
            parts.append(entry(name, 0o20666 if name != "dev/mem" else 0o20600, b"", ino, (maj, mi)))
            ino += 1
    parts.append(entry("TRAILER!!!", 0, b"", 0))
    open(out, "wb").write(b"".join(parts))
    print("%s: %d entries" % (out, ino - 1))


if __name__ == "__main__":
    main()
