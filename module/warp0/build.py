#!/usr/bin/env python3
"""Build the Warp0 firmware images (README.md; fw/, MIPS I, the flake's LLVM on the host through mips.py), in
emu/img/warp0/ ($W0_OUT in the nix build, `nix build .#warp0-fw`): per image NAME.bin (or NAME_local.bin +
NAME_sdram.bin), NAME.elf, NAME.s, NAME.sym (the code symbols, for hang reports); inval.bin.

  python3 module/warp0/build.py       (in nix develop)
"""

import os
import re
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", ".."))
OUT = os.environ.get("W0_OUT") or os.path.join(REPO, "emu", "img", "warp0")
sys.path.insert(0, HERE)
import mips  # noqa: E402


def abi():
    """w0abi.h's numeric #defines, by name (expressions of earlier ones included)"""
    names = {}
    for line in open(os.path.join(HERE, "w0abi.h")):
        m = re.match(r"#define\s+(\w+)\s+(.+?)\s*(/\*.*)?$", line)
        if not m:
            continue
        expr = re.sub(r"(\b0x[0-9a-fA-F]+|\b\d+)u\b", r"\1", m.group(2))
        try:
            names[m.group(1)] = eval(expr, {}, dict(names))
        except Exception:
            pass  # not a number (a macro continued on the next line, a guard)
    return names


A = abi()
AAC, DSP, VORBIS, FLAC = (os.path.join(REPO, "module", "codecs", m) for m in ("aac", "dsp", "vorbis", "flac"))
LRAM = '__attribute__((section(".rodata.lram")))'
# no delay-slot filling (mips.compile_c says why)
CFLAGS = [
    "--target=mipsel-unknown-linux-gnu",
    "-march=mips1",
    "-mabi=32",
    "-O2",
    "-G0",
    "-ffreestanding",
    "-fno-pic",
    "-mno-abicalls",
    "-nostdlib",
    "-fno-builtin",
    "-fno-stack-protector",
    "-Wall",
    "-Wextra",
    "-Wno-main-return-type",
    "-mllvm",
    "-disable-mips-delay-filler",
]

# name: sources (C, then assembly), linker script, extra flags, split (a local part at 0 and an SDRAM part)
IMAGES = {
    "w0fw": dict(
        c=["fw/rt.c", "fw/main.c", "fw/alac.c"],
        s=["fw/start.S", "fw/poly.S"],
        ld="fw/w0.ld",
        flags=["-I" + DSP],
        split=False,
    ),
    "w0aac": dict(
        c=[
            "fw/rt.c",
            "fw/aac_main.c",
            AAC + "/aacdec.c",
            AAC + "/aac_tables.c",
            DSP + "/imdct.c",
            DSP + "/imdct_tables.c",
        ],
        s=["fw/start.S"],
        ld="fw/w0aac.ld",
        flags=["-DAAC_EXT_BUFFERS", "-DAAC_FAST=" + LRAM, "-DDSP_FAST=" + LRAM, "-I" + AAC, "-I" + DSP],
        split=True,
    ),
    "w0vorbis": dict(
        c=[
            "fw/rt.c",
            "fw/vorbis_main.c",
            VORBIS + "/vorbisdec.c",
            VORBIS + "/vorbis_tables.c",
            DSP + "/imdct.c",
            DSP + "/imdct_tables.c",
        ],
        s=["fw/start.S"],
        ld="fw/w0vorbis.ld",
        flags=[
            "-DVORBIS_EXT_BUFFERS",
            "-DVORBIS_FAST=" + LRAM,
            "-DDSP_FAST=" + LRAM,
            "-DVORBIS_FAST_ARENA=10240",
            "-DVORBIS_STAGE(n)=(*(volatile unsigned *)0x61F1002C = (unsigned)(n))",  # C_STAGE
            "-I" + VORBIS,
            "-I" + DSP,
        ],
        split=True,
    ),
    "w0flac": dict(
        c=["fw/rt.c", "fw/flac_main.c", FLAC + "/flacdec.c"],
        s=["fw/start.S"],
        ld="fw/w0.ld",
        flags=["-I" + FLAC, "-I" + DSP],
        split=False,
    ),
}


def build_image(name, img, tools):
    mc, ld, objcopy, nm = tools
    with tempfile.TemporaryDirectory() as d:
        srcs = [p if os.path.isabs(p) else os.path.join(HERE, p) for p in img["c"]]
        asms = [os.path.join(d, os.path.splitext(os.path.basename(src))[0] + ".s") for src in srcs]
        for src, asm in zip(srcs, asms):
            mips.compile_c(CFLAGS + img["flags"], src, asm)
        objs = []
        for src in asms + [os.path.join(HERE, p) for p in img["s"]]:
            o = os.path.join(d, os.path.basename(src) + ".o")
            subprocess.run([mc, "-triple=mipsel", "-mcpu=mips1", "-filetype=obj", "-o", o, src], check=True)
            objs.append(o)
        elf = os.path.join(OUT, name + ".elf")
        subprocess.run([ld, "-m", "elf32ltsmip", "-T", os.path.join(HERE, img["ld"]), "-o", elf] + objs, check=True)
        open(os.path.join(OUT, name + ".s"), "w").write("".join(open(a).read() for a in asms))
    if img["split"]:
        subprocess.run(
            [objcopy, "-O", "binary", "-j", ".lram", elf, os.path.join(OUT, name + "_local.bin")], check=True
        )
        subprocess.run(
            [
                objcopy,
                "-O",
                "binary",
                "-j",
                ".text",
                "-j",
                ".rodata",
                "-j",
                ".data",
                elf,
                os.path.join(OUT, name + "_sdram.bin"),
            ],
            check=True,
        )
    else:
        subprocess.run([objcopy, "-O", "binary", elf, os.path.join(OUT, name + ".bin")], check=True)
    # the code symbols, address order: the hang report names the function a PC is in
    syms = subprocess.run([nm, "-n", "--defined-only", elf], capture_output=True, text=True, check=True).stdout
    code = [(int(a, 16), n) for a, t, n in (l.split() for l in syms.splitlines() if len(l.split()) == 3) if t in "tT"]
    open(os.path.join(OUT, name + ".sym"), "w").write("".join("%08x %s\n" % c for c in code))
    sym = {n: a for a, t, n in (l.split() for l in syms.splitlines() if len(l.split()) == 3) for a in [int(a, 16)]}
    if img["split"]:
        local = os.path.getsize(os.path.join(OUT, name + "_local.bin"))
        sdram = os.path.getsize(os.path.join(OUT, name + "_sdram.bin"))
        print(
            "warp0 %s: local %d bytes (+ .bss.lram to 0x%x, stack from 0xb000), sdram %d bytes (.bss to 0x%x)%s"
            % (
                name,
                local,
                sym.get("_lbss_end", 0),
                sdram,
                sym.get("_bss_end", 0),
                ", arena 0x%x-0x%x" % (sym["_heap_start"], sym["_heap_end"]) if "_heap_start" in sym else "",
            )
        )
    else:
        size = os.path.getsize(os.path.join(OUT, name + ".bin"))
        if sym.get("_bss_end", 0) > A["W0_LOCAL_MAX"]:
            raise SystemExit("%s: image + .bss run into the stack" % name)
        print("warp0 %s: %d bytes (.bss to 0x%x)" % (name, size, sym.get("_bss_end", 0)))


def build_fw():
    os.makedirs(OUT, exist_ok=True)
    tools = tuple(mips.tool(t) for t in ("llvm-mc", "ld.lld", "llvm-objcopy", "llvm-nm"))
    for name, img in IMAGES.items():
        build_image(name, img, tools)
    inval = mips.assemble(open(os.path.join(HERE, "fw", "inval.S")).read(), A["W0_INVAL"])
    open(os.path.join(OUT, "inval.bin"), "wb").write(inval)


if __name__ == "__main__":
    build_fw()
