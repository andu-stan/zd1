"""XModem over any byte transport: the KnOS shell's socket (knos/fburn.py). The transport is two functions:
`write(bytes)` and `getc(secs)`, one byte or b"" at the timeout.

  send(write, getc, data)   XModem-1K with CRC-16, each block until ACKed, then EOT
  recv(write, getc, n)      XModem-CRC (128- and 1024-byte blocks), the first n bytes
"""

import time

SOH, STX, EOT, ACK, NAK, CAN = 0x01, 0x02, 0x04, 0x06, 0x15, 0x18


def crc16(data):
    """XModem's CRC-16 (CCITT)."""
    c = 0
    for b in data:
        c ^= b << 8
        for _ in range(8):
            c = ((c << 1) ^ 0x1021 if c & 0x8000 else c << 1) & 0xFFFF
    return c


def _answer(getc, secs, c_is_nak):
    """the receiver's ACK, NAK or CAN within secs (b"" if none); its 'C's and any text skipped, or a 'C' taken for a
    NAK (block 1, to a receiver still asking)"""
    end = time.time() + secs
    while (left := end - time.time()) > 0:
        b = getc(left)
        if b in (bytes([ACK]), bytes([NAK]), bytes([CAN])):
            return b
        if b == b"C" and c_is_nak:
            return bytes([NAK])
    return b""


def send(write, getc, data, timeout=30, first_on_c=False, progress=lambda n, total: None):
    """XModem-1K with CRC-16; the number of blocks once the EOT is ACKed. Each block goes again on a NAK (10 tries);
    CAN or `timeout` s without an answer aborts. With `first_on_c`, block 1 also goes again on each 'C' (a receiver
    that asked before the block was there, or one that cancels unless block 1 is already waiting: the emulator's
    BL2); without, the caller has seen the receiver's 'C' and its later ones are skipped (sending block 1 twice would
    make it ACK twice, and the second ACK would pass for block 2's)."""
    blocks = [data[i : i + 1024] for i in range(0, len(data), 1024)]
    for n, blk in enumerate(blocks, 1):
        blk = blk.ljust(1024, b"\x1a")
        pkt = bytes([STX, n & 0xFF, 0xFF - (n & 0xFF)]) + blk + crc16(blk).to_bytes(2, "big")
        for _ in range(10):
            write(pkt)
            r = _answer(getc, timeout, first_on_c and n == 1)
            if r == bytes([ACK]):
                break
            if r != bytes([NAK]):
                raise OSError("block %d: %s" % (n, "cancelled by the receiver" if r else "no answer in %d s" % timeout))
        else:
            raise OSError("block %d: 10 NAKs" % n)
        progress(n, len(blocks))
    for _ in range(10):
        write(bytes([EOT]))
        if _answer(getc, 10, False) == bytes([ACK]):
            return len(blocks)
    raise OSError("the EOT was not ACKed")


def recv(write, getc, length):
    """XModem-CRC: 'C' until the first block comes, every good block ACKed, a repeated one too; the first `length`
    bytes."""

    def exact(n, secs):
        buf, end = b"", time.time() + secs
        while len(buf) < n:
            if (left := end - time.time()) <= 0:
                raise TimeoutError("receive timeout (%d/%d)" % (len(buf), n))
            buf += getc(left)
        return buf

    for _ in range(10):
        write(b"C")
        first = getc(1.5)
        if first:
            break
    else:
        raise TimeoutError("no XModem start")
    out, expected = bytearray(), 1
    while True:
        t = (first or exact(1, 5))[0]
        first = b""
        if t == EOT:
            write(bytes([ACK]))
            break
        if t == CAN:
            raise OSError("sender cancelled")
        if t not in (SOH, STX):
            continue
        hdr = exact(2, 5)
        data = exact(128 if t == SOH else 1024, 10)
        crc = exact(2, 5)
        ok = hdr[0] == 255 - hdr[1] and crc16(data) == crc[0] << 8 | crc[1]
        if ok and hdr[0] == expected & 0xFF:
            out += data
            expected += 1
            write(bytes([ACK]))
        elif ok and hdr[0] == (expected - 1) & 0xFF:
            write(bytes([ACK]))
        else:
            write(bytes([NAK]))
    return bytes(out[:length])
