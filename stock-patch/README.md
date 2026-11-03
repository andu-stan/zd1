# Patching the stock MCU firmware

`patch.py` puts the telnet hook into the stock Zeppelin Air MCU app (2.04.05, `blobs/mcu/zd1-2.04.05.hex`): once the
module is ready, the MCU writes reg 0x75 = "23" and reg 0x76 = 5, the module's KnOS shell on telnet port 23,
re-asserted every ~30 s (`telnet_hook.S`). Python only:

```
python3 -I stock-patch/patch.py out.hex        # or: python3 tools/zeppelin stock-patch out.hex
```
Flashed and checked on the unit 2026-10-16. It is not on the unit now: that runs our own firmware (`mcu/`), which has
the amp interlock built in and puts the shell on telnet with `zd1 reg write` (`docs/module/knos.md`).

How: the BcoWrapper tick calls `voTask` through a pointer literal at 0x8001e870; the patch points it at the hook in
free flash at 0x80038100, which runs a small state machine and then calls the real `voTask`. One byte of state at RAM
0x8250 (crt0's bss end and the heap base move from 0x8250 to 0x8258). Every patched word is checked against the stock
value first. Nothing in the bootloader or the user page changes, so DFU recovery stays available.

Flash: `zd1 dfu write out.hex --backup <backup.tar> --yes --start` (`zd1 dfu backup` first) (DFU: hold ON/OFF at power-up).
Back to stock: the same with `blobs/mcu/zd1-2.04.05.hex`.

The hook blob (`telnet_hook.bin`) is committed; `build.sh` rebuilds it from the source (`nix build .#telnet-hook`)
and shows a diff if anything changed.
