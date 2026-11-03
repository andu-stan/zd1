#!/usr/bin/env python3
"""The BridgeCo user-CnE NVRAM log (NAND 0x60000, two blocks) in 512-byte slots: u16le header (bit 2 active, bit 3
programmed, bits 4-15 the parameter id), the value (a NUL-terminated string), u32le crc32(header | active + value +
NUL). Values that aren't obviously harmless are redacted (the Wi-Fi passphrase is among them); --raw shows all
(keep that output local).

    python3 -I tools/zeppelin/fw/cneu_dump.py USER_CNE.bin [--raw]
"""

import argparse
import re
import struct
import zlib

SAFE = re.compile(r"^([0-9.:\-]*|wireless|wired|STOPPED|PLAYING|PAUSED|TRUE|FALSE|true|false|\d+\.\d+\.\d+)$")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("image", metavar="USER_CNE.bin")
    ap.add_argument("--raw", action="store_true", help="don't redact")
    a = ap.parse_args()
    d = open(a.image, "rb").read()
    for o in range(0, len(d), 0x200):
        r = d[o : o + 0x200]
        if r[:2] in (b"\xff\xff", b"\0\0"):
            continue
        try:
            end = r.index(b"\0", 2)
        except ValueError:
            continue
        h = struct.unpack_from("<H", r, 0)[0]
        crc = struct.unpack_from("<I", r, end + 1)[0]
        ok = zlib.crc32(struct.pack("<H", h | 4) + r[2 : end + 1]) == crc
        v = r[2:end].decode("latin1")
        shown = v if (a.raw or SAFE.match(v)) else f"<redacted len={len(v)}>"
        print(f"0x{o:05x} id=0x{h >> 4:03x} active={int(bool(h & 4))} crc_ok={int(ok)} {shown!r}")


if __name__ == "__main__":
    main()
