#!/usr/bin/env python3
"""Minimal AirPlay 1 (RAOP) sender for testing an AirTunes receiver (the stock module app, emulated or real).
Uncompressed L16/44100/2 over RTP/UDP, no encryption, no Apple-Challenge (the receiver doesn't require one).
Answers the receiver's timing requests and sends sync packets, like iTunes did.

    raop_send.py HOST WAV [PORT=1024] [VOLUME_DB=-20]       WAV: 44.1 kHz, 16-bit, stereo

RAOP_DACP=1: also send DACP-ID / Active-Remote on every request and DMAP track metadata, like iTunes / iOS do.
RAOP_CODEC=alac: announce AppleLossless like iTunes and send uncompressed ALAC frames instead of L16.
RAOP_LATENCY=<frames>: sync packets say rtptime - <frames> is playing now (iTunes: 77175; default 0 = the packet
being sent plays now).
RAOP_DACP_MDNS=<port>: also be the DACP remote (implies RAOP_DACP): an HTTP server for /ctrl-int/1/<command>
(printed as "[raop] DACP <command> (Active-Remote ...)") and an mDNS responder on UDP 127.0.0.1:<port> for its
iTunes_Ctrl_<DACP-ID>._dacp._tcp.local SRV (an emulator run's mDNS queries can be sent there).
RAOP_VCLOCK=1: the emulator's virtual time instead of the host's: stdin gets "<virtual seconds>" lines (from whatever
runs the emulation in slices); packets go out as that clock reaches them and the NTP times in timing replies and sync
packets are on it, so the stream is paced like a real sender's however fast the emulation runs.
"""

import argparse
import os
import random
import socket
import struct
import sys
import threading
import time
import wave

FRAMES = 352  # samples per packet (AirPort convention)
NTP_EPOCH = 0x83AA7E80


class Clock:
    """The host's time, or (virtual) the emulator's from stdin."""

    def __init__(self, virtual):
        self.virtual, self.vnow, self.cond = virtual, None, threading.Condition()
        if virtual:
            threading.Thread(target=self.reader, daemon=True).start()
            with self.cond:
                self.cond.wait_for(lambda: self.vnow is not None)

    def reader(self):
        for line in sys.stdin:
            with self.cond:
                self.vnow = float(line)
                self.cond.notify_all()
        with self.cond:
            self.vnow = float("inf")
            self.cond.notify_all()  # the rig is gone

    def now(self):
        return self.vnow if self.virtual else time.time()

    def wait_until(self, t):
        """Sleep until now() >= t."""
        if self.virtual:
            with self.cond:
                self.cond.wait_for(lambda: self.vnow >= t)
        else:
            d = t - time.time()
            if d > 0:
                time.sleep(d)

    def ntp(self):
        t = self.now() + (1_700_000_000 if self.virtual else 0)
        return int(t) + NTP_EPOCH, int((t % 1) * (1 << 32))


def timing_responder(timing, clock):
    while True:
        try:
            d, a = timing.recvfrom(64)
        except OSError:
            return
        if len(d) >= 32 and d[1] & 0x7F == 0x52:  # timing request -> reply 0xd3 with ref/recv/send times
            sec, frac = clock.ntp()
            r = bytearray(32)
            r[0] = 0x80
            r[1] = 0xD3
            r[2:4] = d[2:4]
            r[8:16] = d[24:32]
            struct.pack_into(">IIII", r, 16, sec, frac, sec, frac)
            timing.sendto(bytes(r), a)
            rs, rf = struct.unpack_from(">II", d, 24)
            print(
                "[raop] timing request answered (receiver %.6f, ours %.6f)" % (rs + rf / 2**32, sec + frac / 2**32),
                flush=True,
            )


def dacp_remote(dacp_id, mdns_port):
    """The sender's side of DACP: commands over HTTP, the SRV record that says where."""
    import http.server

    class H(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            print(
                f"[raop] DACP {self.path.rsplit('/', 1)[-1]} (Active-Remote {self.headers.get('Active-Remote')})",
                flush=True,
            )
            self.send_response(204)
            self.end_headers()

        def log_message(self, *a):
            pass

    httpd = http.server.HTTPServer(("127.0.0.1", 0), H)
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    name = f"iTunes_Ctrl_{dacp_id}._dacp._tcp.local"
    enc = lambda n: b"".join(bytes([len(x)]) + x.encode() for x in n.split(".")) + b"\0"  # noqa: E731
    rr = lambda n, t, d: enc(n) + struct.pack(">HHIH", t, 0x8001, 120, len(d)) + d  # noqa: E731
    answer = (
        struct.pack(">6H", 0, 0x8400, 0, 1, 0, 1)
        + rr(name, 33, struct.pack(">HHH", 0, 0, httpd.server_port) + enc("raop-send.local"))
        # the emulator's gateway: TCP to it reaches the host's loopback, where the DACP server listens (emu/README.md)
        + rr("raop-send.local", 1, socket.inet_aton("192.168.50.1"))
    )
    u = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    u.bind(("127.0.0.1", mdns_port))

    def respond():
        while True:
            q, peer = u.recvfrom(1500)
            if enc(name).lower() in q.lower() and not q[2] & 0x80:
                u.sendto(q[:2] + answer[2:], peer)
                print("[raop] DACP mDNS query answered", flush=True)

    threading.Thread(target=respond, daemon=True).start()
    print(f"[raop] DACP remote {dacp_id} on port {httpd.server_port}", flush=True)


def alac_frame(pcm_le):
    """Uncompressed ALAC frame: element CPE(1) 3b, instance 4b, unused 12b, hassize 1b, unusedbytes 2b,
    isNotCompressed=1 1b, then 16-bit big-endian L,R per frame, then ID_END (7) 3b, byte-padded."""
    acc = 0
    n = 0
    out = bytearray()

    def put(v, w):
        nonlocal acc, n
        acc = (acc << w) | (v & ((1 << w) - 1))
        n += w
        while n >= 8:
            n -= 8
            out.append((acc >> n) & 0xFF)
        acc &= (1 << n) - 1

    put(1, 3)
    put(0, 4)
    put(0, 12)
    put(0, 1)
    put(0, 2)
    put(1, 1)
    for i in range(0, len(pcm_le), 2):
        put(pcm_le[i] | pcm_le[i + 1] << 8, 16)
    put(7, 3)
    if n:
        put(0, 8 - n)
    return bytes(out)


def dmap(tag, val):
    b = val.encode()
    return tag.encode() + struct.pack(">I", len(b)) + b


class Rtsp:
    def __init__(self, host, port, dacp_headers):
        self.s = socket.create_connection((host, port), timeout=30)
        self.cseq, self.session, self.dacp = 0, None, dacp_headers

    def __call__(self, method, uri, headers=None, body=b""):
        self.cseq += 1
        h = {
            "CSeq": str(self.cseq),
            "User-Agent": "iTunes/4.6 (Macintosh; U; PPC Mac OS X 10.3)",
            "Client-Instance": "56B29BB6CB904862",
        }
        if self.session:
            h["Session"] = self.session
        h.update(self.dacp)
        h.update(headers or {})
        if body:
            h["Content-Length"] = str(len(body))
        req = f"{method} {uri} RTSP/1.0\r\n" + "".join(f"{k}: {v}\r\n" for k, v in h.items()) + "\r\n"
        self.s.sendall(req.encode() + body)
        data = b""
        while b"\r\n\r\n" not in data:
            data += self.s.recv(4096)
        head, rest = data.split(b"\r\n\r\n", 1)
        lines = head.decode(errors="replace").split("\r\n")
        hdrs = {l.split(":", 1)[0].lower(): l.split(":", 1)[1].strip() for l in lines[1:] if ":" in l}
        n = int(hdrs.get("content-length", 0))
        while len(rest) < n:
            rest += self.s.recv(4096)
        print(f"[raop] {method} -> {lines[0]}", flush=True)
        return lines[0], hdrs, rest


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("host")
    ap.add_argument("wav")
    ap.add_argument("port", type=int, nargs="?", default=1024)
    ap.add_argument("vol", type=float, nargs="?", default=-20.0, metavar="volume_db")
    a = ap.parse_args()
    host = a.host
    dacp_mdns = int(os.environ.get("RAOP_DACP_MDNS", "0"))
    dacp = os.environ.get("RAOP_DACP") == "1" or dacp_mdns > 0
    dacp_id = "%016X" % random.getrandbits(64)
    alac = os.environ.get("RAOP_CODEC") == "alac"
    latency = int(os.environ.get("RAOP_LATENCY", "0"))
    clock = Clock(os.environ.get("RAOP_VCLOCK") == "1")

    ctrl = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    ctrl.bind(("", 0))
    timing = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    timing.bind(("", 0))
    audio = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    my_ip = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    my_ip.connect((host, 9))
    local = my_ip.getsockname()[0]
    my_ip.close()
    threading.Thread(target=timing_responder, args=(timing, clock), daemon=True).start()

    rtsp = Rtsp(host, a.port, {"DACP-ID": dacp_id, "Active-Remote": str(random.randint(1, 1 << 31))} if dacp else {})
    sid = str(random.randint(1 << 30, 1 << 31))
    if dacp_mdns:
        dacp_remote(dacp_id, dacp_mdns)

    uri = f"rtsp://{local}/{sid}"
    rtsp("OPTIONS", "*")
    sdp = (
        f"v=0\r\no=iTunes {sid} 0 IN IP4 {local}\r\ns=iTunes\r\nc=IN IP4 {host}\r\nt=0 0\r\n"
        f"m=audio 0 RTP/AVP 96\r\n"
        + (
            "a=rtpmap:96 AppleLossless\r\na=fmtp:96 352 0 16 40 10 14 2 255 0 0 44100\r\n"
            if alac
            else "a=rtpmap:96 L16/44100/2\r\n"
        )
    ).encode()
    rtsp("ANNOUNCE", uri, {"Content-Type": "application/sdp"}, sdp)
    _, h, _ = rtsp(
        "SETUP",
        uri,
        {
            "Transport": "RTP/AVP/UDP;unicast;interleaved=0-1;mode=record;"
            f"control_port={ctrl.getsockname()[1]};timing_port={timing.getsockname()[1]}"
        },
    )
    rtsp.session = h.get("session", "1")
    tp = dict(kv.split("=", 1) for kv in h["transport"].split(";") if "=" in kv)
    server_port, control_port = int(tp["server_port"]), int(tp.get("control_port", 0))
    print(
        f"[raop] server_port {server_port} control_port {control_port} jack: {h.get('audio-jack-status')}", flush=True
    )
    seq = random.randint(0, 0xFFFF)
    rtptime = random.randint(0, 1 << 31)
    ssrc = random.randint(0, 1 << 31)
    rtsp("RECORD", uri, {"Range": "npt=0-", "RTP-Info": f"seq={seq};rtptime={rtptime}"})
    rtsp("SET_PARAMETER", uri, {"Content-Type": "text/parameters"}, f"volume: {a.vol:.6f}\r\n".encode())
    if dacp:
        item = dmap("minm", "Test tone") + dmap("asar", "raop_send") + dmap("asal", "zd1 emu")
        rtsp(
            "SET_PARAMETER",
            uri,
            {"Content-Type": "application/x-dmap-tagged", "RTP-Info": f"rtptime={rtptime}"},
            b"mlit" + struct.pack(">I", len(item)) + item,
        )
        rtsp(
            "SET_PARAMETER",
            uri,
            {"Content-Type": "text/parameters"},
            f"progress: {rtptime}/{rtptime}/{rtptime + 8 * 44100}\r\n".encode(),
        )

    w = wave.open(a.wav, "rb")
    assert w.getframerate() == 44100 and w.getsampwidth() == 2 and w.getnchannels() == 2
    t0 = clock.now()
    sent = 0
    first = True
    while True:
        pcm = w.readframes(FRAMES)
        if not pcm:
            break
        if alac:
            be = alac_frame(pcm)
        else:
            be = bytearray(len(pcm))
            be[0::2] = pcm[1::2]
            be[1::2] = pcm[0::2]  # L16 is big-endian
        if sent % 125 == 0 and control_port:  # sync packet ~ every 1 s
            sec, frac = clock.ntp()
            sync = struct.pack(
                ">BBHIIII",
                0x90 if first else 0x80,
                0xD4,
                7,
                (rtptime - latency) & 0xFFFFFFFF,
                sec,
                frac,
                rtptime & 0xFFFFFFFF,
            )
            # from the control port announced in SETUP (the receiver connects to it)
            ctrl.sendto(sync, (host, control_port))
        hdr = struct.pack(">BBHII", 0x80, 0xE0 if first else 0x60, seq & 0xFFFF, rtptime & 0xFFFFFFFF, ssrc)
        audio.sendto(hdr + bytes(be), (host, server_port))
        first = False
        seq += 1
        rtptime += FRAMES
        sent += 1
        clock.wait_until(t0 + sent * FRAMES / 44100)
    print(f"[raop] sent {sent} packets ({sent * FRAMES / 44100:.1f} s)", flush=True)
    clock.wait_until(clock.now() + 2)
    rtsp("TEARDOWN", uri)


if __name__ == "__main__":
    main()
