#!/usr/bin/env python3
"""Summarise a PCM WAV (16/24/32-bit) from the emulator's audio sinks, stdlib only.

usage: wavstat.py file.wav [--win SECS] [--expect HZ]
Per window: RMS and peak in dBFS per channel, frequency from zero crossings, and dropouts (runs of >= 32 exact-zero
frames inside a non-silent window). Ends with the longest non-silent stretch and its median level/frequency, and
exits 1 if --expect HZ is given and that stretch is not within 2 % of it.
"""

import argparse
import math
import sys
import wave

ap = argparse.ArgumentParser()
ap.add_argument("wav")
ap.add_argument("--win", type=float, default=1.0)
ap.add_argument("--expect", type=float)
a = ap.parse_args()
w = wave.open(a.wav, "rb")
ch, sw, fs, n = w.getnchannels(), w.getsampwidth(), w.getframerate(), w.getnframes()
full = float(1 << (8 * sw - 1))
print(f"{a.wav}: {ch} ch, {8 * sw}-bit, {fs} Hz, {n} frames ({n / fs:.2f} s)")


def frames(k):
    b = w.readframes(k)
    out = []
    for i in range(0, len(b) - sw * ch + 1, sw * ch):
        out.append([int.from_bytes(b[i + c * sw : i + (c + 1) * sw], "little", signed=True) for c in range(ch)])
    return out


def db(x):
    return 20 * math.log10(x / full) if x > 0 else -999.0


win = max(1, int(a.win * fs))
t = 0.0
runs, cur = [], None  # non-silent stretches: [start, end, [rms_db...], [hz...]]
while True:
    f = frames(win)
    if not f:
        break
    cols = list(zip(*f))
    rms = [math.sqrt(sum(x * x for x in c) / len(c)) for c in cols]
    pk = [max(abs(x) for x in c) for c in cols]
    zc = sum(1 for i in range(1, len(cols[0])) if (cols[0][i - 1] < 0) != (cols[0][i] < 0))
    hz = zc / 2 / (len(f) / fs)
    loud = max(rms) > full * 1e-4
    gaps, z = 0, 0
    for fr in f:
        if not any(fr):
            z += 1
        else:
            if z >= 32:
                gaps += 1
            z = 0
    print(
        f"{t:7.2f}s  rms "
        + " ".join(f"{db(r):6.1f}" for r in rms)
        + "  peak "
        + " ".join(f"{db(p):6.1f}" for p in pk)
        + f" dBFS  {hz:7.1f} Hz"
        + (f"  dropouts {gaps}" if loud and gaps else "")
        + ("" if loud else "  (silent)")
    )
    if loud:
        if cur is None:
            cur = [t, t, [], []]
        cur[1] = t + len(f) / fs
        cur[2].append(db(max(rms)))
        cur[3].append(hz)
    elif cur:
        runs.append(cur)
        cur = None
    t += len(f) / fs
if cur:
    runs.append(cur)
if not runs:
    print("no signal")
    sys.exit(1 if a.expect else 0)
r = max(runs, key=lambda r: r[1] - r[0])
med = lambda v: sorted(v)[len(v) // 2]  # median: the first/last windows are only partly filled
mhz = med(r[3])
print(f"signal {r[0]:.2f}-{r[1]:.2f} s ({r[1] - r[0]:.2f} s): median rms {med(r[2]):.1f} dBFS, {mhz:.1f} Hz")
if a.expect and abs(mhz - a.expect) > 0.02 * a.expect:
    print(f"FAIL: expected {a.expect} Hz")
    sys.exit(1)
