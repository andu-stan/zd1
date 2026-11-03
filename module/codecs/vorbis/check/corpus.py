#!/usr/bin/env python3
"""The Vorbis decoder against libvorbis over a test corpus: synthetic signals (and speech from macOS `say`, if
there) encoded by libvorbis's `oggenc` at qualities -1..10 and managed bitrates 96/160/320 kb/s (Spotify's), mono
and stereo, 44.1 and 48 kHz; each file decoded by `oggdec` (libvorbis, float, rounded to 16 bits) and by ours
(vorbischeck), sample by sample. Tools from nixpkgs (vorbis-tools).

  python3 -I module/codecs/vorbis/check/corpus.py WORKDIR [VORBISCHECK]
"""

import math
import os
import random
import shutil
import struct
import subprocess
import sys
import wave

WORK = sys.argv[1]
CHECK = sys.argv[2] if len(sys.argv) > 2 else os.path.join(WORK, "vorbischeck")
HERE = os.path.dirname(os.path.abspath(__file__))


def need(name):
    return shutil.which(name) or sys.exit("%s not found: run inside `nix develop` (vorbis-tools)" % name)


OGGENC, OGGDEC = need("oggenc"), need("oggdec")


def write_wav(path, rate, ch, frames):
    with wave.open(path, "wb") as w:
        w.setnchannels(ch)
        w.setsampwidth(2)
        w.setframerate(rate)
        w.writeframes(
            b"".join(struct.pack("<%dh" % ch, *(max(-32768, min(32767, int(round(v)))) for v in f)) for f in frames)
        )


def signal(kind, rate, secs, ch):
    rnd = random.Random(kind)
    n = int(rate * secs)
    out = []
    for i in range(n):
        t = i / rate
        if kind == "music":  # chord + melody + noise bursts + kick/hat transients
            v = sum(0.12 * math.sin(2 * math.pi * f * t) for f in (220, 277.2, 329.6))
            v += 0.15 * math.sin(2 * math.pi * (440 * 2 ** (int(t * 4) % 8 / 12)) * t)
            if (i % (rate // 2)) < rate // 40:
                v += 0.5 * math.exp(-(i % (rate // 2)) / (rate / 200)) * math.sin(2 * math.pi * 60 * t)
            if (i % (rate // 4)) < rate // 200:
                v += 0.3 * (rnd.random() * 2 - 1)
            l, r = v + 0.02 * (rnd.random() - 0.5), 0.8 * v + 0.1 * math.sin(2 * math.pi * 660 * t)
        elif kind == "sweep":  # log sweep 20 Hz - 20 kHz, near full scale
            f = 20 * (1000 ** (t / secs))
            l = r = 0.9 * math.sin(2 * math.pi * 20 * secs / math.log(1000) * (1000 ** (t / secs)))
            del f
        elif kind == "clipped":  # a square wave at full scale, then clipping overdrive
            l = 1.0 if (i // 50) % 2 else -1.0
            r = max(-1, min(1, 3 * math.sin(2 * math.pi * 100 * t)))
        elif kind == "noise":  # white noise, different per channel
            l, r = 0.5 * (rnd.random() * 2 - 1), 0.5 * (rnd.random() * 2 - 1)
        elif kind == "quiet":  # -60 dB tones and silence gaps (floors unused)
            g = 0 if int(t * 2) % 2 else 0.001
            l, r = g * math.sin(2 * math.pi * 1000 * t), g * math.sin(2 * math.pi * 1500 * t)
        elif kind == "wide":  # uncorrelated channels (coupling's worst case)
            l, r = 0.4 * math.sin(2 * math.pi * 500 * t), 0.4 * math.sin(2 * math.pi * 733 * t + 1)
        out.append((l * 32767, r * 32767)[:ch] if ch == 2 else ((l + r) / 2 * 32767,))
    return out


def main():
    os.makedirs(WORK, exist_ok=True)
    if not os.path.exists(CHECK):
        subprocess.run(
            [
                "cc",
                "-O2",
                "-DVORBIS_STATS",
                "-I" + os.path.join(HERE, ".."),
                "-I" + os.path.join(HERE, "..", "..", "dsp"),
                "-o",
                CHECK,
                os.path.join(HERE, "vorbischeck.c"),
                os.path.join(HERE, "..", "vorbisdec.c"),
                os.path.join(HERE, "..", "vorbis_tables.c"),
                os.path.join(HERE, "..", "..", "dsp", "imdct.c"),
                os.path.join(HERE, "..", "..", "dsp", "imdct_tables.c"),
                "-lm",
            ],
            check=True,
        )
    sources = []
    for kind in ("music", "sweep", "clipped", "noise", "quiet", "wide"):
        for rate, ch in ((44100, 2), (44100, 1), (48000, 2)):
            if kind in ("noise", "quiet", "wide") and (rate, ch) != (44100, 2):
                continue
            p = os.path.join(WORK, "%s-%d-%d.wav" % (kind, rate, ch))
            if not os.path.exists(p):
                write_wav(p, rate, ch, signal(kind, rate, 8, ch))
            sources.append(p)
    speech = os.path.join(WORK, "speech.aiff")
    if not os.path.exists(speech) and subprocess.run(["which", "say"], capture_output=True).returncode == 0:
        subprocess.run(["say", "-o", speech, "The quick brown fox jumps over the lazy dog. Vorbis on Warp zero."])
    if os.path.exists(speech):
        sp = os.path.join(WORK, "speech-44100-2.wav")
        if not os.path.exists(sp):
            subprocess.run(["afconvert", "-f", "WAVE", "-d", "LEI16@44100", "-c", "2", speech, sp], check=True)
        sources.append(sp)
    modes = [("q%d" % q, ["-q", str(q)]) for q in (-1, 0, 2, 4, 6, 8, 10)]
    modes += [("%dk" % b, ["-b", str(b), "--managed"]) for b in (96, 160, 320)]
    bad = 0
    for src in sources:
        for name, args in modes:
            if "1.wav" in src and name in ("q8", "q10", "320k"):
                continue
            ogg = src[:-4] + "-" + name + ".ogg"
            ref = ogg[:-4] + ".ref"
            if not os.path.exists(ogg):
                subprocess.run([OGGENC, "-Q"] + args + ["-o", ogg, src], check=True)
            if not os.path.exists(ref):
                subprocess.run([OGGDEC, "-Q", "-R", "-b", "16", "-e", "0", "-s", "1", "-o", ref, ogg], check=True)
            r = subprocess.run([CHECK, "cmp", ogg, ref], capture_output=True, text=True)
            if name == "q-1" and "vorbis_init -2" in r.stderr:  # 512/4096-sample blocks: not ours (Spotify's
                print("skip " + os.path.basename(ogg) + ": 512/4096 blocks, refused as expected")  # lowest is 96k)
                continue
            line = r.stdout.strip() or r.stderr.strip().splitlines()[-1]
            info = [l for l in r.stderr.splitlines() if "Hz" in l]
            print(
                ("FAIL " if r.returncode else "ok   ")
                + os.path.basename(ogg)
                + ": "
                + line.split(": ", 1)[-1]
                + ("  [" + info[0].split(": ", 1)[-1] + "]" if info else "")
            )
            bad += r.returncode != 0
    print("%d failed" % bad)
    return bad != 0


if __name__ == "__main__":
    sys.exit(main())
