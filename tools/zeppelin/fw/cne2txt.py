#!/usr/bin/env python3
"""A BridgeCo CnE attribute blob ("[START_OF_ATTR_NAME_SPACE]"; "[Sect\\0 key\\0 val\\0 ... ]\\0") as indented text.

python3 -I tools/zeppelin/fw/cne2txt.py CNE.bin
"""

import argparse


def text(d):
    toks = d.split(b"\0")
    depth, out, i = 0, [], 0
    while i < len(toks):
        t = toks[i].decode("latin1")
        if t.startswith("["):
            out.append("  " * depth + t + "]")
            depth += 1
            i += 1
            continue
        if t == "]":
            depth = max(0, depth - 1)
            i += 1
            continue
        if t == "" and i + 1 < len(toks) and all(x == b"" or x == b"\xff" * len(x) for x in toks[i : i + 4]):
            i += 1
            continue
        v = toks[i + 1].decode("latin1") if i + 1 < len(toks) else ""
        if v.startswith("["):
            out.append("  " * depth + t + " = ")
            i += 1
            continue
        out.append("  " * depth + f"{t} = {v!r}")
        i += 2
    return "\n".join(out)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("cne", metavar="CNE.bin")
    print(text(open(ap.parse_args().cne, "rb").read()))


if __name__ == "__main__":
    main()
