#!/usr/bin/env python3
"""Stamp the firmware's identification (src/version.c) into a built image: the commit and its date, found by the
block's magic in the .elf, .bin and .hex alike. The nix build runs it after linking (nix/default.nix), so a new
commit only re-stamps (on the host) instead of rebuilding the firmware (on a Linux builder).

  stamp.py --version V --built B IN_DIR OUT_DIR     every NAME.{elf,bin,hex} of IN_DIR stamped into OUT_DIR, .map copied

`zd1 image FILE` reads the identification back.
"""

import argparse
import os
import shutil
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "tools"))
from zeppelin.fw import ihex  # noqa: E402

MAGIC = b"ZD1-FW-IDENT-01\0"  # char magic[16]; char version[41]; char built[25];
FIELDS = (("version", 41), ("built", 25))


def block(version, built):
    out = b""
    for (name, size), v in zip(FIELDS, (version, built)):
        b = v.encode()
        if len(b) >= size:
            sys.exit("stamp: %s %r longer than %d bytes" % (name, v, size - 1))
        out += b + b"\0" * (size - len(b))
    return out


def find(data):
    i = data.find(MAGIC)
    if i < 0 or data.find(MAGIC, i + 1) >= 0:
        return None
    return i


def stamp_hex(text, stamp):
    lo, mem = ihex.image(text)
    i = find(bytes(mem))
    if i is None:
        sys.exit("stamp: the ident block is not in the hex (or twice)")
    start, end = lo + i + len(MAGIC), lo + i + len(MAGIC) + len(stamp)
    lines = text.splitlines()
    for n, typ, a, d in ihex.records(text):
        if typ != ihex.DATA or a + len(d) <= start or a >= end:
            continue
        d = bytearray(d)
        for k in range(len(d)):
            if start <= a + k < end:
                d[k] = stamp[a + k - start]
        lines[n] = ihex.line(a & 0xFFFF, ihex.DATA, d)
    nl = "\r\n" if "\r\n" in text else "\n"  # objcopy writes CRLF: keep the file's own line ends
    out = nl.join(lines) + nl
    if bytes(ihex.image(out)[1][i + len(MAGIC) : i + len(MAGIC) + len(stamp)]) != stamp:
        sys.exit("stamp: the stamped hex does not read back")
    return out


def stamp_binary(data, stamp):
    i = find(data)
    if i is None:
        sys.exit("stamp: the ident block is not in the image (or twice)")
    p = i + len(MAGIC)
    return data[:p] + stamp + data[p + len(stamp) :]


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--version", required=True)
    ap.add_argument("--built", default="")
    ap.add_argument("src", metavar="IN_DIR")
    ap.add_argument("dst", metavar="OUT_DIR")
    a = ap.parse_args()
    src, dst = a.src, a.dst
    stamp = block(a.version, a.built)
    os.makedirs(dst, exist_ok=True)
    for f in sorted(os.listdir(src)):
        p, q = os.path.join(src, f), os.path.join(dst, f)
        if f.endswith((".elf", ".bin")):
            open(q, "wb").write(stamp_binary(open(p, "rb").read(), stamp))
        elif f.endswith(".hex"):
            open(q, "w", newline="").write(stamp_hex(open(p, newline="").read(), stamp))
        else:
            shutil.copyfile(p, q)
        print(
            "stamped %s: %s %s" % (f, a.version, a.built) if f.endswith((".elf", ".bin", ".hex")) else "copied %s" % f
        )


if __name__ == "__main__":
    main()
