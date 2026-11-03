#!/usr/bin/env python3
"""Warp core probes over the KnOS shell (docs/module/warp.md, section 9): RAM and registers only.

python3 -I warpprobe.py HOST cachemap [OUT.json]    Warp0: COP3 register 0, then reads at candidate cache
                                                    RAM/tag addresses with COP3 bit 4 set (one run each: a hang
                                                    is caught by the runner's timeout and parked)
python3 -I warpprobe.py HOST contention [OUT.json]  Warp0 load timing (AV CTR) alone, with Warp1 busy in its
                                                    local RAM, and with Warp1 copying SDRAM (uncached, then
                                                    with the I-cache on)
python3 -I warpprobe.py HOST cachemap2 [OUT.json]   Warp0: run a loop with the I-cache on, then read the cache
                                                    data RAM and tags through COP3 bit 4
python3 -I warpprobe.py HOST dma [OUT.json]         Warp1 block moves (1 KB each way, SDRAM <-> Sol data memory)
                                                    through the two contexts wsl.ko makes (see dma(): wsl.ko
                                                    loaded and /dev/wsl_admin held open on the module)
python3 -I warpprobe.py HOST contention2 [LABEL [OUT.json]] [--warp1-spin]
                                                    Warp0 SDRAM load cost by wall clock (under our OS, where
                                                    AV CTR stops); other loads started by hand
python3 -I warpprobe.py HOST clock [OUT.json]       both cores' clock (cached register loop, wall clock)
"""

import json
import os
import sys
import time

sys.path.insert(
    0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "..", "tools", "zeppelin", "warp")
)
import warprun as W  # noqa: E402

MTC3_T0 = ".word 0x4c880000"  # mtc3 $8, $0
MFC3_T0 = ".word 0x4c080000"  # mfc3 $8, $0
RESULT = "sw $8, 0($17)\n addiu $17, $17, 4\n addiu $18, $18, 1\n"
AVCTR = 0xE001B004

_fresh = [0x61FD9000]


def fresh():
    """A new 4 KB code address per snippet: the Warp0 cache is not invalidated by a reset."""
    a = _fresh[0]
    _fresh[0] += 0x1000
    return a


def cop3_read(w):
    return w.snippet(" %s\n nop\n %s jr $31\n nop\n" % (MFC3_T0, RESULT), at=fresh())[0]


def read_with_cop3(w, cop3, addr, n=4):
    """COP3 = cop3, read n words from addr (results), COP3 = 0."""
    asm = " li $8, %d\n %s\n nop\n nop\n lui $9, 0x%04x\n ori $9, $9, 0x%04x\n" % (
        cop3,
        MTC3_T0,
        addr >> 16,
        addr & 0xFFFF,
    )
    for i in range(n):
        asm += " lw $8, %d($9)\n nop\n %s" % (4 * i, RESULT)
    asm += " li $8, 0\n %s\n nop\n nop\n jr $31\n nop\n" % MTC3_T0
    return w.snippet(asm, at=fresh(), timeout=15)


def cachemap(host, out):
    w = W.Warp(host, core=0)
    res = []
    try:
        v = cop3_read(w)
        print("Warp0 COP3 reg 0 = %#x" % v)
        res.append({"label": "cop3", "value": v})
        for cop3, addr in (
            (0x10, 0x00010000),
            (0x10, 0x02000000),
            (0x10, 0x0000C000),
            (0x10, 0x00020000),
            (0x10, 0x02000200),
            (0x00, 0x02000000),
            (0x10, 0x00014000),
        ):
            label = "cop3 %#x read %#010x" % (cop3, addr)
            try:
                r = read_with_cop3(w, cop3, addr)
                print("%-32s %s" % (label, " ".join("%08x" % x for x in r)))
                res.append({"label": label, "words": r})
            except RuntimeError as e:
                print("%-32s HANG: %s" % (label, e))
                res.append({"label": label, "hang": str(e)})
                w.park()
                time.sleep(1)
        v = cop3_read(w)
        print("Warp0 COP3 reg 0 after = %#x" % v)
        res.append({"label": "cop3 after", "value": v})
    finally:
        w.park()
        w.close()
    if out:
        json.dump(res, open(out, "w"), indent=1)


LOADS = 64
BUSY_LOCAL = 0x61FD8000  # Warp1 code: spin in registers + local RAM loads (no SDRAM traffic)
BUSY_SDRAM = 0x61FD8800  # Warp1 code: copy SDRAM 0x61FE0000 -> 0x61FE8000, 4 KB, forever
TIMING_AT = 0x61FDA000  # Warp0 timing code (fixed: the same code every run, cache off)


def timing_snippet(w, cached=False):
    """Warp0: AV CTR delta around LOADS dependent loads from SDRAM (0x61FE4000), and around a register loop."""
    asm = " lui $9, 0x61fe\n ori $9, $9, 0x4000\n lui $11, 0xe001\n ori $11, $11, 0xb004\n"
    if cached:
        asm += " li $8, 5\n %s\n nop\n nop\n" % MTC3_T0
    asm += " lw $12, 0($11)\n nop\n"
    for i in range(LOADS):
        asm += " lw $10, %d($9)\n nop\n" % (4 * (i % 256))
    asm += " lw $13, 0($11)\n nop\n subu $8, $13, $12\n" + RESULT
    asm += " lw $12, 0($11)\n nop\n li $10, 256\n1: addiu $10, $10, -1\n bne $10, $0, 1b\n nop\n"
    asm += " lw $13, 0($11)\n nop\n subu $8, $13, $12\n" + RESULT
    if cached:
        asm += " li $8, 0\n %s\n nop\n nop\n" % MTC3_T0
    asm += " jr $31\n nop\n"
    return w.snippet(asm, at=TIMING_AT + (0x800 if cached else 0), timeout=20)


def start_busy(w1, which):
    """Start Warp1 on a busy loop (no runner); park() stops it."""
    if which == "local":
        asm = " li $9, 0x100\n1: lw $10, 0($9)\n nop\n lw $10, 4($9)\n nop\n b 1b\n nop\n"
        at = BUSY_LOCAL
    else:
        asm = (
            " lui $9, 0x61fe\n1: ori $10, $9, 0x0000\n ori $11, $9, 0x8000\n li $12, 1024\n"
            "2: lw $13, 0($10)\n addiu $10, $10, 4\n sw $13, 0($11)\n addiu $11, $11, 4\n addiu $12, $12, -1\n"
            " bne $12, $0, 2b\n nop\n b 1b\n nop\n"
        )
        at = BUSY_SDRAM
    code = W.assemble("    .set noreorder\n    .set noat\n    .text\n    .globl _start\n_start:\n" + asm, at)
    w1.halt()
    w1.write_block(at, code)
    w1.start(at)


def contention(host, out):
    w0 = W.Warp(host, core=0)
    res = []
    try:
        for phase in ("alone", "warp1 local", "warp1 sdram copy", "alone again"):
            w1 = None
            if phase.startswith("warp1"):
                w1 = W.Warp(host, core=1) if False else None  # one shell session: reuse w0's shell
                w1 = W.Warp.__new__(W.Warp)
                w1.sh, w1.verbose, w1.dbg, w1.chunk = w0.sh, False, 0xE0015000, w0.chunk
                start_busy(w1, "local" if "local" in phase else "sdram")
            runs = []
            for _i in range(3):
                r = timing_snippet(w0)
                runs.append(r)
            for _i in range(2):
                runs.append(timing_snippet(w0, cached=True))
            if w1:
                st = w1.inspect()
                w1.park()
            else:
                st = None
            print(
                "%-18s loads x%d: %s ticks; reg loop x256: %s ticks (last two: I-cache on)%s"
                % (
                    phase,
                    LOADS,
                    [r[0] for r in runs],
                    [r[1] for r in runs],
                    "" if not st else " (warp1 was at pc %08x)" % st.get("pc", 0),
                )
            )
            res.append({"phase": phase, "runs": runs, "warp1": st})
    finally:
        w0.park()
        w0.close()
    if out:
        json.dump(res, open(out, "w"), indent=1)


def cachemap2(host, out):
    """Warp0: I-cache on, a 64-instruction loop runs 4 times (fills 16 lines), I-cache off; then COP3 = 0x10 and
    the cache data RAM (0x10000) and tags (0x02000000) are read: the data RAM should hold the loop's code."""
    w = W.Warp(host, core=0)
    res = {}
    try:
        at = fresh()
        asm = " li $8, 5\n %s\n nop\n nop\n li $10, 4\n1:" % MTC3_T0
        asm += " addiu $9, $9, 1\n" * 60
        asm += " addiu $10, $10, -1\n bne $10, $0, 1b\n nop\n li $8, 0\n %s\n nop\n nop\n jr $31\n nop\n" % MTC3_T0
        w.snippet(asm, at=at, timeout=15)
        res["loop_at"] = at
        words = []
        for off in range(0, 0x1000, 0x400):
            words += read_with_cop3(w, 0x10, 0x10000 + off, 8)
        res["data"] = words
        print("cache data RAM (COP3 0x10) at 0x10000 + 0/0x400/0x800/0xC00:")
        for i in range(0, 32, 8):
            print("  " + " ".join("%08x" % x for x in words[i : i + 8]))
        tags = read_with_cop3(w, 0x10, 0x02000000, 8) + read_with_cop3(w, 0x10, 0x02000200, 8)
        res["tags"] = tags
        print("tags 0x02000000: %s" % " ".join("%08x" % x for x in tags[:8]))
        print("tags 0x02000200: %s" % " ".join("%08x" % x for x in tags[8:]))
        # the loop's code as it is in SDRAM, for comparison
        res["code"] = w.rd(at + W.UNCACHED, 8)
        print("code at %#x: %s" % (at, " ".join("%08x" % x for x in res["code"])))
    finally:
        w.park()
        w.close()
    if out:
        json.dump(res, open(out, "w"), indent=1)


MBOX_DESC = 0x3C  # wsl.ko's SRAM mailbox: two block-move descriptors {ctx, client, handle} from +0x3C
DMA_SRC, DMA_SOL, DMA_WORDS = 0x61FE0000, 0xA8001800, 256  # SDRAM buffer, Sol bank 6 (bus addresses), 1 KB


def move_snippet(ctx, client, src, dst, n):
    """Warp1 code for a SuluDMA block move as Nokia's WarpSol does it (blob 0x4BA0 / 0x4C44): ctx +0x0C = source,
    +0x08 = destination, start word n | client << 16 at 0xB5008400 + ctx * 4, then poll 0xB5000000 bit `client`.
    Result: the number of polls until the bit cleared."""
    c = 0xB5004000 + ctx * 0x40
    return """
    lui $11, 0x%04x
    ori $11, $11, 0x%04x
    lui $12, 0x%04x
    ori $12, $12, 0x%04x
    sw $12, 0x0c($11)
    lui $12, 0x%04x
    ori $12, $12, 0x%04x
    sw $12, 0x08($11)
    lui $8, 0xb500
    lui $11, 0xb500
    ori $11, $11, 0x%04x
    lui $12, 0x%04x
    ori $12, $12, 0x%04x
    addu $10, $0, $0
    sw $12, 0($11)
1:  lw $13, 0($8)
    addiu $10, $10, 1
    andi $13, $13, 0x%x
    bne $13, $0, 1b
    nop
    sw $10, 0($17)
    addiu $17, $17, 4
    jr $31
    addiu $18, $18, 1
""" % (c >> 16, c & 0xFFFF, src >> 16, src & 0xFFFF, dst >> 16, dst & 0xFFFF, 0x8400 + ctx * 4, client, n, 1 << client)


def dma(host, out):
    """Warp1 block moves through the contexts wsl.ko creates. Needs, on the module (our OS): `insmod
    warp_debugger.ko; insmod wsl.ko` (from /lib/modules/2.6.24.7/kernel/arch/arm/mach-sulu/drivers) and
    /dev/wsl_admin held open (`(sleep 100000 <>/dev/wsl_admin &)`): the open makes two block-move contexts
    (newdma_blockmove_create) and puts their {ctx, client} in the SRAM mailbox whose address is Warp1 local word
    0. 1 KB SDRAM -> Sol bank 6 on the first, 1 KB Sol bank 7 -> SDRAM on the second. Afterwards: kill the
    holder, `rmmod wsl warp_debugger`."""
    w = W.Warp(host, core=1)
    res = {}
    try:
        mbox = w.rd(0xA8000000)[0]
        if not 0x00400000 <= mbox < 0x00410000:
            raise RuntimeError("no wsl.ko mailbox (Warp1 local word 0 = %08x): load wsl.ko, hold /dev/wsl_admin" % mbox)
        d = w.rd(mbox + MBOX_DESC, 6)
        (c0, k0), (c1, k1) = (d[0], d[1]), (d[3], d[4])
        res["mailbox"], res["descriptors"] = mbox, [[c0, k0], [c1, k1]]
        res["contexts_before"] = [w.rd(0xB5004000 + c * 0x40, 11) for c in (c0, c1)]
        print("mailbox %08x: ctx %d client %d, ctx %d client %d" % (mbox, c0, k0, c1, k1))
        pat = [0x5A000000 + i for i in range(DMA_WORDS)]
        w.write_words([(DMA_SRC + W.UNCACHED + 4 * i, v) for i, v in enumerate(pat)])
        r = w.snippet(
            move_snippet(c0, k0, DMA_SRC, DMA_SOL, DMA_WORDS),
            at=W.SNIPPET,
            job_before=[(W.FILL, 0x1800, 0x11110000, DMA_WORDS)],
            job_after=[(W.R32, 0x1800 + 4 * i, 0, 0) for i in range(DMA_WORDS)],
        )
        ok = r[1:] == pat
        print("SDRAM -> Sol, %d words: %d polls, %s" % (DMA_WORDS, r[0], "copied" if ok else "MISMATCH"))
        res["to_sol"] = {"polls": r[0], "copied": ok, "got": r[1:9]}
        pat2 = [0x7B000000 + i for i in range(DMA_WORDS)]
        w.write_words([(DMA_SRC + 0x2000 + W.UNCACHED + 4 * i, 0) for i in range(DMA_WORDS)])
        r = w.snippet(
            move_snippet(c1, k1, DMA_SOL + 0x400, DMA_SRC + 0x2000, DMA_WORDS),
            at=W.SNIPPET + 0x400,
            job_before=[(W.FILL, 0x1C00, 0x7B000000, DMA_WORDS)],
        )
        got = w.rd(DMA_SRC + 0x2000 + W.UNCACHED, DMA_WORDS)
        ok = got == pat2
        print("Sol -> SDRAM, %d words: %d polls, %s" % (DMA_WORDS, r[0], "copied" if ok else "MISMATCH"))
        res["from_sol"] = {"polls": r[0], "copied": ok, "got": got[:8]}
        res["contexts_after"] = [w.rd(0xB5004000 + c * 0x40, 11) for c in (c0, c1)]
        res["busy"] = w.rd(0xB5000000, 4)
    finally:
        w.park()
        w.close()
    if out:
        json.dump(res, open(out, "w"), indent=1)


CONT_SPIN = 0x61FDC000  # Warp1: a 2-instruction loop fetched uncached from SDRAM
CONT_ITERS = 5000000  # x 4 loads: ~3.6 s alone


def contention2(host, out, label="alone", warp1_spin=False, runs=3):
    """Warp0 SDRAM load cost by wall clock (AV CTR does not run under our OS): 20 M loads from SDRAM (I-cache on,
    4 loads per iteration), timed from the Mac by polling the runner's DONE word every 20 ms; cycles per load =
    time x 160 MHz / 20 M (+-1 %; `clock` checks the 160 MHz). Any other load (ARM `dd if=/dev/zero of=/dev/null
    bs=1M`, Ethernet traffic) is started by hand on the module; `warp1_spin` runs Warp1 uncached meanwhile."""
    w = W.Warp(host, core=0)
    res = []
    try:
        if warp1_spin:
            spin = W.assemble(
                "    .set noreorder\n    .text\n    .globl _start\n_start:\n    .word 0x4c800000\n1:  b 1b\n    nop\n",
                CONT_SPIN,
            )
            w.write_block(CONT_SPIN, spin)
            w.dbg = 0xE0015000
            w.halt()
            w.start(CONT_SPIN)
            w.dbg = 0xE0014000
        asm = (
            " li $8, 5\n %s\n nop\n nop\n lui $9, 0x61fe\n lui $10, 0x%04x\n ori $10, $10, 0x%04x\n"
            "1: lw $11, 0($9)\n lw $12, 4($9)\n lw $13, 8($9)\n lw $14, 12($9)\n addiu $10, $10, -1\n"
            " bne $10, $0, 1b\n nop\n jr $31\n nop\n" % (MTC3_T0, CONT_ITERS >> 16, CONT_ITERS & 0xFFFF)
        )
        res = [{"seconds": t, "cycles_per_load": t * 160e6 / (4 * CONT_ITERS)} for t in w.timed_call(asm, runs)]
        print("%-28s %s cycles/load" % (label, " ".join("%.1f" % r["cycles_per_load"] for r in res)))
    finally:
        w.park()
        if warp1_spin:
            w.dbg = 0xE0015000
            w.park()
        w.close()
    if out:
        json.dump({"label": label, "warp1_spin": warp1_spin, "runs": res}, open(out, "w"), indent=1)


def clock(host, out):
    """Both cores' clock by wall clock: 99 M cached register instructions (33 per iteration, 3 M iterations)."""
    iters = 3000000
    asm = (
        " li $8, 5\n %s\n nop\n nop\n lui $10, 0x%04x\n ori $10, $10, 0x%04x\n1:"
        % (MTC3_T0, iters >> 16, iters & 0xFFFF)
        + " addiu $11, $11, 1\n" * 30
        + " addiu $10, $10, -1\n bne $10, $0, 1b\n nop\n jr $31\n nop\n"
    )
    res = {}
    for core in (0, 1):
        w = W.Warp(host, core=core)
        try:
            ts = w.timed_call(asm, 2)
        finally:
            w.park()
            w.close()
        res["warp%d" % core] = [33 * iters / t / 1e6 for t in ts]
        print("Warp%d: %s MHz" % (core, " ".join("%.1f" % m for m in res["warp%d" % core])))
    if out:
        json.dump(res, open(out, "w"), indent=1)


if __name__ == "__main__":
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    if sys.argv[2] == "contention2":
        args = sys.argv[3:]
        spin = "--warp1-spin" in args
        args = [a for a in args if a != "--warp1-spin"]
        contention2(
            sys.argv[1], args[1] if len(args) > 1 else None, label=args[0] if args else "alone", warp1_spin=spin
        )
    else:
        {"cachemap": cachemap, "contention": contention, "cachemap2": cachemap2, "dma": dma, "clock": clock}[
            sys.argv[2]
        ](sys.argv[1], sys.argv[3] if len(sys.argv) > 3 else None)
