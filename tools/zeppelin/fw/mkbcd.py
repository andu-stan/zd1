#!/usr/bin/env python3
"""Build a BridgeCo DM870 .bcd image whose app partition runs a custom stage2 payload.

Usage:
  python3 -I mkbcd.py TEMPLATE.bcd PAYLOAD.bin OUT.bcd     # PAYLOAD linked/loaded at 0x60008000, entry = first byte
  python3 -I mkbcd.py --selftest TEMPLATE.bcd              # rebuild stock payload, must be byte-identical
  --misaligned: the payload padded to a length of 2 mod 4, which BL2 rejects (error 680): its failed-burn path

The stock image is used as a template: stage1 (SDRAM/clock/NAND preloader), factory CnE (section 1)
and resources (section 2) are copied unchanged; only stage2 is replaced and every checksum recomputed.
Format: docs/module/boot-chain.md (".bcd images", "Boot header"); tools/zeppelin/fw/bcd.py reads it.

WARNING: the output is flashable. Only test payloads via JTAG into SDRAM first, never erase NAND
block 0 (BL1) or the BL2 partition at 0xA0000, and keep a way to reach BL2 (serial menu "Mark Image
as invalid", factory-reset strap) before flashing anything.
"""

import argparse
import os
import struct
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
from zeppelin.fw.bcd import HDR_LEN, PART_HDR, STAGE2_OFF, crc32, fletcher_dm870, sections  # noqa: E402


def build(template, payload, pad=4):
    t = bytes(template)
    hdr = bytearray(t[: HDR_LEN + 8])  # header + 'DMP 3.x' tag
    secs = [[s["off"], s["nand"], s["size"], s["psize"], s["data"]] for s in sections(t)]

    # --- section 0: boot header + stock stage1 + new stage2 ---
    app_off, app_nand, _, app_psize, app = secs[0]
    assert app[:4] == b"\x55\x55\xaa\xaa", "template app has no boot header"
    payload += b"\0" * (-len(payload) % pad)  # BL2's web updater rejects a section whose length is not a
    max_len = app_psize - PART_HDR - STAGE2_OFF
    assert len(payload) <= max_len, "payload too big (%d > %d)" % (len(payload), max_len)
    sec0 = bytearray(app[:STAGE2_OFF]) + payload
    struct.pack_into("<I", sec0, 0x24, app_nand + PART_HDR + STAGE2_OFF)  # stage2 NAND address
    struct.pack_into("<I", sec0, 0x28, len(payload))  # stage2 length
    struct.pack_into("<I", sec0, 0x2C, fletcher_dm870(payload))  # stage2 checksum
    (s1_words,) = struct.unpack_from("<I", sec0, 0x10)
    # stage1 CRC covers the stage2 fields above, so it is computed last
    struct.pack_into("<I", sec0, 0x14, crc32(sec0[0x18 : 0x18 + s1_words * 8]))
    secs[0][4] = bytes(sec0)

    # --- lay out sections back to back, as in the stock file ---
    out = bytearray()
    pos = secs[0][0]
    body = bytearray()
    for i, (_off, nand, _size, psize, data) in enumerate(secs):
        struct.pack_into("<5I", hdr, 0x30 + 0x20 * i, pos, nand, len(data), crc32(data), psize)
        body += data
        pos += len(data)
    struct.pack_into("<I", hdr, 0x2C, 0)
    struct.pack_into("<I", hdr, 0x2C, crc32(hdr[:HDR_LEN]))
    out = hdr + body
    # stock file is 0xFF-padded to a 1 KiB boundary
    pad = (-len(out)) % 0x400
    out += b"\xff" * pad
    return bytes(out)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("template", metavar="TEMPLATE.bcd")
    ap.add_argument("payload", nargs="?", metavar="PAYLOAD.bin")
    ap.add_argument("out", nargs="?", metavar="OUT.bcd")
    ap.add_argument("--selftest", action="store_true", help="rebuild the template's own payload: byte-identical?")
    ap.add_argument("--misaligned", action="store_true", help="a section length BL2 rejects (2 mod 4)")
    a = ap.parse_args()
    t = open(a.template, "rb").read()
    if a.selftest:
        app = sections(t)[0]["data"]
        rebuilt = build(t, app[STAGE2_OFF:])
        ok = rebuilt == t
        print("stage2 checksum 0x%08x" % fletcher_dm870(app[STAGE2_OFF:]))
        print("round-trip byte-identical:", ok, "(%d vs %d bytes)" % (len(rebuilt), len(t)))
        return 0 if ok else 1
    if not a.out:
        ap.error("TEMPLATE.bcd PAYLOAD.bin OUT.bcd")
    p = open(a.payload, "rb").read()
    if a.misaligned:
        p += b"\0" * ((2 - len(p)) % 4)  # length = 2 mod 4
        out = build(t, p, pad=2)
    else:
        out = build(t, p)
    open(a.out, "wb").write(out)
    print("wrote %s (%d bytes, payload %d bytes)" % (a.out, len(out), len(p)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
