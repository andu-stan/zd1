"""Where things are in the repo, for every tool: blobs (kept in git), downloads (fetched), units (per-unit data)."""

import os

REPO = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
BLOBS = os.path.join(REPO, "blobs")
DOWNLOADS = os.path.join(REPO, "downloads")
UNITS = os.path.join(REPO, "units")  # per-unit data (MAC, Wi-Fi settings, live dumps): never in git

MCU_BOOTLOADER = os.path.join(BLOBS, "mcu", "bootloader.bin")  # 0x80000000-0x80003FFF, 16 KB
MCU_STOCK_HEX = os.path.join(BLOBS, "mcu", "zd1-2.04.05.hex")  # stock app 2.04.05 as B&W ship it
MODULE_BL1 = os.path.join(BLOBS, "module", "bl1.enc")  # NAND block 0 as stored (encrypted)
MODULE_BL2 = os.path.join(BLOBS, "module", "bl2.bin")  # NAND 0xA0000 partition (MetaInfo + BL2)
MODULE_BL1_RUN = os.path.join(BLOBS, "module", "bl1.run")  # BL1 decrypted, re-runnable at 0x400000
MODULE_STOCK_BCD = os.path.join(BLOBS, "module", "app_DMP.bcd")  # stock module app + CnE + resources
NOKIA_KERNEL = os.path.join(BLOBS, "module", "nokia-hd1", "kernel.bin")  # Nokia HD-1 Kernel_A (2.6.24.7)
NOKIA_ROOTFS = os.path.join(BLOBS, "module", "nokia-hd1", "rootfs")  # the subset of its rootfs we use

APP_BASE, APP_SIZE = 0x80004000, 0x3C000  # MCU app region (B&W DFU layout)


def stock_mcu_app():
    """The stock MCU app region (0x3C000 bytes), = a DFU readback of a stock unit."""
    from zeppelin.fw import ihex

    return bytes(ihex.image(open(MCU_STOCK_HEX).read(), APP_BASE, APP_SIZE)[1])
