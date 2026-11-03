# Dump the DM870 remote-register (SPI host link) table from the app image (base 0x60008000)
import struct
import sys

f = open(sys.argv[1], "rb").read()
B = 0x60008000


def u32(a):
    return struct.unpack_from("<I", f, a - B)[0]


def cstr(a):
    o = a - B
    return f[o : f.index(b"\0", o)].decode("latin1")


lit = int(sys.argv[2], 16)  # address of the literal holding table pointer
t = u32(lit)
print(f"# table @0x{t:08x}")
print("reg_dec reg_hex size  p3 p4  name")
i = 0
while True:
    name, reg, size, p3, p4 = struct.unpack_from("<5I", f, t - B + i * 20)
    if name == 0:
        break
    print(f"{reg:5d}  0x{reg:02x}  {size:5d} {p3 & 0xFF:3d} {p4 & 0xFF:3d}  {cstr(name)}")
    i += 1
