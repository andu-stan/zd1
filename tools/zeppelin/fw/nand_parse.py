#!/usr/bin/env python3
"""Parse a raw (no-OOB) DM870 NAND dump: the MetaInfo partition headers with their CRCs and boot headers, and the
PersParam block (MAC and GUID masked unless --raw: keep raw output out of git).

    python3 -I tools/zeppelin/fw/nand_parse.py DUMP.bin [--raw]
"""

import argparse
import os
import struct
import sys
import zlib

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
from zeppelin.fw import bcd  # noqa: E402
from zeppelin.recover import persparam  # noqa: E402

BLK = 0x20000
PTYPE = {1: "BL2 (Bootloader Second Level)", 2: "Application", 4: "CnE Factory (attr)", 7: "Resources (FFS)"}


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("dump")
    ap.add_argument("--raw", action="store_true", help="don't mask MAC/GUID")
    a = ap.parse_args()
    n = open(a.dump, "rb").read()
    print("== MetaInfo partitions")
    for off in range(0, len(n), BLK):
        if n[off + 0x30 : off + 0x38] != b"MetaInfo":
            continue
        date = n[off : off + 16].decode("latin1").strip()
        model, swver, pid, plen, base, ilen, crc, flag = struct.unpack_from("<8I", n, off + 0x10)
        ok = bcd.crc32(n[base : base + ilen]) == crc
        print(
            f"0x{off:07x} type={pid} {PTYPE.get(pid, '?'):30s} date={date} model=0x{model:x} swver={swver} "
            f"part_len=0x{plen:x} data@0x{base:x} len=0x{ilen:x} crc32=0x{crc:08x} ok={ok} flag=0x{flag:x}"
        )
        h = bcd.boothdr(n[base : base + ilen])
        if h:
            img = n[base : base + ilen]
            s1ok = bcd.crc32(img[0x18 : 0x18 + h["stage1_words"] * 8]) == h["stage1_crc"]
            s2 = n[h["stage2_nand"] : h["stage2_nand"] + h["stage2_size"]]
            s2ok = h["stage2_crc"] in (bcd.fletcher_dm870(s2), zlib.adler32(s2))  # the app's / BL2's
            print(
                f"     boot hdr: stage1 -> SRAM 0x{h['stage1_sp']:x} len={h['stage1_words']}*8 crc32 ok={s1ok}; "
                f"stage2 -> 0x{0x60000000 + h['stage2_load']:08x} from NAND 0x{h['stage2_nand']:x} "
                f"len 0x{h['stage2_size']:x} chk 0x{h['stage2_crc']:08x} ok={s2ok}"
            )
    print("== PersParam (block 1)")
    recs, end = persparam.parse(n[BLK : 2 * BLK])
    for r in recs:
        print(
            f"  0x{BLK + r['off']:06x} {'live' if r['live'] else 'old '} {r['key']} = {persparam.fmt(r['key'], r['data'], a.raw)}"
        )


if __name__ == "__main__":
    main()
