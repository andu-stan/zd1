#!/usr/bin/env python3
"""Extract the BridgeCo FFS resources partition (the stock app's web UI and its other files) by heuristics: entries
are "<path>\\0<content>", the content running to the next path. Paths that would leave OUTDIR are skipped.

    python3 -I tools/zeppelin/fw/ffsres_extract.py RESOURCES.bin OUTDIR
"""

import argparse
import os
import re
import sys


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("image", metavar="RESOURCES.bin")
    ap.add_argument("out", metavar="OUTDIR")
    a = ap.parse_args()
    d = open(a.image, "rb").read()
    out = os.path.realpath(a.out)
    ms = list(re.finditer(rb"\x00(/[A-Za-z0-9_./\-]+\.(?:asp|html?|css|js|png|gif|jpg|xml|ico|txt|json))\x00", d))
    for i, m in enumerate(ms):
        name = m.group(1).decode()
        p = os.path.realpath(os.path.join(out, name.lstrip("/")))
        if not p.startswith(out + os.sep):
            print("skipped %s: outside %s" % (name, out), file=sys.stderr)
            continue
        e = ms[i + 1].start() + 1 if i + 1 < len(ms) else len(d)
        body = d[m.end() : e]
        if not name.endswith((".png", ".gif", ".jpg", ".ico")):
            body = body.split(b"\0")[0]
        os.makedirs(os.path.dirname(p), exist_ok=True)
        open(p, "wb").write(body)
        print(name, len(body))


if __name__ == "__main__":
    main()
