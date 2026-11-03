#!/usr/bin/env python3
"""Patch the stock Zeppelin Air MCU firmware (ZD1 2.04.05) with the telnet hook (README.md): once the module is
ready, the MCU writes reg 0x75 = "23" and reg 0x76 = 5, the module's KnOS shell on telnet port 23, re-asserted every
~30 s. Python only: the hook blob is prebuilt (telnet_hook.bin; build.sh rebuilds it from telnet_hook.S).

  usage: python3 -I stock-patch/patch.py OUT.hex [--stock STOCK.hex]

STOCK.hex defaults to blobs/mcu/zd1-2.04.05.hex. Every patched word is checked against the stock value first.
Flash with: zd1 bootloader, then zd1 dfu write OUT.hex --backup BACKUP.tar --yes --start (a `zd1 dfu backup` first).
Back to stock: the same with blobs/mcu/zd1-2.04.05.hex. Our own firmware (mcu/) puts the shell on telnet with `zd1 reg write` (docs/module/knos.md).
"""

import argparse
import os
import struct
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "tools"))
from zeppelin.fw import ihex  # noqa: E402

BASE = 0x80000000
CAVE = 0x80038100  # free flash the hook goes to (telnet_hook.S is linked there, hook.ld)
VOTASK_PTR = 0x8001E870  # the literal the BcoWrapper tick calls voTask through
BSS_END_LIT = 0x80008088  # crt0's bss-clear end
HEAP_BASE_LIT = 0x8002AE28  # sbrk's heap base
OLD_VOTASK = 0x8000E19C
OLD_MEMEND = 0x8250
NEW_MEMEND = 0x8258  # 0x8250..0x8257 become the hook's state (crt0 zeroes it)


def patch(stock, blob):
    """The stock image with the hook at CAVE, voTask's call pointer on it and 8 bytes of RAM carved for its state."""
    rd = lambda a: struct.unpack_from(">I", stock, a - BASE)[0]  # noqa: E731
    assert rd(VOTASK_PTR) == OLD_VOTASK, "voTask pointer is not stock"
    assert rd(BSS_END_LIT) == OLD_MEMEND, "bss-end literal is not stock"
    assert rd(HEAP_BASE_LIT) == OLD_MEMEND, "heap-base literal is not stock"
    assert CAVE - BASE >= len(stock), "the cave overlaps the image"
    assert CAVE - BASE + len(blob) <= 0x40000, "the hook runs past the app region"
    img = bytearray(stock) + b"\xff" * (CAVE - BASE - len(stock)) + blob
    for addr, val in ((VOTASK_PTR, CAVE), (BSS_END_LIT, NEW_MEMEND), (HEAP_BASE_LIT, NEW_MEMEND)):
        struct.pack_into(">I", img, addr - BASE, val)
    return img


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("out", metavar="OUT.hex")
    ap.add_argument("--stock", default=os.path.join(HERE, "..", "blobs", "mcu", "zd1-2.04.05.hex"))
    a = ap.parse_args()
    blob = open(os.path.join(HERE, "telnet_hook.bin"), "rb").read()
    img = patch(ihex.image(open(a.stock).read(), BASE)[1], blob)
    open(a.out, "w").write(ihex.dump(img, BASE))
    print(
        "hook %d bytes at 0x%08x; voTask pointer 0x%08x -> 0x%08x; bss-end/heap 0x%x -> 0x%x; image end 0x%08x"
        % (len(blob), CAVE, OLD_VOTASK, CAVE, OLD_MEMEND, NEW_MEMEND, BASE + len(img))
    )
    print("wrote", a.out)


if __name__ == "__main__":
    main()
