#!/usr/bin/env python3
"""A Warp core's memory and multiplier, timed on the unit (docs/module/warp.md section 3): what a load and a store
cost from its local RAM and from SDRAM, whether code runs from the local RAM and how fast it is fetched there, and the
multiplier's latency. Wall clock over long loops (the AV cycle timer reads 0 under our OS), through zd1-memsh.
Warp0 by default; `--core 1` for Warp1, whose local RAM is Sol's 10 KB data memory (do not run it during playback).

  python3 -I w0probe.py HOST[:PORT] [--core 1] mem | exec | mul | sram | map | cop3 | cachebit | localcache | tags |
                                             delay | odd | bus | bit0 | lru | tagbits | bigloop | unaligned | all
  (cop3 ... bus: the cache, the load delay, non-MIPS-I instructions and bus timing, 2026-10-25)
  (`map`, Warp1 only: which of 0x0000-0xFFFF is RAM; Warp0 hangs on its 0xC000 and up)
"""

import os
import struct
import sys
import time

sys.path.insert(
    0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "..", "tools", "zeppelin", "warp")
)
import warprun as W  # noqa: E402

MARK = 0x61FE6000  # SDRAM word the local-RAM code sets when done
LOCAL_ARM = {0: 0xAC000000, 1: 0xA8000000}  # the local RAM (Warp address 0) as the ARM sees it


def loop(body, n, setup=""):
    """A counted loop around `body`, cache on, as position-independent code for Warp.timed_call."""
    return (
        " li $8, 5\n .word 0x4c880000\n nop\n nop\n li $12, 0x12345\n li $13, 0x6789a\n"
        + setup
        + f" lui $10, 0x{n >> 16:04x}\n ori $10, $10, 0x{n & 0xFFFF:04x}\n"
        "1:\n" + body + " addiu $10, $10, -1\n bnez $10, 1b\n nop\n jr $31\n nop\n"
    )


def mem(w):
    """Loads and stores from local RAM (Warp 0x100) and SDRAM, one per iteration of a 5-instruction loop."""
    n = 20_000_000
    at = lambda hi, lo: f" lui $9, 0x{hi:04x}\n ori $9, $9, 0x{lo:04x}\n"
    for name, setup, body in (
        ("registers only", at(0, 0x100), " nop\n nop\n"),
        ("local load", at(0, 0x100), " lw $11, 0($9)\n nop\n"),
        ("local 4 loads", at(0, 0x100), " lw $11, 0($9)\n lw $12, 4($9)\n lw $13, 8($9)\n lw $14, 12($9)\n"),
        ("local store", at(0, 0x200), " sw $10, 0($9)\n nop\n"),
        ("SDRAM load", at(0x61FE, 0x4000), " lw $11, 0($9)\n nop\n"),
        ("SDRAM store", at(0x61FE, 0x4100), " sw $10, 0($9)\n nop\n"),
    ):
        t = min(w.timed_call(loop(body, n, setup), 2))
        print(f"{name:15s} {t * 160e6 / n:6.2f} cycles per iteration (5 instructions; 4 loads: 7)", flush=True)


def sram(w):
    """Loads from the on-chip SRAM (ARM 0x00400000, Warp 0x04000000; read only: Linux keeps buffers there)."""
    n = 5_000_000
    for name, lo in (("SRAM load 0x04000100", 0x0100), ("SRAM load 0x0400F000", 0xF000)):
        setup = f" lui $9, 0x0400\n ori $9, $9, 0x{lo:04x}\n"
        t = min(w.timed_call(loop(" lw $11, 0($9)\n nop\n", n, setup), 2))
        print(f"{name:22s} {t * 160e6 / n:6.2f} cycles per iteration (5 instructions; 1-cycle load: 5)", flush=True)


def localmap(w):
    """Warp1: a tag (0x5A000000 | address) stored at every 1 KB of 0x0000-0xFFFF in order, then all read back."""
    out = 0x61FE7000
    asm = (
        " li $8, 5\n .word 0x4c880000\n nop\n nop\n lui $10, 0x61fe\n ori $10, $10, 0x7000\n lui $12, 1\n"
        " li $9, 0\n1: lui $11, 0x5a00\n or $11, $11, $9\n sw $11, 0($9)\n addiu $9, $9, 0x400\n"
        " bne $9, $12, 1b\n nop\n li $9, 0\n2: lw $11, 0($9)\n nop\n sw $11, 0($10)\n addiu $10, $10, 4\n"
        " addiu $9, $9, 0x400\n bne $9, $12, 2b\n nop\n jr $31\n nop\n"
    )
    w.snippet(asm, at=0x61FC7800, timeout=10)
    words = w.rd(out + W.UNCACHED, 64)
    for i in range(0, 64, 8):
        print(" ".join(f"{(i + k) * 0x400:04x}:{v:08x}" for k, v in enumerate(words[i : i + 8])), flush=True)


def _run_local(w, fn, at_local, cop3, slot):
    """Copy `fn` into local RAM at `at_local`, call it from the runner with COP3 = cop3; seconds until MARK = 1."""
    words = struct.unpack("<%dI" % (len(fn) // 4), fn)
    w.write_words([(LOCAL_ARM[w.core] + at_local + 4 * i, v) for i, v in enumerate(words)])
    asm = (
        f" li $8, {cop3}\n .word 0x4c880000\n nop\n nop\n addiu $29, $29, -8\n sw $31, 0($29)\n"
        f" lui $25, 0\n ori $25, $25, 0x{at_local:04x}\n jalr $25\n nop\n lw $31, 0($29)\n nop\n"
        " addiu $29, $29, 8\n jr $31\n nop\n"
    )
    w.wr(MARK + W.UNCACHED, 0)
    w.halt()
    if not getattr(w, "loaded", False):
        w.write_block(W.BASE, W.build_runner())
        w.loaded = True
    at = W.SNIPPET + slot
    w.write_block(
        at, W.assemble("    .set noreorder\n    .set noat\n    .text\n    .globl _start\n_start:\n" + asm, at)
    )
    job = [x for cmd in [(W.CALL, at, 0, 0), (W.END, 0, 0, 0)] for x in cmd]
    w.write_block(W.JOB, struct.pack("<%dI" % len(job), *job))
    w.wr(W.STATUS + W.UNCACHED, 0)
    w.start(W.BASE)
    t0 = time.time()
    while w.rd(MARK + W.UNCACHED)[0] != 1:
        if time.time() - t0 > 60:
            raise RuntimeError("local code did not finish")
        time.sleep(0.02)
    dt = time.time() - t0
    w.park()
    return dt


def execute(w):
    """A loop (local load + 3 ALU + delay slot) placed in local RAM: does it run, and at what speed."""
    n = 20_000_000
    fn = W.assemble(
        f"""    .set noreorder
    .set noat
    .text
    .globl _start
_start:
    lui $10, 0x{n >> 16:04x}
    ori $10, $10, 0x{n & 0xFFFF:04x}
    li $9, 0x100
1:  lw $11, 0($9)
    addu $12, $12, $11
    addiu $10, $10, -1
    bnez $10, 1b
    nop
    lui $8, 0x61fe
    ori $8, $8, 0x6000
    li $9, 1
    sw $9, 0($8)
    jr $31
    nop
""",
        0x61FC8000,
    )  # position independent: linked anywhere, copied to local RAM
    for cop3 in (0, 5):
        t = min(_run_local(w, fn, 0x2000, cop3, 0x400 + cop3 * 0x40) for _ in range(3))
        print(f"code in local RAM, COP3={cop3}: {t * 160e6 / n:5.2f} cycles per 5-instruction iteration", flush=True)


def mul(w):
    """multu: when its result can be read, and a 32 x 32 -> 64 multiply-accumulate step."""
    n = 10_000_000
    base = None
    for name, body, k in (
        ("2 nops", " nop\n nop\n", 2),
        ("multu; mflo; mfhi", " multu $12, $13\n mflo $14\n mfhi $15\n", 3),
        ("multu; 4 nops; mflo; mfhi", " multu $12, $13\n nop\n nop\n nop\n nop\n mflo $14\n mfhi $15\n", 7),
        (
            "multiply-accumulate (7 instructions)",
            " multu $12, $13\n mflo $14\n mfhi $15\n addu $16, $16, $14\n"
            " sltu $1, $16, $14\n addu $17, $17, $15\n addu $17, $17, $1\n",
            7,
        ),
    ):
        c = min(w.timed_call(loop(body, n), 2)) * 160e6 / n
        base = c - 2 if base is None else base
        print(f"{name:38s} {c:6.2f} cycles per iteration: {c - base - k:+.2f} stall", flush=True)


OUT = 0x61FE6100  # SDRAM words the probes below write their results to


def _asm(body):
    return "    .set noreorder\n    .set noat\n    .text\n    .globl _start\n_start:\n" + body


def _local_fn(body, at_arm=0x61FC8000):
    """Position-independent code for _run_local: `body`, then MARK = 1 and return."""
    return W.assemble(
        _asm(body + " lui $8, 0x61fe\n ori $8, $8, 0x6000\n li $9, 1\n sw $9, 0($8)\n jr $31\n nop\n"), at_arm
    )


def cop3_reset(w):
    """What COP3 register 0 holds after a Warp reset (halt = reset with the halt request), for 5, 0x10 and 0."""
    for v, at in ((5, 0x400), (0x10, 0x440), (0, 0x480)):
        w.snippet(f" li $8, {v}\n .word 0x4c880000\n nop\n nop\n jr $31\n nop\n", at=W.SNIPPET + at)
        w.halt()
        print(f"COP3 set to {v:#x}, then a Warp reset: reads {w.debug_read(0x4C080000):#x}", flush=True)  # mfc3 $t0, $0
        w.park()


def cache_bit(w):
    """A loop in SDRAM timed with COP3 = 0, 1, 4, 5: which bit turns the I-cache on. Long runs (~0.5 s cached): at
    1,000,000 passes (~30 ms) the 20 ms completion polling made any cached value read 5.2-9.9."""
    n = 16_000_000
    for v in (0, 1, 4, 5):
        asm = loop(" nop\n nop\n", n).replace(" li $8, 5\n", f" li $8, {v}\n", 1)
        t = min(w.timed_call(asm, 2))
        print(f"COP3={v}: {t * 160e6 / n:7.2f} cycles per 5-instruction iteration from SDRAM", flush=True)


def local_cached(w):
    """Is code in local RAM cached? Run it with the cache on, overwrite it, run it again without an invalidate:
    the old result = cached (stale). The same in SDRAM as the control (known to go stale)."""

    def fn(v):
        return _local_fn(f" lui $10, 0x61fe\n ori $10, $10, 0x6104\n li $11, {v}\n sw $11, 0($10)\n")

    w.icache_invalidate()
    res = []
    for v in (0x1111, 0x2222):
        _run_local(w, fn(v), 0x2000, 5, 0x500)
        res.append(w.rd(0x61FE6104 + W.UNCACHED)[0])
    print(
        f"local RAM code, cache on: wrote 0x1111 then 0x2222 at one address, ran {res[0]:#x} then {res[1]:#x}"
        f" -> {'cached (stale)' if res[1] == 0x1111 else 'not cached'}",
        flush=True,
    )
    res = []
    at = W.SNIPPET + 0x600
    for v in (0x3333, 0x4444):
        w.snippet(
            f" li $8, 5\n .word 0x4c880000\n nop\n nop\n lui $10, 0x61fe\n ori $10, $10, 0x6108\n"
            f" li $11, {v}\n sw $11, 0($10)\n jr $31\n nop\n",
            at=at,
        )
        res.append(w.rd(0x61FE6108 + W.UNCACHED)[0])
    print(
        f"SDRAM code (control), cache on: ran {res[0]:#x} then {res[1]:#x}"
        f" -> {'cached (stale)' if res[1] == 0x3333 else 'not cached'}",
        flush=True,
    )


def tag_dump(w):
    """The tag RAM (0x02000000-0x020005FF), read by code in local RAM with COP3 = 0x10, as bytes."""
    dump = _local_fn(
        " lui $9, 0x0200\n lui $10, 0x61fe\n ori $10, $10, 0x8000\n li $12, 384\n"
        "1: lw $11, 0($9)\n addiu $9, $9, 4\n sw $11, 0($10)\n addiu $12, $12, -1\n"
        " bnez $12, 1b\n addiu $10, $10, 4\n"
    )
    _run_local(w, dump, 0x2400, 0x10, 0x540)
    return struct.pack("<384I", *w.read_many(0x61FE8000 + W.UNCACHED, 384))


def tags(w):
    """The tag RAM's layout: dump it, run 8 lines of known code at 0x61FCA800 (index 0x80-0x87), dump again."""
    w.icache_invalidate()
    a = tag_dump(w)
    body = " li $8, 5\n .word 0x4c880000\n" + " nop\n" * 28 + " jr $31\n nop\n"  # 32 instructions = 8 lines
    w.snippet(body, at=0x61FCA800)
    b = tag_dump(w)
    print("tag RAM bytes that changed (offset: before -> after):", flush=True)
    for i in range(len(a)):
        if a[i] != b[i]:
            print(f"  +{i:#05x} (entry {i // 6} byte {i % 6} / word {i // 4}): {a[i]:02x} -> {b[i]:02x}", flush=True)
    nz = [i for i in range(0, len(b), 4) if b[i : i + 4] != bytes(4)]
    print(
        "non-zero words after:",
        " ".join(f"+{i:#05x}={struct.unpack_from('<I', b, i)[0]:08x}" for i in nz[:48]),
        flush=True,
    )


def delay(w):
    """The load delay: what the next instruction sees, and who wins when it writes the load's register."""
    w.wr(OUT + W.UNCACHED, 0xAAAA5555)
    w.snippet(
        " lui $9, 0x61fe\n ori $9, $9, 0x6100\n"
        " .word 0x8D280000\n .word 0x01005021\n nop\n"  # lw $8, 0($9); addu $10, $8, $0
        " sw $10, 4($9)\n sw $8, 8($9)\n"
        " .word 0x8D280000\n .word 0x24080005\n nop\n"  # lw $8, 0($9); addiu $8, $0, 5
        " sw $8, 12($9)\n jr $31\n nop\n",
        at=W.SNIPPET + 0x680,
    )
    r = w.rd(OUT + 4 + W.UNCACHED, 3)
    print(
        f"next instruction saw {r[0]:#x} (address {OUT:#x}); after it {r[1]:#x}; a write of 5 in the delay slot"
        f" -> {r[2]:#x} ({'the load wins' if r[2] == 0xAAAA5555 else 'the delay-slot write wins' if r[2] == 5 else '?'})",
        flush=True,
    )


def odd(w):
    """Instructions outside MIPS I: movz/movn (rt = 0 and 7), SPECIAL2 mul and clz, beql taken, bnel not taken."""
    cases = (
        ("movz $10,$11,$12 ($12=0)", 0x016C500A, 0),
        ("movz $10,$11,$12 ($12=7)", 0x016C500A, 7),
        ("movn $10,$11,$12 ($12=0)", 0x016C500B, 0),
        ("movn $10,$11,$12 ($12=7)", 0x016C500B, 7),
        ("mul $10,$11,$13 ($13=3)", 0x716D5002, 0),
        ("clz $10,$11", 0x716A5020, 0),
    )
    for i, (name, word, r12) in enumerate(cases):
        w.snippet(
            f" lui $9, 0x61fe\n ori $9, $9, 0x6100\n li $10, 0x1111\n li $11, 0x2222\n li $12, {r12}\n"
            f" li $13, 3\n nop\n .word {word:#x}\n nop\n nop\n sw $10, 0($9)\n jr $31\n nop\n",
            at=W.SNIPPET + 0x700 + 0x40 * i,
        )
        print(f"{name:28s} $10 = {w.rd(OUT + W.UNCACHED)[0]:#x} (before 0x1111, $11 = 0x2222)", flush=True)
    w.snippet(
        " lui $9, 0x61fe\n ori $9, $9, 0x6100\n li $14, 0\n li $15, 0\n li $16, 0\n li $17, 0\n"
        " .word 0x50000002\n .word 0x240E0001\n .word 0x240F0001\n"  # beql $0,$0,+2; delay: $14=1; skipped: $15=1
        " .word 0x54000002\n .word 0x24100001\n .word 0x24110001\n"  # bnel $0,$0,+2; delay: $16=1; next: $17=1
        " nop\n sw $14, 0($9)\n sw $15, 4($9)\n sw $16, 8($9)\n sw $17, 12($9)\n jr $31\n nop\n",
        at=W.SNIPPET + 0x900,
    )
    r = w.rd(OUT + W.UNCACHED, 4)
    print(
        f"beql taken: delay slot ran {r[0]}, the instruction after it ran {r[1]}; "
        f"bnel not taken: delay slot ran {r[2]}, the next ran {r[3]}",
        flush=True,
    )


def bus(w):
    """Store to the on-chip SRAM (an unclaimed word, ARM 0x0040FFF0) and loads/stores to peripherals (0xE0011000,
    unused scratch RAM; 0xE0010800, the AV block's port-presence register: loads only)."""
    n = 5_000_000
    at = lambda hi, lo: f" lui $9, 0x{hi:04x}\n ori $9, $9, 0x{lo:04x}\n"
    for name, setup, body in (
        ("registers only", "", " nop\n nop\n"),
        ("SRAM store", at(0x0400, 0xFFF0), " sw $0, 0($9)\n nop\n"),
        ("peripheral load (scratch)", at(0xE001, 0x1000), " lw $11, 0($9)\n nop\n"),
        ("peripheral store (scratch)", at(0xE001, 0x1000), " sw $10, 0($9)\n nop\n"),
        ("peripheral load (AV block)", at(0xE001, 0x0800), " lw $11, 0($9)\n nop\n"),
    ):
        t = min(w.timed_call(loop(body, n, setup), 2))
        print(f"{name:28s} {t * 160e6 / n:6.2f} cycles per iteration (5 instructions)", flush=True)


LOOPS = 0x61FD8000  # loop code for bit0 (an area of the Warp1 window the probes here don't use)
PROLOGUE = 0x61FD7000  # per-COP3-value prologues that set COP3 and jump to the loop


def _bit0_run(w, cop3, entry, n, invalidate=True):
    """Seconds for the loop at `entry` (n passes) entered through a prologue that sets COP3 = cop3. The loop's bytes
    don't depend on cop3, so a run can reuse lines another run filled (invalidate=False)."""
    if invalidate:
        w.icache_invalidate()
    pro = PROLOGUE + cop3 * 0x40
    w.halt()
    if not getattr(w, "loaded", False):
        w.write_block(W.BASE, W.build_runner())
        w.loaded = True
    w.write_block(
        pro,
        W.assemble(
            _asm(
                f" li $8, {cop3}\n .word 0x4c880000\n nop\n nop\n lui $10, 0x{n >> 16:04x}\n ori $10, $10, 0x{n & 0xFFFF:04x}\n"
                f" lui $25, 0x{entry >> 16:04x}\n ori $25, $25, 0x{entry & 0xFFFF:04x}\n jr $25\n nop\n"
            ),
            pro,
        ),
    )
    words = [x for cmd in [(W.CALL, pro, 0, 0), (W.END, 0, 0, 0)] for x in cmd]
    w.write_block(W.JOB, struct.pack("<%dI" % len(words), *words))
    w.wr(W.STATUS + W.UNCACHED, 0)
    w.start(W.BASE)
    t0 = time.time()
    while w.rd(W.STATUS + W.UNCACHED)[0] != W.DONE:
        if time.time() - t0 > 120:
            raise RuntimeError("loop did not finish")
        time.sleep(0.02)
    dt = time.time() - t0
    w.park()
    return dt


def _bit0_loop(w, entry, size):
    """`size` instructions (nops, then addiu/bnez/nop) at `entry`, looping on $10, then jr $31."""
    body = " nop\n" * (size - 3) + " addiu $10, $10, -1\n bnez $10, _start\n nop\n jr $31\n nop\n"
    w.write_block(entry, W.assemble(_asm(body), entry))


def bit0(w):
    """COP3 bit 0 alone: loops of 5 instructions in one 32-byte line and across two, 32 (4 lines) and 128 (16 lines),
    against bit 2 (COP3 = 4) and off (0); the tags after a COP3 = 1 run; lines filled with COP3 = 4, then run with 1;
    the one- and two-line cases ten times."""
    cases = (
        ("5 in one line", LOOPS + 0x000, 5),
        ("5 across two lines", LOOPS + 0x218, 5),
        ("32 = 4 lines", LOOPS + 0x400, 32),
        ("128 = 16 lines", LOOPS + 0x800, 128),
    )
    for name, entry, size in cases:
        _bit0_loop(w, entry, size)
        n = 80_000_000 // size
        out = []
        for cop3 in (1, 4, 0):
            t = min(_bit0_run(w, cop3, entry, n if cop3 else n // 8) for _ in range(2))
            out.append(f"COP3={cop3}: {t * 160e6 / ((n if cop3 else n // 8) * size):6.2f}")
        print(f"{name:20s} cycles per instruction  " + "  ".join(out), flush=True)
    entry, size = LOOPS + 0x400, 32
    _bit0_loop(w, entry, size)
    _bit0_run(w, 1, entry, 1000)
    t = tag_dump(w)
    want = 0x300000 | (entry >> 11)
    hits = [i // 4 for i in range(0, 0x200, 4) if struct.unpack_from("<I", t, i)[0] == want]
    print(
        f"tags after a COP3 = 1 run of 4 lines at {entry:#x}: {len(hits)} entries {want:#x} (words {hits})", flush=True
    )
    n = 80_000_000 // size
    _bit0_run(w, 4, entry, 1000)
    t1 = _bit0_run(w, 1, entry, n, invalidate=False)
    t2 = _bit0_run(w, 1, entry, n, invalidate=False)
    print(
        f"4 lines filled with COP3 = 4, then COP3 = 1 without an invalidate: {t1 * 160e6 / (n * size):.2f}, then"
        f" {t2 * 160e6 / (n * size):.2f} cycles per instruction",
        flush=True,
    )
    for name, entry, size in cases[:2]:
        _bit0_loop(w, entry, size)
        n = 80_000_000 // size
        r = [_bit0_run(w, 1, entry, n) * 160e6 / (n * size) for _ in range(10)]
        print(f"{name:20s} COP3=1 x10: " + " ".join(f"{x:.2f}" for x in r), flush=True)


LINES = 0x61FD9000  # one-line functions for lru/tagbits, all in set 0: A, A + 2 KB, A + 4 KB


def _line_fn(w, at, mark):
    """A one-line (8-instruction) function at `at` that stores `mark` to OUT + 0x40 and returns."""
    w.write_block(
        at,
        W.assemble(
            _asm(f" lui $2, 0x61fe\n ori $2, $2, 0x6140\n li $3, {mark}\n sw $3, 0($2)\n nop\n nop\n jr $31\n nop\n"),
            at,
        ),
    )


def _call(x):
    return f" lui $25, 0x{x >> 16:04x}\n ori $25, $25, 0x{x & 0xFFFF:04x}\n jalr $25\n nop\n"


def _cache_view(set_, data=True):
    """Code (COP3 = 0x10 already set) that stores the set's two tag words and each way's third instruction word (the
    data view shows the word at index i at i - 8) to OUT + 0x80.."""
    return (
        f" lui $9, 0x0200\n lw $11, {4 * set_}($9)\n lw $12, {0x100 + 4 * set_}($9)\n"
        + (
            f" lui $9, 0x0001\n lw $13, {32 * set_}($9)\n lw $14, {0x800 + 32 * set_}($9)\n"
            if data
            else " li $13, 0\n li $14, 0\n"
        )
        + " lui $10, 0x61fe\n ori $10, $10, 0x6180\n sw $11, 0($10)\n sw $12, 4($10)\n sw $13, 8($10)\n"
        " sw $14, 12($10)\n"
    )


def lru(w):
    """Replacement: three one-line functions in set 0 (A, B = A + 2 KB, C = A + 4 KB) called in a given order from
    code in local RAM (never cached, the caller uncached), then which two the set holds (tags, data view)."""
    a, b, c = LINES, LINES + 0x800, LINES + 0x1000
    names = {a >> 11: "A", b >> 11: "B", c >> 11: "C"}
    for x, m in ((a, 0xA), (b, 0xB), (c, 0xC)):
        _line_fn(w, x, m)
    first = {}
    for x, m in ((a, 0xA), (b, 0xB), (c, 0xC)):  # each function's third word (li $3, mark): the data view
        first[w.rd(x + 8 + W.UNCACHED)[0]] = "ABC"[m - 0xA]
    for pattern in ("AB", "ABC", "ABAC", "ABAC", "ABBC", "ACBA", "ABACB", "data:AB"):
        data = pattern.startswith("data:")
        pattern = pattern.replace("data:", "")
        w.icache_invalidate()
        calls = "".join(_call({"A": a, "B": b, "C": c}[k]) for k in pattern)
        fn = _local_fn(
            " move $24, $31\n li $8, 5\n .word 0x4c880000\n nop\n nop\n"
            + calls
            + " li $8, 0x10\n .word 0x4c880000\n nop\n nop\n"
            + _cache_view(0, data)
            + " li $8, 0\n .word 0x4c880000\n nop\n nop\n move $31, $24\n"
        )
        _run_local(w, fn, 0x2000, 0, 0x5C0)
        t0, t1, d0, d1 = w.rd(OUT + 0x80 + W.UNCACHED, 4)
        ways = [names.get(t & 0xFFFFF, "-") if t & 0x300000 else "-" for t in (t0, t1)]
        print(
            f"{pattern:6s} way 0: {ways[0]} ({t0:#08x}), way 1: {ways[1]} ({t1:#08x})"
            + (f"; data view: {first.get(d0, hex(d0))} / {first.get(d1, hex(d1))}" if data else ""),
            flush=True,
        )


def viewread(w):
    """How reads of the cache RAM view behave: fill set 0 with A then B (A, A + 2 KB), then read tag and data words in
    a fixed order (each address twice in a row) with COP3 = 0x10 and print every value, to separate a read lag from
    the layout."""
    a, b = LINES, LINES + 0x800
    _line_fn(w, a, 0xA)
    _line_fn(w, b, 0xB)
    words = {w.rd(x + 4 * i + W.UNCACHED)[0]: f"{n}{i}" for x, n in ((a, "A"), (b, "B")) for i in range(8)}
    order = [
        0x02000000,
        0x02000000,
        0x02000100,
        0x02000100,
        0x02000004,
        0x02000004,
        0x020000FC,
        0x020000FC,
        0x02000104,
        0x02000104,
        0x02000000,
        0x02000100,
        0x02000000,
        0x00010000,
        0x00010000,
        0x00010004,
        0x00010008,
        0x00010008,
        0x00010800,
        0x00010800,
        0x00010808,
        0x00010000,
        0x00010800,
        0x00010000,
    ]
    reads = "".join(
        f" lui $9, 0x{x >> 16:04x}\n ori $9, $9, 0x{x & 0xFFFF:04x}\n lw $11, 0($9)\n nop\n sw $11, {4 * i}($10)\n"
        for i, x in enumerate(order)
    )
    w.icache_invalidate()
    fn = _local_fn(
        " move $24, $31\n li $8, 5\n .word 0x4c880000\n nop\n nop\n"
        + _call(a)
        + _call(b)
        + " li $8, 0x10\n .word 0x4c880000\n nop\n nop\n lui $10, 0x61fe\n ori $10, $10, 0x6200\n"
        + reads
        + " li $8, 0\n .word 0x4c880000\n nop\n nop\n move $31, $24\n"
    )
    _run_local(w, fn, 0x2000, 0, 0x640)
    r = w.rd(0x61FE6200 + W.UNCACHED, len(order))
    tags = {a >> 11: "tagA", b >> 11: "tagB"}
    for x, v in zip(order, r):
        what = (tags.get(v & 0xFFFFF, "?") if v & 0x100000 else "-") if x >> 24 == 2 else words.get(v, "?")
        print(f"  read {x:#010x}: {v:#010x} {what}", flush=True)


def assoc(w):
    """The cache's geometry by timing: a loop in local RAM calls one-line functions at given distances apart,
    2,000,000 times; cycles per pass. Lines that fit stay cached; conflicting ones miss (~24 cycles each)."""
    n = 2_000_000
    for name, offs in (
        ("A alone", (0,)),
        ("A, A+1K", (0, 0x400)),
        ("A, A+2K", (0, 0x800)),
        ("A, A+4K", (0, 0x1000)),
        ("A, A+8K", (0, 0x2000)),
        ("A, A+2K, A+4K", (0, 0x800, 0x1000)),
        ("A, A+1K, A+2K", (0, 0x400, 0x800)),
        ("A..A+3K by 1K", (0, 0x400, 0x800, 0xC00)),
    ):
        for i, o in enumerate(offs):
            _line_fn(w, LINES + o, 0xA + i)
        calls = "".join(_call(LINES + o) for o in offs)
        fn = _local_fn(
            " move $24, $31\n li $8, 5\n .word 0x4c880000\n nop\n nop\n"
            f" lui $16, 0x{n >> 16:04x}\n ori $16, $16, 0x{n & 0xFFFF:04x}\n2:\n"
            + calls
            + " addiu $16, $16, -1\n bnez $16, 2b\n nop\n li $8, 0\n .word 0x4c880000\n nop\n nop\n"
            " move $31, $24\n"
        )
        w.icache_invalidate()
        t = min(_run_local(w, fn, 0x2000, 0, 0x680) for _ in range(2))
        print(f"{name:16s} {t * 160e6 / n:7.1f} cycles per pass ({len(offs)} calls)", flush=True)


def policy(w):
    """Replacement policy: call sequences over three lines of one set (A, B = A + 2 KB, C = A + 4 KB), 2,000,000
    passes; misses per pass from the cycles (a hit ~14 cycles a call, a miss ~25 more). Expected misses per pass:
    ABAC and ABCB: LRU 2, FIFO 3, random ~2.4; AABC: LRU 3, FIFO 3, random 2; ABACAB: LRU 2, FIFO 3, random ~2.9."""
    n = 2_000_000
    at = {"A": LINES, "B": LINES + 0x800, "C": LINES + 0x1000}
    for i, k in enumerate("ABC"):
        _line_fn(w, at[k], 0xA + i)
    for seq in ("AB", "ABAC", "ABCB", "AABC", "ABACAB"):
        calls = "".join(_call(at[k]) for k in seq)
        fn = _local_fn(
            " move $24, $31\n li $8, 5\n .word 0x4c880000\n nop\n nop\n"
            f" lui $16, 0x{n >> 16:04x}\n ori $16, $16, 0x{n & 0xFFFF:04x}\n2:\n"
            + calls
            + " addiu $16, $16, -1\n bnez $16, 2b\n nop\n li $8, 0\n .word 0x4c880000\n nop\n nop\n"
            " move $31, $24\n"
        )
        w.icache_invalidate()
        c = min(_run_local(w, fn, 0x2000, 0, 0x6C0) for _ in range(2)) * 160e6 / n
        print(f"{seq:7s} {c:7.1f} cycles per pass: ~{(c - 14.3 * len(seq)) / 25:.2f} misses", flush=True)


def present(w):
    """Which lines a set holds after one pass of a call sequence over A, B = A + 2 KB, C = A + 4 KB (from empty):
    then every function's code in SDRAM is changed (stores, which the I-cache doesn't see) and one of them is called:
    its old mark = it was cached, 0xD = refetched. Expected after ABAC: LRU {A, C}, FIFO {B, C}; after ABC: {B, C}."""
    at = {"A": LINES, "B": LINES + 0x800, "C": LINES + 0x1000}
    for i, k in enumerate("ABC"):
        _line_fn(w, at[k], 0xA + i)
    orig = {k: w.rd(at[k] + 8 + W.UNCACHED)[0] for k in "ABC"}
    new = struct.unpack("<I", W.assemble(_asm(" li $3, 0xD\n"), LINES + 8)[:4])[0]
    patch = "".join(
        f" lui $9, 0x{(at[k] + 8) >> 16:04x}\n ori $9, $9, 0x{(at[k] + 8) & 0xFFFF:04x}\n sw $11, 0($9)\n"
        for k in "ABC"
    )
    for seq in ("ABC", "ABAC", "BABC", "ABACB"):
        held = []
        for probe_ in "ABC":
            got = []
            for _ in range(3):
                for k in "ABC":
                    w.wr(at[k] + 8 + W.UNCACHED, orig[k])
                w.icache_invalidate()
                fn = _local_fn(
                    " move $24, $31\n li $8, 5\n .word 0x4c880000\n nop\n nop\n"
                    + "".join(_call(at[k]) for k in seq)
                    + f" lui $11, 0x{new >> 16:04x}\n ori $11, $11, 0x{new & 0xFFFF:04x}\n"
                    + patch
                    + _call(at[probe_])
                    + " li $8, 0\n .word 0x4c880000\n nop\n nop\n move $31, $24\n"
                )
                _run_local(w, fn, 0x2000, 0, 0x700)
                got.append(w.rd(OUT + 0x40 + W.UNCACHED)[0])
            held.append(f"{probe_}: " + "/".join("cached" if g != 0xD else "not" for g in got))
        print(f"after {seq:6s} " + ", ".join(held), flush=True)


def tagbits(w):
    """Which flag bit of a tag entry keeps a line valid: fill A (stores 0xA), let A's code in SDRAM say 0xD, write
    A's tag entry back with a mask applied, call A again: 0xA = the line still valid (stale), 0xD = refetched."""
    a = LINES
    _line_fn(w, a, 0xA)
    orig = w.rd(a + 8 + W.UNCACHED)[0]
    for mask in (0xFFFFFFFF, 0xFFDFFFFF, 0xFFEFFFFF, 0x0):
        w.wr(a + 8 + W.UNCACHED, orig)  # the Warp rewrote it last time (the tool's shadow doesn't know)
        w.icache_invalidate()
        new = W.assemble(_asm(" li $3, 0xD\n"), a + 8)[:4]  # the word at A + 8 (li $3, 0xA) -> 0xD
        fn = _local_fn(
            " move $24, $31\n li $8, 5\n .word 0x4c880000\n nop\n nop\n"
            + _call(a)
            + f" lui $9, 0x{a >> 16:04x}\n ori $9, $9, 0x{(a & 0xFFFF) + 8:04x}\n"
            f" lui $11, 0x{struct.unpack('<I', new)[0] >> 16:04x}\n"
            f" ori $11, $11, 0x{struct.unpack('<I', new)[0] & 0xFFFF:04x}\n sw $11, 0($9)\n"
            " li $8, 0x10\n .word 0x4c880000\n nop\n nop\n"
            " lui $9, 0x0200\n lw $11, 0($9)\n nop\n bnez $11, 1f\n nop\n addiu $9, $9, 0x100\n"
            " lw $11, 0($9)\n nop\n"
            f"1: lui $12, 0x{mask >> 16:04x}\n ori $12, $12, 0x{mask & 0xFFFF:04x}\n"
            " and $13, $11, $12\n sw $13, 0($9)\n lw $14, 0($9)\n"
            " li $8, 5\n .word 0x4c880000\n nop\n nop\n"
            + _call(a)
            + " li $8, 0\n .word 0x4c880000\n nop\n nop\n lui $10, 0x61fe\n ori $10, $10, 0x6180\n"
            " sw $11, 0($10)\n sw $14, 4($10)\n move $31, $24\n"
        )
        _run_local(w, fn, 0x2000, 0, 0x600)
        before, after = w.rd(OUT + 0x80 + W.UNCACHED, 2)
        res = w.rd(OUT + 0x40 + W.UNCACHED)[0]
        print(
            f"tag {before:#08x} written as {before & mask:#08x} (reads back {after:#08x}): the call stored {res:#x}"
            f" -> {'still valid (stale)' if res == 0xA else 'refetched' if res == 0xD else '?'}",
            flush=True,
        )


def bigloop(w):
    """Straight-line loops of 2-16 KB from SDRAM with the cache on (COP3 = 5), ~1 s each: cycles per instruction."""
    for kb in (2, 3, 4, 5, 6, 8, 12, 16):
        size = kb * 256
        _bit0_loop(w, LOOPS, size)
        n = 160_000_000 // size
        r = [_bit0_run(w, 5, LOOPS, n) * 160e6 / (n * size) for _ in range(2)]
        print(f"{kb:2d} KB loop: " + " ".join(f"{x:.2f}" for x in r) + " cycles per instruction", flush=True)


def unaligned(w):
    """lw/lh/lhu at every misalignment of 0x44332211 0x88776655, and ll/sc."""
    w.wr(OUT + W.UNCACHED, 0x44332211)
    w.wr(OUT + 4 + W.UNCACHED, 0x88776655)
    w.wr(OUT + 8 + W.UNCACHED, 0xCAFEF00D)
    loads = (
        [("lw", 0x23, o) for o in (1, 2, 3)] + [("lh", 0x21, o) for o in (1, 3)] + [("lhu", 0x25, o) for o in (1, 3)]
    )
    body = " lui $9, 0x61fe\n ori $9, $9, 0x6100\n"
    for i, (_name, op, o) in enumerate(loads):  # results to OUT + 0x20 + 4i
        body += f" .word {(op << 26) | (9 << 21) | (10 << 16) | o:#x}\n nop\n nop\n sw $10, {0x20 + 4 * i}($9)\n"
    body += (
        " li $11, 0x1234\n li $10, 0x5555\n"
        " .word 0xC12A0008\n nop\n nop\n sw $10, 0x40($9)\n"  # ll $10, 8($9)
        " .word 0xE12B0008\n nop\n nop\n sw $11, 0x44($9)\n"  # sc $11, 8($9)
        " jr $31\n nop\n"
    )
    w.snippet(body, at=W.SNIPPET + 0xA00)
    r = w.rd(OUT + 0x20 + W.UNCACHED, len(loads))
    for (name, _, o), v in zip(loads, r):
        print(f"{name:3s} at +{o}: {v:#010x}", flush=True)
    ll, sc = w.rd(OUT + 0x40 + W.UNCACHED, 2)
    print(
        f"ll from 0xCAFEF00D: {ll:#x}; sc of 0x1234: $11 = {sc:#x}, memory {w.rd(OUT + 8 + W.UNCACHED)[0]:#x}",
        flush=True,
    )


def main():
    args = sys.argv[1:]
    core = 0
    if "--core" in args:
        i = args.index("--core")
        core = int(args[i + 1])
        del args[i : i + 2]
    host, what = args[0], args[1] if len(args) > 1 else "all"
    w = W.Warp(host, core=core)
    w.core = core
    try:
        w.icache_invalidate()  # stale lines from earlier code made one Warp1 case run at 79 cycles until this
        for name, f in (
            ("mem", mem),
            ("exec", execute),
            ("mul", mul),
            ("sram", sram),
            ("map", localmap),
            ("cop3", cop3_reset),
            ("cachebit", cache_bit),
            ("localcache", local_cached),
            ("tags", tags),
            ("delay", delay),
            ("odd", odd),
            ("bus", bus),
            ("bit0", bit0),
            ("lru", lru),
            ("viewread", viewread),
            ("assoc", assoc),
            ("policy", policy),
            ("present", present),
            ("tagbits", tagbits),
            ("bigloop", bigloop),
            ("unaligned", unaligned),
        ):
            if name == "map" and core != 1:
                continue
            if what in (name, "all"):
                f(w)
    finally:
        w.close()


if __name__ == "__main__":
    main()
