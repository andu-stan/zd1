#!/usr/bin/env python3
"""Install every section of a .bcd into an emulator NAND image the way BL2's flasher leaves it after a module relay
(XModem over SPI), without pushing the bytes through both emulated CPUs (`zemu run IMAGE.bcd`): tools/zeppelin/fw/bcd.py
install(). Checked against a NAND written by the emulated real BL2 in a full relay with --compare.
Emulation only: the result is a NAND image for the emulator, never written to hardware.

usage: bcdinstall.py NAND.bin IMAGE.bcd [OUT.bin]          (OUT defaults to NAND.bin, in place)
       bcdinstall.py --compare A.bin B.bin                  (diff two NAND images by partition; exit 1 if they differ)
"""

import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "tools"))
from zeppelin.fw import bcd  # noqa: E402


def install(nand_path, bcd_path, out_path):
    nand = bytearray(open(nand_path, "rb").read())
    bcd.install(nand, open(bcd_path, "rb").read(), log=lambda s: print("  " + s))
    open(out_path, "wb").write(nand)


def compare(a_path, b_path):
    a, b = open(a_path, "rb").read(), open(b_path, "rb").read()
    if len(a) != len(b):
        print("sizes differ: %d vs %d" % (len(a), len(b)))
        return 1
    diffs = []
    i = 0
    while i < len(a):  # runs of differing bytes, coalesced within 64 B
        if a[i] != b[i]:
            j = i
            while j < len(a) and (a[j] != b[j] or a[j : j + 64] != b[j : j + 64]):
                j += 1
            diffs.append((i, j))
            i = j
        i += 1
    for s, e in diffs[:20]:
        print("  differ 0x%07x..0x%07x (%d B): %s | %s" % (s, e, e - s, a[s : s + 16].hex(), b[s : s + 16].hex()))
    print("%s: %d differing run(s)" % ("IDENTICAL" if not diffs else "DIFFERENT", len(diffs)))
    return 1 if diffs else 0


if __name__ == "__main__":
    if len(sys.argv) == 4 and sys.argv[1] == "--compare":
        sys.exit(compare(sys.argv[2], sys.argv[3]))
    if len(sys.argv) not in (3, 4):
        sys.exit(__doc__)
    install(sys.argv[1], sys.argv[2], sys.argv[3] if len(sys.argv) == 4 else sys.argv[1])
