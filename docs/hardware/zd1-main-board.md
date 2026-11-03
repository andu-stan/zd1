# Main board (ZD1): MCU, audio chain, controls

The main board is the system controller. Everything on it hangs off one **Atmel AT32UC3A1256** (AVR32 UC3A,
32-bit, **big-endian**, 256 KB flash, 64 KB SRAM). Evidence: the stock firmware's code (`re/mcu/`), our firmware
running on the unit, live register reads from its console. Tags: `docs/README.md`.

## Chips

| Part | Bus / address | Role |
|---|---|---|
| AT32UC3A1256 | — | system controller: sources, volume, amp, LED, keys, IR, USB, the network module |
| ADAU1445 DSP | TWI 0x38 (2-byte sub-address) | all audio processing: input routing, volume (software only), bass, driver EQ/limits |
| AD193x codec (AD1937 assumed) | TWI 0x04 (1-byte) | DAC to the amps, ADC for AUX analog; S/PDIF receive for AUX optical |
| CS2000-CP clock synthesiser | bit-banged I2C on PB13 SCL / PB14 SDA, 8-bit 0x9C | MCLK = 256·fs for USB/AUX sources, from a 12 MHz reference (GCLK2 on PB30) |
| 24C02 EEPROM | TWI 0x50 | stock settings (A/B copies at 0x00/0x28), our settings at 0x80 |
| Apple auth coprocessor 2.0B | TWI 0x10, reset PA02 | iPod dock (iAP) only; the network module has its own |
| Class-D amp + its SMPS | PB09 supply enable, PB16 unmute | the power stage (see Power) |

## MCU pin map
GPIO numbering in our tools (`zd1 peek/poke`, the log): `pin = port·32 + bit` (PB09 = 41).

| Pin | Function | Tag |
|---|---|---|
| PA00 / PA01 | USART0 RX/TX: stock debug log, 115200 | RE |
| PA02 | auth chip nRESET (high = run) | RE |
| PA03 | IR receiver (RC6), GPIO edge interrupt | RE |
| PA04 | network module power (high = on) | HW |
| PA05 / PA06 | USART1 RXD / TXD = **the network module's console** (its TXD1 / RXD1, J2 37 / 35): BL1, BL2, the app's shell, Linux's ttyS1. 115200 8N1. Both directions measured 2026-10-23; the host link carries it (`Console`, `Module ESC`) | HW |
| PA07 | rear button, active low (setup / factory reset) | RE |
| PA08 | ON/OFF key, active low; EIC7 wake; the bootloader's DFU pin on a cold boot | HW |
| PA09 / PA11 / PA12 / PA13 | SPI0 NPCS2 / MISO / MOSI / SCK to the network module | HW |
| PA14 / PA15 / PA16 / PA18 | SSC TX_FS / TX_CLK / TX_DATA / RX_CLK: I2S to the DSP (USB audio); the bit clock comes in on RX_CLK | HW |
| PA17 | network module nRESET (low = reset; input while the module is off) | HW |
| PA19, PA22, PB11, PB12, PB17 | external USB routing / dock power (table below) | RE logic, INF meaning |
| PA20 | network module request line (high = data queued), level-polled | HW |
| PA21 / PA23 | ADC0 USB ID / board revision; ADC2 board variant (≥ 0x200 = variant 2, "2.04.35") | RE |
| PA24 | rear USB VBUS ("PC connected"), high = present | RE |
| PA25 / PA26 / PA27 | ADC4 / ADC5 / ADC6 (see Protections) | HW |
| PA29 / PA30 | TWI SDA / SCL, 400 kHz | HW |
| PB00 | AUX jack inserted, **active low** | HW |
| PB01 | **the module's factory-reset strap** (PDOUT0 = module GPIO 4, J2 pin 68, active high, sampled at boot): module GPIO 4 follows it (2026-10-23). Stock drives it low. BL1 doesn't read it [RE]; BL2 has a rescue-pin path [RE], untried on the unit. B&W's recovery-utility image holds it high from 3.3 s before module power-on [EMU] | HW |
| PB02 / PB03 | DSP nRESET / DSP power enable (the TWI bus hangs while PB03 is low) | HW |
| PB04 | fault input, active low (error 4) | INF |
| PB05 / PB06 | Vol− / Vol+ keys, active low | HW |
| PB09 | amp SMPS supply enable | HW |
| PB13 / PB14 | CS2000 bit-banged I2C SCL / SDA | HW |
| PB15 | CS2000 power/reset (driven low early by stock and the bootloader) | INF, weak |
| PB16 | amp unmute (high = unmuted) | HW |
| PB18 | amp fault/clip input (error 2); reads low (fault) while the amp supply is off and drained (ADC6 0x010 after a mains power-up), high once it is up: our protection, like stock's, looks at it only with the amp on [HW 2026-10-27] | INF |
| PB19 | driven low at boot and in sleep (stock); ours does the same | INF, weak |
| PB20 / PB21 / PB22 | PWM 1/2/3: RGB LED, **R, G, B** | HW |
| PB23, PB28, PB31 | status input / factory mode / boot strap | INF, guess |
| PB27 | codec mute: must be **low** to play (high only during codec init) | HW |
| PB30 | GCLK2 output, 12 MHz CS2000 reference | HW |

Stock opens USART1 only when factory mode starts (init 0x8000f7c0) [RE]; a normal stock boot configures no USART
pins through the GPIO controller [EMU: the MCU trace of `zemu run stock --mcu stock`].

## Clocks
- OSC0 = 12 MHz. Stock: PLL0 → **66 MHz** CPU and buses, flash 1 wait state. Ours: PLL0 → **48 MHz**,
  shared with USB (ASF). [HW]
- The DFU bootloader hands the app over running on the ~115 kHz RC oscillator. [RE]
- A cold boot works only if the app sets its early pins on the RC clock first, then OSC0, then PLL0 (the order the
  bootloader and stock use; switching RC → PLL0 directly left the board dark on a plain power-up). [HW]
- GCLK2 = OSC1/4 on PB30 is the CS2000 reference (stock's GCCTRL0 0x005, GCCTRL2 0x115). [HW]

## Audio chain
```
USB (PC) ── MCU SSC I2S ──┐
module AV port 2 I2S ─────┤  (module is I2S master: 512·fs, left-justified, 24-bit, 44.1 kHz)
AUX analog ── codec ADC ──┼──► ADAU1445 ──► AD193x DAC ──► class-D amp ──► drivers
AUX optical ── codec ─────┘     (input select: param 0x14 + registers E080/E002)
```
| Source | DSP routing (stock) | Clock |
|---|---|---|
| USB | 0x14 = 0, E080 = 0x0000, E002 = 0x0800 | CS2000 MCLK; the I2S bit clock comes back into the MCU on RX_CLK (PA18) |
| Wi-Fi (module) | 0x14 = 0, E080 = 0x0002, E002 = 0x0908 | the module drives the I2S clocks; the CS2000 is not reprogrammed |
| AUX analog | 0x14 = 2 | CS2000 |
| AUX optical (mini-TOSLINK in the AUX jack) | 0x14 = 1; lock bit = 2nd byte bit 0 of DSP 0xE0C9 | recovered |
| iPod dock | digital over USB host + SSC; old analog iPods probably through the AUX ADC (two dock cables, one shielded to the back board) | INF |

USB, Wi-Fi and AUX analog play on the unit with our firmware [HW]; AUX optical and the dock are untested.
- **The module's input is 24 bits wide**: its serial input mode, E002 = 0x0908 (USB's 0x0800 too), has the word
  length field [7:6] at 00 = 24 bits [RE: the ADAU1445's Serial Input Port Modes registers, 0xE000-0xE008, from a
  datasheet mirror; it matches the module's 24-bit left-justified I2S]. The DSP's core runs at the CS2000's rate and
  the module's input reaches it through an ASRC: a 48 kHz stream from the module plays at the right pitch with the
  CS2000 left at 44.1 kHz [HW 2026-10-28], so another input rate is converted to the core's [INF; E080.. would be the
  ASRCs' input selects, stock's E080 = 2 for the module].
- **Volume is DSP software only** (no analogue attenuator): master volume = param **0x4B**, written by safeload
  (0x0001 ← value, 0x0006 ← param − 1 = 0x4A, 0x0007 ← word count). Index 0 = +10 dB … 70 = −60 dB, **71 = mute**,
  coefficients from stock's table. User step s (0..71) → index 71 − s. [HW]
- **Codec DAC master mute** = codec reg 4 bit 0. The codec init leaves it set; stock clears it right after the amp
  unmute and sets it on every source switch. Missing this = silence. [HW]
- Bring-up order that works: CS2000 first (it makes the codec's MCLK), then DSP power/reset/download, then codec
  (PLL lock ~30 ms after the kick). Details: `docs/mcu/stock-firmware.md`. [HW]

## Power
- Mains → SMPS → logic rails, plus a **separate amp supply gated by PB09**; ~2 s soft start before PB16 may unmute.
  The amp rails are ±17 V (measured; the traced secondary's V+ / V-): ~28 W of sine per channel into 4 Ω, B&W's
  4 x 25 W. ADC6 follows the rail: ~0x010-0x02E off, ~0x3D5 on, 96 % of full scale, so more a "supply up" than a
  measurement (the divider is unknown; stock never reads ADC6; ours shows it in `diag`). [HW]
- The MCU and the network module share the 5 V side; a module power-up with stock timing does not brown out the
  MCU. [HW]
- **This unit's amp supply is fragile**: it killed the PSU once (repaired: new diodes and all the caps) and runs
  hot. Our firmware switches the amp off after 5 min of silence and caps the volume when ADC4 or ADC5 runs low (the
  limit a guess so far: `docs/backlog.md`); `zd1 amp deny` locks it. Agents keep amp-on time short.
- Schematic photos of the SMPS: `img/smps-schematic.jpg`, `img/smps-primary.jpg`.

## Protections (stock thresholds, also in our firmware)

| Error | Condition | Notes |
|---|---|---|
| 1 | ADC5 < 0x1FF for 9 samples at 500 ms in a row (one at or over 0x1FF resets the count) [RE `FUN_8001aa0c`]; ours counts a conversion that times out as under | temperature: ADC5 **falls as the board warms** (0x398 cold, 0x359 after 15 min amp-on, 0x302 after a long day) [HW] |
| 2 | PB18 low > 79 % of the time for 3 s | amp fault/clip |
| 3 | ADC4 < 0x1BC for 9 samples at 500 ms in a row [RE `FUN_8001c6a0`; ours the same, timeouts too] | a second temperature [INF]: stock's check is ADC5's, ADC4 falls as the board warms (0x37F cold, 0x2D9 playing) and stays low with the amp off; below |
| 4 | PB04 low | fault |
| 5 / 10 / 11 | network module never ready / lost after ready / lost after the first drain | `docs/protocol/spi-registers.md` |
| 6 | self-test (e.g. no auth chip answering) | |
| 7 | module VersionImage without "9112" | |

**ADC5 in °C** [INF]: read as an NTC to ground under a pull-up, ratiometric to the ADC's reference (it falls as it
warms). The cold point [HW 2026-10-27]: 0x398 right after a power-up from 62 min unplugged, the room at 26-26.5 °C
(samples over the next 2 min 0x391-0x39C: the ADC's noise is about ±6 counts, ±1.5 °C). The NTC is then ~8.9x the
pull-up at 26 °C: a 100 k NTC under 10 k would read 0x39D. B = 4250 is the working value, the usual one for a 100 k
SMD NTC (Murata NCP15/18WF104: B25/50 4250, ~4300 to 85 °C); not measured: it needs a cold reading in a room clearly
warmer or cooler. With it: 0x38F (board up, amp off) 28 °C; 0x35E (amp on a few minutes, silent) 37; 0x338 (playing)
43; 0x302 (the hottest seen) 51; 0x2A0 / 0x280 (our soft limit clears / caps) 63 / 66; 0x1FF (stock's trip) 81
(86 with B = 3950). The older cold reading 0x3A1 was a room of ~24 °C. There are no heatsinks (seen inside): the amps
shed their heat into the board copper, so this is a board temperature at the sensor, which hasn't been found (not on
the traced SMPS secondary).

**ADC4 in °C** [INF]: the same model with its own cold point (0x37F at the same power-up; the NTC ~7.0x its pull-up)
puts stock's trip 0x1BC at 82 °C, next to ADC5's 81 °C: two thermistors with one cutoff. ADC4 runs 7-9 °C hotter
than ADC5 (2026-10-27: amp on ~5 min silent 45 vs 37 °C; playing AUX 50 vs 43; after the flash with the amp off 36 vs
28), so it would reach the trip first. Our soft limit caps the volume when either reaches ~66 °C (ADC5 0x280, ADC4 0x248).

An error mutes, drops PB09, blinks red and enters the ERROR mode. [RE]

## USB
- **Device** (to a PC): stock = 19B5:0005 "Bowers & Wilkins / Zeppelin Air", UAC1 (feature unit 0..+16 dB, PCM16
  48/44.1/32 kHz, iso adaptive OUT EP 0x05) + HID consumer control [RE]. Ours = 03EB:2FE0 with stock's strings and
  serial "ZD1": UAC1 (with a status interrupt) + HID + a vendor interface (the host link, the module's console, the
  bulk pipe) [HW];
  layout in `mcu/README.md`, Design notes.
- **Host** (to a docked iPod): iAP over USB HID, auth chip at TWI 0x10. Not implemented in our firmware.
- Host vs device is **software-forced** with an external mux (stock `FUN_80013f98`) [RE logic]:

| Request | PB11/PB12 | PA19 | PA22 | USB | When |
|---|---|---|---|---|---|
| 1 | H/H | L | L | host | iPod, AUX, Wi-Fi |
| 2 | H/L | L | H | device | PC |
| 3 | L/L | H | L | off | sleep (variant 1) |
| 4 | H/L | L | H | off | standby |

PB17 pulses high on each switch. The DFU bootloader leaves PB11 = H, PB12/PA19/PA22 = L, PB17 = H; our firmware keeps
that (overriding it with table values once left USB dead) [HW].

## LED, keys, IR
- RGB LED on PWM 1-3 (R, G, B; CMR 0x20A, CPRD 400) [HW]. 12 colours and 40 patterns in stock
  (`re/mcu/extracted/led_patterns.txt`): dim red = standby, bright red = sleep, green = USB, blue = dock, orange = AUX,
  purple = AirPlay, yellow = iPod sync, white = firmware update; fast flash in the source colour = volume change, fast
  red = volume at a limit, slow purple = no network, slow yellow = wireless setup, breathing red = booting, red blink
  = error.
- The LED measured [HW 2026-10-27; an iPhone in Apple Log filming a duty sequence driven
  through the firmware; the model in `mcu/tools/led_model.json`]: light linear in PWM duty and additive (full
  white = the sum of its channels, 0.999). Chromaticity (CIE xy, as the camera sees it) red 0.609/0.296, green
  0.280/0.502, blue 0.177/0.124; at full duty red gives ~1/3 of green's light, blue a lot (the camera overrates blue
  against the eye, but the eye agrees blue dominates). Equal duty is a bluish white (0.235/0.204); a neutral white
  needs duties ~334 / 400 / 67. So any colour with some blue drifts to blue-white: a violet couldn't be told from a
  sky blue, a lavender looked white. Our palette is fitted to this (`mcu/README.md`, The LED).
- Keys: ON/OFF (PA08), Vol± (PB05/PB06), rear (PA07), all active low.
- IR [RE; the real remote untested]: Philips RC6 mode 0, T = 444 µs, address 0xD2: 0x0C standby, 0x10 vol+, 0x11
  vol−, 0x20 next, 0x21 prev, 0x2C play/pause; 0xD9-0xE2, 0xFD, 0xFE special modes (0xDC/0xDD possibly discrete
  on/off), 0xA0-0xD8 factory tests.

## Open
- Physical meaning of the USB mux lines, PB15/PB19/PB23/PB28/PB31/PA10; where the two thermistors (ADC4, ADC5) sit.
- Where the dock's shielded cable lands (analog iPod path) and whether a switch selects dock vs AUX jack.
- AUX optical on hardware, IR with a real remote, the iPod dock.
- The MCU's JTAG pads (needed only for a total brick).
