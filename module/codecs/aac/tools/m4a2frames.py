#!/usr/bin/env python3
"""Raw codec frames out of an .m4a (one audio track), the way an AirPlay sender puts them in RTP packets:
writes OUT as [u32 LE length][frame] records. For ALAC, the first record is the 24-byte ALACSpecificConfig.
usage: python3 -I m4a2frames.py IN.m4a OUT.frames"""

import struct
import sys


def boxes(data, start, end):
    i = start
    while i + 8 <= end:
        size, kind = struct.unpack_from(">I4s", data, i)
        hdr = 8
        if size == 1:
            size, hdr = struct.unpack_from(">Q", data, i + 8)[0], 16
        elif size == 0:
            size = end - i
        yield kind.decode("latin1"), i + hdr, i + size
        i += size


def find(data, start, end, path):
    for kind, s, e in boxes(data, start, end):
        if kind == path[0]:
            return (s, e) if len(path) == 1 else find(data, s, e, path[1:])
    return None


def main(src, dst):
    d = open(src, "rb").read()
    stbl = find(d, 0, len(d), ["moov", "trak", "mdia", "minf", "stbl"])
    s, e = stbl
    tab = {k: (bs, be) for k, bs, be in boxes(d, s, e)}
    bs, _ = tab["stsz"]
    fixed, n = struct.unpack_from(">II", d, bs + 4)
    sizes = [fixed] * n if fixed else list(struct.unpack_from(">%dI" % n, d, bs + 12))
    if "stco" in tab:
        bs, _ = tab["stco"]
        cnt = struct.unpack_from(">I", d, bs + 4)[0]
        offs = list(struct.unpack_from(">%dI" % cnt, d, bs + 8))
    else:
        bs, _ = tab["co64"]
        cnt = struct.unpack_from(">I", d, bs + 4)[0]
        offs = list(struct.unpack_from(">%dQ" % cnt, d, bs + 8))
    bs, _ = tab["stsc"]
    cnt = struct.unpack_from(">I", d, bs + 4)[0]
    runs = [struct.unpack_from(">III", d, bs + 8 + 12 * k) for k in range(cnt)]
    per_chunk = []
    for ci in range(len(offs)):
        spc = [r[1] for r in runs if r[0] - 1 <= ci][-1]
        per_chunk.append(spc)
    frames, si = [], 0
    for ci, off in enumerate(offs):
        for _ in range(per_chunk[ci]):
            frames.append(d[off : off + sizes[si]])
            off += sizes[si]
            si += 1
    out = []
    bs, be = tab["stsd"]
    entry = d[bs + 8 : be]
    codec = entry[4:8].decode("latin1")
    if codec == "alac":
        i = entry.find(b"alac", 8)  # the inner 'alac' box: version/flags, then the 24-byte config
        out.append(entry[i + 8 : i + 8 + 24])
    out += frames
    with open(dst, "wb") as f:
        for fr in out:
            f.write(struct.pack("<I", len(fr)) + fr)
    print("%s: %s, %d frames, %d bytes" % (dst, codec, len(frames), sum(map(len, frames))))


if __name__ == "__main__":
    main(*sys.argv[1:3])
