#!/usr/bin/env python3
"""The Nokia HD-1 kernel's symbols, decoded from the kernel image's own compressed kallsyms tables.

    python3 -I tools/zeppelin/jtag/kallsyms.py [KERNEL] [--at ADDR]...

Prints `addr type name` lines (the /proc/kallsyms format), or with --at the symbol each address falls in. KERNEL
defaults to blobs/module/nokia-hd1/kernel.bin (the uncompressed 2.6.24.7 Image, base 0xC0008000), the kernel
module/linux boots. Library: `load()`, `Symbols(...).at(addr)`.

Linux 2.6.24's scripts/kallsyms lays out, each 16-aligned: addresses (u32 per symbol, sorted), the symbol count,
names (a length byte, then token indices; the first character is the type), markers (the names offset of every 256th
symbol), the token table (256 NUL-terminated strings) and the token index (u16 offsets into it). The tables are found
by looking for a count preceded by that many sorted kernel addresses, and every part is checked before use.
"""

import argparse
import bisect
import os
import struct
import sys

REPO = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", ".."))
KERNEL = os.path.join(REPO, "blobs", "module", "nokia-hd1", "kernel.bin")
KBASE = 0xC0000000


def align(x):
    return (x + 15) & ~15


def _addresses_before(b, off, n):
    """The n sorted kernel addresses ending just before off (16-aligned table), or None."""
    start = (off - 4 * n) & ~15
    if start < 0 or align(start + 4 * n) != off:
        return None
    a = struct.unpack_from("<%dI" % n, b, start)
    if a[0] < KBASE or any(x > y for x, y in zip(a, a[1:])):
        return None
    return a


def _decode(b, off, addrs):
    n = len(addrs)
    p = align(off + 4)
    names, starts = [], []
    for _ in range(n):
        starts.append(p - align(off + 4))
        ln = b[p]
        names.append(b[p + 1 : p + 1 + ln])
        p += 1 + ln
    p = align(p)
    nmark = (n + 255) // 256
    markers = struct.unpack_from("<%dI" % nmark, b, p)
    if list(markers) != starts[::256]:
        raise ValueError("markers don't match the names")
    p = align(p + 4 * nmark)
    table_start, tokens = p, []
    for _ in range(256):
        e = b.index(b"\0", p)
        tokens.append(b[p:e])
        p = e + 1
    p = align(p)
    index = struct.unpack_from("<256H", b, p)
    offs, q = [], 0
    for t in tokens:
        offs.append(q)
        q += len(t) + 1
    if list(index) != offs:
        raise ValueError("token index doesn't match the token table at 0x%x" % table_start)
    syms = []
    for a, nm in zip(addrs, names):
        s = b"".join(tokens[i] for i in nm).decode("ascii")
        if len(s) < 2 or not s.isprintable() or s[0] not in "aAbBdDgGrRsStTuUvVwW?":
            raise ValueError("undecodable name at 0x%x: %r" % (a, s))
        syms.append((a, s[0], s[1:]))
    return syms


def load(path=KERNEL):
    """[(address, type, name)] from a kernel image, sorted by address. Raises ValueError if no table decodes."""
    b = open(path, "rb").read()
    for off in range(16, len(b) - 4, 16):
        (n,) = struct.unpack_from("<I", b, off)
        if not 256 <= n <= 200000 or 4 * n > off:
            continue
        addrs = _addresses_before(b, off, n)
        if addrs is None:
            continue
        try:
            return _decode(b, off, addrs)
        except (ValueError, IndexError, struct.error, UnicodeDecodeError):
            continue
    raise ValueError("%s: no kallsyms tables that decode" % path)


class Symbols:
    """Address -> function lookup over (address, type, name) entries."""

    def __init__(self, syms, types="tTwW"):
        syms = sorted((a, n) for a, t, n in syms if t in types)
        self.addrs = [a for a, _ in syms]
        self.names = [n for _, n in syms]

    def at(self, a):
        i = bisect.bisect_right(self.addrs, a) - 1
        return "%08x = %s+0x%x" % (a, self.names[i], a - self.addrs[i]) if i >= 0 else "%08x" % a


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("kernel", nargs="?", default=KERNEL)
    ap.add_argument("--at", action="append", type=lambda s: int(s, 16), help="symbolise this address (hex)")
    a = ap.parse_args()
    try:
        syms = load(a.kernel)
    except ValueError as e:
        sys.exit(str(e))
    if a.at:
        s = Symbols(syms)
        for x in a.at:
            print(s.at(x))
        return
    for addr, t, n in syms:
        print("%08x %s %s" % (addr, t, n))


if __name__ == "__main__":
    main()
