# usage: fnnames.py disasm.txt hex.bin [lo hi] -> per function: strings referenced via base+offset (assert names etc.)
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from avr32img import Image  # noqa: E402

dis, binf = sys.argv[1], sys.argv[2]
lo = int(sys.argv[3], 16) if len(sys.argv) > 3 else 0
hi = int(sys.argv[4], 16) if len(sys.argv) > 4 else 0xFFFFFFFF
img = Image(binf)
rd = img.word


def s(v):
    """The string at v (flash or .data), its first 60 characters."""
    t = img.cstr(v, most=1 << 16) if v is not None else None
    return t[:60].replace("\n", "\\n") if t else None


reg = {}
cur = None
out = {}
for line in open(dis):
    if line.startswith("; ---- "):
        cur = line.split()[-1]
        reg = {}
        continue
    m = re.match(r"^([0-9a-f]{8})\s+(.*)", line)
    if not m:
        continue
    a = int(m.group(1), 16)
    ins = m.group(2)
    if a < lo or a >= hi:
        continue
    x = re.match(r"LDDPC (\w+),0x([0-9a-f]+)", ins)
    if x:
        reg[x.group(1)] = rd(int(x.group(2), 16))
        continue
    x = re.match(r"LD.W (\w+),PC\[(-?0x[0-9a-f]+)\]", ins)
    if x:
        reg[x.group(1)] = rd((a + int(x.group(2), 16)) & ~3)
        continue
    x = re.match(r"SUB (\w+),(\w+),(-?0x[0-9a-f]+)$", ins)
    if x and reg.get(x.group(2)) is not None:
        v = (reg[x.group(2)] - int(x.group(3), 16)) & 0xFFFFFFFF
        t = s(v)
        if t:
            out.setdefault(cur, []).append(t)
        reg[x.group(1)] = v
        continue
    x = re.match(r"SUB (\w+),(-?0x[0-9a-f]+)$", ins)
    if x and reg.get(x.group(1)) is not None:
        v = (reg[x.group(1)] - int(x.group(2), 16)) & 0xFFFFFFFF
        t = s(v)
        if t:
            out.setdefault(cur, []).append(t)
        reg[x.group(1)] = v
for k, v in out.items():
    u = []
    for t in v:
        if t not in u:
            u.append(t)
    print(k, " | ".join(u)[:300])
