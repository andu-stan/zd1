#!/usr/bin/env python3
"""Characterise the module's network paths (Wi-Fi vs the wired lab link) with an AirPlay-like UDP stream, pings per
hop, and the module's own counters before/after each test. The module runs /tmp/udpprobe (module/os/debug/udpprobe.c,
copied over); its console is reached over the host link (`zd1 console run`). Prints one block per test.

  netprobe.py [--seconds N] [--module-wifi IP] [--mac-wifi IP] [--router IP] TEST...
  (the Wi-Fi addresses have no defaults: each test asks for the ones it uses)
  tests: wired (Mac -> module over the dongle), wifi (Mac Wi-Fi -> module Wi-Fi), wifi-up (module -> Mac, Wi-Fi),
         ping-router (module -> router: the 2.4 GHz hop alone), mac-ping-router (Mac -> router: the Mac's own hop),
         ping-module (Mac -> module over Wi-Fi: both hops), counters (the module's snapshot only)
"""

import argparse
import os
import re
import subprocess
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, ".."))
import zd1  # noqa: E402

PROBE = os.path.join(HERE, "udpprobe.py")
MIB = ("mib_trc_stat", "mib_dp_pp_stat", "mib_dp_sec_stat")


def mod(cmd, t):
    """A command on the module's console, to its end (at most `t` s): its output."""
    return zd1.run_on_module(cmd, max(t, 5))


def snapshot():
    """The module's interface, UDP and IRQ counters and the Wi-Fi firmware's raw statistics tables."""
    out = mod(
        "grep -E 'wlan0|eth0' /proc/net/dev; grep '^Udp:' /proc/net/snmp | tail -1; "
        "grep -E ' (9|15):' /proc/interrupts; mount | grep -q debugfs || mount -t debugfs d /sys/kernel/debug; "
        + "; ".join(
            'echo @%s; hexdump -v -e \'32/1 "%%02x" "\\n"\' /sys/kernel/debug/sulu-wlan/%s' % (f, f) for f in MIB
        ),
        8,
    )
    snap, cur = {}, None
    for line in out.splitlines():
        line = line.strip()
        m = re.match(r"(wlan0|eth0):\s*(.*)", line)
        if m:
            v = list(map(int, m.group(2).split()))
            snap[m.group(1)] = {"rx_pkts": v[1], "rx_drop": v[3], "rx_err": v[2], "tx_pkts": v[9], "tx_drop": v[11]}
        elif line.startswith("Udp:") and line[5:6].isdigit():
            v = list(map(int, line.split()[1:]))
            snap["udp"] = {"in": v[0], "noports": v[1], "inerr": v[2], "rcvbuferr": v[4]}
        elif re.match(r"(9|15):", line):
            snap["irq" + line.split(":")[0]] = int(line.split()[1])
        elif line.startswith("@"):
            cur = line[1:]
            snap[cur] = b""
        elif cur and re.fullmatch(r"([0-9a-f]{2})+", line):
            snap[cur] += bytes.fromhex(line)
    return snap


def delta(a, b):
    """Counters that moved. The firmware tables by byte offset: its counters are 32-bit little-endian but not all
    word aligned (this firmware build's layout; re/netmodule/wlan_mib_schema.txt is the stock one's), so a counter is
    read wherever a changed byte starts a small increase."""
    out = []
    for k in ("wlan0", "eth0", "udp"):
        if k in a and k in b:
            d = {f: b[k][f] - a[k][f] for f in a[k] if b[k][f] != a[k][f]}
            if d:
                out.append("%s %s" % (k, " ".join("%s+%d" % kv for kv in d.items())))
    irq = " ".join("%s+%d" % (k, b[k] - a[k]) for k in ("irq9", "irq15") if k in a and k in b)
    if irq:
        out.append(irq)
    for f in MIB:
        if f in a and f in b:
            x, y, moved, o = a[f], b[f], [], 0
            while o + 4 <= min(len(x), len(y)):
                d = (int.from_bytes(y[o : o + 4], "little") - int.from_bytes(x[o : o + 4], "little")) & 0xFFFFFFFF
                if x[o] != y[o] and d < 1 << 24:
                    moved.append("@%d+%d" % (o, d))
                    o += 4
                else:
                    o += 1
            if moved:
                out.append("%s %s" % (f, " ".join(moved)))
    return "\n".join("  " + l for l in out)


def udp_to_module(dest, bind, seconds, interval=8000, size=1200):
    def sender():
        time.sleep(3)
        subprocess.run(
            [sys.executable, "-I", PROBE, "send", dest, "9100", str(interval), str(size), str(seconds)]
            + (["--bind", bind] if bind else []),
            capture_output=True,
        )

    th = threading.Thread(target=sender)
    th.start()
    out = mod("/tmp/udpprobe recv 9100 %d" % seconds, seconds + 10)
    th.join()
    return "\n".join(l for l in out.splitlines() if re.match(r"(received|loss|arrival|lost|nothing)", l))


def udp_from_module(mac_ip, seconds, interval=8000, size=1200):
    res = {}

    def receiver():
        res["out"] = subprocess.run(
            [sys.executable, "-I", PROBE, "recv", "9101", str(seconds)], capture_output=True, text=True
        ).stdout

    th = threading.Thread(target=receiver)
    th.start()
    time.sleep(1)
    mod("/tmp/udpprobe send %s 9101 %d %d %d" % (mac_ip, interval, size, seconds), seconds + 5)
    th.join()
    return res["out"].strip()


def ping_summary(out):
    return "\n".join(l for l in out.splitlines() if re.search(r"packet loss|round-trip|rtt", l))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("tests", nargs="+")
    ap.add_argument("--seconds", type=int, default=60)
    ap.add_argument("--module-wifi", help="the module's Wi-Fi address")
    ap.add_argument("--module-wired", default="169.254.1.1")
    ap.add_argument("--mac-wifi", help="this Mac's Wi-Fi address")
    ap.add_argument("--router", help="the Wi-Fi router's address")
    ap.add_argument("--interval", type=int, default=8000, help="UDP tests: µs between packets")
    ap.add_argument("--size", type=int, default=1200, help="UDP tests: bytes per packet")
    a = ap.parse_args()
    needs = {
        "wifi": ("module_wifi", "mac_wifi"),
        "wifi-up": ("mac_wifi",),
        "ping-router": ("router",),
        "mac-ping-router": ("router",),
        "ping-module": ("module_wifi",),
    }
    for t in a.tests:
        for n in needs.get(t, ()):
            if getattr(a, n) is None:
                ap.error("%s needs --%s" % (t, n.replace("_", "-")))
    for t in a.tests:
        print("== %s (%s)" % (t, time.strftime("%H:%M:%S")), flush=True)
        before = snapshot()
        if t == "wired":
            print(udp_to_module(a.module_wired, None, a.seconds, a.interval, a.size))
        elif t == "wifi":
            print(udp_to_module(a.module_wifi, a.mac_wifi, a.seconds, a.interval, a.size))
        elif t == "wifi-up":
            print(udp_from_module(a.mac_wifi, a.seconds, a.interval, a.size))
        elif t == "ping-router":
            n = a.seconds * 20
            print(ping_summary(mod("ping -q -c %d -i 0.05 -s 1172 %s" % (n, a.router), a.seconds + 15)))
        elif t == "mac-ping-router":
            print(
                ping_summary(
                    subprocess.run(
                        ["ping", "-q", "-c", str(a.seconds * 10), "-i", "0.1", "-s", "1172", a.router],
                        capture_output=True,
                        text=True,
                    ).stdout
                )
            )
        elif t == "ping-module":
            print(
                ping_summary(
                    subprocess.run(
                        ["ping", "-q", "-c", str(a.seconds * 10), "-i", "0.1", "-s", "1172", a.module_wifi],
                        capture_output=True,
                        text=True,
                    ).stdout
                )
            )
        elif t != "counters":
            sys.exit("unknown test " + t)
        print(delta(before, snapshot()), flush=True)


if __name__ == "__main__":
    main()
