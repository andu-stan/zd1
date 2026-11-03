#!/usr/bin/env python3
"""The Warp0 firmware images (build.py: emu/img/warp0/*.elf) run under Unicorn on the host, their job
loop fed through the ring as the ARM feeds it. For what the hardware shows only as a hang: Warp0 has no exceptions,
an unaligned store silently drops the low address bits, and an access outside its memory hangs the bus (and can
take the module down). Here only what Warp0 really has is mapped -- local RAM 0-0xBFFF and the window
0x61F00000-0x61FBFFFF -- so any other access, and any unaligned word or halfword access, stops the run with the PC,
the function it is in and the last blocks executed. MIPS32 semantics: not for the MIPS I hazards themselves
(build.py's passes cover those).

  python3 -I module/warp0/tools/emu.py checks          every image's bad-pointer jobs refused
  ... emu.py vorbis FILE.pkts [N]   the Vorbis image: VORBIS_INIT + N packets (vorbischeck packets), the PCM
                                    against FILE.pkts.ours (the host build: vorbischeck dump FILE.ogg FILE.pkts.ours)
  ... emu.py aac FILE.frames [N] [CH] [RATE]   the AAC image against FILE.frames.ours (host build)
  ... emu.py flac FILE.chunks REF.raw [N]      the FLAC image: FLAC_INIT + N chunks (flaccheck chunks), the samples
                                    against REF.raw (`flac -d --force-raw-format --endian=little --sign=signed`)
  ... emu.py alac FILE.alac [N] [plain|aes|chacha]   the crypto image: N frames of an ALAC vector (vectors/,
                                    ALACGV01, 16- or 24-bit) as RTP_AUDIO jobs, the samples against its PCM: plain
                                    (buffered AirPlay's), encrypted here as AirPlay 1 (AES-128-CBC) or AirPlay 2
                                    realtime (ChaCha20-Poly1305, a tampered tag refused first) sends them
"""

import array
import os
import struct
import sys
from collections import deque

from unicorn import (
    UC_ARCH_MIPS,
    UC_HOOK_BLOCK,
    UC_HOOK_MEM_INVALID,
    UC_HOOK_MEM_READ,
    UC_HOOK_MEM_WRITE,
    UC_MODE_LITTLE_ENDIAN,
    UC_MODE_MIPS32,
    Uc,
    UcError,
)
from unicorn.mips_const import UC_MIPS_REG_PC, UC_MIPS_REG_RA, UC_MIPS_REG_SP

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", "..", ".."))
BUILD = os.path.join(REPO, "emu", "img", "warp0")
sys.path.insert(0, os.path.join(HERE, ".."))
import build  # noqa: E402  (module/warp0/build.py: w0abi.h's numbers)

A = build.abi()
RING, CTRL, DATA = A["W0_RING"], A["W0_CTRL"], A["W0_DATA"]


class Warp:
    def __init__(self, elf):
        self.syms = sorted((int(a, 16), n) for a, n in (l.split() for l in open(elf[:-4] + ".sym")))
        d = open(elf, "rb").read()
        mu = self.mu = Uc(UC_ARCH_MIPS, UC_MODE_MIPS32 + UC_MODE_LITTLE_ENDIAN)
        mu.mem_map(0x0, 0xC000)  # local RAM: 0xC000-0xFFFF hangs on Warp0, so unmapped here
        mu.mem_map(A["W0_BASE"], A["W0_SIZE"])  # the window
        (phoff,) = struct.unpack_from("<I", d, 0x1C)
        phentsize, phnum = struct.unpack_from("<HH", d, 0x2A)
        for i in range(phnum):
            t, off, vaddr, _, filesz, _ = struct.unpack_from("<IIIIII", d, phoff + i * phentsize)
            if t == 1 and filesz:
                mu.mem_write(vaddr, d[off : off + filesz])
        self.main = dict((n, a) for a, n in self.syms)["main"]
        self.blocks = deque(maxlen=24)
        self.fault = None
        mu.hook_add(UC_HOOK_BLOCK, lambda uc, a, s, u: self.blocks.append(a))
        mu.hook_add(UC_HOOK_MEM_INVALID, self._invalid)
        mu.hook_add(UC_HOOK_MEM_WRITE, self._unaligned_w)
        mu.hook_add(UC_HOOK_MEM_READ, self._unaligned_r)
        mu.hook_add(UC_HOOK_MEM_WRITE, self._done, begin=RING, end=RING + A["W0_RING_N"] * 64 - 1)

    def name(self, a):
        k = max(((v, n) for v, n in self.syms if v <= a), default=(0, "?"))
        return "%s+0x%x" % (k[1], a - k[0])

    def _done(self, uc, access, addr, size, value, user):
        if value == A["W0_DONE"]:
            self.done = True
            uc.emu_stop()

    def _invalid(self, uc, access, addr, size, value, user):
        self.fault = "an access Warp0 would hang on: %d bytes at 0x%08x" % (size, addr)
        return False

    def _unaligned_w(self, uc, access, addr, size, value, user):
        if size > 1 and addr % size:
            self.fault = "an unaligned %d-byte store at 0x%08x (Warp0 drops the low bits)" % (size, addr)
            uc.emu_stop()

    def _unaligned_r(self, uc, access, addr, size, value, user):
        if size > 1 and addr % size:
            self.fault = "an unaligned %d-byte load at 0x%08x (Warp0 returns mixed bytes)" % (size, addr)
            uc.emu_stop()

    def put(self, addr, data):
        self.mu.mem_write(addr, bytes(data))

    def get(self, addr, n):
        return bytes(self.mu.mem_read(addr, n))

    def job(self, **f):
        """one job in ring slot 0, run from main (whose job loop starts at slot 0) until DONE; returns the
        descriptor words. The images keep their state in .bss, which main doesn't clear, so a stream's state
        carries over from job to job as on the hardware."""
        names = ("state op len src dst key nonce aad aadlen tag ctr result _ conf outlen _").split()
        desc = [0] * 16
        for k, v in f.items():
            desc[names.index(k)] = v & 0xFFFFFFFF
        at = RING
        self.put(at, struct.pack("<16I", *desc))
        self.put(at, struct.pack("<I", A["W0_SUBMIT"]))
        self.fault, self.done = None, False
        try:  # main, not _start: no mtc3 (COP3 is not MIPS32), .bss is zero
            self.mu.reg_write(UC_MIPS_REG_SP, A["W0_STACK_TOP"])  # as start.S sets it
            self.mu.emu_start(self.main, 0xFFFFFFF0, count=2_000_000_000)
        except UcError as e:  # (stopping from the write hook can surface as an error too)
            if not self.done:
                self.fault = self.fault or str(e)
        w = struct.unpack("<16I", self.get(at, 64))
        if self.fault or w[0] != A["W0_DONE"]:
            pc = self.mu.reg_read(UC_MIPS_REG_PC)
            print(
                "STOPPED: %s at pc %08x (%s), ra %s; C_STAGE %#x"
                % (
                    self.fault or "no DONE",
                    pc,
                    self.name(pc),
                    self.name(self.mu.reg_read(UC_MIPS_REG_RA)),
                    struct.unpack("<I", self.get(CTRL + 4 * A["C_STAGE"], 4))[0],
                )
            )
            print("  last blocks: " + " ".join(self.name(a) for a in self.blocks))
            raise SystemExit(1)
        return w


def checks():
    """Every op of every image with each pointer it takes moved out of the buffer area in each way: refused."""
    bad_total = 0
    images = {
        "w0fw": [(A["W0_OP_RTP_AUDIO"], "src dst key nonce aad tag conf")],
        "w0aac": [(A["W0_OP_AAC_DECODE"], "src dst")],
        "w0vorbis": [(A["W0_OP_VORBIS_INIT"], "src aad"), (A["W0_OP_VORBIS_DECODE"], "src dst")],
        "w0flac": [(A["W0_OP_FLAC_INIT"], "src"), (A["W0_OP_FLAC_DECODE"], "src dst")],
    }
    for img, ops in images.items():
        w = Warp(os.path.join(BUILD, img + ".elf"))
        w.job(op=A["W0_OP_NOP"])
        end = struct.unpack("<I", w.get(CTRL + 4 * A["C_DATA_END"], 4))[0]
        if img == "w0aac":
            w.job(op=A["W0_OP_AAC_INIT"], len=44100, ctr=2)
        if img == "w0flac":  # a STREAMINFO (44.1 kHz stereo 16-bit) so DECODE checks its pointers
            w.put(DATA + 0xC000, bytes([0, 16, 0x12, 0, 0, 0, 0, 0, 0, 0, 0x0A, 0xC4, 0x42, 0xF0]) + bytes(20))
            w.job(op=A["W0_OP_FLAC_INIT"], src=DATA + 0xC000, len=34)
        n = bad = 0
        for op, fields in ops:
            for field in fields.split():
                for how, val, length in (
                    ("below the buffers", A["W0_BASE"], 64),
                    ("across their end", end - 8, 64),
                    ("unaligned", DATA + 0x1002, 64),
                    ("wrapping", end - 64, 0xFFFFFFF0),
                    ("null", 0, 64),
                    ("local RAM", 0x100, 64),
                ):
                    f = dict(
                        op=op,
                        len=34 if op == A["W0_OP_FLAC_INIT"] else 64,
                        src=DATA,
                        dst=DATA + 0x4000,
                        key=DATA + 0x8000,
                        nonce=DATA + 0x8100,
                        aad=DATA + 0x8200,
                        aadlen=64,
                        tag=DATA + 0x8300,
                        conf=DATA + 0x8400,
                        ctr=0x12 if op == A["W0_OP_RTP_AUDIO"] else 0,
                    )
                    f[field] = val
                    if how == "wrapping":
                        if field in ("src", "dst"):
                            f["len"] = length
                        elif field == "aad":
                            f["aadlen"] = length
                        else:
                            continue
                    r = w.job(**f)[11]
                    n += 1
                    if r not in (A["W0_ERANGE"], A["W0_EALIGN"], A["W0_ELEN"]):
                        bad += 1
                        print("FAIL %s op %d %s %s: result %d" % (img, op, field, how, r))
        print("%s: %d bad-pointer jobs, %d not refused" % (img, n, bad))
        bad_total += bad
    return bad_total != 0


def vorbis(path, count):
    w = Warp(os.path.join(BUILD, "w0vorbis.elf"))
    raw = open(path, "rb").read()
    pk, i = [], 0
    while i + 4 <= len(raw):
        (n,) = struct.unpack_from("<I", raw, i)
        pk.append(raw[i + 4 : i + 4 + n])
        i += 4 + n
    ident, setup = DATA, DATA + 0x100
    w.put(ident, pk[0])
    w.put(setup, pk[2])
    r = w.job(op=A["W0_OP_VORBIS_INIT"], src=ident, len=len(pk[0]), aad=setup, aadlen=len(pk[2]))
    print(
        "VORBIS_INIT -> %d (decoder %d), arena %d bytes" % (r[11], r[10] - (1 << 32) if r[10] >> 31 else r[10], r[14])
    )
    if r[11]:
        return 1
    pcm = bytearray()
    src, dst = DATA + 0x8000, DATA + 0x10000
    for _k, p in enumerate(pk[3 : 3 + count]):
        w.put(src, p + b"\0" * 4)
        r = w.job(op=A["W0_OP_VORBIS_DECODE"], src=src, len=len(p), dst=dst)
        if r[11] == 0:
            pcm += w.get(dst, r[14])
    a = array.array("h", pcm)
    ref = path + ".ours"
    if not os.path.exists(ref):
        print("%d samples decoded (no %s to compare with)" % (len(a), ref))
        return 0
    b = array.array("h", open(ref, "rb").read()[: len(pcm)])
    bad = [i for i in range(min(len(a), len(b))) if a[i] != b[i]]
    print("%d samples, %d differ from the host build%s" % (len(a), len(bad), (": first at %d" % bad[0]) if bad else ""))
    return 1 if bad else 0


def aac(path, count, ch, rate):
    w = Warp(os.path.join(BUILD, "w0aac.elf"))
    print("AAC_INIT ->", w.job(op=A["W0_OP_AAC_INIT"], len=rate, ctr=ch)[11])
    raw = open(path, "rb").read()
    frames, i = [], 0
    while i + 4 <= len(raw):
        (n,) = struct.unpack_from("<I", raw, i)
        frames.append(raw[i + 4 : i + 4 + n])
        i += 4 + n
    pcm = bytearray()
    src, dst = DATA, DATA + 0x10000
    for f in frames[:count]:
        w.put(src, f + b"\0" * 4)
        r = w.job(op=A["W0_OP_AAC_DECODE"], len=len(f), src=src, dst=dst)
        pcm += w.get(dst, r[14]) if r[11] == 0 else b"\0" * (2048 * ch)
    a = array.array("h", pcm)
    b = array.array("h", open(path + ".ours", "rb").read()[: len(pcm)])
    bad = [i for i in range(min(len(a), len(b))) if a[i] != b[i]]
    print(
        "%d samples, %d differ from the host build%s"
        % (len(a), len(bad), (": first at %d (frame %d)" % (bad[0], bad[0] // (1024 * ch))) if bad else "")
    )
    return 1 if bad else 0


def flac(path, ref, count):
    w = Warp(os.path.join(BUILD, "w0flac.elf"))
    raw = open(path, "rb").read()
    ck, i = [], 0
    while i + 4 <= len(raw):
        (n,) = struct.unpack_from("<I", raw, i)
        ck.append(raw[i + 4 : i + 4 + n])
        i += 4 + n
    si = ck[0]
    bits = ((si[12] & 1) << 4 | si[13] >> 4) + 1
    w.put(DATA, si + b"\0\0")
    r = w.job(op=A["W0_OP_FLAC_INIT"], src=DATA, len=34)
    print("FLAC_INIT -> %d (decoder %d), %d bits" % (r[11], r[10] - (1 << 32) if r[10] >> 31 else r[10], bits))
    if r[11]:
        return 1
    out = bytearray()
    src, dst = DATA + 0x1000, DATA + 0x10000
    for c in ck[1 : 1 + count]:
        w.put(src, c + b"\0" * 4)
        r = w.job(op=A["W0_OP_FLAC_DECODE"], src=src, len=len(c), dst=dst)
        if r[11] or r[9] != len(c):
            print(
                "chunk: result %d (decoder %d), %d of %d bytes taken"
                % (r[11], r[10] - (1 << 32) if r[10] >> 31 else r[10], r[9], len(c))
            )
            return 1
        out += w.get(dst, r[14])
    a = array.array("i", out)
    width = (bits + 7) // 8
    ours = b"".join(struct.pack("<i", v >> (32 - 8 * width))[:width] for v in a)
    want = open(ref, "rb").read()[: len(ours)]
    bad = next((k for k in range(min(len(ours), len(want))) if ours[k] != want[k]), None)
    print(
        "%d samples from %d chunks: %s"
        % (
            len(a),
            min(count, len(ck) - 1),
            "identical to libFLAC" if bad is None and len(ours) == len(want) else "DIFFER at byte %s" % bad,
        )
    )
    return 0 if bad is None and len(ours) == len(want) else 1


def encrypt(cipher, frame, k):
    """frame k as a sender would send it: (payload, key, nonce, aad, tag, D_CTR's cipher bits). AirPlay 1:
    AES-128-CBC over the whole blocks, the tail in the clear; AirPlay 2 realtime: ChaCha20-Poly1305 with the RTP
    header's bytes as aad."""
    if cipher == "plain":
        return frame, b"", b"", b"", b"", A["W0_RTP_PLAIN"]
    from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
    from cryptography.hazmat.primitives.ciphers.aead import ChaCha20Poly1305

    key = bytes((k * 7 + i) & 0xFF for i in range(32 if cipher == "chacha" else 16))
    nonce = struct.pack("<I", k) + bytes(8 if cipher == "chacha" else 12)
    if cipher == "aes":
        whole = len(frame) & ~15
        enc = Cipher(algorithms.AES(key), modes.CBC(nonce)).encryptor()
        return enc.update(frame[:whole]) + enc.finalize() + frame[whole:], key, nonce, b"", b"", A["W0_RTP_AES_CBC"]
    aad = struct.pack(">II", 0x80600000 | k & 0xFFFF, k * 352)
    sealed = ChaCha20Poly1305(key).encrypt(nonce, frame, aad)
    return sealed[:-16], key, nonce, aad, sealed[-16:], A["W0_RTP_CHACHA_POLY"]


def alac(path, count, cipher):
    w = Warp(os.path.join(BUILD, "w0fw.elf"))
    d = open(path, "rb").read()
    assert d[:8] == b"ALACGV01"
    ch, depth = d[12], d[13]
    (clen,) = struct.unpack_from("<I", d, 16)
    ck = d[20 : 20 + clen]
    p = 20 + clen
    conf = struct.pack("<6I", struct.unpack_from(">I", ck, 24)[0], ck[29], ck[30], ck[31], ck[32], ck[33])
    (n,) = struct.unpack_from("<I", d, p)
    p += 4
    frames = []
    for _ in range(n):
        (fl,) = struct.unpack_from("<I", d, p)
        frames.append(d[p + 4 : p + 4 + fl])
        p += 4 + fl
    pcm = d[p + 4 :]
    cf, src, dst, key, nonce, aad, tag = (DATA + o for o in (0, 0x100, 0x1000, 0x2000, 0x2100, 0x2200, 0x2300))
    w.put(cf, conf)
    got = []
    for k, f in enumerate(frames[:count]):
        payload, kb, nb, ab, tb, mode = encrypt(cipher, f, k)
        for at, b in ((src, payload + b"\0" * 4), (key, kb), (nonce, nb), (aad, ab), (tag, tb)):
            w.put(at, b)
        job = dict(op=A["W0_OP_RTP_AUDIO"], src=src, len=len(payload), dst=dst, conf=cf, key=key, nonce=nonce)
        job.update(aad=aad, aadlen=len(ab), tag=tag, ctr=mode | A["W0_RTP_ALAC"])
        if cipher == "chacha" and k == 0:  # a tampered tag first: refused, nothing written
            w.put(tag, bytes([tb[0] ^ 1]) + tb[1:])
            r = w.job(**job)
            if r[11] != A["W0_EAUTH"] or r[14]:
                print("a tampered tag: result %d, %d bytes out" % (r[11], r[14]))
                return 1
            w.put(tag, tb)
        r = w.job(**job)
        if r[11]:
            print("frame %d: result %d" % (len(got), r[11]))
            return 1
        out = w.get(dst, r[14])
        got += array.array("i" if depth == 24 else "h", out)
    wd = depth // 8
    want = [int.from_bytes(pcm[k : k + wd], "little", signed=True) for k in range(0, len(got) * wd, wd)]
    bad = next((k for k in range(len(got)) if got[k] != want[k]), None)
    print(
        "%s, %s: %d-bit, %d channels, %d frames, %d samples: %s"
        % (
            os.path.basename(path),
            cipher,
            depth,
            ch,
            min(count, n),
            len(got),
            "identical to the PCM" if bad is None else "DIFFER at sample %d (%d, want %d)" % (bad, got[bad], want[bad]),
        )
    )
    return 0 if bad is None else 1


def main():
    cmd = sys.argv[1] if len(sys.argv) > 1 else ""
    if cmd == "checks":
        return checks()
    if cmd == "vorbis":
        return vorbis(sys.argv[2], int(sys.argv[3]) if len(sys.argv) > 3 else 100)
    if cmd == "flac":
        return flac(sys.argv[2], sys.argv[3], int(sys.argv[4]) if len(sys.argv) > 4 else 1 << 30)
    if cmd == "alac":
        return alac(
            sys.argv[2],
            int(sys.argv[3]) if len(sys.argv) > 3 else 1 << 30,
            sys.argv[4] if len(sys.argv) > 4 else "plain",
        )
    if cmd == "aac":
        return aac(
            sys.argv[2],
            int(sys.argv[3]) if len(sys.argv) > 3 else 40,
            int(sys.argv[4]) if len(sys.argv) > 4 else 2,
            int(sys.argv[5]) if len(sys.argv) > 5 else 44100,
        )
    print(__doc__)
    return 2


if __name__ == "__main__":
    sys.exit(main())
