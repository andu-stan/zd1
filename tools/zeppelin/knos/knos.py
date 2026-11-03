#!/usr/bin/env python3
"""The KnOS shell on telnet (the stock module app's; our OS's zd1-memsh answers `rd`/`wr` the same way): the one
client every tool uses (docs/module/knos.md).

What the shell needs:
  - It submits a line on CR: always send CRLF. A lone LF never submits and wedges the line buffer.
  - The line buffer and any XModem transfer are one state shared by every connection, so a session starts with
    flush(): CANs cancel a stuck XModem sender, a CRLF gives a clean prompt.
  - It echoes character by character with backspaces: cmd() applies them and cuts the echoed command off.

    python3 -I tools/zeppelin/knos/knos.py [--host HOST[:PORT]] CMD...     each command's output

As a library: `sh = knos.connect(); sh.cmd("os timestamp")` -> the output. HOST defaults to $ZD1_IP, else 169.254.1.1
(the module's link-local address on a wired dongle); PORT to 23. In the emulator: `zemu run stock --keep` and
127.0.0.1:4555, or our OS's zd1-memsh (the debug system): `zemu run --fwd 4557:23 --keep`, 127.0.0.1:4557.
"""

import argparse
import os
import socket
import time

PROMPT = b"sds://>"
CAN = 0x18


def target(host=None):
    """(host, port) from "HOST[:PORT]", $ZD1_IP or 169.254.1.1."""
    host, _, port = (host or os.environ.get("ZD1_IP") or "169.254.1.1").partition(":")
    return host, int(port or 23)


def clean(raw, cmd=""):
    """The shell's output with its backspaces applied, CRs and the prompt dropped, and everything up to the echoed
    `cmd` cut off."""
    out = []
    for ch in raw.decode("latin1"):
        if ch == "\x08":
            if out:
                out.pop()
        elif ch != "\r":
            out.append(ch)
    out = "".join(out)
    i = out.find(cmd) if cmd else -1
    if i >= 0:
        out = out[i + len(cmd) :]
    return out.replace(PROMPT.decode(), "").strip("\n")


class Shell:
    def __init__(self, sock):
        self.s = sock
        self.s.settimeout(1.0)

    def recv_for(self, secs):
        """What arrives within `secs`, ending early on a quiet second once something came."""
        got, t0 = b"", time.time()
        while time.time() - t0 < secs:
            try:
                d = self.s.recv(8192)
                if not d:
                    break
                got += d
            except TimeoutError:
                if got:
                    break
        return got

    def flush(self):
        """Cancel any stuck XModem sender, then a clean prompt."""
        self.s.sendall(bytes([CAN] * 5))
        self.recv_for(0.8)
        self.s.sendall(b"\r\n")
        self.recv_for(0.8)

    def read_to_prompt(self, secs):
        buf, t0 = b"", time.time()
        while time.time() - t0 < secs:
            try:
                d = self.s.recv(8192)
                if not d:
                    break
                buf += d
            except TimeoutError:
                pass
            if buf.rstrip().endswith(PROMPT):
                break
        return buf

    def wait_for(self, text, secs):
        """Read until `text` (bytes) arrives; TimeoutError with the last of what came instead."""
        buf, t0 = b"", time.time()
        while text not in buf:
            if time.time() - t0 > secs:
                raise TimeoutError("no %r; got %r" % (text, buf[-160:]))
            try:
                d = self.s.recv(256)
                if not d:
                    raise ConnectionError("closed")
                buf += d
            except TimeoutError:
                pass
        return buf

    def cmd(self, c, timeout=12.0, settle=0.2):
        """Run `c`: its output once the prompt is back (at most `timeout` s). `settle` s of whatever is still
        arriving are dropped first (0: none, for a quick series of commands). Without a prompt the shell is flushed,
        so the next command starts clean."""
        if settle:
            self.recv_for(settle)
        self.s.sendall(c.encode() + b"\r\n")
        raw = self.read_to_prompt(timeout)
        if not raw.rstrip().endswith(PROMPT):
            self.flush()
        return clean(raw, c)

    def close(self):
        try:
            self.s.close()
        except OSError:
            pass


def connect(host=None, timeout=5, tries=1):
    """A flushed session. The shell takes one session at a time and refuses a new one for a moment after the last
    closed: `tries` attempts 2 s apart."""
    for attempt in range(tries):
        try:
            sh = Shell(socket.create_connection(target(host), timeout=timeout))
            break
        except OSError:
            if attempt == tries - 1:
                raise
            time.sleep(2)
    sh.flush()
    return sh


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--host", help="HOST[:PORT] (default $ZD1_IP or 169.254.1.1, port 23)")
    ap.add_argument("-t", "--timeout", type=float, default=12.0, help="seconds per command")
    ap.add_argument("cmds", nargs="+", metavar="CMD")
    a = ap.parse_args()
    sh = connect(a.host)
    try:
        for c in a.cmds:
            print("\n$ %s\n%s" % (c, sh.cmd(c, a.timeout)))
    finally:
        sh.close()


if __name__ == "__main__":
    main()
