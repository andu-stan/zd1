# Nokia Home Music HD-1: a Linux port for our chip family

Source: an owner's NAND dump, archive.org item `nokia-hd-1-dump` (`mtd0-11ro.zip`, `mtdblock0-11.zip`). Analysed 2026-10-18, static only.
Kernel symbols: `python3 -I tools/zeppelin/jtag/kallsyms.py` decodes them from the kernel image's own kallsyms tables.

**Bottom line:** the HD-1 is a BridgeCo "Sulu" (DM860-class) running BCoLinux 2.2.1 (Linux 2.6.24.7,
`arch/arm/mach-sulu`, JukeBlox 4.2). Its peripheral map is **the same as our DM870's**: the same addresses,
the same IRQ numbers, and the same WILD Wi-Fi firmware build. So it is a (GPL, unstripped, `-O0`) Linux
driver set for our module's hardware. Our stock app names the chip too ("WILD core did trigger interrupt on
Sulu").

## 1. Layout of the dump
NAND `mtdparts=sulu-nand:` BL1 128K ro | SEL_A | BL2_A | SEL_B | BL2_B (128K each) | BL2Config 256K |
Kernel_A 2M ro | RootFS_A 64M ro | Kernel_B 2M | RootFS_B 64M | Runtime_Data 64M | BIST_Kernel 2M.
"Sulu Security Loader" (SEL, scrambled) → "Sulu Bootloader 2 (r1105)" → uImage-ish kernel (gzip at +0x3380) →
jffs2 rootfs. Console `ttyS1,115200` = UART1 0xB1001000. Its BL2 is **not** our KnOS BL2 (only 2 of 75
strings in common), so it doesn't help with `.bcd`/BL2 handoff questions.
Extraction: `jefferson` for the rootfs, `vmlinux-to-elf` for kallsyms (17,334 symbols, base 0xC0008000).

**Two kernel builds in the dump (2026-10-20); use Kernel_A.**

| Where | Build | |
|---|---|---|
| Kernel_A (mtd6), Kernel_B (mtd8) | `2.6.24.7 (root@steve-desktop) #1 Mon Jul 20 12:11:21 CST 2009` | byte-identical (inflated sha1 4b22a582...); what the HD-1 boots and the rootfs modules (`/lib/modules/2.6.24.7`) belong to; `module/linux`'s `kernel.bin`; symbols `tools/zeppelin/jtag/kallsyms.py` (17,244 of 17,246 match the running kernel's /proc/kallsyms) |
| rootfs `/boot/zImage-2.6.24.7-r1116`, `/boot/vmlinux-2.6.24.7-r1116` | `2.6.24.7-r1116 (marcel@telamon) #1 Wed Jun 24 16:58:08 CEST 2009` | an older development build left in the rootfs: ~4% of its code words match Kernel_A, no modules for it, nothing boots it. Its unstripped `vmlinux` names the wrong functions for Kernel_A: don't use it for symbols |
| BIST_Kernel (mtd11) | - | blank |

### Nokia BL2 vs ours (2026-10-18)
- Nokia's BL1 (mtd0) and ours (block 0) both start `ff ff ff e6` and are encrypted (entropy 7.99): probably the same
  BridgeCo ROM-loaded BL1 format, but they load **different BL2 formats**. Nokia: SEL (scrambled) + BL2 with magic
  `0x21111978` and a size/address header (`0x4c40, 0x4000, 0x4000`), plain code, 19 KB, "DM8x0 NAND Bootloader 2 (r1105)".
  Ours: a MetaInfo type-1 partition at 0xA0000 (stage 1 -> SRAM 0x40DA00, stage 2 -> 0x60008000). The magic appears
  nowhere in our NAND, so our BL1 would not load Nokia's BL2 as is.
  **Confirmed from our decrypted BL1 (2026-10-19, re/netmodule/bl1):** our BL1's loader looks for `MetaInfo` + boot
  magic `0xAAAA5555` (both in its code) and never references Nokia's `0x21111978`; Nokia's BL2 (mtd2ro) starts
  `0x21111978` with neither marker. The two BL1 binaries also differ (both carry the `ff ff ff e6` ROM-load prefix =
  same BridgeCo decrypt family, but different payloads). So our BL1 boots only our MetaInfo chain, Nokia's only its
  SEL+BL2 chain; neither loads the other. (Linux on our module is unaffected: BL1 enters an app at 0x60008000 and
  `module/os` wraps the Nokia kernel in our .bcd format.)
- Not needed to boot Linux: our chain already enters an app image at 0x60008000 (`module/os/mkpayload.py`), and our BL2 is
  the recovery path (web/XModem/TFTP, `docs/module/boot-chain.md`), so it stays.
- What Nokia's BL2 gives the kernel, i.e. what a Linux image on our module must supply itself: the standard ATAGs, the
  command line from BL2Config (`kernel_cmdline_A`, `mtdparts=sulu-nand:...`), and two Sulu tags: **`0x41000F01`** (crystal
  frequency, `parse_tag_sulu_xtali`; the default fits our 24 MHz board: the kernel derives 480 MHz on the unit)
  and **`0x54410010`** (`parse_tag_boot_reason`). The Ethernet MAC is not passed to the kernel (`sulu_eth` has no
  parameter; userspace sets it, Nokia keeps it in BL2Config as `eth_mac_address`, read by `/usr/sbin/bl2config`); the
  Wi-Fi MAC is the `sulu_wlan macaddr=` module parameter (random otherwise). Ours lives in PersParam.

## 2. Static I/O map (`sulu_io_desc`)

| virt | phys | len |
|---|---|---|
| 0xF0000000 | 0xE0000000 | 1 MiB (the APB-ish peripheral block) |
| 0xF0F00000 | 0xFFFF0000 | 4 KiB (IRQ ctl + timers + GPIO) |
| 0xF1000000 | 0xB1000000 | 20 KiB (UART0/1, SPI, NAND, SSM) |

## 3. Platform devices (`sulu_devices` + `board_devices`), compared with ours

| device | MEM | IRQ | vs our DM870 |
|---|---|---|---|
| sulu-serial.0/.1 | 0xB1000000 / 0xB1001000 | 21 / 22 | same bases |
| sulu-spi.0 + **sulu-spi-slave.0** | 0xB1002000 | **20** | same: our MCU link, IRQ 20 |
| sulu-nand.0 | 0xB1003000 | 24 | same base |
| sulu-ssm.0 | 0xB1004000 | 23 | block exists (ID at +0xFF8) |
| sulu-rtc.0 | 0xE0004000 | 75 | |
| sulu-eth.0 | 0xE0017000 | 9 | same base (macb, rev 0x0001010d) |
| **sulu-wlan.0** | **0x30000000–0x301FFFFF** + **0x30200000–0x30FFFFFF** | **15** | see §5 |
| sulu-usb-hcd.0 | 0xB0000000 (16 MiB) | 8 | |
| sulu-ide.0 | 0x48000000 (64 MiB) | 33 (= GPIO 6) | |
| sulu-fb.0 | 0xE0024000, 0xE0020000 | | LCD, n/a for us |
| anagram-softdac.0 | 0xE001E000 | | |
| sulu-av / sulu-audio / sulu-i2c / sulu-spi-gpio | (in modules) | | |

Other IRQs from the code: timer 2 → **7**; SuluDMA tag group 1 → **11** (`sulu_dma_init`: `request_irq(11, sulu_dma_isr,
dev_id 1)`, the "old DMA" block moves sulu_eth uses), groups 2/3 → **12/13** and a **FIQ on line 10** (all three from
`sulu_newdma_init`); GPIO 0..19 → IRQ **27+n**.

## 4. Core blocks (confirmed or new)
- **IRQ controller 0xFFFF0000**: EOI = write the IRQ number to +0x98; priority slot bytes at +0x100;
  mask/unmask via +0x180/+0x190; **trigger mode 2 bits per line at +0x160** (16 lines per word; Linux only uses it
  for the GPIO lines 27..46).
- **Timers**: cfg set/clr +0x200/+0x204 (bit n = run, bit 4+n = one-shot, bit 12 = timestamp), counter
  +0x210+4n, threshold +0x220+4n, timestamp +0x230 = the **clocksource** (`sulu_timer_read`, rating 400, 32-bit,
  `mult` for **80 MHz**: the timestamp runs at the timer clock). Linux tick = timer 2 → IRQ 7, periodic threshold
  800000 = 10 ms ⇒ **80 MHz** timer clock.
- **GPIO 0xFFFF0400**: the kernel's `gpio_setdirection` writes +0x400/+0x404 and `gpio_setvalue` +0x408/+0x40C,
  as if they were direction and output. On our unit +0x400 releases the pad, +0x404 makes it drive the +0x408/+0x40C
  bit, which is the pull-up while the pad is released (JTAG, 2026-10-25: `docs/module/memory-map.md`), so the
  kernel's "output high" is a released pad with its pull-up, low wherever the board pulls down. GPIO 0..19 have IRQs (27+n); higher GPIOs are polled (`kgpiopoll`).
- **tgen / clocks 0xE0007000**: `sulu_power_enable(n)` writes `1<<n` to **+0x28** and `sulu_power_disable(n)` to
  **+0x2C** (refcounted, n ≤ 25): clock/power gates. **Gate 19 = WLAN ARM7.** Reset set/clear +0x20/+0x24
  (bit 1 = WLAN ARM7). Misc +0x40 (bit 15 cleared before WLAN boot). PLL registers +0x140..0x148,
  measure status +0x840, AV-port PLL attach +0x500..0x50C, USB clock +0x740. The
  kernel prints "Root/AHB/ARM/MEM/AVSYS/HVSYS/AVOS/WARP/WARPSOL/SOL" clocks: `tgen_get_clock`/`tgen_init`
  decode the clock tree (not decoded here yet).
- **0xE0013000**: +0x28 = `MtxWild` (bit 0), the same name our stock app prints. The kernel touches only that word;
  the block is the pad switch matrix, which our stock chain sets up and this kernel never does
  (`docs/module/memory-map.md`).

## 5. Wi-Fi (WILD): same firmware as ours
- `sulu_wlan.ko` is a **mac80211** driver (GPL, symbols intact) + `/lib/firmware/sulu-wlan.fw` (rev78512, 60 KB,
  an ARM7 image with a vector table at 0).
- **The firmware's version routine loads `0x00a70506`, which is exactly the `FirmwareVersion` (MIB 00:02) our
  unit reports live.** So it's the same WILD MAC firmware build. The Linux host side (slot/mailbox queues
  `gi_*`, `sulu_wlan_send_cmd`, MIB get/set via debugfs, TX/RX buffer rings, `set_key`) is a working driver; our OS
  loads it unchanged, and on the unit it associates with WPA2 (`docs/module/memory-map.md`, Wi-Fi).
- Boot sequence (`sulu_wlan_arm7_startup`): power gate 19 off, wait ~100 µs, on → tgen+0x40 &= ~0x8000 →
  MtxWild &= ~1 → assert ARM7 reset (tgen+0x20 = 2) → `memcpy_toio` the firmware to **window 0 = 0x30000000**
  (WILD SRAM) → reg 0x30200000+**0x584** |= 1<<IRQ (15) → release reset (tgen+0x24 = 2) → wait for the
  firmware-ready interrupt.
- 0x30000000 is the **WILD core's own memory window** (2 MiB), with WILD registers from 0x30200000.
- Where our stock app keeps its copy of the firmware is unknown (no plain vector-table match; probably compressed or
  in a resource). Not needed while we ship Nokia's blob; its licence is open.

## 6. SuluDMA (`newdma_*`, exported)
- Linux maps the controller at **0xB5000000, 64 KiB** (the cfg block live at 0xB5001000 = +0x1000).
- Init writes 1 to +0x1C08, +0x1C28, +0x1C38 (group IRQ enables); old-DMA init writes +0x1C18.
- Context RAM at **+0x4000**, 16 words (0x40 B) per context, so it agrees with our 64 × 0x40 model;
  `newdma_packet_create` fills ctx words 9 (|0x80), 10, and 2 or 3 (AHB address) depending on direction.
  Packet types 0x38000000 / 0x78000000 (read/write side), and AHB read/write access setup per client.
- Start: write `ctx | ctx<<16` to **+0x6000 + ctx*0x40**, set bit ctx in the bitmap at **+0x2810**.
- The driver's use of the engine: `docs/module/memory-map.md` (SuluDMA), `SULUDMA.md`.

## 7. Audio
- `av-device.ko` ("Sulu Low Level Audio Device Library"): `sulu_av_setup/cmd/open`, AV-port bitmap, in/out DMA
  handlers; it touches **0xE0008000** (+0x00/+0x08/+0x0C/+0x20, the AV port block: ours are
  E0008/A/B/C000), **0xE0010000** and **0xE0012000/+0x10**.
- `snd-sulu.ko` = ALSA card (codec glue for WM8776/WM8955 over I²C; our codec path is different).
- `wsl.ko` = "Warp/SOL audio engine" driver: loads a MIPS program into Warp1 (drives the Sol DSP) and its queues;
  `securewarp.ko` = Warp0 (BWOS, DRM). Details and how to load our own code: `docs/module/warp.md`.
- With the PLL `pll_attach_avport` code, this is the reference for the AV/I²S output (`docs/module/memory-map.md`,
  Audio out).
