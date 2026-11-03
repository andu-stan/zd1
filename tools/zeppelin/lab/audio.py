"""What the acoustic lab tools share (usprobe, musictest, synctest): test signals, finding them in a recording, the
microphone with host timestamps, the choice of devices. numpy and sounddevice come with `nix develop`.

Devices: the microphone is $MIC (a substring of its name), else the system's default input; the speaker's USB audio
output is $OUT, else the device named "Zeppelin Air"; local=True means the system's default output (the computer's own
speaker) unless $OUT names one.
"""

import os
import time
import wave

import numpy as np

SPEAKER = "Zeppelin Air"  # the USB audio device's name (the MCU's descriptors)


def _sd():
    import sounddevice as sd

    return sd


def device(kind, name=None):
    """The sounddevice index of an input ("in") or output ("out") device whose name contains `name`; None: the
    system's default."""
    sd = _sd()
    if name is None:
        return sd.default.device[0 if kind == "in" else 1]
    key = "max_input_channels" if kind == "in" else "max_output_channels"
    for i, d in enumerate(sd.query_devices()):
        if name in d["name"] and d[key]:
            return i
    raise SystemExit(
        "no %s device named like %r (%s)" % ("input" if kind == "in" else "output", name, sd.query_devices())
    )


def mic():
    return device("in", os.environ.get("MIC"))


def out(local=False):
    return device("out", os.environ.get("OUT") or (None if local else SPEAKER))


def name(dev):
    return _sd().query_devices(dev)["name"]


# ---- signals -----------------------------------------------------------------------------------------------------
def mseq31():
    """The 31-chip maximal-length sequence of x^5 + x^3 + 1, as 0/1."""
    reg, code = [1, 0, 0, 0, 0], []
    for _ in range(31):
        code.append(reg[-1])
        reg = [reg[4] ^ reg[2]] + reg[:-1]
    return np.array(code)


def keyed_tone(code, hz, fs, dbfs, chip_s=0.025, before_s=0.0, after_s=0.0):
    """A tone at `hz` keyed on and off by `code` (`chip_s` a chip, 3 ms raised edges), silence around it."""
    env = np.repeat(np.asarray(code, float), int(chip_s * fs))
    r = int(0.003 * fs)
    env = np.convolve(env, np.ones(2 * r) / (2 * r), "same")
    env = np.concatenate([np.zeros(int(before_s * fs)), env, np.zeros(int(after_s * fs))])
    return 10 ** (-abs(dbfs) / 20) * env * np.sin(2 * np.pi * hz * np.arange(len(env)) / fs)


def code_score(x, fs, hz, code, chip_s=0.025):
    """The `hz` band's envelope (a quadrature mixer, a chip-long moving average, 5 frames a chip) correlated with
    `code`: (score = the peak over the spread of the other lags, the peak's time in s, the carrier's level on and
    off in dB). A score over 6 means the code is there."""
    chip = int(chip_s * fs)
    hop = chip // 5
    bb = x * np.exp(-2j * np.pi * hz * np.arange(len(x)) / fs)
    e = np.abs(np.convolve(bb, np.ones(chip) / chip, "same"))[::hop]
    ref = np.repeat(np.asarray(code) * 2.0 - 1, 5)
    c = np.correlate(e - e.mean(), ref, "valid")
    pk = int(np.argmax(c))
    others = np.delete(c, range(max(0, pk - 10), pk + 10))
    score = (c[pk] - others.mean()) / (others.std() + 1e-12)
    seg = e[pk : pk + len(ref)]
    db = lambda v: 20 * np.log10(v + 1e-12)  # noqa: E731
    return score, pk * hop / fs, db(seg[ref > 0].mean()), db(seg[ref < 0].mean())


def chirp(f0, f1, secs, fs, level_db=0.0):
    """A linear sweep from f0 to f1 Hz, Hann-windowed."""
    t = np.arange(int(secs * fs)) / fs
    return 10 ** (level_db / 20) * np.hanning(len(t)) * np.sin(2 * np.pi * (f0 * t + (f1 - f0) / (2 * secs) * t * t))


def envelope(x, c):
    """The matched filter for `c` over x, its analytic envelope: index i = `c` starting at sample i."""
    nf = 1 << (len(x) + len(c) - 1).bit_length()
    y = np.fft.irfft(np.fft.rfft(x, nf) * np.fft.rfft(c[::-1], nf), nf)[: len(x) + len(c) - 1]
    Y = np.fft.fft(y)
    h = np.zeros(len(Y))
    h[0] = 1
    h[1 : (len(Y) + 1) // 2] = 2
    if len(Y) % 2 == 0:
        h[len(Y) // 2] = 1
    return np.abs(np.fft.ifft(Y * h))[len(c) - 1 :]


def locate(x, c, gap, near=None, width=None):
    """Where `c` starts in x (sub-sample, by the envelope's peak). Without `near`: every arrival over 30 % of the
    loudest, at least `gap` samples apart. With `near`: the peak within `width` of each expected position, None
    where it isn't 5 times the window's median."""
    env = envelope(x, c)
    out = []
    if near is None:
        thr, i = env.max() * 0.3, 0
        while i < len(env):
            if env[i] > thr:
                j = i + int(np.argmax(env[i : i + gap]))
                out.append(j)
                i = j + gap
            else:
                i += 1
    else:
        for p in near:
            a, b = max(0, int(p - width)), min(len(env), int(p + width))
            if b - a < 3:
                out.append(None)
                continue
            j = a + int(np.argmax(env[a:b]))
            out.append(j if env[j] > 5 * np.median(env[a:b]) else None)
    res = []
    for j in out:
        if j is None or j <= 0 or j >= len(env) - 1:
            res.append(None)
            continue
        a, b, cc = env[j - 1], env[j], env[j + 1]
        res.append(j + (0.5 * (a - cc) / (a - 2 * b + cc) if a - 2 * b + cc else 0))
    return res


def write_wav(path, x, fs):
    """x (float, -1..1) as a 16-bit stereo WAV, the same on both channels."""
    s = (np.asarray(x) * 32767).astype("<i2")
    w = wave.open(path, "wb")
    w.setnchannels(2)
    w.setsampwidth(2)
    w.setframerate(fs)
    w.writeframes(np.stack([s, s], 1).tobytes())
    w.close()


# ---- recording ---------------------------------------------------------------------------------------------------
class Recorder:
    """The microphone, each block with its host time (PortAudio's stream time is CoreAudio's host time on a Mac, the
    clock CLOCK_UPTIME_RAW reads, which is also the Mac's AirPlay PTP time)."""

    def __init__(self, fs=48000, dev=None):
        self.sd, self.fs, self.blocks, self.checks = _sd(), fs, [], []
        self.dev = mic() if dev is None else dev

    def __enter__(self):
        def cb(indata, frames, t, status):
            self.blocks.append((t.inputBufferAdcTime, indata[:, 0].copy()))
            if len(self.checks) < 200:  # stream time - CLOCK_UPTIME_RAW
                self.checks.append(t.currentTime - time.clock_gettime_ns(time.CLOCK_UPTIME_RAW) / 1e9)

        self.st = self.sd.InputStream(samplerate=self.fs, channels=1, device=self.dev, callback=cb, blocksize=1024)
        self.st.start()
        self.t0 = time.time()
        return self

    def __exit__(self, *a):
        self.st.stop()
        self.st.close()

    def now(self):
        """Seconds since the recording started."""
        return time.time() - self.t0

    def signal(self):
        """(the samples, sample index -> CLOCK_UPTIME_RAW seconds), from a line through the blocks' times."""
        x = np.concatenate([b for _, b in self.blocks]).astype(float)
        idx = np.cumsum([0] + [len(b) for _, b in self.blocks[:-1]])
        slope, icp = np.polyfit(idx, np.array([t for t, _ in self.blocks]), 1)
        off = np.median(self.checks)
        return x, lambda i: icp + slope * i - off
