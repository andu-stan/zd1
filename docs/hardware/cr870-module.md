# The CR870 network module

The network side of the Zeppelin Air is a plug-in **SMSC/BridgeCo CR870-2C** "JukeBlox" module: a **DM870** SoC
("Sulu", ARM926EJ-S at 240 MHz, little-endian) with 32 MB SDRAM, 128 MB NAND (Hynix HY27U1G8F2B: 2 KB pages, 64 B
spare, 128 KB blocks), an on-chip 802.11b/g MAC ("WILD") with a T6201 RF chip, Ethernet (Cadence MACB on the DM870;
the SMSC LAN8700 PHY sits behind module GPIO-12), USB host, and a 120-pin board-to-board connector (J2) to the host
board. The same module or the bare DM870 is in other AirPlay 1 speakers of the time (Philips DS9800W, Klipsch G-17
Air, Bose SoundLink Air, Monitor Audio ASB-2, iHome iW1/iW2, ...; archive.org `bridgeco-jukeblox-firmware`, its `INDEX.md`), which is why the
emulator's `cr870` machine and the module tools don't assume the Zeppelin.

What runs on it: `docs/module/` (boot chain, KnOS, memory map, Linux, JTAG, Warp).

Measured live [HW]: ARM 240 MHz, SDRAM 160 MHz, audio PLL0 22.579 MHz; audio out = AV port 2, **I2S master, 512·fs
MCLK, left-justified, 24-bit, 44.1 kHz** (the datasheet's 256·fs is the generic default).

## The datasheet
"JukeBlox Networked Media Module – CR Series Datasheet", SMSC, Rev 3.8 (2012-01-11), 34 pages, from McIntosh's FCC
filing for its CR870-2Q (viewer: https://manuals.plus/m/17af2876918f103b6d07a0882231b00256e96b2c7daac6107134788959b37d64,
append `.pdf` for the PDF; behind a Cloudflare challenge, open it in a browser). Rev 3.6 is in B&O's FCC filing
TTUCR870-2Q (`fcc/TTUCR870-2Q_2011-11-11_Users-Manual_1580250.pdf` below). Every revision from v2.5 (Feb 2010, LITE-ON's filing
PPQ-CR8702C for this very module, CR870-2C) to 3.8, the **DM870A signal table** (LITE-ON's approval sheet for the
D&M CX870-3B, 43 pages: every J2 pin with its DM870A ball, GPIO number and function), BridgeCo's "Hardware Guide DM870
Core-2" and the internal photos of the CR870-2C and the Nokia HD-1 are in archive.org item `bridgeco-jukeblox-documents`, `fcc/`
(53 FCC exhibits, `fcc/SOURCES.md`; fetched 2026-10-23 from apps.fcc.gov; `python3 tools/zeppelin downloads fetch
bridgeco-jukeblox-documents/fcc`).

The Zeppelin Air uses a **CR870-2C**: DM870 + T6201 RF, Wi-Fi + Ethernet + USB, 32 MB SDRAM, no LCD, PIFA antenna,
no top shield.

Never public, only on the BridgeCo customer portal: `BridgeCo_JB2x - DeviceControlProtocol_Registers_v_2_1.pdf` (the
SPI host protocol; reconstructed from code instead: `docs/protocol/spi-registers.md`), `dat_DM870_16_datasheet.pdf` /
DM870A datasheet rev 1.3 (chip timing and registers), `A EVMCE2-4_schematic.pdf`, `CR Module USB_V1.0.pdf`, `CR
Module Ethernet_V1.0.pdf` (example motherboards).

## Connectors
The module carries the *female* halves; the product board needs Kyocera 5046-series 0.5 mm plugs (datasheet 3.6,
section 8.2):
- **J2 "Media"**: 120-pin, 2 × 60, plug `14-5046-120-145-829+`.
- **J1 "LCD"**: 30-pin, 2 × 15, plug `14-5046-030-145-829+`. Unused on the 2C.

Kyocera's numbering: `14-5046-...` = plug, `24-5046-...` = receptacle, then positions, a stacking-height code, the
plating (`829+` gold, `868+` gold over nickel). The 120-way receptacle (`24-5046-120-600-868+`) is in production
(Digi-Key); the 120-way plugs are listed obsolete at brokers (e.g. `145046120645829+`, 3.8 mm). No breakout or
interposer exists off the shelf (2026-10-23). Probing is easier on the B&W carrier board's traces and test pads.

## Power and reset (datasheet)
- Supplies: 3.3 V I/O; 1.2 V core, external; 1.9 V for the RF section, generated on the module (leave the
  VIN_OUTSIDE pins open).
- Sequencing: 3.3 V first, with NRESET_MOD held low; 1.2 V in range within 300 ms of 3.3 V and never above
  3.3 V + 0.5 V; supplies stable for at least 2 ms before NRESET_MOD is released; on power-down 3.3 V stays above 1.2 V.
- NRESET_MOD: active low, 10 kΩ pull-up on the module.
- Typical power: about 1.75 W (3.3 V) + 0.6 W (1.2 V) with WLAN active.

## J2 pins that matter

| Signal | J2 pin | Dir (from DM870) | Function |
|---|---|---|---|
| SPI_DOUT | 25 | O | SPI data DM870 → host (host MISO) |
| SPI_DIN | 27 | I | SPI data host → DM870 (host MOSI) |
| SPI_CLK | 29 | I | SPI clock from host; datasheet: max 2 MHz recommended, typical 1 MHz |
| SPI_NCS0 | 30 | I | SPI chip select from host |
| PDOUT1 = SPI_REQ (GPIO-06) | 64 | O | high = message waiting, the host should read the changed registers; low = buffer empty |
| NRESET_MOD | 34 | I | module reset, active low |
| NPD_RF | 119 | I | RF power-down, active low (pull-up) |
| PDOUT0 (GPIO-04) | 68 | I | **factory reset** strap, active high, sampled only at boot (internal pull-down) |
| GPIO-12 (AV3CLK) | 72 | O | ETH NRESET of the PHY on the product board (low = reset) [HW] |
| VCO0 (GPIO-05) | 70 | I | IR input (Schmitt trigger); pull to 3.3 V when a host controller is used |
| NCS3 (GPIO-17) | 48 | I | **BIST activate**, low-active: invokes the production built-in self test at boot (internal pull-up). Reads low on this carrier with its pad released and the pull-up off (BL2 and our OS) [HW]; the level at reset and what the boot chain does with it are not analysed |
| A22 (GPIO-18) | 71 | I | "IRQ input" in the datasheet's J2 table; high here, unused |
| VCO1 (GPIO-07) | 66 | IO | not assigned (the Nokia kernel takes an IRQ on it: `GPIO_X17_P31`) |
| A23 (GPIO-16), NWAIT (GPIO-15) | 69, 117 | IO | not assigned |
| MIICRS / MIICOL / MIITXER (GPIO-00/01/02) | 104 / 106 / 108 | IO | not assigned |
| SSMD4-7 (GPIO-08..11) | 38, 43, 36, 41 | IO | the DM870's **SSM** port (SD card / shift-register extended GPIO; "SD card firmware is not supported in JB2"); as GPIOs 8-11 (the stock app's second I2C on 8/10, Nokia's shift-register SPI). The SSM block is at 0xB1004000 |
| AV2DATA0 | 101 | O | **I2S / left-justified audio out**, main L/R |
| AV2CTRL0 | 93 | O | LRCK (Fs, up to 192 kHz) |
| AV2CTRL1 | 86 | O | MCLK (datasheet: 256·Fs; this product: 512·Fs) |
| AV2CLK | 90 | O | SCLK = 64·Fs (up to 32-bit slots) |
| AV2DATA2 / AV2DATA3 | 97 / 95 | I | I2S inputs (A/D), unused |
| AV4DATA1 | 94 | O | S/PDIF out (up to 192 kHz) |
| AV3CTRL1 / AV3CTRL0 (GPIO-14/13) | 73 / 75 | IO | bit-banged I2C SDA/SCL, ≤ 400 kHz, needs external 4.7 kΩ pull-ups: the module's Apple auth chip |
| RXD1 / TXD1 | 35 / 37 | I / O | **console UART1** (3.3 V) |
| RXD0 / TXD0 | 33 / 31 | I / O | UART0, optionally iPod control |
| TMS / TCK / TDI / TDO | 22 / 24 / 26 / 28 | I/O | DM870 JTAG (balls B20 / A20 / B19 / A19), "leave open". **No TRST, RTCK or JTAG-enable on J2**; pins 21/23 are GND (checked against rev 3.6, 2026-10-23) |
| USB_DN / USB_DP | 55 / 57 | – | USB 2.0 (host) |
| NCS2 (GPIO-19) | 50 | I | iPod NDETECT (high here, nothing docked) |
| MIITXCLK (GPIO-03) | 110 | – | iPod accessory power |

Pin numbering: odd pins are the left column, even the right. The GPIO rows were checked against the DM870A signal
table and the v2.5 datasheet's J2 table (2026-10-23); the rest is from the extracted text of rev 3.6. Re-check
against the PDF and the board before soldering.

## On this product
- **The DM870 is the I2S clock master** (MCLK, BCLK, LRCK), at 512·fs [HW]. Anything replacing the module's audio
  has to drive the clocks too.
- **SPI**: a plain SPI slave plus the level-type request line (GPIO 6). Stock's MCU clocks it at ~4.4 MHz (SCBR 15
  at 66 MHz) [RE]; our firmware runs 3 MHz against the stock app, BL2 and our OS without errors, and 800 kHz behaves
  the same [HW].
- **UART1** (pins 35/37) is the console of BL1, BL2, the stock app's shell and Linux; it is wired to the MCU's
  USART1 (PA06 → RXD1, TXD1 → PA05) [HW 2026-10-23]. No RTS/CTS on J2.
- **UART speed** [HW 2026-10-26, our OS, `stty` on ttyS1, the MCU's USART1 set by `zd1 poke -w 0xFFFF1820`]:
  the UART clock is ~80 MHz (divisor 0x56A at 115200; baud = 2·clk/(div + 2)), so 1 and 2 Mbaud are exact.
  1 Mbaud: module → Mac 512 KB md5-identical at 95 KB/s (the line's rate); Mac → module 99.92 % (421 bytes of 512 KB lost,
  all of it above the UART: the driver counted every byte, Linux's 4 KB tty buffer overflows with no flow control).
  2 Mbaud: the module sends at 182 KB/s and receives everything at the driver; our MCU is the limit (one interrupt per
  byte and byte-wise USB writes: 1/3 of the module's output arrived, the Mac's side capped at 99 KB/s).
- **Factory-reset strap** (GPIO-04, PDOUT0, pin 68) is wired to MCU PB01 [HW 2026-10-23]. BL1 doesn't read it (its
  rescue-pin check is a stub) [RE]; BL2 has a rescue-pin path [RE] that in emulation logs "Could not retrieve GPIO
  index for rescue pin" [EMU]. What BL2 does with it on hardware is untested.
- **The I2C bus** on GPIO-13/14 is the module's own bit-banged bus for its Apple auth chip, the only device on it: a
  scan of 0x08-0x77 from our OS answers at 0x10 alone (after one NAK while the chip wakes); registers 0-8 read device
  version 3 (an Authentication Coprocessor 2.0B), firmware 1, protocol 2.0, device ID 0x200, error code 0
  [HW 2026-10-25]. KnOS's TVService also probes a video encoder at 0x44, which isn't fitted (NAK).
- **JTAG** reaches the carrier board's four test pads (`docs/module/jtag.md`).
