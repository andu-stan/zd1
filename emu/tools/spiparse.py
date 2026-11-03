#!/usr/bin/env python3
"""Parse the MCU->module SPI byte stream (zd1 trace 'SPI0 xx -> yy') as the stock register protocol and report
malformed messages. Requests: 00 reg b0 b1 | 01 reg 00 00 | 03 reg 00 00 | 02 reg nlo nhi data*n; data/lengths
0x27-stuffed (27 01 = ff, 27 27 = 27), headers raw, raw 0xff = idle. usage: spiparse.py zd1.txt [max_report]"""

import re
import sys

b = [
    int(m.group(1), 16)
    for m in re.finditer(r"SPI0 ([0-9a-f]{2}) -> [0-9a-f]{2}", open(sys.argv[1], errors="replace").read())
]
i, msgs, bad = 0, 0, 0


def stuffed():
    global i
    v = b[i]
    i += 1
    if v == 0x27:
        n = b[i]
        i += 1
        return 0xFF if n == 0x01 else n
    return v


while i < len(b) - 4:
    if b[i] == 0xFF:
        i += 1
        continue
    t, reg = b[i], b[i + 1]
    start = i
    i += 2
    if t in (0, 1, 3):
        d = [stuffed(), stuffed()]
    elif t == 2:
        n = stuffed() | stuffed() << 8
        if n > 4096:
            print("V write reg %02x with length %d at byte %d: malformed" % (reg, n, start))
            bad += 1
            if bad >= int(sys.argv[2] if len(sys.argv) > 2 else 3):
                break
            continue
        for _ in range(n):
            stuffed()
        if msgs < 3 or reg != 0x2D:
            pass
    else:
        print(
            "unknown type %02x at byte %d, context %s"
            % (t, start, " ".join("%02x" % x for x in b[start - 8 : start + 8]))
        )
        bad += 1
        if bad >= int(sys.argv[2] if len(sys.argv) > 2 else 3):
            break
        i = start + 1
        continue
    msgs += 1
print("%d messages, %d bytes, %d anomalies" % (msgs, len(b), bad))
