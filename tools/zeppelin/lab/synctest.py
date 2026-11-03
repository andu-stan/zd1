#!/usr/bin/env python3
"""Measures when the speaker plays a timed AirPlay 2 stream, against the Mac's clock: the multi-room sync error.

ap2send (tools/zeppelin/lab/ap2send) is the sender and the PTP master (the Mac's mach_absolute_time, CLOCK_UPTIME_RAW,
which is also CoreAudio's host time); it streams chirps (18-20 kHz, 40 ms: inaudible to most adults, sharp to
locate) every 2 s and anchors playback at a known master time. The Mac's microphone records with CoreAudio host
timestamps; each chirp's arrival minus the time it was due is the error (plus the microphone path's own delay, which
`loop` measures through the Mac's speaker).

  synctest.py gen DIR [SECS] [RATE]  test signal (RATE 44100, or 48000: AAC-LC/48000): DIR/chirps.wav -> .m4a (afconvert, AAC 256k) -> .frames, and the chirps'
                                 positions in our decoder's output (module/codecs/aac/check/aaccheck.c dump) -> DIR/onsets.json
  synctest.py loop DIR           the Mac speaker -> microphone delay (chirps through the built-in output)
  synctest.py run DIR HOST [SECS]  stream DIR/chirps.frames with ap2send --ptp, record, print the error per chirp
Grouped with the Mac's own speakers through the Music app: musictest.py. The microphone is $MIC, else the default
input; `loop` plays through $OUT, else the default output (lab/audio.py). Run in `nix develop` (numpy, sounddevice).
"""

import argparse
import json
import os
import subprocess
import sys
import time

import numpy as np

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
from zeppelin.lab import audio  # noqa: E402

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))))
AP2SEND = os.path.join(REPO, "target/release/ap2send")  # cargo build --release -p ap2send
AAC = os.path.join(REPO, "module/codecs/aac")
DSP = os.path.join(REPO, "module/codecs/dsp")
M4A2FRAMES = os.path.join(REPO, "module/codecs/aac/tools/m4a2frames.py")
FS = 44100
PERIOD_S, FIRST_S, CHIRP_S = 2.0, 2.0, 0.040
# the chirp's band (SYNC_BAND=F0,F1 Hz): 18-20 kHz is inaudible to most adults but MacBook speakers barely play it;
# an audible band (e.g. 3000,6000) for tests against the Mac's own speakers
F0, F1 = (float(v) for v in os.environ.get("SYNC_BAND", "18000,20000").split(","))
LEVEL_DB = float(os.environ.get("SYNC_LEVEL", "-12"))


def chirp(fs, level_db=None):
    return audio.chirp(F0, F1, CHIRP_S, fs, LEVEL_DB if level_db is None else level_db)


def locate(x, fs, near=None, width=None):
    """Chirp arrivals in x (sample positions of their starts, sub-sample)."""
    return audio.locate(x, chirp(fs), int(0.5 * PERIOD_S * fs), near, width)


def gen(d, secs=64, fs=FS):
    os.makedirs(d, exist_ok=True)
    x = np.zeros(int(secs * fs))
    c = chirp(fs)
    k = 0
    while FIRST_S + k * PERIOD_S + CHIRP_S < secs - 1:
        i = int((FIRST_S + k * PERIOD_S) * fs)
        x[i : i + len(c)] += c
        k += 1
    audio.write_wav(os.path.join(d, "chirps.wav"), x, fs)
    subprocess.run(
        [
            "afconvert",
            "-f",
            "m4af",
            "-d",
            "aac@%d" % fs,
            "-b",
            "256000",
            os.path.join(d, "chirps.wav"),
            os.path.join(d, "chirps.m4a"),
        ],
        check=True,
    )
    subprocess.run(
        [sys.executable, "-I", M4A2FRAMES, os.path.join(d, "chirps.m4a"), os.path.join(d, "chirps.frames")], check=True
    )
    check = os.path.join(d, "aaccheck")
    srcs = ["check/aaccheck.c", "aacdec.c", "aac_tables.c", DSP + "/imdct.c", DSP + "/imdct_tables.c"]
    subprocess.run(
        ["cc", "-O2", "-I" + AAC, "-I" + DSP, "-o", check] + [os.path.join(AAC, f) for f in srcs] + ["-lm"], check=True
    )
    ours = os.path.join(d, "chirps.raw")
    subprocess.run([check, "dump", os.path.join(d, "chirps.frames"), ours, "2", str(fs)], check=True)
    pcm = np.frombuffer(open(ours, "rb").read(), "<i2").reshape(-1, 2)[:, 0] / 32768
    on = [p for p in locate(pcm.astype(float), fs) if p is not None]
    json.dump({"onsets": on, "chirps": k, "rate": fs}, open(os.path.join(d, "onsets.json"), "w"))
    print(
        "%d chirps in the WAV; in the decoded stream at %s ... (samples; first expected %.0f + priming)"
        % (k, ["%.1f" % p for p in on[:3]], FIRST_S * fs)
    )
    lvl = 20 * np.log10(np.abs(pcm).max() + 1e-12)
    print("decoded peak %.1f dBFS (the encoder kept the chirps)" % lvl if lvl > -30 else "chirps lost in encoding!")


def loop(d):
    sd = audio._sd()
    fs = 48000
    c = chirp(fs, -6)
    sig = np.zeros(int(12 * fs))
    starts = [int((1 + 2 * k) * fs) for k in range(5)]
    for s0 in starts:
        sig[s0 : s0 + len(c)] = c
    dac, out, pos = [], audio.out(local=True), [0]

    def cb(outdata, frames, t, status):
        dac.append((pos[0], t.outputBufferDacTime))
        outdata[:, 0] = sig[pos[0] : pos[0] + frames] if pos[0] + frames <= len(sig) else 0
        pos[0] += frames

    with audio.Recorder(fs) as rec:
        with sd.OutputStream(samplerate=fs, channels=1, device=out, callback=cb, blocksize=1024):
            time.sleep(len(sig) / fs + 0.5)
    x, t_of = rec.signal()
    p, tt = np.array([a for a, _ in dac]), np.array([b for _, b in dac])
    sl, ic = np.polyfit(p, tt, 1)
    sent = [ic + sl * s0 - np.median(rec.checks) for s0 in starts]
    got = locate(x, fs, near=[(t - t_of(0)) * fs for t in sent], width=0.3 * fs)
    errs = [(t_of(g) - s) * 1e3 for g, s in zip(got, sent) if g is not None]
    print("speaker -> mic, by the timestamps: %s ms (median %.2f)" % (["%.2f" % e for e in errs], np.median(errs)))
    json.dump({"loop_ms": float(np.median(errs))}, open(os.path.join(d, "loop.json"), "w"))


def run(d, host, secs=40.0):
    meta = json.load(open(os.path.join(d, "onsets.json")))
    on, rate = meta["onsets"], meta.get("rate", FS)
    loop_ms = (
        json.load(open(os.path.join(d, "loop.json")))["loop_ms"]
        if os.path.exists(os.path.join(d, "loop.json"))
        else 0.0
    )
    anchor_file = os.path.join(d, "anchor.json")
    if os.path.exists(anchor_file):
        os.remove(anchor_file)
    with audio.Recorder() as rec:
        p = subprocess.run(
            [
                AP2SEND,
                host,
                os.path.join(d, "chirps.frames"),
                "--ptp",
                "--lead",
                "3",
                "--secs",
                str(secs),
                "--anchor-out",
                anchor_file,
            ]
            + (["--aac48"] if rate == 48000 else []),
            capture_output=True,
            text=True,
        )
        time.sleep(1.0)
    print(p.stdout.strip())
    if p.returncode:
        print(p.stderr)
        return
    a = json.load(open(anchor_file))
    x, t_of = rec.signal()
    fs = rec.fs
    np.savez(os.path.join(d, "last-run.npz"), x=x, t0=t_of(0), t1=t_of(1) - t_of(0))
    # every chirp in the recording, wherever it is, against the nearest due time
    seen = [p for p in locate(x, fs) if p is not None]
    due_all = np.array([a["net_ns"] / 1e9 + (o - (a["rtp"] - a["first_ts"])) / rate for o in on])
    near = [((t_of(p) - due_all[np.argmin(np.abs(due_all - t_of(p)))]) * 1e3) for p in seen]
    print(
        "chirp-like peaks anywhere: %d; offset from the nearest due time (ms): %s"
        % (len(seen), " ".join("%.1f" % v for v in near[:30]))
    )
    due = [a["net_ns"] / 1e9 + (o - (a["rtp"] - a["first_ts"])) / rate for o in on]
    due = [t for t in due if t < a["net_ns"] / 1e9 + secs]
    got = locate(x, fs, near=[(t - t_of(0)) * fs for t in due], width=0.25 * fs)
    errs = [(t_of(g) - t) * 1e3 - loop_ms for g, t in zip(got, due) if g is not None]
    print(
        "chirps heard %d of %d; error (heard - due, ms; mic path %.2f ms taken off):" % (len(errs), len(due), loop_ms)
    )
    print("  " + " ".join("%.1f" % e for e in errs))
    if errs:
        e = np.array(errs)
        print(
            "  median %.2f  mean %.2f  sd %.2f  min %.2f  max %.2f"
            % (np.median(e), e.mean(), e.std(), e.min(), e.max())
        )
    json.dump({"errors_ms": errs}, open(os.path.join(d, "run-%d.json" % int(time.time())), "w"))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("gen")
    p.add_argument("dir")
    p.add_argument("secs", type=float, nargs="?", default=64)
    p.add_argument("rate", type=int, nargs="?", default=FS, choices=[44100, 48000])
    p = sub.add_parser("loop")
    p.add_argument("dir")
    p = sub.add_parser("run")
    p.add_argument("dir")
    p.add_argument("host")
    p.add_argument("secs", type=float, nargs="?", default=40.0)
    a = ap.parse_args()
    if a.cmd == "gen":
        gen(a.dir, a.secs, a.rate)
    elif a.cmd == "loop":
        loop(a.dir)
    else:
        run(a.dir, a.host, a.secs)


if __name__ == "__main__":
    main()
