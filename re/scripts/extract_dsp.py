# Extract ADAU1445 SigmaStudio default download sequence from ZD1 v2.04.05 MCU image
# Source: FUN_80020980 (dsp_default_download). R6=0x0c54 (.data, initialized), R5=0x7b14 (.bss => zeros)
# usage: extract_dsp.py IMAGE OUTDIR  -> download_sequence.{bin,txt,json}
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from avr32img import Image  # noqa: E402

img = Image(sys.argv[1])
out = sys.argv[2]
os.makedirs(out, exist_ok=True)


R6 = 0xC54
seq = [
    ("R6", 0x13C, 0xE220, 2),
    ("R5", 0x13C, 0xE228, 2),
    ("R6", 0x140, 0x2000, 0x1A94),
    ("R6", 0x1BD4, 0xE000, 0x12),
    ("R6", 0x1BE8, 0xE040, 0x12),
    ("R5", 0x140, 0xE049, 2),
    ("R6", 0x1BFC, 0xE080, 0x10),
    ("R6", 0x1C0C, 0xE088, 0x10),
    ("R6", 0x1C1C, 0xE090, 0x18),
    ("R5", 0x144, 0xE0C0, 0x18),
    ("R6", 0x1C34, 0xE100, 4),
    ("R6", 0x1C38, 0xE140, 4),
    ("R5", 0x15C, 0xE180, 0x30),
    ("R5", 0x18C, 0xE1C0, 0x40),
    ("R6", 0x1C3C, 0xE200, 0x54),
    ("R6", 0x1C90, 0xE240, 0x1A),
    ("R6", 0x1CAC, 0xE280, 2),
    ("R6", 0x1CB0, 0x0000, 0x640),
    ("R6", 0x22F0, 0xE21F, 2),
    (None, 0, 0x6000, 0),
    ("R6", 0x22F4, 0xE228, 2),
    ("R6", 0x22F8, 0xE220, 2),
    ("R5", 0x1CC, 0xE221, 2),
    ("R6", 0x22FC, 0xE221, 2),
]
txt = []
allbin = bytearray()
meta = []
for i, (base, off, addr, n) in enumerate(seq):
    data = img.bytes(R6 + off, n) if base == "R6" else bytes(n)  # R5 arrays live in .bss -> all zero
    allbin += addr.to_bytes(2, "big") + n.to_bytes(2, "big") + data
    meta.append(
        dict(
            step=i,
            i2c_addr7=0x38,
            subaddr=hex(addr),
            length=n,
            src=(".data+" + hex(R6 + off)) if base == "R6" else (".bss(zero)" if base else "none"),
        )
    )
    txt.append(f"{i:02d} write subaddr=0x{addr:04x} len={n:5d} {data[:24].hex(' ')}{' ...' if n > 24 else ''}")
open(os.path.join(out, "download_sequence.txt"), "w").write("\n".join(txt) + "\n")
open(os.path.join(out, "download_sequence.bin"), "wb").write(allbin)
json.dump(meta, open(os.path.join(out, "download_sequence.json"), "w"), indent=1)
print("\n".join(txt))
