#!/usr/bin/env python3
"""The LED palette's perception check (src/led.c). Each look's PWM duty as led.c computes it (gamma 2, integer duty of
400) -> CIE XYZ through the measured LED model (led_model.json: each channel's XYZ at full duty; light is linear in
duty and additive on this LED) -> CIELAB (D65 at the LED's full white) -> CIEDE2000. --srgb: the channels as linear
sRGB instead (white at equal duty), which got the blues badly wrong (a violet looked like the sky blue, a lavender
white).

The model [HW 2026-10-27]: an iPhone in Apple Log (Rec.2020) filmed a sequence the firmware drove, each channel alone
at duties 12-400, then every look. From the footage: each channel's XYZ at full duty (XYZ400) and its brightness by
duty (Y_by_duty; a power law with the exponent `exponent`, ~1.1), full white (white400: the channels' sum, 0.999)
and the looks as filmed. The values are the camera's (it overrates blue against the eye). The LED is the same on
every unit, so the model is too.

Prints the smallest difference between looks that must be told apart (every colour playing and asleep, each asleep
against standby's red, each playing against the error's); --search looks for a better palette with the computer's
blue and Spotify's green kept, AirPlay one of a few whites, the others within their hue ranges at two channels
(a third reads as white on this LED). A model: the eye on the unit has the last word.
  python3 mcu/tools/led_palette.py [--search] [--srgb]"""

import colorsys
import itertools
import json
import math
import os
import random
import sys

P = 400
MODEL = os.path.join(os.path.dirname(os.path.abspath(__file__)), "led_model.json")

# = src/led.c (Wi-Fi without a named receiver shares AirPlay's white)
CURRENT = {
    "USB": (0, 174, 255),
    "AUX": (255, 221, 0),
    "AirPlay": (255, 240, 120),
    "Spotify": (106, 255, 0),
    "Snapcast": (0, 255, 128),
    "iPod": (255, 0, 161),
}
RED = (255, 0, 0)


def duty(c, lvl):  # led.c: perceptual c*lvl, gamma 2, integer duty
    p = c * lvl // 255
    return p * p * P // 65025


def load_model(srgb):
    if srgb:
        return [[0.4124, 0.3576, 0.1805], [0.2126, 0.7152, 0.0722], [0.0193, 0.1192, 0.9505]], "sRGB"
    m = json.load(open(MODEL))
    cols = [m["channels"][c]["XYZ400"] for c in ("red", "green", "blue")]
    return [[cols[j][i] for j in range(3)] for i in range(3)], os.path.basename(MODEL)


M, SOURCE = load_model("--srgb" in sys.argv)
YW = sum(M[1])  # the LED's full white
WHITE_REF = (0.95047 * YW, YW, 1.08883 * YW)


def lab(d):
    v = [x / P for x in d]
    X, Y, Z = (sum(M[i][j] * v[j] for j in range(3)) for i in range(3))
    f = lambda t: t ** (1 / 3) if t > 216 / 24389 else (24389 / 27 * t + 16) / 116
    fx, fy, fz = f(X / WHITE_REF[0]), f(Y / WHITE_REF[1]), f(Z / WHITE_REF[2])
    return (116 * fy - 16, 500 * (fx - fy), 200 * (fy - fz))


def de2000(a, b):
    L1, a1, b1 = a
    L2, a2, b2 = b
    C1, C2 = math.hypot(a1, b1), math.hypot(a2, b2)
    Cb = (C1 + C2) / 2
    G = 0.5 * (1 - math.sqrt(Cb**7 / (Cb**7 + 25**7)))
    a1p, a2p = (1 + G) * a1, (1 + G) * a2
    C1p, C2p = math.hypot(a1p, b1), math.hypot(a2p, b2)
    h1p = math.degrees(math.atan2(b1, a1p)) % 360
    h2p = math.degrees(math.atan2(b2, a2p)) % 360
    dLp, dCp = L2 - L1, C2p - C1p
    dhp = (
        0
        if C1p * C2p == 0
        else (h2p - h1p if abs(h2p - h1p) <= 180 else h2p - h1p - 360 if h2p > h1p else h2p - h1p + 360)
    )
    dHp = 2 * math.sqrt(C1p * C2p) * math.sin(math.radians(dhp / 2))
    Lbp, Cbp = (L1 + L2) / 2, (C1p + C2p) / 2
    hbp = (
        h1p + h2p
        if C1p * C2p == 0
        else (
            (h1p + h2p) / 2
            if abs(h1p - h2p) <= 180
            else (h1p + h2p + 360) / 2
            if h1p + h2p < 360
            else (h1p + h2p - 360) / 2
        )
    )
    T = (
        1
        - 0.17 * math.cos(math.radians(hbp - 30))
        + 0.24 * math.cos(math.radians(2 * hbp))
        + 0.32 * math.cos(math.radians(3 * hbp + 6))
        - 0.20 * math.cos(math.radians(4 * hbp - 63))
    )
    dth = 30 * math.exp(-(((hbp - 275) / 25) ** 2))
    Rc = 2 * math.sqrt(Cbp**7 / (Cbp**7 + 25**7))
    Sl = 1 + 0.015 * (Lbp - 50) ** 2 / math.sqrt(20 + (Lbp - 50) ** 2)
    Sc = 1 + 0.045 * Cbp
    Sh = 1 + 0.015 * Cbp * T
    Rt = -math.sin(math.radians(2 * dth)) * Rc
    return math.sqrt((dLp / Sl) ** 2 + (dCp / Sc) ** 2 + (dHp / Sh) ** 2 + Rt * (dCp / Sc) * (dHp / Sh))


def L(c, lvl):
    return lab(tuple(duty(x, lvl) for x in c))


STANDBY, ERROR = L(RED, 64), L(RED, 255)


def pairs(pal):
    play = {k: L(c, 255) for k, c in pal.items()}
    sleep = {k: L(c, 90) for k, c in pal.items()}
    for a, b in itertools.combinations(pal, 2):
        yield de2000(play[a], play[b]), "%s/%s playing" % (a, b)
        yield de2000(sleep[a], sleep[b]), "%s/%s asleep" % (a, b)
    for k in pal:
        yield de2000(sleep[k], STANDBY), "%s asleep/standby" % k
        yield de2000(play[k], ERROR), "%s/error" % k


def score(pal):
    return min(pairs(pal))


def rgb(h, s):
    r, g, b = colorsys.hsv_to_rgb(h / 360, s, 1)
    return (round(r * 255), round(g * 255), round(b * 255))


def search(seed=1, restarts=12):
    fixed = {"USB": CURRENT["USB"], "Spotify": CURRENT["Spotify"]}
    whites = [(233, 255, b) for b in (80, 105, 130, 160)] + [(255, 240, b) for b in (90, 120, 150)] + [(210, 255, 120)]
    ranges = {"AUX": (0, 60), "Snapcast": (130, 200), "iPod": (280, 358)}
    opts = {k: [v] for k, v in fixed.items()}
    opts["AirPlay"] = whites
    for k, (lo, hi) in ranges.items():
        opts[k] = [rgb(h % 360, 1.0) for h in range(lo, hi + 1, 2)]
    rnd, best = random.Random(seed), None
    for _ in range(restarts):
        pal = {k: rnd.choice(v) for k, v in opts.items()}
        cur = score(pal)[0]
        for _ in range(6):
            imp = False
            for k in opts:
                for c in opts[k]:
                    t = dict(pal)
                    t[k] = c
                    s = score(t)[0]
                    if s > cur + 1e-9:
                        pal, cur, imp = t, s, True
            if not imp:
                break
        if best is None or cur > best[0]:
            best = (cur, pal)
    return best


if __name__ == "__main__":
    print("model:", SOURCE)
    print("current: min %.1f (%s)" % score(CURRENT))
    for d, n in sorted(pairs(CURRENT))[:6]:
        print("   %5.1f %s" % (d, n))
    if "--search" in sys.argv:
        b = max((search(s) for s in (1, 2, 3)), key=lambda r: r[0])
        print("search: min %.1f (%s)\n   %s" % (*score(b[1]), b[1]))
