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
- ~~The stock MCU's network-settings backup~~ **settled from B&W's code** (ZD1L 2.04.35's MCU app, decompiled with
  Ghidra's AVR32 module; not from the emulator, whose stock pair asserts in `dmhBcoMgr_ercGetCneValue()` at 21 s and
  never writes the page). The backup lives in the user page (0x1FC bytes, marker `CooL` at 0x1F7). The only writes:
  - **the end of the sync** (0x8001d36c, state 3): after a complete pass (config `HWID`, `MAC0`, `CTRY`/`FPIN`, regs
    0x2E, 0x5A–0x66, 0x35, 0x58, 0x37–0x3F, `Application/NetIF/mode`), and only if the flag at RAM 0x50A0 is 1 and
    the SSID read back (reg 0x5E) is non-empty and not `FALSE` (0x8001d110). Each read that fails leaves the state
    machine where it is (it retries); it never moves on with what it has;
  - **the factory reset** (0x8001da70: marker cleared), B&W's own key combination.

  The strings `Save network settings in user page …` and `Read network settings from user page …` are referenced
  nowhere in 2.04.35. Our image never answers config `HWID`, so the sync stops at its fourth step; and if it ever got
  further, the SSID it reads from us is empty and the save is refused. **So the image can't overwrite the backup.**
  (The restore, 24 `Networking/…` writes, is taken as `wifi.add`.)
- The DRC timing (DE/DF/E1/E2 resent after 3 s without an ack), the play state's auto-switch, and the volume path,
  against the stock MCU app in the emulator.
- 44.1 kHz output: zd1-audio's output converts other rates on their way into the queue (`resample.rs`, integer
  linear interpolation) and runs the device at 44.1 kHz in this mode. Not by offering senders only 44.1 kHz
  formats: an Apple TV doesn't convert its 48 kHz lossy items then, it sends empty frames (docs/streaming.md,
  Formats). Heard on no unit yet.

## Recovery
The image writes only the app partition: BL1, BL2 and PersParam are never touched. ON/OFF held at power-up brings
up BL2's web updater on Ethernet, where B&W's `zd1lv20435.fw` puts the stock app back. That restore is not yet seen
on a unit.

## Not carried over
Our MCU firmware's features: the host link over USB (settings, updates, the console), the volume cap, the amp's idle
sleep, the LED per receiver.
