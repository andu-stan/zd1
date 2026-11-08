# Stock-MCU mode: our module OS with B&W's MCU firmware (the Zeppelin Air with Lightning)

Work in progress on the `lightning-stock-mcu` branch. Nothing here has run on a unit yet. Tags: `docs/README.md`.

## Why
The Zeppelin Air with Lightning (ZD1L, model FP35548) runs the same network module and a near-twin MCU app
(`docs/mcu/stock-firmware.md`), but its MCU bootloader is another one. Held ON/OFF at power-up, a ZD1L lights its
LED white, **shows nothing on USB**, and starts the module into **BL2's web updater on wired Ethernet** (a DHCP
lease; B&W's "Zeppelin Air Setup — Firmware Update" page, `/bl_index.asp`) [HW, one ZD1L, read-only]. So
`zd1 install` (DFU + the relay) can't reach it. BL2's web path takes a module image only: no code in BL2 forwards a
`.fw`'s MCU part, and the MCU app's own network-update receiver is dead code. The ZD1L's main-board update path lives
in its bootloader, which no `.fw` carries [RE].

Stock-MCU mode leaves the MCU alone: the module runs our OS and speaks the stock MCU's dialect
(`docs/protocol/spi-registers.md` §4.2), installed and updated through BL2's web updater only.

## The image: `os-zd1l.bcd`
`nix build .#os-image` builds it next to `os.bcd` (`module/os/assemble.py`, `lightning`):
- Wrapped like the Lightning's stock app: preloader, factory CnE and resources from B&W's `zd1lv20435.fw` (fetched
  by Nix from the archive.org item `downloads/manifest.json` lists, pinned by its SHA-256). The header carries
  **HWID 0xBE**, so BL2's web check (PersParam HWID against the header) passes on a ZD1L without "expert" mode.
- The command line adds `zd1.mcu=stock`.
- The initramfs (uncompressed, at 0x61000000) is the rescue plus `system.cpio.gz` and its stamp `system.id`.

## Booting
1. The stub asks reg 0xC4 for a slot. The stock MCU doesn't answer, so the rescue boots (unchanged).
2. The rescue (`zd1-init`, `stock.rs`): if slot A's stamp isn't `system.id`, install the system into slot A
   (`zd1_slot::install`). Then mount slot A and `switch_root` into it. It says nothing on the MCU link meanwhile: the
   stub's BOOTING (0x2000) stands. The stock MCU starts its setup task once on READY, and a module that goes quiet
   after READY leaves it stuck (`module/boot/src/bin/linuxboot.rs`). Supervision allows ~30 min to READY.
3. The system finds its slot from the root mount (`zd1_slot::boot`: the command line still says rescue). With no MCU
   keeping a good slot, the running one is the good one.

## The dialect (`zd1-linkd`, to do)
Per `docs/protocol/spi-registers.md` §4.2:
- **Versions.** VersionImage carries "9112" in stripped characters 13..16. 0x26 and 0x27 are stock-shaped.
- **Configuration channel.** 0x45: the `R pssCne/CneId` reply, HostVersion written, the `Networking/…` restore
  writes taken as the Wi-Fi settings, `Application/NetIF/mode`.
- **0x32** network status.
- **0x47** play XML: `pl` / `pa` / `em`.
- **Volume** on 0x40 / 0xE4: `03 03 01 vv`, an attenuation 0–240 with 0xFF for mute.
- **DRC 0x48** from the MCU:
  - next / prev / play-pause (B0 / B1 / B3);
  - pause / resume / release / acquire (DE / DF / E1 / E2), acked on 0xE3 with 1 / 0 / 4 / 5;
  - 9D / 9F / 9E: JBConnect start, JBConnect stop and wireless apply.
- **0xE2** shutdown.
- **Audio** at 44.1 kHz only: the stock Wi-Fi path's rate.

## Open before any unit
- **The stock MCU's network-settings backup.** It keeps the stock app's network settings in its user page
  (`BCO.ZD1: Save network settings in user page`), reads them from the module in its sync task (0x8001d36c: regs
  0x35, 0x37–0x3F, 0x58, 0x5A–0x66), and restores them with 24 `Networking/…` writes. BL2's updater erases the
  module's user settings with every update, so this backup is how a speaker going back to stock gets its Wi-Fi back.
  What decides "back up" versus "restore", and what it stores when those reads time out (we answer none of them yet),
  must be seen in the emulator with a user page that holds a backup. **A run that overwrites the backup with
  nothing is a blocker.**
- The DRC timing (DE/DF/E1/E2 resent after 3 s without an ack), the play state's auto-switch, and the volume path,
  against the stock MCU app in the emulator.
- 44.1 kHz output (not done yet).

## Recovery
The image writes only the app partition: BL1, BL2 and PersParam are never touched. ON/OFF held at power-up brings
up BL2's web updater on Ethernet, where B&W's `zd1lv20435.fw` puts the stock app back. That restore is not yet seen
on a unit.

## Not carried over
Our MCU firmware's features: the host link over USB (settings, updates, the console), the volume cap, the amp's idle
sleep, the LED per receiver.
