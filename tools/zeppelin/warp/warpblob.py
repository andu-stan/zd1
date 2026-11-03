#!/usr/bin/env python3
"""Extract a Warp1 program (docs/module/warp.md): Nokia's WarpSol image, the symbol wslsepp_warp (length:
wslsepp_warp_length) in /home/root/JukeBlox_Release_ELX of the Nokia rootfs, linked at 0x61FC0000. Its output is kept
as blobs/module/nokia-hd1/wslsepp_warp.bin (blobs/README.md); this redoes it from the ELF (archive.org item
nokia-hd-1-dump, its JFFS2 rootfs extracted with jefferson). Reads the file as data only.

    python3 -I tools/zeppelin/warp/warpblob.py JukeBlox_Release_ELX OUT
"""

import argparse
import struct


def symbols(d):
    (shoff,) = struct.unpack_from("<I", d, 0x20)
    shentsize, shnum = struct.unpack_from("<HH", d, 0x2E)
    secs = [struct.unpack_from("<10I", d, shoff + i * shentsize) for i in range(shnum)]
    for s in secs:
        if s[1] == 11:  # SHT_DYNSYM
            strtab = secs[s[6]]
            for o in range(s[4], s[4] + s[5], 16):
                name, value, size, info, other, shndx = struct.unpack_from("<IIIBBH", d, o)
                n = d[strtab[4] + name : d.index(b"\0", strtab[4] + name)].decode()
                yield n, value


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("elf", metavar="JukeBlox_Release_ELX")
    ap.add_argument("out", metavar="OUT")
    a = ap.parse_args()
    d = open(a.elf, "rb").read()
    sym = dict(symbols(d))
    (phoff,) = struct.unpack_from("<I", d, 0x1C)
    (phnum,) = struct.unpack_from("<H", d, 0x2C)
    segs = [struct.unpack_from("<8I", d, phoff + 32 * i) for i in range(phnum)]

    def off(va):
        for t, o, v, _p, fs, _ms, _fl, _al in segs:
            if t == 1 and v <= va < v + fs:
                return va - v + o
        raise SystemExit("%#x not in a loaded segment" % va)

    (n,) = struct.unpack_from("<I", d, off(sym["wslsepp_warp_length"]))
    blob = d[off(sym["wslsepp_warp"]) : off(sym["wslsepp_warp"]) + n]
    open(a.out, "wb").write(blob)
    print("wslsepp_warp: %d bytes -> %s" % (n, a.out))


if __name__ == "__main__":
    main()
