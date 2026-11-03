# DM870 memory map and hardware blocks

From live register reads (the KnOS shell, JTAG), the stock firmware, and the Nokia HD-1 Linux kernel (same chip
family, same map: `re/netmodule/NOKIA_HD1.md`). Tags: `docs/README.md`; untagged rows agree between the live unit and
the vendor code. **What our code uses is in `module/dm870`** (the PAC: every address, field and value by name, with
the same evidence); `zd1-board show` on the module prints the live state of those blocks. Code reaches registers
through it only; a register learned here goes there too.

## Clocks [HW]
Crystal 24 MHz → system PLL 480 MHz: **ARM 240 MHz**, AHB 80 MHz, SDRAM 160 MHz, Warp/Sol 160 MHz, AV system
80 MHz; timers 80 MHz. Audio PLL0 = **22.579 MHz** (512 × 44.1 kHz); PLL2 50 MHz, PLL3 12 MHz, PLL4 27 MHz (video).
PLL frequency (stock `FUN_604067ec`): f = 480 MHz / (div · (3 + x2 + x1 + x3 + int + frac/2048)).

How the clock tree is programmed (tgen 0xE0007000, decoded from the Nokia kernel's `tgen_init` / `tgen_get_clock`
[RE] and checked against the live registers [HW 2026-10-23]):
- **+0x00** system PLL: root = 24 MHz × [15:8] / [20:16] / 2^[1:0]; live 0x00022800 = ×40 / 2 / 1 = 480 MHz. **+0x04** = 1:
  the system PLL is the clock source.
- **+0x10** AHB divider (6 → 80 MHz). **+0x14** one 2-bit multiplier of the AHB clock per domain (0 means ×4):
  [1:0] ARM, [5:4] Warp0, [9:8] SDRAM, [13:12] AV system, [17:16] HV system, [21:20] AV oversampling, [25:24] Warp1,
  [29:28] Sol; live 0x22221223 = 240/160/160/80/160/160/160/160 MHz, the same as KnOS's `sys clock`.
- **+0x20 / +0x24** reset set / clear, bit 1 = the WILD core's ARM7 (`sulu_wlan` asserts it while loading the firmware
  and clears it after). Reads back the asserted bits: 2 under BL2 (Wi-Fi off), 0 once the driver has started the core.
- **+0x28 / +0x2C** clock gates set / clear (`sulu_power_enable/disable(n)`, n ≤ 25). Gate → block, from the Nokia
  drivers [RE] (gate 7 checked live: off = the Ethernet link dead, on = back [HW 2026-10-23]):

  | gate | block (who enables it under Linux) |
  |---|---|
  | 2 | Warp0 (`securewarp`) |
  | 3 | AV ports / PTU (`av-device`) |
  | 7 | **Ethernet MAC** (`sulu_eth`) |
  | 8 | USB host (`sulu_usb`) |
  | 13 | NAND (`sulu_nand`) |
  | 17 | LCD controller (`sulufb`; not fitted here) |
  | 19 | **WILD ARM7** (`sulu_wlan`) |
  | 20, 21, 22 | Warp1 / Sol / their window (`wsl`) |
  | 0, 4, 5, 9, 10, 11, 15, 16, 23, 24, 25 | kept on by the kernel's board init, not attributed (ARM, SDRAM, UARTs, SPI, DMA, timers ... are among them) |

  The Nokia kernel's board init (`ce2_init` → `sulu_power_init`) keeps gates 0, 2, 4, 5, 7, 9, 10, 11, 15, 16, 23, 24, 25
  on and **switches every other gate off at boot**; each driver turns its own back on. On our board that left 6, 14, 18
  and 20-22 off (nothing in Linux owns them) until init's `zd1-board up` re-enables the stock app's set (0x745040,
  `dm870::clocks::Gates::STOCK_APP`). **The readback is the enable mask** except bits 1, 4 and 24 up, which read 0 whatever is written (enabling gates
  1, 4, 24 and 25 changed nothing; gates 8 and 17 follow their writes) [HW 2026-10-25]. It reads 0x00FFFFED at reset
  (JTAG), under BL2, KnOS and our OS: `sulu_power_init` gives the gates on its list a use count of 1 without writing
  them and switches every other gate off, and the drivers (`av-device` 3, `sulu_usb` 8, NAND 13, `sulufb` 17,
  `sulu_wlan` 19) and `zd1-board up` switch theirs back on, which ends where reset started. Whether gates 1, 4, 24 and 25
  exist is not known.
- **+0x40** misc (bit 15 is cleared before the WILD core boots; live 0x208000), **+0x50**, **+0x140..** PLL
  registers, **+0x240..** the AV1 PLL (`zd1-board up`), **+0x600/+0x604**, **+0x680** (another clock select, field [19:16]),
  **+0x740** USB clock, **+0x840** measure status.

## Memory

| Address | Size | What |
|---|---|---|
| 0x00000000 | 4 MB | after boot: the SRAM repeated every 64 KB through 0x003FFFFF; the ROM isn't visible. Reads and writes [HW, JTAG 2026-10-25: a word written at 0x0000FFF0 reads back at 0x0040FFF0, 0x0001FFF0, 0x003FFFF0 and 0x0050FFF0; 0x0-0x3FFF and samples at 0x10000 and 0x3F0000 equal the SRAM under our OS]. The Nokia kernel's "USB FIFO memory" resource (0x40-0x3FFF, `/proc/iomem`) is therefore SRAM 0x400040-0x403FFF, the same bytes Linux hands to the LCD and Ethernet buffers: `sulu_usb` (not loaded by our OS) would share them |
| 0x00400000 | 64 KB | SRAM "UMR0": BL1 runs at 0x400000, stage 1 at 0x40DA00, the app's SP 0x40FA00; uncached alias **0x00500000** (the MAC's DMA uses it). Under Linux (`/proc/iomem`, idle): LCD line buffers 0x400000-0x403C0F (`sulu-fb`; the speaker has no display), Ethernet buffers 0x403C10-0x40A74F, 0x40A750-0x40FFF7 unclaimed (snd-sulu's packets go there while playing [EMU]). Both Warps load from it at ~6 cycles (Warp 0x04000000) [HW 2026-10-24] |
| 0x30000000 / 0x30200000 | 2 MB / 14 MB | WILD Wi-Fi core: its SRAM window / its registers (IRQ 15). The SRAM is **128 KB**, repeated through the whole 2 MB window (the words at +0x20000 are the same as at +0) [HW 2026-10-23]; the firmware (vectors at 0x30000000) and its data occupy 0x30000000-0x3001BFFF |
| 0x40000000 | | NOR window (CFI-probed by the app; nothing fitted) |
| 0x48000000 | 64 MB | IDE (Nokia; unused here) |
| 0x60000000 | 32 MB | SDRAM: images run at 0x60008000; KnOS cached heap 0x60900000-0x61CE0000; uncached alias **0x70000000** (KnOS uses 0x71D00000-0x72000000 for its non-cached heap) |
| 0xB0000000 | 16 MB | USB host controller (IRQ 8) |
| 0xB4000000 | | AV/PTU FIFO data port (SuluDMA audio endpoint) |
| 0xFFFF0000 | | high vectors (BL2 and the app install their table here; the ROM sets SCTLR.V) |

Our OS's use of SDRAM (`mem=31M`, the boot stub, the Warp1 window): `docs/module/warp.md` section 4.

## Peripherals

| Base | Block | IRQ | Notes |
|---|---|---|---|
| 0xB1000000 / 0xB1001000 | UART0 / UART1 | 21 / 22 | FIFO window: RX data +0xE00, RX fill +0xF18, TX data +0xE80, TX free +0xF1C, status +0xF44 (0x100 RX, 0x400 TX space, bit 1 TX full), IRQ mask set/clr +0xF50/+0xF54; baud divisor +0x08 = 2·clk/baud − 2; +0x24 bit 0x100 = TX flushed (BL1 polls it before a reset). UART1 = the console, wired to the MCU's USART1 [HW] |
| 0xB1002000 | SPI slave (the MCU link) | 20 | KnOS's names (`sys spi reg`): iSpiCfgSet/Clr +0x000/+0x004, iSpiDiv +0x008 (master clock: ⌈clock / baud⌉ − 2), iSpiNop +0x00C (the nop character: sent while the TX FIFO is empty, and with the filter on dropped from RX), iSpiWd +0x010, iSpiStatSet +0x020, iSpiIrqMaskSet +0x030; then the UARTs' FIFO port (iFifo…, +0xF00/+0xF04 written 0 to reset RX/TX). The configuration word, decoded from the stock driver (`FUN_6032152c`, its `sys spi config` options) [RE]: bit 0 on, 1 master, [5:4] width 8/16/32, 8 filter the nop character, 12-15 chip-select polarity 0 (21: polarity 1), 17-19 CPHA, 20 CPOL, 22 MSB first. The stock value 0x5EF101 = on, slave, 8-bit, filtering, SPI mode 3 (our MCU's master is mode 3), MSB first: the MCU's idle byte 0xFF never reaches the RX FIFO, which is why the link protocol stuffs a data 0xFF as 27 01. Request line = GPIO 6. No kernel driver binds it: `zd1-linkd` polls it through `/dev/mem`; `dm870::spi_slave` |
| 0xB1003000 | NAND controller | 24 | command +0x200/+0x204, address +0x210.., RX FIFO +0xE00/+0xF18, TX +0xE80/+0xF1C; KnOS polls status bit 5, Linux +0x20 (R/B, write done) and +0x88 (command FIFO free) |
| 0xB1004000 | SSM | 23 | function unknown. Live words (the same under BL2, KnOS and our OS): +0x10..+0x1C = 0xFFFFFFFF, +0x24 = 0x61FB0000 (0xF1FB0000 over JTAG), +0x28 = 0xE00, +0x40 = 0x01A09999, +0x50 = 0x40, +0x58 = 0x20000; the Nokia kernel registers the device (`sulu-ssm.0`) and never drives it [HW] |
| 0xB1005000 | **BL2 mailbox** (3 words) | | survives a warm reset, not a power cycle; `{0xDEADBEEF, mode 0, 0x21524110}` = start BL2 [HW] |
| 0xB5000000 | SuluDMA | 10-13 | below |
| 0xE0004000 | RTC | 75 | |
| 0xE0007000 | clocks ("tgen"): PLL presets, AV PLL status 0xE0007800 + n·0x10 (InLock, PhErr, IIRStat), reset set/clear +0x20/+0x24, power/clock gates +0x28 (enable, reads the mask) / +0x2C (disable) | | |
| 0xE0008000 / 0xE000A000 | AV port 0 (video) / **AV port 2 (audio out)** | | |
| 0xE0010000 | AV common: avActivate +0x00, avCUUnitOn +0x04, avComConfig0 +0x10, avCUConfig[0..1] +0x20, avAllAvOn +0x100, prtIsAvPort / prtIsPresent / prtHasAdat / prtHasPwm / prtHasAes +0x800..+0x810 (live 1 / 0x1D / 0x10 / 0 / 0x18: ports 0, 2, 3, 4 fitted; 3 and 4 carry AES, 4 ADAT), avInt set/clr/mask +0xC00.. | | |
| 0xE0012000 | AV PTU (the FIFOs between SuluDMA and the AV ports): avPtuCtrl0-2 +0x00.., fifoLuTable[0..4] +0x10.., ptuFifoEmpty / ptuFifoFull +0x800 / +0x804, ptuFillLevel[n] +0x810.., AV-bound-empty / bus-bound-full / DMA-abort interrupts +0xC00 / +0xC10 / +0xC20, ptuReset +0xFFC | | |
| 0xE0013000 | **pad switch matrix** (`sys swmtx` names, `re/netmodule/shell-dumps/emu_sys_regs.txt`): +0x00..+0x14 MtxGpioGroupA..F (0x102, 0x703, 0xF01, 0x706, 0x103, 0x307: which function owns each GPIO pad group), +0x20 MtxUSB (0x20), +0x24 MtxEthernet (1), +0x28 MtxWild (1), +0x2C MtxOthers, +0x30 MtxAllIns, +0x34 MtxAvLoop, +0x40 MtxTestBusSel, +0x44 MtxTestBusEnable; the block repeats 16 times through 0xE0013000-0xE0013FFF | | BL2 and KnOS write it at start-up, the Nokia kernel never does; without it the SPI-slave pads, the request line and the GPIO inputs are disconnected [HW 2026-10-23]. Written once per boot stage: our boot stub, then init (`zd1-board up`); `dm870::pads::BL2`. MtxWild = 1 (BL2's) with Wi-Fi up and associated: read on the unit 2026-10-26 |
| 0xE0017000 | Ethernet MACB (Cadence, rev 0x0001010d); SMSC LAN8700 PHY at MDIO address 5 (IDs 0x0007/0xc0c4) | 9 | 128-byte RX buffers. Under our OS: NCFGR 0x843 (100 Mb/s, full duplex, multicast hash, MDC = AHB/32), SA1 = MAC0, so unicast and broadcast are received without promiscuous mode; `sulu_eth`'s `TX packets` counter runs at ~400/s with nothing to send (it counts something else; RX and the IRQ count are sane) [HW 2026-10-23]. Clock gate 7. The PHY answers MDIO only with the pads routed, ETH NRESET (GPIO 12) released and the **AV1 PLL** running (tgen 0xE0007240.., BL2's values 0x80052666 / 0x100d0802 / 0x00110002): it is the PHY's clock. Turning AV1 off drops the link, and it doesn't come back when AV1 returns (the driver doesn't re-negotiate); AV2 off keeps it [HW 2026-10-23]. `zd1-board up` sets AV1 with the stock clock gates (0xE0007028) and tgen 0x7040/0x7050 |
| 0xE001B000 | AV cycle timer "CTR" (1394-style sec/cycle/offset, 24.576 MHz; counts once config +0x08 bit 0 is set: KnOS writes 1 there [EMU trace], under our OS nothing does and the whole block reads 0, also while audio plays [HW 2026-10-25]): ctr +0x00, lin25 +0x04, config +0x08, adjust +0x0C, drift +0x10, driftAccu +0x14, a second timer at +0x20, latchCtrl/latchValue +0x100/+0x104, trigCtrl +0x200, the rate as n/m/lsb +0x300.. (live n = 0xC0, m = 0x1B1) | | |
| 0xE001D000 | SDRAM controller (`sys sdram reg` names: AhbProtCfg +0x00, SDRamCfg +0x04, RefreshTHD +0x08, SDRamCfgSet +0x0C, Status +0x14, TestBusAddr +0x18, SelfRefreshTHD +0x1C, Rd/Wr wait-state, activate and deactivate words per bank +0x20..+0x94, SupCfg/SupCnt/SupRefCnt +0x100..) | | stage 1 polls command bits [20:19]; live: SDRamCfg 0xFE207C60, RefreshTHD 0x460000, CfgSet 0x0100046D, Status 5 [HW] |
| 0xE0018000 | 9 groups of 4 words (ID 0x04032200) | | most likely the **UMR arbiter** (`sys umr`: priorities for the 9 AHB masters DMA0, DMA1, ARM9I, ARM9D, SECWARPI, SECWARPD, ETH, WILD, SOLWARP); live {0, 2, 6, 0}, {0, 2, 6, 0}, then {8, 0, 0, 0} × 7, identical under BL2 and Linux [HW]; not confirmed |
| 0xE0019000 | AHB switch "swx" (ID 0x05112900) | | five address-protection units of 0x40 (`sys swx reg`: iAddressMask +0x00, iAddress +0x04, iIrqClr +0x0C, iIrqMaskClr +0x14, iErrMaskClr +0x1C, iAuthClr +0x24), summary +0xC00, master / slave IRQ clear +0xC14 / +0xC1C, test bus +0xFF4. All masks 0 on the unit: nothing is protected, bus errors don't interrupt [HW] |
| 0xE0011000 | scratch | | reads as random bytes that differ between power cycles: an uninitialised RAM, nothing uses it |
| 0xE001E000 | "anagram-softdac" (Nokia's name; ID reads 0) | | present (live 0x4020, 0, 0, 0x4001, 3, 0x40000000), unused here: the DM870's PWM/1-bit audio output, `prtHasPwm` = 0 |
| 0xFFFF0080.. | interrupt controller | | irqOffset +0x90 read = acknowledge, EOI = write the number to +0x98, priorities at +0x100, mask/unmask +0x180/+0x190, trigger mode 2 bits per line at +0x160 [RE] |
| 0xFFFF0200.. | timers 0-2: cfg set/clr +0x200/+0x204 (bit n run, 4+n one-shot, 8+n /16, **bit 31 on timer 0 = reset on expiry**, bit 12 = timestamp at +0x230), count +0x210 + 4n, threshold +0x220 + 4n. All at **80 MHz**, the timestamp too (Linux's clocksource) | 5, 6, 7 | KnOS tick = timer 1 (1 ms); Linux tick = timer 2 (10 ms). **Timer 0 is the chip's watchdog / software reset**: Linux's `reboot` (`arm_machine_restart`) stops it, sets the threshold to 8,000,000 (100 ms), sets bit 31 and starts it, and the chip resets: `reboot` under our OS restarts the module through BL1 into our OS in ~20 s, and zd1-linkd reaches READY again [HW 2026-10-23]; KnOS's watchdog uses the same. Under our OS nothing arms it (cfg reads 0x1226: timers 1 and 2 running) [HW] |
| 0xFFFF0300 page | hardware semaphores +0x308, +0x30C (Warp ↔ ARM, `docs/module/warp.md`) | 51 | |
| 0xFFFF0400 | GPIO [HW, JTAG 2026-10-25]: two masks. **A** (+0x400 set, +0x404 clear, reads back at either): 1 = the pad is **not driven**, 0 = it drives B. **B** (+0x408 set, +0x40C clear, reads back at +0x40C): the level a driven pad outputs, and the pad's **pull-up** while A is set. **+0x408 reads the levels.** Measured on the unconnected GPIO 0: A=0 B=1 high, A=0 B=0 low; released with B=1 high at once (the pull-up), with B=0 it keeps its level and floats up within 0.5 s. KnOS bit-bangs its I2C open-drain this way (B clear, A toggled). Linux's "output high" (A and B set) only releases the pad with its pull-up, which the board's pull-downs on ETH NRESET and the request line beat: that held both low. Released pads on this carrier read low on 4 (the strap's pull-down), 6 (also against the pull-up), 15 and 17, high on the rest | 27 + n (n < 20) | 20 GPIOs. Their functions, from the Nokia kernel's GPIO table (`ce2_init` → `gpio_init`), which names the DM870's pins and matches the CR870 datasheet and this carrier [RE, HW]: 0-3 GPIO_X17_P17/P15/P13/P11 (3 = MIITXCLK, iPod accessory power on J2), **4 fact_reset** (the strap, input), **5 infrared** (IRQ 32), **6 host_nireq** (the SPI request line), 7 GPIO_X17_P31 (IRQ 34), 8-11 sr_spi_mosi/miso/clk/cs (Nokia's bit-banged shift-register SPI; the stock app uses 8/10 as a second I2C), **12 nreset_eth**, **13/14 i2c_scl/sda** (the auth chip), 15 GPIO_X17_P27, 16 lcd_a0, 17 GPIO_P29, 18 irq_tuner_mod, **19 ipod_ndetect**. Under our OS with nothing attached: A 0x000FE2FF, B 0x00001440, levels 0x000D76AF (low: 4, 6, 8, 11, 15, 17; 8 and 11 driven low) [HW 2026-10-25]. The Nokia kernel takes IRQs on 5 and 7 (one each at boot) and clocks 8-11 at boot through its `ce2-gpio-sr` driver: harmless here |
| — | AV 8 kHz stream tick | 4 | drives KnOS's ISO audio engine |

Interrupt lines in use, from `/proc/interrupts` under our OS and KnOS's `sys irq table` [HW]: 4 AV tick (KnOS), 5/6/7
timers 0/1/2 (Linux ticks on 7), 8 USB host, 9 Ethernet, 10 SuluDMA FIQ, 11-13 SuluDMA tag groups 1-3, 15 WILD, 20
SPI slave, 21/22 UART0/1, 23 SSM, 24 NAND, 27 + n GPIO n (32 = IR on GPIO 5, 34 = GPIO 7, 46 = GPIO 19 iPod detect),
51 hardware semaphores, 75 RTC. KnOS also installs handlers on 18 and 25 (not identified).

Every block has an ID word at +0xFF0..+0xFFC (live values: `re/netmodule/cr870-live-block-ids.txt`). Register dumps
of the whole peripheral window under Linux and under BL2: `re/netmodule/jtag/`.

## MMU (stock app) [RE]
TTB 0x70000000, DACR 0x40000000 (domain 15 = client). All 4096 sections are first mapped flat with AP = 00 (no
access), then SRAM/SDRAM/NOR, then nine device regions with AP = 11 (0x30000000-0x30FFFFFF, 0xA8-0xAA…, 0xAC-0xAE…,
0xB0-0xB2…, 0xB4-0xB5…, 0xB8…, 0xD0-0xDF…, 0xE0000000-0xE0025FFF, 0xFFFF0000-). The shell's `rd` checks this table in
software ("access denied by MMU"); `rd -c` turns the check off, and then an access to an unmapped region faults the
module (watchdog reboot) [HW]. Stage 1 and BL2 hand over with the MMU off.

## SuluDMA
Base **0xB5000000**, 64 contexts of 0x40 bytes. Register map, context layout and flag bits: `re/netmodule/SULUDMA.md`.
- **Packet moves** (KnOS audio) [RE, HW]: write the packet address to ctx +0x0C, then `ctx | 0x800` to the client
  register (+0x3000 + client·4); 0x800 reads back set while the move runs. Audio output = **context 58** →
  0xB4000000; contexts 59-61 are video.
- **Block moves** (Linux `sulu_eth`: every frame between SDRAM and the MAC rings in SRAM) [RE]: start bitmap at
  +0x00 (ctx at +0x80 + 4n), busy at +0x04; ctx +0x0C source, +0x08 destination (< 0x10000 = offset into SRAM
  0x400000), +0x10 length, Flags0 [5:0] = next context; completion = a tag in the group-1 FIFO, IRQ 11.
- **Packet streams** (Linux `av-device`, the Nokia kernel's `newdma_*`) [RE]: a context chain started through the
  lookup window (+0x6000 + ctx·0x40) and the context bitmaps; audio uses contexts 33 ↔ 34 (ping-pong from SRAM
  packets to the FIFO 0xB4000000). Completion: the context number in its group's tag FIFO (+0x1C40 + 4·group), IRQ
  10 + group. **Flags0 bit 7 = the context is empty**: the driver queues a packet only into an empty context; the
  engine sets the bit when it has moved the packet.
- **Access widths** (Flags1: read [22:21], write [24:23]; 2 = 32-bit, 1 = 16-bit, 0 = 8-bit): a block move's length
  is the byte count shifted by the client's width. sulu_eth TX (Flags1 0x01481000) counts bytes, sulu_eth RX
  (0x01501000) and av-device (0x00480000) count quadlets [EMU, from the drivers' own values].

## Audio out (AV port 2)
KnOS [HW]: decoder → StreamDeviceMgr → ISO audio output stream (isoChannel 44, ~5,512 packets/s, **runs all the time
and sends silence when idle**) → SuluDMA ctx 58 → PTU FIFO 0xB4000000 → AV port 2 → I2S (512·fs, left-justified,
24-bit, 44.1 kHz, master) → the main board's DSP. Packet = word0 [15:0] byte count (0x44), word1 timestamp, then 8
frames × 2 channels, 24-bit samples in [23:0].

**AV Kind (+0x08)** [RE], decoded from KnOS's `sys av port` (FUN_603962fc, FUN_60403d0c): [9:6] interface (0 = I2S),
[5] alignment, [4] polarity, [3:0] memory data format: 0 AM824_16, 1 AM824_20, **2 AM824_24**, 8 Packed24, **9
FP32**, 10 Packed16HL, 11 Packed16LH, 12 MSB16, 13 MSB20, 14 MSB24. KnOS: 0x22 = AM824_24 (IEC 61883-6 quadlets:
label byte + 24-bit sample, 8-frame packets with a presentation timestamp, SYT interval 8). Linux's `av-device`:
0x00030029 = FP32 (a 32-bit fixed-point word, the sample in [31:8]: ALSA's S32_LE as is, block-moved per 256-frame
period, timestamp word 0; [17:16] = 3 is the TsValidRange field). Same I2S on the wire; only how the port reads
memory differs.

**Linux's path on the unit [HW 2026-10-24]** (`speaker-test -D hw:0,0 -F S32_LE`; the receivers play through it,
heard through the speaker since 2026-10-24): `av-device` opens PLL "av_clock" = tgen +0x140.. (0x80220849 / 0x100D0801 / 0x00110002) at
**45,158,400 Hz = 1024·fs**, sets port 2's dividers bit/ws/mclk = 16/64/2 (BCLK 64·fs, 64 bit clocks per frame, **MCLK
= PLL/2 = 512·fs**: the same clocks KnOS drives, measured on the wire) and port 4's to 8/128/4 (the S/PDIF port shares
the PLL), writes AV_LUT(2) = 0x24, activates ports 0x14 (avActivate = 0x14), sets Kind 0x00030029, and port 2 reads ctrl
0x01000001 (On, UpAndRunning), status "In sync" with OS count 0x716 as under KnOS; PLL status +0x804 = 0xFFFF; the PTU
FIFO fill level shows the stream; the packets go through SuluDMA context 33 from the SRAM buffer at 0x40A750. The ARM
was 75 % idle while generating the tone. One `EPIPE` underrun during a long run while 4 KB register dumps were taken
over `zd1-memsh` (1024-frame buffer = 23 ms). How the driver plays and what it costs: `docs/module/linux.md`.

PLL status (InLock / PhErr / IIRStat at 0xE0007800 + 0x10·n): all 0 while the PLL isn't programmed, 0 / 0xFFFF / 0 with
`av-device`'s PLL running (also after playback) [HW 2026-10-25]; what KnOS's lock monitor reads is not measured.
Nothing plays unless **AV PLL 0 is locked** (the lock monitor resets the PLL and the sync domain stays muted
otherwise) [RE]. The amp, its mute and the volume are on the main board: a module that outputs samples is audible
only if the MCU has the amp on and the DSP routed to it. Port 3 = standard I2S (512·fs), port 4 = AES/S/PDIF, port 0
= video.

## Wi-Fi (WILD)
On-chip FullMAC (Wipro-NewLogic "WiLD" 802.11b/g MAC + baseband, firmware 0x00A70506 [HW], an ARM7 of its own with
**128 KB of SRAM** at 0x30000000) behind 0x30000000/0x30200000, presenting an **802.3 interface** (frames in and out
are Ethernet). The radio is the T6201 (the "Eagle RF" the MIB names). KnOS drives it through a shared-memory MIB (28
groups, 329 elements: `re/netmodule/wlan_mib_schema.txt`, `re/netmodule/WIFI_DRIVER.md`), with CCMP in hardware. The
Nokia kernel's GPL `sulu_wlan.ko` (mac80211) loads the same firmware build (`sulu-wlan.fw`): gate 19 off and on,
tgen +0x40 bit 15 cleared, MtxWild cleared, ARM7 reset asserted (tgen +0x20 = 2), the firmware copied to 0x30000000,
the reset released (+0x24 = 2), then it waits for the firmware's first interrupt [RE].

**On the unit [HW 2026-10-23]** the driver finds the hardware, the firmware starts (IRQ 15 fires, "phy0 started", no
"timeout waiting for firmware"), `wlan0` carries MAC0 with the locally administered bit, and a Wireless-Extensions scan
(through `SIOCSIWSCAN`/`SIOCGIWSCAN`; `wpa_cli scan` does the same) returns the access points around the unit
with channel, level and WPA information elements; the firmware's debugfs MIB views (`/sys/kernel/debug/sulu-wlan/mib_*`,
the group's number in the first word, the schema `re/netmodule/wlan_mib_schema.txt` from offset 4) show live PHY
calibration (TemperatureOffset, ChipProcess). Two things to know: **tgen +0x20 = 2 holds the core in reset**
(released with +0x24 = 2); **tgen +0x40 bit 15 must stay as `sulu_wlan` leaves it**
(cleared): setting it back to BL2's value after the driver has loaded leaves the core running but deaf, scans empty
and IRQ 15 quiet, until the driver is reloaded [HW 2026-10-24]. MtxWild (switch matrix +0x28) is not part of it: it
reads 1 (BL2's) in all 16 copies while the unit's Wi-Fi is associated with a DHCP lease [HW 2026-10-26]; the driver
clears it while it starts the core, and afterwards its value doesn't matter. The driver's
`macaddr=` wants `0xee,0x66,...` (what `zd1-persparam mac --wlan` prints), not colons.

**Association works [HW 2026-10-24]**: `wpa_supplicant` 2.11 (`-Dwext`) joins a WPA2-PSK (CCMP) network at -57 dBm,
`udhcpc` gets a lease, and the receivers play over it (AirPlay 2 realtime recovers the lost packets by retransmit
requests: `module/os/forks.md`). WPA3-only networks (SAE) are not reachable this way:
WEXT has no SAE authentication path [INF]. `wpa_cli signal_poll` reports -80 dBm whatever the level: the driver doesn't fill it in;
`wpa_cli bss BSSID` has the real one.

**The antenna is on the radio's second port** [HW 2026-10-24]: `sulu_wlan`'s `diversity` parameter (0 both, the
default; 1 / 2 one port; 3 toggling) sets the firmware's AntennaMode (DR_PHY_STAT, +8 in the debugfs table: 22 / 10 /
11). Init loads the driver with `diversity=2`: the AP (UniFi) then hears the module at -56 dBm, as the module hears
it (-57): uplink 0 % lost at the rate control's own choice (54 Mb/s; 3 % retried, no failures), downlink 0.7-1.0 %
lost (single packets, gaps <= 76 ms), router ping 1 % / 6 ms average (`tools/zeppelin/lab/netprobe.py`: an
AirPlay-like UDP stream, 1,200 B every 8 ms, `module/os/debug/udpprobe.c` on the module). With the default (both
ports) the AP heard the module at -70..-79 dBm: the uplink lost 0.2-1.9 % in bursts, the 2.6.24 kernel's 'pid' rate
control wandered between 1 and 54 Mb/s, and the AP missed the module's ACKs (2-12 % of received frames were
retransmissions of frames the module already had). Port 1 alone: the uplink nearly dead. (The stock firmware's own
value is AntennaMode 10, i.e. port 1 under this firmware build: its encoding differs, or the stock app's TX power
table compensates; not looked into.)

No loss inside the module: the firmware's receive counters equal what the kernel and the probe got, no buffer or
decryption errors. The firmware's statistics tables (`/sys/kernel/debug/sulu-wlan/mib_*`) are the stock schema's
groups shifted by 4 bytes in places and unaligned in others: TRC_STAT +8 transmitted, +16 failed, +20 retried,
+24 retried more than once; DP_PP_STAT +8 duplicates, +12 multicast received, +24 unicast received; DP_SEC_STAT +20
decrypted [HW, from counters moving with known traffic]. `module/os/debug/wifi.c` prints the WEXT view (rate,
power, retry limit) and sets a fixed rate (`SIOCSIWRATE`), the retry limit or the RTS threshold.
The emulator doesn't model the WILD core.
