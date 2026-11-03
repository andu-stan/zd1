# module/boot: bare-metal images for the DM870

Two `armv5te-none-eabi` binaries on the MCU link (`module/link`), each with its own linker script:

| Binary | Linked at | What |
|---|---|---|
| `linuxboot` | 0x61F00000 (`linuxboot.ld`) | the boot stub in front of the Linux kernel: link up (status booting, "zd1-boot" on 0x25); with the payload's slots flag, the MCU's answer on reg 0xC4 picks a system slot as the root (`root=` on the command line, no initramfs) or the rescue initramfs (`docs/module/storage.md`, The boot); then initramfs + kernel into place, ATAGs, jump. Image layout: `module/os/mkpayload.py` |
| `mini` | 0x60008000 (`mini.ld`) | a minimal module image (console + link, HCR 0x21 into BL2, prints its `ZD1_TAG`): a small known image for trying an install path (`zemu run IMAGE.bcd` after `tools/zeppelin/fw/mkbcd.py`) |

`src/lib.rs` has what both share: the entry point (`entry!`), the board (registers at their physical addresses), the
console (UART1, which our MCU firmware logs) and the panic handler.

`nix build .#boot` builds both (`linuxboot.bin`, `mini-dev.bin`; the OS and Linux images take the stub from it),
`module/os/assemble.py mini TAG` a tagged mini into `emu/img/mini-TAG.bin`. Toolchain: the nightly in `rust-toolchain.toml` with rust-src (this target needs `build-std`;
`nix/boot.nix` vendors the std's dependencies from that toolchain's own lock file). In the
emulator: `python3 emu/zemu run os --nand` boots through BL1, the preloader and the stub like the unit.
