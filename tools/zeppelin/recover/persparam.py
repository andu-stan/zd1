#!/usr/bin/env python3
"""DM870 PersParam store (NAND 0x20000, one 128 KB block): parse, dump, build.

Format (docs/module/boot-chain.md, "PersParam"): 0x10-byte block header 80808080 00020000 00020000 00000000, the name "PersParam\\0\\0\\0",
then 4-byte aligned records {mark, rec_len, data_len, 0, key_len=4, key (stored byte-reversed), data, pad 0xFF}.
mark 08080808 = live; 00000000 = superseded (cleared in place when a newer record for the key is appended).

Per-unit values (MAC0/MAC1/GUID) are masked in dumps unless --raw (keep raw output out of git).
"""

import argparse
import struct
import sys

HDR = bytes.fromhex("80808080000002000000020000000000") + b"PersParam\0\0\0"
LIVE = 0x08080808
SECRET = {"MAC0", "MAC1", "GUID"}


def parse(blob):
    if blob[: len(HDR)] != HDR:
        raise ValueError("no PersParam header")
    off, recs = len(HDR), []
    while off + 20 <= len(blob):
        mark, rlen, dlen, z, klen = struct.unpack_from("<5I", blob, off)
        if mark == 0xFFFFFFFF:
            break
        if klen != 4 or rlen < 20 + klen + dlen or rlen % 4:
            raise ValueError(f"bad record at +{off:#x}")
        key = blob[off + 20 : off + 24][::-1].decode("latin-1")
        data = blob[off + 24 : off + 24 + dlen]
        recs.append(dict(off=off, live=mark == LIVE, mark=mark, key=key, data=data, rlen=rlen, z=z))
        off += rlen
    return recs, off


def live_values(recs):
    """Last live record per key, in first-appearance order of the key."""
    vals = {}
    for r in recs:
        if r["live"]:
            vals[r["key"]] = r["data"]
    return vals


def record(key, data):
    rlen = (24 + len(data) + 3) & ~3
    r = struct.pack("<5I", LIVE, rlen, len(data), 0, 4) + key.encode()[::-1] + data
    return r + b"\xff" * (rlen - len(r))


def build(items, size=0x20000):
    """items: list of (key, bytes[, live]) written as one record each (live unless live=False: superseded, mark 0)."""
    out = HDR
    for it in items:
        r = record(it[0], it[1])
        out += r if len(it) < 3 or it[2] else b"\0" * 4 + r[4:]
    return out + b"\xff" * (size - len(out))


def u32(v):
    return struct.pack("<I", v)


def parse_mac(s):
    b = bytes(int(x, 16) for x in s.replace("-", ":").split(":"))
    if len(b) != 6:
        raise ValueError("MAC must be 6 bytes, aa:bb:cc:dd:ee:ff")
    return b


def factory(mac):
    """The block as the factory leaves it: byte-identical on this unit and the archive.org unit up to the end of the
    DTCV record, except for the MAC. Order = BL2/image-writer defaults, then the factory's MAC0 programming (the
    8-byte zero MAC0 superseded by a 6-byte one, like BL2 menu 8 / `persparam wrb MAC0`), then DTCV = 1.
    The app appends SAFE on every boot and its `persparam check` defaults (POWB ... SRNO) later."""
    z8 = bytes(8)
    return build(
        [
            ("CnEU", u32(0x60000)),
            ("CnES", u32(0x20000)),
            ("CnEA", u32(0x40080)),
            ("BLDb", u32(0)),
            ("HWID", u32(0xBD)),
            ("HWVS", u32(0)),
            ("GUID", z8),
            ("MAC0", z8, False),
            ("MAC1", z8),
            ("BLDF", b"081008"),
            ("MAC0", mac),
            ("DTCV", u32(1)),
        ]
    )


def fmt(key, data, raw):
    if key in SECRET and not raw and any(data):
        return f"<{len(data)} bytes, masked>"
    if len(data) == 4:
        return f"{struct.unpack('<I', data)[0]:#x}"
    if all(32 <= b < 127 for b in data):
        return repr(data.decode())
    return data.hex()


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("image", help="PersParam block, or a NAND image/slice with --offset")
    ap.add_argument("--factory", metavar="MAC", help="write a factory-fresh block for MAC to IMAGE (128 KB) instead")
    ap.add_argument("--check", metavar="MAC", help="compare IMAGE's factory part with the block built for MAC")
    ap.add_argument("--offset", type=lambda s: int(s, 0), default=0)
    ap.add_argument("--raw", action="store_true", help="don't mask MAC/GUID")
    ap.add_argument("--all", action="store_true", help="list superseded records too")
    a = ap.parse_args()
    if a.factory:
        open(a.image, "wb").write(factory(parse_mac(a.factory)))
        print(f"wrote factory PersParam ({a.image}, 0x20000 B)")
        return
    if a.check:
        blob = open(a.image, "rb").read()[a.offset : a.offset + 0x20000]
        ref = factory(parse_mac(a.check))
        n = ref.index(b"\xff" * 64)
        same = blob[:n] == ref[:n]
        print(f"factory part (0..{n:#x}) {'IDENTICAL' if same else 'DIFFERENT'} to the block generated for that MAC")
        sys.exit(0 if same else 1)
    blob = open(a.image, "rb").read()[a.offset : a.offset + 0x20000]
    recs, end = parse(blob)
    for r in recs:
        if a.all or r["live"]:
            print(f"+{r['off']:#06x} {'live' if r['live'] else 'old '} {r['key']} {fmt(r['key'], r['data'], a.raw)}")
    print(
        f"{len(recs)} records, {sum(r['live'] for r in recs)} live, end +{end:#x}; "
        f"rest erased: {all(b == 0xFF for b in blob[end:])}"
    )


if __name__ == "__main__":
    main()
