# Reverse-engineering material

The raw material of the reverse engineering: dumps from the unit, the probe results, the scripts, and the deep-dive
notes that `docs/` summarises. **Start in `docs/`** for what is true now; come here for the
detail behind it. Where a note here and a doc in `docs/` disagree, the doc wins.

Binaries the analysis is of are in `blobs/` (bootloaders, stock images) and, regenerated from the vendor images,
`netmodule/extracted/` (gitignored: `python3 tools/zeppelin bcd`/`nand` rebuilds it). The decompiles, disassembly and
strings of the vendor images are not kept: `python3 -I re/ghidra/setup.py [IMAGE...]` (Ghidra: `nix develop .#re`)
rebuilds them from `blobs/` into `re/gen/IMAGE/` (gitignored), in minutes per image, about 20 for the module app:

| Image | What, in `re/gen/IMAGE/` |
|---|---|
| `mcu-app` | the stock MCU app (`image.bin`, below): `decomp_all.c`, `decomp_resolved.c`, `decomp_orphans.c`, `disasm.txt`, `functions.txt` |
| `mcu-updater`, `mcu-updater-win` | B&W's DFU updater (Mac `updater.dylib`, Windows `DFUflasher.exe`, from `downloads/`), decompiled: the source for `host/core/src/dfu.rs` |
| `module-app`, `module-pre` | the stock module app (KnOS) and its preloader, from `app_DMP.bcd`: `decomp_all.c`, `functions.txt`, `string_xrefs.txt` |
| `bl2`, `bl2-pre` | the module's BL2 and its preloader, from `bl2.bin`, the same exports |

Each also has `image.bin` (the input) and `strings.txt`.

## Layout

| Path | What |
|---|---|
| `mcu/work/bootloader/bl.dis` | the DFU bootloader disassembled (SRAM pc → flash 0x80000228 + pc − 0x4000) |
| `mcu/work/usb_ipod_notes.txt`, `bco_link_notes.txt` | iAP-over-USB and the module link, the long form behind `docs/mcu/` and `docs/protocol/` |
| `mcu/extracted/dsp/` | the DSP download sequence (`download_sequence.bin`) and the runtime parameter tables |
| `mcu/extracted/led_patterns.txt` | the 12 colours / 40 LED patterns |
| `netmodule/bl1/` | the SRAM capture with BL1 decrypted (`README.md` has the boot logic) |
| `netmodule/spi_register_map.txt`, `nand_layout.txt`, `nand_blockmap.txt`, `cne_attr.txt` | the module register table, NAND map, factory CnE |
| `netmodule/wlan_mib_schema.txt`, `WIFI_DRIVER.md`, `SULUDMA.md` | the Wi-Fi MIB and driver under KnOS, the DMA engine's registers and contexts |
| `netmodule/NOKIA_HD1.md` | the Nokia HD-1 Linux analysis (the basis for `docs/module/linux.md`) |
| `netmodule/jtag/` | register dumps of the peripheral window under our Linux and under BL2, over JTAG (`docs/module/jtag.md`) |
| `netmodule/sol-probes/`, `netmodule/sol_dump.py` | the Warp/Sol probes and their raw results (its `README.md`), a Sol program printer (`docs/module/warp.md`) |
| `netmodule/inject/` | a payload runner for the stock module app, through its shell (its `README.md`) |
| `netmodule/shell-dumps/`, `cr870-live-block-ids.txt` | the live KnOS-shell command dumps, live block IDs |
| `scripts/` | the analysis scripts (below) |

## The MCU image (`re/gen/mcu-app/`)
The stock app 2.04.05 (AT32UC3A1256, AVR32, big-endian) as a raw image, `image.bin` (the stock HEX from 0x80000000,
as `setup.py` cuts it), base 0x80000000: code 0x80008000-0x80031884, constants from 0x80031a00, the `.data` load
image at 0x80034ad0 (to RAM 0x8-0x3630), `.bss` 0x3630-0x8250, the stack's top 0x10000.

`decomp_resolved.c` has the literal-pool loads resolved: `FUN_xxxxxxxx` is a call target, `"..."` a
string, `RAM_xxxx` a pointer to SRAM 0xXXXX (`RAM_xxxx/*"str"*/` a string in `.data`), peripheral registers named
(`USART1_0x8`, `TWI_0x34`, `&SPI0`; `DAT_fffe05xx`, the USBB host registers, are not). The rest: `decomp_all.c` (not
resolved), `decomp_orphans.c` (functions only reached through pointers, resolved), `disasm.txt`, `functions.txt`;
`re/scripts/fnnames.py` lists the strings each function references. The files are ~1 MB: print one function
(`python3 -I re/scripts/fn.py re/gen/mcu-app/decomp_resolved.c ADDR [MAXLINES]`, ADDR in hex without 0x) or bound the
output; `re/scripts/xref.py re/gen/mcu-app/image.bin "string"|0xADDR` finds the 32-bit pointers to a string or
address in flash. A function that returns a bool in the flags shows `extraout_Z`: read it as the
return value.

| Address | Function |
|---|---|
| 8001c500 | `printf` (the debug console, USART0 on PA00/PA01) |
| 8000f988, 8000e3f8 | `delay_ms(ms)`, the watchdog kick / idle |
| 8000f9f0, 8000f954 | `timer_start(id, ms)`, `timer_expired(id)` |
| 8002c43c | `memcpy` |
| 80029384, 800293b4 | `event_set(group << 28 \| bits)`, `event_test_and_clear` |
| 8001ba7c, 8001bad0 | `gpio_enable_module_pin(pin, func)` (0 = A .. 3 = D), `gpio_enable_module(table {u8 pin, u8 func}[], n)` |
| 8001bb2a / 8001bbb2, 8001bb40, 8001bb54, 8001bb68 | GPIO input, pull-up on, pull-up off, `gpio_get(pin)` |
| 8001bb7e, 8001bb98 | `gpio_set_high(pin)`, `gpio_set_low(pin)` |
| 8001bbc8, 8001bc10, 8001bc24, 8001bc3c | GPIO interrupt on (mode 0 change, 1 rise, 2 fall), off, flag, clear |
| 80019160, 80019280 | `twi_master_write(&TWI, pkg)`, `twi_master_read` (ASF's `twi_package`: chip, addr, addr_len, buf, len) |
| 8001a0f8, 8001a14e, 8001a180, 8001a19e | SPI0 select(cs), unselect, write(byte), read(*out) (the names not checked) |
| 8001b958 | the CS2000-CP clock multiplier (I2C 0x9C, bit-banged on PB13/PB14) |
| 800232d8, 800231c8, 800230e8 | the SSC (I2S to the DSP) driver, fed by PDCA channel 0; the mixer table at 0x80018104 |
| 8000dd58, 8000dcdc, 8000da88 | the module's attach and reset (PA04, PA17) |

GPIO pin n is port n >> 5, bit n & 31 (PA 0-30, PB 32-63, PC 64-69). On the I2C bus: 0x38 the ADAU1445 DSP (2-byte
sub-address), 0x04 the AD193x codec, 0x10 the Apple authentication coprocessor, 0x50 the EEPROM. SPI0 (PA13 SCK,
PA11 MISO, PA12 MOSI, PA09 NPCS2) is the link to the DM870 network module. USART0 is the debug console; USART1
(PA05/PA06, 115200) is the "factory mode UART", opened only in factory mode, and on this board the module's console.
The SSC (PA14-PA16 TX, PA18 RX clock) carries USB audio to the DSP. USB is the USBB at 0xFFFE0000; the device
descriptor at 0x8002a8ec (VID 0x19B5, PID 0x0005: UAC1 16-bit stereo at 32/44.1/48 kHz on iso OUT 0x05, HID consumer
control on 0x81); host-mode code for the dock's iPod (PA22 toggled in 8001b9cc, maybe VBUS; PA21 pulled up and read by
the ADC in 800146e4). The module link is B&W's `dmhBcoMgr` component (`..\src\iZound\comps\dmhBcoMgr\src\*.c`):
SPI block writes, HCR/DRC codes, XModem, PersParam, CnE, DCP (AirPlay/DLNA), network firmware update, site survey,
WPS.

## Scripts (`re/scripts/`, run with `python3 -I`)
- Ghidra headless: `re/ghidra/setup.py` cuts each image from its blob and drives `analyzeHeadless` (import at the
  right base with the right processor, a pre-script for the memory map, post-scripts to decompile and export); the
  per-image Java is in `re/ghidra/scripts/` (`ZdPre`/`ZdDump`/`OrphanDec` = MCU, `NetPre`/`NetDump` = module, `Bl2*` =
  BL2, `UpdDump` = the DFU updater; `AddrDec` decompiles chosen MCU addresses, on a copy of the mcu-app project).
- Decompile post-processing: `resolve_decomp.py`, `fn.py`, `fnnames.py`, `fnof.py`, `xref.py`, `callsites.py`,
  `dis.py`, `ramdump.py`, `extract_dsp.py` (the MCU image, through `avr32img.py`: flash words, strings, the `.data`
  mapping of RAM 0x8-0x3630); `dump_regtable.py` (the module app).
- The runnable tools graduated to `tools/zeppelin/` (bcd/nand/cne parsers, mkbcd, the Intel HEX reader, the kernel's kallsyms).

## Privacy
`netmodule/extracted/` (gitignored) and the archive.org NAND dump hold a user-settings area with the Wi-Fi passphrase;
`python3 tools/zeppelin cneu` prints it redacted. Per-unit data lives in `units/` (gitignored). The archive.org dump
is another unit with bit rot: a structural reference only, not a source for this unit's data.
