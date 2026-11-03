#!/usr/bin/env python3
"""Decode a bit-banged 2-wire bus from a zd1 emulator trace (PIN lines).
SCL/SDA default PB13/PB14. Line level: HIGH or input (released, pulled up) = 1, LOW = 0.
Prints START/STOP-framed byte sequences with ack bits, plus the guest PC of each frame's first edge."""

import re
import sys

path = sys.argv[1]
scl_n = sys.argv[2] if len(sys.argv) > 2 else "PB13"
sda_n = sys.argv[3] if len(sys.argv) > 3 else "PB14"
lvl = {"HIGH": 1, "input": 1, "periph": 1, "LOW": 0}
scl, sda = 1, 1
frames, cur, bits, pc0 = [], None, [], None
for line in open(path):
    m = re.match(r"\s+PIN (\w+) (\w+) -> (\w+)\s+\(pc=([0-9a-f]+)\)", line)
    if not m or m.group(1) not in (scl_n, sda_n):
        continue
    name, new, pc = m.group(1), lvl[m.group(3)], m.group(4)
    if name == sda_n:
        if scl == 1 and sda == 1 and new == 0:  # START (or repeated start)
            if cur is not None and bits:
                cur.append(("partial", bits))
            cur = []
            bits = []
            pc0 = pc
            frames.append((pc0, cur))
        elif scl == 1 and sda == 0 and new == 1 and cur is not None:  # STOP
            if bits:
                cur.append(("partial", bits))
            cur.append(("STOP",))
            cur = None
            bits = []
        sda = new
    else:
        if scl == 0 and new == 1:  # rising edge: sample
            if cur is None:
                cur = []
                frames.append((pc, cur))
                cur.append(("noSTART",))
            bits.append(sda)
            if len(bits) == 9:
                byte = int("".join(map(str, bits[:8])), 2)
                cur.append(("byte", byte, "ACK" if bits[8] == 0 else "NAK"))
                bits = []
        scl = new
for pc, fr in frames:
    out = []
    for it in fr:
        if it[0] == "byte":
            out.append("%02X%s" % (it[1], "" if it[2] == "ACK" else "~"))
        elif it[0] == "partial":
            out.append("[%d bits:%s]" % (len(it[1]), "".join(map(str, it[1]))))
        else:
            out.append(it[0])
    first = next((it[1] for it in fr if it[0] == "byte"), None)
    addr = "  addr7=0x%02X %s" % (first >> 1, "R" if first & 1 else "W") if first is not None else ""
    print("pc=%s  S %s%s" % (pc, " ".join(out), addr))
print("%d frames (~ = NAK; emulator has no device so reads/acks float high)" % len(frames))
