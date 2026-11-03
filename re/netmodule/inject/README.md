# Payloads in the stock module app

`inject.py` runs a small position-independent Thumb payload once inside the stock module app (KnOS), through its
shell: the payload is written into the app's heap and called as the `guard` command's execute method, then chains to
the original method so the command completes as stock, and the vtable is restored. It ran on the unit under KnOS
[HW 2026-10-16]: calls into the app (string compare, the shell's printf), a NAND read that matched the BL1 dump, and a
Wi-Fi MIB read of the firmware version 0x00a70506. `nandread.S` is the example kept: `python3 -I
re/netmodule/inject/inject.py` reads NAND bytes 0-15 through the app's flash driver (169.254.1.1 by default).
Toolchain: `arm-none-eabi-gcc` (nix: `pkgsCross.arm-embedded`), `-mcpu=arm926ej-s -march=armv5te`.

Rules for a payload:
- The app's heap (cached SDRAM) is executable; its library functions are ARM, its shell command methods Thumb.
- A payload must hand control back to the app quickly and leave its state as it was: the module's watchdog reboots
  it after a fault (~15 s), and the MCU's supervision notices a module that stops answering.
- The Wi-Fi device exists only when the module booted on Wi-Fi (no Ethernet plugged in).

Development on the module happens in our OS (`module/os/`); the debug system's `zd1-memsh` gives the same `rd` / `wr`
access to memory.
