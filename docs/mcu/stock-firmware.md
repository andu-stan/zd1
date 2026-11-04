# The stock MCU firmware (ZD1 2.04.05)

What B&W's MCU app does, as far as our firmware needs it. Source: static analysis of `blobs/mcu/zd1-2.04.05.hex`
(built "Dec 1 2014", avr32-gcc + newlib + ASF, source tree "iZound"; the Ghidra exports: `re/ghidra/setup.py
mcu-app`, then `re/gen/mcu-app/decomp_resolved.c` has literals resolved). Everything here is [RE] unless tagged; much of it was also exercised by
running the binary in the emulator, and where our firmware reproduces it and works on the unit it says [HW].
`FUN_x` = function at flash address x.

The same binary prints "2.04.05" on board variant 1 (model FP30570, ours) and "2.04.35" on variant 2 (FP35548);
the variant is ADC2 (PA23) at boot. Also embedded: DSP release v05.09, factory protocol v01.06, EEPROM version 0x0204.

Variant 2 is almost certainly the Zeppelin Air with Lightning connector (ZD1L), whose last release B&W calls 2.04.35
(March 2015, OTA `zd1lv20435.fw`, md5 d71a10be…: `downloads/bw-zeppelin-air-software/zeppelin-air-lightning/
SOURCES.md`). On variant 2 the binary reports model FP35548 and no video out over iAP (variant 1:
FP30570, video on), and only variant 2 honours the module's request to enter DFU (`docs/protocol/spi-registers.md`,
0x27). That doesn't bring the image over the network: the MCU then waits in USB DFU for a computer on the rear port
(`docs/mcu/bootloader-dfu.md`). The ZD1L's MCU app 2.04.35 (`ZD1L_v2-04-35.dmb` in the `.fw`) is a near-twin of this
binary, not the same bytes: the same size (0x80008000-0x800380F4), build date ("Dec 1 2014") and model strings, but
39132 bytes differ, almost all by exactly -4, most of them in 0x80028000-0x80037FFF (a data offset moved by 4). The
archive.org module NAND (item `20230115_20230115_1916`, a ZZ34088 board) fits this. Its module app and resources are
byte-identical to the 2.04.05 `app_DMP.bcd` sections 0 and 2, and its user settings record HostVersion "2.04.05" and
"2.04.35" alternately (19 records, last one 2.04.05). So one module image ran under MCUs that reported both versions.
The A7's 2.04.77 is the same March 2015 release: B&W's notes for 2.04.05 (`release_id=81`), 2.04.35 and 2.04.77 have
the same text. This binary has no A7 variant (both variants are Zeppelins with a dock); the A7's MCU app is
`AZ1_v2-04-77.dmb` in `a7v20477.fw`, another build of the same code ("Dec 12 2014", 191 KB, ~147k bytes differ).

A B&W OTA `.fw` (A7, ZD1L; the A5 and Z2 too) is BridgeCo's encryption wrapper, unencrypted, around a module image,
then the MCU part: a 32-byte header, u32 LE: 2 (the wrapper's version), the module image's length, 0 (encryption mode:
none), the MCU part's length, a checksum and a smaller sum (not yet checked), 0, 0; then the `.bcd` (`bCoD`, as
`app_DMP.bcd`); then the MCU part. BL2's web updater reads the wrapper (`docs/module/boot-chain.md`) [EMU]. On the A7
and the ZD1L the MCU part is a `.dmb`: a 24-byte header (`00 01 00 80`, a word, the app's length, 0x80000000 + a sum),
its name at +24, the app at +128, loaded at 0x80008000. The A5's and Z2's MCU part (63 KB, "B&WAZ2LITEON~",
"B&WMA1LITEON~") is another MCU, and their module images name a CX870. B&W's manifest marks ZD1 2.04.05 `usb` (desktop
app only) and ZD1L 2.04.35 and A7 2.04.77 `ota` (the iOS app too).

**The three models' module images** (`tools/zeppelin/fw/bcd.py`, `cne2txt.py`, `ffsres_extract.py`): the module app
(section 0) is byte-identical in ZD1 2.04.05, ZD1L 2.04.35 and A7 2.04.77. They differ in the header's HWID (0xBD,
0xBE, 0xA7), the factory CnE (`TCPIPTunnel`, the TCP-to-SPI-registers tunnel on port 50001: `REMOTE_REGISTERS` on the
ZD1, `OFF` on the others; the A7's names and its own certificate for the setup page) and the resources: only the
ZD1L's and A7's setup page has the "Prepare for Firmware" panel.

**B&W's update over the LAN** (the ZD1L's and A7's setup page, `scripts/device_base.js`) [EMU 2026-11-04, the stock
images, `zemu run` below]: `GET /1000/firmware_prepare.asp`, then
`/goform/formPrepareFirmwareUpdateHandler?FUOption=LAN&button=++++++Ok++++++` (redirects to `firmware_reboot.asp`,
which answers the speaker's IP). The module app prints "Sending control to host" (the request 0x11 = 0x27), the page
counts down 35 s and opens `http://IP/`, expecting BL2's web updater there. The MCU app:

| MCU app | board variant | says it is | on 0x27 |
|---|---|---|---|
| ZD1 2.04.05 | 1 | ZD1 | (not tried: per the code, nothing) |
| ZD1 2.04.05 | 2 | ZD1L | ISP_FORCE, the module held in reset, a watchdog reset: the bootloader |
| ZD1L 2.04.35 | 1 | ZD1 | nothing |
| ZD1L 2.04.35 | 2 | ZD1L | as 2.04.05 on variant 2 |
| A7 2.04.77 | the ZD1 board's ADC2 (0x100) | AZ1 | as 2.04.05 on variant 2 |

The ZD1's bootloader (0514, the one we have) then waits in USB DFU with the module off, so with it the LAN update
ends there. The ZD1L's and A7's bootloaders, which no `.fw` carries, must start the module into BL2 instead
[INF]; the A7's 2.00.28 was `a7v20028-bsl.fw` (not recovered). Each app writes its identity's version into the
EEPROM record (+6: 0x0005 for ZD1, 0x0035 for ZD1L, 0x0077 for the A7) and takes a record with another version as
invalid (its defaults). The A7 app boots on the ZD1 board model ("PSS AZ1", DSP firmware v01.00, finds its DSP, codec
and EEPROM, reaches RUN); the ZD1L app looks like 2.04.05 throughout (the -4 differences changed nothing seen).

Other ways a `.fw` might get in, from the module app's strings [RE, untried]: its own online check (`GetLatestFwUrl`,
`FirmwareIndexPage` = BridgeCo's dead `bridgeco-jukeblox.net/download/index.xml`), a USB stick
(`Firmware_update_prepare_for_usb.asp`), the iOS app over the dock (`JBLinkPacket_UpgradeFirmware`); its way to the
MCU besides 0x27: an XModem sender on register 0x2D ("XModem Host", 1030-byte frames). This MCU app's XModem receiver
and network-update task (`dmhBcoMgr_Nfu.c`, `voBcoMgrNetFwUpdateTask`) are dead code in 2.04.05 (spi-registers, HCR
0x2520); the ZD1L and A7 builds have them too. BCD images may be Blowfish-encrypted
(`FirmwareUpdate/BCDEncryption_BFKey = 'FEDCBA9876543210'`, IV `12345678`; not mandatory).

The runs: `python3 tools/zeppelin/fw/bwfw.py FW OUTDIR` cuts a `.fw` into `module.bcd` and a 256 KB MCU flash image,
then `zemu run OUTDIR/module.bcd --mcu OUTDIR/mcu_flash.bin` (variant 2: `--renode 'zd1:board Adc "2:0x300"'`; the
first boot's Wi-Fi setup crashes the module: `emu/README.md`, Recipes, Stock on both boards), the requests above with
`--fwd` and `--host`.

## Image layout

| Address | Content |
|---|---|
| 0x80004000 | jump to 0x80008000 |
| 0x80008000 | trampoline into crt0 0x80008024: SP = 0x10000, .data → SRAM 0x8-0x3630, .bss 0x3630-0x8250, board init 0x80026e2c, main 0x80026774 |
| 0x80031A00 | EVBA (exception vectors) |
| 0x80034AD0-0x800380F4 | .data load image (end of the app) |
| 0x80038100- | free flash (the stock patches put their hook here) |

## Start-up
1. Pins on the RC clock (PA02 low, PB02 low, PB15 low, PA19/PA22 low, PB11/PB17 high, PB03 high), then the CS2000
   init over bit-banged I2C (9 writes: `85←08, 82←00, 83←00, 86←00 0F 0D 84, 96←10, 97←00, 85←01, 80←99, F7←80`;
   bit 7 of the register byte = auto-increment).
2. Clocks: OSC0 → PLL0 ×11 ÷2 = 66 MHz, flash 1 WS (`FUN_80029784`). Watchdog CTRL 0x55001001 / 0xAA001001
   (~1.1 s), kicked constantly; TC channel 0 = the 1 kHz tick (IRQ 448).
3. Self-test (auth chip answers on TWI 0x10, else system error 6), flash checksum over 0x80000000-0x800FFFFF.
4. DSP, codec, CS2000 (below), settings from the EEPROM, then the module (`docs/protocol/sequences.md`).

## DSP (ADAU1445, TWI 0x38)
- Power: PB03 high, 10 ms, PB02 high, 100 ms, then the 24-step download (`FUN_80020980`), extracted to
  `re/mcu/extracted/dsp/` (`download_sequence.bin` = records `{u16 addr BE, u16 len BE, data}`): core control 0xE220/
  0xE228, **program RAM 0x2000 (6804 B)**, configuration 0xE000-0xE280, **parameter RAM 0x0000 (1600 B)**, 0xE21F,
  a zero-length write to 0x6000, start (0xE228 = 1, 0xE220 = 1, 0xE221 = 0, 0xE221 = 5). Our firmware replays it
  [HW].
- Parameters are 5.23 fixed point (1.0 = 0x00800000). **Safeload** (`FUN_8001fab0`): data words → 0x0001-0x0005,
  target address − 1 → 0x0006, word count → 0x0007 (triggers).

| Control | Parameter | Values |
|---|---|---|
| master volume (`FUN_8001fbc8`, every volume path ends here) | **0x4B** (0x0006 ← 0x4A) | index 0 = +10 dB … 70 = −60 dB, 71 = mute; coefficients at .data 0x0C5C [HW] |
| user step → index | table 0x80012F5C | 72 steps, index = 71 − step (plus an iPod volume 0-255) |
| AirPlay volume | | index = 0x47 − (v + 3)·70/240 for v = 0..0xF0 |
| bass | 0x3F-0x43 (one biquad), safeload of 5 words | 5 presets at 0x8001F49C ("Bass 0/+1/−1/−2/−3") |
| input select | 0x14 + registers E080/E002 | `docs/hardware/zd1-main-board.md` |
| S/PDIF lock | read 0xE0C9, 2nd byte bit 0 | |
| mains compensation | 0x11D, 0x11E, 0x121, 0x122, 0x175 | from ADC6 |
| driver mutes | tweeter 0x13C-0x13F, mid 0x145/0x146 + 0x149/0x14A, woofer 0x185/0x186 | |
| input level meter | 0x031 (read) | linear peak, 5.23, of the selected input before the volume stage; follows the signal (falls ~50 dB in 0.4 s after it stops); AUX with a source plugged in and paused -86 dBFS, music -66..-28 [HW 2026-10-27] |

Tables: `re/mcu/extracted/dsp/runtime_tables.txt`, `param_records_8001f5bc.txt`. The "soft mute" records stock
writes on some paths are input-path selectors, not a mute: `FUN_800200b4` writes the source gains 0x147/0x148/0x173/
0x174/0x18E/0x18F = 1.0 (`FUN_8002005c`, records +0x138..+0x160) and then +0x168..+0x1D0 (selector pairs 0xB9/0xBA,
0xBE/0xBF, 0xBB/0xBC, 0x11F/0x120, 0x123/0x124, 0x177/0x178, 0x126/0x127 set to 0 / 1.0); `FUN_80020214` writes
+0x1F8..+0x260 (the same pairs the other way round), then the source gain again (`FUN_800201c8`). Our firmware
writes none of them: with the download's defaults USB, AUX analog and the module play [HW].

## Codec (AD193x, TWI 0x04) and CS2000
- Init `FUN_80020db0`: reg0 = 0x19, 20 ms, reg0 = 0x18, poll reg1 bit 3 (PLL lock, needs MCLK from the CS2000 first;
  ~30 ms), reg2 = 0x02, reg4 = 0x01, reg5 = 0xC0, reg14 = 0x7E (0x72 for AUX), reg0 = 0x98. PB27 high during init only.
- **reg4 bit 0 = DAC master mute.** Stock clears it right after the amp unmute (`FUN_80013a68` → `FUN_80020d68`) and
  sets it on every source switch (`FUN_800138f0` → `FUN_80020d8c`) [HW].
- CS2000 ratios (12.20, 12 MHz reference): 0x000AEC33 = 32 kHz, 0x000F0D84 = 44.1 kHz, 0x0010624D = 48 kHz. Not
  reprogrammed for Wi-Fi (the module clocks the I2S).

## Amp
- PB09 = supply on (`FUN_8001c7c4`) / off (`FUN_8001c7ac`); ~2 s soft start; PB16 = unmute (`FUN_8001abf4(0)`) / mute
  (`FUN_8001abf4(1)`); then a 5-step volume ramp to the user volume (`FUN_800120ac`). PB09 and PB16 are driven only
  through these functions (via the GPIO helpers `FUN_8001bb7e` high / `FUN_8001bb98` low).
- The amp is on whenever a source is active (also during a pause: the source then feeds silence) and off in standby,
  sleep and ERROR. Standby does not power the module off; only sleep can.

## Modes, sources, keys
- Main modes (jump table 0x80026374): POWER_ON, STANDBY, SLEEP, SYNC, RUN, VOLT_DWN, ERROR, BCO UC, BCO IRQ,
  FACT_RST, BOOTING. RUN polls USB, inputs, the factory UART, volume, IR, the source manager (`FUN_800248bc`),
  AUX sense and the LED engine.
- **Boot source = the one stored in the EEPROM** (settings +0x18; copies at 0x00 and 0x28, checksum = sum of bytes
  4..0x27); no automatic switch to USB on VBUS. Source numbers: 0 iPod, 1 USB (green), 2 AUX (orange), 3 Wi-Fi
  (breathing red until the module is ready, then purple).
- **ON/OFF in RUN** (`FUN_80024198`): a press arms a 2 s hold timer; release before 2 s = "next source" (event
  0x10001000), the LED shows the candidate and the switch happens 1 s after the last press, stepping through the
  *available* sources in the order iPod, USB, Wi-Fi, AUX; only if at least 2 are available. Held 2 s = mute, then
  standby on release. Our firmware does the same [HW].
- **AirPlay auto-switch**: a module play-state `pl` on register 0x47 while idle switches to Wi-Fi (RUN) or wakes onto
  it (SLEEP). A bare RAOP stream without DACP never reports `pl` (the module shows "Now Playing" with play = empty).
- Standby: save settings, auth chip reset, USB off, PB09 low. Sleep: RC clock, wake on PA08; PA07 held at wake =
  factory reset request.
- Events (group << 28 | bits): 0x40000001 ON/OFF, 0x40000004 IR standby, 0x40000010 play/pause, 0x40000020 IR vol+,
  0x44000000 key vol+, 0x40000040 vol−, 0x40000200 release, 0x40000400/0x800 long press, 0x40002000 rear key,
  0x50000001/2 next/prev, 0x20000004 PC USB in, 0x30001000 USB audio activated. Event words at SRAM 0x2F98 + (ev >> 28)·4.
- SRAM globals for tracing (`ZD1_WATCH` in the emulator): 0x7D18 UI/key state, 0x2F98+ events, 0x7F98 settings copy.

## Stored data
- **EEPROM 24C02**: 0x60-byte settings record (version 0x0204) in copies A (0x00) and B (0x28), 16-byte pages. A blank
  EEPROM is fine: stock writes its default record.
- **User page**: stock's copy of the module's network setup (device name, DHCP/IP/mask/gateway/DNS/proxy, fixed PIN,
  `FUN_8001ced8`), "CooL" at 0x1F7 = valid, ISP word at 0x1FC. Stock rebuilds it from the module after a blank page
  (byte-identical to the original) [EMU]. It holds the Wi-Fi settings: per-unit data, keep it out of git.
- PM GPLP0 flags survive resets: 0x02000000 factory reset requested, 0x08000000 woke from sleep.

## Factory UART (USART1, 115200 8N1)
Frame `[len][op][params][checksum]`, checksum = 8-bit sum of all preceding bytes; opcodes 0x80 ("get version")
to 0x93 ("force watchdog reset"); reply `04 op status err checksum` (00 = accepted, FF = rejected). Sets the MAC,
fixed PIN and country code through the module (`docs/protocol/spi-registers.md`, register 0x45). Notes:
`re/mcu/work/bco_link_notes.txt` section 6. Init 0x8000f7c0, called when factory mode starts:
8N1, 115200 (57600 or 2400 in some modes), RX ISR 0x8000ea44. On this board USART1's pins carry the module's console
(`docs/hardware/wiring.md`).

## iPod dock (not in our firmware)
USB host + iAP over HID (SET_REPORT out, interrupt IN back, `[reportID][link byte][0x55 …]`), IDPS (0x38-0x3B) with
IdentifyDeviceLingoes (0x13) fallback, auth (0x14-0x19) against the chip at TWI 0x10 (registers 0x04 ID, 0x10 control,
0x11/0x12 signature, 0x20/0x21 challenge, 0x30/0x31 certificate), lingoes General, Simple Remote, Display Remote,
Accessory Equalizer (the bass presets), Digital Audio; then Apple's 500 mA charge request (vendor 0x40/0x40). Notes:
`re/mcu/work/usb_ipod_notes.txt`.
