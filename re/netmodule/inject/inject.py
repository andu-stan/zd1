#!/usr/bin/env python3
"""Hardened injector: run a PIC Thumb payload once on the DM870 via the `guard` command's execute
(vtable slot 7), cloning the vtable so lookup/help stay valid, and (payload-side) CHAINING to the
original execute so the command completes exactly as stock — minimizing module->MCU heartbeat
disruption (which otherwise flickers the MCU-driven LED / can look like a reboot). Fully reversible.

ABI (proven): command vtable methods are Thumb; app shared-lib funcs are ARM (call via blx to EVEN
addr). Heap (os mlc HEAP) is executable. guard obj ptr global @0x60016b60; execute = slot 7.
Keep payloads fast (<<60 s) — a stalled module trips the MCU's 60 s supervision -> full-speaker reset.

run(payload_bytes, placeholders, strings, reads, bufsize, ip):
  placeholders: {literal_u32: ("buf",off) | ("origexec",) | ("abs",addr)} patched in the blob.
  strings:      {buf_off: bytes}  written into the buffer.
  reads:        [(buf_off, nwords)] returned after the trigger.
Returns (guard_output, {buf_off: [words]}, B).  Default target 169.254.1.1 (wired, link-local)."""

import os
import re
import struct
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "..", "tools"))
from zeppelin.knos import knos  # noqa: E402

OBJ_PTR_GLOBAL = 0x60016B60
EXEC_SLOT = 7


def run(payload, placeholders=None, strings=None, reads=None, bufsize=512, ip=None):
    blob = bytearray(payload)
    placeholders = placeholders or {}
    k = knos.connect(ip)
    rd1 = lambda a: int(k.cmd("rd 0x%x 1" % a, 6).split(":")[1].split()[0], 16)
    rdn = lambda a, n: [int(t, 16) for t in re.findall(r"0x[0-9a-f]{8}(?!:)", k.cmd("rd 0x%x %d" % (a, n), 6))]
    wr = lambda a, v: k.cmd("wr 0x%x 0x%x" % (a, v & 0xFFFFFFFF), 6)
    obj = vt = None
    try:
        obj = rd1(OBJ_PTR_GLOBAL)
        vt = rd1(obj)
        vwords = rdn(vt, 8)
        B = int(k.cmd("os mlc %d HEAP" % bufsize, 6).split("ptr=")[1].split()[0], 16)
        code = B + 0x10
        fvt = B + 0x80
        for val, spec in placeholders.items():
            i = blob.find(struct.pack("<I", val))
            if i < 0:
                raise ValueError("placeholder 0x%x not found" % val)
            tgt = B + spec[1] if spec[0] == "buf" else vwords[7] if spec[0] == "origexec" else spec[1]
            struct.pack_into("<I", blob, i, tgt)
        for i in range(0, len(blob), 4):
            wr(code + i, struct.unpack_from("<I", blob + b"\0\0\0", i)[0])
        for off, b in (strings or {}).items():
            b = bytes(b) + b"\0" * ((-len(b)) % 4)
            for j in range(0, len(b), 4):
                wr(B + off + j, struct.unpack_from("<I", b, j)[0])
        for off, n in reads or []:
            for j in range(0, n * 4, 4):
                wr(B + off + j, 0)
        for i, w in enumerate(vwords[:8]):
            wr(fvt + 4 * i, w)
        wr(fvt + 4 * EXEC_SLOT, code | 1)
        wr(obj, fvt)
        out = k.cmd("guard", 8)
        res = {off: rdn(B + off, n) for off, n in (reads or [])}
        return out, res, B
    finally:
        if obj and vt:
            try:
                wr(obj, vt)
            except Exception:
                pass
        k.close()


if __name__ == "__main__":
    # demo: read NAND[0..15] via the module's flash driver, chaining to original execute
    blob = bytearray(open(os.path.join(os.path.dirname(__file__), "nandread.bin"), "rb").read())
    out, res, B = run(blob, placeholders={0xDD000000: ("buf", 0xA0), 0xEE000007: ("origexec",)}, reads=[(0xA0, 4)])
    print("NAND[0..15]:", " ".join("0x%08x" % w for w in res[0xA0]))
