#!/usr/bin/env python3
"""Read the running module's kernel log over JTAG (J-Link on the carrier's JTAG pads, OpenOCD, the real BL1/Linux).

    python3 -I tools/zeppelin/jtag/kmsg.py [--openocd PATH] [--out FILE] [--pc]

Halts the DM870 briefly, reads the Nokia 2.6.24.7 kernel's log_buf / log_buf_len / log_end (addresses from the
literal pools of log_buf_copy: the kernel's kallsyms have no static variables), dumps the buffer through the
kernel's own mapping, resumes, prints the log. --pc also prints where the core was (symbolised with kallsyms.py).
"""

import argparse
import os
import re
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import kallsyms  # noqa: E402

CFG = os.path.join(HERE, "dm870.cfg")
LOG_BUF_LEN, LOG_BUF, LOG_END = 0xC02B7C48, 0xC02B7C4C, 0xC02E4134  # 2.6.24.7 HD-1 kernel


def openocd(binary, cmds):
    r = subprocess.run(
        [binary, "-f", CFG] + sum((["-c", c] for c in cmds), []) + ["-c", "shutdown"], capture_output=True, text=True
    )
    return r.stdout + r.stderr


def words(out):
    w = {}
    for line in out.splitlines():
        m = re.match(r"0x([0-9a-f]+):\s+(.*)", line)
        if m:
            a = int(m.group(1), 16)
            for i, x in enumerate(m.group(2).split()):
                w[a + 4 * i] = int(x, 16)
    return w


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--openocd", default=os.environ.get("OPENOCD", "openocd"))
    ap.add_argument("--out", help="also save the raw ring buffer")
    ap.add_argument("--pc", action="store_true", help="print the halted PC and CPSR")
    a = ap.parse_args()
    out = openocd(
        a.openocd,
        [
            "init",
            "halt 2000",
            "reg pc",
            "reg cpsr",
            "mdw 0x%x 2" % LOG_BUF_LEN,
            "mdw 0x%x" % LOG_END,
            "resume",
        ],
    )
    if "tap/device found" not in out:
        sys.exit("no TAP: " + out[-600:])
    w = words(out)
    if LOG_BUF not in w:
        sys.exit("could not read the log variables (MMU off? not Linux?):\n" + out[-800:])
    buf, ln, end = w[LOG_BUF], w[LOG_BUF_LEN], w[LOG_END]
    if a.pc:
        pc = re.search(r"pc \(/32\): 0x([0-9a-f]+)", out)
        cpsr = re.search(r"cpsr \(/32\): 0x([0-9a-f]+)", out)
        where = kallsyms.Symbols(kallsyms.load()).at(int(pc.group(1), 16)) if pc else "? (no pc in OpenOCD's output)"
        print("# halted at %s, cpsr %s" % (where, cpsr.group(1) if cpsr else "?"))
    print("# log_buf %08x len %d log_end %d" % (buf, ln, end))
    with tempfile.TemporaryDirectory(prefix="kmsg-") as d:
        dump = a.out or os.path.join(d, "log_buf.bin")
        out = openocd(a.openocd, ["init", "halt 2000", "dump_image %s 0x%x %d" % (dump, buf, ln), "resume"])
        if "dumped" not in out:
            sys.exit("dump failed: " + out[-600:])
        b = open(dump, "rb").read()
    txt = b[:end] if end <= ln else bytes(b[(end - ln + i) % ln] for i in range(ln))
    sys.stdout.write(txt.decode("latin1"))


if __name__ == "__main__":
    main()
