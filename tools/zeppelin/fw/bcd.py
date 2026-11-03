#!/usr/bin/env python3
"""BridgeCo DM870 images: the .bcd container, the NAND partition header (MetaInfo) and the app's boot header
(docs/module/boot-chain.md, ".bcd images", "Partition header", "Boot header").

    python3 -I tools/zeppelin/fw/bcd.py IMAGE.bcd [OUTDIR]     the header and sections, checked; OUTDIR: extracted

As a library: parse() a .bcd, sections() its checked sections, metainfo() / partition() the header BL2 writes before a
section in NAND, install() a .bcd into a NAND image as BL2 does, fletcher_dm870() the stage-2 checksum.
"""

import argparse
import os
import struct
import zlib

MAGIC = b"bCoD"
HDR_LEN = 0xB0  # the header the CRC at 0x2C covers; "DMP 3.x" follows
PART_HDR = 0x80  # the MetaInfo header before a section's data in NAND
STAGE2_OFF = 0x2000  # stage 2 inside the app section (stage 1, the preloader, before it)
BOOT_MAGIC = b"\x55\x55\xaa\xaa"
NAMES = {0: "app", 1: "attr", 2: "ffs_resources"}
PID = {0: 2, 1: 4, 2: 7}  # section -> partition type in its MetaInfo: application, factory CnE, resources
USER_CNE = (0x60000, 0xA0000)  # the user settings, which BL2 erases with every update ("Update Image & CnE")


def crc32(b):
    return zlib.crc32(b) & 0xFFFFFFFF


def fletcher_dm870(data):
    """The app's stage-2 checksum: 16-bit LE words, a = b = 0xFFFF, folded every 360 words, a | b << 16."""
    if len(data) & 1:
        data = data + b"\0"
    a = b = 0xFFFF
    words = struct.unpack_from("<%dH" % (len(data) // 2), data)
    for i in range(0, len(words), 360):
        for w in words[i : i + 360]:
            a += w
            b += a
        a = (a & 0xFFFF) + (a >> 16)
        b = (b & 0xFFFF) + (b >> 16)
    a = (a & 0xFFFF) + (a >> 16)
    b = (b & 0xFFFF) + (b >> 16)
    return (a | (b << 16)) & 0xFFFFFFFF


def header_crc(f):
    hb = bytearray(f[:HDR_LEN])
    hb[0x2C:0x30] = bytes(4)
    return crc32(hb)


def parse(f):
    """(header, sections): every field, the CRCs checked but not enforced; sections with size 0 are left out."""
    if f[:4] != MAGIC:
        raise ValueError("not a bCoD container")
    h = {}
    (h["version"],) = struct.unpack_from("<I", f, 4)
    h["build"] = f[8:0x18].decode().strip()
    h["verA"], h["verB"], h["hwid"], h["u24"], h["u28"], h["hcrc"] = struct.unpack_from("<6I", f, 0x18)
    h["hcrc_ok"] = header_crc(f) == h["hcrc"]
    h["tag"] = f[HDR_LEN : HDR_LEN + 8].split(b"\0")[0].decode()
    secs = []
    for i in range(3):
        off, nand, size, crc, psize = struct.unpack_from("<5I", f, 0x30 + 0x20 * i)
        if size == 0:
            continue
        d = f[off : off + size]
        secs.append(dict(idx=i, off=off, nand=nand, size=size, crc=crc, psize=psize, crc_ok=crc32(d) == crc, data=d))
    return h, secs


def sections(f):
    """The three sections of a .bcd whose CRCs all match (else ValueError), as parse() gives them."""
    h, secs = parse(f)
    if not h["hcrc_ok"]:
        raise ValueError("bcd header CRC")
    for s in secs:
        if not s["crc_ok"]:
            raise ValueError("bcd section %d CRC" % s["idx"])
    if [s["idx"] for s in secs] != [0, 1, 2]:
        raise ValueError("bcd: not three sections")
    return secs


def boothdr(d):
    """The boot header at the start of an app section (the NAND bootloaders read it), or None."""
    if d[:4] != BOOT_MAGIC:
        return None
    v = struct.unpack_from("<12I", d, 0)
    return dict(
        ver=v[1],
        stage1_sp=v[3],
        stage1_words=v[4],
        stage1_crc=v[5],
        branch=v[6],
        stage2_load=v[8],
        stage2_nand=v[9],
        stage2_size=v[10],
        stage2_crc=v[11],
    )


def metainfo(date, model, swver, pid, part, plen, data, extra=b""):
    """A partition header: date[16], <8I> model swver pid plen base ilen crc eFlag, "MetaInfo", then (resources only)
    the entry table; 0xFF up to 0x80. The data follows at part + 0x80."""
    h = (
        date
        + struct.pack("<8I", model, swver, pid, plen, part + PART_HDR, len(data), crc32(data), 0x7F)
        + b"MetaInfo"
        + extra
    )
    return h + b"\xff" * (PART_HDR - len(h))


def partition(f, sec, part=None):
    """The partition header BL2 writes before section `sec` of .bcd `f` (at `part`: default the section's NAND
    address; the factory CnE's is 0 in the container). The resources' entry table holds 8 start addresses (unused =
    the end) and the 8 sizes from the .bcd's words at 0x90."""
    part = sec["nand"] if part is None else part
    date, (model, swver) = f[8:0x18], struct.unpack_from("<2I", f, 0x18)
    extra = b""
    if sec["idx"] == 2:
        sizes, ptrs, a = struct.unpack_from("<8I", f, 0x90), [], part + PART_HDR
        for s in sizes:
            ptrs.append(a)
            a += s
        extra = struct.pack("<18I", 0x48, 0, *ptrs, *sizes)
    return metainfo(date, model, swver, PID[sec["idx"]], part, sec["psize"], sec["data"], extra)


def install(nand, f, log=lambda s: None):
    """.bcd `f` into the NAND image `nand` (a bytearray, changed in place) the way BL2's flasher leaves it: per
    section the partition's MetaInfo gets the new length and CRC (a blank partition the header BL2 writes), the data
    goes at partition + 0x80 and the rest of the partition is erased; the factory CnE section (no NAND address in the
    container) erases the user settings instead, like BL2."""
    for s in sections(f):
        part, data = s["nand"], s["data"]
        if part == 0:
            nand[USER_CNE[0] : USER_CNE[1]] = b"\xff" * (USER_CNE[1] - USER_CNE[0])
            log("section %d (CnE): user settings 0x%x..0x%x erased" % ((s["idx"],) + USER_CNE))
            continue
        if nand[part + 0x30 : part + 0x38] != b"MetaInfo":
            nand[part : part + PART_HDR] = partition(f, s)
            log("section %d: no MetaInfo at 0x%x, header generated" % (s["idx"], part))
        plen, base = struct.unpack_from("<2I", nand, part + 0x1C)
        if base != part + PART_HDR:
            raise ValueError("section %d: the MetaInfo at 0x%x has base 0x%x" % (s["idx"], part, base))
        if len(data) > plen - PART_HDR:
            raise ValueError("section %d: %d B > partition %d B" % (s["idx"], len(data), plen - PART_HDR))
        nand[base : part + plen] = b"\xff" * (part + plen - base)
        nand[base : base + len(data)] = data
        struct.pack_into("<2I", nand, part + 0x24, len(data), crc32(data))
        log("section %d -> partition 0x%x: %d B" % (s["idx"], part, len(data)))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("image", metavar="IMAGE.bcd")
    ap.add_argument("out", nargs="?", metavar="OUTDIR", help="extract the sections here")
    a = ap.parse_args()
    f = open(a.image, "rb").read()
    h, secs = parse(f)
    print({k: (hex(v) if isinstance(v, int) and not isinstance(v, bool) else v) for k, v in h.items()})
    out = a.out
    for s in secs:
        name = "sec%d_%s" % (s["idx"], NAMES[s["idx"]])
        print(
            "sec%d %-14s file_off=0x%x nand_off=0x%x size=0x%x part=0x%x crc=0x%08x ok=%s"
            % (s["idx"], NAMES[s["idx"]], s["off"], s["nand"], s["size"], s["psize"], s["crc"], s["crc_ok"])
        )
        bh = boothdr(s["data"])
        if bh:
            print("   inner boot header:", {k: v if k == "ver" else hex(v) for k, v in bh.items()})
        if out:
            os.makedirs(out, exist_ok=True)
            open(os.path.join(out, name + ".bin"), "wb").write(s["data"])
            if bh:
                open(os.path.join(out, name + "_preloader.bin"), "wb").write(s["data"][:STAGE2_OFF])
                open(os.path.join(out, name + "_main_0x60008000.bin"), "wb").write(s["data"][STAGE2_OFF:])


if __name__ == "__main__":
    main()
