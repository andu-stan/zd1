# usage: callsites.py disasm.txt hex.bin target1,target2,... [nprev]  -> each call site with the preceding R12/R11/R10 immediates
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from avr32img import Image  # noqa: E402

dis, binf = sys.argv[1], sys.argv[2]
tg = set(int(x, 16) for x in sys.argv[3].split(","))
npv = int(sys.argv[4]) if len(sys.argv) > 4 else 12
img = Image(binf)


cur = None
hist = []
for line in open(dis):
    if line.startswith("; ---- "):
        cur = line.split()[-1]
        hist = []
        continue
    m = re.match(r"^([0-9a-f]{8})\s+(.*)", line)
    if not m:
        continue
    a = int(m.group(1), 16)
    ins = m.group(2).strip()
    t = None
    x = re.match(r"MCALL PC\[0x([0-9a-f]+)\]", ins)
    if x:
        t = img.word(int(x.group(1), 16))
    x = re.match(r"(R?CALL) 0x([0-9a-f]+)", ins)
    if x:
        t = int(x.group(2), 16)
    if t in tg:
        regs = {}
        for _ha, hi in hist[-npv:]:
            y = re.match(r"MOV (R1[012]|R9),(-?0x[0-9a-f]+|\d+)$", hi)
            if y:
                regs[y.group(1)] = y.group(2)
            y = re.match(r"MOV (R1[012]|R9),(R\d+|SP)$", hi)
            if y:
                regs[y.group(1)] = y.group(2)
            y = re.match(r"(?:SUB|ADD) (R1[012]|R9),.*", hi)
            if y:
                regs[y.group(1)] = hi
            y = re.match(r"LD\.\w+ (R1[012]|R9),(.*)", hi)
            if y:
                regs[y.group(1)] = hi
        print(
            "%s %08x -> %08x  %s"
            % (cur, a, t, "  ".join("%s=%s" % (k, regs[k]) for k in ("R12", "R11", "R10", "R9") if k in regs))
        )
    hist.append((a, ins))
