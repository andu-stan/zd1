# usage: resolve_decomp.py IMAGE IN.c OUT.c  -> a Ghidra AVR32 decompile with the literal-pool loads (PTR_*/DAT_*/xRAM*
# at flash addresses) resolved: call targets, strings, RAM addresses, peripheral registers
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from avr32img import Image, in_data, in_flash  # noqa: E402

img = Image(sys.argv[1])
src = open(sys.argv[2]).read()
PER = {
    0xFFFE0000: "USBB",
    0xFFFE1000: "HMATRIX",
    0xFFFE1400: "FLASHC",
    0xFFFE1800: "MACB",
    0xFFFE1C00: "SMC",
    0xFFFF0000: "PDCA",
    0xFFFF0800: "INTC",
    0xFFFF0C00: "PM",
    0xFFFF0D00: "RTC",
    0xFFFF0D30: "WDT",
    0xFFFF0D80: "EIC",
    0xFFFF1000: "GPIO",
    0xFFFF1100: "GPIO1",
    0xFFFF1200: "GPIO2",
    0xFFFF1300: "GPIO3",
    0xFFFF1400: "USART0",
    0xFFFF1800: "USART1",
    0xFFFF1C00: "USART2",
    0xFFFF2000: "USART3",
    0xFFFF2400: "SPI0",
    0xFFFF2800: "SPI1",
    0xFFFF2C00: "TWI",
    0xFFFF3000: "PWM",
    0xFFFF3400: "SSC",
    0xFFFF3800: "TC",
    0xFFFF3C00: "ADC",
}


def per(v):
    best = None
    for b, n in PER.items():
        if b <= v < b + 0x400 and (best is None or b > best[0]):
            best = (b, n)
    if best:
        off = v - best[0]
        return f"{best[1]}+0x{off:x}" if off else best[1]
    return None


def word(a):
    return img.word(a) if in_flash(a) else None


def cstr(a):
    return img.cstr(a, 3, 200, (9, 10, 13)) if in_flash(a) else None


def fmt(v):
    p = per(v)
    if p:
        return f"/*{p}*/0x{v:08x}"
    s = cstr(v)
    if s is not None:
        return (
            '"'
            + s.replace("\\", "\\\\").replace("\n", "\\n").replace("\r", "\\r").replace("\t", "\\t").replace('"', '\\"')
            + '"'
        )
    if in_flash(v):
        return f"FUN_{v:08x}"
    if in_data(v):
        s2 = img.cstr(v, 3, 200, (9, 10, 13))
        if s2 is not None and len(s2) >= 4:
            return f'RAM_{v:04x}/*"{s2[:40]}"*/'
    if v < 0x10000:
        return f"RAM_{v:04x}"
    return f"0x{v:x}"


def rep_call(m):
    a = int(m.group(1), 16)
    v = word(a)
    return f"FUN_{v:08x}" if v is not None else m.group(0)


src = re.sub(r"\(\*\(code \*\)(?:PTR_)?[A-Za-z_]*?_?(?:FUN|LAB|DAT|PTR)?_?([0-9a-f]{8})\)", rep_call, src)


def rep_tok(m):
    tok = m.group(0)
    a = int(m.group(2), 16)
    if not in_flash(a):
        return tok
    v = word(a)
    if v is None:
        return tok
    return fmt(v)


src = re.sub(r"\b(PTR_[A-Za-z0-9_]*?|DAT|[a-z]{1,3}RAM|PTR_DAT|PTR_FUN|PTR_LAB)_?([0-9a-f]{8})\b", rep_tok, src)
src = re.sub(
    r"\b(?:pu|pc|i|u|c|pi|ppu|p)RAM([0-9a-f]{8})\b",
    lambda m: fmt(word(int(m.group(1), 16))) if word(int(m.group(1), 16)) is not None else m.group(0),
    src,
)
src = re.sub(
    r"\bDAT_(ff[0-9a-f]{6})\b",
    lambda m: (lambda p: f"{p.replace('+', '_')}" if p else m.group(0))(per(int(m.group(1), 16))),
    src,
)
open(sys.argv[3], "w").write(src)
