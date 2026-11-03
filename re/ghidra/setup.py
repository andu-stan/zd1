#!/usr/bin/env python3
"""Rebuild the Ghidra analyses of the vendor images from blobs/, headless: re/gen/<image>/ (gitignored).

Each image is cut from its blob into re/gen/<image>/image.bin, imported at its base with the right processor, a
pre-script lays out the memory map (SRAM/SDRAM/peripheral blocks), and post-scripts decompile everything into
decomp_all.c / functions.txt (+ disasm.txt for the MCU, string_xrefs.txt for the module). The MCU app also gets
decomp_orphans.c (functions only reachable through pointers) and decomp_resolved.c, both with the literal pools
resolved (re/scripts/resolve_decomp.py). Every image gets strings.txt (offset in hex, then the string). The Ghidra
projects land in re/gen/ghidra/.

  python3 -I re/ghidra/setup.py [image...]         (default: all; `list` prints the table)

Needs Ghidra (`nix develop .#re` sets GHIDRA_HOME, or have `analyzeHeadless` on PATH). The two updater images come
from downloads/ (`python3 tools/zeppelin downloads fetch bw-zeppelin-air-software/zeppelin-air/firmware-2.04.05`).
Analysis takes minutes per image; the module app about 20.
"""

import os
import re
import shutil
import struct
import subprocess
import sys

REPO = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
sys.path.insert(0, os.path.join(REPO, "tools"))
from zeppelin import paths  # noqa: E402
from zeppelin.fw import bcd, ihex  # noqa: E402

SCRIPTS = os.path.join(REPO, "re", "ghidra", "scripts")
GEN = os.path.join(REPO, "re", "gen")
PROJECTS = os.path.join(GEN, "ghidra")
UPDATER = os.path.join(paths.DOWNLOADS, "bw-zeppelin-air-software", "zeppelin-air", "firmware-2.04.05")
MCU_CODE = (0x80008000, 0x80034000)  # the MCU app's code and constants, where OrphanDec looks


def mcu_app():
    """The stock MCU image from 0x80000000 (the hex's jump at 0, the app from 0x80004000) up to its last byte."""
    return bytes(ihex.image(open(paths.MCU_STOCK_HEX).read(), 0x80000000, 0x40000)[1]).rstrip(b"\xff")


def bl2_stages():
    """(stage 1, stage 2) of the module's BL2 partition (docs/module/boot-chain.md, "Boot header")."""
    b = open(paths.MODULE_BL2, "rb").read()
    if b[0x30:0x38] != b"MetaInfo" or b[0x80:0x84] != b"\x55\x55\xaa\xaa":
        sys.exit("%s: no MetaInfo + boot header" % paths.MODULE_BL2)
    (base,) = struct.unpack_from("<I", b, 0x20)  # the partition's NAND address + 0x80
    s1_words, _, _, _, _, s2_nand, s2_len = struct.unpack_from("<7I", b, 0x90)
    s2 = b[s2_nand - base + 0x80 :][:s2_len]
    if len(s2) != s2_len:
        sys.exit("%s: stage 2 runs past the partition" % paths.MODULE_BL2)
    return b[0x98 : 0x98 + 8 * s1_words], s2


def module_app():
    """The stock module app's section (stage 1 at 0, stage 2 at 0x2000), from the .bcd."""
    return bcd.sections(open(paths.MODULE_STOCK_BCD, "rb").read())[0]["data"]


def file(path):
    return lambda: open(path, "rb").read() if os.path.exists(path) else None


# name -> (image bytes (None: missing), processor, base, [pre scripts], [(post script, [args])], [steps after])
#   processor ids as Ghidra names them (AVR32 big-endian, ARMv5T little-endian); None: Ghidra's loader decides.
#   In the args and steps, {out} is re/gen/<image>, {img} its image.bin.
IMAGES = {
    "mcu-app": (
        mcu_app,
        "avr32:BE:32:default",
        0x80000000,
        ["ZdPre.java"],
        [
            ("ZdDump.java", ["{out}"]),
            ("OrphanDec.java", ["{out}/decomp_orphans.c", "%x" % MCU_CODE[0], "%x" % MCU_CODE[1]]),
        ],
        [
            ["re/scripts/resolve_decomp.py", "{img}", "{out}/decomp_all.c", "{out}/decomp_resolved.c"],
            ["re/scripts/resolve_decomp.py", "{img}", "{out}/decomp_orphans.c", "{out}/decomp_orphans.c"],
        ],
    ),
    "mcu-updater": (
        file(os.path.join(UPDATER, "updater.dylib")),
        None,
        None,
        [],
        [("UpdDump.java", ["{out}/decomp_all.c"])],
        [],
    ),
    "mcu-updater-win": (
        file(os.path.join(UPDATER, "DFUflasher.exe")),
        None,
        None,
        [],
        [("UpdDump.java", ["{out}/decomp_all.c"])],
        [],
    ),
    "module-app": (
        lambda: module_app()[0x2000:],
        "ARM:LE:32:v5t",
        0x60008000,
        ["NetPre.java"],
        [("NetDump.java", ["{out}"])],
        [],
    ),
    "module-pre": (
        lambda: module_app()[:0x2000],
        "ARM:LE:32:v5t",
        0x0040DA00,
        ["NetPre.java"],
        [("NetDump.java", ["{out}"])],
        [],
    ),
    "bl2": (
        lambda: bl2_stages()[1],
        "ARM:LE:32:v5t",
        0x60008000,
        ["Bl2Pre.java", "Bl2Thumb.java", "Bl2Recover.java"],
        [("NetDump.java", ["{out}"])],
        [],
    ),
    "bl2-pre": (
        lambda: bl2_stages()[0],
        "ARM:LE:32:v5t",
        0x0040DA00,
        ["Bl2Pre.java"],
        [("NetDump.java", ["{out}"])],
        [],
    ),
}


def analyze_headless():
    home = os.environ.get("GHIDRA_HOME")
    if home:
        return os.path.join(home, "support", "analyzeHeadless")
    found = shutil.which("analyzeHeadless")
    if found:
        return found
    sys.exit("analyzeHeadless not found: set GHIDRA_HOME (nix develop .#re) or put it on PATH")


def strings(data, path, least=4):
    with open(path, "w") as f:
        for m in re.finditer(rb"[\x20-\x7e\t]{%d,}" % least, data):
            f.write("%x %s\n" % (m.start(), m.group().decode()))


def run(name):
    image, proc, base, pre, post, after = IMAGES[name]
    data = image()
    if data is None:
        print("SKIP %s: its input is not in downloads/ (see --help)" % name)
        return
    out = os.path.join(GEN, name)
    os.makedirs(out, exist_ok=True)
    os.makedirs(PROJECTS, exist_ok=True)
    img = os.path.join(out, "image.bin")
    open(img, "wb").write(data)
    strings(data, os.path.join(out, "strings.txt"))
    sub = lambda a: a.format(out=out, img=img)  # noqa: E731
    cmd = [analyze_headless(), PROJECTS, name, "-import", img, "-overwrite", "-scriptPath", SCRIPTS]
    if proc:
        cmd += ["-processor", proc, "-loader", "BinaryLoader", "-loader-baseAddr", hex(base)]
    for s in pre:
        cmd += ["-preScript", s]
    for s, args in post:
        cmd += ["-postScript", s] + [sub(a) for a in args]
    print("== %s -> %s" % (name, os.path.relpath(out, REPO)), flush=True)
    subprocess.run(cmd, check=True, cwd=REPO)
    for script, *args in after:
        subprocess.run([sys.executable, "-I", os.path.join(REPO, script)] + [sub(a) for a in args], check=True)


def main():
    args = sys.argv[1:]
    if args in (["--help"], ["-h"]):
        return print(__doc__)
    if args == ["list"]:
        for n, (_, proc, base, *_rest) in IMAGES.items():
            print("%-16s %-20s %s" % (n, proc or "(Ghidra's loader)", hex(base) if base else ""))
        return
    for name in args or IMAGES:
        if name not in IMAGES:
            sys.exit("unknown image %r (try: list)" % name)
        run(name)


if __name__ == "__main__":
    main()
