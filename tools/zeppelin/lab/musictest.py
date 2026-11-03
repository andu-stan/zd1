#!/usr/bin/env python3
"""AirPlay from a real Mac: the Music app sends test tracks (chirps at 15.5-17.5 kHz, barely audible and not from
another room), the MacBook microphone listens, the speaker's receiver log says what it saw (docs/streaming.md,
Synchronised playback).

  musictest.py sync [N]          N sessions (default 1) grouped with the Mac's own speakers ("This Computer"): both,
                                 then the Mac muted (the speaker alone), then the speaker muted (the Mac alone); each
                                 one's chirps on the 2 s grid, speaker - Mac in ms (the microphone's path cancels), and
                                 each against the network timeline (the Mac's PTP time is its mach clock, which also
                                 stamps the microphone; the receiver's log gives the anchor)
  musictest.py events [EV,...]   the same per segment through Music's controls: play,pause,seek,next (default all)
  musictest.py stress [N]        N cycles of pick / play / pause / resume / seek / volume / next track / stop / pick
                                 the Mac; the receiver's memory, threads and descriptors after each (run the receiver
                                 apart from the MCU first: ZD1_LINK=/tmp/none zd1-airplay --device null, so no amp)
  musictest.py channels [LO,HI]  chirps (LO-HI Hz, default the band above) to the speaker's USB audio input, left
                                 channel then right: each one's arrivals against CoreAudio's output time. Both should
                                 be heard first at about the same time; a weak early arrival under a later, stronger
                                 one is a shadowed tweeter (the MacBook's lid), not a late speaker: at 4000,6000
                                 (audible) both come back on time
  musictest.py dump TRACK SECS   play TRACK to the speaker alone at volume 0 (the receiver started with
                                 ZD1_PCM_HASH and ZD1_PCM_DUMP=/tmp/pcm.raw writes what it decoded)
  musictest.py gen               the test tracks into the work directory (afconvert)

Music quirks met on the way, which the tool works around: setting the volume of an AirPlay device that isn't selected
makes Music cancel the connection at the next play (-128); so does recording while Music starts (corespeechd, Siri's
listener, takes the microphone back and audiomxd interrupts Music: "beginInterruption"), so the microphone opens once
playback runs, and a session the speaker was dropped from at the start is run again; a receiver that went away stays selected but dead until it is picked
again, so every run picks the Mac first; Music applies its loudness gain to these tracks (-18 dB: chirps written at
-10 dBFS arrive at -28). Tracks are added to Music once (a playlist "zd1-test"); remove them by hand when done.
Work files (tracks, recordings) go to $MUSICTEST_DIR (default ~/.cache/zd1-musictest). The microphone is $MIC, else
the default input; the speaker is "Zeppelin Air" in Music's AirPlay list and on USB ($ZEP, $OUT: lab/audio.py). The
receiver's side comes from the module's console (`zd1 console run`). Run in `nix develop` (numpy, sounddevice).
"""

import argparse
import json
import os
import random
import re
import subprocess
import sys
import time

import numpy as np

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
from zeppelin import zd1  # noqa: E402
from zeppelin.lab import audio  # noqa: E402

WORK = os.environ.get("MUSICTEST_DIR", os.path.expanduser("~/.cache/zd1-musictest"))
ZEP = os.environ.get("ZEP", audio.SPEAKER)
FS_MIC, FS = 48000, 44100
BAND = (15500.0, 17500.0, 0.25)  # the chirp: Hz, Hz, seconds; every 2 s from 2 s into the track
PLAYLIST = "zd1-test"


def osa(script):
    r = subprocess.run(["osascript", "-e", script], capture_output=True, text=True)
    return r.stdout.strip()


def music(cmd):
    return osa('tell application "Music" to ' + cmd)


def modcon(cmd):
    """a shell command on the module (its console over the host link): its output"""
    return zd1.run_on_module(cmd)


def gen():
    os.makedirs(WORK, exist_ok=True)
    for name, level_db in (("chirps-a", -10), ("chirps-b", -10)):
        x = np.zeros(64 * FS)
        c = 10 ** (level_db / 20) * audio.chirp(*BAND[:2], BAND[2], FS)
        for k in range(31):
            i = int((2 + 2 * k) * FS)
            x[i : i + len(c)] += c
        path = os.path.join(WORK, name)
        audio.write_wav(path + ".wav", x, FS)
        subprocess.run(
            ["afconvert", "-f", "m4af", "-d", "aac", "-b", "256000", path + ".wav", path + ".m4a"], check=True
        )
        os.remove(path + ".wav")
    print("tracks in", WORK)


def track(name):
    """Music's persistent ID of WORK/NAME.m4a, added once"""
    db = os.path.join(WORK, "tracks.json")
    ids = json.load(open(db)) if os.path.exists(db) else {}
    pid = ids.get(name)
    if pid and music('exists (some track of library playlist 1 whose persistent ID is "%s")' % pid) == "true":
        return pid
    if not os.path.exists(os.path.join(WORK, name + ".m4a")):
        gen()
    pid = music('get persistent ID of (add (POSIX file "%s"))' % os.path.join(WORK, name + ".m4a"))
    ids[name] = pid
    json.dump(ids, open(db, "w"))
    return pid


def playlist():
    if music('exists playlist "%s"' % PLAYLIST) != "true":
        music('make new user playlist with properties {name:"%s"}' % PLAYLIST)
        for t in ("chirps-a", "chirps-b"):
            music('duplicate (some track whose persistent ID is "%s") to playlist "%s"' % (track(t), PLAYLIST))
    return PLAYLIST


def mac_name():
    return osa('tell application "Music" to get name of (every AirPlay device whose kind is computer)').split(", ")[0]


def select(devices, vols=None, wait=6.0):
    """Music's output: a fresh pick of DEVICES (the Mac first), then their volumes once selected"""
    music('set current AirPlay devices to {AirPlay device "%s"}' % mac_name())
    time.sleep(1.0)
    music("set current AirPlay devices to {%s}" % ", ".join('AirPlay device "%s"' % d for d in devices))
    time.sleep(wait)
    for d, v in (vols or {}).items():
        music('set sound volume of AirPlay device "%s" to %s' % (d, v))
    time.sleep(1.0)


def envelope(x, band=BAND):
    return audio.envelope(x, audio.chirp(*band[:2], band[2], FS_MIC))


def ticks(env, lo_s, hi_s):
    """the strongest arrival in each 2 s period between lo and hi (s into the recording)"""
    out, k, hi, per = [], int(lo_s * FS_MIC), int(hi_s * FS_MIC), 2 * FS_MIC
    while k + per <= hi:
        out.append(k + int(np.argmax(env[k : k + per])))
        k += per
    return out


def grid_ms(js, base):
    """offsets (ms) from the 2 s grid through sample `base`"""
    return [((j - base) / FS_MIC - round((j - base) / FS_MIC / 2) * 2) * 1e3 for j in js]


def earlier(env, js, lo_ms=20.0, floor=0.15):
    """arrivals before the strongest one: the envelope around each tick in JS, averaged; its peaks at least FLOOR of
    the tick's and 1-LO_MS ms ahead of it, as (ms before, level). A tweeter whose direct path to the microphone is
    shadowed (the MacBook's own lid) leaves its direct sound there and a room reflection as the tick: the tick is
    then late by the reflection's extra path, 11 ms in the case met (docs/streaming.md, Calibration)"""
    n = int(lo_ms / 1e3 * FS_MIC)
    segs = [env[j - n : j + 1] / env[j] for j in js if j >= n]
    if not segs:
        return []
    p = np.mean(segs, 0)
    return [
        ((n - i) / FS_MIC * 1e3, p[i])
        for i in range(1, n - int(FS_MIC / 1e3))
        if p[i] >= p[i - 1] and p[i] > p[i + 1] and p[i] >= floor
    ]


def ended():
    """teardowns the receiver has seen (a drop at the start shows as one more: of the stream, or of the session)"""
    out = modcon('cat /tmp/ap.log /var/log/airplay.log 2>/dev/null | grep -c "TEARDOWN streams_only"')
    nums = [int(v) for v in out.split() if v.isdigit()]
    return nums[-1] if nums else 0


def started():
    """play starts the receiver has seen (AP2's SETRATEANCHORTIME with a rate)"""
    out = modcon('cat /tmp/ap.log /var/log/airplay.log 2>/dev/null | grep -c "AP2 play start"')
    nums = [int(v) for v in out.split() if v.isdigit()]
    return nums[-1] if nums else 0


def play_kept(what):
    """play WHAT (a Music object) and make sure the speaker stayed in: False if Music dropped it"""
    n = ended()
    music("play " + what)
    time.sleep(3)
    return ended() == n


def anchor():
    """the receiver's last play start: (anchor RTP time, its network time s, the first queued frame's RTP time, the
    stream's RTP rate)"""
    # by time, both logs: a receiver run from /tmp leaves the service's log with older anchors
    out = modcon(
        'grep -hE "AP2 play start|Playout on the network timeline|Audio format detected"'
        " /tmp/ap.log /var/log/airplay.log 2>/dev/null | sort | tail -6"
    )
    rtp = net = front = None
    rate = FS
    for line in out.splitlines():
        m = re.search(r"AP2 play start rtp_time=(\d+) anchor_time_ns=(\d+)", line)
        if m:
            rtp, net = int(m.group(1)), int(m.group(2)) / 1e9
        m = re.search(r"front=Some\(\(\d+, (\d+)\)\)", line)
        if m:
            front = int(m.group(1))
        m = re.search(r"src_sr=(\d+)", line)
        if m:
            rate = int(m.group(1))  # Music sends AAC at 48 kHz too, its RTP clock at that rate
    return rtp, net, front, rate


class Session:
    """Music playing to the Mac and the speaker, the microphone open, volumes put back after"""

    def __init__(self, macvol="20", zepvol="35"):
        self.macvol, self.zepvol, self.mac = macvol, zepvol, mac_name()

    def __enter__(self):
        self.vol0 = osa("output volume of (get volume settings)")
        music("set shuffle enabled to false")
        music("set song repeat to off")
        music("set sound volume to 100")
        osa("set volume output volume %s" % self.macvol)
        select([self.mac, ZEP], {ZEP: self.zepvol, self.mac: 100})
        self.rec = None
        return self

    def play(self, what):
        """play WHAT; then the microphone (recording while Music starts makes corespeechd, Siri's listener, take
        the microphone back and interrupt Music, which drops the speaker): False if the speaker was dropped"""
        if not play_kept(what):
            return False
        self.rec = audio.Recorder(FS_MIC).__enter__()
        time.sleep(1.0)
        return True

    def __exit__(self, *a):
        if self.rec:
            self.rec.__exit__()
        music("stop")
        music('set sound volume of AirPlay device "%s" to %s' % (self.mac, 100))
        music('set sound volume of AirPlay device "%s" to %s' % (ZEP, self.zepvol))
        time.sleep(1.0)
        music('set current AirPlay devices to {AirPlay device "%s"}' % self.mac)
        osa("set volume output volume %s" % (self.vol0 or 30))

    def alone(self, secs):
        """the speaker alone (the Mac muted), then the Mac alone: the windows (s into the recording)"""
        t1 = self.rec.now()
        music('set sound volume of AirPlay device "%s" to 0' % self.mac)
        time.sleep(secs)
        t2 = self.rec.now()
        music('set sound volume of AirPlay device "%s" to 100' % self.mac)
        music('set sound volume of AirPlay device "%s" to 0' % ZEP)
        time.sleep(secs)
        t3 = self.rec.now()
        music('set sound volume of AirPlay device "%s" to %s' % (ZEP, self.zepvol))
        return t1, t2, t3


def compare(env, t1, t2, t3):
    zb, mc = ticks(env, t1 + 0.8, t2 + 0.3), ticks(env, t2 + 0.8, t3 + 0.3)
    if not zb or not mc:
        return None
    ob, oc = grid_ms(zb, mc[0]), grid_ms(mc, mc[0])
    level = np.median([env[j] for j in zb]) / np.median([env[j] for j in mc])
    return np.median(ob) - np.median(oc), np.std(ob), np.std(oc), level, zb, mc


def sync(n=1):
    done = tries = 0
    while done < n and tries < 3 * n:
        tries += 1
        n0 = started()
        with Session() as s:
            if not s.play('(some track whose persistent ID is "%s")' % track("chirps-a")):
                print("Music dropped the speaker at the start: again")
                continue
            time.sleep(4)
            t1, t2, t3 = s.alone(14)
        if started() == n0:
            # what the microphone heard was the Mac alone: no figure
            print("the speaker never started playing (a sender asking for a PIN? a Music dialog left open?): again")
            continue
        done += 1
        x, host = s.rec.signal()
        env = envelope(x)
        r = compare(env, t1, t2, t3)
        if r is None:
            print("nothing heard: did the speaker play? (its log, `zd1 info`)")
            continue
        d, sb, sc, level, zb, mc = r
        line = "speaker - Mac %.2f ms (sd %.2f / %.2f; speaker at %.2f of the Mac at the microphone)" % (
            d,
            sb,
            sc,
            level,
        )
        rtp, net, front, rate = anchor()
        if net and front is not None:
            d = ((front - rtp + (1 << 31)) & 0xFFFFFFFF) - (1 << 31)  # signed: the first frame may come before
            t0 = net + d / rate + 2.0  # chirp 0: 2 s after the first frame sent
            ab = lambda js: np.median([((host(j) - t0) - round((host(j) - t0) / 2) * 2) * 1e3 for j in js])
            line += "; vs the network timeline (+ the microphone path): speaker %.2f, Mac %.2f ms" % (ab(zb), ab(mc))
        print(line)
        early = earlier(env, zb)
        if early:
            print(
                "  but the speaker was heard earlier too: %s (ms before, level): its direct sound may be shadowed and "
                "the figure a reflection; check with `musictest.py channels`"
                % ", ".join("%.1f at %.2f" % e for e in early)
            )
        time.sleep(4)


def events(evs):
    pl = playlist()
    segs = []
    with Session() as s:
        for ev in evs:
            if ev == "play":
                if not s.play('playlist "%s"' % pl):
                    print("Music dropped the speaker at the start")
                    return
            elif ev == "pause":
                music("pause")
                time.sleep(3.0)
                music("play")
                time.sleep(2.0)
            elif ev == "seek":
                music("set player position to (player position + 7)")
                time.sleep(2.5)
            elif ev == "next":
                music("next track")
                time.sleep(3.5)
            segs.append((ev,) + s.alone(6.5))
    x, _ = s.rec.signal()
    env = envelope(x)
    for ev, t1, t2, t3 in segs:
        r = compare(env, t1, t2, t3)
        if not r:
            print("%-6s nothing heard" % ev)
            continue
        zb, mc = r[4], r[5]
        print(
            "%-6s speaker - Mac %.2f ms (sd %.2f / %.2f); speaker %s | Mac %s"
            % (
                (ev,)
                + r[:3]
                + (" ".join("%.1f" % v for v in grid_ms(zb, mc[0])), " ".join("%.1f" % v for v in grid_ms(mc, mc[0])))
            )
        )


def stress(n):
    def res():
        o = modcon(
            'p=$(pidof zd1-airplay); echo $(grep VmRSS /proc/$p/status | tr -s " " | cut -d" " -f2)'
            " $(grep Threads /proc/$p/status | cut -f2) $(ls /proc/$p/fd | wc -l)"
        )
        return o.strip().split("\n")[-1]

    pl = playlist()
    v0 = osa("output volume of (get volume settings)")
    osa("set volume output volume 4")  # if Music falls back to the Mac's speakers
    print("start: RSS kB, threads, descriptors:", res())
    try:
        for i in range(n):
            select([ZEP], {ZEP: random.choice([10, 30, 50])}, wait=5)
            music('play playlist "%s"' % pl)
            time.sleep(4)
            music("pause")
            time.sleep(random.uniform(0.3, 2))
            music("play")
            time.sleep(2)
            music("set player position to (player position + %d)" % random.randint(3, 20))
            time.sleep(2)
            music('set sound volume of AirPlay device "%s" to %d' % (ZEP, random.randint(0, 60)))
            time.sleep(1)
            music("next track")
            time.sleep(3)
            state = music("player state")
            music("stop")
            time.sleep(1)
            print("cycle %2d: Music %s | %s" % (i + 1, state, res()))
    finally:
        music("stop")
        music('set current AirPlay devices to {AirPlay device "%s"}' % mac_name())
        osa("set volume output volume %s" % v0)


def channels(band=BAND, n=5):
    """chirps in BAND every 2 s to the speaker's USB audio input, the left channel, then the right (not through the
    module: the speaker's DSP, amp and drivers, and the room): each one's arrivals against CoreAudio's output time"""
    sd, dev = audio._sd(), audio.out()
    c = 10 ** (-10 / 20) * audio.chirp(band[0], band[1], band[2], FS)
    sig = np.zeros(int((1 + 2 * n) * FS))
    starts = [int((1 + 2 * k) * FS) for k in range(n)]
    for i in starts:
        sig[i : i + len(c)] += c
    for ch, name in ((0, "left"), (1, "right")):
        pos, first = [0], []

        def cb(out, frames, t, status):
            if not first:
                first.append((t.outputBufferDacTime, pos[0]))
            k = min(frames, len(sig) - pos[0])
            out[:] = 0
            out[:k, ch] = sig[pos[0] : pos[0] + k]
            pos[0] += frames
            if pos[0] >= len(sig):
                raise sd.CallbackStop

        with audio.Recorder(FS_MIC) as rec:
            time.sleep(1.0)
            with sd.OutputStream(samplerate=FS, channels=2, device=dev, callback=cb, blocksize=512) as st:
                while st.active:
                    time.sleep(0.1)
            time.sleep(0.5)
        x, host = rec.signal()
        env = envelope(x, band)
        per = (host(len(x) - 1) - host(0)) / (len(x) - 1)
        dac0, p0 = first[0]
        # the first chirp only primes the path (the speaker switches to USB when sound arrives)
        segs = []
        for s in starts[1:]:
            a = int((dac0 + (s - p0) / FS - host(0)) / per)
            w = env[a : a + int(0.2 * FS_MIC)]
            segs.append(w / w.max())
        p = np.mean(segs, 0)
        pk = [i for i in range(1, len(p) - 1) if p[i] >= p[i - 1] and p[i] > p[i + 1] and p[i] > 0.15 * p.max()]
        print(
            "%-5s heard (ms after the output time: level) %s"
            % (name, "  ".join("%.1f: %.2f" % (i / FS_MIC * 1e3, p[i] / p.max()) for i in pk[:8]))
        )


def dump(name, secs):
    select([ZEP], {ZEP: 0})
    music('play (some track whose persistent ID is "%s")' % track(name))
    time.sleep(secs)
    music("stop")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("sync").add_argument("n", type=int, nargs="?", default=1)
    sub.add_parser("events").add_argument("evs", nargs="?", default="play,pause,seek,next", metavar="EV,...")
    sub.add_parser("stress").add_argument("n", type=int, nargs="?", default=10)
    sub.add_parser("channels").add_argument("band", nargs="?", metavar="LO,HI")
    p = sub.add_parser("dump")
    p.add_argument("track")
    p.add_argument("secs", type=float)
    sub.add_parser("gen")
    a = ap.parse_args()
    if a.cmd == "gen":
        gen()
    elif a.cmd == "sync":
        sync(a.n)
    elif a.cmd == "events":
        events(a.evs.split(","))
    elif a.cmd == "stress":
        stress(a.n)
    elif a.cmd == "channels":
        channels(tuple(float(v) for v in a.band.split(",")) + (BAND[2],) if a.band else BAND)
    else:
        dump(a.track, a.secs)


if __name__ == "__main__":
    main()
