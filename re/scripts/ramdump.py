# usage: ramdump.py IMAGE ADDR [LEN]  -> hexdump of a flash address, or of an initialised-RAM one (< 0x3630, from the
# .data load image)
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from avr32img import Image  # noqa: E402

img = Image(sys.argv[1])
a = int(sys.argv[2], 16)
n = int(sys.argv[3], 0) if len(sys.argv) > 3 else 64
if img.offset(a) is None:
    sys.exit("0x%x is neither in the image nor in .data" % a)
b = img.bytes(a, n)
for i in range(0, len(b), 16):
    ch = b[i : i + 16]
    print(
        "%08x: %s  %s"
        % (a + i, " ".join("%02x" % c for c in ch), "".join(chr(c) if 32 <= c < 127 else "." for c in ch))
    )
