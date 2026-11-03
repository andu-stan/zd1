#!/usr/bin/env python3
"""Build the factory-fresh images of one Zeppelin Air from its MAC: everything a recovery writes back, both boards.

  usage: python3 -I tools/zeppelin/recover/mkunit.py --mac EC:66:D1:xx:xx:xx --out DIR [--bcd app_DMP.bcd] [--check REF_NAND]

Outputs (DIR is per-unit data: keep it out of git, the MAC is in it):
  nand.bin        128 MB logical NAND image (no spare/ECC, bad blocks not skipped: a programmer image needs the
                  per-chip layout step on top). BL1 @0, PersParam @0x20000 (persparam.factory), factory CnE @0x40000,
                  user settings 0x60000-0xA0000 erased, BL2 @0xA0000, app @0x240000, resources @0xC40000.
  mcu_flash.bin   256 KB: DFU bootloader (0x0-0x3FFF) + stock app (0x4000-).
  user_page.bin   512 B: erased + the ISP config word at 0x1FC (the stock app fills the rest from the module).
  eeprom.bin      256 B blank 24C02: the stock app writes its default record on first boot.
  manifest.json   sha256 of every file, sources, GP fuse word, what each region is.

Sources (generic, not per unit): blobs/mcu/{bootloader.bin,zd1-2.04.05.hex}, blobs/module/{bl1.enc,bl2.bin}
and the vendor app_DMP.bcd (untrusted data: parsed, never executed). Partition headers (MetaInfo) are generated from
the .bcd the way BL2 writes them; --check compares the generated NAND region by region with a reference image.
"""

import argparse
import hashlib
import json
import os
import struct
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
from zeppelin import paths  # noqa: E402
from zeppelin.fw import bcd as bcdfmt  # noqa: E402
from zeppelin.recover import persparam  # noqa: E402

NAND_SIZE = 128 << 20
ISP_WORD = 0x929E0870  # user page 0x1FC on this unit; the bootloader checks its CRC-8 + signature 0x494F
FGPFR = 0x7C04FFFF  # GP fuses at backup time: LOCK 0xFFFF, BOOTPROT 2, ISP_IO_COND_EN 1, ISP_FORCE 0
REGIONS = [  # (name, start, end) for --check and the manifest
    ("BL1", 0x0, 0x20000),
    ("PersParam", 0x20000, 0x40000),
    ("factory CnE", 0x40000, 0x60000),
    ("user settings", 0x60000, 0xA0000),
    ("BL2", 0xA0000, 0x200000),
    ("(gap)", 0x200000, 0x240000),
    ("app", 0x240000, 0xC40000),
    ("resources", 0xC40000, 0x1040000),
    ("(rest)", 0x1040000, NAND_SIZE),
]


def rd(path):
    return open(path, "rb").read()


def build_nand(mac, bcd):
    nand = bytearray(b"\xff" * NAND_SIZE)
    put = lambda off, d: nand.__setitem__(slice(off, off + len(d)), d)  # noqa: E731
    put(0x0, rd(paths.MODULE_BL1))
    put(0x20000, persparam.factory(mac))
    app, cne, res = bcdfmt.sections(bcd)
    put(0x40000, bcdfmt.partition(bcd, cne, 0x40000) + cne["data"])
    bl2 = rd(paths.MODULE_BL2)
    plen, base, ilen, crc = struct.unpack_from("<4I", bl2, 0x1C)
    assert bcdfmt.crc32(bl2[0x80 : 0x80 + ilen]) == crc, "BL2 backup partition CRC"
    put(0xA0000, bl2)
    for s in (app, res):
        put(s["nand"], bcdfmt.partition(bcd, s) + s["data"])
    return bytes(nand)


def build_mcu():
    bl, app = rd(paths.MCU_BOOTLOADER), paths.stock_mcu_app()
    assert len(bl) == 0x4000 and len(app) == 0x3C000
    page = bytearray(b"\xff" * 512)
    struct.pack_into(">I", page, 0x1FC, ISP_WORD)
    return bl + app, bytes(page)


def check(nand, ref):
    print("region check against %s:" % ref)
    r = rd(ref)
    for name, a, b in REGIONS:
        diff = sum(1 for i in range(a, b, 4096) if nand[i : i + 4096] != r[i : i + 4096])
        first = next((i for i in range(a, b) if nand[i] != r[i]), None) if diff else None
        print(
            "  %-14s 0x%07x-0x%07x %s"
            % (name, a, b, "identical" if not diff else "%d x 4 KB differ, first at 0x%x" % (diff, first))
        )


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--mac", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--bcd", default=paths.MODULE_STOCK_BCD)
    ap.add_argument("--check", metavar="REF_NAND")
    a = ap.parse_args()
    mac = persparam.parse_mac(a.mac)
    os.makedirs(a.out, exist_ok=True)
    nand = build_nand(mac, rd(a.bcd))
    flash, page = build_mcu()
    files = {"nand.bin": nand, "mcu_flash.bin": flash, "user_page.bin": page, "eeprom.bin": b"\xff" * 256}
    for n, d in files.items():
        open(os.path.join(a.out, n), "wb").write(d)
    man = {
        "files": {n: {"bytes": len(d), "sha256": hashlib.sha256(d).hexdigest()} for n, d in files.items()},
        "bcd": {"path": os.path.relpath(a.bcd, paths.REPO), "sha256": hashlib.sha256(rd(a.bcd)).hexdigest()},
        "fgpfr": "0x%08X" % FGPFR,
        "isp_word": "0x%08X" % ISP_WORD,
        "nand_regions": {n: "0x%07x-0x%07x" % (s, e) for n, s, e in REGIONS},
    }
    open(os.path.join(a.out, "manifest.json"), "w").write(json.dumps(man, indent=2) + "\n")
    print("wrote %s: %s" % (a.out, ", ".join(files)))
    if a.check:
        check(nand, a.check)


if __name__ == "__main__":
    main()
