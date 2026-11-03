#!/usr/bin/env python3
"""Symbolise a profile from prof.c (a C program linked with it, e.g. the AAC bench with -DPROF): pairs of u32 (PC, LR).
Needs the unstripped binary's symbol table (llvm-nm reads ARM ELF on any host; `nix develop` has it):

  llvm-nm -C --defined-only -n BINARY > syms.txt
  python3 -I module/os/debug/profsym.py syms.txt prof.bin [TOP] [--callers FUNC]
"""

import bisect
import collections
import struct
import sys


def main():
    syms_path, prof_path = sys.argv[1], sys.argv[2]
    top = int(sys.argv[3]) if len(sys.argv) > 3 and sys.argv[3].isdigit() else 30
    callers_of = sys.argv[sys.argv.index("--callers") + 1] if "--callers" in sys.argv else None
    syms = []
    for line in open(syms_path):
        p = line.split(" ", 2)
        if len(p) == 3 and p[1] in "tTwW":
            syms.append((int(p[0], 16), p[2].strip()))
    syms.sort()
    addrs = [a for a, _ in syms]

    def name(a):
        i = bisect.bisect_right(addrs, a) - 1
        return syms[i][1].split("(")[0] if i >= 0 else "?"

    d = open(prof_path, "rb").read()
    s = [struct.unpack_from("<II", d, i) for i in range(0, len(d) - 7, 8)]
    n = len(s) or 1
    print("%d samples" % len(s))
    for f, c in collections.Counter(name(pc) for pc, _ in s).most_common(top):
        print("%5.1f%%  %s" % (100 * c / n, f[:120]))
    if callers_of:
        c = collections.Counter(name(lr) for pc, lr in s if name(pc) == callers_of)
        print("callers of %s:" % callers_of)
        for f, k in c.most_common(10):
            print("  %5d  %s" % (k, f[:120]))


if __name__ == "__main__":
    main()
