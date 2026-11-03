# usage: fn.py decomp.c addr [maxlines]
import sys

f = sys.argv[1]
a = sys.argv[2].lower().replace("0x", "")
mx = int(sys.argv[3]) if len(sys.argv) > 3 else 150
out = []
on = False
for line in open(f):
    if line.startswith("// ===== "):
        if on:
            break
        on = line.strip().endswith("@ " + a)
    if on:
        out.append(line.rstrip("\n"))
print("\n".join(out[:mx]))
if len(out) > mx:
    print(f"... ({len(out)} lines total)")
