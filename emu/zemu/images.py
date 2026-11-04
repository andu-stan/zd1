"""What the boards boot: the factory images of a test unit, our builds, the stock images. Our builds and the test unit
come from nix (flake.nix), linked into emu/img (gitignored); the rest is made on first use and cached there."""

import hashlib
import os
import shutil
import subprocess
import sys

from .paths import IMG, REPO, TOOLS

PY = [sys.executable, "-I"]
ZTOOLS = os.path.join(REPO, "tools", "zeppelin")
STATE_FILES = ("mcu_flash.bin", "user_page.bin", "eeprom.bin", "nand.bin")


def sh(cmd, **kw):
    print("  $ " + (cmd if isinstance(cmd, str) else " ".join(cmd)), flush=True)
    return subprocess.run(cmd, shell=isinstance(cmd, str), cwd=REPO, check=True, **kw)


def rel(p):
    return os.path.relpath(p, REPO)


def nix_out(attr, link):
    """A flake output, built if needed and linked at `link` (nix rebuilds only what changed)."""
    sh(["nix", "build", "-o", rel(link), ".#" + attr])
    return link


def fast_copy(src, dst):
    """Copy a file; on APFS a copy-on-write clone (instant, no space used until written): the NAND is 128 MB."""
    if os.path.exists(dst):
        os.remove(dst)
    if sys.platform == "darwin":
        import ctypes

        try:
            libc = ctypes.CDLL("libc.dylib", use_errno=True)
            if libc.clonefile(os.fsencode(src), os.fsencode(dst), 0) == 0:
                return dst
        except (OSError, AttributeError):
            pass
    return shutil.copyfile(src, dst)


def factory_unit():
    """Factory-fresh images for the test unit (nix/test-unit.nix, tools/zeppelin/recover/mkunit.py): MCU flash (B&W
    bootloader + stock app), user page, EEPROM, NAND (BL1, BL2, stock app, PersParam with a made-up MAC)."""
    return nix_out("test-unit", os.path.join(IMG, "test-unit"))


def stock_stage2():
    """The stock module app's stage 2 (loaded at 0x60008000 for a direct boot), carved from blobs/module/app_DMP.bcd."""
    out = os.path.join(IMG, "stock_stage2_0x60008000.bin")
    if not os.path.exists(out):
        d = os.path.join(IMG, "stock_bcd")
        sh(
            PY + [os.path.join(ZTOOLS, "fw", "bcd.py"), "blobs/module/app_DMP.bcd", rel(d)],
            stdout=subprocess.DEVNULL,
        )
        shutil.copyfile(os.path.join(d, "sec0_app_main_0x60008000.bin"), out)
    return out


def os_image():
    """Our module OS (module/os, nix/os-image.nix)."""
    return nix_out("os-image", os.path.join(IMG, "os"))


def bcd_nand(bcd, out):
    """A .bcd installed into a copy of the factory NAND the way BL2's flasher leaves it (emu/tools/bcdinstall.py)."""
    sh(
        PY + [os.path.join(TOOLS, "bcdinstall.py"), os.path.join(factory_unit(), "nand.bin"), bcd, out],
        stdout=subprocess.DEVNULL,
    )
    return out


def module_image(name, nand_boot):
    """IMAGE -> dict(nand=, stage2=, stock=): what the module machine loads. stage2 None = boot from NAND via BL1."""
    unit = factory_unit()
    if name == "os":
        d = os_image()
        # the dev payload: the debug system as one plain initramfs; with --nand, os.bcd through BL1
        stage2 = os.path.join(d, "payload_dev.bin")
        return {"nand": os.path.join(d, "nand_os.bin"), "stage2": None if nand_boot else stage2, "stock": False}
    if name == "stock":
        return {"nand": os.path.join(unit, "nand.bin"), "stage2": None if nand_boot else stock_stage2(), "stock": True}
    path = os.path.abspath(name)
    if not os.path.isfile(path):
        sys.exit("no such image: %s (os, stock, none, a .bcd, a NAND image or a stage-2 .bin)" % name)
    if path.endswith(".bcd"):
        # keyed on the install code too: a fix to it must not reuse a NAND it installed before
        key = open(path, "rb").read() + open(os.path.join(ZTOOLS, "fw", "bcd.py"), "rb").read()
        out = os.path.join(IMG, "bcd-%s.nand" % hashlib.sha1(key).hexdigest()[:12])
        if not os.path.exists(out):
            bcd_nand(path, out)
        return {"nand": out, "stage2": None, "stock": False}
    if os.path.getsize(path) == os.path.getsize(os.path.join(unit, "nand.bin")):
        return {"nand": path, "stage2": None, "stock": False}
    return {"nand": os.path.join(unit, "nand.bin"), "stage2": path, "stock": False}  # a stage 2 at 0x60008000


# ---- the main board ---------------------------------------------------------------------------------------------
def mcu_flash(app_bin, out):
    """256 KB raw flash: the B&W DFU bootloader + an app-region .bin (0x80004000), 0xFF fill."""
    bl = open(os.path.join(REPO, "blobs", "mcu", "bootloader.bin"), "rb").read()
    app = open(app_bin, "rb").read()
    assert len(bl) == 0x4000 and len(app) <= 0x3C000
    os.makedirs(os.path.dirname(out), exist_ok=True)
    open(out, "wb").write(bl + app + b"\xff" * (0x3C000 - len(app)))
    return out


def our_mcu():
    """Our MCU firmware (mcu/) as the emulator build (nix's mcu-emu): its log mirrored on USART0 (the run's
    mcu_log.txt). Returns the 256 KB flash image."""
    d = nix_out("mcu-emu", os.path.join(IMG, "mcu-emu"))
    return mcu_flash(os.path.join(d, "zd1emu.bin"), os.path.join(IMG, "mcu-emu.bin"))


def mcu_image(spec):
    """--mcu ours | stock | PATH (a 256 KB raw flash image) -> the flash image."""
    if spec == "ours":
        return our_mcu()
    if spec == "stock":
        return os.path.join(factory_unit(), "mcu_flash.bin")
    if os.path.isfile(spec) and os.path.getsize(spec) == 0x40000:
        return os.path.abspath(spec)
    sys.exit("--mcu: ours, stock or a 256 KB raw flash image (got %s)" % spec)
