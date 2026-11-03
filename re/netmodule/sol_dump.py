# Dump a Sol program (docs/module/warp.md) as four 16-bit slots per 64-bit word, both halves side by side.
# A program is two arrays of 64-bit words loaded into mem1 (0xD0760000: ALU/MAC/?/sequencer) and mem0 (0xD0720000:
# three address/load-store slots). Input: a raw Warp blob (Nokia's wslsepp_warp, KnOS's at 0x605DA510).
#   python3 -I sol_dump.py BLOB MEM1_OFF MEM0_OFF COUNT        e.g. nokia: 0x5ec8 0x6078 54
#   python3 -I sol_dump.py BLOB --stats ...                    op histogram only
import struct
import sys

args = [a for a in sys.argv[1:] if not a.startswith("--")]
stats = "--stats" in sys.argv
b = open(args[0], "rb").read()
m1o, m0o, n = (int(x, 0) for x in args[1:4])
ALU = {
    1: "Not",
    2: "Or",
    3: "Nor",
    4: "And",
    5: "Nand",
    6: "Xor",
    0xB: "Add",
    0xC: "Sub",
    0xD: "AddA",
    0xE: "SubA",
    0xF: "AddS",
    0x10: "SubS",
    0x11: "AddAS",
    0x12: "SubAS",
    0x13: "Comp",
    0x14: "Shift1",
    0x15: "Shift2",
    0x16: "Rot1",
    0x17: "Rot2",
    0x18: "SimdAdd",
    0x19: "SimdSub",
    0x1A: "SimdAddA",
    0x1B: "SimdSubA",
    0x1C: "SimdAddS",
    0x1D: "SimdSubS",
    0x1E: "SimdAddAS",
    0x1F: "SimdSubAS",
    0x20: "SimdComp",
    0x21: "SimdShift",
    0x22: "SimdRot",
    0x23: "Modeset",
    0x24: "Moderead",
}
MAC = {1: "Mac", 2: "Mult", 3: "InitMac", 5: "SMac", 6: "SMult", 7: "SInitMac"}


def slots(w):
    return [(w >> sh) & 0xFFFF for sh in (48, 32, 16, 0)]


hist = {}
if not stats:
    print(" i | mem1: ALU  MAC  ?    seq  | mem0: -    au2  au1  au0  | ALU               | MAC")
for i in range(n):
    (w1,) = struct.unpack_from("<Q", b, m1o + 8 * i)
    (w0,) = struct.unpack_from("<Q", b, m0o + 8 * i)
    a = slots(w1)
    c = slots(w0)
    alu = a[0]
    ad = "" if alu == 0 else "%-9s %03x" % (ALU.get(alu >> 10, "op%#x" % (alu >> 10)), alu & 0x3FF)
    m = a[1]
    md = "" if m == 0 else "%-8s %02x %02x" % (MAC.get(m >> 14, "mac%d" % (m >> 14)), (m >> 6) & 0xFF, m & 0x3F)
    if alu:
        hist["alu " + ad.split()[0]] = hist.get("alu " + ad.split()[0], 0) + 1
    if m:
        hist["mac " + md.split()[0]] = hist.get("mac " + md.split()[0], 0) + 1
    if not stats:
        print("%3d | %04x %04x %04x %04x | %04x %04x %04x %04x | %-17s | %s" % (i, *a, *c, ad, md))
if stats:
    print(" ".join("%s=%d" % kv for kv in sorted(hist.items())))
