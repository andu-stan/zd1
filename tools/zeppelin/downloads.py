"""The downloads/ archive: vendor firmware, installers and documents, kept on archive.org (downloads/manifest.json).

  downloads list [PREFIX...]     what the manifest has (path, size, here or not)
  downloads fetch [PREFIX...]    download into downloads/<path> from archive.org, checked against the sha256
  downloads verify [PREFIX...]   check the files that are here

PREFIX selects by path, e.g. `bw-zeppelin-air-software/zeppelin-air/firmware-2.04.05` or `bridgeco-jukeblox-firmware/bose`.
The files are untrusted data: read them with `python3 -I`, never run them outside the emulator.
"""

import argparse
import hashlib
import json
import os
import sys
import urllib.parse
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from paths import DOWNLOADS  # noqa: E402

MANIFEST = os.path.join(DOWNLOADS, "manifest.json")


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def url(f):
    item, name = f["path"].split("/", 1)
    return "https://archive.org/download/%s/%s" % (item, urllib.parse.quote(name))


def fetch(f, local):
    os.makedirs(os.path.dirname(local), exist_ok=True)
    tmp = local + ".part"
    h = hashlib.sha256()
    with urllib.request.urlopen(url(f)) as r, open(tmp, "wb") as out:
        for b in iter(lambda: r.read(1 << 20), b""):
            h.update(b)
            out.write(b)
    if h.hexdigest() != f["sha256"]:
        os.remove(tmp)
        raise SystemExit("%s: sha256 mismatch" % f["path"])
    os.replace(tmp, local)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("cmd", choices=["list", "fetch", "verify"])
    ap.add_argument("prefix", nargs="*")
    a = ap.parse_args()
    files = [
        f
        for f in json.load(open(MANIFEST))["files"]
        if not a.prefix or any(f["path"].startswith(p.rstrip("/")) for p in a.prefix)
    ]
    if not files:
        sys.exit("nothing in the manifest matches")
    bad = 0
    for f in files:
        local = os.path.join(DOWNLOADS, f["path"])
        here = os.path.exists(local)
        if a.cmd == "list":
            print("%s %10d  %s" % ("*" if here else " ", f["size"], f["path"]))
        elif a.cmd == "verify":
            if here:
                ok = sha256(local) == f["sha256"]
                bad += not ok
                print("%s  %s" % ("ok " if ok else "BAD", f["path"]))
        elif not here:
            print("fetch %s (%d bytes)" % (f["path"], f["size"]), flush=True)
            fetch(f, local)
    if bad:
        sys.exit("%d file(s) differ from the manifest" % bad)


if __name__ == "__main__":
    main()
