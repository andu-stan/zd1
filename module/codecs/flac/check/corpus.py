"""corpus.py DIR FLACCHECK [FLAC]: our FLAC decoder against libFLAC's over a corpus: test signals written as WAV,
encoded by `flac` (libFLAC) with each set of options below, decoded by `flac -d` (raw, little-endian, signed) and by
FLACCHECK (check/flaccheck.c, `dump`); the two must be the same bytes (FLAC is lossless: there is no tolerance).
FLAC: the flac binary (default: from PATH; nix: nixpkgs#flac.bin). Run with python3 -I."""

import math
import os
import random
import struct
import subprocess
import sys
import wave

SECONDS = 4


def signal(name, rate, ch, bits, n):
    """interleaved integer samples"""
    full = (1 << (bits - 1)) - 1
    rnd = random.Random(name)
    out = []
    for i in range(n):
        t = i / rate
        if name == "music":  # chords with an envelope and a little noise
            env = 0.5 + 0.5 * math.sin(2 * math.pi * 0.7 * t)
            v = [
                env
                * (
                    0.3 * math.sin(2 * math.pi * 220 * t)
                    + 0.2 * math.sin(2 * math.pi * 277.2 * t + c)
                    + 0.15 * math.sin(2 * math.pi * 329.6 * t)
                    + 0.02 * rnd.uniform(-1, 1)
                )
                for c in range(ch)
            ]
        elif name == "sweep":  # near full scale, 20 Hz to 20 kHz
            f = 20 * (1000 ** (t / SECONDS))
            v = [0.98 * math.sin(2 * math.pi * f * t * (0.5 if c else 1)) for c in range(ch)]
        elif name == "noise":
            v = [rnd.uniform(-1, 1) for _ in range(ch)]
        elif name == "silence":
            v = [0.0] * ch
        elif name == "quiet":  # -60 dB: few bits used
            v = [0.001 * math.sin(2 * math.pi * 440 * t + c) for c in range(ch)]
        elif name == "square":  # clipped
            v = [max(-1.0, min(1.0, 3 * math.sin(2 * math.pi * 110 * t)))] * ch
        elif name == "uncorrelated":
            v = [math.sin(2 * math.pi * 330 * t), rnd.uniform(-1, 1)][:ch]
        elif name == "same":  # both channels equal: the side channel is all zero
            v = [0.7 * math.sin(2 * math.pi * 550 * t) + 0.1 * rnd.uniform(-1, 1)] * ch
        elif name == "wasted":  # the low 4 bits always zero (wasted bits)
            v = [0.6 * math.sin(2 * math.pi * 200 * t + c) for c in range(ch)]
            out += [int(x * full) & ~15 for x in v]
            continue
        out += [max(-full - 1, min(full, int(round(x * full)))) for x in v]
    return out


def write_wav(path, rate, ch, bits, samples):
    w = wave.open(path, "wb")
    w.setnchannels(ch)
    w.setsampwidth(bits // 8)
    w.setframerate(rate)
    if bits == 8:  # WAV's 8-bit is unsigned
        w.writeframes(bytes((s + 128) & 0xFF for s in samples))
    elif bits == 16:
        w.writeframes(struct.pack("<%dh" % len(samples), *samples))
    else:
        w.writeframes(b"".join(struct.pack("<i", s)[:3] for s in samples))
    w.close()


OPTIONS = [
    ("l0", ["-0"]),
    ("l2", ["-2"]),
    ("l5", ["-5"]),
    ("l8", ["-8"]),
    ("b192", ["-5", "-b", "192"]),
    ("b576", ["-5", "-b", "576"]),
    ("b2048", ["-5", "-b", "2048"]),
    ("b4608", ["-8", "-b", "4608"]),
    ("lpc32", ["--lax", "-l", "32", "-b", "4096"]),
    ("exhaustive", ["-8", "-e", "-p", "-r", "0,8"]),
    ("nofixed", ["-8", "--disable-fixed-subframes"]),
    ("fixedonly", ["-2", "-l", "0"]),
    ("nomidside", ["-5", "--no-mid-side"]),
    ("adaptive", ["-1"]),  # adaptive mid-side
]
# beyond Warp0's limits (2 channels, 4608-sample blocks): for the ARM's build (rust/build.rs's -D), with ARM=1
OPTIONS_ARM = [("b16384", ["--lax", "-5", "-b", "16384"]), ("b65535", ["--lax", "-8", "-b", "65535"])]
SIGNALS = [  # name, rate, channels, bits
    ("music", 44100, 2, 16),
    ("sweep", 44100, 2, 16),
    ("noise", 44100, 2, 16),
    ("silence", 44100, 2, 16),
    ("quiet", 44100, 2, 16),
    ("square", 44100, 2, 16),
    ("uncorrelated", 44100, 2, 16),
    ("same", 44100, 2, 16),
    ("wasted", 44100, 2, 16),
    ("music", 44100, 1, 16),
    ("music", 48000, 2, 16),
    ("music", 44100, 2, 24),
    ("noise", 44100, 2, 24),
    ("sweep", 48000, 2, 24),
    ("music", 44100, 2, 8),
]
SIGNALS_ARM = [("music", 44100, 6, 16), ("noise", 48000, 3, 24), ("music", 44100, 8, 16)]


def main():
    d, check = sys.argv[1], sys.argv[2]
    flac = sys.argv[3] if len(sys.argv) > 3 else "flac"
    os.makedirs(d, exist_ok=True)
    bad = n = 0
    arm = os.environ.get("ARM") == "1"
    for sig, rate, ch, bits in SIGNALS + (SIGNALS_ARM if arm else []):
        wav = os.path.join(d, "%s_%d_%d_%d.wav" % (sig, rate, ch, bits))
        if not os.path.exists(wav):
            write_wav(wav, rate, ch, bits, signal(sig, rate, ch, bits, rate * SECONDS))
        for oname, opts in OPTIONS + (OPTIONS_ARM if arm else []):
            f = wav[:-4] + "." + oname + ".flac"
            ref, ours = f + ".ref.raw", f + ".ours.raw"
            # (more than 2 channels: WAV has no channel mask here, so none for FLAC either)
            subprocess.run(
                [flac, "-s", "-f"] + (["--channel-map=none"] if ch > 2 else []) + opts + ["-o", f, wav],
                check=True,
                stderr=subprocess.DEVNULL,
            )
            subprocess.run(
                [flac, "-s", "-f", "-d", "--force-raw-format", "--endian=little", "--sign=signed", "-o", ref, f],
                check=True,
            )
            r = subprocess.run([check, "dump", f, ours], capture_output=True, text=True)
            same = r.returncode == 0 and open(ref, "rb").read() == open(ours, "rb").read()
            n += 1
            if not same:
                bad += 1
                print("DIFFER %s: %s" % (os.path.basename(f), r.stderr.strip()))
            for p in (ref, ours):
                os.remove(p)
    print("%d streams, %d differ from libFLAC" % (n, bad))
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
