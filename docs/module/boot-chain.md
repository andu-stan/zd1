# Module boot chain, NAND and images

How the CR870 boots, where everything lives in its NAND, what each stage checks, and how images get in. Tags:
`docs/README.md`.

```
boot ROM ──► BL1 (NAND block 0, encrypted) ──► app (0x240000)   or   BL2 (0xA0000), the updater
              │ decrypted into SRAM 0x400000      │ each: stage 1 (preloader) → SRAM 0x40DA00
              │                                     │        stage 2           → SDRAM 0x60008000
```

## Boot ROM
Not readable after boot: 0x0 is an alias of the 64 KB SRAM, and probes of the high regions crashed the module [HW].
It decrypts BL1 from NAND block 0 (length-preserving; both copies use 0x869C bytes) into SRAM 0x400000 and starts
it. Whether it has any other boot source (UART, USB) with a blank NAND is unknown.

## BL1 (block 0)
- Encrypted in NAND (entropy 7.99, starts `FF FF FF E6`); identical between our unit and the archive.org unit.
  `blobs/module/bl1.enc` as stored, `blobs/module/bl1.plain` decrypted (captured from SRAM on the unit,
  `re/netmodule/bl1/`). Build "Sep 30 2010 17:52:25". One data region is LZ/RLE-packed and unpacked at start-up;
  code runs in place.
- Decision (main 0x40453C) [RE]:
  1. A 2 s serial window on **UART1, 115200**, polled again just before an image is started: a **0x01 or 0x02**
     byte opens the **BL1 menu** (0x02 on the unit, `zd1 module esc`, 2026-10-23 [HW]; 0x01 [EMU]). Other bytes are ignored.
  2. **Forced BL2** if the mailbox at **0xB1005000** holds `{0xDEADBEEF, mode, 0x21524110}` (BL1 copies and clears
     it) [HW] or **PersParam BLDb = 1**. The rescue-pin check is a stub returning 0.
  3. Otherwise start the app if its partition is valid (MetaInfo, CRC32 over the data = MetaInfo CRC, boot header
     0xAAAA5555, stage-1 CRC); else BL2.
- BL1 menu: 1 download BL2 (XModem), 2 show meta info, 3 reset, **4 start BL2** [HW], 5 start app, 6 erase flash
  (range not traced: avoid).
- BL1 loads only this MetaInfo format; the Nokia HD-1's BL1 loads a different BL2 format (magic 0x21111978): the two
  chains don't mix. Linux on our module goes in as an ordinary app image (`linux.md`).

## Stages 1 and 2
- **Stage 1** (8 KB preloader, part of every app/BL2 image) runs from SRAM 0x40DA00: SDRAM init, NAND read with
  bad-block skipping and ECC, loads stage 2 to **0x60008000**, checks it (app: Fletcher-32 variant; 2010 BL2:
  Adler-32), enters it with a plain call: MMU and caches off, flat memory, no arguments, the app sets SP = 0x40FA00.
- If the app's stage-2 checksum fails, the preloader writes the BL2 mailbox and spins until the watchdog resets: **a
  corrupt app falls back to BL2 by itself** [RE]; a valid image that hangs does not.

## Getting into BL2

| Situation | Path |
|---|---|
| any image, BL1 intact | **`zd1 module esc`** (`Module ESC`, the MCU driving it): BL1's serial menu over the module console, option 4 [HW 2026-10-23, 2026-11-02 (MCU d865f5e); in the emulator `zemu run os --nand --real-bl1 --mcu --at-once --allow-panic --host 'zd1 module esc'` (the factory NAND's slots are empty, so the image panics where the MCU starts slot A: the escape doesn't need it)] |
| the image answers SPI | HCR 0x21 (`zd1 module bl2`; `docs/protocol/sequences.md`): the image writes the mailbox, arms timer 0 as a watchdog (0xFFFF0200 = 0x80000001 + threshold 0xFFFF0220) and resets [HW: the stock app, our boot stub, `zd1-linkd`] |
| JTAG attached | write the mailbox words to 0xB1005000, then a reset pulse on PA17 (not a power cycle: that clears the mailbox) [HW, `jtag.md`] |
| app corrupt or erased | BL1 starts BL2 by itself [RE, EMU] |
| BLDb = 1 in PersParam | BL1 starts BL2 at every boot (BL2's menu "a) Set Start Mode") [RE, EMU] |

## BL2 (0xA0000), the updater
`blobs/module/bl2.bin` (MetaInfo + boot header + stage 1 + stage 2 at 0x60008000, mostly Thumb-2; build 2010-09-27;
the archive.org copy has bit rot, ours verifies). It reports **0x11 = 0x2300** to the MCU, brings up KnOS, Ethernet,
the SPI link and:
- **Web updater** (GoAhead, port 80, no authentication) [HW]: `POST /goform/formPostHandler` multipart field
  `appFirmware`, then `GET /goform/formDownloadConfirmationHandler?button=Ok` (or the Expert handler with
  `hwcheckoverride` / `cnedownload`), progress `bl_flash_burn_status.asp`. Client: `python3 tools/zeppelin bl2web`.
  After a burn BL2 resets the board.
  - An update writes a fresh header over the user settings (they are gone).
  - Each `.bcd` section length must be a multiple of 4, else "CRC check of image failed" (error 680) [HW]. `mkbcd`
    pads.
  - The status page keeps the previous update's verdict while the next burn runs: after a failed update in the
    same BL2 session the next upload shows "100 % / failed" at once while the burn succeeds and BL2 resets the board
    [HW, EMU]. `bl2web` waits out a "failed" page for the reset.
  - It also takes B&W's over-the-air `.fw` (`docs/mcu/stock-firmware.md`) [EMU]: it reads the `.fw`'s 32-byte header
    as BridgeCo's encryption wrapper ("version = 2", "encryption mode = 0": not encrypted) and checks the `.bcd`
    inside. The A7's on the test unit (0xBD): refused, "HWID(0xbd), bcd HWID(0xa7)". With PersParam's HWID patched
    to 0xA7, every upload of it in that BL2 session failed the header CRC instead: BL2 read the wrapper one byte late
    ("total of 367 bytes in buffer"; the first upload of the first session read 366, correctly) [EMU, not
    understood]. What BL2 does with the MCU part after the module image is not seen.
- TFTP server, XModem over UART1 or SPI (the MCU bootloader's relay, `zd1 xmodem` [HW]), and a serial menu on UART1 ("Bootloader Level
  2 Menu") [RE, EMU]: 1 download image + CnE, 2 image only, 3 resource pages, **4 start image**, 5 print image info,
  **6 mark image as invalid**, 7 CnE to factory default, 8 program MAC0, 9 program GUID, a set start mode, b exit to
  shell. `zd1 module esc` stops sending after BL1's menu, so it never reaches these.
- Image check (`checkBCDAndFlashStatus`, 0x60040de4) [RE]: header CRC32, the `.bcd`'s HWID against PersParam's
  (expert mode skips it), each section's CRC32, a version compare. **No signature.** Optional Blowfish with
  BridgeCo's placeholder key, off. Keep the version ≥ stock. **Only the web updater runs it**: its one call (0x60040c56)
  is in `WebFirmwareUpdate::closeBinData`; the SPI XModem (the relay, `zd1 xmodem`) hands the data to the
  FlashUpdateService without it, and BL2 reads PersParam's HWID nowhere else but its banner and the CnE value
  `HardwareID`. An `os.bcd` with its header's HWID set to 0xBE (CRC recomputed) went on a unit whose PersParam says
  0xBD through `zd1 xmodem`: "doFirmwareUpdate() OK", the partitions' CRCs right [EMU 2026-11-04: `zemu run --nand
  --mcu --allow-panic --until 'waiting for a system' --host 'zd1 xmodem IMAGE'`].
- "a) Set Start Mode" asks "enable JTAG start mode [y/n]" and writes **BLDb** (y = 1): BL1 then forces BL2 at every
  boot, which parks the module where a debugger can attach. It is not a hardware JTAG enable [RE, EMU].
- HCR 0x2421 (factory reset of the user settings) is handled like the app does [RE, EMU].
- If PersParam is blank, BL2 rebuilds the defaults (BLDb, HWID 0xBD, HWVS, GUID 0, MAC0/MAC1 0, CnEA/U/S, BLDF)
  [RE, EMU].

## NAND layout (HY27U1G8F2B, 128 MB, 2 KB pages + 64 B spare, 128 KB blocks)

| Offset | Content |
|---|---|
| 0x0000000 (block 0) | BL1 (0x869C used) |
| 0x0020000 | **PersParam** (per unit) |
| 0x0040000 | factory CnE (type 4, = `.bcd` section 1; data at +0x80) |
| 0x0060000-0x009FFFF | user settings, two copies (device name, UUID, network, Wi-Fi incl. passphrase) |
| 0x00A0000 (0x160000) | BL2 (type 1) |
| 0x0240000 (0xA00000) | app (type 2), single slot |
| 0x0C40000 (0x400000) | resources (type 7: web UI, UI strings) |
| rest | erased |

- Addresses are **logical**: stage 1 skips bad blocks (marker in the spare area of pages 0/1). Our chip ends at
  0x07FDFFFF (one block less than nominal) [HW].
- **ECC** [HW 2026-10-23, `tools/zeppelin/nand/ecc.py`]: one 3-byte code per 512-byte sector, a plain Hamming code,
  **not inverted** (a blank sector's code is 00 00 00). Each byte holds four (P, P') pairs, P in the odd bit, P_k =
  parity of the bits whose address has bit k set: byte 0 = byte-address bits 4-7, byte 1 = bits 0-3, byte 2 = bit 8
  and the bit-in-byte 0-2. Fitted against 166 pages of a burned image with their live spare areas: every bit
  matches, and all 151 written pages read raw from blocks 1-4 and the app check out.
- **Spare area** [HW]: byte 0x0C = 00 on a programmed page; **0x0F (then 0x0E) counts in-place rewrites**, one bit
  pair cleared per write (3f = first, 0f = second, ...); write n puts its four codes in the 12-byte slot
  0x10 + 12·(n-1) (0x10, 0x1C, 0x28, 0x34). Page 0 of each burned partition is written twice (the header is
  finalised last: codes at 0x1C, the stale set at 0x10). The PersParam page, appended to in place, is at its fifth
  write (0x0F = 00, 0x0E = 3f): no slot left, so it has **no valid ECC** (the four slots hold codes of older
  contents).
- **BL1's and BL2's pages also carry SmartMedia codes** in spare bytes 40-63 [HW, the 2026-10-23 raw dump]: one 3-byte
  code per 256-byte half, Linux's `nand_calculate_ecc` with its first two bytes swapped (the SmartMedia order); all
  3,208 halves of block 0 and blocks 5-11 match. Only these factory-programmed blocks have them; who checks them is
  not known ([INF] the boot ROM, for BL1).
- **Linux reads are miscorrected** [HW]. The Nokia kernel's `sulu_nand` sets Linux soft ECC (`ecc.mode` 1 in
  sulu_nand_probe) with the default large-page layout (codes at spare 40-63, all 0xFF on this chip). For a 256-byte
  half with odd parity, ~FF against the computed code looks like a single-bit error, and the kernel "corrects" a good
  bit: one flipped bit in about half of all 256-byte halves of every normal MTD read (588 bits in 342 KB of the app).
  Read raw (`MTDFILEMODE` = `MTD_MODE_RAW`): block 1 is byte-identical to the 2026-10-17 dump, 57 records, 24 live.
  `zd1-persparam` reads raw; `zd1-persparam raw OFF N` (the debug system) dumps pages with their spare areas, `ecc.py` checks them.
  Never write the vendor's area through the kernel's MTD: our OS has it as a read-only partition; its own partitions
  (system, data) are written and read with the kernel's ECC (`storage.md`).

### Partition header (first 0x80 bytes)
`char date[16]; u32 modelId, swVersion, partitionId, partitionLength, baseAddress (= partition + 0x80), imageLength,
crc32, eFlag (0x7F = valid); "MetaInfo"`; for type 7 (resources) at 0x38: 0x48, 0, 8 container start addresses
(unused = the end) and their 8 sizes (the `.bcd`'s words at 0x90: the FFS directory, its files, the language
container). The app finds its resource containers through this table: with another image's table it reads garbage
(B&W's A7 image: resource errors; the Lightning model's: a hang in ResourceManagerService) [EMU].
`tools/zeppelin/fw/bcd.py` `partition()` writes it as BL2 does (byte-identical after an emulated SPI XModem).

### Boot header (at partition + 0x80)
```
+00 55 55 AA AA   +04 u32 1   +08 A5A5A5A5   +0C stage-1 load address (0x0040DA00)
+10 stage-1 length / 8   +14 CRC32 of image[0x18 : 0x18 + len·8]   +18 EA000008 (first stage-1 instruction)
+1C A5A5A5A5   +20 stage-2 offset from 0x60000000 (0x8000)   +24 stage-2 NAND address   +28 stage-2 length
+2C stage-2 checksum   +40 stage 1 (8 KB)   +2000 stage 2
```
App stage-2 checksum: 16-bit LE words, a = b = 0xFFFF, folded every 360 words, result a | b << 16.

## `.bcd` images

| Offset | Field (all LE u32) |
|---|---|
| 0x00 | `bCoD`; 0x04 container version 1 |
| 0x08 | build date/time, 16 chars |
| 0x18 / 0x1C | model id (0x69) / software version (0x2398 = build 9112) |
| 0x20 | low 16 bits = HWID: 0xBD (also BL2's PersParam default) in B&W's Zeppelin Air, A5 and Z2 images; 0xBE in the Zeppelin Air with Lightning's (2.01.04, 2.04.35); 0xA7 in the A7's |
| 0x28 / 0x2C | header version (2 = 0xB0-byte header) / **header CRC32** (zlib, over the header with this field zeroed) |
| 0x30 + 0x20·i | sections {file offset, NAND address, size, CRC32, partition size}: 0 app (→ 0x240000), 1 factory CnE, 2 resources (→ 0xC40000; 0x90 + 4·i = the sizes of its containers, 8 words, 3 used) |
| 0xB0 | "DMP 3.x" |

No signature. Tools: `python3 tools/zeppelin bcd` (parse, check, extract), `mkbcd` (wrap a custom stage 2 with the
stock preloader, CnE and resources, recompute everything, pad sections to 4).

## PersParam (0x20000)
0x10-byte block header, "PersParam", then records `{mark (08080808 live, 0 superseded), record length, data length, 0,
key length 4, key (byte-reversed), data}`; updates are appended, the old record's mark cleared in place.
Factory records: CnEU 0x60000, CnES 0x20000, CnEA 0x40080, BLDb 0, HWID 0xBD, HWVS 0, GUID 0, MAC0 (8 zero bytes,
superseded), MAC1 0, BLDF "081008", **MAC0 (6 bytes, the unit's MAC, OUI EC:66:D1)**, DTCV 1. **The MAC is the only
per-unit value**; MAC0 = the Ethernet and Wi-Fi MAC on this unit. After 0x180 the stock app appends SAFE (every
boot) and the defaults of `persparam check` (POWB/POWG 0xFF, DUMS 0, MIDB/MAXB/MINB 9, MIDG/MAXG/MING 1, FWUP 0, CnET
0x1300000, SRNO "0"×18). `tools/zeppelin/recover/persparam.py --factory MAC` builds the block; byte-identical to
ours. From the stock app's shell: `persparam rd|wrb <key> 0x...` (`wr DTCV 1` stores 20 bytes of garbage: use `wrb`).

## User settings (0x60000)
One record per 512-byte sub-page: 16-bit header (bit 2 active, bit 3 = 1, bits 4-15 parameter id), NUL-terminated
string, CRC32 over (header with the active bit set + value + NUL). `python3 tools/zeppelin cneu` prints them with
secrets redacted. Factory = erased; BL2 replaces them on every update.

## Writing NAND from the stock app
`fburn listFlashAddr` (FlashId 0 = NAND), `fburn rd <id> <addr> <len>` (XModem-CRC out: `zeppelin fburn rd`)
[HW], `fburn bin|ebin <id> <addr> <len>` (asks "Are you sure [y/n]?", then XModem in, through the NAND driver with
ECC and bad-block skipping). The confirm reads the shell's own device and never sees an answer over telnet; over
UART1 a 128 KB write worked once in emulation. Over the module console (`zd1 console`) on the unit it is untested,
so **rewriting BL2 from a running image is not solved** (`docs/recovery/factory-reset.md`).
