#!/usr/bin/env python3
"""Pack a Linux kernel for the DM870 app slot: the stage-2 payload the stock preloader loads to and enters at
0x60008000 (then `tools/zeppelin/fw/mkbcd.py` wraps it into a .bcd with the stock preloader + checksums).

Layout (offsets from 0x60008000):
  0x000  trampoline: copies the stub to 0x61F00000 and jumps there (position independent, 12 words)
  0x040  header 'ZLB1': +0x08 stub off, +0x0C stub len, +0x10 kernel off, +0x14 kernel len, +0x18 initrd off,
         +0x1C initrd len, +0x20 reserved (0), +0x24 machine id, +0x28 initrd destination
         (0 = the stub's default 0x61800000), +0x2C flags (bit 0: system slots, the initramfs is the rescue: the stub
         asks the MCU which to start), +0x30 the kernel's inflated length (0: stored as it is; else +0x14 is the
         length of its raw deflate stream), +0x40 kernel command line
  0x200  stub (module/boot, bin linuxboot, linked at 0x61F00000)
  ...    kernel Image (4 KiB aligned), initramfs (4 KiB aligned)
The stub moves the initramfs to its destination and the kernel down to 0x60008000, writes ATAGs, jumps. The
initramfs may be gzip'd or a plain cpio (the kernel takes both); a plain one (emulator dev boots: no inflate) is
~8 MB and goes lower (--initrd-at 0x61000000).

usage: mkpayload.py STUB.bin KERNEL.bin INITRD OUT.bin [--cmdline S] [--mach ID] [--initrd-at A] [--slots]
       [--deflate-kernel]
"""

import argparse
import struct

TRAMPOLINE = [
    0xE24F4008,  # sub  r4, pc, #8         r4 = image base (this instruction)
    0xE5945048,  # ldr  r5, [r4, #0x48]    stub offset
    0xE594604C,  # ldr  r6, [r4, #0x4c]    stub length
    0xE0845005,  # add  r5, r4, r5
    0xE59F7018,  # ldr  r7, [pc, #0x18]    -> literal at 0x30
    0xE1A08007,  # mov  r8, r7
    0xE4950004,  # 1: ldr r0, [r5], #4
    0xE4870004,  #    str r0, [r7], #4
    0xE2566004,  #    subs r6, r6, #4
    0xCAFFFFFB,  #    bgt 1b
    0xE1A0F008,  # mov  pc, r8
    0xE1A00000,  # nop
    0x61F00000,  # literal: stub link address
]
STUB_OFF, ALIGN, MAX_STUB = 0x200, 0x1000, 0x100000


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("stub")
    ap.add_argument("kernel")
    ap.add_argument("initrd")
    ap.add_argument("out")
    ap.add_argument("--cmdline", default="console=ttyS1,115200 mem=32M")
    ap.add_argument("--mach", type=lambda x: int(x, 0), default=0x270F)
    ap.add_argument("--initrd-at", type=lambda x: int(x, 0), default=0x61800000)
    ap.add_argument(
        "--slots", action="store_true", help="the initramfs is the rescue: the stub asks the MCU for a slot"
    )
    ap.add_argument("--deflate-kernel", action="store_true", help="the kernel as raw deflate: the stub inflates it")
    a = ap.parse_args()
    stub, kernel = open(a.stub, "rb").read(), open(a.kernel, "rb").read()
    initrd = open(a.initrd, "rb").read() if a.initrd != "-" else b""
    assert kernel[:4] == bytes.fromhex("d3f021e3"), "kernel is not an ARM Image starting with msr cpsr_c, #0xd3"
    inflated = 0
    if a.deflate_kernel:
        import zlib

        z = zlib.compressobj(9, zlib.DEFLATED, -15, 9)
        inflated, kernel = len(kernel), z.compress(kernel) + z.flush()
    assert len(stub) < MAX_STUB and len(initrd) < 0x61F00000 - a.initrd_at, "initramfs does not fit below the stub"
    cmd = a.cmdline.encode() + b"\0"
    assert len(cmd) <= 0x1C0 - 0x40
    out = bytearray(struct.pack("<13I", *TRAMPOLINE)).ljust(0x40, b"\0")
    pad = lambda n: (n + ALIGN - 1) // ALIGN * ALIGN
    k_off = pad(STUB_OFF + len(stub))
    i_off = pad(k_off + len(kernel))
    stub_len = (len(stub) + 3) & ~3
    out += struct.pack("<4s7I", b"ZLB1", 1, STUB_OFF, stub_len, k_off, len(kernel), i_off, len(initrd))
    out += struct.pack("<5I", 0, a.mach, 0 if a.initrd_at == 0x61800000 else a.initrd_at, 1 if a.slots else 0, inflated)
    out = out.ljust(0x80, b"\0") + cmd
    out = out.ljust(STUB_OFF, b"\0") + stub
    out = out.ljust(k_off, b"\0") + kernel
    out = out.ljust(i_off, b"\0") + initrd
    # the kernel lands at 0x60008000 and may grow into BSS: keep the initramfs destination clear of everything
    assert 0x60008000 + len(out) < a.initrd_at, "payload overlaps the initramfs destination"
    open(a.out, "wb").write(out)
    print(
        f"{a.out}: {len(out)} B (stub {len(stub)} @0x{STUB_OFF:x}, kernel {len(kernel)} @0x{k_off:x}, "
        f"initrd {len(initrd)} @0x{i_off:x} -> 0x{a.initrd_at:x})"
    )


if __name__ == "__main__":
    main()
