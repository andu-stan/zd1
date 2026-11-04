# MCU bootloader and DFU

The MCU's 16 KB bootloader at 0x80000000-0x80003FFF ("ZD1 Bootloader version 0514", DevID 7904) is the way back from
anything that can go wrong with the app. It is Atmel's FLIP-style USB DFU bootloader with B&W extensions, including a
relay that forwards network-module firmware over SPI. `blobs/mcu/bootloader.bin` is a copy read from our unit over
DFU: BOOTPROT blocks writes and erases, not reads; LOCK is unset [HW]. The disassembly is
`re/mcu/work/bootloader/bl.dis` (its DFU core runs from SRAM 0x4000 = flash 0x80000228 + offset). Tags:
`docs/README.md`.

## Memory and fuses

| Region | Content |
|---|---|
| 0x80000000-0x80003FFF | bootloader (BOOTPROT = 2: DFU can't write it; only JTAG can) |
| 0x80004000-0x8003FFFF | app (stock: a jump to 0x80008000, crt0 at 0x80008024) |
| 0x80800000 | user page (512 B): stock's copy of the network setup; **ISP config word 0x929E0870 at +0x1FC** |
| GP fuses (FGPFR) | 0x7C04FFFF on this unit: LOCK 0xFFFF, EPFL 0, BOOTPROT 2, BOD settings, ISP_IO_COND_EN 1, **ISP_FORCE (bit 31) 0** [HW] |

## Reset decision [RE]
```
if ISP_FORCE (GP fuse bit 31)              -> DFU          (checked first, for every reset cause)
elif reset cause POR / external / JTAG:
     if the ISP word at 0x808001FC is valid (CRC-8 + signature 0x494F)
        and PA08 (ON/OFF key) is held       -> DFU          (the "hold ON/OFF at power-up" path)
     else                                    -> app
elif watchdog reset                          -> app          (clears the SRAM magic)
else (BOD, other warm reset):
     if SRAM[0] == "ISPK"                    -> DFU
     else                                    -> app
```
- **Invalid ISP word = DFU on every cold boot**, key or not: a blank user page is not a brick, the app just never
  runs until the page is rewritten.
- DFU entry disables the watchdog first thing. There is no DFU idle timeout: the only ways out are the start-app
  commands.
- The app starts at 0x80004000 with all registers zeroed, running on the RC oscillator (~115 kHz).

## Ways into DFU
1. **Hold ON/OFF while powering on** (needs a valid ISP word) [HW]. Let the caps drain (~10 s unplugged) before
   re-plugging, or the reset doesn't take.
2. **Self-DFU from the app** [HW]: set ISP_FORCE (EGPB, bit 31), then a watchdog reset with a period long enough for
   the bootloader to disable the WDT. Our firmware's `Dfu` request (`zd1 bootloader`) does it with PSEL 16 (~1.1 s) and is in DFU ~3 s
   later. A minimum-period watchdog (ASF `wdt_reset_mcu`) resets in a loop, because an armed WDT survives a WDT
   reset. Stock does it when the module reports status 0x27, on variant-2 boards only, with the module powered off
   first [RE]. Since ISP_FORCE wins on every reset, that speaker stays in DFU, power cycles included, until a USB host
   starts the app. The bootloader has no other way to take an MCU image: its MCU-flash writes are reached only from
   the DFU request handler (0x80002310), not from the SPI relay (0x80002500-0x80002c00); it never touches USART1 and
   never reads GPLP [RE `bl.dis`, direct calls; the relay code has no indirect ones].
3. **Any DFU session** sets ISP_FORCE itself (the DNLOAD handler, so a reset mid-programming comes back to DFU); the
   start-app commands clear it [RE]. A unit that was left in a DFU session boots into DFU again.

## DFU protocol (USB 03EB:2FF8, interface 0)
Atmel FLIP v2 framing. `host/core/src/dfu.rs` implements it (`zd1 dfu`, `zd1 flash`, the web page), checked frame
by frame against the decompiled B&W updater (`re/ghidra/setup.py mcu-updater`; `cargo test -p zd1-host`).

| Operation | Download payload |
|---|---|
| select memory (flash / user page / fuses) | `06 03 00 mem` |
| select 64 KB page | `06 03 01 hi lo` |
| erase app (app area only; BOOTPROT keeps the bootloader) | `04 00 FF`, poll status, blank check |
| program block | `01 00 start(2) end(2)`, pad to `(addr & 0x3F) + 0x40`, data, 16-byte DFU suffix |
| read | upload after `03 00 start end` |
| start app | `04 03 01 00 00` (direct jump) or `04 03 00 ...` (watchdog reset), then a zero-length download. The jump leaves USB attached: the app needs a moment before a host talks to it (`zd1` waits 500 ms; a setup that arrived while our firmware brought USB up left it spinning in its USB driver at 0x800058f2 [EMU 2026-10-30]) |
| B&W: start module BL2 | `04 04 00 00 00` |
| B&W: coprocessor init | `06 09 t1(2) t2(2)` |
| B&W: select coprocessor | `06 03 00 07` (app section), `06 03 00 08` (BL2 section) |
| B&W: end of transfer | `06 04 07/08/09 00` (coproc / BL2 / extra) |

GETSTATUS is the usual 6 bytes, with byte 5 (iString) reused as the coprocessor status:
1 module off, 2 powered, 0x03 the module's app ready, 0x11 HCR sent / starting BL2, **0x12 BL2 ready**, 0x13
download active (also after the end of transfer).

The user page is written with the flash controller's page buffer (CPB, fill, EUP, WUP); `dfu userpage-write` drives
it and refuses an image without the factory ISP word unless `--force`.

## The module relay (`zd1 dfu relay`) [HW, RE, EMU]
The bootloader can update the network module without the MCU app: `coproc-init` powers the module (PA04, then PA17
~100 ms later), sets up SPI0 and, over the stock register protocol, asks the running module app for BL2 (HCR 0x21).
BL2 then answers XModem 'C' on register 0x2C and the bootloader streams the image as XModem-1K over registers
0x2C/0x2D (0x400-byte DFU blocks in). B&W's `upgrade.sh` uses this path.
- The handshake [RE `bl.dis` 0x80002500 and 0x80002b88, EMU trace 2026-10-27]. Two state machines, polled from the DFU
  loop: the link's (SRAM 0xA7C) and the relay's (0xA8; coproc status at 0xA4). The link's reads 0x11 until it says
  0x2100 (app ready) or 0x2300 (BL2), then idles in state 5. With the request line (PA20) high it reads 0x0C and the
  register it names, but only a register its kind table marks F or V (SRAM 0x75D8: the stock firmware's table less
  0xDC, 0xDD, 0xE3, 0xE4); for any other register it reads 0x0C again, forever. The relay's machine: 0x2100 seen ->
  status 0x03; then, with the link's machine idle and 0x11 still 0x2100, HCR 0x2120 (reg 0x28) -> status 0x11;
  0x2300 -> **0x12 BL2-ready**; then HCR 0x2220 starts BL2's XModem receiver. **A module whose queue holds a
  register the table doesn't know never gets the HCR** (status 0x03 until the host gives up): `zd1-linkd` queues only
  registers the stock tables know until our MCU firmware has greeted it (0xC3), which the bootloader never does.
- The first read of 0x11 comes ~18 s after the module's power-on [EMU]: a module still booting (our stub says
  0x2000) is read again until it's ready.
- **On the unit** [HW 2026-10-26/27]: a running image hands over to BL2 33-40 s after `coproc-init` (our OS from
  2e20f8d on: 33.8 s); with the app partition invalid BL1 goes straight to BL2 (20 s). Then 0x400-byte blocks at
  22.7-23.4 KB/s (median 38 ms each, max 263 ms): a 9 MB image (8946 blocks) in 382 s with no retries, `os.bcd`
  (2 MB) in 85 s. BL2 burns and resets the module into the new image; the bootloader's coproc status stays 0x13
  ("download-active") after the end of transfer, so it doesn't say when the module is done. Everything over USB:
  `zd1 bootloader`, `zd1 dfu relay IMAGE.bcd --yes`, `zd1 dfu start`.
- **BL2 erases and writes the app partition as the blocks arrive**: a transfer stopped after 500 blocks left
  "Application: invalid image" (BL2's menu 5), and BL1 starts BL2 at every boot until a complete image is installed
  [HW 2026-10-26].
- **Our MCU app as the sender** (`zd1 xmodem`, the host link's `XmFrame`) [HW 2026-10-26]: `Module BL2`,
  HCR 0x2220, BL2's 'C' 0.2 s later, then the image: a 9 MB image in 938 s (9.5 KB/s, 0 retries, EOT ACKed,
  "Application: CRC ok!" in BL2's menu), `os.bcd` in 305 s. Per 1 KB block (median): the SPI write 5.9 ms (3 MHz),
  **BL2 until it answers 18.0 ms**, reading the answer 31.4 ms (ours: the pending register, then 0x2C, each a request
  with a 5 ms gap and BL2's slow reply), the host link 26.5 ms (five EP0 requests). BL2's own share caps this path at
  ~40 KB/s (1 KB / (5.9 + 18) ms); everything else is ours. BL2 does not reset after the EOT (the relay does
  something after the transfer that we don't: not traced); a power cycle starts the image (`zd1 module
  restart`).
- In emulation the relay of a full 6.8 MB stock image misframed after ~2,100 of 6,687 blocks ("Invalid SPI Message"
  in BL2); the unit doesn't. The emulator's SPI timing is not the unit's.

## Rules
- Never write the bootloader region, the ISP word (except with a factory image) or the fuses other than ISP_FORCE.
  The tools refuse.
- Every backup (`zd1 dfu backup`, and the one `zd1 flash`, the installs and the page take first) reads the bootloader
  region too, as `bootloader.bin` in the tar: for the record, since another unit's (B&W's other models) may differ
  from `blobs/mcu/bootloader.bin` and only JTAG could put one back [EMU: byte-identical to it].
- After a `flash`, `restore` or `userpage-write`, the tool reads back and verifies.
- A raw `.bin` app image is based at 0x80004000; a verify must read back independently of the map that wrote it.
