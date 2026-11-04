#!/usr/bin/env python3
"""A B&W over-the-air .fw (A5, A7, Z2, Zeppelin Air with Lightning; docs/mcu/stock-firmware.md) cut into its parts, for
the emulator: BridgeCo's encryption wrapper (unencrypted) around a .bcd, then the MCU part.

    python3 -I tools/zeppelin/fw/bwfw.py FILE.fw [OUTDIR]

OUTDIR gets module.bcd, mcu.bin (the MCU part as it is) and, when the MCU part is an AVR32 .dmb (A7, ZD1L),
mcu_flash.bin: a 256 KB flash image for `zemu run OUTDIR/module.bcd --mcu OUTDIR/mcu_flash.bin` = B&W's ZD1 bootloader,
0x80004000-0x80007FFF from the ZD1's stock app (its jump to 0x80008000; the .dmb starts there) and the .dmb's app.
The bootloader is the ZD1's: the other models' are in no .fw. Emulation only, never flashed.
"""

import argparse
import os
import struct
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
from zeppelin import paths  # noqa: E402

FLASH = 0x40000
APP = 0x8000  # the .dmb's app goes to 0x80008000


def parse(d):
    """-> (wrapper words, the .bcd, the MCU part, the .dmb's name and app or None)."""
    w = struct.unpack_from("<8I", d)
    if w[0] != 2 or w[2] != 0:
        raise ValueError("not an unencrypted wrapper (version %d, encryption mode %d)" % (w[0], w[2]))
    bcd, mcu = d[32 : 32 + w[1]], d[32 + w[1] : 32 + w[1] + w[3]]
    if bcd[:4] != b"bCoD" or len(mcu) != w[3]:
        raise ValueError("no .bcd after the header, or the file is short")
    dmb = None
    if mcu[:4] == b"\x00\x01\x00\x80":
        n = struct.unpack_from(">I", mcu, 8)[0]
        name = mcu[24 : mcu.index(b"\0", 24)].decode()
        if 128 + n > len(mcu) or APP + n > FLASH:
            raise ValueError("the .dmb's app length 0x%x doesn't fit" % n)
        dmb = (name, mcu[128 : 128 + n])
    return w, bcd, mcu, dmb


def flash(app):
    bl, stock = open(paths.MCU_BOOTLOADER, "rb").read(), paths.stock_mcu_app()
    img = bl + stock[: APP - len(bl)] + app
    return img + b"\xff" * (FLASH - len(img))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("fw", metavar="FILE.fw")
    ap.add_argument("out", nargs="?", metavar="OUTDIR")
    a = ap.parse_args()
    w, bcd, mcu, dmb = parse(open(a.fw, "rb").read())
    hwid = struct.unpack_from("<I", bcd, 0x20)[0] & 0xFFFF
    print("module .bcd %d B (HWID 0x%X, built %s)" % (len(bcd), hwid, bcd[8:0x18].decode().strip()))
    print(
        "MCU part %d B: %s"
        % (len(mcu), "%s, app 0x%x B at 0x80008000" % (dmb[0], len(dmb[1])) if dmb else "not a .dmb")
    )
    print("wrapper words 4, 5: 0x%08x 0x%x (not checked)" % (w[4], w[5]))
    if a.out:
        os.makedirs(a.out, exist_ok=True)
        open(os.path.join(a.out, "module.bcd"), "wb").write(bcd)
        open(os.path.join(a.out, "mcu.bin"), "wb").write(mcu)
        if dmb:
            open(os.path.join(a.out, "mcu_flash.bin"), "wb").write(flash(dmb[1]))


if __name__ == "__main__":
    main()
