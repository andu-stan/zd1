#!/usr/bin/env python3
"""The Mac side of module/os/debug/udpprobe.c: a paced stream of numbered UDP packets (AirPlay realtime: ~1.2 KB every
8 ms) and the same summary of what arrived (loss, bursts, arrival gaps, losses per second).

  udpprobe.py send HOST PORT INTERVAL_US SIZE SECONDS [--bind ADDR]
  udpprobe.py recv PORT SECONDS [--bind ADDR]
"""

import argparse
import socket
import struct
import time


def send(host, port, interval, size, seconds, bind=None):
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    if bind:
        s.bind((bind, 0))
    n = int(seconds * 1e6 / interval)
    pad = b"\0" * max(0, size - 12)
    t0 = time.monotonic()
    for i in range(n):
        due = t0 + i * interval / 1e6
        while True:
            d = due - time.monotonic()
            if d <= 0:
                break
            time.sleep(d - 0.001 if d > 0.002 else 0)
        s.sendto(struct.pack(">IQ", i, int(time.monotonic() * 1e6)) + pad, (host, port))
    print("sent %d packets of %d B every %d us" % (n, size, interval))


def recv(port, seconds, bind=None):
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 1 << 20)
    s.bind((bind or "0.0.0.0", port))
    s.settimeout(1)
    seen, arrivals, dup = set(), [], 0
    deadline = time.monotonic() + seconds + 30
    while time.monotonic() < deadline:
        try:
            data = s.recv(4096)
        except TimeoutError:
            if arrivals and time.monotonic() - arrivals[-1][1] > 2:
                break
            continue
        t = time.monotonic()
        seq = struct.unpack(">I", data[:4])[0]
        if not arrivals:
            deadline = t + seconds + 3
        if seq in seen:
            dup += 1
            continue
        seen.add(seq)
        arrivals.append((seq, t))
    if not arrivals:
        print("nothing received")
        return
    first, hi = min(seen), max(seen)
    expected = hi - first + 1
    lost = expected - len(seen)
    ooo = sum(1 for a, b in zip(arrivals, arrivals[1:]) if b[0] < a[0])
    gaps = [b[1] - a[1] for a, b in zip(arrivals, arrivals[1:])]
    bursts, run = [0] * 5, 0
    for i in range(first, hi + 2):
        if i <= hi and i not in seen:
            run += 1
        elif run:
            bursts[0 if run == 1 else 1 if run == 2 else 2 if run <= 5 else 3 if run <= 10 else 4] += 1
            run = 0
    span = arrivals[-1][1] - arrivals[0][1]
    print(
        "received %d of %d (%.2f %% lost), %d duplicates, %d out of order, over %.1f s"
        % (len(seen), expected, 100 * lost / expected, dup, ooo, span)
    )
    print("loss bursts: 1:%d 2:%d 3-5:%d 6-10:%d >10:%d" % tuple(bursts))
    print(
        "arrival gaps: max %.1f ms, >20 ms %d, >50 ms %d, >100 ms %d"
        % (max(gaps) * 1e3, sum(g > 0.02 for g in gaps), sum(g > 0.05 for g in gaps), sum(g > 0.1 for g in gaps))
    )
    per = int(expected / (span + 1e-6)) + 1
    line = ""
    for i in range(first, hi + 1, per):
        n = sum(1 for j in range(i, min(i + per, hi + 1)) if j not in seen)
        line += "." if n == 0 else str(n) if n < 10 else "*"
    print("lost per second: " + line)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("send")
    p.add_argument("host")
    p.add_argument("port", type=int)
    p.add_argument("interval", type=int, metavar="INTERVAL_US")
    p.add_argument("size", type=int)
    p.add_argument("seconds", type=float)
    p.add_argument("--bind", metavar="ADDR")
    p = sub.add_parser("recv")
    p.add_argument("port", type=int)
    p.add_argument("seconds", type=float)
    p.add_argument("--bind", metavar="ADDR")
    a = ap.parse_args()
    if a.cmd == "send":
        send(a.host, a.port, a.interval, a.size, a.seconds, a.bind)
    else:
        recv(a.port, a.seconds, a.bind)


if __name__ == "__main__":
    main()
