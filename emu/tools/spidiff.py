#!/usr/bin/env python3
"""Compare the SPI byte stream the MCU clocked out (zd1 trace 'SPI0 xx -> yy') with what the module firmware read from
its RX FIFO (dm870 trace 'R? b1002e00 = ...', DM870_TRACE_ONLY=b1002e00). Idle 0xFF bytes the MCU clocks to read are
included on both sides (the slave FIFO receives them too). usage: spidiff.py zd1.txt dm870.trace"""

import re
import sys

mcu = [
    int(m.group(1), 16)
    for m in re.finditer(r"SPI0 ([0-9a-f]{2}) -> [0-9a-f]{2}", open(sys.argv[1], errors="replace").read())
]
mod = []
for line in open(sys.argv[2], errors="replace"):  # the dm870 trace folds repeats into "   ... xN"
    m = re.match(r"R\d b1002e00 = ([0-9a-f]+)", line)
    if m:
        mod.append(int(m.group(1), 16) & 0xFF)
        continue
    r = re.match(r"\s+\.\.\. x(\d+)", line)
    if r and mod:
        mod.extend([mod[-1]] * int(r.group(1)))
print("MCU sent %d bytes, module read %d" % (len(mcu), len(mod)))
n = min(len(mcu), len(mod))
for i in range(n):
    if mcu[i] != mod[i]:
        print("first mismatch at byte %d: mcu %02x module %02x" % (i, mcu[i], mod[i]))
        print(" mcu   ", " ".join("%02x" % b for b in mcu[max(0, i - 12) : i + 12]))
        print(" module", " ".join("%02x" % b for b in mod[max(0, i - 12) : i + 12]))
        break
else:
    print("identical over the common %d bytes" % n)
