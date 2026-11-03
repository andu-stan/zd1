#!/usr/bin/env python3
"""NAND over the KnOS shell's `fburn` (docs/module/knos.md): read with `fburn rd` (XModem-CRC out of the module, 128-
and 1024-byte packets), write with `fburn ebin` (erase the range, then XModem-1K in, programmed through the module's
own NAND driver: ECC, bad blocks skipped, addresses logical). The stock app must be running with telnet on.

    fburn.py rd FLASH_ID ADDR LENGTH OUT [--host HOST[:PORT]]
    fburn.py wr FLASH_ID ADDR LENGTH IN [--host HOST[:PORT]] [--yes] [--verify] [--allow-bl2]

  e.g. fburn.py rd 0 0x20000 0x20000 persparam.bin        (the PersParam block)
       fburn.py wr 0 0x20000 0x20000 persparam.bin --yes
HOST as for knos.py ($ZD1_IP, else 169.254.1.1). wr: LENGTH is the range erased (whole 128 KB blocks); IN may be
shorter (the rest stays erased); --verify reads the range back and compares; without --yes it only says what it
would do.

Over telnet `fburn ebin` never sees the "y" to its confirmation: the shell reads it from its own device
(docs/module/boot-chain.md, "Writing NAND from the stock app"), so wr stops there on the unit.

wr WRITES FLASH. NAND block 0 (BL1, encrypted, unrecoverable without the backup) is refused. The BL2 partition
(0xA0000-0x1FFFFF, the module's recovery path: its web, XModem and TFTP updater) needs --allow-bl2.
"""

import argparse
import os
import sys
import time

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
from zeppelin import xmodem  # noqa: E402
from zeppelin.knos import knos  # noqa: E402

BLOCK = 0x20000
BL2_START, BL2_END = 0xA0000, 0x200000


def getc(s, secs):
    s.settimeout(max(secs, 0.001))
    try:
        return s.recv(1)
    except TimeoutError:
        return b""
    finally:
        s.settimeout(1.0)


def wait_c(s):
    """the receiver's first 'C', on a line of its own ("[Ctrl-D to cancel]" comes before it)"""
    prev, t0 = b"\n", time.time()
    while True:
        c = getc(s, 1)
        if c == b"C" and prev in (b"\n", b"C"):
            return
        prev = c or prev
        if time.time() - t0 > 60:
            raise TimeoutError("no 'C' from the receiver")


def progress(n, total):
    if n % 64 == 0 or n == total:
        sys.stderr.write("\r  block %d/%d%s" % (n, total, "\n" if n == total else ""))
        sys.stderr.flush()


def read(host, fid, addr, length):
    sh = knos.connect(host)
    try:
        sh.s.sendall(b"fburn rd %d 0x%x 0x%x\r\n" % (fid, addr, length))
        # the banner first, so none of its text is taken for XModem bytes
        sh.wait_for(b"xmodem now", 8)
        time.sleep(0.2)
        return xmodem.recv(sh.s.sendall, lambda secs: getc(sh.s, secs), length)
    finally:
        sh.close()


def write(host, fid, addr, length, data):
    sh = knos.connect(host)
    try:
        sh.s.sendall(b"fburn ebin %d 0x%x 0x%x\r\n" % (fid, addr, length))
        sh.wait_for(b"[y/n]", 10)
        sh.s.sendall(b"y")
        sh.wait_for(b"XModem now", 120)  # after the erase
        wait_c(sh.s)
        n = xmodem.send(sh.s.sendall, lambda secs: getc(sh.s, secs), data, timeout=15, progress=progress)
        out = sh.wait_for(b"bytes burnt", 120)
        return n, out.split(b"\n")[-1].strip().decode("latin-1")
    finally:
        sh.close()


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("op", choices=["rd", "wr"])
    ap.add_argument("fid", type=lambda v: int(v, 0), metavar="FLASH_ID")
    ap.add_argument("addr", type=lambda v: int(v, 0), metavar="ADDR")
    ap.add_argument("length", type=lambda v: int(v, 0), metavar="LENGTH")
    ap.add_argument("file", metavar="OUT|IN")
    ap.add_argument("--host", help="HOST[:PORT] (default $ZD1_IP or 169.254.1.1, port 23)")
    ap.add_argument("--yes", action="store_true", help="wr: erase and program (else a dry run)")
    ap.add_argument("--verify", action="store_true", help="wr: read the range back and compare")
    ap.add_argument("--allow-bl2", action="store_true", help="wr: allow the BL2 partition")
    a = ap.parse_args()
    if a.op == "rd":
        data = read(a.host, a.fid, a.addr, a.length)
        open(a.file, "wb").write(data)
        print("wrote %s: %d bytes (requested 0x%x)" % (a.file, len(data), a.length))
        return
    data = open(a.file, "rb").read()
    if a.fid == 0 and a.addr < BLOCK:
        sys.exit("refusing: NAND block 0 is BL1")
    if a.fid == 0 and a.addr < BL2_END and a.addr + a.length > BL2_START and not a.allow_bl2:
        sys.exit(
            "refusing: 0x%x-0x%x overlaps the BL2 partition (the recovery path); pass --allow-bl2 if you mean it"
            % (a.addr, a.addr + a.length)
        )
    if a.addr % BLOCK or a.length % BLOCK or len(data) > a.length:
        sys.exit("addr/length must be whole 128 KB blocks and the file must fit (%d > %d?)" % (len(data), a.length))
    print("fburn ebin %d 0x%x 0x%x <- %s (%d bytes)" % (a.fid, a.addr, a.length, a.file, len(data)))
    if not a.yes:
        sys.exit("dry run. re-run with --yes to erase + program the range.")
    n, said = write(a.host, a.fid, a.addr, a.length, data)
    print("sent %d blocks; module: %s" % (n, said))
    if a.verify:
        time.sleep(1)
        back = read(a.host, a.fid, a.addr, len(data))
        if back != data:
            bad = next((i for i in range(min(len(back), len(data))) if back[i] != data[i]), min(len(back), len(data)))
            sys.exit("VERIFY FAILED at +0x%x (read %d bytes)" % (bad, len(back)))
        print("VERIFY OK (%d bytes)" % len(back))


if __name__ == "__main__":
    main()
