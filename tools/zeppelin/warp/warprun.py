#!/usr/bin/env python3
"""Run jobs on one of the DM870's Warp cores (Warp1 unless told otherwise) from a Mac, through a KnOS shell
(docs/module/warp.md).

The runner (runner.S, built here with nix's LLVM) interprets a job list in SDRAM; this script writes runner, job and
data with the shell's `wr` through the uncached SDRAM alias (0x7xxxxxxx: the ARM's cache would hide the words from
the Warp), starts the core with the debug sequence of Nokia's warp_debugger.ko, waits for the runner's DONE marker,
reads the results, and halts the core again (held in reset, as KnOS leaves it). RAM and registers only.

  python3 -I warprun.py HOST[:PORT] selftest        runner check: stores, loads, copy, poll timeout
  python3 -I warprun.py HOST[:PORT] alu OP A B [MODE]  one Sol ALU op through KnOS's test program
As a module: Warp(host).run(job) -> results; sol_alu_job(...) builds the Sol jobs.
Needs a shell: the stock app's on telnet (docs/module/knos.md) or our OS's zd1-memsh (the debug system, `zd1 debug on`); HOST as for
knos.py, the emulator's included.
"""

import argparse
import os
import struct
import sys
import time
import zlib

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "..", "module", "warp0"))
from mips import assemble  # noqa: E402  (the MIPS I toolchain, module/warp0/mips.py; the probes use W.assemble)
from zeppelin.knos import knos  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
BASE = 0x61FC0000  # runner, as the Warp sees it
UNCACHED = 0x10000000  # ARM: SDRAM 0x6xxxxxxx = uncached 0x7xxxxxxx
STATUS, JOB, RESULTS, DATA, CLEARS = BASE + 0xF00, BASE + 0x1000, BASE + 0x8000, BASE + 0x4000, BASE + 0x6000
SNIPPET = BASE + 0x7000  # code for CALL (up to 4 KB)
CODE_SLOTS = 0x61FF0000  # 1024 slots of 64 bytes for cached code, chosen by content (code_slot)
WARP1 = 0xE0015000  # debug block: +0 exec, +4 data in, +8 data out, +0xC halt/debug, +0x10 reset
DONE = 0x444F4E45


END, W32, W16, W8, R32, R16, POLL16, COPY32, DELAY, STORELIST, CALL, FILL, DIFF = range(13)


def build_runner():
    return assemble(open(os.path.join(HERE, "runner.S")).read(), BASE)


def code_slot(asm):
    """An address for position-independent Warp code chosen from its text: the I-cache keeps lines across resets
    and cannot be invalidated on Warp0 (docs/module/warp.md, section 3), so code that runs cached must never share an
    address with different code; the same code always lands at the same place."""
    return CODE_SLOTS + (zlib.crc32(asm.encode()) % 1024) * 0x40


class Warp:
    def __init__(self, host, verbose=False, core=1):
        self.sh = knos.connect(host, tries=10)
        ip = knos.target(host)[0]
        self.verbose = verbose
        self.dbg = 0xE0014000 + 0x1000 * core  # debug block of Warp0 / Warp1
        # The emulated module UART drops bytes if more than ~a dozen `wr` lines are sent before it drains; hardware's
        # UART does not. Send small chunks to the emulator (localhost), the full burst to hardware.
        self.chunk = 8 if ip in ("127.0.0.1", "localhost", "::1") else 128

    def wr(self, a, v):
        r = self.sh.cmd("wr 0x%08X 0x%08X" % (a, v & 0xFFFFFFFF), settle=0)
        if "set to value" not in r:
            raise RuntimeError("wr %08x: %r" % (a, r))

    def rd(self, a, n=1):
        out = self.sh.cmd("rd 0x%08X %d" % (a, n), settle=0)
        words = []
        for line in out.splitlines():
            if ":" in line:
                words += [int(x, 16) for x in line.split(":", 1)[1].split()]
        return words[:n]

    def write_words(self, pairs):
        """[(arm_addr, value)]: `wr` lines sent in batches without waiting for each prompt (the shell runs them in
        order; ~46 ms a word instead of ~100), every line's acknowledgement counted."""
        for i in range(0, len(pairs), self.chunk):
            batch = pairs[i : i + self.chunk]
            self.sh.s.sendall(b"".join(b"wr 0x%08X 0x%08X\r\n" % (a, v & 0xFFFFFFFF) for a, v in batch))
            buf, t = b"", time.time()
            while buf.count(b"set to value") < len(batch) and time.time() - t < 30:
                try:
                    buf += self.sh.s.recv(65536)
                except TimeoutError:
                    pass
            n = buf.count(b"set to value")
            while not buf.rstrip().endswith(knos.PROMPT) and time.time() - t < 30:
                try:
                    buf += self.sh.s.recv(65536)
                except TimeoutError:
                    pass
            if n != len(batch):
                raise RuntimeError("batch write: %d of %d acknowledged" % (n, len(batch)))

    def write_block(self, warp_addr, data):
        """Words already written with this value in this session are skipped: only the ARM writes these blocks
        (runner, job, programs), so consecutive probe cases resend just the words that changed."""
        data = data + b"\0" * (-len(data) % 4)
        shadow = self.__dict__.setdefault("shadow", {})
        pairs = [(warp_addr + UNCACHED + i, struct.unpack_from("<I", data, i)[0]) for i in range(0, len(data), 4)]
        pairs = [(a, v) for a, v in pairs if shadow.get(a) != v]
        self.write_words(pairs)
        shadow.update(pairs)

    def snippet(self, asm, job_before=(), job_after=(), blocks=(), timeout=30.0, at=SNIPPET):
        """Run MIPS code (`.set noreorder`, label `_start`, ends with `jr $31` + delay slot) once; returns results.
        `at`: where the code goes. With the instruction cache on (COP3 register 0 = 5), use a fresh address for
        new code: the cache is not invalidated by a Warp reset (see docs/module/warp.md)."""
        code = assemble("    .set noreorder\n    .set noat\n    .text\n    .globl _start\n_start:\n" + asm, at)
        return self.run(
            list(job_before) + [(CALL, at, 0, 0)] + list(job_after), list(blocks) + [(at, code)], timeout=timeout
        )

    def icache_invalidate(self, at=SNIPPET + 0xF00):
        """Invalidate this core's I-cache (both cores, docs/module/warp.md section 3): COP3 = 0x10 (cache off, cache
        RAM mapped), zero the tag RAM 0x02000000-0x020005FF, COP3 = 5. Keep `at` for this code only."""
        asm = (
            " li $8, 0x10\n .word 0x4c880000\n nop\n nop\n lui $9, 0x0200\n lui $10, 0x0200\n ori $10, $10, 0x0600\n"
            "1: sw $0, 0($9)\n addiu $9, $9, 4\n bne $9, $10, 1b\n nop\n li $8, 5\n .word 0x4c880000\n nop\n nop\n"
            " jr $31\n nop\n"
        )
        self.snippet(asm, at=at)

    def halt(self):
        self.wr(self.dbg + 0xC, 1)  # halt request
        self.wr(self.dbg + 0x10, 0)  # reset held ...
        self.wr(self.dbg + 0x10, 1)  # ... and released: debug mode (with the halt request)
        st = self.rd(self.dbg + 0xC)[0]
        if st != 3:
            raise RuntimeError("Warp not in debug mode after reset: %#x" % st)

    def start(self, pc):
        self.wr(self.dbg + 0x4, pc)
        self.wr(self.dbg + 0x0, 0x4408B000)  # mfc1 $t0, $22   (data in)
        self.wr(self.dbg + 0x0, 0x4488A800)  # mtc1 $t0, $21   (DebugPC)
        self.wr(self.dbg + 0xC, 0)
        self.wr(self.dbg + 0x0, 0x4480A000)  # mtc1 $0, $20    (leave debug mode)

    def debug_read(self, insn_to_t0):
        """In debug mode: run an instruction that puts a value in $t0, then `mtc1 $t0, $22`; returns it ($t0 is
        clobbered)."""
        self.wr(self.dbg + 0x0, insn_to_t0)
        self.wr(self.dbg + 0x0, 0x4488B000)  # mtc1 $t0, $22
        return self.rd(self.dbg + 0x8)[0]

    def inspect(self):
        """Stop the running core (no reset) and read where it is: PC ($23), DebugPC ($21), COP0 8, 12, 13, 14."""
        self.wr(self.dbg + 0xC, 1)
        self.wr(self.dbg + 0x10, 1)
        st = self.rd(self.dbg + 0xC)[0]
        out = {"state": st}
        if st & 2:
            out["pc"] = self.debug_read(0x4408B800)  # mfc1 $t0, $23
            out["debugpc"] = self.debug_read(0x4408A800)  # mfc1 $t0, $21
            for n in (8, 12, 13, 14):
                out["c0_%d" % n] = self.debug_read(0x40080000 | n << 11)  # mfc0 $t0, $n
        return out

    def park(self):
        """Back to how KnOS leaves it: halted, held in reset."""
        self.wr(self.dbg + 0xC, 1)
        self.wr(self.dbg + 0x10, 1)
        self.wr(self.dbg + 0x10, 0)
        self.wr(self.dbg + 0xC, 0)

    def run(self, job, blocks=(), runner=None, timeout=30.0):
        """job: list of (op, a, b, c); blocks: [(warp_addr, bytes)] written before the start. Returns the results.
        The runner is written once per connection (SDRAM keeps it across Warp resets)."""
        self.halt()
        if not getattr(self, "loaded", False):
            self.write_block(BASE, runner or build_runner())
            self.loaded = True
        for a, data in blocks:
            self.write_block(a, data)
        words = [w for cmd in list(job) + [(END, 0, 0, 0)] for w in cmd]
        self.write_block(JOB, struct.pack("<%dI" % len(words), *[w & 0xFFFFFFFF for w in words]))
        self.wr(STATUS + UNCACHED, 0)
        self.wr(STATUS + UNCACHED + 4, 0)
        self.start(BASE)
        t0 = time.time()
        try:
            while True:
                st = self.rd(STATUS + UNCACHED, 3)
                if st[0] == DONE:
                    break
                if time.time() - t0 > timeout:
                    where = self.inspect()
                    raise RuntimeError(
                        "runner did not finish: status %s, core %s"
                        % (["%08x" % x for x in st], {k: "%08x" % v for k, v in where.items()})
                    )
                time.sleep(0.2)
            n = st[1]
            return self.rd(RESULTS + UNCACHED, n) if n else []
        finally:
            self.park()

    def timed_call(self, asm, runs):
        """Run `asm` (position independent, ends with jr $31) through the runner `runs` + 1 times at its
        code_slot(); returns the wall-clock seconds of runs 1.. (run 0 fills the I-cache), polling the DONE word
        every 20 ms (slow polls: the shell traffic is a load too)."""
        at = code_slot(asm)
        code = assemble("    .set noreorder\n    .set noat\n    .text\n    .globl _start\n_start:\n" + asm, at)
        out = []
        for i in range(runs + 1):
            self.halt()
            if not getattr(self, "loaded", False):
                self.write_block(BASE, build_runner())
                self.loaded = True
            self.write_block(at, code)
            words = [x for cmd in [(CALL, at, 0, 0), (END, 0, 0, 0)] for x in cmd]
            self.write_block(JOB, struct.pack("<%dI" % len(words), *words))
            self.wr(STATUS + UNCACHED, 0)
            self.start(BASE)
            t0 = time.time()
            while self.rd(STATUS + UNCACHED)[0] != DONE:
                if time.time() - t0 > 60:
                    raise RuntimeError("Warp code did not finish")
                time.sleep(0.02)
            if i:
                out.append(time.time() - t0)
            self.park()
        return out

    def read_many(self, addr, n, chunk=1024):
        """rd in chunks: one `rd ADDR N` for a large N floods the shell past its read-to-prompt window (~1100
        words/s, so >~13000 words times out and desyncs). 1024 words is ~256 lines, under a second a chunk."""
        out = []
        for i in range(0, n, chunk):
            out += self.rd(addr + i * 4, min(chunk, n - i))
        return out

    def close(self):
        self.sh.close()


# ---- Sol, as KnOS's `sys sol testInstr` drives it (FUN_603282b4 / FUN_603d8xxx), from the Warp's side ----

SOL_MEM0, SOL_MEM1, SOL_CTRL = 0x01720000, 0x01760000, 0x01740000  # = ARM 0xD072/0xD076/0xD0740000
SOL_UNITS = 0x01C00000  # unit n window: + n * 0x40000
SOL_DATA = 0x00000000  # Warp1's local RAM = Sol data memory; the ARM sees it at 0xA8000000 (measured)
# KnOS's test program (0x60585E28 mem1, 0x60585E88 mem0 in the stock app), 12 instructions
KNOS_MEM1 = [
    0x0000A000,
    0,
    0x0000A000,
    0,
    0x0000A000,
    0x612C0000,
    0x0000A000,
    0x612C0000,
    0x0000A000,
    0,
    0x0000A000,
    0,
    0x00006408,
    0,
    0x0000A000,
    0,
    0x0000A000,
    0,
    0x00000038,
    0,
    0x0000A000,
    0,
    0x0000A000,
    0,
]
KNOS_MEM0 = [0xF00FF00F, 0, 0x78007800, 0, 0x78007800, 0, 0, 0, 0, 0, 0x000060FB, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]


def unit_reg(unit, reg):
    return SOL_UNITS + unit * 0x40000 + (reg << 2)


def sol_init(ctrl=(0xE0, 0x0E, 0x54)):
    """ctrl: the control block's +0x14 (16 bits), +0x16 and +0x17 (KnOS's values by default)."""
    return [
        (W16, SOL_CTRL + 0x14, ctrl[0], 0),
        (W8, SOL_CTRL + 0x16, ctrl[1], 0),
        (W8, SOL_CTRL + 0x17, ctrl[2], 0),
        (W8, 0x01700018, 0, 0),
        (W16, SOL_CTRL + 0x18, 1, 0),
        (W16, SOL_CTRL + 0x20, 4, 0),
    ]


def sol_unit_clear_list():
    """The addresses sol_unit_clear writes (0 each), for one STORELIST command."""
    return [cmd[1] for cmd in sol_unit_clear()]


def sol_unit_clear():
    """KnOS's FUN_603d8a9c / FUN_603d8b0c for units 4 and 5 (all selects 0), unit 4 0xE476, unit 5 0xE498 and unit 3
    0x8C02 (FUN_60407530), all written 0 before each case."""
    job = []
    for u in (4, 5):
        job.append((W32, SOL_UNITS + u * 0x40000 + 0x3C03C, 0, 0))
        for g in (
            0xC000,
            0xC200,
            0xC400,
            0xC600,
            0xE600,
            0xC800,
            0xCC00,
            0xCA00,
            0xCE00,
            0xD200,
            0xD000,
            0xD400,
            0xD800,
            0xD600,
            0xDA00,
            0xDE00,
            0xDC00,
        ):
            job.append((W32, unit_reg(u, g), 0, 0))
    job += [(W32, unit_reg(4, 0xE476), 0, 0), (W32, unit_reg(5, 0xE498), 0, 0), (W32, unit_reg(3, 0x8C02), 0, 0)]
    return job


def sol_load(mem1, mem0):
    """Program words (lo, hi pairs) into both program memories, as COPY32 from a data block."""
    blob = struct.pack("<%dI" % len(mem1), *mem1) + struct.pack("<%dI" % len(mem0), *mem0)
    blocks = [(DATA, blob)]
    job = [(COPY32, DATA, SOL_MEM1, len(mem1)), (COPY32, DATA + 4 * len(mem1), SOL_MEM0, len(mem0))]
    return job, blocks


def sol_run(timeout=100000, entry=4):
    """Start Sol at instruction `entry` and poll for its end (the poll gives up after `timeout`)."""
    return [(W16, SOL_CTRL + 0x20, entry, 0), (W16, SOL_CTRL + 0x18, 0, 0), (POLL16, SOL_CTRL + 0x18, 1, timeout)]


def sol_alu_job(cases, mem1=KNOS_MEM1, mem0=KNOS_MEM0):
    """cases: [(alu_slot16, a64, b64)]. Per case: operands at 0xA8001800 (A lo/hi 0x1800/0x1C00, B 0x2000/0x2400),
    the ALU slot (bits 63:48 of instructions 2 and 3), run, result from A's place. Results per case: tries left,
    lo, hi."""
    job, blocks = sol_load(mem1, mem0)
    clears = sol_unit_clear_list()
    blocks.append((CLEARS, struct.pack("<%dI" % len(clears), *clears)))
    job = sol_init() + job
    for slot, a, b in cases:
        job += [
            (STORELIST, CLEARS, 0, len(clears)),  # KnOS clears the units before every case: needed
            (W32, SOL_DATA + 0x1800, a & 0xFFFFFFFF, 0),
            (W32, SOL_DATA + 0x1C00, a >> 32, 0),
            (W32, SOL_DATA + 0x2000, b & 0xFFFFFFFF, 0),
            (W32, SOL_DATA + 0x2400, b >> 32, 0),
            (W16, SOL_MEM1 + 0x16, slot, 0),
            (W16, SOL_MEM1 + 0x1E, slot, 0),
        ]
        job += sol_run()
        job += [(R32, SOL_DATA + 0x1800, 0, 0), (R32, SOL_DATA + 0x1C00, 0, 0)]
    return job, blocks


def sol_cases_job(cases, entry=4, timeout=100000, ctrl=None):
    """General Sol probe. cases: [{'mem1': [words], 'mem0': [words], 'data': {warp_addr: word}, 'reads': [warp_addr],
    'fill': base (optional: local words in 'fill_range' (default banks 6-9, 0x1800-0x27FF) = base + word index; filling
    banks 0-5 stops Sol from finishing), 'unit_regs': [(unit, reg)] (address-encoded writes after the clears),
    'pre_start': [job commands] (just before the start)}] (program words are (lo, hi) pairs, like KNOS_MEM1). Per
    case: the programs are copied in (from SDRAM blocks), the data written, the units cleared, Sol run from `entry`
    (sol_run); results per case: tries left, then one word per read. ctrl: sol_init's control bytes."""
    clears = sol_unit_clear_list()
    blocks = [(CLEARS, struct.pack("<%dI" % len(clears), *clears))]
    job = sol_init(ctrl) if ctrl else sol_init()
    at = DATA
    for c in cases:
        m1, m0 = c["mem1"], c["mem0"]
        blob = struct.pack("<%dI" % len(m1), *m1) + struct.pack("<%dI" % len(m0), *m0)
        blocks.append((at, blob))
        job += [
            (COPY32, at, SOL_MEM1, len(m1)),
            (COPY32, at + 4 * len(m1), SOL_MEM0, len(m0)),
            (STORELIST, CLEARS, 0, len(clears)),
        ]
        at += len(blob)
        lo, hi = c.get("fill_range", (0x1800, 0x2800))
        if c.get("fill"):  # local words in fill_range = base + word index
            job.append((FILL, lo, c["fill"] + lo // 4, (hi - lo) // 4))
        job += [(W32, a, v, 0) for a, v in c.get("data", {}).items()]
        job += [(W32, unit_reg(u, r), 0, 0) for u, r in c.get("unit_regs", [])]
        job += list(c.get("pre_start", [])) + sol_run(timeout, entry)
        job += [(R32, a, 0, 0) for a in c.get("reads", [])]
        if c.get("fill"):  # then the words that differ from the fill (max 16)
            job.append((DIFF, lo, c["fill"] + lo // 4, (hi - lo) // 4))
    if at > CLEARS:
        raise ValueError("programs overflow the data area")
    return job, blocks


def parse_cases(cases, r):
    """Split the results of sol_cases_job per case: (tries left, [reads], [(addr, value)] diffs or None)."""
    out, k = [], 0
    for c in cases:
        tries = r[k]
        reads = r[k + 1 : k + 1 + len(c.get("reads", []))]
        k += 1 + len(reads)
        diffs = None
        if c.get("fill"):
            n = r[k]
            m = min(n, 16)
            diffs = [(r[k + 1 + 2 * i], r[k + 2 + 2 * i]) for i in range(m)] + ([("more", n - 16)] if n > 16 else [])
            k += 1 + 2 * m
        out.append((tries, reads, diffs))
    return out


def knos_case(alu=None, mac=None, a=0, b=0, extra_mem1=None, extra_mem0=None, reads=(0x1800, 0x1C00, 0x2000, 0x2400)):
    """KnOS's test program with the ALU slot (instructions 2 and 3, bits 63:48) and/or the MAC slot (instruction 3,
    bits 47:32) set; extra_mem1/extra_mem0: {word_index: value} overrides."""
    m1, m0 = list(KNOS_MEM1), list(KNOS_MEM0)
    if alu is not None:
        for i in (5, 7):
            m1[i] = (m1[i] & 0x0000FFFF) | (alu << 16)
    if mac is not None:
        m1[7] = (m1[7] & 0xFFFF0000) | mac
        m1[5] &= 0xFFFF0000 if alu is not None else 0x0000FFFF
    for i, v in (extra_mem1 or {}).items():
        m1[i] = v
    for i, v in (extra_mem0 or {}).items():
        m0[i] = v
    return {
        "mem1": m1,
        "mem0": m0,
        "reads": list(reads),
        "data": {0x1800: a & 0xFFFFFFFF, 0x1C00: a >> 32, 0x2000: b & 0xFFFFFFFF, 0x2400: b >> 32},
    }


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("host", metavar="HOST[:PORT]")
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("selftest")
    p = sub.add_parser("alu")
    for n in ("op", "a", "b"):
        p.add_argument(n, type=lambda v: int(v, 0))
    p.add_argument("mode", type=lambda v: int(v, 0), nargs="?", default=0x12C)
    a = ap.parse_args()
    w = Warp(a.host)
    try:
        if a.cmd == "selftest":
            data = struct.pack("<4I", 0x11111111, 0x22222222, 0x33333333, 0x44444444)
            job = [
                (W32, DATA + 0x100, 0xCAFEF00D, 0),
                (R32, DATA + 0x100, 0, 0),
                (COPY32, DATA, DATA + 0x200, 4),
                (R32, DATA + 0x20C, 0, 0),
                (W16, DATA + 0x300, 0x8001, 0),
                (R16, DATA + 0x300, 0, 0),
                (POLL16, DATA + 0x300, 0x0002, 50),
                (POLL16, DATA + 0x300, 0x8000, 50),
                (DELAY, 0, 0, 1000),
            ]
            r = w.run(job, [(DATA, data)])
            want = [0xCAFEF00D, 0x44444444, 0x8001, 0, 50]
            print("results", ["%08x" % x for x in r], "OK" if r == want else "MISMATCH (want %s)" % want)
        else:
            slot = (a.op << 10) | a.mode
            job, blocks = sol_alu_job([(slot, a.a, a.b)])
            r = w.run(job, blocks)
            print("slot %04x: tries left %d, result %08x%08x" % (slot, r[0], r[2], r[1]))
    finally:
        w.close()


if __name__ == "__main__":
    main()
