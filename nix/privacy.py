#!/usr/bin/env python3
"""Fails on per-unit data in the repo's text files: a MAC or BSSID other than the test ones, a unit directory's path,
a home LAN address, a home directory. `nix flake check` runs it on the tracked files; by hand:
`python3 -I nix/privacy.py [ROOT]` (the files git tracks under ROOT, default the repo).

Allowed: the test MAC EC:66:D1:00:00:42 and B&W's OUI with zeros (EC:66:D1:00:00:xx, EE:... its locally administered
form), locally administered 02:..., all zeros or ones, and the few fixed values listed in ALLOWED_MACS. The emulator's
own network is 192.168.50.0/24. Examples use 192.0.2.0/24 and units/<unit>.
"""

import os
import re
import subprocess
import sys

# not ours to change: the vendored ASF; the vendor firmware's factory attributes (addresses from B&W's builds).
# And this file, whose patterns match themselves.
SKIP = re.compile(r"^(mcu/asf/|re/netmodule/cne_attr\.txt$|nix/privacy\.py$)")

# fixed values that are no one's unit: documentation examples and the vendor firmware's defaults
ALLOWED_MACS = {
    "00:11:22:33:44:55",  # an example
    "aa:bb:cc:dd:ee:ff",  # an example
    "00:21:6a:7d:74:fc",  # Snapcast's JSON-RPC documentation
    "00:07:f5:00:00:01",  # BL2's default MAC (a string in the firmware)
    "16:0b:04:02:02:02",  # the KnOS MIB's unused Bssid field
}

MAC = re.compile(r"(?<![0-9A-Za-z:.-])[0-9A-Fa-f]{2}([:-])(?:[0-9A-Fa-f]{2}\1){4}[0-9A-Fa-f]{2}(?![0-9A-Za-z:-])")
OK_MAC = re.compile(r"^(?:e[ce]:66:d1:00:00:[0-9a-f]{2}|02:.*|(?:00:){5}00|(?:ff:){5}ff)$")
RULES = [
    ("B&W MAC (compact)", re.compile(r"(?i)\bec66d1(?!0000)[0-9a-f]{6}\b")),
    ("unit directory", re.compile(r"units/zd1-")),
    ("home LAN address", re.compile(r"\b192\.168\.(?!50\.)\d{1,3}\.\d{1,3}\b")),
    ("home directory", re.compile(r"/Users/")),
]


def files(root):
    try:
        out = subprocess.run(["git", "-C", root, "ls-files", "-z"], capture_output=True, check=True)
        return [p for p in out.stdout.decode().split("\0") if p]
    except (OSError, subprocess.CalledProcessError):
        pass
    # a nix store copy of the tree: what git tracks, without .git
    return [os.path.relpath(os.path.join(d, f), root) for d, _, fs in os.walk(root) for f in fs]


def main():
    root = sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
    bad = 0
    for rel in sorted(files(root)):
        if SKIP.match(rel):
            continue
        path = os.path.join(root, rel)
        if os.path.islink(path) or not os.path.isfile(path):
            continue
        with open(path, "rb") as f:
            data = f.read()
        if b"\0" in data[:8192]:
            continue
        text = data.decode("utf-8", "replace")
        for n, line in enumerate(text.splitlines(), 1):
            hits = []
            for m in MAC.finditer(line):
                v = m.group(0).lower().replace("-", ":")
                if not OK_MAC.match(v) and v not in ALLOWED_MACS:
                    hits.append(("MAC", m.group(0)))
            hits += [(what, m.group(0)) for what, r in RULES for m in r.finditer(line)]
            for what, v in hits:
                print("%s:%d: %s %s" % (rel, n, what, v))
                bad += 1
    if bad:
        sys.exit("privacy: %d finding(s); use the test values (nix/privacy.py's docstring)" % bad)


if __name__ == "__main__":
    main()
