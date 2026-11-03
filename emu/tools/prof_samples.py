#!/usr/bin/env python3
"""Where a Renode run spends host time, from ZEMU_SAMPLE output (emu/run/<name>/samples.txt).

    ZEMU_SAMPLE=cr870 ZEMU_SAMPLE_STEP=0.01 python3 emu/zemu run os
    python3 emu/tools/prof_samples.py emu/run/os/samples.txt [--syms FILE]

Each sample line: virtual time, host time, PC, LR, CPSR. A slice's host cost (minus the sampler's own monitor
round trips, estimated as the cheapest slices) is charged to the PC at its end: a statistical profile, good for
"which phase / which function costs host time". PCs become kernel symbols (by default the kallsyms tables inside
blobs/module/nokia-hd1/kernel.bin, tools/zeppelin/jtag/kallsyms.py); user space (PC < 0xc0000000) and the boot code
are bucketed by region.
"""

import argparse
import bisect
import collections
import os
import subprocess
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "tools", "zeppelin", "jtag"))
import kallsyms  # noqa: E402


def vsecs(s):
    h, m, x = s.split(":")
    return int(h) * 3600 + int(m) * 60 + float(x)


def load_syms(path):
    """Symbols from /proc/kallsyms output (addr type name [module]) or an ELF (nm); None: the running kernel's own
    (Kernel_A, re/netmodule/NOKIA_HD1.md), not the rootfs /boot vmlinux, which is another build."""
    if path is None:
        s = kallsyms.Symbols(kallsyms.load())
        return s.addrs, s.names
    head = open(path, "rb").read(4)
    lines = (
        subprocess.run(["nm", "-n", path], capture_output=True, text=True).stdout.splitlines()
        if head == b"\x7fELF"
        else open(path).read().splitlines()
    )
    syms = []
    for line in lines:
        p = line.split()
        if len(p) >= 3 and p[1] in "tTwW":
            syms.append((int(p[0], 16), p[2] + (" " + p[3] if len(p) > 3 else "")))
    syms.sort()
    return [a for a, _ in syms], [n for _, n in syms]


def region(pc):
    if pc >= 0xC0000000:
        return "kernel"
    if 0x61F00000 <= pc < 0x62000000:
        return "linuxboot stub"
    if 0x400000 <= pc < 0x410000:
        return "SRAM (BL1/stage1/KnOS)"
    if 0xFFFF0000 <= pc:
        return "vectors"
    if pc < 0x10000:
        return "low RAM"
    if 0xBF000000 <= pc < 0xC0000000:
        return "kernel modules"
    if 0x60000000 <= pc < 0x62000000:
        return "SDRAM phys (decompressor, pre-MMU)"
    return "user space"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("samples")
    ap.add_argument(
        "--elf",
        "--syms",
        dest="elf",
        help="kallsyms text or ELF (default: the kernel image's own kallsyms; with module symbols: "
        'a kernel.kallsyms dumped by dev/linux-shell; "none": regions only)',
    )
    ap.add_argument("--top", type=int, default=25)
    ap.add_argument("--phase", type=float, default=1.0, help="virtual seconds per phase row")
    a = ap.parse_args()
    rows = []
    for line in open(a.samples):
        p = line.split()
        if len(p) >= 3 and p[2].startswith("0x"):
            rows.append((vsecs(p[0]), float(p[1]), int(p[2], 16), p[4] if len(p) > 4 else "?"))
    if len(rows) < 3:
        raise SystemExit("too few samples")
    d = [(rows[i][0] - rows[i - 1][0], rows[i][1] - rows[i - 1][1], rows[i]) for i in range(1, len(rows))]
    base = sorted(h for _, h, _ in d)[len(d) // 20]  # 5th percentile = sampler overhead
    syms = load_syms(a.elf) if a.elf != "none" else None
    fn, ph, mode = collections.Counter(), collections.defaultdict(lambda: [0.0, 0.0]), collections.Counter()
    for dv, dh, (vt, _, pc, cpsr) in d:
        cost = max(0.0, dh - base)
        name = region(pc)
        if syms and pc >= 0xBF000000:
            i = bisect.bisect_right(syms[0], pc) - 1
            name = syms[1][i] if i >= 0 else "kernel?"
        fn[name] += cost
        k = int(vt // a.phase)
        ph[k][0] += dv
        ph[k][1] += cost
        try:
            mode[
                {0x10: "usr", 0x11: "fiq", 0x12: "irq", 0x13: "svc", 0x17: "abt", 0x1B: "und", 0x1F: "sys"}.get(
                    int(cpsr, 16) & 0x1F, "?"
                )
            ] += cost
        except ValueError:
            pass
    tot = sum(fn.values())
    print(
        "%d samples, sampler overhead %.3f s each (subtracted), emulation cost %.1f s host for %.1f s virtual"
        % (len(d), base, tot, rows[-1][0] - rows[0][0])
    )
    print("\nper %.0f s of virtual time: host s / virtual s" % a.phase)
    for k in sorted(ph):
        v, h = ph[k]
        print(
            "  %6.1f-%-6.1f %5.2f  %s"
            % (k * a.phase, (k + 1) * a.phase, h / v if v else 0, "#" * int(10 * h / max(v, 1e-9)))
        )
    print("\nby CPU mode: " + ", ".join("%s %.0f%%" % (m, 100 * c / tot) for m, c in mode.most_common()))
    print("\ntop %d (host s, share):" % a.top)
    for name, c in fn.most_common(a.top):
        print("  %7.2f %5.1f%%  %s" % (c, 100 * c / tot, name))


if __name__ == "__main__":
    main()
