"""MIPS I code for the DM870's Warp cores (docs/module/warp.md sections 3 and 6): the flake's LLVM, and the passes
that make clang's output safe on a core without a load interlock or a divider. module/warp0/build.py builds the
Warp0 images with it; tools/zeppelin/warp/warprun.py assembles its runner and the probes' snippets.

  tool(name)                  clang, llvm-mc, ld.lld, llvm-objcopy, llvm-nm from $ZD1_LLVM
  assemble(src, base)         MIPS I assembly linked at base -> raw bytes
  compile_c(cflags, src, out) C -> MIPS I assembly at out, through fill_load_delays and hilo_hazards, refused if it
                              uses what the Warp can't run (BANNED) or has a load in a branch delay slot
"""

import os
import re
import subprocess
import sys
import tempfile

_tools = {}
_BINARIES = {"clang": "clang-21", "llvm-mc": "llvm-mc", "ld.lld": "ld.lld", "llvm-objcopy": "llvm-objcopy"}


def tool(name):
    """A tool of $ZD1_LLVM: the flake's LLVM (21), set by `nix develop` and the nix builds. clang is the *unwrapped*
    one (binary `clang-21`): the Darwin cc-wrapper injects host sysroot and `-mmacos-version-min` flags and warns on a
    cross target."""
    if name not in _tools:
        d = os.environ.get("ZD1_LLVM")
        if not d:
            sys.exit("no $ZD1_LLVM: run inside `nix develop` (the flake's LLVM for the Warp cores)")
        _tools[name] = os.path.join(d, "bin", _BINARIES.get(name, name))
    return _tools[name]


def assemble(src, base, entry="_start"):
    """MIPS I assembly (text) linked at base -> raw bytes. Write `.set noreorder` code: delay slots by hand."""
    with tempfile.TemporaryDirectory() as d:
        s, o, elf, b = (os.path.join(d, n) for n in ("a.S", "a.o", "a.elf", "a.bin"))
        open(s, "w").write(src)
        subprocess.run([tool("llvm-mc"), "-triple=mipsel", "-mcpu=mips1", "-filetype=obj", "-o", o, s], check=True)
        subprocess.run(
            [tool("ld.lld"), "-m", "elf32ltsmip", "-Ttext=0x%x" % base, "-e", entry, "-o", elf, o], check=True
        )
        subprocess.run([tool("llvm-objcopy"), "-O", "binary", "-j", ".text", elf, b], check=True)
        return open(b, "rb").read()


# MIPS I GPR loads from memory: the instruction right after one must not read the loaded register (true hazard, no
# interlock on an R3000). lui is immediate and li/la are pseudos, so they are not here.
LOADS = frozenset(("lb", "lbu", "lh", "lhu", "lw", "lwl", "lwr", "ll"))
_LABEL = re.compile(r"[\w.$]+:\s*")  # a label prefix, incl. GAS numeric ("1:") and local (".L0:")
_HILO_WRITERS = frozenset(("mult", "multu", "div", "divu", "mthi", "mtlo"))
_BRANCHES = re.compile(r"^(b\w*|j|jal|jr|jalr)$")
# what the Warp lacks (no divider, no MIPS II/32, lwl/lwr leave the target unchanged) and what isn't there (libc)
BANNED = (
    "div",
    "divu",
    "movz",
    "movn",
    "mul",
    "lwl",
    "lwr",
    "swl",
    "swr",
    "memcpy",
    "memset",
    "memmove",
    "__udivdi3",
    "__umoddi3",
    "__divdi3",
    "__moddi3",
    "__udivsi3",
    "__divsi3",
    "__ashldi3",
    "__lshrdi3",
    "__ashrdi3",
    "__muldi3",
    "__clzsi2",
)


def mnemonic(line):
    """The instruction mnemonic on an asm line, or None for a blank, comment, label-only or directive line."""
    s = line.split("#", 1)[0].strip()
    while True:  # strip any leading labels ("foo:" / "$func_end0:")
        m = _LABEL.match(s)
        if not m:
            break
        s = s[m.end() :]
    if not s or s.startswith("."):
        return None
    return s.split(None, 1)[0].lower()


def fill_load_delays(asm):
    """Put a nop after every memory load. clang marks mips1 output `.set noreorder` and leaves the load-delay slots
    to its own filler, which it calls experimental; this is the belt to that suspenders, and it also covers inline
    asm (which the compiler's filler does not touch). A load already followed by a nop is left alone."""
    lines = asm.splitlines()
    out = []
    for i, line in enumerate(lines):
        out.append(line)
        if mnemonic(line) not in LOADS:
            continue
        j = i + 1  # the next non-blank line (a label/directive counts as "not a nop")
        while j < len(lines) and not lines[j].split("#", 1)[0].strip():
            j += 1
        if j >= len(lines) or mnemonic(lines[j]) != "nop":
            out.append(line[: len(line) - len(line.lstrip())] + "nop")
    return "\n".join(out) + "\n"


def hilo_hazards(asm):
    """MIPS I: a mult/div/mthi/mtlo within two instructions after an mfhi/mflo corrupts what the move reads on an
    R3000; and an mfhi/mflo right after an mthi/mtlo (clang reloads spilled HI/LO that way) is not specified for
    this core. Not measured on the Warp; two nops in either case cost little."""
    lines, out = asm.splitlines(), []
    for i, line in enumerate(lines):
        out.append(line)
        m = mnemonic(line)
        if m not in ("mfhi", "mflo", "mthi", "mtlo"):
            continue
        nxt = [mnemonic(l) for l in lines[i + 1 : i + 8] if mnemonic(l)][:2]
        risky = _HILO_WRITERS if m in ("mfhi", "mflo") else ("mfhi", "mflo")
        if any(n in risky for n in nxt):
            ind = line[: len(line) - len(line.lstrip())]
            out += [ind + "nop", ind + "nop"]
    return "\n".join(out) + "\n"


def check_delay_slots(asm, src):
    """No load in a branch delay slot: the load-delay pass can't protect the branch target's first instruction."""
    insns = [(n, mnemonic(l)) for n, l in enumerate(asm.splitlines(), 1) if mnemonic(l)]
    for (_n, m), (n2, m2) in zip(insns, insns[1:]):
        if _BRANCHES.match(m) and m2 in LOADS:
            raise SystemExit("%s: a load in a branch delay slot (asm line %d: %s; %s)" % (src, n2, m, m2))


def compile_c(cflags, src, out):
    """C -> MIPS I assembly at `out` with the load-delay and HI/LO passes, checked for what the Warp can't run.
    `cflags` are clang's (the target among them); compile with `-mllvm -disable-mips-delay-filler`: a load clang
    moves into a branch's delay slot is unprotected at the branch target (fill_load_delays only sees the textual
    next instruction), and check_delay_slots refuses it."""
    subprocess.run([tool("clang")] + list(cflags) + ["-S", "-o", out, src], check=True)
    asm = hilo_hazards(fill_load_delays(open(out).read()))
    bad = sorted({m for m in BANNED if re.search(r"\b%s\b" % re.escape(m), asm)})
    if bad:
        raise SystemExit(
            "%s: the compiler emitted %s (no divider, no MIPS II/32 instructions, no libc on the Warp): "
            "rewrite the C" % (src, bad)
        )
    check_delay_slots(asm, src)
    open(out, "w").write(asm)
