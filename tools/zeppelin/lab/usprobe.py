#!/usr/bin/env python3
"""Inaudible "does the speaker play?" probe, for checks without anyone listening: a 19 kHz tone keyed by a 31-chip
m-sequence (25 ms chips), the microphone's 19 kHz band correlated with the code. Detected down to about -60 dBFS
with the stock firmware (score 13.5; silence scores about 3), the Mac and the speaker side by side. Music playing
masks it (a track dropped a -40 dBFS probe to score 2).

  usprobe.py [play] [--dbfs 40] [--hz 19000]   play it 3 times to the speaker's USB audio input ($OUT), after 2.5 s
                                               of silence (an amp asleep wakes and soft-starts), and listen: the
                                               score (DETECTED over 6) and the carrier's level on and off
  usprobe.py gen OUT.wav [--dbfs 20]           the probe as a 44.1 kHz WAV for a stream (AirPlay: raop_send.py):
                                               2 s of silence, 6 repetitions, 1 s of silence
  usprobe.py listen SECS                       record SECS and look for the code (while the WAV plays)

The microphone is $MIC, else the default input (lab/audio.py). Run in `nix develop` (numpy, sounddevice).
"""

import argparse
import os
import sys
import threading

import numpy as np

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
from zeppelin.lab import audio  # noqa: E402

FS = 48000


def verdict(score):
    return "DETECTED" if score > 6 else "not detected"


def play(dbfs, hz):
    code = np.tile(audio.mseq31(), 3)
    # 2.5 s of silence first: an amp asleep wakes when the stream opens and unmutes after its 2 s soft start
    x = audio.keyed_tone(code, hz, FS, dbfs, before_s=2.5, after_s=0.3).astype(np.float32)
    y = np.stack([x, x], 1)
    sd, out, mic = audio._sd(), audio.out(), audio.mic()

    def player():
        with sd.OutputStream(samplerate=FS, channels=2, device=out, dtype="float32") as o:
            o.write(y)

    th = threading.Thread(target=player)
    th.start()
    with sd.InputStream(samplerate=FS, channels=1, device=mic, dtype="float32") as i:
        rec, _ = i.read(len(y) + int(0.5 * FS))
    th.join()
    score, _, on, off = audio.code_score(rec[:, 0].astype(np.float64), FS, hz, code)
    print(
        "%s -> %s: %.0f Hz at -%.0f dBFS | score %.1f | carrier on %.1f dB, off %.1f dB -> %s"
        % (audio.name(out), audio.name(mic), hz, abs(dbfs), score, on, off, verdict(score))
    )


def gen(path, dbfs, fs=44100):
    x = audio.keyed_tone(np.tile(audio.mseq31(), 6), 19000, fs, dbfs, before_s=2, after_s=1)
    audio.write_wav(path, x, fs)
    print("wrote %s, %.1f s" % (path, len(x) / fs))


def listen(secs):
    sd = audio._sd()
    r = sd.rec(int(secs * FS), samplerate=FS, channels=1, device=audio.mic(), blocking=True)[:, 0].astype(np.float64)
    score, at, _, _ = audio.code_score(r, FS, 19000, np.tile(audio.mseq31(), 3))
    print("19 kHz code correlation score %.1f (peak at %.1f s) -> %s" % (score, at, verdict(score)))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd")
    p = sub.add_parser("play")
    p.add_argument("--dbfs", type=float, default=40)
    p.add_argument("--hz", type=float, default=19000)
    p = sub.add_parser("gen")
    p.add_argument("out")
    p.add_argument("--dbfs", type=float, default=20)
    p = sub.add_parser("listen")
    p.add_argument("secs", type=float)
    argv = sys.argv[1:]
    if not argv or argv[0].startswith("--") and argv[0] != "--help":
        argv = ["play"] + argv
    a = ap.parse_args(argv)
    if a.cmd == "play":
        play(a.dbfs, a.hz)
    elif a.cmd == "gen":
        gen(a.out, a.dbfs)
    else:
        listen(a.secs)


if __name__ == "__main__":
    main()
