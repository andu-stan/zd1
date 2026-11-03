"""Apply a Kconfig fragment to a .config: kconfig-merge.py .config FRAGMENT [--applets SRC LIST] (busybox.config: our
changes on top of `make defconfig`). Lines of the fragment replace the same symbol's line; new symbols are appended.
--applets: busybox's applets in LIST (one name per line) on, every other applet of the source tree SRC off (its
`//applet:IF_SYMBOL(... APPLET(name, ...` lines map names to symbols); `make oldconfig` then drops the options of the
applets that are off."""

import os
import re
import sys

args = sys.argv[1:]
applets = None
if "--applets" in args:
    i = args.index("--applets")
    src, listfile = args[i + 1 : i + 3]
    del args[i : i + 3]
    applets = {}
    for dp, _, fs in os.walk(src):
        for f in fs:
            if f.endswith((".c", ".h", ".src")):
                for line in open(os.path.join(dp, f), errors="ignore"):
                    m = re.search(r"//applet:.*?IF_(\w+)\(.*?APPLET\w*\(\s*([^,\s]+)\s*,", line)
                    if m:
                        applets.setdefault(m.group(2), m.group(1))
    want_names = [l.strip() for l in open(listfile) if l.strip() and not l.startswith("#")]
    unknown = [n for n in want_names if n not in applets]
    if unknown:
        sys.exit("kconfig-merge: not busybox applets: " + " ".join(unknown))
    on = {applets[n] for n in want_names}
cfg, frag = args[:2]
want = {}
if applets is not None:
    for sym in set(applets.values()):
        want["CONFIG_" + sym] = ("CONFIG_%s=y" % sym) if sym in on else ("# CONFIG_%s is not set" % sym)
for line in open(frag):
    line = line.strip()
    m = re.match(r"^(CONFIG_\w+)=(.*)$", line) or re.match(r"^# (CONFIG_\w+) is not set$", line)
    if m:
        want[m.group(1)] = line
out = []
for line in open(cfg):
    m = re.match(r"^(CONFIG_\w+)=", line) or re.match(r"^# (CONFIG_\w+) is not set", line)
    if m and m.group(1) in want:
        line = want.pop(m.group(1)) + "\n"
    out.append(line)
out += [v + "\n" for v in want.values()]
open(cfg, "w").writelines(out)
