# What connects to what

Board-level wiring as far as firmware, live probing and the CR870 datasheet show it (the boards have not been traced
end to end). Tags: `docs/README.md`.

```
                       ┌─────────────────────────── main board ───────────────────────────┐
 rear USB-B ───────────┤ USB mux ── MCU USBB (device to a PC / host to a docked iPod)        │
 dock (30-pin) ──┬─────┤ USB D+/D-, 5 V, detect                                              │
                 └─────┼─► (shielded, to the back board) analog line-out ─► AUX ADC  [INF]   │
 AUX jack ─────────────┤ codec ADC (analog) / S/PDIF (optical)                               │
 IR, keys, LED ────────┤ MCU GPIO / PWM                                                      │
                       │                                                                     │
                       │  MCU ── TWI ── DSP 0x38, codec 0x04, EEPROM 0x50, auth 0x10         │
                       │  MCU ── bit-bang I2C ── CS2000 (MCLK for USB/AUX)                   │
                       │  MCU SSC ── I2S #1 ──► DSP                                          │
                       │  DSP ─► codec DAC ─► class-D amp (PB09 supply, PB16 unmute) ─► drivers
                       │                                                                     │
                       │  14-pin link ══ carrier board ══ J2 (120-pin) ══ CR870 module       │
                       │    SPI0, PA04 power, PA17 reset, PA20 request, USART1, PB01 strap   │
                       │    I2S #2 (module = master) ──────────────────────► DSP             │
 Ethernet jack ────────┼── back board ── carrier board ───────────────────► module MACB/PHY  │
 antenna ──────────────┼──────────────────────────────────────────────────► module RF        │
                       └─────────────────────────────────────────────────────────────────────┘
```

## Main board ↔ module
The CR870 sits on a B&W **carrier board** (the 120-pin J2 lands there). The carrier is tied to the MCU board by a
**14-pin link** and to the back (interface) board, which holds the Ethernet jack, by a separate connector (seen
2026-10-22). The I2S lines run to the DSP on the main board too. Fourteen pins fits the table below plus supply and
ground [INF for the count].

| Signal | MCU | Module (J2 pin, DM870) | Notes |
|---|---|---|---|
| SPI clock | PA13 SCK | 29 SPI_CLK | MCU = master, mode 3 (CPOL 1), 8-bit [HW] |
| MCU → module | PA12 MOSI | 27 SPI_DIN | [HW] |
| module → MCU | PA11 MISO | 25 SPI_DOUT | [HW] |
| chip select | PA09 NPCS2 | 30 SPI_NCS0 | asserted per request [HW] |
| request line | PA20 (input, level-polled) | 64 PDOUT1 = GPIO 6 | high = notifications queued [HW] |
| module console in | PA06 USART1 TXD | 35 RXD1 | 115200 8N1; bytes from USART1 reach ttyS1 [HW 2026-10-23] |
| module console out | PA05 USART1 RXD | 37 TXD1 | a byte written to ttyS1 lands in USART1's RHR [HW 2026-10-23] |
| factory-reset strap | PB01 (output) | 68 PDOUT0 = GPIO 4 | module GPIO 4 follows PB01; stock holds it low [HW 2026-10-23] |
| power | PA04 (high = on) | supply switch | [HW] |
| reset | PA17 (low = reset) | 34 NRESET_MOD | [HW] |
| I2S data | — | 101 AV2DATA0 → DSP | the module drives the clocks [HW] |
| LRCK / MCLK / BCLK | — | 93 / 86 / 90 → DSP (and codec) | 512·fs MCLK, left-justified, 24-bit, 44.1 kHz [HW] |

The stock MCU firmware has a factory protocol on USART1 and opens it only when factory mode starts
(`docs/mcu/stock-firmware.md`) [RE]. How the factory shared that line with the module's console is unknown.

Not connected to the MCU: module UART0 (33/31, iPod iAP on other products), JTAG (22/24/26/28: the carrier's test
pads), USB host (55/57) [RE: no MCU code uses them]. The Ethernet PHY's reset is module GPIO-12 (J2 72) [HW].

## Auth chips: one per board
- MCU side: TWI 0x10, reset PA02, used only for the iPod dock. [RE]
- Module side: its own chip on the module's bit-banged I2C (GPIO 13/14, address 0x10, the bus's only device; a 2.0B,
  `docs/hardware/cr870-module.md`) with its own reset, for AirPlay (MFi).
  `ipodauth devid` = 0x200 with nothing docked and no MCU proxy traffic (registers 0xE5/0xE6 unused). [HW]

## Back board and dock
- The back board carries the AUX jack (analog + mini-TOSLINK optical), the rear USB-B (to the MCU through the USB mux)
  and the Ethernet jack (to the module through the carrier). [HW]
- The dock arm has two cables (photos): ~11 wires to the main board (USB, 5 V, detect) and ~7 partly shielded
  wires to the back board, most likely the analog line-out into the AUX ADC path ("AUX reused in iPod mode"). [INF]

## Open
- Exact module I2S pins on the main board side; the dock's analog landing and any analog switch; the power tree in
  detail (which SMPS feeds what); the 14-pin link's pinout.
