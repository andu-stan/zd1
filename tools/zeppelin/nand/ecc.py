"""The DM870 NAND ECC (BL1, BL2 and the stock app; fitted against the unit's OOB, 2026-10-23).

One code per 512-byte sector, 3 bytes, a plain Hamming code, not inverted (a blank sector's code is 00 00 00).
Each byte holds four (P, P') pairs, P in the odd bit: P_k = parity of the bits whose address has bit k set, P'_k
the rest. Byte addresses are bits 0-8, the bit inside the byte is c0-c2:
    byte 0: a4 a5 a6 a7   (bits 1:0 = a4, 3:2 = a5, ...)
    byte 1: a0 a1 a2 a3
    byte 2: a8 c0 c1 c2
Spare area: byte 0x0F (then 0x0E) counts in-place writes of the page, one bit pair cleared per write; write n keeps
its four codes (sector 0-3) at 0x10 + 12*(n-1) + 3*sector. Past four writes there is no slot and no valid code (the
PersParam page). docs/module/boot-chain.md has the evidence.

    python3 -I tools/zeppelin/nand/ecc.py DUMP [PAGE...]   DUMP = pages of 2048 + 64 (zd1-persparam raw); per-page status
"""

import argparse

ORDER = (4, 5, 6, 7, 0, 1, 2, 3, 8, "c0", "c1", "c2")


def parities(sector: bytes) -> dict:
    """(P, P') for every address bit of a 512-byte sector."""
    p = {k: [0, 0] for k in ORDER}
    for a, x in enumerate(sector):
        if not x:
            continue
        odd = bin(x).count("1") & 1
        for k in range(9):
            p[k][0 if a >> k & 1 else 1] ^= odd
        for j in range(3):
            hi = bin(x & sum(1 << b for b in range(8) if b >> j & 1)).count("1") & 1
            p[f"c{j}"][0] ^= hi
            p[f"c{j}"][1] ^= hi ^ odd
    return p


def calc(sector: bytes) -> bytes:
    assert len(sector) == 512
    p = parities(sector)
    v = 0
    for i, k in enumerate(ORDER):
        v |= (p[k][0] << 1 | p[k][1]) << (2 * (i % 4) + 8 * (i // 4))
    return bytes([v & 0xFF, v >> 8 & 0xFF, v >> 16])


def correct(sector: bytes, stored: bytes):
    """-> (data, status): 'ok', 'fixed bit A.B', 'code bit' (the error is in the code), 'uncorrectable'."""
    s = int.from_bytes(calc(sector), "little") ^ int.from_bytes(stored, "little")
    if not s:
        return sector, "ok"
    if bin(s).count("1") == 1:
        return sector, "code bit"
    pairs = [(s >> (2 * i)) & 3 for i in range(12)]
    if any(x in (0, 3) for x in pairs):
        return sector, "uncorrectable"
    bits = {k: pairs[i] >> 1 for i, k in enumerate(ORDER)}
    a = sum(bits[k] << k for k in range(9))
    b = sum(bits[f"c{j}"] << j for j in range(3))
    fixed = bytearray(sector)
    fixed[a] ^= 1 << b
    return bytes(fixed), f"fixed bit {a:#x}.{b}"


def generation(oob: bytes) -> int:
    """How many times the page was programmed (cleared bit pairs of spare 0x0F, then 0x0E)."""
    return sum(1 for b in (oob[0x0F], oob[0x0E]) for k in (6, 4, 2, 0) if (b >> k) & 3 == 0)


def page(data: bytes, oob: bytes):
    """Correct a 2048-byte page with its 64-byte spare area; -> (data, [status per sector]) or (data, None) when the
    page carries no valid code (never programmed, or past four writes)."""
    g = generation(oob)
    if not 1 <= g <= 4:
        return data, None
    out, st = b"", []
    for n in range(4):
        o = 0x10 + 12 * (g - 1) + 3 * n
        d, s = correct(data[n * 512 : (n + 1) * 512], oob[o : o + 3])
        out += d
        st.append(s)
    return out, st


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("dump", metavar="DUMP")
    ap.add_argument("pages", nargs="*", type=lambda v: int(v, 0), metavar="PAGE")
    a = ap.parse_args()
    dump = open(a.dump, "rb").read()
    pages = a.pages or range(len(dump) // 2112)
    for i in pages:
        p = dump[i * 2112 : (i + 1) * 2112]
        if p[:2048] != b"\xff" * 2048:
            print(f"page {i} ({i * 2048:#x}): gen {generation(p[2048:])}", page(p[:2048], p[2048:])[1])


if __name__ == "__main__":
    main()
