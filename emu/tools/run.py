#!/usr/bin/env python3
"""Run a Renode script line by line through the zemu driver (stops at the first error, prints every output).

python3 emu/tools/run.py script.resc [--log run.log]
"""

import os
import signal
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
from zemu.rig import Renode  # noqa: E402


def main():
    script = sys.argv[1]
    d = os.path.dirname(os.path.abspath(sys.argv[2] if len(sys.argv) > 2 else script))
    r = Renode(d)
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(1))  # timeout(1) etc.: still close (kill) Renode
    try:
        for line in open(script):
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            if line == "quit":
                break
            out = r.cmd(line, timeout=3600)
            text = "\n".join(
                l
                for l in out.replace("\r", "").split("\n")
                if l.strip() and not l.startswith("(") and 'echo "@@zemu' not in l
            )
            if text.strip():
                print(text.strip(), flush=True)
    finally:
        r.close()


if __name__ == "__main__":
    main()
