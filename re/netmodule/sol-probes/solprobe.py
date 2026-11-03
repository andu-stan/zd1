#!/usr/bin/env python3
"""Sol probes on Warp1 through the KnOS shell or our OS's zd1-memsh (docs/module/warp.md, sections 3, 5 and 8). RAM
and registers only; some probes stall Sol on purpose (parking Warp1 recovers it) and a few wedge it (a Sol reset, tgen
0xE0007020/24 bit 21, recovers that).

  python3 -I solprobe.py HOST[:PORT] NAME[,NAME...] OUT     run probes, print a summary, save OUT/NAME.json
  python3 -I solprobe.py --list                             the probes, by pass

Two kinds, one registry (PROBES). The sets (first to third pass) are lists of cases, each one Sol program with its
data and reads, and one readout (a case may override it with 'kind'): `copy` (which bank-6..9 word every store wrote
and which word it had loaded: the fill is 0x10000000 + word index and the kernel stores ~loaded), `mac` (the
accumulator after the run = 1 + number of loop passes, in the dual MAC configuration), `count` (number of words
changed in the fill range: stores; with `store_loop` = loop passes), `raw` (the reads), `raw2` / `regs` (kern():
the stored results, lane 1.lane 0; `regs` undoes the Not), `condrow` (the nine lane-sign cases of a predicate),
`trace` (issue order), `au2`. The experiments (`p4-*`, `p5-*`: the fourth and fifth pass) drive the Warp themselves:
reads of the control block inside the Warp1 job (job_with_reads), several cases per job with re-runs after a stall
(run_batch), the Sol reset. Sol keeps registers, flags, loop counters, unit registers and the MAC configuration
across runs: each kernel sets what it relies on. The committed results are in the hw-* directories next to this
file (README.md).
"""

import json
import os
import struct
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.normpath(os.path.join(HERE, "..", "..", ".."))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(REPO, "tools", "zeppelin", "warp"))
import warpprobe as WP  # noqa: E402
import warprun as W  # noqa: E402

FILL = 0x10000000
NOT4 = (0x01 << 10) | 0x104  # Not, src1 = 4 (address unit 0's queue)
NOT5 = (0x01 << 10) | 0x105  # Not, src1 = 5 (address unit 1's queue)
CLEAR_EXTRA = [(4, 0xE200)]  # E2xx is not in KnOS's clear list and is sticky (bit 0: stores write 0)


def prog(n, slots):
    """slots: {instr: {alu, mac, third, seq, u0, u1, u2}} -> (mem1, mem0) as (lo, hi) word pairs, n instructions."""
    m1, m0 = [], []
    for i in range(n):
        s = slots.get(i, {})
        m1 += [(s.get("third", 0) << 16) | s.get("seq", 0xA000), (s.get("alu", 0) << 16) | s.get("mac", 0)]
        m0 += [(s.get("u1", 0) << 16) | s.get("u0", 0), s.get("u2", 0)]
    return m1, m0


def case(m1, m0, regs=(), fill=FILL, fill_range=(0x1800, 0x2800), reads=(), data=None, entry=4, ctrl=None):
    c = {
        "mem1": m1,
        "mem0": m0,
        "fill": fill,
        "fill_range": fill_range,
        "unit_regs": CLEAR_EXTRA + list(regs),
        "reads": list(reads),
        "entry": entry,
    }
    if data:
        c["data"] = data
    if ctrl:
        c["ctrl"] = ctrl
    return c


# ---- kernels -------------------------------------------------------------------------------------------------


def copy_kernel(n=8, regs=(), ld=0x7800, st=0x60FB, extra=None):
    """Address unit 1 loads (banks 8/9) at 1..n, Not at 2..n+1, address unit 0 stores (banks 6/7) at 4..n+3."""
    s = {0: {"u0": 0xF00F, "u1": 0xF00F}}
    for k in range(1, n + 1):
        s.setdefault(k, {})["u1"] = ld
    for k in range(2, n + 2):
        s.setdefault(k, {})["alu"] = NOT5
    for k in range(4, n + 4):
        s.setdefault(k, {})["u0"] = st
    s.setdefault(n + 6, {})["seq"] = 0x0038
    for k, v in (extra or {}).items():
        s.setdefault(k, {}).update(v)
    return case(*prog(n + 8, s), regs=regs)


def mac_loop(pre=(), jump=0xF380, body=12, length=8, pre_at=3):
    """Counted-loop probe: InitMac(1 x 1) before the loop, one Mac(1 x 1) per pass, the accumulator stored after it.
    Result lane 0 = 1 + passes. `pre`: sequencer words placed from instruction `pre_at`."""
    s = {0: {"u0": 0xF00F, "u1": 0xF00F}, 1: {"u0": 0x7800, "u1": 0x7800}, 2: {"mac": 0xC02C}}
    s.setdefault(body, {}).update(u0=0x7800, u1=0x7800)
    s.setdefault(body + 1, {})["mac"] = 0x402C
    s.setdefault(body + length - 1, {})["seq"] = jump | body
    for i, v in enumerate(pre):
        s.setdefault(pre_at + i, {})["seq"] = v
    st = body + length + 2
    s.setdefault(st, {})["u0"] = 0x60F2
    s.setdefault(st + 3, {})["seq"] = 0x0038
    return case(*prog(st + 5, s), fill=0, reads=(0x1800, 0x1C00), data={0x1800: 1, 0x1C00: 0, 0x2000: 1, 0x2400: 0})


def imm(idx, value, flags=0):
    """Sequencer immediate word: value[15:5] flags[4:2] idx[1:0]."""
    return (value << 5) | (flags << 2) | idx


def store_loop(body=12, length=8, jump=0xF380, pre=(), pre_at=3, extra=None):
    """Store pass counter: each pass loads one word (stride 1) and stores its complement, so the number of changed
    words in bank 6 = passes (up to 256). `pre`: sequencer words placed from instruction `pre_at`."""
    s = {0: {"u0": 0xF00F, "u1": 0xF00F}, 1: {"u1": 0xCC01}, 2: {"u1": 0xD201}}
    s.setdefault(body, {})["u1"] = 0x7800
    s.setdefault(body + 2, {})["alu"] = NOT5
    s.setdefault(body + 4, {})["u0"] = 0x60FB
    s.setdefault(body + length - 1, {})["seq"] = jump | body
    for i, v in enumerate(pre):
        s.setdefault(pre_at + i, {})["seq"] = v
    for k, v in (extra or {}).items():
        s.setdefault(k, {}).update(v)
    s.setdefault(body + length + 5, {})["seq"] = 0x0038
    return case(*prog(body + length + 8, s), fill_range=(0x1800, 0x1C00))


def mac_config(word):
    """A program that only issues one MAC word (op 0 = configuration, e.g. 0x2C00 = configuration 0xB0)."""
    s = {0: {"u0": 0xF00F, "u1": 0xF00F}, 2: {"mac": word}, 6: {"seq": 0x0038}}
    return case(*prog(8, s), fill=0, fill_range=(0x1800, 0x1804))


def knos_alu(op, a, b, opf=0x12C, mode=None):
    c = W.knos_case(alu=(op << 10) | opf, a=a, b=b)
    c.update(fill=0, entry=4, unit_regs=CLEAR_EXTRA + ([(3, 0x8C00 | mode)] if mode is not None else []))
    return c


def knos_mac(op, mm, a, b, opf=0x2C):
    c = W.knos_case(mac=(op << 14) | (mm << 6) | opf, a=a, b=b, extra_mem0={10: 0x60F2})
    c.update(fill=0, entry=4, unit_regs=list(CLEAR_EXTRA))
    return c


def nokia_program():
    """Nokia's 54-instruction Sol program from wslsepp_warp (blobs/module/nokia-hd1/wslsepp_warp.bin, or $WSLSEPP)."""
    path = os.environ.get("WSLSEPP", os.path.join(REPO, "blobs", "module", "nokia-hd1", "wslsepp_warp.bin"))
    b = open(path, "rb").read()
    return list(struct.unpack_from("<108I", b, 0x5EC8)), list(struct.unpack_from("<108I", b, 0x6078))


# ---- probe sets: name -> (readout, cases) --------------------------------------------------------------------

AGU_GROUPS = [0xC800, 0xCA00, 0xCC00, 0xCE00, 0xD000, 0xD200, 0xD400, 0xD600, 0xD800, 0xDA00, 0xDC00, 0xDE00]


def set_agu():
    c = [("base", copy_kernel())]
    c += [("u5 %04x" % (g | v), copy_kernel(regs=[(5, g | v)])) for g in AGU_GROUPS for v in (1, 3)]
    c += [
        ("u5 cc01 d201 +%04x" % (g | 5), copy_kernel(regs=[(5, 0xCC01), (5, 0xD201), (5, g | 5)]))
        for g in AGU_GROUPS
        if g not in (0xCC00, 0xD200)
    ]
    c += [("u4 %04x (store side)" % (g | 3), copy_kernel(regs=[(4, g | 3)])) for g in AGU_GROUPS]
    c += [("bitrev", copy_kernel(ld=0x7A00, regs=[(5, 0xCC01)]))]
    return "copy", c


def set_alu():
    A, B = 0x0000010000000010, 0x0000200000030000
    S1, S2 = 0x7FFFFFF0FFFFFFF0, 0x0000002000000020
    c = [("op%02x lanes" % op, knos_alu(op, A, B)) for op in (0x18, 0x19, 0x20)]
    c += [
        ("mode%x op%02x" % (m, op), knos_alu(op, S1, S2, mode=m))
        for m in (0, 1, 2, 3, 0xF)
        for op in (0x0F, 0x11, 0x1A, 0x1B, 0x1C, 0x1D)
    ]
    c += [
        ("op%02x amt%d" % (op, n), knos_alu(op, 0x8000000180000001, n))
        for op in (0x14, 0x15, 0x16, 0x17, 0x21, 0x22)
        for n in (4, 31, 32, 45, 63)
    ]
    c += [
        ("op%02x opf%03x" % (op, f), knos_alu(op, 0x1111111122222222, 0x3333333344444444, f))
        for op in range(0x25, 0x2D)
        for f in (0x12C, 0x0EC, 0x125, 0x064, 0x3E4)
    ]
    return "raw", c


def set_mac():
    v = (0x0004000300020001, 0x0000000700000005)
    c = [("mult mm%02x" % mm, knos_mac(2, mm, *v)) for mm in (0x00, 0x40, 0x80, 0xC0, 0x98, 0x74, 0xD0)]
    c += [("mult mm%02x 0x100x1" % mm, knos_mac(2, mm, 0x100, 1)) for mm in (0, 1, 2, 3)]
    c += [("initmac", knos_mac(3, 0x80, *v)), ("mac", knos_mac(1, 0x80, *v))]
    return "raw", c


def set_loop():
    c = [
        ("no jump", mac_loop(jump=0xA000 - 12)),
        ("F38x", mac_loop()),
        ("F34x", mac_loop(jump=0xF340)),
        ("F30x (unconditional: times out)", mac_loop(jump=0xF300)),
    ]
    c += [("pre %04x" % w, mac_loop((w,))) for w in (0xA004, 0xA024, 0xA044, 0xA084, 0x0004, 0x9401, 0x6408)]
    c += [("imm 5 -> 88%02x" % n, mac_loop((0x14, 0x15, 0x16, 0x17, 0x8800 | n))) for n in range(1, 8)]
    return "mac", c


def set_seq():
    c = [("count %d" % n, store_loop(body=24, pre=(imm(0, n), 0x8801))) for n in (1, 3, 100, 255, 0, 300)]
    c += [
        (
            "cond %d%d%d, count 5" % (k >> 2, (k >> 1) & 1, k & 1),
            store_loop(body=24, jump=0xF200 | k << 6, pre=(imm(0, 5), 0x8801)),
        )
        for k in range(8)
    ]
    c += [("no commit (leftover counter)", store_loop(body=24))]
    return "count", c


def set_macmode():
    v = (0x0004000300020001, 0x0000000700000005)
    c = []
    for cfg in (0xB0, 0x80):
        c.append(("configure %02x" % cfg, mac_config(cfg << 6)))
        c += [("  mult mm%02x" % mm, knos_mac(2, mm, *v)) for mm in (0x00, 0x80, 0x84, 0x88, 0x8C, 0x98)]
        c.append(("  mult mm00 0x100x1", knos_mac(2, 0, 0x100, 1)))
    return "raw", c


def set_program():
    c = []
    for e in (4, 0x28, 0x140, 0x144, 0x14C):  # kernel at 40, ends at 1 and 30 in front of it
        s = {40: {"u0": 0xF00F, "u1": 0xF00F}, 1: {"seq": 0x0038}, 30: {"seq": 0x0038}, 52: {"seq": 0x0038}}
        for k in range(42, 46):
            s.setdefault(k, {})["u1"] = 0x7800
        for k in range(43, 47):
            s.setdefault(k, {})["alu"] = NOT5
        for k in range(45, 49):
            s.setdefault(k, {})["u0"] = 0x60FB
        c.append(("entry %#x" % e, case(*prog(60, s), entry=e)))
    return "copy", c


def set_nokia():
    m1, m0 = nokia_program()
    c = [("nokia", case(list(m1), list(m0), fill_range=(0, 0x2800)))]
    c += [("nokia bank %d" % b, case(list(m1), list(m0), fill_range=(b * 0x400, b * 0x400 + 0x400))) for b in range(10)]
    for j in (0xA000, 0xF304, 0xF384, 0xF3C4, 0xF244):
        a = list(m1)
        a[80] = (a[80] & 0xFFFF0000) | j
        c.append(("instr40 seq %04x" % j, case(a, list(m0), fill_range=(0x2400, 0x2800))))
    return "count", c


# ---- second pass (2026-10-22, afternoon): the open questions of warp.md section 9 -----------------------------


def set_loops2():
    """Loop geometry with the counter committed (LC = 5): is a 20-instruction body or a jump at 64+ really a
    problem, or was it the leftover counter?"""
    c = []
    for body, length in (
        (12, 20),
        (20, 20),
        (5, 20),
        (12, 16),
        (12, 24),
        (12, 32),
        (33, 20),
        (50, 12),
        (50, 15),
        (50, 16),
        (60, 10),
        (40, 30),
        (20, 50),
        (10, 100),
    ):
        c.append(
            ("body %d len %d LC=5" % (body, length), store_loop(body=body, length=length, pre=(imm(0, 5), 0x8801)))
        )
    return "count", c


def set_immfmt():
    """Immediate word bits 4:2: a fifth index (vendor 0x0004), flags, or part of idx0? And a 0x0004 after the
    commit (Nokia's order: its loop runs 256 passes with idx0 = 1)."""
    L = lambda *pre: store_loop(body=24, pre=pre)
    c = [
        ("imm0=5 commit", L(imm(0, 5), 0x8801)),
        ("imm0=5 0x0004 commit", L(imm(0, 5), 0x0004, 0x8801)),
        ("imm0=5 (7<<5)|4 commit", L(imm(0, 5), (7 << 5) | 4, 0x8801)),
        ("imm0=5 (7<<5)|8 commit", L(imm(0, 5), (7 << 5) | 8, 0x8801)),
        ("imm0=5 (7<<5)|0x10 commit", L(imm(0, 5), (7 << 5) | 0x10, 0x8801)),
        ("(5<<5)|4 commit", L((5 << 5) | 4, 0x8801)),
        ("(5<<5)|8 commit", L((5 << 5) | 8, 0x8801)),
        ("(5<<5)|0x10 commit", L((5 << 5) | 0x10, 0x8801)),
        ("imm0=5 commit 0x0004", L(imm(0, 5), 0x8801, 0x0004)),
        ("imm0=5 commit 0x0003", L(imm(0, 5), 0x8801, 0x0003)),
        ("imm0=5 commit imm0=9", L(imm(0, 5), 0x8801, imm(0, 9))),
        ("imm0=5 commit a004", L(imm(0, 5), 0x8801, 0xA004)),
        ("imm0=5 commit 0x8800", L(imm(0, 5), 0x8801, 0x8800)),
        ("imm0=5 0x8800", L(imm(0, 5), 0x8800)),
        ("imm0=5 0x8808", L(imm(0, 5), 0x8808)),
        ("imm0=5 0x8809", L(imm(0, 5), 0x8809)),
        ("imm0=5 0x8811", L(imm(0, 5), 0x8811)),
        ("imm0=5 0x8821", L(imm(0, 5), 0x8821)),
        ("imm0=5 0x8841", L(imm(0, 5), 0x8841)),
        ("imm0=5 0x8881", L(imm(0, 5), 0x8881)),
        ("imm0=5 0x8901", L(imm(0, 5), 0x8901)),
        ("imm0=5 0x8A01", L(imm(0, 5), 0x8A01)),
        ("imm0=5 0x8C01", L(imm(0, 5), 0x8C01)),
        ("imm0=5 0x9001", L(imm(0, 5), 0x9001)),
    ]
    return "count", c


def u2_kernel(n=8, regs=(), fill_range=(0, 0x2800), st=0x60FB):
    """copy_kernel with the stores issued by address unit 2 (mem0 bits 47:32), all ten banks filled."""
    k = copy_kernel(n, regs=regs, st=0x8000, extra={i: {"u2": st} for i in range(4, n + 4)})
    k["fill_range"] = fill_range
    return k


def set_unit2b():
    c = [
        ("u2 store, all banks", u2_kernel()),
        ("u2 store, window 6 F00F + E454", u2_kernel(regs=[(6, 0xF00F), (6, 0xE454)])),
        ("u2 store, window 6 E410", u2_kernel(regs=[(6, 0xE410)])),
        ("u2 store, window 2 E454", u2_kernel(regs=[(2, 0xE454)])),
        ("u2 store, window 7 E454", u2_kernel(regs=[(7, 0xE454)])),
        ("u2 store, window 0 E454", u2_kernel(regs=[(0, 0xE454)])),
        ("u2 store, window 1 E454", u2_kernel(regs=[(1, 0xE454)])),
        ("u2 store 0x61FB (packed)", u2_kernel(st=0x61FB)),
        ("u2 store, window 6 E601", u2_kernel(regs=[(6, 0xE601)])),
        ("u0 store control, all banks", dict(copy_kernel(), fill_range=(0, 0x2800))),
    ]
    return "copy", c


def mac_case(mm, lane, short, op=2):
    """Mult (op 2) with both lanes = `lane` and all four shorts = `short` (opf 0x2C: src2 = B lanes, src1 = A
    shorts; macmode bits 5:2 pick the shorts): result lane 1 = acc A, lane 0 = acc B."""
    a = (short & 0xFFFF) * 0x0001000100010001
    b = (lane & 0xFFFFFFFF) | ((lane & 0xFFFFFFFF) << 32)
    return knos_mac(op, mm, a, b)


def set_macshift():
    """The MAC output for products 2^k under each output-shift code (macmode bits 1:0), dual configuration."""
    c = [("configure b0", mac_config(0x2C00))]
    ks = list(range(0, 45, 4)) + [46]
    for mm in (0x83, 0x82, 0x81, 0x80):
        for k in ks:
            if k <= 30:
                lane, short = 1 << k, 1
            else:
                lane, short = 1 << 30, 1 << (k - 30)
            if k == 46:
                lane, short = 0x7FFFFFFF, 0x7FFF
            c.append(("mm%02x 2^%d (%x x %x)" % (mm, k, lane, short), mac_case(mm, lane, short)))
    for k in (8, 32, 44):
        lane, short = (1 << k, 1) if k <= 30 else (1 << 30, 1 << (k - 30))
        c.append(("mm83 -2^%d" % k, mac_case(0x83, lane, -short)))
        c.append(("mm82 -2^%d" % k, mac_case(0x82, lane, -short)))
    return "raw", c


def not_src(k):
    return (0x01 << 10) | (k & 7)


def seqword_kernel(word, third, setvals=(0x11, 0x22, 0x33, 0x44), setn=6, srcs=(0, 1, 2), at=8, alu=None):
    """A set committed with known values, then one sequencer `word` with `third` in the third field, then ALU
    Not from each queue in `srcs` (one per instruction) and a store per result: what each queue held. An unfed
    queue reads 0 (store = 0xFFFFFFFF)."""
    s = {0: {"u0": 0xF00F, "u1": 0xF00F}}
    for i, v in enumerate(setvals):
        s[1 + i] = {"seq": imm(i, v)}
    s[5] = {"seq": 0x8800 | setn}
    s[at] = {"seq": word, "third": third}
    for i, k in enumerate(srcs):
        s[at + 2 + i] = {"alu": alu if alu is not None else not_src(k)}
        s[at + 4 + i] = {"u0": 0x60FB}
    s[at + 4 + len(srcs) + 4] = {"seq": 0x0038}
    return case(*prog(at + 4 + len(srcs) + 6, s), fill_range=(0x1800, 0x1C00))


def set_seqwords():
    """The undecoded sequencer words of the vendor programs with their third fields: does any of them put data in
    a queue (read through ALU sources 0, 1, 2, then 3, 6, 7)?"""
    vendor = [
        (0xD58C, 0x0033),
        (0xD58C, 0x0023),
        (0xD58C, 0x2030),
        (0xD58C, 0x7030),
        (0xD58C, 0x3050),
        (0xD58C, 0x1050),
        (0xD58C, 0x2000),
        (0xD58C, 0x0000),
        (0xD5AD, 0x6000),
        (0xD5AD, 0x4050),
        (0xD5AD, 0x4000),
        (0xDD8D, 0x1000),
        (0x998F, 0x6000),
        (0x998F, 0x3000),
        (0x998F, 0x6050),
        (0x998F, 0x3100),
        (0x99EC, 0x7000),
        (0x99EC, 0x6000),
        (0x99EC, 0x7050),
        (0x99EC, 0x6100),
        (0x9D8F, 0x2000),
        (0x9401, 0x0000),
        (0x9402, 0x0000),
        (0x8806, 0x7000),
        (0x8806, 0x5000),
        (0xA000, 0x0045),
        (0xA000, 0x6033),
        (0xA000, 0x1541),
        (0xA000, 0x0153),
        (0xA004, 0x0000),
    ]
    c = [("%04x third %04x src012" % (w, t), seqword_kernel(w, t)) for w, t in vendor]
    c += [
        ("%04x third %04x src367" % (w, t), seqword_kernel(w, t, srcs=(3, 6, 7)))
        for w, t in ((0xD58C, 0x0033), (0xD58C, 0x2030), (0x998F, 0x6000), (0x99EC, 0x7000), (0xD5AD, 0x6000))
    ]
    c += [
        ("%04x third %04x 2B-move src%d" % (w, t, k), seqword_kernel(w, t, srcs=(k,), alu=(0x2B << 10) | k))
        for w, t, k in ((0xD58C, 0x0033, 0), (0xD58C, 0x0033, 1), (0x998F, 0x6000, 0), (0x99EC, 0x7000, 0))
    ]
    return "copy", c


def queue_kernel(feed, srcs, data=None, fill_range=(0x1800, 0x1C00), fill=FILL):
    """`feed`: {instr: slots} that put data somewhere; then Not from each of `srcs`, a store each."""
    s = {0: {"u0": 0xF00F, "u1": 0xF00F}}
    for k, v in feed.items():
        s.setdefault(k, {}).update(v)
    at = max(feed) + 2
    for i, k in enumerate(srcs):
        s.setdefault(at + i, {})["alu"] = not_src(k)
        s.setdefault(at + 2 + i, {})["u0"] = 0x60FB
    s[at + 2 + len(srcs) + 3] = {"seq": 0x0038}
    c = case(*prog(at + 2 + len(srcs) + 5, s), fill_range=fill_range, fill=fill)
    if data:
        c["data"] = data
    return c


MULT_B0 = (2 << 14) | (0xB0 << 6) | 0x2C  # Mult, dual, src2 = 5 (unit 1 lanes), src1 = 4 (unit 0 shorts)


def set_queues():
    """Which source select names which queue: the MAC output, unit 2 loads, the ALU destination field, and the
    store word's lane sources."""
    c = []
    # MAC output: unit 0 loads shorts (bank 6/7 words 0 = A), unit 1 loads lanes (bank 8/9 word 0 = B)
    data = {0x1800: 0x00030000, 0x1C00: 0x00050000, 0x2000: 0x00000100, 0x2400: 0x00000200}
    for k in range(8):
        c.append(
            (
                "mult then Not src%d" % k,
                queue_kernel({1: {"u0": 0x7800, "u1": 0x7800}, 2: {"mac": MULT_B0}}, (k,), data=data, fill=0),
            )
        )
    c.append(
        (
            "mult then store 60F2 (control)",
            queue_kernel(
                {1: {"u0": 0x7800, "u1": 0x7800}, 2: {"mac": MULT_B0}, 4: {"u0": 0x60F2}}, (), data=data, fill=0
            ),
        )
    )
    # unit 2 load, all banks filled with 0x10000000 + word index
    for k in range(8):
        c.append(("u2 load then Not src%d" % k, queue_kernel({1: {"u2": 0x7800}}, (k,), fill_range=(0, 0x2800))))
    c.append(
        (
            "u2 load (window 6 E498) then Not src0-2",
            queue_kernel({1: {"u2": 0x7800}}, (0, 1, 2), fill_range=(0, 0x2800)),
        )
    )
    c[-1][1]["unit_regs"] = CLEAR_EXTRA + [(6, 0xF00F), (6, 0xE498)]
    # ALU dst: Add(4,5) with dst d, then Not from src d (and 3)
    for d in (0, 1, 2, 3, 6, 7, 8, 15):
        add = (0x0B << 10) | (d << 6) | (5 << 3) | 4
        c.append(
            (
                "add dst%d then Not src%d,3" % (d, d & 7),
                queue_kernel({1: {"u0": 0x7800, "u1": 0x7800}, 2: {"alu": add}}, (d & 7, 3), fill_range=(0, 0x2800)),
            )
        )
    # store lane sources after loads (unit 0 = banks 6/7, unit 1 = banks 8/9 at word 0), ALU idle
    for st in (0x60F0, 0x60F1, 0x60F4, 0x60F5, 0x60F8, 0x60FC, 0x60FF, 0x60F3):
        c.append(
            (
                "loads then store %04x" % st,
                queue_kernel({1: {"u0": 0x7800, "u1": 0x7800}, 3: {"u0": st}}, (), fill_range=(0, 0x2800)),
            )
        )
    return "copy", c


def set_alu2():
    """ALU: which ops the mode shift (bits 3:2) reaches; mode bit 1; op 0x2C."""
    A, B = 0x0000100000001000, 0x0000001000000010
    ops = list(range(0x0B, 0x14)) + list(range(0x18, 0x23))
    c = [("mode%x op%02x" % (m, op), knos_alu(op, A, B, mode=m)) for m in (0, 0xC) for op in ops]
    S1, S2 = 0x7FFFFFF0FFFFFFF0, 0x0000002000000020
    c += [("sat mode%x op%02x" % (m, op), knos_alu(op, S1, S2, mode=m)) for m in (0, 1, 2, 3) for op in (0x0F, 0x11)]
    N1, N2 = 0x8000001080000010, 0xFFFFFFE0FFFFFFE0
    c += [("neg mode%x op%02x" % (m, op), knos_alu(op, N1, N2, mode=m)) for m in (0, 1, 2, 3) for op in (0x0F, 0x11)]
    pairs = [
        (0x1111111122222222, 0x3333333344444444),
        (0x0000000100000002, 0x0000000300000004),
        (0xAAAAAAAABBBBBBBB, 0xCCCCCCCCDDDDDDDD),
        (0x1111111122222222, 0x1111111122222222),
    ]
    c += [("op2C %016x %016x opf%03x" % (a, b, f), knos_alu(0x2C, a, b, f)) for a, b in pairs for f in (0x12C, 0x125)]
    c += [
        ("op2C %016x %016x opf%03x" % (pairs[0][0], pairs[0][1], f), knos_alu(0x2C, *pairs[0], f))
        for f in (0x02C, 0x06C, 0x0AC, 0x0EC, 0x16C, 0x1AC, 0x1EC, 0x32C, 0x124, 0x12D, 0x134, 0x13C)
    ]
    return "raw", c


def set_ctrl():
    """Control bytes +0x14 / +0x16 / +0x17: each bit flipped from the vendor values (0xE0, 0x0E, 0x54) under the
    copy kernel (loads, Not, stores) and a MAC known answer."""
    base = (0xE0, 0x0E, 0x54)
    data = {0x1800: 0x00030000, 0x1C00: 0x00050000, 0x2000: 0x00000100, 0x2400: 0x00000200}

    def mac():
        return queue_kernel(
            {1: {"u0": 0x7800, "u1": 0x7800}, 2: {"mac": MULT_B0}, 4: {"u0": 0x60F2}}, (), data=data, fill=0
        )

    variants = [("base", base)]
    for bit in range(8):
        variants.append(("+14 ^%02x" % (1 << bit), (0xE0 ^ (1 << bit), 0x0E, 0x54)))
    for bit in range(8):
        variants.append(("+16 ^%02x" % (1 << bit), (0xE0, 0x0E ^ (1 << bit), 0x54)))
    for bit in range(8):
        variants.append(("+17 ^%02x" % (1 << bit), (0xE0, 0x0E, 0x54 ^ (1 << bit))))
    variants += [("all zero", (0, 0, 0)), ("base again", base)]
    c = []
    for label, ctrl in variants:
        k = copy_kernel()
        k["ctrl"] = ctrl
        c.append(("%s copy" % label, k))
        m = mac()
        m["ctrl"] = ctrl
        m["reads"] = [0x1800, 0x1C00]
        c.append(("%s mac" % label, m))
    return "copy", c


def set_loops3():
    """Why do 20-instruction bodies and jumps at 64+ 'run once', and why did LC = 5 not land in loops2? The
    same geometry with the MAC pass counter, with program memory padded to 128 instructions, with the commit
    placed just before the loop, and under the vendor's flag conditions."""
    pre = (imm(0, 5), 0x8801)
    c = [
        ("store body 24 len 8 LC=5 (seq set repeat)", store_loop(body=24, pre=pre)),
        ("store body 24 len 8 LC=5 pre_at 20", store_loop(body=24, pre=pre, pre_at=20)),
        ("store body 12 len 16 LC=5 pre_at 8", store_loop(body=12, length=16, pre=pre, pre_at=8)),
        ("store body 12 len 20 LC=5 pre_at 8", store_loop(body=12, length=20, pre=pre, pre_at=8)),
        ("mac body 12 len 16 LC=5", mac_loop(pre=pre, body=12, length=16)),
        ("mac body 12 len 20 LC=5", mac_loop(pre=pre, body=12, length=20)),
        ("mac body 12 len 24 LC=5", mac_loop(pre=pre, body=12, length=24)),
        ("mac body 50 len 15 LC=5", mac_loop(pre=pre, body=50, length=15)),
        ("mac body 12 len 8 LC=5", mac_loop(pre=pre, body=12, length=8)),
    ]
    for body, length in ((12, 16), (12, 20)):
        k = store_loop(body=body, length=length, pre=pre)
        m1, m0 = k["mem1"], k["mem0"]
        n = len(m1) // 2
        k["mem1"] = m1 + [0xA000, 0] * (128 - n)
        k["mem0"] = m0 + [0, 0] * (128 - n)
        c.append(("store body %d len %d LC=5 padded 128" % (body, length), k))
    for j in (0xF340, 0xF3C0, 0xF280, 0xF300):
        c.append(("store body 12 len 20 LC=5 jump %04x" % j, store_loop(body=12, length=20, pre=pre, jump=j)))
    c.append(("store body 12 len 20 no commit", store_loop(body=12, length=20)))
    c.append(("store body 12 len 16 no commit", store_loop(body=12, length=16)))
    return "count", c


def set_unit2c():
    """Address unit 2 stores: in-slot reset and bank map (as the vendors reset it, P1 74/77), the vendors' own store
    words (one lane at a time), and every unit window for its bank map."""

    def k(regs=(), st=0x60FB, inslot=None):
        extra = {i: {"u2": st} for i in range(4, 12)}
        extra[0] = {"u2": 0xF00F}
        for i, v in enumerate(inslot or []):
            extra[1 + i] = {"u2": v}
        c = copy_kernel(regs=regs, st=0x8000, extra=extra)
        c["fill_range"] = (0, 0x2800)
        return c

    c = [
        ("u2 F00F in-slot, store 60FB", k()),
        ("u2 F00F, E454 in-slot, store 60FB", k(inslot=[0xE454])),
        ("u2 F00F, E454, C000, C200 in-slot", k(inslot=[0xE454, 0xC000, 0xC200])),
        ("u2 F00F, E454 in-slot, store 605B", k(inslot=[0xE454], st=0x605B)),
        ("u2 F00F, E454 in-slot, store 609B", k(inslot=[0xE454], st=0x609B)),
        ("u2 F00F, E454 in-slot, store 617B", k(inslot=[0xE454], st=0x617B)),
        ("u2 F00F in-slot, store 605B", k(st=0x605B)),
        ("u2 F00F, E600 in-slot, store 605B", k(inslot=[0xE600], st=0x605B)),
        ("u2 F00F, E601 in-slot, store 605B", k(inslot=[0xE601], st=0x605B)),
    ]
    c += [
        ("u2 window %d F00F E454, store 605B" % u, k(regs=[(u, 0xF00F), (u, 0xE454)], st=0x605B))
        for u in range(16)
        if u not in (3, 4, 5)
    ]
    extra = {i: {"u1": 0x605B} for i in range(4, 12)}
    extra.update({0: {"u1": 0xF00F}, 1: {"u1": 0xE454}})
    c.append(
        (
            "u1 F00F E454 in-slot store 605B (control)",
            dict(copy_kernel(regs=[], st=0x8000, extra=extra), fill_range=(0, 0x2800)),
        )
    )
    return "copy", c


def set_loops4():
    """One variable at a time from the known-good geometry (commit at 4, loop 24..31, LC = 5): the commit
    position, the jump position, the target."""
    LC = lambda n: (imm(0, n), 0x8800)
    c = [
        ("C%d T24 J31" % C, store_loop(body=24, length=8, pre=LC(5), pre_at=C - 1))
        for C in (1, 2, 6, 8, 10, 12, 14, 16, 18, 20, 22)
    ]
    c += [
        ("C4 T24 J%d" % J, store_loop(body=24, length=J - 23, pre=LC(5))) for J in (29, 33, 35, 39, 43, 47, 55, 63, 71)
    ]
    c += [("C4 T%d J31" % T, store_loop(body=T, length=32 - T, pre=LC(5))) for T in (8, 12, 16, 18, 20, 22, 26, 28)]
    c += [("C4 T24 J31 LC=%d" % n, store_loop(body=24, length=8, pre=LC(n))) for n in (1, 2)]
    c.append(("C4 T24 J31 LC=5 again", store_loop(body=24, length=8, pre=LC(5))))
    return "count", c


def set_macneg():
    """Negative products under each output shift, Mult and InitMac, positive controls in between."""
    c = [("configure b0", mac_config(0x2C00))]
    for mm in (0x80, 0x81, 0x82, 0x83):
        for op, name in ((2, "mult"), (3, "initmac")):
            for k in (0, 4, 8, 12, 16, 24, 32, 40):
                lane, short = (1 << k, 1) if k <= 30 else (1 << 30, 1 << (k - 30))
                c.append(("mm%02x %s +2^%d" % (mm, name, k), mac_case(mm, lane, short, op=op)))
                c.append(("mm%02x %s -2^%d" % (mm, name, k), mac_case(mm, lane, -short, op=op)))
    return "raw", c


def mac_chain(n, lane, short, mm=0x83, delay=4, stores=1, gap=1):
    """InitMac then n - 1 Mac ops of lane x short (every op fed by its own loads, `gap` instructions apart), the
    accumulators stored `delay` instructions after the last op (`stores` consecutive stores)."""
    a = (short & 0xFFFF) * 0x0001000100010001
    b = (lane & 0xFFFFFFFF) | ((lane & 0xFFFFFFFF) << 32)
    s = {0: {"u0": 0xF00F, "u1": 0xF00F}}
    for i in range(n):
        s[1 + i * gap] = {"u0": 0x7800, "u1": 0x7800}
        s.setdefault(2 + i * gap, {})["mac"] = ((3 if i == 0 else 1) << 14) | (mm << 6) | 0x2C
    last = 2 + (n - 1) * gap
    for k in range(stores):
        s.setdefault(last + delay + k, {})["u0"] = 0x60F2
    s[last + delay + stores + 4] = {"seq": 0x0038}
    return case(
        *prog(last + delay + stores + 6, s),
        fill=0,
        reads=(0x1800, 0x1C00, 0x1804, 0x1C04, 0x1808, 0x1C08),
        data={0x1800: a & 0xFFFFFFFF, 0x1C00: a >> 32, 0x2000: b & 0xFFFFFFFF, 0x2400: b >> 32},
    )


def set_macacc():
    """Accumulator width: chains of 2^44 (and 2^40) products read through the >> 16 output."""
    c = [("configure b0", mac_config(0x2C00))]
    c += [("2^44 x %d >>16" % n, mac_chain(n, 1 << 30, 1 << 14)) for n in (1, 2, 4, 7, 8, 9, 12, 15, 16, 17, 24, 32)]
    c += [("2^44 x %d >>0" % n, mac_chain(n, 1 << 30, 1 << 14, mm=0x80)) for n in (8, 16)]
    c += [("-2^44 x %d >>16" % n, mac_chain(n, 1 << 30, -(1 << 14))) for n in (1, 8, 16, 17)]
    c += [("2^40 x %d >>16" % n, mac_chain(n, 1 << 30, 1 << 10)) for n in (1, 64, 100)]
    return "raw", c


def reg_kernel(words, reads_at, srcs=(0,), stores_from=None, n=None, fill_range=(0x1800, 0x1C00), loop=None):
    """Sequencer words {instr: (seq, third)} then at each of `reads_at` a Not from `srcs[i % len]` with its
    store two later. `loop`: (start, end, cond) adds a jump at `end` back to `start`."""
    s = {0: {"u0": 0xF00F, "u1": 0xF00F}}
    for i, (seq, third) in words.items():
        s.setdefault(i, {}).update(seq=seq, third=third)
    last = 0
    for k, i in enumerate(reads_at):
        s.setdefault(i, {})["alu"] = not_src(srcs[k % len(srcs)])
        s.setdefault(i + 2, {})["u0"] = 0x60FB
        last = max(last, i + 2)
    if loop:
        start, end, cond = loop
        s.setdefault(end, {})["seq"] = cond | start
        last = max(last, end)
    s.setdefault(last + 4, {})["seq"] = 0x0038
    return case(*prog(n or last + 6, s), fill_range=fill_range)


def set_seqreg():
    """Source 0 = the committed set registers? Read it at distances from a commit, with two sets, through the
    vendor words, with indexes 4-7, and inside a counted loop."""

    def commit(at, vals, setn):
        return {at + i: (imm(i, v), 0) for i, v in enumerate(vals)} | {at + len(vals): (0x8800 | setn, 0)}

    S6 = (0x11, 0x22, 0x33, 0x44)
    S3 = (0x55, 0x66, 0x77, 0x88)
    c = [("set6 then src0 at +%d" % d, reg_kernel(commit(1, S6, 6), [5 + d])) for d in (1, 2, 3, 5, 8, 12)]
    c += [("set6 then src%d at +8" % k, reg_kernel(commit(1, S6, 6), [13], srcs=(k,))) for k in (1, 2, 3)]
    c.append(("set3 then set6, src0 +8", reg_kernel(commit(1, S3, 3) | commit(6, S6, 6), [18])))
    c.append(("set6 then set3, src0 +8", reg_kernel(commit(1, S6, 6) | commit(6, S3, 3), [18])))
    c.append(
        (
            "set6, set3 staged no commit, src0 +8",
            reg_kernel(commit(1, S6, 6) | {6: (imm(0, 0x55), 0), 7: (imm(1, 0x66), 0)}, [18]),
        )
    )
    for w, t in (
        (0xD58C, 0x0033),
        (0xD58C, 0x0023),
        (0xD58C, 0x2030),
        (0xD58C, 0x7030),
        (0xD58C, 0x0000),
        (0x998F, 0x6000),
        (0x998F, 0x3000),
        (0x99EC, 0x7000),
        (0x99EC, 0x6000),
        (0x9D8F, 0x2000),
        (0xD5AD, 0x6000),
        (0xDD8D, 0x1000),
        (0x9401, 0),
        (0x9402, 0),
        (0x9403, 0),
        (0x9406, 0),
        (0xA004, 0),
        (0xA000, 0x1541),
        (0xA000, 0x0153),
    ):
        c.append(
            (
                "set3, set6, %04x/%04x, src0 +8" % (w, t),
                reg_kernel(commit(1, S3, 3) | commit(6, S6, 6) | {12: (w, t)}, [20]),
            )
        )
    c.append(
        (
            "set6 + idx4-7 = 99 aa bb cc, src0,1,2 +8",
            reg_kernel(
                {
                    1: (imm(0, 0x11), 0),
                    2: (imm(1, 0x22), 0),
                    3: (imm(2, 0x33), 0),
                    4: (imm(3, 0x44), 0),
                    5: (imm(4, 0x99), 0),
                    6: (imm(5, 0xAA), 0),
                    7: (imm(6, 0xBB), 0),
                    8: (imm(7, 0xCC), 0),
                    9: (0x8806, 0),
                },
                [17, 18, 19],
                srcs=(0, 1, 2),
            ),
        )
    )
    c.append(
        (
            "set6 idx0 = 0x1FF (11 bits), src0 +8",
            reg_kernel(
                {1: (imm(0, 0x1FF), 0), 2: (imm(1, 0), 0), 3: (imm(2, 0), 0), 4: (imm(3, 0), 0), 5: (0x8806, 0)}, [13]
            ),
        )
    )
    # inside a counted loop: LC = 5 committed as set 1; one src0 read per pass
    c.append(("loop LC=5: src0 each pass", reg_kernel(commit(1, (5, 0, 0, 0), 1), [24], loop=(24, 31, 0xF380))))
    c.append(
        (
            "loop LC=5 set6 before: src0 each pass",
            reg_kernel(commit(1, S6, 6) | commit(8, (5, 0, 0, 0), 1), [24], loop=(24, 31, 0xF380)),
        )
    )
    c.append(
        ("loop LC=5: src1 each pass", reg_kernel(commit(1, (5, 0, 0, 0), 1), [24], srcs=(1,), loop=(24, 31, 0xF380)))
    )
    c.append(
        ("loop LC=5: src2 each pass", reg_kernel(commit(1, (5, 0, 0, 0), 1), [24], srcs=(2,), loop=(24, 31, 0xF380)))
    )
    c.append(("after loop LC=5: src0", reg_kernel(commit(1, (5, 0, 0, 0), 1), [40], loop=(24, 31, 0xF380))))
    return "copy", c


def set_queues2():
    """The MAC output and which source it reaches (redo with reads)."""
    data = {0x1800: 0x00030000, 0x1C00: 0x00050000, 0x2000: 0x00000100, 0x2400: 0x00000200}
    c = [("configure b0", mac_config(0x2C00))]
    for k in range(8):
        q = queue_kernel({1: {"u0": 0x7800, "u1": 0x7800}, 2: {"mac": MULT_B0}}, (k,), data=data, fill=0)
        q["reads"] = [0x1800, 0x1C00]
        c.append(("mult then Not src%d" % k, q))
    q = queue_kernel({1: {"u0": 0x7800, "u1": 0x7800}, 2: {"mac": MULT_B0}, 4: {"u0": 0x60F2}}, (), data=data, fill=0)
    q["reads"] = [0x1800, 0x1C00]
    c.append(("mult then store 60F2 (control)", q))
    return "raw", c


def set_alu3():
    """Op 0x2C and the vendors' 0x2B with single-bit operands; the 64-bit carry behaviour of Add."""
    c = []
    for k in (0, 4, 31, 32, 36, 63):
        c.append(("op2C A=2^%d B=0 mode0" % k, knos_alu(0x2C, 1 << k, 0, mode=0)))
    for k in (0, 4, 31, 32, 36, 63):
        c.append(("op2C A=0 B=2^%d mode0" % k, knos_alu(0x2C, 0, 1 << k, mode=0)))
    c.append(("op2C A=1111111122222222 B=0 mode0", knos_alu(0x2C, 0x1111111122222222, 0, mode=0)))
    c.append(("op2C A=0 B=3333333344444444 mode0", knos_alu(0x2C, 0, 0x3333333344444444, mode=0)))
    c.append(
        (
            "op2C A=1111111122222222 B=3333333344444444 mode0",
            knos_alu(0x2C, 0x1111111122222222, 0x3333333344444444, mode=0),
        )
    )
    c.append(
        (
            "op2C A=1111111122222222 B=3333333344444444 mode3",
            knos_alu(0x2C, 0x1111111122222222, 0x3333333344444444, mode=3),
        )
    )
    A = 0x0000100000001000
    for imm in (0, 4, -4, 22, -8, 2, 31, -31):
        c.append(("op2B imm %d src A" % imm, knos_alu(0x2B, A, 0x0000002000000020, ((imm & 0x3F) << 3) | 4, mode=0)))
    for imm in (0, 4, -4):
        c.append(("op2B imm %d src B" % imm, knos_alu(0x2B, A, 0x0000002000000020, ((imm & 0x3F) << 3) | 5, mode=0)))
    for a, b in (
        (0x0000000080000000, 0),
        (0x00000000FFFFFFFF, 1),
        (0x0000000080000000, 0x0000000080000000),
        (0x7FFFFFFF00000000, 0x0000000100000000),
        (0x8000000000000000, 0),
        (0xFFFFFFFFFFFFFFFF, 1),
    ):
        c.append(("add %016x + %016x mode0" % (a, b), knos_alu(0x0B, a, b, mode=0)))
        c.append(("addS %016x + %016x mode1" % (a, b), knos_alu(0x0F, a, b, mode=1)))
    return "raw", c


def trace_case(C, LC, T, J, cond=0xF380, region=(6, 44), n=50, commit=0x8800):
    """Execution trace: every instruction p in `region` loads (unit 1, stride 1, bank 8/9), adds p to lane 0
    (op 0x25 from source 5) and stores (unit 0, bank 6/7). Stored word k: lane 1 = 0x10000A00 + load sequence
    number, lane 0 = 0x10000800 + sequence + p: the order in which the sequencer issued the instructions.
    Commit of LC at C (immediate at C - 1), jump word at J to T."""
    s = {0: {"u0": 0xF00F, "u1": 0xF00F}, 1: {"u1": 0xCC01}, 2: {"u1": 0xD201}}
    if C:
        s.setdefault(C - 1, {})["seq"] = imm(0, LC)
        s.setdefault(C, {})["seq"] = commit
    for p in range(region[0], region[1]):
        s.setdefault(p, {})["u1"] = 0x7800
        s.setdefault(p + 1, {})["alu"] = (0x25 << 10) | ((p & 0x7F) << 3) | 5
        s.setdefault(p + 3, {})["u0"] = 0x60FB
    if J:
        s.setdefault(J, {})["seq"] = cond | T
    s.setdefault(n - 2, {})["seq"] = 0x0038
    reads = [0x1800 + 4 * i for i in range(128)] + [0x1C00 + 4 * i for i in range(128)]
    return case(*prog(n, s), reads=reads)


def trace_path(r):
    """Decode a trace_case result: [(position, load sequence)] per stored word."""
    out = []
    for k in range(128):
        lo, hi = r["reads"][k], r["reads"][128 + k]
        if hi == FILL + 0x600 + 0x100 + k and lo == FILL + 0x600 + k:  # untouched fill (bank 6/7 word k)
            break
        seq = hi - (FILL + 0x900)  # bank 9 word `seq`
        out.append(((lo - (FILL + 0x800) - seq) & 0xFFFFFFFF, seq))  # bank 8 word `seq` + position
    return out


def set_trace():
    c = [
        ("C4 T24 J31 LC=3", trace_case(4, 3, 24, 31)),
        ("C4 T18 J31 LC=3", trace_case(4, 3, 18, 31)),
        ("C4 T20 J31 LC=3", trace_case(4, 3, 20, 31)),
        ("C4 T16 J31 LC=3", trace_case(4, 3, 16, 31)),
        ("C4 T12 J31 LC=3", trace_case(4, 3, 12, 31)),
        ("C4 T24 J43 LC=3", trace_case(4, 3, 24, 43)),
        ("C9 T12 J27 LC=3", trace_case(9, 3, 12, 27)),
        ("C4 T12 J27 LC=3", trace_case(4, 3, 12, 27)),
        ("C20 T24 J31 LC=3 (hangs?)", trace_case(20, 3, 24, 31)),
        ("C4 T24 J31 LC=3 F340", trace_case(4, 3, 24, 31, cond=0xF340)),
        ("C4 T24 J31 LC=3 F280", trace_case(4, 3, 24, 31, cond=0xF280)),
        ("C4 T24 J31 LC=3 F300", trace_case(4, 3, 24, 31, cond=0xF300)),
        ("no jump", trace_case(4, 3, 24, 0)),
        ("C4 T24 J31 LC=0", trace_case(4, 0, 24, 31)),
        ("C4 T28 J31 LC=3", trace_case(4, 3, 28, 31)),
        ("C4 T5 J24 LC=3", trace_case(4, 3, 5, 24)),
    ]
    return "trace", c


def set_seqreg2():
    """Reproduce the 0x44332211 (set bytes) reading of source 0 from the seqwords run: the same kernels, then the
    read distance and the op (Not vs 0x2B move) varied."""
    S6 = (0x11, 0x22, 0x33, 0x44)

    def commit(at, vals, setn):
        return {at + i: (imm(i, v), 0) for i, v in enumerate(vals)} | {at + len(vals): (0x8800 | setn, 0)}

    c = [
        ("seqwords: a000/1541 src012", seqword_kernel(0xA000, 0x1541)),
        ("seqwords: a000/0000 src012", seqword_kernel(0xA000, 0x0000)),
        ("seqwords: d58c/0033 2B-move src0", seqword_kernel(0xD58C, 0x0033, srcs=(0,), alu=(0x2B << 10))),
        ("seqwords: a000/0000 2B-move src0", seqword_kernel(0xA000, 0x0000, srcs=(0,), alu=(0x2B << 10))),
        ("seqwords: a000/0000 2B-move src1", seqword_kernel(0xA000, 0x0000, srcs=(1,), alu=(0x2B << 10) | 1)),
        ("seqwords: a000/0000 2B-move src2", seqword_kernel(0xA000, 0x0000, srcs=(2,), alu=(0x2B << 10) | 2)),
        ("no commit: 2B-move src0", seqword_kernel(0xA000, 0x0000, srcs=(0,), alu=(0x2B << 10), setvals=())),
        ("no commit: Not src0", seqword_kernel(0xA000, 0x0000, srcs=(0,), setvals=())),
    ]
    for d in (1, 2, 3, 4, 6, 8):
        c.append(("set6, 2B-move src0 at +%d" % d, reg_kernel(commit(1, S6, 6), [5 + d]) | {}))
        c[-1][1]["mem1"][2 * (5 + d) + 1] = (0x2B << 10) << 16  # the ALU slot: 0x2B move from source 0
    c.append(("set6 then a000/1541 +2 Not src0", reg_kernel(commit(1, S6, 6) | {8: (0xA000, 0x1541)}, [10])))
    c.append(("set6 then a000/1541 +4 Not src0", reg_kernel(commit(1, S6, 6) | {8: (0xA000, 0x1541)}, [12])))
    c.append(("set 6 = 55 66 77 88, 2B-move src0 +2", reg_kernel(commit(1, (0x55, 0x66, 0x77, 0x88), 6), [7])))
    c[-1][1]["mem1"][2 * 7 + 1] = (0x2B << 10) << 16
    c.append(("set 3 = 55 66 77 88, 2B-move src0 +2", reg_kernel(commit(1, (0x55, 0x66, 0x77, 0x88), 3), [7])))
    c[-1][1]["mem1"][2 * 7 + 1] = (0x2B << 10) << 16
    c.append(
        (
            "staged 55 66 77 88 no commit, 2B-move src0 +2",
            reg_kernel({1: (imm(0, 0x55), 0), 2: (imm(1, 0x66), 0), 3: (imm(2, 0x77), 0), 4: (imm(3, 0x88), 0)}, [7]),
        )
    )
    c[-1][1]["mem1"][2 * 7 + 1] = (0x2B << 10) << 16
    return "copy", c


def set_macacc2():
    """MAC chains: store delay, chain spacing, and whether the output queue holds several entries."""
    c = [("configure b0", mac_config(0x2C00))]
    for delay in (4, 8, 16, 40):
        c.append(("2^40 x 4, store +%d" % delay, mac_chain(4, 1 << 30, 1 << 10, delay=delay)))
    for gap in (2, 4, 8):
        c.append(("2^40 x 4, gap %d, store +8" % gap, mac_chain(4, 1 << 30, 1 << 10, delay=8, gap=gap)))
    c.append(("2^40 x 4, 3 stores +4", mac_chain(4, 1 << 30, 1 << 10, delay=4, stores=3)))
    c.append(("2^40 x 1, 3 stores +4", mac_chain(1, 1 << 30, 1 << 10, delay=4, stores=3)))
    c.append(("2^40 x 4 gap 8, 3 stores +16", mac_chain(4, 1 << 30, 1 << 10, delay=16, stores=3, gap=8)))
    c.append(("2^40 x 1 again", mac_chain(1, 1 << 30, 1 << 10)))
    return "raw", c


# ---- third pass (2026-10-23, under our OS): register file, predication, counters, unit 2, MAC timing ----------
# Results: re/netmodule/sol-probes/hw-2026-10-23/. The kernels below set every state they rely on (registers by
# commits, counters by immediates, unit-2 registers before an 0xF00F); flags, counters, registers and unit
# registers persist across runs.


def commit(n, value, at):
    """Sequencer words from instruction `at` (5 instructions): counters 0-3 := the bytes of `value` (immediates
    idx 0-3), then 0x8800 | n: register n := counters 3..0."""
    return {at + k: {"seq": imm(k, (value >> 8 * k) & 0xFF)} for k in range(4)} | {at + 4: {"seq": 0x8800 | n}}


def rv(n):
    """A distinct 32-bit value for register n: bytes n1 n2 n3 n4."""
    return 0x04030201 + n * 0x10101010


def regs_prog(regs):
    """Commits of {n: value} from instruction 1; returns (slots, first free instruction)."""
    s = {}
    for i, (n, v) in enumerate(sorted(regs.items())):
        s |= commit(n, v, 1 + 5 * i)
    return s, 1 + 5 * len(regs) + 2


def all_commits(at=1, values=None):
    s = {}
    for n in range(8):
        s |= commit(n, (values or rv)(n), at + 5 * n)
    return s


def mv_src(k):
    """0x2B with immediate 0: a move of source k (both lanes)."""
    return (0x2B << 10) | (k & 7)


def kern(slots, reads, n=None, fill_range=(0x1800, 0x2800), data=None, regs=(), fill=FILL):
    """slots: {instr: {slot: word}} (merged); reads: [(instr, alu, third)]: the ALU op with that third field at
    instr and a store (unit 0, 0x60FB) two instructions later, in order: store k lands in banks 6/7 word k.
    Results: 'reads' = banks 6 and 7 words 0..len(reads)-1. A read placed on an instruction that has its own third
    field overrides it: pass the same third."""
    s = {0: {"u0": 0xF00F, "u1": 0xF00F}}
    for i, v in slots.items():
        s.setdefault(i, {}).update(v)
    last = max(slots) if slots else 0
    for i, alu, third in reads:
        s.setdefault(i, {}).update(alu=alu, third=third)
        s.setdefault(i + 2, {})["u0"] = 0x60FB
        last = max(last, i + 2)
    s.setdefault(last + 4, {})["seq"] = 0x0038
    k = len(reads)
    return case(
        *prog(n or last + 6, s),
        fill_range=fill_range,
        data=data,
        regs=regs,
        fill=fill,
        reads=[0x1800 + 4 * i for i in range(k)] + [0x1C00 + 4 * i for i in range(k)],
    )


def set_regs():
    """Registers r0-r7 committed with distinct values, then read through ALU source 0 with the third field's bits
    14:12 = k and through source 1 with bits 10:8 = k. A third field of 0 keeps the previous one."""
    s = all_commits()
    at = 44
    c = [
        (
            "commits, src0 sel k, src1 sel k",
            kern(
                s,
                [(at + k, not_src(0), k << 12) for k in range(8)]
                + [(at + 8 + k, not_src(1), k << 8) for k in range(8)],
            ),
        )
    ]
    c.append(
        (
            "commits, third 0: src0, src1, src2, src7",
            kern(s, [(at + k, not_src(j), 0) for k, j in enumerate((0, 1, 2, 7))]),
        )
    )
    c.append(("commits, src0 sel 8-15", kern(s, [(at + k, not_src(0), (8 + k) << 12) for k in range(8)])))
    c.append(("commits, src1 sel 8-15", kern(s, [(at + k, not_src(1), (8 + k) << 8) for k in range(8)])))
    c.append(("commits, Or src0 sel 3 | src1 sel 5", kern(s, [(at, (0x02 << 10) | (1 << 3) | 0, 0x3500)])))
    c.append(("no commits: src0 sel k (third 0 first)", kern({}, [(4 + k, not_src(0), k << 12) for k in range(8)])))
    return "regs", c


def set_regwr():
    """Third-field bits 7:4 = destination register, 3:0 = the source written (lane 0): loads (4, 5), the ALU
    result (3), read back through source 0. Banks 6-9 hold 0x10000000 + word index."""
    s = all_commits()
    at = 44
    c = [
        (
            "r3 <- src5 (u1 load)",
            kern(
                s | {at: {"u1": 0x7800}, at + 1: {"third": 0x0035}},
                [(at + 3, not_src(0), 0x3000), (at + 4, not_src(0), 0x4000)],
            ),
        ),
        (
            "r4 <- src4 (u0 load)",
            kern(
                s | {at: {"u0": 0x7800}, at + 1: {"third": 0x0044}},
                [(at + 3, not_src(0), 0x4000), (at + 4, not_src(0), 0x3000)],
            ),
        ),
    ]
    for d in (1, 2, 3):
        c.append(
            (
                "r6 <- src3 at +%d after Not r1" % d,
                kern(s | {at + d: {"third": 0x0063}}, [(at, not_src(0), 0x1000), (at + 6, not_src(0), 0x6000)]),
            )
        )
    c.append(
        (
            "third 0x0053 after u1 load",
            kern(
                s | {at: {"u1": 0x7800}, at + 1: {"third": 0x0053}},
                [(at + 3, not_src(0), 0x5000), (at + 4, not_src(0), 0x3000)],
            ),
        )
    )
    c.append(
        (
            "r2 <- src5 and read r2 same instr",
            kern(s | {at: {"u1": 0x7800}}, [(at + 1, not_src(0), 0x2025), (at + 3, not_src(0), 0x2000)]),
        )
    )
    c.append(
        (
            "r0 <- src5, then src0/src1",
            kern(
                s | {at: {"u1": 0x7800}, at + 1: {"third": 0x0005}}, [(at + 3, not_src(0), 0), (at + 4, not_src(1), 0)]
            ),
        )
    )
    return "regs", c


def set_srcs():
    """The other ALU sources: 2 = the MAC output, 6 = unit 2's loads (after unit 2 stored), 7; a load's data is
    there in the next instruction only."""
    c = [("configure b0", mac_config(0x2C00))]
    data = {0x1800: 0x00030000, 0x1C00: 0x00050000, 0x2000: 0x00000100, 0x2400: 0x00000200}
    for d in (2, 4, 8, 16):
        c.append(
            (
                "mult, Not src2 at +%d" % d,
                kern(
                    {1: {"u0": 0x7800, "u1": 0x7800}, 2: {"mac": MULT_B0}},
                    [(2 + d, not_src(2), 0)],
                    fill_range=(0x1800, 0x1C00),
                    data=data,
                    fill=0,
                ),
            )
        )
    s = all_commits()
    at = 44
    for ld in (1, 4):
        c.append(
            (
                "u2 store Not r1, u2 load +%d, Not src6" % ld,
                kern(
                    s | {at + 2: {"u2": 0x60FB}, at + 2 + ld: {"u2": 0x7800}},
                    [(at, not_src(0), 0x1000), (at + 3 + ld, not_src(6), 0)],
                    fill_range=(0, 0x2800),
                ),
            )
        )
    c.append(("Not r1 then Not src7", kern(s, [(at, not_src(0), 0x1000), (at + 2, not_src(7), 0)])))
    c.append(
        (
            "u1 load at 1, Not src5 at 2 and at 3",
            kern({1: {"u1": 0x7800}}, [(2, not_src(5), 0x0001), (3, not_src(5), 0x0001)]),
        )
    )
    return "regs", c


def set_src7():
    """Source 7 against source 3: the same timing (the result of the op two instructions before), another value."""
    s = all_commits()
    at = 44
    c = []
    for src in (3, 7):
        for d in (1, 2):
            c.append(
                (
                    "moves r1-r4, src%d %d after" % (src, d),
                    kern(
                        s, [(at + i, mv_src(0), (1 + i) << 12) for i in range(4)] + [(at + 3 + d, mv_src(src), 0x0001)]
                    ),
                )
            )
    return "raw2", c


def set_lanes():
    """The SIMD ops' "dst" field (bits 9:6) routes operand A per lane: SimdAdd / SimdSub of two loads with
    distinct lanes (A = 0x00300000 / 0x00050000, B = 0x200 / 0x7) for dst 0-15. 64-bit ops ignore the field."""
    data = {0x1800: 0x00050000, 0x1C00: 0x00300000, 0x2000: 0x00000007, 0x2400: 0x00000200}
    c = []
    for op in (0x18, 0x19):
        for dst in range(16):
            c.append(
                (
                    "op%02x dst%d" % (op, dst),
                    kern(
                        {1: {"u0": 0x7800, "u1": 0x7800}},
                        [(2, (op << 10) | (dst << 6) | (5 << 3) | 4, 0)],
                        data=data,
                        fill=0,
                    ),
                )
            )
    for dst in (0, 4, 0xC, 1):
        c.append(
            (
                "op0b (Add) dst%d" % dst,
                kern(
                    {1: {"u0": 0x7800, "u1": 0x7800}},
                    [(2, (0x0B << 10) | (dst << 6) | (5 << 3) | 4, 0)],
                    data=data,
                    fill=0,
                ),
            )
        )
    return "raw2", c


COND_REGS = {1: 5, 2: 3, 3: 5, 4: 0xFFFFFFFD, 5: 0x55555555, 6: 0x66666666, 7: 0x00007777}
COND_WORDS = (0xA000, 0xD58C, 0xD5AD, 0xDD8D, 0x998F, 0x99EC, 0x9D8F, 0x9401, 0x9402)
SIMDSUB_AB = (0x19 << 10) | (4 << 6) | (1 << 3) | 0  # SimdSub, lane k: source 0 (port A) - source 1 (port B)


def cond_case(word, a, b, new, dist=3, regs=COND_REGS, dst=5):
    """Registers from `regs`; SimdSub r_a - r_b at F (stored: read 0); at F + dist the sequencer `word` with the
    write r_dst <- r_new (third: port A = new, dst <- source 0); r_dst read at F + dist + 3 (read 1)."""
    s, F = regs_prog(regs)
    s.setdefault(F + dist, {}).update(seq=word, third=(new << 12) | (dst << 4))
    return kern(s, [(F, SIMDSUB_AB, (a << 12) | (b << 8)), (F + dist + 3, mv_src(0), dst << 12)])


def set_cond():
    """Which sequencer words predicate a register write, on what: a SimdSub r_a - r_b (positive 5-3, zero 5-5,
    negative 3-5) three instructions before, the write r5 <- r6 (new > old) or r5 <- r1 (new < old)."""
    c = []
    for w in COND_WORDS:
        for a, b, name in ((1, 2, "+"), (1, 3, "0"), (2, 1, "-")):
            for new in (6, 1):
                c.append(("%04x cmp%s r5<-r%d" % (w, name, new), cond_case(w, a, b, new)))
    return "raw2", c


def set_cond2():
    """Flag latency (the predicated write at F + d; at d < 3 the word sees the flags from before: the previous
    case's, which alternate) and ALU predication (a false word also suppresses the ALU op and any store in its
    instruction)."""
    c = []
    for w in (0xD58C, 0x998F):
        for d in (1, 2, 4, 6):
            for a, b, name in ((1, 2, "+"), (2, 1, "-")):
                c.append(("%04x cmp%s write at +%d" % (w, name, d), cond_case(w, a, b, 6, dist=d)))
    for w in (0xA000, 0xD58C, 0xD5AD, 0xDD8D, 0x998F, 0x99EC, 0x9D8F):
        for a, b, name in ((1, 2, "+"), (1, 3, "0"), (2, 1, "-")):
            s, F = regs_prog(COND_REGS)
            s.setdefault(F + 3, {})["seq"] = w
            c.append(
                (
                    "%04x cmp%s: move r1, move r6 [w]" % (w, name),
                    kern(
                        s,
                        [(F, SIMDSUB_AB, (a << 12) | (b << 8)), (F + 2, mv_src(0), 0x1000), (F + 3, mv_src(0), 0x6000)],
                    ),
                )
            )
    return "raw2", c


LANE_SIGNS = [(s1, s0) for s1 in (1, 0, -1) for s0 in (1, 0, -1)]  # (lane 1, lane 0) of A - B


def condmulti_case(word, op=None):
    """All nine lane-sign cases (LANE_SIGNS order) in one program: per case k, unit 1 loads A (banks 8/9 word k:
    lane j = 10 + 2 s_j), SimdSub A - r2 (r2 = 10), move r1 (plain), move r6 under `word` three instructions after
    the SimdSub, store (banks 6/7 word k = 0x66666666 iff the move executed, 5 if not)."""
    s, at = regs_prog({1: 5, 2: 10, 6: 0x66666666})
    s.setdefault(at, {})["u1"] = 0xCC01  # unit 1 load stride 1 (both lanes)
    s.setdefault(at + 1, {})["u1"] = 0xD201
    F = at + 4
    data = {}
    sub = op if op is not None else (0x19 << 10) | (4 << 6) | (1 << 3) | 5  # SimdSub lane k: load.k - r2
    for k, (s1, s0) in enumerate(LANE_SIGNS):
        data[0x2000 + 4 * k], data[0x2400 + 4 * k] = 10 + 2 * s0, 10 + 2 * s1
        s.setdefault(F - 1, {})["u1"] = 0x7800
        s.setdefault(F, {}).update(alu=sub, third=0x0200)
        s.setdefault(F + 2, {}).update(alu=mv_src(0), third=0x1000)
        s.setdefault(F + 3, {}).update(alu=mv_src(0), third=0x6000, seq=word)
        s.setdefault(F + 5, {})["u0"] = 0x60FB
        F += 6
    s.setdefault(F + 2, {})["seq"] = 0x0038
    return case(*prog(F + 4, s), fill_range=(0x1800, 0x2800), data=data, reads=[0x1800 + 4 * k for k in range(9)])


def cond_row(r):
    return "".join("X" if v == 0x66666666 else "." if v == 5 else "?" for v in r["reads"])


def set_condbits():
    """Truth tables (lane 1, lane 0 signs: ++ +0 +- 0+ 00 0- -+ -0 --) for the vendor words and every
    single-bit change of the 12 low bits of 0x998F and 0xD58C (bit 13 of 0xD58C makes a jump: not tried)."""
    c = [("%04x" % w, condmulti_case(w)) for w in (0xA000, 0xD58C, 0xD5AD, 0xDD8D, 0x998F, 0x99EC, 0x9D8F, 0x9401)]
    for base in (0x998F, 0xD58C):
        c += [("%04x" % (base ^ (1 << bit)), condmulti_case(base ^ (1 << bit))) for bit in range(12)]
    return "condrow", c


def set_condsweep():
    """Truth tables for 0x99xx and 0xD5xx, every low byte, and 0x9x8F / 0xDx8C for every bits 11:8."""
    c = [("%04x" % w, condmulti_case(w)) for w in [0x9900 | b for b in range(256)] + [0xD500 | b for b in range(256)]]
    c += [
        ("%04x" % w, condmulti_case(w))
        for w in [0x908F | n << 8 for n in range(16)] + [0xD08C | n << 8 for n in range(16)]
    ]
    return "condrow", c


def set_condcombine():
    """Bits 15:10 with term A = lane 0 > 0 and term B = lane 1 < 0 (low bits 0x18F): the combine function.
    0x8xxx / 0xCxxx (bit 12 clear) are other instructions: some restart the program or never finish."""
    return "condrow", [
        ("%04x" % w, condmulti_case(w)) for w in [(t << 12) | (m << 10) | 0x18F for t in (0x9, 0xD) for m in range(4)]
    ]


def set_flagops():
    """Which ops set the flags: SimdSub makes them negative, then op X with a positive result, then D58C (lane 0
    > 0) / D5AD (lane 0 < 0) three instructions after X. A Mult with a positive output and term-B sources 0-7."""
    regs = {1: 5, 2: 3, 3: 0xFFFFFFF0, 6: 0x66666666}

    def k(setter, word, third):
        s, F = regs_prog(regs)
        s.setdefault(F, {})["mac"] = 0xB0 << 6
        s.setdefault(F + 1, {}).update(alu=SIMDSUB_AB, third=0x2100)  # r2 - r1 < 0
        G = F + 5
        s.setdefault(G, {}).update(setter)
        s[G]["third"] = third
        s.setdefault(G + 3, {})["seq"] = word
        c = kern(s, [(G + 2, mv_src(0), 0x1000), (G + 3, mv_src(0), 0x6000)])
        c["kind"] = "pred"
        return c

    ops = {
        "Add r1+r2 (64)": ((0x0B << 10) | (1 << 3), 0x1200),
        "Sub r1-r2 (64)": ((0x0C << 10) | (1 << 3), 0x1200),
        "SimdAdd r1+r2": ((0x18 << 10) | (4 << 6) | (1 << 3), 0x1200),
        "Or r1|r2": ((0x02 << 10) | (1 << 3), 0x1200),
        "Not r3": (0x01 << 10, 0x3000),
        "move r1 (2B)": (0x2B << 10, 0x1200),
        "Comp r1-r2": ((0x13 << 10) | (1 << 3), 0x1200),
        "SimdComp r1-r2": ((0x20 << 10) | (4 << 6) | (1 << 3), 0x1200),
    }
    c = []
    for name, (op, third) in ops.items():
        c += [
            ("%s, then D58C" % name, k({"alu": op}, 0xD58C, third)),
            ("%s, then D5AD" % name, k({"alu": op}, 0xD5AD, third)),
        ]
    mult = {"mac": (2 << 14) | (0xA0 << 6) | (1 << 3)}
    for src in range(8):
        c += [
            ("Mult (pos), term B source %d, lane0 > 0" % src, k(mult, 0xD500 | (src << 2), 0x1200)),
            ("Mult (pos), term B source %d, lane0 < 0" % src, k(mult, 0xD500 | (src << 2) | 1, 0x1200)),
        ]
    return "pred", c


# ---- sequencer: counters and jumps ----


def jump_word(J, S, k):
    """Counted jump at J back to S on counter k: 0xF2xx/0xF3xx, displacement d = S - J + 63 in bits 8:3."""
    d = S - J + 63
    assert 0 <= d < 64
    return 0xF200 | (d << 3) | k


def counted(pre=(), word=None, extra=None, k=0):
    """store_loop (one store per pass, body 40-47) with the jump replaced by `word` (default: counter k), `pre`
    sequencer words from instruction 3, `extra` slots. Readout: changed words = passes."""
    s = dict(extra or {})
    for i, v in enumerate(pre):
        s.setdefault(3 + i, {})["seq"] = v
    c = store_loop(body=40, length=8, extra=s)
    m1 = list(c["mem1"])
    m1[2 * 47] = (m1[2 * 47] & 0xFFFF0000) | (word if word is not None else jump_word(47, 40, k))
    c["mem1"] = m1
    return c


def set_counters():
    """Eight 8-bit loop counters: counter k is the one a jump with bits 2:0 = k counts (0 = 256 passes). Immediates
    idx 0-7 write counter idx; a commit packs counters 3..0 into a register; 0xA00k: counter k := counter 0;
    0x940N: counters 3..0 := rN; 0x900N: counters 1..0 := rN[15:0]."""
    c = [("counter %d := 7 (imm idx %d), jump on counter %d" % (k, k, k), counted((imm(k, 7),), k=k)) for k in range(8)]
    c += [("counter 4 := 7, jump on counter 0", counted((imm(0, 9), imm(4, 7)), k=0))]
    stage = (imm(0, 2), imm(1, 3), imm(2, 4), imm(3, 6), 0x8800)
    c += [("counters 0-3 := 2 3 4 6 + commit, jump on %d" % k, counted(stage, k=k)) for k in range(4)]
    c += [
        ("counter 0 := 21, a004, jump on 4", counted((imm(4, 9), imm(0, 21), 0xA004), k=4)),
        ("counter 0 := 21, a001, jump on 1", counted((imm(1, 9), imm(0, 21), 0xA001), k=1)),
        ("counter 0 := 21, a000, jump on 1", counted((imm(1, 9), imm(0, 21), 0xA000), k=1)),
    ]
    regs = commit(1, 0x14131211, 20) | commit(3, 0x34333231, 25)
    for wd in (0x9401, 0x9403, 0x9001):
        for k in (0, 1, 2):
            c.append(
                (
                    "r1 = 14131211, r3 = 34333231; counters 3 4 5; %04x; jump on %d" % (wd, k),
                    counted(
                        extra=regs
                        | {30: {"seq": imm(0, 3)}, 31: {"seq": imm(1, 4)}, 32: {"seq": imm(2, 5)}, 34: {"seq": wd}},
                        k=k,
                    ),
                )
            )
    return "count", c


def set_counters2():
    """Counters 3..0 read exactly: stage bytes, a word, a commit to r5, r5 read back. r1-r4 hold rv(n)."""
    c = []
    for wd in (0xA000, 0x9401, 0x9402, 0x9404, 0x9408, 0x9001, 0x9002, 0x9801):
        for b in ((0x03, 0x04, 0x05, 0x06), (0xFF, 0x80, 0x7F, 0x01)):
            s = all_commits(values=lambda n: rv(n) if n else 0)
            at = 41
            for k in range(4):
                s[at + k] = {"seq": imm(k, b[k])}
            s[at + 5] = {"seq": wd}
            s[at + 7] = {"seq": 0x8805}
            c.append(("%04x after %02x %02x %02x %02x" % (wd, *b), kern(s, [(at + 11, mv_src(0), 0x5000)])))
    for wd in (0x8801, 0x8C01, 0x8001, 0x8401, 0x8885):
        s = all_commits()
        at = 41
        for k, b in enumerate((0x5A, 0x5B, 0x5C, 0x5D)):
            s[at + k] = {"seq": imm(k, b)}
        s[at + 5] = {"seq": wd}
        c.append(("%04x: r1..r7" % wd, kern(s, [(at + 8 + k, mv_src(0), k << 12) for k in range(1, 8)])))
    return "raw2", c


def trace_long(words, region=(3, 122), n=127):
    """trace_case over a 127-instruction program with sequencer words placed at {instr: word}."""
    c = trace_case(0, 0, 0, 0, region=region, n=n)
    m1 = list(c["mem1"])
    for i, wd in words.items():
        m1[2 * i] = (m1[2 * i] & 0xFFFF0000) | wd
    c["mem1"] = m1
    return c


def set_jumps():
    """Jump displacement: a jump at 60 on counter 4 (= 2) with bits 8:3 = d returns to 60 - 63 + d: 0xF2xx are the
    long backward jumps, not a condition. Plus 0xF3xx, d = 53 (target 50), and counter 4 = 1 (not taken)."""
    c = [("F2 J60 d=%d ctr4=2" % d, trace_long({1: imm(4, 2), 60: 0xF200 | d << 3 | 4})) for d in (8, 16, 24, 31)]
    c += [
        ("F3 J60 d=53 ctr4=2", trace_long({1: imm(4, 2), 60: 0xF200 | 53 << 3 | 4})),
        ("F2 J60 d=16 ctr4=1", trace_long({1: imm(4, 1), 60: 0xF200 | 16 << 3 | 4})),
        ("F3 J60 d=53 ctr4=3", trace_long({1: imm(4, 3), 60: 0xF200 | 53 << 3 | 4})),
    ]
    return "trace", c


def set_imm():
    """Immediates idx 8-31 (values 0, 1, 0x20): six moves stored after each; 0x0038 (idx 24, value 1) ends the
    program, 0x0037 (idx 23, value 1) never finishes."""
    c = []
    for k in range(8, 32):
        for v in (0, 1, 0x20):
            c.append(
                (
                    "imm idx %d value %#x" % (k, v),
                    kern({2: {"seq": (v << 5) | k}}, [(4 + i, mv_src(0), (1 + i) << 12) for i in range(6)]),
                )
            )
    return "raw2", c


# ---- address unit 2 ----

AU2_CFG = (0xC800, 0xCC01, 0xCB00, 0xCE00, 0xD201, 0xD100)  # stride 1, modulo 0x100, start words 0


def au2_case(K, R, cfg=AU2_CFG, n_read=None):
    """Unit 2's memory, configured as the vendors do (registers, then 0xF00F): loop 1 (counter 4 = K) copies banks
    8/9 word k (0x10000800 + k / 0x10000900 + k) into unit 2 (move src5, unit 2 store); 0xF00F; loop 2 (counter 5
    = R) loads unit 2 and stores what it read to banks 6/7 (move src6, unit 0 store)."""
    s = {
        0: {"u0": 0xF00F, "u1": 0xF00F},
        1: {"u1": 0xCC01},
        2: {"u1": 0xD201},
        3: {"seq": imm(4, K & 0xFF)},
        4: {"seq": imm(5, R & 0xFF)},
    }
    for i, v in enumerate(cfg):
        s.setdefault(1 + i, {})["u2"] = v
    s.setdefault(1 + len(cfg), {})["u2"] = 0xF00F
    S1 = 14
    s[S1] = {"u1": 0x7800}
    s[S1 + 1] = {"alu": mv_src(5), "third": 0x0001}
    s[S1 + 3] = {"u2": 0x60FB}
    s[S1 + 4] = {"seq": jump_word(S1 + 4, S1, 4)}
    s[S1 + 9] = {"u2": 0xF00F}
    S2 = S1 + 14
    s[S2] = {"u2": 0x7800}
    s[S2 + 1] = {"alu": mv_src(6)}
    s[S2 + 3] = {"u0": 0x60FB}
    s[S2 + 4] = {"seq": jump_word(S2 + 4, S2, 5)}
    s[S2 + 10] = {"seq": 0x0038}
    n = n_read or (R or 256)
    return case(
        *prog(S2 + 12, s),
        fill_range=(0x1800, 0x2800),
        reads=[0x1800 + 4 * k for k in range(n)] + [0x1C00 + 4 * k for k in range(n)],
    )


def set_au2():
    """Unit 2: 70 words round trip, modulo 3, and 256 stores (the memory is 128 words per lane: reads wrap)."""
    c = [
        ("70 stores, 70 loads, modulo 256", au2_case(70, 70)),
        ("5 stores, 9 loads, modulo 3", au2_case(5, 9, cfg=(0xC800, 0xCC01, 0xCA03, 0xCE00, 0xD201, 0xD003))),
        ("256 stores, 256 loads", au2_case(0, 0)),
    ]
    c[-1][1]["reads"] += [0x2800 + 4 * k for k in range(0, 0x1800 // 4, 64)]
    return "au2", c


def au2_summary(r):
    n = len(r["reads"]) // 2 if len(r["reads"]) <= 512 else 256

    def runs(vals, base):
        out = []
        for v in vals:
            i = v - base if 0 <= v - base < 0x100 else None
            if out and i is not None and out[-1][1] is not None and i == out[-1][1] + 1:
                out[-1][1] = i
            else:
                out.append([i, i])
        return " ".join("?" if a is None else ("%d-%d" % (a, b) if a != b else "%d" % a) for a, b in out)

    s = "lane0: %s | lane1: %s" % (runs(r["reads"][:n], 0x10000800), runs(r["reads"][n : 2 * n], 0x10000900))
    if len(r["reads"]) > 2 * n:
        s += " | Warp 0x2800-0x3FFF: %s" % " ".join("%x" % v for v in sorted(set(r["reads"][2 * n :])))
    return s


# ---- MAC timing and configuration ----

MAC_REGS = {1: 1, 2: 0x10, 3: 0x100, 4: 0x1000, 5: 0x10000}


def mac_seq(ops, cfg=0xB0, upto=10, regs=MAC_REGS):
    """MAC ops [(op, k, macmode) or None] one per instruction from t0 (r_k lanes x r1 shorts: opf 0x08, port A
    = r1, port B = r_k), after a configuration word; source 2 read (lane 1) at t0 + 1 .. t0 + upto - 1."""
    s, F = regs_prog(regs)
    s.setdefault(F, {})["mac"] = cfg << 6
    t0 = F + 4
    th = {}
    for i, o in enumerate(ops):
        if o is None:
            continue
        op, k, mm = o
        th[t0 + i] = (1 << 12) | (k << 8)
        s.setdefault(t0 + i, {}).update(mac=(op << 14) | (mm << 6) | 0x08, third=th[t0 + i])
    return kern(s, [(t0 + d, mv_src(2), th.get(t0 + d, 0)) for d in range(1, upto)])


def set_macpipe():
    """MAC latency and the output shift: chains read through source 2 every instruction (lane 1 = accumulator
    A), shift codes varied per op; the direct store 0x60F2; 2^44 accumulated 16 times then -2^44 (>> 16)."""
    c = [
        ("InitMac r2", mac_seq([(3, 2, 0xA0)])),
        ("InitMac r2, Mac r3, Mac r4", mac_seq([(3, 2, 0xA0), (1, 3, 0xA0), (1, 4, 0xA0)])),
        ("same, 2 apart", mac_seq([(3, 2, 0xA0), None, (1, 3, 0xA0), None, (1, 4, 0xA0)])),
        ("Mult r2, r3, r4", mac_seq([(2, 2, 0xA0), (2, 3, 0xA0), (2, 4, 0xA0)])),
        (
            "Mult a3 (>>16), -, Mult a0",
            mac_seq([(2, 2, 0xA3), None, (2, 3, 0xA0)], regs={1: 1, 2: 0x10000, 3: 0x100000}),
        ),
        (
            "Mult a0, Mult a3, Mult a0",
            mac_seq([(2, 2, 0xA0), (2, 3, 0xA3), (2, 4, 0xA0)], regs={1: 1, 2: 0x10000, 3: 0x100000, 4: 0x1000000}),
        ),
        ("configure b3, Mult a0", mac_seq([(2, 2, 0xA0)], cfg=0xB3, regs={1: 1, 2: 0x10000})),
    ]
    # 0x60F2 stores at +1..+8 after InitMac r2, Mac r3, Mac r4
    s, F = regs_prog(MAC_REGS)
    s.setdefault(F, {})["mac"] = 0xB0 << 6
    t0 = F + 4
    for i, k in enumerate((2, 3, 4)):
        s.setdefault(t0 + i, {}).update(
            mac=((3 if i == 0 else 1) << 14) | (0xA0 << 6) | 0x08, third=(1 << 12) | (k << 8)
        )
    for d in range(1, 9):
        s.setdefault(t0 + d, {})["u0"] = 0x60F2
    s.setdefault(t0 + 14, {})["seq"] = 0x0038
    st = case(*prog(t0 + 16, s), reads=[0x1800 + 4 * k for k in range(8)] + [0x1C00 + 4 * k for k in range(8)])
    c.append(("60F2 stores at +1..+8", st))
    # accumulator wrap: shift 16 on every op (the first output uses the configuration's shift 0)
    s, F = regs_prog({1: 0x4000, 2: 0x40000000, 3: 0xC0000000})
    s.setdefault(F, {})["mac"] = 0xB3 << 6
    t0 = F + 4
    ops = [3] + [1] * 19
    reads = []
    for i, op in enumerate(ops):
        th = (1 << 12) | ((2 if i < 16 else 3) << 8)
        s.setdefault(t0 + i, {}).update(mac=(op << 14) | (0xA3 << 6) | 0x08, third=th)
    for i in range(len(ops)):
        t = t0 + i + 2
        reads.append((t, mv_src(2), s.get(t, {}).get("third", 0)))
    c.append(("+2^44 x16 then -2^44 x4, >>16", kern(s, reads)))
    return "raw2", c


def set_maccfg():
    """MAC configuration values 0x00-0xFC (bits 1:0 = 0) followed by a Mult whose dual and combined outputs
    differ (macmode 0xD8: A = B.lane1 x A.short1, B = B.lane1 x A.short2): dual = 0x20000.0x30000, combined =
    0x2.0x30000."""
    data = {0x1800: 0x00020001, 0x1C00: 0x00040003, 0x2000: 0x00000100, 0x2400: 0x00010000}
    return "raw2", [
        (
            "configure %02x" % cfg,
            kern(
                {2: {"mac": cfg << 6}, 5: {"u0": 0x7800, "u1": 0x7800}, 6: {"mac": (2 << 14) | (0xD8 << 6) | 0x2C}},
                [(8, mv_src(2), 0)],
                data=data,
                fill=0,
            ),
        )
        for cfg in range(0, 256, 4)
    ]


# ---- ALU: negative shifts, saturation ----


def alu_case(op_word, a, b, mode=2):
    """One ALU op on two loads (A = unit 0, B = unit 1, both lanes) after Modeset `mode` (operand bits 3:0)."""
    data = {0x1800: a & 0xFFFFFFFF, 0x1C00: a >> 32, 0x2000: b & 0xFFFFFFFF, 0x2400: b >> 32}
    return kern(
        {2: {"alu": (0x23 << 10) | (mode & 0xF)}, 5: {"u0": 0x7800, "u1": 0x7800}}, [(6, op_word, 0)], data=data, fill=0
    )


def set_alu4():
    """0x2B / 0x27 / 0x29 with negative counts (logical per-lane right shifts), and the add/sub family in modes
    0, 1 and 3 on overflowing operands (AddS / AddAS: mode bit 0 clear = unsigned saturation, set = signed)."""
    A = 0x8000100000001000
    c = [
        ("2B imm %d" % n, alu_case((0x2B << 10) | ((n & 0x7F) << 3) | 4, A, 0))
        for n in (-1, -6, -7, -8, -16, -31, -32, -64, 4)
    ]
    c += [
        ("%02x imm %d" % (op, n), alu_case((op << 10) | ((n & 0x7F) << 3) | 4, A, 0))
        for op in (0x27, 0x29)
        for n in (-8, 4)
    ]
    pairs = [
        (0x7FFFFFF07FFFFFF0, 0x0000002000000020),
        (0x8000001080000010, 0xFFFFFFE0FFFFFFE0),
        (0x4000000040000000, 0x4000000040000000),
        (0x00000000FFFFFFFF, 0x0000000000000001),
        (0x8000000000000000, 0x0000000000000001),
        (0x7FFFFFFFFFFFFFFF, 0xFFFFFFFFFFFFFFFF),
    ]
    for op in list(range(0x0B, 0x14)) + list(range(0x18, 0x20)):
        opw = (op << 10) | ((4 << 6) if op >= 0x18 else 0) | (5 << 3) | 4
        for a, b in pairs:
            for m in (0, 1, 3):
                c.append(("op%02x %016x %016x mode%d" % (op, a, b, m), alu_case(opw, a, b, m)))
    return "raw2", c


def set_clamp():
    """Candidates for the one-time lane-1 16-bit clamp (hw-2026-10-22/lane1-clamp.json): unit-0 register groups
    written before an 0xF00F (each restored to 0 after), access-word bits 0-8 and 10, Modeset operand bits 0-9; a
    unit-0 load of lane 1 = 0x00008000 must read back unchanged."""
    data = {0x1800: 0x00040003, 0x1C00: 0x00008000, 0x2000: 0x00050007, 0x2400: 0xFFFF8000}
    c = []
    for g in (
        0xC800,
        0xCE00,
        0xD400,
        0xD600,
        0xD800,
        0xDA00,
        0xDC00,
        0xDE00,
        0xE000,
        0xE200,
        0xE600,
        0xE800,
        0xEA00,
        0xEC00,
        0xEE00,
    ):
        for v in (1, 2, 4, 0x10, 0x80, 0x100):
            c.append(
                (
                    "u0 %04x before F00F, u0 load" % (g | v),
                    kern(
                        {1: {"u0": g | v}, 2: {"u0": 0xF00F}, 4: {"u0": 0x7800}},
                        [(5, mv_src(4), 0x0001)],
                        data=data,
                        fill=0,
                    ),
                )
            )
            c.append(
                ("  restore %04x" % g, kern({1: {"u0": g}, 2: {"u0": 0xF00F, "u1": 0xF00F}}, [(4, mv_src(0), 0x0001)]))
            )
    c += [
        (
            "u0 load %04x" % (0x7800 | 1 << b),
            kern({2: {"u0": 0x7800 | 1 << b}}, [(3, mv_src(4), 0x0001)], data=data, fill=0),
        )
        for b in list(range(9)) + [10]
    ]
    c += [
        (
            "Modeset %03x, move" % (1 << b),
            kern({2: {"alu": (0x23 << 10) | (1 << b)}, 5: {"u0": 0x7800}}, [(6, mv_src(4), 0x0001)], data=data, fill=0),
        )
        for b in range(10)
    ]
    c.append(("Modeset 2 (restore)", kern({2: {"alu": (0x23 << 10) | 2}}, [(4, mv_src(0), 0x0001)])))
    return "raw2", c


def set_jumpslots():
    """Which instructions around a taken jump execute each pass: a loop 20..27 on counter 4 = 3, one store placed
    at instruction P (no other stores): changed words = executions of P's store. J + 1 is a delay slot."""
    c = []
    for P in (19, 20, 26, 27, 28, 29, 30, 31):
        s = {
            0: {"u0": 0xF00F, "u1": 0xF00F},
            3: {"seq": imm(4, 3)},
            27: {"seq": jump_word(27, 20, 4)},
            38: {"seq": 0x0038},
        }
        s.setdefault(P, {})["u0"] = 0x60FB
        c.append(("store at %d (loop 20-27, 3 passes)" % P, case(*prog(40, s), fill_range=(0x1800, 0x1C00))))
    return "count", c


def set_macpred():
    """A predicate on a MAC op: InitMac r2 (0x10) unconditional, then InitMac r3 (0x100) under D58C (lane 0 > 0)
    or D5AD (lane 0 < 0) three instructions after a negative SimdSub; source 2 read after."""
    c = []
    for w_ in (0xA000, 0xD58C, 0xD5AD):
        s, F = regs_prog(MAC_REGS)
        s.setdefault(F, {})["mac"] = 0xB0 << 6
        s.setdefault(F + 1, {}).update(alu=SIMDSUB_AB, third=0x2300)  # 0x10 - 0x100 < 0
        s.setdefault(F + 2, {}).update(mac=(3 << 14) | (0xA0 << 6) | 0x08, third=0x1200)
        s.setdefault(F + 4, {}).update(mac=(3 << 14) | (0xA0 << 6) | 0x08, third=0x1300, seq=w_)
        c.append(("InitMac r3 under %04x" % w_, kern(s, [(F + 8, mv_src(2), 0x0001)])))
    return "raw2", c


def set_jumpslots2():
    """The same loop (20..27, counter 4 = 3) with a unit-1 load (stride 1) or a MAC Mac (+1) at P: the loads
    counted by the load pointer (a load + store after the loop reads bank 8 word = number of loads), the Macs by
    the accumulator (read through source 2 after the loop)."""
    c = []
    regs = {1: 1, 2: 1}
    for P in (26, 27, 28, 29):
        s, F = regs_prog(regs)  # commits at 1-10
        s = {k + 0: v for k, v in s.items()}
        s.setdefault(0, {}).update(u0=0xF00F, u1=0xF00F)
        s.setdefault(11, {})["u1"] = 0xCC01
        s.setdefault(12, {}).update(seq=imm(4, 3), mac=0xB0 << 6)
        s.setdefault(14, {}).update(mac=(3 << 14) | (0xA0 << 6) | 0x08, third=0x1000)  # InitMac r0 x r1 = 0
        s.setdefault(27, {})["seq"] = jump_word(27, 20, 4)
        s.setdefault(P, {})["u1"] = 0x7800
        s.setdefault(34, {})["u1"] = 0x7800
        s.setdefault(35, {}).update(alu=mv_src(5), third=0x0001)
        s.setdefault(37, {})["u0"] = 0x60FB
        s.setdefault(42, {})["seq"] = 0x0038
        k = case(*prog(44, s), fill_range=(0x1800, 0x2800), reads=[0x1800])
        k["kind"] = "raw"
        c.append(("load at %d (stored: 0x10000800 + loads)" % P, k))
    for P in (26, 27, 28, 29):
        s, F = regs_prog(regs)
        s.setdefault(11, {}).update(seq=imm(4, 3), mac=0xB0 << 6)
        s.setdefault(13, {}).update(mac=(3 << 14) | (0xA0 << 6) | 0x08, third=0x1000)
        s.setdefault(27, {})["seq"] = jump_word(27, 20, 4)
        s.setdefault(P, {}).update(mac=(1 << 14) | (0xA0 << 6) | 0x08, third=0x1200)  # Mac r2 x r1 = +1
        c.append(("Mac at %d" % P, kern(s, [(36, mv_src(2), 0x0001)])))
    return "raw2", c


# ---- running -------------------------------------------------------------------------------------------------


def run_case(w, c):
    """One case (its 'entry' and 'ctrl' as sol_cases_job takes them) -> {'tries', 'reads', 'diffs'}."""
    c = dict(c)
    entry, ctrl = c.pop("entry", 4), c.pop("ctrl", None)
    job, blocks = W.sol_cases_job([c], entry=entry, timeout=20000, ctrl=ctrl)
    ((tries, reads, diffs),) = W.parse_cases([c], w.run(job, blocks, timeout=120))
    return {"tries": tries, "reads": reads, "diffs": diffs}


def run_set(w, kind, cases):
    """A probe set's cases one by one, a summary line each; the results with each case's program words."""
    res = []
    for label, c in cases:
        r = run_case(w, c)
        res.append(dict(label=label, **r, mem1=["%08x" % x for x in c["mem1"]], mem0=["%08x" % x for x in c["mem0"]]))
        print("%-34s %s" % (label, summary(c.get("kind", kind), r)), flush=True)
    return res


def summary(kind, r):
    if r["tries"] == 0:
        return "TIMED OUT"
    if kind == "mac":
        return "passes %d" % (r["reads"][0] - 1)
    if kind == "raw":
        return " ".join("%08x" % v for v in r["reads"])
    if kind == "condrow":  # condmulti_case: the nine lane cases, X = executed
        return cond_row(r)
    if kind == "pred":  # set_flagops: did the predicated move (read 1) execute
        v = r["reads"][1]
        return "EXEC" if v == 0x66666666 else "skip" if v == 5 else "?%08x" % v
    if kind == "au2":
        return au2_summary(r)
    if kind == "raw2":  # kern(): store k = banks 6/7 word k, lane 1.lane 0
        n = len(r["reads"]) // 2
        return " ".join("%08x.%08x" % (r["reads"][n + k], r["reads"][k]) for k in range(n))
    if kind == "regs":  # kern(): store k = banks 6/7 word k, the ALU op is a Not
        n = len(r["reads"]) // 2
        return " ".join("%08x.%08x" % (~r["reads"][n + k] & 0xFFFFFFFF, ~r["reads"][k] & 0xFFFFFFFF) for k in range(n))
    if kind == "trace":
        path = trace_path(r)
        runs = []
        for pos, _seq in path:
            if runs and pos == runs[-1][1] + 1:
                runs[-1][1] = pos
            else:
                runs.append([pos, pos])
        return "%d stores: %s" % (len(path), " ".join("%d-%d" % (a, b) if a != b else "%d" % a for a, b in runs))
    d = r["diffs"] or []
    if kind == "count":
        return "changed %d" % (sum(1 for a, _ in d if a != "more") + sum(v for a, v in d if a == "more"))
    out = ["%08x" % v for v in r["reads"]]
    for a, v in d:
        if a == "more":
            out.append("+%d" % v)
            continue
        i = (~v & 0xFFFFFFFF) - FILL
        out.append("%04x<-%s" % (a, "%04x" % (4 * i) if 0 <= i < 0xA00 else "%08x" % v))
    return " ".join(out)


# ---- fourth pass (2026-10-23): experiments ----------------------------------------------------------------------

CTRL = 0x01740000  # Sol control block, as Warp1 sees it (ARM 0xD0740000)
REGV = {n: rv(n) for n in range(1, 8)}
W0 = 48  # where classify() puts the word under test
out = {}


def run(w, c):
    return run_case(w, c)


def job_with_reads(w, c, after=(), timeout=3000, entry=4):
    """One case (programs, data, clears, start, bounded poll) and then R32 of `after` in the same Warp1 job, so the
    reads see Sol before Warp1 is parked. Returns [tries, case reads..., diff results..., after...]."""
    job, blocks = W.sol_cases_job([c], entry=entry, timeout=timeout)
    return w.run(job + [(W.R32, a, 0, 0) for a in after], blocks, timeout=60)


def sol_reset(w):
    """Sol reset: a pulse on tgen gate 21 (clears r1-r7, the counters, +0x0C/+0x0D, the MAC configuration)."""
    w.park()
    w.wr(0xE0007020, 1 << 21)
    time.sleep(0.05)
    w.wr(0xE0007024, 1 << 21)
    time.sleep(0.05)


def ka(w):
    """Known answer: Not of a committed register, and a 3-pass counted loop. A dummy run first: after a stall the
    next run's first store writes a stale value."""
    run(w, kern({}, [(2, mv_src(0), 0x0001)]))
    s, F = regs_prog({1: 0x12345678})
    r = run(w, kern(s, [(F, not_src(0), 0x1000)]))
    r2 = run(w, counted((imm(4, 3),), k=4))
    d = r2["diffs"] or []
    return (
        bool(r["tries"])
        and r["reads"][0] == 0xEDCBA987
        and bool(r2["tries"])
        and sum(1 for a, _ in d if a != "more") == 3
    )


def stores_count(r):
    d = r["diffs"] or []
    return sum(1 for a, _ in d if a != "more") + sum(v for a, v in d if a == "more")


# ---- start offset ----


def x_entry(w):
    """+0x20 start offset: one store in each of instructions 0..94; the first instruction that ran = 95 - stores.
    Start instruction = +0x20 / 4."""
    res = {}
    for entry in (0, 4, 8, 0x10, 0x20, 0x28, 0x40, 0x80, 0x100, 0x140, 0x144):
        s = {p: {"u0": 0x60FB} for p in range(95)}
        s[98] = {"seq": 0x0038}
        c = case(*prog(100, s), fill_range=(0x1800, 0x1C00), entry=entry)
        c["unit_regs"] = []
        res["%#x" % entry] = 95 - stores_count(run(w, c))
        print("entry %#5x: first instruction %d" % (entry, res["%#x" % entry]), flush=True)
    return res


# ---- address unit pointer rule and the lane-1 wrap condition ----


def load_seq(w, regs, n=14, lane=0):
    """Unit 1 registers in-slot from instruction 1 (then 0xF00F), n loads (both lanes) moved and stored; returns
    the loaded word indexes of lane 0 and lane 1."""
    s = {}
    i = 1
    for r_ in regs:
        if r_ == "F":
            s.setdefault(i, {}).update(u0=0xF00F, u1=0xF00F)
        else:
            s.setdefault(i, {})["u1"] = r_
        i += 1
    for k in range(n):
        s.setdefault(i + 1 + k, {})["u1"] = 0x7800
        s.setdefault(i + 2 + k, {}).update(alu=mv_src(5), third=0x0001)
        s.setdefault(i + 4 + k, {})["u0"] = 0x60FB
    s.setdefault(i + n + 8, {})["seq"] = 0x0038
    c = case(
        *prog(i + n + 10, s),
        fill_range=(0, 0x2800),
        reads=[0x1800 + 4 * k for k in range(n)] + [0x1C00 + 4 * k for k in range(n)],
    )
    r = run(w, c)
    return [v - 0x10000800 for v in r["reads"][:n]], [v - 0x10000900 for v in r["reads"][n:]]


def x_pointer(w):
    """Load pointer: offset starts at C8 (lane 1: CE); address = base + offset, base starts at D4 (DA); after each
    load offset += stride; when offset >= modulo: offset = C8 and base += D8 (DE). Also: registers in instruction 0
    never apply (it does not run with start offset 4)."""
    res = {}
    import itertools

    for c8, d4, d8 in itertools.product((0, 1, 3), (0, 2), (0, 3)):
        l0, _ = load_seq(w, [0xCC01, 0xCA04, 0xC800 | c8, 0xD400 | d4, 0xD800 | d8, "F"])
        res["C8=%d D4=%d D8=%d mod4" % (c8, d4, d8)] = l0
        print("C8=%d D4=%d D8=%d mod 4: %s" % (c8, d4, d8, " ".join("%x" % v for v in l0)), flush=True)
    for name, regs in (
        ("lane 1: CE=1 DA=2 DE=3 mod 4", [0xD201, 0xD004, 0xCE01, 0xDA02, 0xDE03, "F"]),
        ("order: F00F then registers", ["F", 0xCC01, 0xCA04]),
        ("order: registers then F00F", [0xCC01, 0xCA04, "F"]),
    ):
        l0, l1 = load_seq(w, regs)
        res[name] = [l0, l1]
        print(
            "%-32s lane0 %s | lane1 %s" % (name, " ".join("%x" % v for v in l0), " ".join("%x" % v for v in l1)),
            flush=True,
        )
    return res


def x_auwrap(w):
    """Predicate term source 4/5/6 (address unit 0/1/2), select 0: a lane-1 load wrapped (modulo). A 12-pass loop with
    one load per pass and a predicated store: the number of true passes for each select."""

    def cnt(word, regs, access=0x7800, passes=12, unit="u1"):
        s = {}
        for i, r_ in enumerate(regs):
            s.setdefault(1 + i, {})[unit] = r_
        s.setdefault(1 + len(regs), {}).update(u0=0xF00F, u1=0xF00F, u2=0xF00F)
        s.setdefault(2 + len(regs), {})["seq"] = imm(4, passes)
        s.setdefault(27, {})["seq"] = jump_word(27, 20, 4)
        s.setdefault(38, {})["seq"] = 0x0038
        s.setdefault(20, {})[unit] = access
        tgt = "u0" if unit != "u0" else "u1"
        s.setdefault(24, {})["seq"] = word
        s[24][tgt] = 0x60FB
        c = case(*prog(40, s), fill_range=(0x1800, 0x1C00) if tgt == "u0" else (0x2000, 0x2400))
        return stores_count(run(w, c))

    res = {}
    S = [0xCC01, 0xD201]
    for name, regs, acc, unit, code in (
        ("u1 l0 mod 2, l1 mod 3, both lanes", S + [0xCA02, 0xD003], 0x7800, "u1", 5),
        ("u1 l0 mod 3, l1 mod 2, both lanes", S + [0xCA03, 0xD002], 0x7800, "u1", 5),
        ("u1 l0 mod 2, l1 mod 3, lane 0 loads", S + [0xCA02, 0xD003], 0x6800, "u1", 5),
        ("u1 l0 mod 2, l1 mod 3, lane 1 loads", S + [0xCA02, 0xD003], 0x7000, "u1", 5),
        ("u1 l1 mod 2, read with code 4", S + [0xD002], 0x7800, "u1", 4),
        ("u2 l1 mod 2, code 6", S + [0xD002], 0x7800, "u2", 6),
    ):
        row = [cnt(0xD500 | (code << 2) | sel, regs, acc, unit=unit) for sel in range(4)]
        res[name] = row
        print("%-36s true passes sel 0-3: %s" % (name, row), flush=True)
    return res


# ---- sources 6 and 7, SIMD routing ----


def x_src67(w):
    """Source 6 = unit 2's load latch (kept until the next unit-2 load, across runs); source 7 = source 3 | source 6."""
    res = {}
    for V, Y, load in ((0xF0, 0x0F, True), (0x12000000, 0x34, True), (0x55000000, 0x34, False), (0, 0x34, True)):
        s, F = regs_prog({4: V, 3: Y})
        for i, v in enumerate(AU2_CFG):
            s.setdefault(F + i, {})["u2"] = v
        G = F + len(AU2_CFG)
        s.setdefault(G, {})["u2"] = 0xF00F
        s.setdefault(G + 1, {}).update(alu=mv_src(0), third=0x4000)
        s.setdefault(G + 3, {})["u2"] = 0x60FB
        s.setdefault(G + 4, {})["u2"] = 0xF00F
        if load:
            s.setdefault(G + 6, {})["u2"] = 0x7800
        s.setdefault(G + 8, {}).update(alu=mv_src(0), third=0x3000)
        r = run(w, kern(s, [(G + 10, mv_src(7), 0x0001), (G + 11, mv_src(6), 0x0001)]))
        key = "u2 stored %08x%s, ALU %08x" % (V, "" if load else " (no load)", Y)
        res[key] = {"src7": r["reads"][0], "src6": r["reads"][1]}
        print("%-44s src7 %08x src6 %08x" % (key, r["reads"][0], r["reads"][1]), flush=True)
    return res


def x_simd(w):
    """dst lane routing on every SIMD add/sub op (0x18-0x20), none on shifts/rotates (0x14, 0x21, 0x22) or 64-bit ops."""
    A1, A0, B1, B0 = 0x00300000, 0x00050000, 0x00000200, 0x00000007
    data = {0x1800: A0, 0x1C00: A1, 0x2000: B0, 0x2400: B1}
    res = {}
    for op in list(range(0x18, 0x21)) + [0x14, 0x21, 0x22, 0x0B]:
        row = {}
        for dst in (0, 1, 2, 4, 5, 8, 12):
            r = run(
                w,
                kern(
                    {1: {"u0": 0x7800, "u1": 0x7800}},
                    [(2, (op << 10) | (dst << 6) | (5 << 3) | 4, 0)],
                    data=data,
                    fill=0,
                ),
            )
            row[dst] = "%08x.%08x" % (r["reads"][1], r["reads"][0])
        res["%02x" % op] = row
        print("op%02x" % op, " ".join("d%d:%s" % kv for kv in row.items()), flush=True)
    return res


# ---- the control block and immediates ----


def x_ctrl(w):
    """Immediates write control-block bytes: idx 0-7 = +0x00-0x07 (the counters), 12/13 = +0x0C/+0x0D, 20/22/23/24 =
    +0x14/+0x16/+0x17/+0x18; +0x20 reads back the instruction (x 4, 3 ahead) and a status in bits 18:16."""
    res = {}
    reads = [CTRL + 4 * k for k in range(10)]

    def after_imm(idx, val):
        s = {1: {"seq": (val << 5) | idx}, 10: {"seq": 0x0038}}
        r = job_with_reads(w, case(*prog(12, s), fill=0, reads=[]), reads)
        return r[0], r[-10:]

    t, base = after_imm(30, 0)
    res["baseline"] = base
    print("baseline", " ".join("%08x" % x for x in base))
    for idx in list(range(0, 20)) + [21, 25, 26, 27, 28, 29, 30, 31]:
        t, v = after_imm(idx, 0x5A)
        res["idx %d := 5a" % idx] = v
        print(
            "idx %2d := 0x5a  changed: %s"
            % (idx, " ".join("+%02x:%08x" % (4 * k, x) for k, x in enumerate(v) if x != base[k]) or "-"),
            flush=True,
        )
        base = v
    for idx, val in ((12, 0x20), (13, 0x20)):
        after_imm(idx, val)
    return res


def x_ctrl17(w):
    """+0x17 per bit (from inside a program): ALU (Not of a register), MAC (InitMac, output), unit-1 load; stall or
    not; known answer after each."""
    res = {}

    def probe(val):
        s, F = regs_prog({1: 0x12345678, 2: 1})
        s.setdefault(F, {})["seq"] = (val << 5) | 23
        G = F + 3
        s.setdefault(G - 2, {})["mac"] = 0xB0 << 6
        s.setdefault(G + 1, {}).update(mac=(3 << 14) | (0xA0 << 6) | 0x08, third=0x2100)
        data = {0x2000: 0x00040003, 0x2400: 0x00008000}
        s.setdefault(G + 6, {})["u1"] = 0x7800
        c = kern(
            s,
            [(G, not_src(0), 0x1000), (G + 4, mv_src(2), 0x0001), (G + 7, mv_src(5), 0x0001)],
            data=data,
            fill=0,
        )
        r = job_with_reads(w, c, [CTRL + 0x14, CTRL + 0x20], timeout=20000)
        return {"tries": r[0], "alu_not_r1": r[1], "mac_out": r[2], "u1_load_lane0": r[3], "ctrl14": r[-2], "pc": r[-1]}

    for val in [0x54] + [0x54 ^ (1 << b) for b in (1, 2, 3, 4, 5, 6, 7, 0)] + [0x01]:
        d = probe(val)
        d["ka_after"] = ka(w)
        res["%02x" % val] = d
        print(
            "+0x17 := %02x: %s"
            % (
                val,
                " ".join(
                    "%s=%s" % (k, ("%08x" % v) if isinstance(v, int) and k != "tries" else v) for k, v in d.items()
                ),
            ),
            flush=True,
        )
    return res


def x_wedge(w):
    """+0x17 = 0x55 written by Warp1 before the start: Sol never starts (PC stays at the start, done 0) and stays so
    with 0x54 restored; a Sol reset (tgen bit 21) recovers it. Then what that reset clears."""
    res = {}
    k = copy_kernel()
    k["ctrl"] = (0xE0, 0x0E, 0x55)
    res["run_with_55_tries"] = run(w, k)["tries"]
    res["ka_after_wedge"] = [ka(w), ka(w)]
    res["ctrl_wedged"] = w.run([(W.R32, CTRL + 4 * i, 0, 0) for i in range(9)])
    sol_reset(w)
    res["ka_after_sol_reset"] = ka(w)
    print(
        "wedge: tries %d, ka after %s, ka after Sol reset %s"
        % (res["run_with_55_tries"], res["ka_after_wedge"], res["ka_after_sol_reset"]),
        flush=True,
    )
    run(w, kern(all_commits(), [(44, mv_src(0), 0x1000)]))
    run(w, mac_config(0xB0 << 6))

    def state():
        rr = run(w, kern({}, [(2 + i, mv_src(0), (n << 12)) for i, n in enumerate(range(1, 8))]))
        ctl = w.run([(W.R32, CTRL + 4 * i, 0, 0) for i in range(4)])
        data = {0x1800: 0x00020001, 0x1C00: 0x00040003, 0x2000: 0x00000100, 0x2400: 0x00010000}
        m = run(
            w,
            kern(
                {5: {"u0": 0x7800, "u1": 0x7800}, 6: {"mac": (2 << 14) | (0xD8 << 6) | 0x2C}},
                [(8, mv_src(2), 0)],
                data=data,
                fill=0,
            ),
        )
        return {"regs": rr["reads"][:7], "ctrl": ctl, "mac_d8": [m["reads"][1], m["reads"][0]]}

    res["before_reset"] = state()
    sol_reset(w)
    res["after_reset"] = state()
    print("state before %s\n      after  %s" % (res["before_reset"], res["after_reset"]), flush=True)
    return res


def x_cond08(w):
    """+0x08 (write-only, 11-bit): a write with value bits 9:8 = 11 sets the condition bits the 0xE0xx/0xE4xx jumps
    test: c1 = bit 6, c0 = any of bits 2, 3, 4, 7. c0 = 1, c1 = 0 after a Sol reset. KnOS's test program writes
    0x320 (0x6408): c0 = c1 = 0."""

    def eseq(word, imm_word):
        s = {}
        for n in range(1, 8):
            s |= commit(n, REGV[n], 1 + 5 * (n - 1))
        if imm_word is not None:
            s[36] = {"seq": imm_word}
        s.setdefault(W0, {}).update(seq=word, third=0x3400, alu=mv_src(0), u0=0x60FB)
        for i, rg in enumerate([1, 2, 4, 6, 7, 1, 2, 4]):
            s.setdefault(W0 + 1 + i, {}).update(alu=mv_src(0), third=rg << 12)
            s.setdefault(W0 + 2 + i, {})["u0"] = 0x60FB
        s[W0 + 12] = {"seq": 0x0038}
        r = run(w, case(*prog(W0 + 14, s), fill_range=(0x1800, 0x1C00)))
        n = stores_count(r)
        return ("T" if n == 6 else "n" if n == 9 else str(n)) if r["tries"] else "H"

    words = [0xE008, 0xE009, 0xE408, 0xE409, 0xE808, 0xE809, 0xEC08, 0xEC09]
    res = {}
    print("                  " + " ".join("%04x" % x for x in words))
    for v in [None, 0x000, 0x100, 0x200, 0x300, 0x301, 0x302, 0x304, 0x308, 0x310, 0x320, 0x340, 0x380, 0x400, 0x700]:
        sol_reset(w)
        row = [eseq(wd, None if v is None else (v << 5) | 8) for wd in words]
        res["reset" if v is None else "%03x" % v] = row
        print("%-18s" % ("after reset" if v is None else "+0x08 := %03x" % v) + "    ".join(row), flush=True)
    sol_reset(w)
    return res


# ---- sequencer words ----


def classify(w, word, third=0x3400):
    """r1-r7 = rv(n), counters k = 0x10 + k, flags negative (SimdSub r1 - r2), then at W0 the word (with a move of
    r3 and a store of r5); markers at W0+1.. (moves r1 r2 r4 r6 r7 r1 r2 r4, each stored 2 later). Reports whether
    Sol finished, the stored sequence, counters and registers afterwards."""
    s = {}
    for n in range(1, 8):
        s |= commit(n, REGV[n], 1 + 5 * (n - 1))
    for k in range(8):
        s[36 + k] = {"seq": imm(k, 0x10 + k)}
    s[44] = {"alu": SIMDSUB_AB, "third": 0x1200}
    s[46] = {"alu": mv_src(0), "third": 0x5000}
    s.setdefault(W0, {}).update(seq=word, third=third, alu=mv_src(0), u0=0x60FB)
    order = [1, 2, 4, 6, 7, 1, 2, 4]
    for i, rg in enumerate(order):
        s.setdefault(W0 + 1 + i, {}).update(alu=mv_src(0), third=rg << 12)
        s.setdefault(W0 + 2 + i, {})["u0"] = 0x60FB
    s[W0 + 2 + len(order) + 2] = {"seq": 0x0038}
    c = case(*prog(W0 + 2 + len(order) + 4, s), fill_range=(0x1800, 0x1C00), reads=[0x1800 + 4 * k for k in range(14)])
    r = job_with_reads(w, c, [CTRL + 0x00, CTRL + 0x04, CTRL + 0x14, CTRL + 0x18, CTRL + 0x20], timeout=5000)
    tries, st, ctl = r[0], r[1:15], r[-5:]
    inv = {v: "r%d" % n for n, v in REGV.items()}
    seq = []
    for v in st:
        if (v >> 12) == 0x10000:
            break
        seq.append(inv.get(v, "%x" % v))
    d = {
        "word": "%04x" % word,
        "done": bool(tries),
        "stores": " ".join(seq),
        "counters": "%08x %08x" % (ctl[0], ctl[1]),
    }
    if not tries:
        d["ka_after"] = ka(w)
    else:
        rr = run(w, kern({}, [(2 + i, mv_src(0), (n << 12)) for i, n in enumerate(range(1, 8))]))
        d["regs"] = " ".join("r%d=%08x" % (n, rr["reads"][n - 1]) for n in range(1, 8) if rr["reads"][n - 1] != REGV[n])
    return d


SEQ_WORDS = (
    ["a000", "8001", "800c", "800d", "800f", "818c", "818f", "80ad"]
    + ["%04x" % (0x8400 | n) for n in (1, 2, 3, 5, 8, 0xB, 0xC, 0x80, 0x3FF, 0x3FE)]
    + ["8c01", "8c03", "8c0f", "8c10", "8d8f", "9011", "9101", "9411", "9501"]
    + ["a020", "a041", "a0e0", "a0e4", "a028", "a120", "a301", "b001", "b021", "b041", "b081", "b009", "b101"]
    + ["c00d", "c18c", "c58c", "c98c", "c98d", "cd8c"]
    + ["e000", "e001", "e004", "e008", "e010", "e080", "e100", "e400", "e800", "ec00"]
    + ["f000", "f004", "f010", "f080", "f400", "f404", "f800", "f8a0", "f8a1", "fc00", "fca0", "fe00", "ff00"]
)


def x_seqwords(w):
    res = []
    for x in SEQ_WORDS:
        d = classify(w, int(x, 16))
        res.append(d)
        print(" ".join("%s=%s" % kv for kv in d.items()), flush=True)
        if d.get("ka_after") is False:
            print("Sol not recovering: stop")
            break
    return res


def x_counterops(w):
    """0xA0mk: counter k := counter m (bits 9:8 set: 0); 0xB0mk: counter k := counter k & ~counter m."""
    CV = [0x35, 0x5A, 0x07, 0xC3, 0x81, 0x10, 0xFF, 0x01]
    res = {}

    def after(word):
        s = {k + 1: {"seq": imm(k, CV[k])} for k in range(8)}
        s[10] = {"seq": word}
        s[14] = {"seq": 0x0038}
        r = job_with_reads(w, case(*prog(16, s), fill=0, reads=[]), [CTRL, CTRL + 4])
        v = r[-2] | (r[-1] << 32)
        return [(v >> (8 * k)) & 0xFF for k in range(8)]

    for cls in (0xA000, 0xB000):
        for k in (1, 3):
            for m in range(8):
                wd = cls | (m << 5) | k
                a = after(wd)
                res["%04x" % wd] = a
                print(
                    "%04x: %s"
                    % (wd, ", ".join("c%d %02x->%02x" % (i, CV[i], a[i]) for i in range(8) if a[i] != CV[i]) or "-"),
                    flush=True,
                )
    return res


def x_call(w):
    """0xF8xx: call (counter 6 := J + 2, target J + 1 + bits 9:0 mod 128); 0xFCxx: the same jump without the link.
    Traced with trace_case over a 127-instruction program."""
    res = {}
    for name, words in (
        ("F8A0 at 40", {40: 0xF8A0}),
        ("F8A1 at 40", {40: 0xF8A1}),
        ("FCA0 at 40", {40: 0xFCA0}),
        ("8414 at 40", {40: 0x8414}),
    ):
        c = trace_long(words, region=(3, 122))
        r = job_with_reads(w, c, [CTRL, CTRL + 4], timeout=5000)
        reads = r[1:257]
        rr = {"reads": reads, "tries": r[0]}
        path = trace_path(rr)
        runs = []
        for pos, _sq in path:
            if runs and pos == runs[-1][1] + 1:
                runs[-1][1] = pos
            else:
                runs.append([pos, pos])
        res[name] = {"path": runs, "counters": "%08x %08x" % (r[-2], r[-1])}
        print(
            "%-12s positions %s  counters %s"
            % (name, " ".join("%d-%d" % tuple(x) for x in runs), res[name]["counters"]),
            flush=True,
        )
    return res


# ---- Warp cores ----


def x_icache(w):
    """I-cache on both cores: stale code at a reused address, then COP3 = 0x10, zero the tags 0x02000000-0x020005FF,
    COP3 = 5: the new code runs. And the cache data RAM at 0x10000 (COP3 bit 4) holds a cached loop at its index."""
    res = {}
    for core in (0, 1):
        wc = w
        wc.dbg = 0xE0014000 + 0x1000 * core
        X = 0x61FF8000 + 0x40 * (int(time.time()) % 64)

        def code(val):
            return W.assemble(
                "    .set noreorder\n    .set noat\n    .text\n    .globl _start\n_start:\n"
                " li $8, 5\n %s\n nop\n nop\n lui $8, 0x%04x\n ori $8, $8, 0x%04x\n %s jr $31\n nop\n"
                % (WP.MTC3_T0, val >> 16, val & 0xFFFF, WP.RESULT),
                X,
            )

        def call():
            return wc.run([(W.CALL, X, 0, 0)])[0]

        def invalidate():
            asm = (
                " li $8, 0x10\n %s\n nop\n nop\n lui $9, 0x0200\n lui $10, 0x0200\n ori $10, $10, 0x0600\n"
                "1: sw $0, 0($9)\n addiu $9, $9, 4\n bne $9, $10, 1b\n nop\n li $8, 5\n %s\n nop\n nop\n jr $31\n nop\n"
                % (WP.MTC3_T0, WP.MTC3_T0)
            )
            at = W.code_slot(asm) - 0x8000
            wc.run(
                [(W.CALL, at, 0, 0)],
                [
                    (
                        at,
                        W.assemble(
                            "    .set noreorder\n    .set noat\n    .text\n    .globl _start\n_start:\n" + asm, at
                        ),
                    )
                ],
            )

        wc.write_block(X, code(0xAAAA0000 | core))
        a = call()
        wc.write_block(X, code(0xBBBB0000 | core))
        b = call()
        invalidate()
        c = call()
        res["warp%d" % core] = {"A": a, "B before invalidate": b, "B after invalidate": c}
        print("Warp%d: A %08x, B written: %08x (stale), after tag invalidate: %08x" % (core, a, b, c), flush=True)
        wc.park()
    w.dbg = 0xE0015000
    return res


def x_dma0(w):
    """Warp0 block move through wsl.ko's first context (wsl.ko loaded, /dev/wsl_admin held: see warpprobe.dma)."""
    w.dbg = 0xE0014000
    mbox = w.rd(0xA8000000)[0]
    if not 0x00400000 <= mbox < 0x00410000:
        raise RuntimeError("no wsl.ko mailbox: load wsl.ko and hold /dev/wsl_admin")
    d = w.rd(mbox + WP.MBOX_DESC, 6)
    pat = [0x0D000000 + i for i in range(256)]
    w.write_words([(WP.DMA_SRC + W.UNCACHED + 4 * i, v) for i, v in enumerate(pat)])
    r = w.snippet(WP.move_snippet(d[0], d[1], WP.DMA_SRC, WP.DMA_SOL, 256), at=W.code_slot("warp0 dma p4") - 0x4000)
    got = w.rd(0xA8001800, 256)
    w.park()
    w.dbg = 0xE0015000
    res = {"ctx": d[0], "client": d[1], "polls": r[0], "copied": got == pat}
    print("Warp0 block move: %s" % res)
    return res


# ---- fifth pass (2026-10-23, late): experiments ------------------------------------------------------------------

STATUS = 0x01400000  # 8 status words (ARM 0xD0400000)
SIGNS = [(-1, -1), (1, 1), (1, -1), (-1, 1), (0, 0)]  # (lane 1, lane 0) of the flag-setting SimdSub


def sign_name(s1, s0):
    return {1: "+", 0: "0", -1: "-"}[s1] + {1: "+", 0: "0", -1: "-"}[s0]


WEDGES = []


def alive(w):
    """One trivial run: did Sol finish? (cheaper than ka)."""
    return bool(run(w, kern({}, [(2, mv_src(0), 0x0001)]))["tries"])


def recover(w, what=""):
    """After a stall: if Sol does not run a trivial program any more (wedged), a Sol reset; recorded in WEDGES."""
    if alive(w):
        return False
    sol_reset(w)
    WEDGES.append(what)
    print("   [Sol wedged after %s: reset, alive %s]" % (what, alive(w)), flush=True)
    return True


def run_batch(w, cases, per_job=8):
    """Several cases per Warp job (one halt/start/park for all); a case Sol does not finish (tries 0) blocks the
    rest of its job, so those are re-run (after a wedge check). Returns one {'tries','reads','diffs'} per case."""
    out = [None] * len(cases)
    todo = list(range(len(cases)))
    while todo:
        chunk, todo = todo[:per_job], todo[per_job:]
        cs = [dict(cases[i]) for i in chunk]
        entry = cs[0].pop("entry", 4)
        for c in cs:
            c.pop("entry", None)
            c.pop("ctrl", None)
        job, blocks = W.sol_cases_job(cs, entry=entry, timeout=4000)
        res = W.parse_cases(cs, w.run(job, blocks, timeout=120))
        for n, (i, (tries, reads, diffs)) in enumerate(zip(chunk, res)):
            out[i] = {"tries": tries, "reads": reads, "diffs": diffs}
            if not tries:
                out[i]["wedged"] = recover(w, "case %d" % i)
                todo = chunk[n + 1 :] + todo
                break
    return out


# ---- kernels --------------------------------------------------------------------------------------------------


def flag_case(word, s1, s0, pre=(), markers=(1, 2, 4, 6, 7, 1, 2, 4), extra=None):
    """Flags from SimdSub(load - r2) at F with lane signs (s1, s0); `word` at W0 = F + 3 with a move of r6 (stored
    at W0 + 2); then markers: moves of registers at W0 + 1 + i, each stored at W0 + 2 + i (unit 0, banks 6/7);
    end at W0 + 12. `pre`: up to 4 sequencer words placed at at..at+3 (before the flags). Readout: the stored
    sequence (8 stores when the word is a no-op: the word's own move then 7 markers; 5 after a jump of 4; none when
    Sol stalled)."""
    s, at = regs_prog({1: 5, 2: 10, 4: 0x44444444, 6: 0x66666666, 7: 0x77777777})
    for i, v in enumerate(pre):
        s.setdefault(at + i, {})["seq"] = v
    s.setdefault(at, {})["u1"] = 0xCC01
    s.setdefault(at + 1, {})["u1"] = 0xD201
    F = at + max(4, len(pre) + 1)
    data = {0x2000: 10 + 2 * s0, 0x2400: 10 + 2 * s1}
    s.setdefault(F - 1, {})["u1"] = 0x7800
    s.setdefault(F, {}).update(alu=(0x19 << 10) | (4 << 6) | (1 << 3) | 5, third=0x0200)  # SimdSub load.k - r2
    s.setdefault(F + 1, {}).update(alu=mv_src(0), third=0x1000)  # ALU result := r1
    W0 = F + 3
    s.setdefault(W0, {}).update(seq=word, alu=mv_src(0), third=0x6000)
    for i, rg in enumerate(markers):
        s.setdefault(W0 + 1 + i, {}).update(alu=mv_src(0), third=rg << 12)
        s.setdefault(W0 + 2 + i, {})["u0"] = 0x60FB
    for k, v in (extra or {}).items():
        s.setdefault(k, {}).update(v)
    s.setdefault(W0 + 12, {})["seq"] = 0x0038
    c = case(*prog(W0 + 14, s), fill_range=(0x1800, 0x1C00), data=data, reads=[0x1800 + 4 * k for k in range(12)])
    c["W0"] = W0
    return c


RNAME = {5: "r1", 10: "r2", 0x44444444: "r4", 0x66666666: "r6", 0x77777777: "r7"}


def flag_decode(r):
    """-> (kind, stores): 'H' stalled, 'X' word's move executed (9 stores), '.' skipped (9 stores), 'J' fewer
    stores (a jump), '?' otherwise."""
    if not r["tries"]:
        return "H", ""
    seq = []
    for k, v in enumerate(r["reads"]):
        if v == FILL + 0x600 + k:
            break
        seq.append(RNAME.get(v, "%x" % v))
    st = " ".join(seq)
    if len(seq) == 8:
        return ("X" if seq[0] == "r6" else "." if seq[0] == "r1" else "?"), st
    return "J", st


def rich_case(ctrl=None, pre=(), extra=None):
    """One program exercising every unit; results banks 6/7 words 0-17 (R0..R17) and bank 8 words 0-4.
    R0 unit-0 load (0x00040003 / 0x00008000), R1 unit-1 load (0x00050007 / 0xFFFF8000), R2 Not r1, R3 SimdAdd
    load+r2 per lane, R4 Shift1 r1 by 3, R5 Add r1+r3, R6 AddS r3+r3 (unsigned, saturates), R7 Mult r1 x 3, R8 Mult
    r3 x 3, R9 InitMac+Mac r1 x 3, R10 r1, R11 r7 under 0xD5AD (lane 0 < 0: true), R12 r1, R13 r7 under 0xD58C
    (false: r1 expected), R14 r5 := counters (0x0000CDAB), R15 r4, R16 unit 2 round trip (0x11112222), R17 load;
    bank 8 word 0 = packed store of R17 (0x80000007), words 1-3 = a 3-pass loop's stores (0x66666666)."""
    s, F = regs_prog({1: 0x12345678, 2: 3, 3: 0x80000001, 4: 0x11112222, 7: 0x66666666})
    for i, v in enumerate(AU2_CFG):
        s.setdefault(1 + i, {})["u2"] = v
    s.setdefault(7, {})["u2"] = 0xF00F
    for i, v in enumerate(pre):
        s.setdefault(F - 2 + i, {})["seq"] = v
    data = {0x1800: 0x00040003, 0x1C00: 0x00008000, 0x2000: 0x00050007, 0x2400: 0xFFFF8000}
    i = F
    reads = []
    s.setdefault(i, {})["u0"] = 0x7800
    reads.append((i + 1, mv_src(4), 0x0001))
    s.setdefault(i + 1, {})["u1"] = 0x7800
    reads.append((i + 2, mv_src(5), 0x0001))
    reads.append((i + 3, not_src(0), 0x1000))
    s.setdefault(i + 4, {})["u1"] = 0x7800
    reads.append((i + 5, (0x18 << 10) | (4 << 6) | (1 << 3) | 5, 0x1200))
    reads.append((i + 6, (0x14 << 10) | (1 << 3) | 0, 0x1200))
    reads.append((i + 7, (0x0B << 10) | (1 << 3) | 0, 0x1300))
    reads.append((i + 8, (0x0F << 10) | (1 << 3) | 1, 0x1300))
    s.setdefault(i + 9, {})["mac"] = 0xB0 << 6
    s.setdefault(i + 10, {}).update(mac=(2 << 14) | (0xB0 << 6) | 1, third=0x1200)
    s.setdefault(i + 11, {}).update(mac=(2 << 14) | (0xB0 << 6) | 1, third=0x3200)
    reads.append((i + 12, mv_src(2), 0x1200))
    reads.append((i + 13, mv_src(2), 0x3200))
    s.setdefault(i + 14, {}).update(mac=(3 << 14) | (0xB0 << 6) | 1, third=0x1200)
    s.setdefault(i + 15, {}).update(mac=(1 << 14) | (0xB0 << 6) | 1, third=0x1200)
    reads.append((i + 17, mv_src(2), 0x1200))
    s.setdefault(i + 18, {}).update(alu=SIMDSUB_AB, third=0x1300)
    reads.append((i + 19, mv_src(0), 0x1000))
    s.setdefault(i + 21, {})["seq"] = 0xD5AD
    reads.append((i + 21, mv_src(0), 0x7000))
    reads.append((i + 22, mv_src(0), 0x1000))
    s.setdefault(i + 23, {})["seq"] = 0xD58C
    reads.append((i + 23, mv_src(0), 0x7000))
    s.setdefault(i + 24, {})["seq"] = imm(0, 0xAB)
    s.setdefault(i + 25, {})["seq"] = imm(1, 0xCD)
    s.setdefault(i + 26, {})["seq"] = imm(2, 0)
    s.setdefault(i + 27, {})["seq"] = imm(3, 0)
    s.setdefault(i + 28, {})["seq"] = 0x8805
    reads.append((i + 30, mv_src(0), 0x5000))
    reads.append((i + 31, mv_src(0), 0x4000))
    s.setdefault(i + 33, {})["u2"] = 0x60FB
    s.setdefault(i + 34, {})["u2"] = 0xF00F
    s.setdefault(i + 35, {})["u2"] = 0x7800
    reads.append((i + 36, mv_src(6), 0x0001))
    s.setdefault(i + 37, {})["u1"] = 0x7800
    reads.append((i + 38, mv_src(5), 0x0001))
    s.setdefault(i + 40, {})["u1"] = 0x61FB
    s.setdefault(i + 42, {})["seq"] = imm(4, 3)
    S = i + 44
    s.setdefault(S, {}).update(alu=mv_src(0), third=0x7000)
    s.setdefault(S + 2, {})["u1"] = 0x60FB
    s.setdefault(S + 3, {})["seq"] = jump_word(S + 3, S, 4)
    s.setdefault(S + 6, {})["seq"] = 0x0038
    for k, v in (extra or {}).items():
        s.setdefault(k, {}).update(v)
    c = kern(s, reads, n=S + 8, data=data, fill_range=(0x1800, 0x2800))
    c["reads"] += [0x2000 + 4 * k for k in range(5)]
    if ctrl:
        c["ctrl"] = ctrl
    return c


RICH_NAMES = ["R%d" % k for k in range(18)]


def rich_decode(r):
    n = 18
    if not r["tries"]:
        return {"stalled": True}
    d = {RICH_NAMES[k]: "%08x.%08x" % (r["reads"][n + k], r["reads"][k]) for k in range(n)}
    d["bank8"] = " ".join("%08x" % v for v in r["reads"][2 * n :])
    return d


def rich_diff(base, d):
    if d.get("stalled"):
        return "STALLED"
    return ", ".join("%s %s" % (k, d[k]) for k in base if d.get(k) != base[k]) or "-"


# ---- experiments ----------------------------------------------------------------------------------------------


def x_cwords(w):
    """0xC0xx/0xC4xx/0xC8xx/0xCCxx and 0x80xx: term A (bits 9:7 source, 6:5 select) and term B (4:2, 1:0) over
    source 0 (never true) and the flags (selects 0-3), under four lane-sign states: stalled / executed / skipped."""
    terms = [(0, 0), (3, 0), (3, 1), (3, 2)]
    res = {}
    for cls in (0x8000, 0xC000, 0xC400, 0xC800, 0xCC00):
        signs = SIGNS[:4]
        labels, cases = [], []
        for a_src, a_sel in terms:
            for b_src, b_sel in terms:
                word = cls | (a_src << 7) | (a_sel << 5) | (b_src << 2) | b_sel
                for s1, s0 in signs:
                    labels.append(("%04x" % word, sign_name(s1, s0)))
                    cases.append(flag_case(word, s1, s0))
        rs = run_batch(w, cases)
        for (wd, sg), r in zip(labels, rs):
            kind, st = flag_decode(r)
            res.setdefault(wd, {})[sg] = kind if kind != "J" else "J:" + st
        for wd in sorted(k for k in res if int(k, 16) & 0xFC00 == cls):
            print(
                "%s  %s" % (wd, "  ".join("%s %s" % (sg, res[wd][sg]) for sg in (sign_name(*x) for x in signs))),
                flush=True,
            )
        ka(w)
    return res


def x_ejumps(w):
    """0xE8xx / 0xECxx (and E0/E4 for calibration) with displacement 4 under: lane-sign states, +0x08 condition
    bytes, counter 0 zero / non-zero. T = jumped (6 stores), n = not (9), H = stalled."""
    words = [0xE008, 0xE009, 0xE408, 0xE409, 0xE808, 0xE809, 0xEC08, 0xEC09]
    res = {}
    sol_reset(w)
    states = [("flags %s" % sign_name(s1, s0), (s1, s0), ()) for s1, s0 in SIGNS]
    states += [("+08 := %03x, flags --" % v, (-1, -1), (imm(8, v),)) for v in (0x300, 0x304, 0x340, 0x344)]
    states += [("counter0 := %d, flags --" % v, (-1, -1), (imm(0, v),)) for v in (0, 1)]
    states += [("counter0 := %d, flags ++" % v, (1, 1), (imm(0, v),)) for v in (0, 1)]
    states += [("counter6 := %d, flags --" % v, (-1, -1), (imm(6, v),)) for v in (0, 7)]
    print("%-26s" % "" + " ".join("%04x" % x for x in words))
    for name, (s1, s0), pre in states:
        rs = run_batch(w, [flag_case(wd, s1, s0, pre=pre) for wd in words])
        row = []
        for r in rs:
            kind, st = flag_decode(r)
            row.append(
                "H"
                if kind == "H"
                else "n"
                if kind in "X."
                else "T"
                if len(st.split()) == 5
                else "J%d" % len(st.split())
            )
        res[name] = row
        print("%-26s" % name + "   ".join(row), flush=True)
    sol_reset(w)
    return res


def x_c08bits(w):
    """+0x08's byte: which bits set c0 (E008 jumps) and c1 (E009 jumps), one value per run."""
    res = {}
    sol_reset(w)
    vals = [1 << b for b in range(8)] + [
        0x9C,
        0x84,
        0x0C,
        0x1C,
        0x18,
        0x14,
        0xFF,
        0x7F,
        0xBF,
        0xDF,
        0xEF,
        0xF7,
        0xFB,
        0xFD,
        0xFE,
        0x63,
        0x03,
        0x00,
    ]
    for v in vals:
        rs = run_batch(w, [flag_case(wd, -1, -1, pre=(imm(8, 0x300 | v),)) for wd in (0xE008, 0xE009, 0xE408, 0xE409)])
        row = "".join("H" if not r["tries"] else "T" if len(flag_decode(r)[1].split()) == 5 else "n" for r in rs)
        res["%02x" % v] = row
        print("+0x08 := 3%02x  e008 %s e009 %s e408 %s e409 %s" % ((v,) + tuple(row)), flush=True)
    sol_reset(w)
    return res


def x_counterwords(w):
    """0xA4xx, 0xA8xx, 0xACxx, 0xB4xx, 0xB8xx, 0xBCxx with (m, k) pairs, 0xA0/0xB0 with bits 4:3 (k = 8-15 =
    control bytes +0x08-0x0F?), 0x88 commit with bits 9:3: finished?, counters and +0x0C/+0x0D after."""
    CV = [0x35, 0x5A, 0x07, 0xC3, 0x81, 0x10, 0xFF, 0x01]
    res = {}

    def after(word):
        s = {k + 1: {"seq": imm(k, CV[k])} for k in range(8)}
        s[9] = {"seq": imm(12, 0x33)}
        s[10] = {"seq": imm(13, 0x44)}
        s[12] = {"seq": word}
        s[16] = {"seq": 0x0038}
        r = job_with_reads(
            w, case(*prog(18, s), fill=0, reads=[]), [CTRL, CTRL + 4, CTRL + 0xC, CTRL + 0x20], timeout=3000
        )
        v = r[-4] | (r[-3] << 32)
        return r[0], [(v >> (8 * k)) & 0xFF for k in range(8)], r[-2], r[-1]

    words = []
    for cls in (0xA400, 0xA800, 0xAC00, 0xB400, 0xB800, 0xBC00):
        words += [cls | (m << 5) | k for m, k in ((3, 1), (1, 3), (6, 6), (5, 2))]
    words += [0xA000 | (m << 5) | k for m in (1, 3) for k in (0x8, 0xC, 0xD, 0x10, 0x14, 0x1C)]
    words += [0xB000 | (3 << 5) | k for k in (0xC, 0xD)]
    words += [0x8800 | b | 5 for b in (0x08, 0x10, 0x20, 0x40, 0x80, 0x100, 0x200, 0x3F8)]
    for wd in words:
        tries, cnt, c0c, st = after(wd)
        d = {"done": bool(tries), "counters": cnt, "ctrl0c": "%08x" % c0c, "ctrl20": "%08x" % st}
        if not tries:
            d["ka"] = ka(w)
            if not d["ka"]:
                sol_reset(w)
                d["ka_after_sol_reset"] = ka(w)
        else:
            rr = run(w, kern({}, [(2 + i, mv_src(0), (n << 12)) for i, n in enumerate(range(1, 8))]))
            d["r5"] = "%08x" % rr["reads"][4]
        res["%04x" % wd] = d
        print(
            "%04x: %s counters %s +0C %s +20 %s %s"
            % (
                wd,
                "done " if tries else "STALL",
                " ".join("%02x" % x for x in cnt),
                d["ctrl0c"],
                d["ctrl20"],
                d.get("r5", "") or d.get("ka", ""),
            ),
            flush=True,
        )
    return res


def x_ret(w):
    """A return for the 0xF8xx call: call at 40 (counter 6 := 42, target 72); a forward jump at 60 to 100; the
    candidate at 80. A return shows as a segment starting at 41 after 72-80 (trace positions = instruction - 1)."""
    res = {}
    cands = [0xF400 | k for k in range(8)] + [
        0xF408 | 6,
        0xF430,
        0xF4F6,
        0xF506,
        0xF606,
        0xF706,
        0xF7FE,
        0xA0D8,
        0xA0DE,
        0x8C16,
        0x9006,
        0x9406,
        0x900E,
        0x940E,
        0xE801,
        0xEC01,
        0xFC06,
        0xFE06,
        0xB4C6,
        0xB8C6,
        0xBCC6,
        0xA4C6,
        0xA461,
        0xA406,
        0xA861,
        0xA806,
        0xF40E,
        0x87FF,
    ]
    for cand in cands:
        c = trace_long({40: 0xF8A0, 60: 0x8427, 80: cand}, region=(3, 122))
        try:
            r = job_with_reads(w, c, [CTRL + 4, CTRL + 0x20], timeout=5000)
        except RuntimeError as e:
            res["%04x" % cand] = {"error": str(e)[:80]}
            print("%04x: runner error %s" % (cand, str(e)[:80]), flush=True)
            w.park()
            continue
        rr = {"reads": r[1:257], "tries": r[0]}
        path = trace_path(rr)
        runs = []
        for pos, _sq in path:
            if runs and pos == runs[-1][1] + 1:
                runs[-1][1] = pos
            else:
                runs.append([pos, pos])
        res["%04x" % cand] = {"done": bool(r[0]), "path": runs, "c4_7": "%08x" % r[-2], "ctrl20": "%08x" % r[-1]}
        print(
            "%04x: %s path %s  c4-7 %08x"
            % (cand, "done " if r[0] else "STALL", " ".join("%d-%d" % tuple(x) for x in runs), r[-2]),
            flush=True,
        )
        if not r[0]:
            ka(w)
    return res


def x_ctrlbits(w):
    """Every unused bit of control bytes +0x14/+0x15/+0x16/+0x17 and the bytes +0x0C/+0x0D against the rich kernel."""
    res = {}
    run(w, rich_case())
    base = rich_decode(run(w, rich_case()))
    res["baseline"] = base
    print("baseline:", " ".join("%s=%s" % kv for kv in base.items()), flush=True)
    tests = [("+14 bit %d" % b, dict(ctrl=(0xE0 | (1 << b), 0x0E, 0x54))) for b in range(7)]
    tests += [("+14 bit %d clear" % b, dict(ctrl=(0xE0 & ~(1 << b), 0x0E, 0x54))) for b in (5, 6)]
    tests += [("+15 bit %d" % b, dict(ctrl=(0xE0 | (1 << (8 + b)), 0x0E, 0x54))) for b in range(8)]
    tests += [("+16 bit %d" % b, dict(ctrl=(0xE0, 0x0E | (1 << b), 0x54))) for b in (0, 4, 5, 6, 7)]
    tests += [("+16 bit %d clear" % b, dict(ctrl=(0xE0, 0x0E & ~(1 << b), 0x54))) for b in (1, 2)]
    tests += [
        ("+17 := %02x" % v, dict(ctrl=(0xE0, 0x0E, v)))
        for v in (0x5C, 0x74, 0x7C, 0x50, 0x44, 0x14, 0xD4, 0x58, 0x64, 0x94)
    ]
    tests += [("+0C := %02x (imm)" % v, dict(pre=(imm(12, v),))) for v in (1, 0x80, 0xFF)]
    tests += [("+0D := %02x (imm)" % v, dict(pre=(imm(13, v),))) for v in (1, 0x80, 0xFF)]
    tests += [("+0C/+0D := 0 (imm)", dict(pre=(imm(12, 0), imm(13, 0))))]
    for name, kw in tests:
        try:
            d = rich_decode(run(w, rich_case(**kw)))
        except RuntimeError as e:
            d = {"error": str(e)[:100]}
            w.park()
        res[name] = d
        print("%-20s %s" % (name, rich_diff(base, d) if "error" not in d else d["error"]), flush=True)
        if d.get("stalled"):
            res[name]["ka"] = ka(w)
    # +0x1C: written by Warp1 before the start
    for v in (0, 4 * 40, 0xFFFF0000 | 4 * 40, 0x00280000, 0xFFFF0000):
        c = dict(rich_case(), pre_start=[(W.W32, CTRL + 0x1C, v, 0)])
        job, blocks = W.sol_cases_job([c], timeout=20000)
        raw = w.run(job + [(W.R32, CTRL + 0x1C, 0, 0), (W.R32, CTRL + 0x20, 0, 0)], blocks, timeout=60)
        ((tries, reads, diffs),) = W.parse_cases([c], raw[:-2])
        d = rich_decode({"tries": tries, "reads": reads})
        d["ctrl1c_after"], d["ctrl20_after"] = "%08x" % raw[-2], "%08x" % raw[-1]
        res["+1C := %08x" % v] = d
        print(
            "+1C := %08x  %s  (+1C after %s, +20 %s)" % (v, rich_diff(base, d), d["ctrl1c_after"], d["ctrl20_after"]),
            flush=True,
        )
        if d.get("stalled"):
            ka(w)
    return res


def x_stallpoke(w):
    """Sol stalled on a word; in the same Warp1 job: +0x18/+0x20 and the 8 status words, then one poke after
    another, each followed by a bounded poll of the done bit: which one resumes it. Job A: control bytes, slot words
    through the known unit windows, the unknown windows, the program word patched to a no-op (last). Job B (Sol
    stalled again): the restarts through +0x18 / +0x20."""
    res = {}
    W0 = flag_case(0xA000, -1, -1)["W0"]
    pokes_a = [
        ("+0C := 1", [(W.W8, CTRL + 0xC, 1, 0)]),
        ("+0D := 1", [(W.W8, CTRL + 0xD, 1, 0)]),
        ("+17 := 54", [(W.W8, CTRL + 0x17, 0x54, 0)]),
        ("+16 := 0E", [(W.W8, CTRL + 0x16, 0x0E, 0)]),
        ("+14 := E0", [(W.W16, CTRL + 0x14, 0xE0, 0)]),
        ("+08 := 300", [(W.W16, CTRL + 0x08, 0x300, 0)]),
        ("+08 := 3FF", [(W.W16, CTRL + 0x08, 0x3FF, 0)]),
        ("unit4 7800", [(W.W32, W.unit_reg(4, 0x7800), 0, 0)]),
        ("unit5 7800", [(W.W32, W.unit_reg(5, 0x7800), 0, 0)]),
        ("unit4 F00F", [(W.W32, W.unit_reg(4, 0xF00F), 0, 0)]),
        ("unit3 A000", [(W.W32, W.unit_reg(3, 0xA000), 0, 0)]),
        ("unit3 Not", [(W.W32, W.unit_reg(3, 0x0400), 0, 0)]),
        ("0x01700018 := 0", [(W.W8, 0x01700018, 0, 0)]),
        ("0x01700018 := 1", [(W.W8, 0x01700018, 1, 0)]),
        ("0x01700018 := 0 again", [(W.W8, 0x01700018, 0, 0)]),
        ("unit6 7800", [(W.W32, W.unit_reg(6, 0x7800), 0, 0)]),
        ("unit6 F00F", [(W.W32, W.unit_reg(6, 0xF00F), 0, 0)]),
        ("unit7 A000", [(W.W32, W.unit_reg(7, 0xA000), 0, 0)]),
        ("unit0 A000", [(W.W32, W.unit_reg(0, 0xA000), 0, 0)]),
        ("unit1 A000", [(W.W32, W.unit_reg(1, 0xA000), 0, 0)]),
        ("unit2 A000", [(W.W32, W.unit_reg(2, 0xA000), 0, 0)]),
        ("unit0 0038", [(W.W32, W.unit_reg(0, 0x0038), 0, 0)]),
        ("mem1 word := A000", [(W.W32, W.SOL_MEM1 + 8 * W0, 0x6000A000, 0)]),
    ]
    pokes_b = [
        ("+18 := 1", [(W.W16, CTRL + 0x18, 1, 0)]),
        ("+18 := 0", [(W.W16, CTRL + 0x18, 0, 0)]),
        ("+20 := (W0+1)*4, +18 := 0", [(W.W16, CTRL + 0x20, 4 * (W0 + 1), 0), (W.W16, CTRL + 0x18, 0, 0)]),
        (
            "+18 := 1, +20 := 4, +18 := 0",
            [(W.W16, CTRL + 0x18, 1, 0), (W.W16, CTRL + 0x20, 4, 0), (W.W16, CTRL + 0x18, 0, 0)],
        ),
    ]
    for name, word, s1, s0 in (
        ("800D (B true)", 0x800D, -1, -1),
        ("C98C (both false)", 0xC98C, -1, -1),
        ("0037 (+17 := 1)", 0x0037, -1, -1),
        ("F400", 0xF400, -1, -1),
        ("A800", 0xA800, -1, -1),
    ):
        c = flag_case(word, s1, s0)
        assert c["W0"] == W0, c["W0"]
        d = {}
        for jname, pokes in (("A", pokes_a), ("B", pokes_b)):
            job, blocks = W.sol_cases_job([c], timeout=2000)
            job += [(W.R32, CTRL + 0x18, 0, 0), (W.R32, CTRL + 0x20, 0, 0)] + [
                (W.R32, STATUS + 4 * k, 0, 0) for k in range(8)
            ]
            for _pname, cmds in pokes:
                job += cmds + [(W.POLL16, CTRL + 0x18, 1, 2000), (W.R32, CTRL + 0x20, 0, 0)]
            try:
                raw = w.run(job, blocks, timeout=90)
            except RuntimeError as e:
                d["job " + jname] = {"error": str(e)[:120]}
                print("%s job %s: runner error %s" % (name, jname, str(e)[:120]), flush=True)
                w.park()
                d["ka after error"] = ka(w)
                continue
            ((tries, reads, diffs),) = W.parse_cases([c], raw)
            k = 1 + len(c["reads"])
            k += 1 + 2 * min(raw[k], 16)
            dj = {
                "tries": tries,
                "stores": flag_decode({"tries": 1, "reads": reads})[1],
                "ctrl18": "%08x" % raw[k],
                "ctrl20": "%08x" % raw[k + 1],
                "status": " ".join("%08x" % x for x in raw[k + 2 : k + 10]),
            }
            k += 10
            resumed = None
            for pname, _cmds in pokes:
                t, st = raw[k], raw[k + 1]
                k += 2
                dj["poke " + pname] = {"tries": t, "ctrl20": "%08x" % st}
                if t and resumed is None:
                    resumed = pname
            dj["resumed_by"] = resumed
            d["job " + jname] = dj
            print(
                "%s job %s: tries %d stores [%s] +18 %s +20 %s status %s\n   resumed by: %s"
                % (name, jname, tries, dj["stores"], dj["ctrl18"], dj["ctrl20"], dj["status"], resumed),
                flush=True,
            )
            for pname, _cmds in pokes:
                p = dj["poke " + pname]
                print("   %-28s tries %5d +20 %s" % (pname, p["tries"], p["ctrl20"]))
            dj["ka_after"] = ka(w)
            if not dj["ka_after"]:
                sol_reset(w)
                dj["ka_after_sol_reset"] = ka(w)
                print("   ka failed; after Sol reset: %s" % dj["ka_after_sol_reset"])
        res[name] = d
    return res


def load_seq_u1(w, regs, n=12, access=0x7800):
    """Unit 1 registers in-slot from instruction 1 (then 0xF00F), n loads (access word) moved and stored; returns
    the loaded lane-0 and lane-1 words (raw)."""
    s = {}
    i = 1
    for r_ in regs:
        if r_ == "F":
            s.setdefault(i, {}).update(u0=0xF00F, u1=0xF00F)
        else:
            s.setdefault(i, {})["u1"] = r_
        i += 1
    for k in range(n):
        s.setdefault(i + 1 + k, {})["u1"] = access
        s.setdefault(i + 2 + k, {}).update(alu=mv_src(5), third=0x0001)
        s.setdefault(i + 4 + k, {})["u0"] = 0x60FB
    s.setdefault(i + n + 8, {})["seq"] = 0x0038
    c = case(
        *prog(i + n + 10, s),
        fill_range=(0, 0x2800),
        reads=[0x1800 + 4 * k for k in range(n)] + [0x1C00 + 4 * k for k in range(n)],
    )
    r = run(w, c)
    return r["tries"], r["reads"][:n], r["reads"][n:]


def x_augroups(w):
    """Address-unit register groups 0xD600, 0xDC00, 0xE000, 0xE800-0xEE00 on unit 1 with several values: the load
    sequence (stride 1, modulo 4), bit-reversed loads, store positions, the lane-1 wrap predicate selects 0-3, and
    a lane-1 load of 0x00008000 (the clamp)."""
    res = {}
    groups = (0xD600, 0xDC00, 0xE000, 0xE800, 0xEA00, 0xEC00, 0xEE00)
    vals = (1, 2, 4, 0x10, 0x80, 0x100, 0x1FF)

    def fmt(l0, l1):
        return (
            " ".join("%x" % (v - 0x10000800) if 0x10000800 <= v < 0x10000900 else "%08x" % v for v in l0)
            + " | "
            + " ".join("%x" % (v - 0x10000900) if 0x10000900 <= v < 0x10000A00 else "%08x" % v for v in l1)
        )

    def stores(regs):
        s = {}
        i = 1
        for r_ in regs:
            s.setdefault(i, {})["u1"] = r_
            i += 1
        s.setdefault(i, {}).update(u0=0xF00F, u1=0xF00F)
        for k in range(6):
            s.setdefault(i + 2 + k, {}).update(alu=mv_src(0), third=(1 + k % 7) << 12)
            s.setdefault(i + 4 + k, {})["u1"] = 0x60FB
        s.setdefault(i + 12, {})["seq"] = 0x0038
        c = case(*prog(i + 14, s), fill_range=(0x2000, 0x2800))
        r = run(w, c)
        return r["tries"], sorted((a - 0x2000) // 4 for a, v in (r["diffs"] or []) if a != "more")

    # registers r1..r7 for the store values
    run(w, kern(all_commits(), [(44, mv_src(0), 0x1000)]))
    t, l0, l1 = load_seq_u1(w, [0xCC01, 0xD201, 0xCA04, 0xD004, "F"])
    res["baseline loads"] = fmt(l0, l1)
    t, b0, b1 = load_seq_u1(w, [0xCC01, 0xD201, "F"], access=0x7A00)
    res["baseline bitrev"] = fmt(b0, b1)
    res["baseline stores"] = stores([])
    print(
        "baseline loads  %s\nbaseline bitrev %s\nbaseline stores %s"
        % (res["baseline loads"], res["baseline bitrev"], res["baseline stores"][1]),
        flush=True,
    )
    clamp_data = {0x2000: 0x00050007, 0x2400: 0x00008000}
    for g in groups:
        for v in vals:
            key = "%04x" % (g | v)
            d = {}
            t, l0, l1 = load_seq_u1(w, [g | v, 0xCC01, 0xD201, 0xCA04, 0xD004, "F"])
            d["loads"] = fmt(l0, l1) if t else "STALL"
            t, b0, b1 = load_seq_u1(w, [g | v, 0xCC01, 0xD201, "F"], access=0x7A00)
            d["bitrev"] = fmt(b0, b1) if t else "STALL"
            t, pos = stores([g | v])
            d["stores"] = pos if t else "STALL"
            r = run(
                w,
                kern(
                    {1: {"u1": g | v}, 2: {"u1": 0xF00F}, 4: {"u1": 0x7800}},
                    [(5, mv_src(5), 0x0001)],
                    data=clamp_data,
                    fill_range=(0x1800, 0x1C00),
                ),
            )
            d["lane1 8000"] = "%08x.%08x" % (r["reads"][1], r["reads"][0]) if r["tries"] else "STALL"
            # restore the group to 0 (the per-run clear covers only D600/DC00)
            run(w, kern({1: {"u1": g}, 2: {"u1": 0xF00F, "u0": 0xF00F}}, [(4, mv_src(0), 0x1000)]))
            res[key] = d
            changed = [
                k
                for k in ("loads", "bitrev", "stores", "lane1 8000")
                if d[k]
                != {
                    "loads": res["baseline loads"],
                    "bitrev": res["baseline bitrev"],
                    "stores": res["baseline stores"][1],
                    "lane1 8000": "00008000.00050007",
                }[k]
            ]
            print("%s: %s" % (key, ", ".join("%s %s" % (k, d[k]) for k in changed) or "no change"), flush=True)
    return res


def x_aupred(w):
    """Predicate sources 4/5 (address units 0/1) selects 1-3 with the unknown groups set, and with lane-0 /
    store-pointer wraps: which event makes them true."""
    res = {}

    def cnt(word, regs, access=0x7800, passes=12, unit="u1", st=None):
        s = {}
        for i, r_ in enumerate(regs):
            s.setdefault(1 + i, {})[unit] = r_
        s.setdefault(1 + len(regs), {}).update(u0=0xF00F, u1=0xF00F, u2=0xF00F)
        s.setdefault(2 + len(regs), {})["seq"] = imm(4, passes)
        s.setdefault(27, {})["seq"] = jump_word(27, 20, 4)
        s.setdefault(38, {})["seq"] = 0x0038
        s.setdefault(20, {})[unit] = access
        if st:
            s.setdefault(22, {})[unit] = st
        tgt = "u0" if unit != "u0" else "u1"
        s.setdefault(24, {})["seq"] = word
        s[24][tgt] = 0x60FB
        c = case(*prog(40, s), fill_range=(0x1800, 0x1C00) if tgt == "u0" else (0x2000, 0x2400))
        r = run(w, c)
        return stores_count(r) if r["tries"] else "H"

    S = [0xCC01, 0xD201]
    tests = [
        ("u1 l0 mod 3 l1 mod 2", S + [0xCA03, 0xD002], 0x7800, None),
        ("u1 l0 mod 3 l1 none", S + [0xCA03], 0x7800, None),
        ("u1 l1 mod 2, stores too", S + [0xD002], 0x7800, 0x60FB),
        ("u1 no load, stores only", [], 0x8000, 0x60FB),
        ("u1 l1 mod 2, bitrev", S + [0xD002], 0x7A00, None),
        ("u1 l1 mod 2, lane swap bit 14", S + [0xD002], 0x3800, None),
    ]
    tests = [t + (None,) for t in tests]
    for g in (0xD600, 0xDC00, 0xE000, 0xE800, 0xEA00, 0xEC00, 0xEE00):
        tests.append(("u1 l1 mod 2, %04x" % (g | 1), [g | 1] + S + [0xD002], 0x7800, None, g))
    for name, regs, acc, st, restore in tests:
        row = [cnt(0xD500 | (5 << 2) | sel, regs, acc, st=st) for sel in range(4)]
        res[name] = row
        print("%-34s true passes sel 0-3: %s" % (name, row), flush=True)
        if restore:
            run(w, kern({1: {"u1": restore}, 2: {"u1": 0xF00F, "u0": 0xF00F}}, [(4, mv_src(0), 0x1000)]))
    return res


def x_u2window(w):
    """Unit 2's window: unit 2 memory words 0..40 := their index (in-slot config), then a window write of 0xCC05
    (load stride 5) to window n, then a run that only resets unit 2's pointers and loads 8 words through source 6:
    stride 5 shows window n is unit 2. Windows 0-2 and 6-15 (3-5 are known). Also: do windows 0/1/2 take a MAC
    configuration word, a sequencer immediate (counter 0 := 0x5A)?"""
    res = {}

    def fill_u2():
        run(w, au2_case(40, 0, n_read=1))

    def read_u2(n=8):
        s = {1: {"u2": 0xF00F}}
        for k in range(n):
            s.setdefault(3 + k, {})["u2"] = 0x7800
            s.setdefault(4 + k, {}).update(alu=mv_src(6), third=0x0001)
            s.setdefault(6 + k, {})["u0"] = 0x60FB
        s.setdefault(n + 10, {})["seq"] = 0x0038
        r = run(w, case(*prog(n + 12, s), fill_range=(0x1800, 0x1C00), reads=[0x1800 + 4 * k for k in range(n)]))
        return [v - 0x10000800 if 0x10000800 <= v < 0x10000900 else v for v in r["reads"]] if r["tries"] else "STALL"

    def restore_u2():
        s = {1 + i: {"u2": v} for i, v in enumerate(AU2_CFG)}
        s[1 + len(AU2_CFG)] = {"u2": 0xF00F}
        run(w, kern(s, [(10, mv_src(0), 0x1000)]))

    fill_u2()
    res["baseline"] = read_u2()
    print("baseline unit-2 read:", res["baseline"], flush=True)
    for n in [6, 7, 0, 1, 2, 8, 9, 10, 11, 12, 13, 14, 15]:
        d = {}
        try:
            before = w.run(
                [
                    (W.R32, CTRL, 0, 0),
                    (W.R32, CTRL + 0x14, 0, 0),
                    (W.W32, W.unit_reg(n, 0xCC05), 0, 0),
                    (W.R32, CTRL, 0, 0),
                    (W.R32, CTRL + 0x14, 0, 0),
                ]
            )
            d["ctrl before/after"] = ["%08x %08x" % tuple(before[:2]), "%08x %08x" % tuple(before[2:4])]
            d["read after CC05"] = read_u2()
            restore_u2()
            d["read after restore"] = read_u2()
            if n in (0, 1, 2, 6, 7):
                aft = w.run(
                    [
                        (W.W32, W.unit_reg(n, imm(0, 0x5A)), 0, 0),
                        (W.R32, CTRL, 0, 0),
                        (W.W32, W.unit_reg(n, imm(0, 0)), 0, 0),
                        (W.R32, CTRL, 0, 0),
                    ]
                )
                d["counters after imm c0:=5a via window"] = "%08x then %08x" % (aft[0], aft[1])
            d["ka"] = ka(w)
        except RuntimeError as e:
            d["error"] = str(e)[:120]
            w.park()
            d["ka"] = ka(w)
        res["window %d" % n] = d
        print("window %2d: %s" % (n, d), flush=True)
    return res


def x_e600(w):
    """0xE600 bit 0 (lane 1's store goes outside banks 6-9): where, and does an 0xE8xx-0xEExx value move it. The
    whole 10 KB is read before and after."""
    res = {}
    before = w.read_many(0xA8000000, 0xA00)

    def store6(regs):
        s = {}
        i = 1
        for r_ in regs:
            s.setdefault(i, {})["u1"] = r_
            i += 1
        s.setdefault(i, {}).update(u0=0xF00F, u1=0xF00F)
        for k in range(6):
            s.setdefault(i + 2 + k, {}).update(alu=mv_src(0), third=(1 + k) << 12)
            s.setdefault(i + 4 + k, {})["u1"] = 0x60FB
        s.setdefault(i + 12, {})["seq"] = 0x0038
        c = case(*prog(i + 14, s), fill=0, reads=[])
        return run(w, c)["tries"]

    run(w, kern(all_commits(), [(44, mv_src(0), 0x1000)]))
    for name, regs in (
        ("E601", [0xE601]),
        ("E601 E801", [0xE601, 0xE801]),
        ("E601 EA01", [0xE601, 0xEA01]),
        ("E601 EC01", [0xE601, 0xEC01]),
        ("E601 EE01", [0xE601, 0xEE01]),
        ("E601 E010", [0xE601, 0xE010]),
        ("E601 D610", [0xE601, 0xD610]),
        ("E601 DC10", [0xE601, 0xDC10]),
        ("E601 E4 bank map 0x21", [0xE601, 0xE421]),
    ):
        t = store6(regs)
        after = w.read_many(0xA8000000, 0xA00)
        diffs = [(k * 4, after[k]) for k in range(0xA00) if after[k] != before[k]]
        res[name] = {"done": bool(t), "changed": ["%04x=%08x" % x for x in diffs[:24]], "n": len(diffs)}
        print(
            "%-26s %s %d changed: %s" % (name, "done " if t else "STALL", len(diffs), " ".join(res[name]["changed"])),
            flush=True,
        )
        run(
            w,
            kern(
                {
                    1: {"u1": 0xE600},
                    2: {"u1": 0xE800},
                    3: {"u1": 0xEA00},
                    4: {"u1": 0xEC00},
                    5: {"u1": 0xEE00},
                    6: {"u1": 0xE000},
                    7: {"u1": 0xE498},
                    8: {"u1": 0xF00F, "u0": 0xF00F},
                },
                [(10, mv_src(0), 0x1000)],
            ),
        )
        before = w.read_many(0xA8000000, 0xA00)
    return res


def x_c08full(w):
    """+0x08 := 0x300 | v for every byte v: c0 (E008 jumps) and c1 (E009 jumps)."""
    res = {}
    sol_reset(w)
    for base in range(0, 256, 4):
        vals = list(range(base, base + 4))
        cases = [flag_case(wd, -1, -1, pre=(imm(8, 0x300 | v),)) for v in vals for wd in (0xE008, 0xE009)]
        rs = run_batch(w, cases)
        for i, v in enumerate(vals):
            row = "".join(
                "H" if not r["tries"] else "T" if len(flag_decode(r)[1].split()) == 5 else "n"
                for r in rs[2 * i : 2 * i + 2]
            )
            res["%02x" % v] = row
        print(" ".join("%02x:%s" % (v, res["%02x" % v]) for v in vals), flush=True)
    sol_reset(w)
    return res


def x_ecbits(w):
    """0xE8xx / 0xECxx (and 0xE0xx) bit by bit: the stored marker sequence shows the jump distance."""
    res = {}
    sol_reset(w)
    words = []
    for cls in (0xE000, 0xE800, 0xEC00):
        words += [cls | b for b in (0, 1, 2, 4, 8, 0x10, 0x20, 0x40, 0x80, 0x100, 0x200, 0x11, 0x09)]
    for s1, s0 in ((-1, -1), (1, 1)):
        rs = run_batch(w, [flag_case(wd, s1, s0) for wd in words])
        for wd, r in zip(words, rs):
            kind, st = flag_decode(r)
            res["%04x flags %s" % (wd, sign_name(s1, s0))] = "%s [%s]" % (kind, st)
            print("%04x flags %s: %s [%s]" % (wd, sign_name(s1, s0), kind, st), flush=True)
    return res


def x_b4b8(w):
    """0xB4mk / 0xB8mk over every (m, k), counters with distinct values; also 0x8Bxx (commit with bits 9:8 set)."""
    CV = [0x11, 0x22, 0x44, 0x88, 0x0F, 0xF0, 0x3C, 0xC3]
    res = {}

    def after(word):
        s = {k + 1: {"seq": imm(k, CV[k])} for k in range(8)}
        s[12] = {"seq": word}
        s[16] = {"seq": 0x0038}
        r = job_with_reads(w, case(*prog(18, s), fill=0, reads=[]), [CTRL, CTRL + 4, CTRL + 0x20], timeout=3000)
        v = r[-3] | (r[-2] << 32)
        return r[0], [(v >> (8 * k)) & 0xFF for k in range(8)], r[-1]

    words = [cls | (m << 5) | k for cls in (0xB400, 0xB800) for m in range(8) for k in range(8)]
    words += [0x8B05, 0x8B0D, 0x8BF5, 0x8A85, 0x89C5, 0x8B00, 0x8B07]
    for wd in words:
        tries, cnt, st = after(wd)
        ch = ", ".join("c%d %02x->%02x" % (i, CV[i], cnt[i]) for i in range(8) if cnt[i] != CV[i])
        res["%04x" % wd] = {"done": bool(tries), "changed": ch, "ctrl20": "%08x" % st}
        print("%04x: %s %s  +20 %08x" % (wd, "done " if tries else "STALL", ch or "-", st), flush=True)
        if not tries:
            res["%04x" % wd]["wedged"] = recover(w, "%04x" % wd)
    return res


CMP_STATES = {
    "A": [0x00, 0x01, 0x80, 0xFF, 0x7F, 0x02, 0x40, 0x10],
    "B": [0x77, 0, 0, 0, 0, 0, 0, 0],
    "C": [0xFF] * 8,
    "D": [0x01] * 8,
}


def x_c08cmp(w):
    """The +0x08 word (idx 8, value 0x300 | v) as a compare: counters 0-7 (and 12/13 := 0x00/0xFF) set to known
    states, then every v: c0 (E008) and c1 (E009)."""
    res = {}
    sol_reset(w)
    for sname, cv in CMP_STATES.items():
        pre_base = [imm(k, cv[k]) for k in range(8)] + [imm(12, 0x00), imm(13, 0xFF)]
        rows = {}
        for base in range(0, 256, 4):
            vals = list(range(base, base + 4))
            cases = [flag_case(wd, -1, -1, pre=pre_base + [imm(8, 0x300 | v)]) for v in vals for wd in (0xE008, 0xE009)]
            rs = run_batch(w, cases)
            for i, v in enumerate(vals):
                rows["%02x" % v] = "".join(
                    "H" if not r["tries"] else "T" if len(flag_decode(r)[1].split()) == 5 else "n"
                    for r in rs[2 * i : 2 * i + 2]
                )
        res[sname] = rows
        print("state %s counters %s:" % (sname, " ".join("%02x" % x for x in cv)))
        for i in range(0, 256, 16):
            print("  %02x: " % i + " ".join(rows["%02x" % v] for v in range(i, i + 16)), flush=True)
    # bits 10:8 of the value other than 011, byte 4 (counter 4) zero vs not
    for hi in (0, 1, 2, 4, 5, 6, 7):
        for c4 in (0, 1):
            pre = [imm(4, c4), imm(8, (hi << 8) | 0x04)]
            rs = run_batch(w, [flag_case(wd, -1, -1, pre=pre) for wd in (0xE008, 0xE009, 0xE408, 0xE409)])
            row = "".join("H" if not r["tries"] else "T" if len(flag_decode(r)[1].split()) == 5 else "n" for r in rs)
            res["value %03x, c4=%d" % ((hi << 8) | 4, c4)] = row
            print("value %03x c4=%d: e008 %s e009 %s e408 %s e409 %s" % ((hi << 8) | 4, c4, *row), flush=True)
    sol_reset(w)
    return res


def x_ecwrite(w):
    """0xE8xx / 0xECxx one at a time: the control block (10 words) and the status words after; does Sol finish."""
    res = {}
    sol_reset(w)
    for wd in (0xA000, 0xE800, 0xE801, 0xE8FF, 0xE9AA, 0xEBFF, 0xEC00, 0xEC01, 0xECFF, 0xEDAA, 0xEFFF, 0xA000):
        c = flag_case(wd, -1, -1)
        r = job_with_reads(w, c, [CTRL + 4 * k for k in range(10)] + [STATUS + 4 * k for k in range(8)], timeout=3000)
        kind, st = flag_decode({"tries": r[0], "reads": r[1:13]})
        d = {
            "kind": kind,
            "stores": st,
            "ctrl": " ".join("%08x" % x for x in r[-18:-8]),
            "status": " ".join("%08x" % x for x in r[-8:]),
        }
        if not r[0]:
            d["wedged"] = recover(w, "%04x" % wd)
        res["%04x" % wd] = d
        print("%04x: %s [%s]\n   ctrl %s\n   status %s" % (wd, kind, st, d["ctrl"], d["status"]), flush=True)
    return res


def x_wedge2(w):
    """Which stall wedges Sol (needs a Sol reset) and which only needs Warp1 parked: self-loops 0x87FF and 0xE3FE,
    0xF400, 0xA800, 0x800D, each followed by an alive check."""
    res = {}
    sol_reset(w)
    for wd in (0x87FF, 0xE3FE, 0xE3FF, 0xF400, 0xA800, 0x800D, 0xC98C, 0x0037, 0x87FF):
        r = run(w, flag_case(wd, -1, -1))
        a1 = alive(w)
        a2 = alive(w)
        d = {"tries": r["tries"], "alive after park": a1, "alive 2nd": a2}
        if not a2:
            sol_reset(w)
            d["alive after Sol reset"] = alive(w)
        res["%04x" % wd] = d
        print("%04x: %s" % (wd, d), flush=True)
    return res


def x_c08ops(w):
    """Selected +0x08 values under counter states A, C, D: the stored sequence of the E008 test and the control
    block after (do the ops change counters?)."""
    res = {}
    sol_reset(w)
    vals = [
        0x00,
        0x01,
        0x04,
        0x20,
        0x21,
        0x24,
        0x40,
        0x41,
        0x44,
        0x60,
        0x61,
        0x64,
        0x80,
        0xA4,
        0xC4,
        0xE4,
        0x104,
        0x204,
        0x404,
        0x504,
        0x604,
        0x704,
        0x700,
        0x720,
    ]
    for sname in ("A", "C", "D"):
        cv = CMP_STATES[sname]
        pre_base = [imm(k, cv[k]) for k in range(8)] + [imm(12, 0x00), imm(13, 0xFF)]
        print("state %s counters %s" % (sname, " ".join("%02x" % x for x in cv)))
        for v in vals:
            word = 0x300 | v if v < 0x100 else v
            c = flag_case(0xE008, -1, -1, pre=pre_base + [imm(8, word)])
            r = job_with_reads(w, c, [CTRL + 4 * k for k in range(10)], timeout=3000)
            kind, st = flag_decode({"tries": r[0], "reads": r[1:13]})
            ctl = r[-10:]
            cnt = [(ctl[0] >> (8 * k)) & 0xFF for k in range(4)] + [(ctl[1] >> (8 * k)) & 0xFF for k in range(4)]
            ch = " ".join("c%d=%02x" % (k, cnt[k]) for k in range(8) if cnt[k] != cv[k])
            d = {
                "kind": kind,
                "stores": st,
                "counters": " ".join("%02x" % x for x in cnt),
                "changed": ch,
                "ctrl": " ".join("%08x" % x for x in ctl),
            }
            if not r[0]:
                d["wedged"] = recover(w, "value %03x" % word)
            res["%s %03x" % (sname, word)] = d
            print(
                "  value %03x: %s [%s] counters %s %s"
                % (word, kind, st, d["counters"], ("CHANGED " + ch) if ch else ""),
                flush=True,
            )
    sol_reset(w)
    return res


def x_ectrace(w):
    """0xECxx / 0xE8xx / 0xE0xx words at instruction 40 of the 127-instruction trace: where execution goes."""
    res = {}
    sol_reset(w)
    words = [
        0xE000,
        0xE008,
        0xE010,
        0xE800,
        0xE808,
        0xE810,
        0xE8FF,
        0xEC00,
        0xEC01,
        0xEC02,
        0xEC04,
        0xEC08,
        0xEC09,
        0xEC10,
        0xEC20,
        0xEC40,
        0xEC80,
        0xECFF,
        0xED00,
        0xEDAA,
        0xEE00,
        0xEE01,
    ]
    for wd in words:
        c = trace_long({40: wd}, region=(3, 122))
        r = job_with_reads(w, c, [CTRL + 0x20], timeout=5000)
        rr = {"reads": r[1:257], "tries": r[0]}
        path = trace_path(rr)
        runs = []
        for pos, _sq in path:
            if runs and pos == runs[-1][1] + 1:
                runs[-1][1] = pos
            else:
                runs.append([pos, pos])
        res["%04x" % wd] = {"done": bool(r[0]), "path": runs, "ctrl20": "%08x" % r[-1]}
        print(
            "%04x: %s path %s  +20 %08x"
            % (wd, "done " if r[0] else "STALL", " ".join("%d-%d" % tuple(x) for x in runs[:12]), r[-1]),
            flush=True,
        )
        if not r[0]:
            res["%04x" % wd]["wedged"] = recover(w, "%04x" % wd)
    return res


def x_cntalu(w):
    """Words 0x2000-0x7FFF as a counter ALU: W = cls[15:13] op[12:10] imm5[9:5] b[4:3] k[2:0] with counters
    00 01 80 ff 7f 02 40 10 (+0C = 0, +0D = ff): Z (E008 jumps), C (E009 jumps), counters changed."""
    res = {}
    sol_reset(w)
    cv = CMP_STATES["A"]
    pre_base = [imm(k, cv[k]) for k in range(8)] + [imm(12, 0x00), imm(13, 0xFF)]
    words = [
        (cls << 13) | (op << 10) | (imm5 << 5) | (b << 3) | k
        for cls in (3, 2, 1)
        for op in range(8)
        for imm5 in (1, 4)
        for b in (1, 0, 2)
        for k in (0, 1, 5)
    ]
    for i in range(0, len(words), 4):
        chunk = words[i : i + 4]
        cases = [flag_case(wd, -1, -1, pre=pre_base + [W_]) for W_ in chunk for wd in (0xE008, 0xE009)]
        cases = [c for c in cases]
        rs = run_batch(w, cases)
        for j, W_ in enumerate(chunk):
            z, cc = [
                "H" if not r["tries"] else "T" if len(flag_decode(r)[1].split()) == 5 else "n"
                for r in rs[2 * j : 2 * j + 2]
            ]
            r2 = job_with_reads(
                w, flag_case(0xA000, -1, -1, pre=pre_base + [W_]), [CTRL, CTRL + 4, CTRL + 0xC], timeout=3000
            )
            ctl = r2[-3:]
            cnt = [(ctl[0] >> (8 * kk)) & 0xFF for kk in range(4)] + [(ctl[1] >> (8 * kk)) & 0xFF for kk in range(4)]
            ch = " ".join("c%d=%02x" % (kk, cnt[kk]) for kk in range(8) if cnt[kk] != cv[kk])
            c12 = ctl[2] & 0xFFFF
            if c12 != 0xFF00:
                ch += " +0C/0D=%04x" % c12
            res["%04x" % W_] = {"Z": z, "C": cc, "changed": ch, "done": bool(r2[0])}
            print("%04x: Z %s C %s %s%s" % (W_, z, cc, ch, "" if r2[0] else " STALL"), flush=True)
            if not r2[0]:
                recover(w, "%04x" % W_)
    sol_reset(w)
    return res


def x_waitwrap(w):
    """A wait on an address-unit wrap (0xC810: until unit 0's lane-1 load pointer wraps; 0x8010: while it has)
    released by loads injected through the unit's window from Warp1."""
    res = {}
    for name, word, unit, win, setup in (
        ("C810 wait u0 wrap, 2 loads via window 4", 0xC810, "u0", 4, [0xD201, 0xD002]),
        ("C814 wait u1 wrap, 2 loads via window 5", 0xC814, "u1", 5, [0xD201, 0xD002]),
        ("C810 wait u0 wrap, window 5 (wrong unit)", 0xC810, "u0", 5, [0xD201, 0xD002]),
        ("C810 wait u0 wrap, mod 4: 2 loads then 2 more", 0xC810, "u0", 4, [0xD201, 0xD004]),
    ):
        s, at = regs_prog({6: 0x66666666})
        for i, v in enumerate(setup):
            s.setdefault(at + i, {})[unit] = v
        s.setdefault(at + 2, {}).update(u0=0xF00F, u1=0xF00F)
        W0 = at + 4
        s.setdefault(W0, {}).update(seq=word, alu=mv_src(0), third=0x6000)
        s.setdefault(W0 + 2, {})["u1" if unit == "u0" else "u0"] = 0x60FB
        s.setdefault(W0 + 4, {})["seq"] = 0x0038
        c = case(
            *prog(W0 + 6, s),
            fill_range=(0x2000, 0x2400) if unit == "u0" else (0x1800, 0x1C00),
            reads=[0x2000 if unit == "u0" else 0x1800],
        )
        job, blocks = W.sol_cases_job([c], timeout=2000)
        job += [(W.R32, CTRL + 0x20, 0, 0)]
        for _n in range(4):
            job += [
                (W.W32, W.unit_reg(win, 0x7800), 0, 0),
                (W.POLL16, CTRL + 0x18, 1, 2000),
                (W.R32, CTRL + 0x20, 0, 0),
            ]
        raw = w.run(job, blocks, timeout=60)
        ((tries, reads, diffs),) = W.parse_cases([c], raw)
        k = 1 + len(c["reads"])
        k += 1 + 2 * min(raw[k], 16)
        d = {"tries": tries, "stored": "%08x" % reads[0], "ctrl20": "%08x" % raw[k]}
        polls = [(raw[k + 1 + 2 * n], "%08x" % raw[k + 2 + 2 * n]) for n in range(4)]
        d["after each window load (tries, +20)"] = polls
        d["released_after_loads"] = next((n + 1 for n, (t, _) in enumerate(polls) if t), None)
        res[name] = d
        print(
            "%s: tries %d, released after %s window loads; polls %s" % (name, tries, d["released_after_loads"], polls),
            flush=True,
        )
        if not any(t for t, _ in polls):
            recover(w, name)
    return res


def zc_after(w, pre):
    """Z (E008 jumps) and C (E009 jumps) after the sequencer words `pre` (after the register commits)."""
    rs = run_batch(w, [flag_case(wd, -1, -1, pre=list(pre)) for wd in (0xE008, 0xE009)])
    return "".join("H" if not r["tries"] else "T" if len(flag_decode(r)[1].split()) == 5 else "n" for r in rs)


def x_scratch(w):
    """Control bytes the ARM cannot read (+0x08-0x0B, +0x0E-0x13, +0x15, +0x19-0x1F): does the byte ALU see a
    value written there? byte := 0x5A (class 0 immediate), then +0x08 ^= byte (0x7C08 | byte << 5): Z = n means
    the byte held 0x5A (0x08 itself: := 0x5A then += 0 via byte 7 (counter 7 := 0)). +0x20 status bits with
    Z/C states."""
    res = {}
    sol_reset(w)
    for b in list(range(8, 0x20)):
        if b == 8:
            pre = [imm(7, 0), imm(8, 0x5A), 0x6008 | (7 << 5)]  # +08 := 5a; +08 += c7 (0): Z = (+08 == 0)
        else:
            pre = [imm(b, 0x5A), imm(8, 0), 0x7C08 | (b << 5)]  # byte := 5a; +08 := 0; +08 ^= byte
        zc = zc_after(w, pre)
        res["byte %02x" % b] = zc
        print(
            "byte +%02x := 5a, tested through the ALU: Z %s C %s -> %s"
            % (b, zc[0], zc[1], "holds a value" if zc[0] == "n" else "reads 0" if zc[0] == "T" else "?"),
            flush=True,
        )
        if b in (0x14, 0x16, 0x17, 0x18):
            pass
    # restore the datapath enables the loop above overwrote (+14 := 5a, +16, +17 := 5a, +18 := 5a)
    sol_reset(w)
    # +0x20 status bits vs Z/C
    for name, pre in (
        ("Z=1 C=0 (c0 := 0; +08 += c0)", [imm(0, 0), 0x6008]),
        ("Z=0 C=0 (c0 := 1; +08 += c0)", [imm(0, 1), 0x6008]),
        ("Z=0 C=1 (c0 := ff; c0 += 2)", [imm(0, 0xFF), 0x6410]),
        ("Z=1 C=1 (c0 := ff; c0 += 1)", [imm(0, 0xFF), 0x6408]),
    ):
        r = job_with_reads(w, flag_case(0xA000, -1, -1, pre=pre), [CTRL + 0x20], timeout=3000)
        res["status " + name] = "%08x" % r[-1]
        print("%-32s +20 after = %08x" % (name, r[-1]), flush=True)
    return res


def x_e8flags(w):
    """0xE8xx after byte-ALU results: negative (bit 7), overflow, zero, carry: does E800/E801/E808/E809 jump."""
    res = {}
    sol_reset(w)
    words = [0xE800, 0xE801, 0xE808, 0xE809, 0xE000, 0xE001]
    for name, pre in (
        ("c0 := 0; c0 -= 5 (ff.. N=1, C=1)", [imm(0, 0), 0x6C28]),
        ("c0 := 7f; c0 += 1 (80: N=1, V=1)", [imm(0, 0x7F), 0x6408]),
        ("c0 := 80; c0 -= 1 (7f: V=1)", [imm(0, 0x80), 0x6C08]),
        ("c0 := 1; c0 -= 1 (0: Z)", [imm(0, 1), 0x6C08]),
        ("c0 := 3; c0 += 1 (4)", [imm(0, 3), 0x6408]),
        ("c0 := ff; c0 += 1 (0: Z, C)", [imm(0, 0xFF), 0x6408]),
    ):
        rs = run_batch(w, [flag_case(wd, -1, -1, pre=pre) for wd in words])
        row = ["H" if not r["tries"] else "T" if len(flag_decode(r)[1].split()) == 5 else "n" for r in rs]
        res[name] = dict(zip(["%04x" % x for x in words], row))
        print("%-36s %s" % (name, "  ".join("%04x %s" % (x, v) for x, v in zip(words, row))), flush=True)
    return res


def x_a8wait(w):
    """0xA8mk and 0xF4xx: waits on a counter? The word with counter m (or k) zero / non-zero; a stalled one
    released by Warp1 writing the counter to 0 through the control block."""
    res = {}
    sol_reset(w)
    for name, word, cv in (
        ("A861 (m=3,k=1), c3=0 c1=5", 0xA861, {3: 0, 1: 5}),
        ("A861, c3=5 c1=0", 0xA861, {3: 5, 1: 0}),
        ("A861, c3=0 c1=0", 0xA861, {3: 0, 1: 0}),
        ("A861, c3=1 c1=1", 0xA861, {3: 1, 1: 1}),
        ("A800, c0=0", 0xA800, {0: 0}),
        ("A800, c0=1", 0xA800, {0: 1}),
        ("F403 (k=3), c3=0", 0xF403, {3: 0}),
        ("F403, c3=1", 0xF403, {3: 1}),
        ("F400, c0=0 c1=0", 0xF400, {0: 0, 1: 0}),
        ("F400, c0=1", 0xF400, {0: 1}),
        ("F4FB (bits 9:3 set, k=3), c3=0", 0xF4FB, {3: 0}),
        ("A8E3 (m=7,k=3) c7=0 c3=0", 0xA8E3, {7: 0, 3: 0}),
        ("A8E3 c7=1 c3=0", 0xA8E3, {7: 1, 3: 0}),
        ("A8E3 c7=0 c3=1", 0xA8E3, {7: 0, 3: 1}),
    ):
        pre = [imm(k, v) for k, v in cv.items()]
        r = run(w, flag_case(word, -1, -1, pre=pre))
        kind, st = flag_decode(r)
        res[name] = kind
        print("%-36s %s [%s]" % (name, kind, st), flush=True)
        if kind == "H":
            recover(w, name)
    # release by the Warp: stall on A861 (c3 = 5), then W8 c3 := 0; also F403 with c3 := 0 ... and c1 := 0
    for name, word, cv, releases in (
        ("A861 stalled (c3=5, c1=5)", 0xA861, {3: 5, 1: 5}, [(3, 0), (1, 0)]),
        ("A861 stalled (c3=5, c1=5), c1 first", 0xA861, {3: 5, 1: 5}, [(1, 0), (3, 0)]),
        ("F403 stalled (c3=5)", 0xF403, {3: 5}, [(2, 0), (3, 0)]),
        ("A800 stalled (c0=5)", 0xA800, {0: 5}, [(1, 0), (0, 0)]),
    ):
        c = flag_case(word, -1, -1, pre=[imm(k, v) for k, v in cv.items()])
        job, blocks = W.sol_cases_job([c], timeout=2000)
        for k, v in releases:
            job += [(W.W8, CTRL + k, v, 0), (W.POLL16, CTRL + 0x18, 1, 2000)]
        raw = w.run(job, blocks, timeout=60)
        ((tries, reads, diffs),) = W.parse_cases([c], raw)
        k0 = 1 + len(c["reads"])
        k0 += 1 + 2 * min(raw[k0], 16)
        polls = raw[k0 : k0 + len(releases)]
        d = {"tries": tries, "release polls": polls}
        res[name] = d
        print("%-40s tries %d; after Warp writes %s: polls %s" % (name, tries, releases, polls), flush=True)
        if not any(polls):
            recover(w, name)
    return res


_SHELL = []


def module_shell(cmd, host, wait=2.0, keep=False):
    """One command on our OS's shell (telnet 2323); returns its output. keep: leave the connection open (a
    background job started here dies with the connection otherwise)."""
    import socket

    sk = socket.create_connection((host.partition(":")[0], 2323), timeout=5)
    time.sleep(0.5)
    sk.recv(65536)
    sk.sendall((cmd + "\n").encode())
    time.sleep(wait)
    out = b""
    sk.settimeout(0.5)
    try:
        while True:
            d = sk.recv(65536)
            if not d:
                break
            out += d
    except TimeoutError:
        pass
    if keep:
        _SHELL.append(sk)
    else:
        sk.close()
    return out.decode(errors="replace")


def x_dmawait(w):
    """Do 0xA8xx / 0xF4xx wait for a SuluDMA block move? wsl.ko loaded (its contexts), Sol stalled on the word,
    Warp1 runs a 1 KB block move SDRAM -> Sol bank 6 on the first context, then polls Sol's done bit."""
    import warpprobe as WP

    res = {}
    host = HOST
    drv = "/lib/modules/2.6.24.7/kernel/arch/arm/mach-sulu/drivers"
    print(
        module_shell(
            "insmod %s/warp_debugger.ko; insmod %s/wsl.ko; sleep 100000 <>/dev/wsl_admin &" % (drv, drv),
            host,
            4.0,
            keep=True,
        )
    )
    print(module_shell("sleep 1; lsmod | head -3; pidof sleep", host, 3.0))
    try:
        mbox = w.rd(0xA8000000)[0]
        if not 0x00400000 <= mbox < 0x00410000:
            raise RuntimeError("no wsl.ko mailbox: %08x" % mbox)
        d = w.rd(mbox + WP.MBOX_DESC, 6)
        ctx, client = d[0], d[1]
        res["ctx"] = [ctx, client]
        print("mailbox %08x: context %d client %d" % (mbox, ctx, client), flush=True)
        pat = [0x0D000000 + i for i in range(256)]
        w.write_words([(WP.DMA_SRC + W.UNCACHED + 4 * i, v) for i, v in enumerate(pat)])
        for word in (0xA800, 0xF400, 0x800D, 0xA000):
            c = flag_case(word, -1, -1)
            job, blocks = W.sol_cases_job([c], timeout=2000)
            job += [(W.R32, CTRL + 0x20, 0, 0)]
            raw = w.snippet(
                WP.move_snippet(ctx, client, WP.DMA_SRC, WP.DMA_SOL, 256),
                job_before=job,
                job_after=[
                    (W.POLL16, CTRL + 0x18, 1, 4000),
                    (W.R32, CTRL + 0x20, 0, 0),
                    (W.R32, 0x1800, 0, 0),
                    (W.R32, 0x18FC, 0, 0),
                ],
                blocks=blocks,
                at=W.code_slot("dmawait p5") - 0x4000,
            )
            ((tries, reads, diffs),) = W.parse_cases([c], raw)
            k = 1 + len(c["reads"])
            k += 1 + 2 * min(raw[k], 16)
            d = {
                "tries": tries,
                "ctrl20 stalled": "%08x" % raw[k],
                "dma polls": raw[k + 1],
                "tries after dma": raw[k + 2],
                "ctrl20 after": "%08x" % raw[k + 3],
                "bank6 first/last": "%08x %08x" % (raw[k + 4], raw[k + 5]),
            }
            res["%04x" % word] = d
            print("%04x: %s" % (word, d), flush=True)
            if not raw[k + 2]:
                recover(w, "%04x dma" % word)
    finally:
        print(module_shell("kill $(pidof sleep); sleep 1; rmmod wsl; rmmod warp_debugger; lsmod | head -2", host, 4.0))
        for sk in _SHELL:
            sk.close()
    return res


def x_loose(w):
    """Loose ends: +0x20 bit 16 (a third flag? after a negative result), 0xF400 / 0xA800 under Z/C states, the
    commit words with bits 9:3 set (0x8BFD) and the flags."""
    res = {}
    sol_reset(w)
    for name, pre in (
        ("c0 := 0; c0 -= 1 (ff: N)", [imm(0, 0), 0x6C08]),
        ("c0 := 1; c0 -= 1 (0: Z)", [imm(0, 1), 0x6C08]),
        ("c0 := 7f; c0 += 1 (80: N, V)", [imm(0, 0x7F), 0x6408]),
        ("c0 := 1; c0 += 1 (2)", [imm(0, 1), 0x6408]),
        ("c0 := 1; +08 += c0; 8BFD", [imm(0, 1), 0x6008, 0x8BFD]),
        ("c0 := 1; +08 += c0; 8805", [imm(0, 1), 0x6008, 0x8805]),
        ("c0 := 1; +08 += c0; 8905", [imm(0, 1), 0x6008, 0x8905]),
        ("c0 := 1; +08 += c0; 8A05", [imm(0, 1), 0x6008, 0x8A05]),
        ("c0 := 1; +08 += c0; 8B05", [imm(0, 1), 0x6008, 0x8B05]),
    ):
        r = job_with_reads(w, flag_case(0xA000, -1, -1, pre=pre), [CTRL + 0x20], timeout=3000)
        zc = zc_after(w, pre)
        res[name] = {"ctrl20": "%08x" % r[-1], "ZC": zc}
        print("%-34s +20 %08x  Z %s C %s" % (name, r[-1], zc[0], zc[1]), flush=True)
    for word in (0xF400, 0xA800):
        for name, pre in (
            ("Z=0 C=0", [imm(0, 1), 0x6008]),
            ("Z=0 C=1", [imm(0, 0xFF), 0x6410]),
            ("Z=1 C=1", [imm(0, 0xFF), 0x6408]),
            ("Z=1 C=0", [imm(0, 0), 0x6008]),
        ):
            r = run(w, flag_case(word, -1, -1, pre=pre))
            kind, st = flag_decode(r)
            res["%04x %s" % (word, name)] = kind
            print("%04x with %s: %s" % (word, name, kind), flush=True)
            if kind == "H":
                recover(w, "%04x" % word)
    return res


PROBES = {  # name -> (pass, function): a set returns (readout, cases); an experiment runs on the Warp itself
    "agu": ("sets", set_agu),
    "alu": ("sets", set_alu),
    "mac": ("sets", set_mac),
    "loop": ("sets", set_loop),
    "seq": ("sets", set_seq),
    "macmode": ("sets", set_macmode),
    "program": ("sets", set_program),
    "nokia": ("sets", set_nokia),
    "loops2": ("sets", set_loops2),
    "immfmt": ("sets", set_immfmt),
    "unit2b": ("sets", set_unit2b),
    "macshift": ("sets", set_macshift),
    "seqwords": ("sets", set_seqwords),
    "queues": ("sets", set_queues),
    "alu2": ("sets", set_alu2),
    "ctrl": ("sets", set_ctrl),
    "loops3": ("sets", set_loops3),
    "unit2c": ("sets", set_unit2c),
    "loops4": ("sets", set_loops4),
    "macneg": ("sets", set_macneg),
    "macacc": ("sets", set_macacc),
    "seqreg": ("sets", set_seqreg),
    "queues2": ("sets", set_queues2),
    "alu3": ("sets", set_alu3),
    "trace": ("sets", set_trace),
    "seqreg2": ("sets", set_seqreg2),
    "macacc2": ("sets", set_macacc2),
    "regs": ("sets", set_regs),
    "regwr": ("sets", set_regwr),
    "srcs": ("sets", set_srcs),
    "src7": ("sets", set_src7),
    "lanes": ("sets", set_lanes),
    "cond": ("sets", set_cond),
    "cond2": ("sets", set_cond2),
    "condbits": ("sets", set_condbits),
    "condsweep": ("sets", set_condsweep),
    "condcombine": ("sets", set_condcombine),
    "flagops": ("sets", set_flagops),
    "counters": ("sets", set_counters),
    "counters2": ("sets", set_counters2),
    "jumps": ("sets", set_jumps),
    "imm": ("sets", set_imm),
    "au2": ("sets", set_au2),
    "macpipe": ("sets", set_macpipe),
    "maccfg": ("sets", set_maccfg),
    "alu4": ("sets", set_alu4),
    "clamp": ("sets", set_clamp),
    "jumpslots": ("sets", set_jumpslots),
    "jumpslots2": ("sets", set_jumpslots2),
    "macpred": ("sets", set_macpred),
    "p4-entry": ("p4", x_entry),
    "p4-pointer": ("p4", x_pointer),
    "p4-auwrap": ("p4", x_auwrap),
    "p4-src67": ("p4", x_src67),
    "p4-simd": ("p4", x_simd),
    "p4-ctrl": ("p4", x_ctrl),
    "p4-ctrl17": ("p4", x_ctrl17),
    "p4-wedge": ("p4", x_wedge),
    "p4-cond08": ("p4", x_cond08),
    "p4-seqwords": ("p4", x_seqwords),
    "p4-counterops": ("p4", x_counterops),
    "p4-call": ("p4", x_call),
    "p4-icache": ("p4", x_icache),
    "p4-dma0": ("p4", x_dma0),
    "p5-cwords": ("p5", x_cwords),
    "p5-ejumps": ("p5", x_ejumps),
    "p5-c08bits": ("p5", x_c08bits),
    "p5-counterwords": ("p5", x_counterwords),
    "p5-ret": ("p5", x_ret),
    "p5-ctrlbits": ("p5", x_ctrlbits),
    "p5-stallpoke": ("p5", x_stallpoke),
    "p5-augroups": ("p5", x_augroups),
    "p5-aupred": ("p5", x_aupred),
    "p5-u2window": ("p5", x_u2window),
    "p5-e600": ("p5", x_e600),
    "p5-c08full": ("p5", x_c08full),
    "p5-ecbits": ("p5", x_ecbits),
    "p5-b4b8": ("p5", x_b4b8),
    "p5-c08cmp": ("p5", x_c08cmp),
    "p5-ecwrite": ("p5", x_ecwrite),
    "p5-wedge2": ("p5", x_wedge2),
    "p5-c08ops": ("p5", x_c08ops),
    "p5-ectrace": ("p5", x_ectrace),
    "p5-cntalu": ("p5", x_cntalu),
    "p5-waitwrap": ("p5", x_waitwrap),
    "p5-scratch": ("p5", x_scratch),
    "p5-e8flags": ("p5", x_e8flags),
    "p5-a8wait": ("p5", x_a8wait),
    "p5-dmawait": ("p5", x_dmawait),
    "p5-loose": ("p5", x_loose),
}
HOST = None


def main():
    global HOST
    if sys.argv[1:] == ["--list"]:
        for p in ("sets", "p4", "p5"):
            print("%s: %s" % (p, " ".join(n for n, (q, _) in PROBES.items() if q == p)))
        return
    if len(sys.argv) != 4 or not all(n in PROBES for n in sys.argv[2].split(",")):
        sys.exit(__doc__)
    HOST, names, out = sys.argv[1], sys.argv[2].split(","), sys.argv[3]
    os.makedirs(out, exist_ok=True)
    w = W.Warp(HOST)
    try:
        for name in names:
            print("=== %s" % name, flush=True)
            t0 = time.time()
            p, f = PROBES[name]
            res = run_set(w, *f()) if p == "sets" else f(w)
            path = os.path.join(out, name + ".json")
            json.dump(res, open(path, "w"), indent=1, default=str)
            print(
                "=== %s done in %.0f s -> %s (wedges so far: %d)" % (name, time.time() - t0, path, len(WEDGES)),
                flush=True,
            )
    finally:
        w.dbg = W.WARP1
        w.park()
        w.close()


if __name__ == "__main__":
    main()
