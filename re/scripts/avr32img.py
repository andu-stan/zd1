"""The stock MCU app as a raw image from 0x80000000 (re/gen/mcu-app/image.bin; re/README.md, "The MCU image"):
big-endian flash words, C strings, and the .data load image that RAM 0x8-0x3630 starts from (.bss after it is zero
at boot, so it has no bytes here)."""

import struct

FLASH = 0x80000000
DATA_LMA, DATA_VMA, DATA_END = 0x80034AD0, 0x8, 0x3630


def in_flash(a):
    return FLASH <= a < FLASH + 0x40000


def in_data(a):
    return DATA_VMA <= a < DATA_END


class Image:
    def __init__(self, path):
        self.b = open(path, "rb").read()

    def offset(self, a):
        """File offset of a flash address, or of an initialised-RAM address in the .data load image; else None."""
        if in_flash(a) and a - FLASH < len(self.b):
            return a - FLASH
        if in_data(a):
            return DATA_LMA - FLASH + a - DATA_VMA
        return None

    def bytes(self, a, n):
        o = self.offset(a)
        return self.b[o : o + n] if o is not None else b""

    def word(self, a):
        w = self.bytes(a, 4)
        return struct.unpack(">I", w)[0] if len(w) == 4 else None

    def cstr(self, a, least=4, most=100, chars=(9, 10)):
        """The NUL-terminated printable string at a (least <= length < most), or None."""
        o = self.offset(a)
        if o is None:
            return None
        e = self.b.find(b"\0", o)
        s = self.b[o : e if e >= 0 else len(self.b)]
        if least <= len(s) < most and all(32 <= c < 127 or c in chars for c in s):
            return s.decode()
        return None
