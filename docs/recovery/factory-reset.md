# Factory recovery, both boards

Goal: bring a Zeppelin Air back to factory-fresh from any brick: stock MCU and module firmware, factory configuration,
no user settings (it comes up unconfigured, like new). Everything that is per unit is rebuilt from the **MAC address**
(label on the unit, or a backup). Two cases:
1. **The MCU's DFU bootloader is intact** (the common case): everything else may be broken. USB only.
2. **Total brick**: both chips wiped. A programmer puts the images back, then case 1.

Every tier was rehearsed in the emulator [EMU], both boards, from the factory images of a test unit (an emulator
result here is a plausibility check, nothing more). The Status column says
which have been done on the unit. **Don't write to a real unit without its owner's
OK.**

## What is per unit

| Part | Factory state | Rebuilt by |
|---|---|---|
| MCU bootloader 0x80000000-0x3FFF | `blobs/mcu/bootloader.bin` | a programmer only (BOOTPROT) |
| MCU app | the stock app (`blobs/mcu/zd1-2.04.05.hex`) | DFU `restore` / `flash` |
| MCU user page | erased + ISP word 0x929E0870 at 0x1FC | DFU `userpage-write`; **the stock app fills the rest from the module** [EMU] |
| EEPROM | blank | **the stock app** writes its default record |
| GP fuses | 0x7C04FFFF | DFU sets/clears ISP_FORCE itself; the rest by programmer |
| NAND block 0, BL1 | `blobs/module/bl1.enc` | a programmer only |
| NAND 0x20000 PersParam | factory records + **the MAC** (`persparam.py --factory MAC`) | **BL2 rebuilds the defaults** (MAC 0) when it's blank [EMU]; then the MAC goes back (below) |
| NAND 0x40000 factory CnE | `.bcd` section 1 | BL2 "update image & CnE" |
| NAND 0x60000-0xA0000 user settings | erased | BL2 resets them on every update |
| NAND 0xA0000 BL2 | `blobs/module/bl2.bin` | open (below) |
| NAND 0x240000 app, 0xC40000 resources | `.bcd` sections 0 and 2 | BL2's web updater |

`python3 tools/zeppelin mkunit --mac EC:66:D1:xx:xx:xx --out units/<unit>/factory` writes all of it for one unit:
`nand.bin` (logical), `mcu_flash.bin`, `user_page.bin`, `eeprom.bin`, `manifest.json`. The output contains the MAC: keep
it in `units/`.

## Case 1: tiers (MCU DFU intact)

| Broken | Recovery | Status |
|---|---|---|
| MCU app | hold ON/OFF at power-up → DFU; `zd1 dfu restore <backup.tar> --yes --start` (or `dfu flash blobs/mcu/zd1-2.04.05.hex --i-have-a-backup --yes --start`) | **unit** |
| MCU user page (+ EEPROM) | DFU comes up by itself (invalid ISP word); `dfu userpage-write <unit>/user_page.bin --yes`; power-cycle | EMU |
| module image, anything (corrupt, erased, hangs, boot-loops) | **`zd1 module esc`** (`Module ESC`): BL1's serial menu over USART1, option 4 → BL2 → install the stock `.bcd` with `bl2web`. Needs our MCU firmware | **unit** (2026-10-23; `Module ESC` 2026-11-02) |
| module image hangs, the image answers SPI | HCR 0x21 → BL2 (`zd1 module bl2`, or the MCU bootloader's relay) → install | **unit** (both) |
| module image, with JTAG on the carrier | the BL2 mailbox over JTAG + a PA17 reset pulse (`docs/module/jtag.md`) → install | **unit** |
| module app corrupt / erased | the stock preloader's checksum fails / BL1 finds no valid app → BL2 by itself → install | EMU |
| PersParam + app | BL2 (any path above) rebuilds the PersParam defaults → install → the MAC back | EMU |
| PersParam only, app intact | the stock app boot-loops before its SPI link is up (~49 resets in 150 s), so HCR can't reach it; `zd1 module esc` works because BL1 runs first [INF] | EMU |
| BL2, app intact | candidate: the stock app's `fburn ebin 0 0xA0000 0x160000` + XModem of `blobs/module/bl2.bin` over the module console (`zd1 console`; over telnet the y/n confirm never sees the answer) | **open** |
| everything but the bootloaders | DFU (by itself) → user page, MCU app (ours, for `zd1 module esc`) → BL2 → `.bcd`, the MAC back | EMU |
| BL1 | programmer | case 2 |

**Both boards back to stock in one go**: `zd1 rollback STOCK.bcd blobs/mcu/zd1-2.04.05.hex --yes` (the module first
through the relay, then the stock MCU firmware with a backup of what is on the MCU; `host/core/src/install.rs`).

**Installing the module image**, over USB only: the MCU bootloader's relay, what B&W's updater uses (`zd1
dfu`, `zd1 dfu relay IMAGE.bcd --yes`, `zd1 dfu start`; ~6.5 min for the stock 9 MB, 85 s for our
`os.bcd`, works with the app partition invalid: `docs/mcu/bootloader-dfu.md`) [HW 2026-10-26]; with our MCU app
running, `zd1 xmodem IMAGE.bcd` [HW]. Or BL2's web updater on the wired link
(`python3 tools/zeppelin bl2web 169.254.1.1 IMAGE.bcd --yes --iface=en8`, after `zd1 module esc`) [HW]: BL2 tries DHCP
first and answers at 169.254.1.1 only minutes later on a link without a DHCP server.

**The MAC back** with the stock module app running and our MCU firmware in place for the telnet switch [HW for the
shell commands]:
```
zd1 reg write 0x75 --v 3233000000000000; zd1 reg write 0x76 0500     # the shell on TCP 23
python3 tools/zeppelin knos --host <module ip> 'persparam wrb MAC0 0xec66d1xxxxxx' 'persparam wrb DTCV 0x01000000' 'persparam check'
zd1 reg write 0x77 --v 3530303031000000; zd1 reg write 0x76 0400     # the stock layout again
```
(`persparam wr DTCV 1` stores 20 bytes: use `wrb`.) Then power-cycle the module. Untested alternative without the
app: BL2's serial menu "8 program MAC0" on the module console (`zd1 console`).

## Case 2: total brick (programmer)
Writing mkunit's images into the emulator's backing files boots both boards to stock with the unit's MAC [EMU]. Nothing of this has been done on hardware. What it needs:
- **MCU** over AVR32 JTAG (Atmel-ICE / JTAGICE3 with avr32program is the reliable route; OpenOCD's AVR32 support is
  weak): chip erase, `mcu_flash.bin`, `user_page.bin`, then the fuses (FGPFR 0x7C04FFFF, BOOTPROT last). The pads are
  not located.
- **Module NAND** with a TSOP48 programmer: `nand.bin` is logical. A chip image needs the chip's bad-block list (ours
  ends at 0x07FDFFFF: one block less than nominal), the spare layout (3-byte Hamming per 512 B in four 12-byte slots
  at spare 0x10-0x3F, `docs/module/boot-chain.md`) and whatever the boot ROM expects of block 0. The emulator's NAND
  synthesizes spare areas for a logical image (`emu/README.md`), so trying a chip image there first needs a model
  that takes one as it is.
