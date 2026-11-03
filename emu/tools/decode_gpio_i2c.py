#!/usr/bin/env python3
"""Decode bit-banged I2C from a dm870 trace's "GPIO 13=x 14=y" lines. Tries both pin assignments and keeps the
one that produces START conditions. usage: decode_gpio_i2c.py <trace>"""

import re
import sys

ev = [(int(a), int(b)) for a, b in re.findall(r"^GPIO 13=(\d) 14=(\d)", open(sys.argv[1]).read(), re.M)]


def decode(scl_i):
    out, bits, cur, prev = [], [], None, (1, 1)
    for e in ev:
        scl, sda = e[scl_i], e[1 - scl_i]
        pscl, psda = prev[scl_i], prev[1 - scl_i]
        if scl and pscl and psda and not sda:
            cur = []
            out.append(cur)
            bits = []  # START
        elif scl and pscl and not psda and sda:
            cur = None  # STOP
        elif scl and not pscl and cur is not None:  # rising SCL: sample
            bits.append(sda)
            if len(bits) == 9:
                b = int("".join(map(str, bits[:8])), 2)
                cur.append((b, "A" if bits[8] == 0 else "N"))
                bits = []
        prev = e
    return out


for scl_i, name in ((0, "SCL=13 SDA=14"), (1, "SCL=14 SDA=13")):
    tr = decode(scl_i)
    if tr and any(t for t in tr):
        print(name, f"{len(tr)} transactions")
        for t in tr[:40]:
            print(
                "  ",
                " ".join(f"{b:02x}{a}" for b, a in t),
                f"(addr 0x{t[0][0] >> 1:02x} {'R' if t[0][0] & 1 else 'W'})" if t else "",
            )
