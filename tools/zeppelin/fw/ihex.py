"""Intel HEX, the MCU images' format (B&W's stock file, avr32-objcopy's output): read it into memory, write it back.

records(text)               every record: (line index, type, absolute address, data), checksums checked
image(text, base, size)     the memory the data records describe, 0xFF where they have none (what DFU leaves in
                            flash): (base, bytearray); base and size default to the records' span
line(addr, type, data)      one record as a line
dump(mem, base)             mem at base as HEX text, the way the stock file is written
"""

DATA, EOF, ESA, ELA = 0, 1, 2, 4  # record types: data, end of file, extended segment / linear address


def records(text):
    base = 0
    for n, s in enumerate(text.splitlines()):
        s = s.strip()
        if not s.startswith(":"):
            continue
        b = bytes.fromhex(s[1:])
        if len(b) < 5 or len(b) != b[0] + 5:
            raise ValueError("hex line %d: the length doesn't match the record" % (n + 1))
        if sum(b) & 0xFF:
            raise ValueError("hex line %d: bad checksum" % (n + 1))
        typ, data = b[3], b[4:-1]
        yield n, typ, base + (b[1] << 8 | b[2]), data
        if typ == ESA:
            base = int.from_bytes(data, "big") << 4
        elif typ == ELA:
            base = int.from_bytes(data, "big") << 16


def image(text, base=None, size=None):
    recs = [(a, d) for _, t, a, d in records(text) if t == DATA]
    if not recs:
        raise ValueError("no data records")
    if base is None:
        base = min(a for a, _ in recs)
    if size is None:
        size = max(a + len(d) for a, d in recs) - base
    mem = bytearray(b"\xff" * size)
    for a, d in recs:  # data outside base..base+size is left out
        lo, hi = max(a, base), min(a + len(d), base + size)
        if lo < hi:
            mem[lo - base : hi - base] = d[lo - a : hi - a]
    return base, mem


def line(addr, typ, data):
    b = bytes([len(data), addr >> 8 & 0xFF, addr & 0xFF, typ]) + bytes(data)
    return ":" + (b + bytes([-sum(b) & 0xFF])).hex().upper()


def dump(mem, base):
    """16 bytes a record, an extended linear address record where the upper half changes; lines that are all 0xFF
    (erased flash) are left out."""
    out, ela = [], None
    for off in range(0, len(mem), 16):
        chunk = bytes(mem[off : off + 16])
        if chunk == b"\xff" * len(chunk):
            continue
        a = base + off
        if a >> 16 != ela:
            ela = a >> 16
            out.append(line(0, ELA, ela.to_bytes(2, "big")))
        out.append(line(a & 0xFFFF, DATA, chunk))
    out.append(line(0, EOF, b""))
    return "\n".join(out) + "\n"
