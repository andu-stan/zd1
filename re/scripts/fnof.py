# usage: fnof.py functions.txt addr... -> containing function
import bisect
import sys

fs = []
for l in open(sys.argv[1]):
    p = l.split()
    if len(p) >= 3:
        fs.append((int(p[0], 16), p[1], int(p[2])))
fs.sort()
st = [f[0] for f in fs]
for a in sys.argv[2:]:
    a = int(a, 16)
    i = bisect.bisect_right(st, a) - 1
    f = fs[i]
    print(hex(a), f[1], "size", f[2], "(inside)" if a < f[0] + f[2] else "(past end)")
