# usage: dis.py IMAGE disasm.txt START END [START END...]  -> the disasm lines in [START,END) with the PC-literal
# loads and MCALL targets resolved (address, and the string there if any)
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from avr32img import Image  # noqa: E402

img = Image(sys.argv[1])
ranges = [(int(sys.argv[i], 16), int(sys.argv[i + 1], 16)) for i in range(3, len(sys.argv) - 1, 2)]


def desc(v):
    if v is None:
        return "?"
    s = img.cstr(v)
    return ("RAM_%04x" % v if 8 <= v < 0x10000 else "%08x" % v) + (' "%s"' % s.replace("\n", "\\n") if s else "")


for line in open(sys.argv[2]):
    m = re.match(r"^([0-9a-f]{8})\s", line)
    if not m:
        continue
    a = int(m.group(1), 16)
    if not any(lo <= a < hi for lo, hi in ranges):
        continue
    line = line.rstrip()
    x = re.search(r"MCALL PC\[0x([0-9a-f]+)\]", line)
    if x:
        line += "   ; -> " + desc(img.word(int(x.group(1), 16)))
    x = re.search(r"LDDPC \w+,0x([0-9a-f]+)", line)
    if x:
        line += "   ; = " + desc(img.word(int(x.group(1), 16)))
    x = re.search(r"LD\.W \w+,PC\[(-?0x[0-9a-f]+)\]", line)
    if x:
        line += "   ; = " + desc(img.word((a + int(x.group(1), 16)) & ~3))
    print(line)
