#!/usr/bin/env python3
"""Breakpoint tracer for a GDB remote stub (no gdb needed; `zemu run --gdb 3333 --keep`, then
gdbtrace.py 127.0.0.1:3333 ..., or as a step: `zemu run --gdb 3333 --host "python3 emu/tools/gdbtrace.py ..."`): logs
every hit of the given ARM addresses with r0-r3, sp, lr (and optional memory words), then lets the guest run on.

usage: gdbtrace.py host:port [-n MAXHITS] [-t SECS] [-o LOG] spec...
  spec = addr[:name][@mem,mem...]   mem = rN+off or a hex address: dump that 32-bit word at each hit
         addr may also be 'w:addr' = write watchpoint (logs the pc that wrote it)
  -n  stop logging a breakpoint (and remove it) after MAXHITS hits (default 20)
  -t  detach after SECS seconds (default: run until the target exits)
Every breakpoint costs a stop of the whole guest, so keep hot paths out or use a small -n.
"""

import argparse
import socket
import struct
import sys
import time

ap = argparse.ArgumentParser()
ap.add_argument("target")
ap.add_argument("spec", nargs="+")
ap.add_argument("-n", type=int, default=20)
ap.add_argument("-t", type=float, default=0)
ap.add_argument("-o", default="-")
a = ap.parse_args()
out = sys.stdout if a.o == "-" else open(a.o, "w", buffering=1)
host, port = a.target.rsplit(":", 1)

for _i in range(300):
    try:
        s = socket.create_connection((host, int(port)))
        break
    except OSError:
        time.sleep(1)
else:
    sys.exit("gdbtrace: cannot connect")
buf = b""


def send(p):
    cs = sum(p.encode()) & 0xFF
    s.sendall(b"$" + p.encode() + b"#%02x" % cs)


def recv(timeout=None):
    """next packet payload (acks '+' are skipped; we ack everything)"""
    global buf
    s.settimeout(timeout)
    while True:
        i = buf.find(b"$")
        if i >= 0:
            j = buf.find(b"#", i)
            if j >= 0 and len(buf) >= j + 3:
                p = buf[i + 1 : j].decode(errors="replace")
                buf = buf[j + 3 :]
                s.sendall(b"+")
                return p
        try:
            d = s.recv(65536)
        except TimeoutError:
            return None
        if not d:
            raise EOFError
        buf += d


def cmd(p):
    """send a command and return its reply, skipping stray stop replies (T/S packets) unless one is expected"""
    send(p)
    while True:
        r = recv()
        if p in ("?", "s", "c") or not (r.startswith("T") or r.startswith("S")):
            return r


def regs():
    g = cmd("g")
    return [struct.unpack("<I", bytes.fromhex(g[i * 8 : i * 8 + 8]))[0] for i in range(16)]


def mem(addr):
    m = cmd("m%x,4" % addr)
    return struct.unpack("<I", bytes.fromhex(m))[0] if m and not m.startswith("E") and len(m) == 8 else None


bps = {}
for sp in a.spec:
    kind = "0"
    if sp.startswith("w:"):
        kind, sp = "2", sp[2:]
    sp, _, mems = sp.partition("@")
    ad, _, name = sp.partition(":")
    ad = int(ad, 16)
    bps[ad] = dict(name=name or "%08x" % ad, kind=kind, mems=[m for m in mems.split(",") if m], hits=0)

s.sendall(b"+")
cmd("qSupported")
stop = cmd("?")
for ad, b in bps.items():
    r = cmd("Z%s,%x,4" % (b["kind"], ad))
    if r != "OK":
        print("gdbtrace: cannot set %s: %r" % (b["name"], r), file=out)
t0 = time.time()
send("c")
while True:
    left = (a.t - (time.time() - t0)) if a.t else None
    if left is not None and left <= 0:
        s.sendall(b"\x03")
        recv(5)
        break
    try:
        p = recv(left if left is not None else None)
    except EOFError:
        break
    if p is None:
        continue
    if p.startswith("W") or p.startswith("X"):
        break
    if not p.startswith("T") and not p.startswith("S"):
        continue
    r = regs()
    pc = r[15]
    hit = bps.get(pc)
    wad = None
    if "watch:" in p:
        wad = int(p.split("watch:")[1].split(";")[0], 16)
        hit = bps.get(wad)
    if hit:
        hit["hits"] += 1
        extra = ""
        for m in hit["mems"]:
            if m.startswith("r"):
                rn, _, off = m[1:].partition("+")
                ad2 = r[int(rn)] + (int(off, 16) if off else 0)
            else:
                ad2 = int(m, 16)
            v = mem(ad2)
            extra += " [%s]=%s" % (m, "%08x" % v if v is not None else "?")
        print(
            "%8.2f HIT %-22s pc=%08x r0=%08x r1=%08x r2=%08x r3=%08x sp=%08x lr=%08x%s"
            % (time.time() - t0, hit["name"], pc, r[0], r[1], r[2], r[3], r[13], r[14], extra),
            file=out,
        )
        ad = wad if wad is not None else pc
        if hit["hits"] >= a.n:
            cmd("z%s,%x,4" % (hit["kind"], ad))
            print("%8.2f ---- %s: %d hits, removed" % (time.time() - t0, hit["name"], hit["hits"]), file=out)
            send("c")
            continue
        # step over the breakpoint, then re-arm it
        cmd("z%s,%x,4" % (hit["kind"], ad))
        send("s")
        while True:
            q = recv()
            if q and (q.startswith("T") or q.startswith("S")):
                break
        cmd("Z%s,%x,4" % (hit["kind"], ad))
    send("c")
for b in bps.values():
    print("summary %-22s %d hits" % (b["name"], b["hits"]), file=out)
try:
    for ad, b in bps.items():
        cmd("z%s,%x,4" % (b["kind"], ad))
    send("D")
except Exception:
    pass
