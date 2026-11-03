#!/usr/bin/env python3
"""24-bit ALAC test vectors from Apple's own encoder (macOS afconvert), as an Apple TV sends lossless audio
(ALAC/48000/S24, 352-sample frames), in shairplay-rust's golden-vector format (ALACGV01, bit depth 24: the PCM as
interleaved signed 24-bit little-endian). fw/alac_test.c and tools/emu.py alac on the host.

  python3 -I module/warp0/vectors/afalac24.py OUT_DIR [SECS]   -> OUT_DIR/stereo24.alac, OUT_DIR/mono24.alac
                                                                (SECS: that long instead of 0.6 s, for a stream test:
                                                                ap2send HOST FILE --alac48)

The signals use the low byte: Apple's encoder shifts it off a 24-bit sample and stores it uncompressed, which is
what a 16-bit stream never exercises. A noise burst makes escape (verbatim) frames; the length isn't a multiple of
352, so the last frame is a partial one (its own sample count)."""

import math
import os
import struct
import subprocess
import sys
import tempfile
import wave

SR = 48000
N = 352
FULL = (1 << 23) - 1


def clamp24(x):
    return max(-(1 << 23), min(FULL, int(round(x * FULL))))


def synth(channels, total):
    seed = 0x2468ACE1
    out = []
    for i in range(total):
        t = i / SR
        seg = i * 8 // total
        for c in range(channels):
            seed ^= (seed << 13) & 0xFFFFFFFF
            seed ^= seed >> 17
            seed ^= (seed << 5) & 0xFFFFFFFF
            noise = ((seed & 0xFFFFFF) - (1 << 23)) / (1 << 23)
            ph = 0.3 * c
            if seg in (0, 1):  # a chord with 24-bit dither
                v = (
                    0.3 * math.sin(2 * math.pi * 220 * t + ph)
                    + 0.2 * math.sin(2 * math.pi * 277.2 * t)
                    + 0.15 * math.sin(2 * math.pi * 329.6 * t)
                    + 1e-6 * noise
                )
            elif seg == 2:  # white noise: escape frames
                v = 0.8 * noise
            elif seg == 3:  # a -110 dBFS tone: only in the low bits
                v = 3.2e-6 * math.sin(2 * math.pi * 1000 * t + ph)
            elif seg == 4:  # clicks over near-silence
                v = 0.9 if i % 5000 < 3 else 2e-7 * noise
            elif seg == 5:  # a full-scale square
                v = 1.0 if (i // 48) % 2 == 0 else -1.0
            else:  # a sweep, the channels apart
                v = 0.5 * math.sin(2 * math.pi * (100 + 3000 * t) * t + ph) + 0.01 * noise
            out.append(clamp24(v))
    return out


def read_caf_chunks(data):
    assert data[:4] == b"caff", "not a CAF file"
    off, chunks = 8, {}
    while off + 12 <= len(data):
        ctype, size = data[off : off + 4], struct.unpack(">q", data[off + 4 : off + 12])[0]
        off += 12
        if size < 0:
            size = len(data) - off
        chunks[ctype] = data[off : off + size]
        off += size
    return chunks


def packet_sizes(pakt):
    n = struct.unpack(">q", pakt[:8])[0]
    off, sizes = 24, []
    for _ in range(n):
        v = 0
        while True:
            b = pakt[off]
            off += 1
            v = v << 7 | (b & 0x7F)
            if not b & 0x80:
                break
        sizes.append(v)
    return sizes


def cookie48(kuki):
    """The 48-byte block shairplay's set_info() reads: frma atom, alac atom header, ALACSpecificConfig."""
    if kuki[4:8] == b"frma":
        cfg = kuki[24:48]
    elif kuki[4:8] == b"alac":
        cfg = kuki[12:36]
    else:
        cfg = kuki[:24]
    assert len(cfg) == 24
    return struct.pack(">I", 12) + b"frmaalac" + struct.pack(">I", 36) + b"alac" + bytes(4) + cfg


def gen(out_dir, name, channels, total):
    pcm = synth(channels, total)
    raw = b"".join(struct.pack("<i", s)[:3] for s in pcm)
    with tempfile.TemporaryDirectory() as tmp:
        wav, caf = os.path.join(tmp, "in.wav"), os.path.join(tmp, "out.caf")
        with wave.open(wav, "wb") as w:
            w.setnchannels(channels)
            w.setsampwidth(3)
            w.setframerate(SR)
            w.writeframes(raw)
        # format flag 3: 24-bit source data
        subprocess.run(["afconvert", "-d", "alac/3#%d" % N, "-f", "caff", wav, caf], check=True)
        chunks = read_caf_chunks(open(caf, "rb").read())
    cookie = cookie48(chunks[b"kuki"])
    sizes = packet_sizes(chunks[b"pakt"])
    data = chunks[b"data"][4:]
    frames, off = [], 0
    for s in sizes:
        frames.append(data[off : off + s])
        off += s
    cfg = cookie[24:]
    assert struct.unpack(">I", cfg[:4])[0] == N and cfg[5] == 24, "not 352-sample 24-bit ALAC"
    out = bytearray(b"ALACGV01")
    out += struct.pack("<IBBH", SR, channels, 24, 0)
    out += struct.pack("<I", len(cookie)) + cookie
    out += struct.pack("<I", len(frames))
    for f in frames:
        out += struct.pack("<I", len(f)) + f
    out += struct.pack("<I", len(raw)) + raw
    path = os.path.join(out_dir, name + ".alac")
    open(path, "wb").write(out)
    escapes = sum(1 for f in frames if len(f) >= N * channels * 3)
    print(f"{path}: {channels} channels, {len(frames)} frames ({escapes} escape), {len(pcm) // channels} samples")


if __name__ == "__main__":
    os.makedirs(sys.argv[1], exist_ok=True)
    total = int(float(sys.argv[2]) * SR) if len(sys.argv) > 2 else N * 80
    gen(sys.argv[1], "stereo24", 2, total + 123)
    gen(sys.argv[1], "mono24", 1, total + 123)
