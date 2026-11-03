# usage: xref.py IMAGE STRING|0xADDR...  -> the 32-bit big-endian pointers in the image to a string or an address
import os
import struct
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from avr32img import FLASH as B  # noqa: E402

img = open(sys.argv[1], "rb").read()
for q in sys.argv[2:]:
    if q.startswith("0x"):
        targets = [int(q, 16)]
    else:
        o = img.find(q.encode())
        targets = [B + o] if o >= 0 else []
    for t in targets:
        p = struct.pack(">I", t)
        refs = []
        i = 0
        while True:
            i = img.find(p, i)
            if i < 0:
                break
            refs.append(hex(B + i))
            i += 1
        print(q, hex(t), refs)
