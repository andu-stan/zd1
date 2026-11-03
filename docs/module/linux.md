# Linux on the module

The module OS is the Nokia Home Music HD-1's kernel (BCoLinux 2.2.1, Linux 2.6.24.7, `arch/arm/mach-sulu`),
unmodified, with our own userspace (`module/os/`). The HD-1 uses the same BridgeCo chip family with the same
peripheral map (same bases, same IRQs, the same WILD Wi-Fi firmware build); its kernel is unstripped and its modules
are GPL: a working driver set for our hardware. Analysis of the dump: `re/netmodule/NOKIA_HD1.md`; the
kernel's symbols: `tools/zeppelin/jtag/kallsyms.py`. Tags: `docs/README.md`.

## How it boots
```
BL1 ──► stock preloader ──► our stage 2 at 0x60008000 = trampoline ──► boot stub (0x61F00000)
          (Fletcher check: a corrupt image still falls back to BL2)        │ pads, SPI link up (status 0x2000),
                                                                             │ the MCU's answer on 0xC4: a slot or the rescue
                                                                             └► kernel inflated → 0x60008000 (+ the rescue's
                                                                                initramfs), ATAGs (machine 0x270F, ttyS1), jump
```
The image is a normal `.bcd` (stock preloader, CnE and resources kept), so the MCU bootloader's relay and BL2's
web updater install it like any app and BL2 stays the way back: `zd1 module esc` reaches BL2 through BL1's
serial menu whatever the image does, and `zd1-linkd` answers HCR 0x21 (`zd1 module bl2`) once Linux is up. The
boot stub (`module/boot`) reports 0x2000 (booting) while it runs, so the stock MCU doesn't start its setup task until
`zd1-linkd` reports 0x2100. Our OS's init (`zd1-init`) brings the board up
(`zd1-board up`: the pad switch matrix, the PHY's reset and clock, the clock gates), then starts `zd1-linkd`, the
network, the receivers (`module/os/README.md`). [HW 2026-10-23] The stub asks the MCU which system slot to boot and
puts it on the command line as the root (`storage.md`, The boot).

What the kernel expects from its bootloader that ours doesn't give it: two Sulu ATAGs (0x41000F01 crystal frequency,
the default fits our 24 MHz board; 0x54410010 boot reason), and these hardware set-ups, which the stock chain does
and the Nokia kernel never does on our board [HW 2026-10-23, found over JTAG: `jtag.md`]. Every value and rule is in
`module/dm870` (the PAC), and `zd1-board up` (`module/os/apps/zd1-board`) is the whole set-up under Linux:
- **The pad switch matrix** (0xE0013000): without it the SPI-slave pads, the request line and the GPIO inputs are
  disconnected. Written once per boot stage: the stub on bare metal, init under Linux (`dm870::pads::BL2`).
- **GPIO**: +0x400 releases a pad and +0x404 makes it drive B (+0x408/+0x40C); Linux's "output high" only releases
  the pad with its pull-up, which the board's pull-downs beat: that held ETH NRESET (GPIO 12) and the request line
  (GPIO 6) low. `dm870::gpio` drives them (`docs/module/memory-map.md`, GPIO).
- **The Ethernet PHY's clock** is the AV1 PLL; `zd1-board up` starts it with BL2's values.
- **Clock gates**: the kernel's board init switches every gate off except 0, 2, 4, 5, 7, 9, 10, 11, 15, 16, 23, 24, 25
  and each driver re-enables its own (`memory-map.md`, Clocks); `zd1-board up` puts the stock app's set back (6, 12,
  14, 18, 20-22) and the SPI slave's (3 or 19). What it must **not** touch, after `sulu_wlan` has loaded: tgen +0x20
  (reset set; bit 1 holds the Wi-Fi core in reset) and tgen +0x40 bit 15 (set again, the core runs deaf). The PAC has
  no call that writes either behind the driver (`dm870::clocks`).

What the kernel does on its own that is Nokia-specific and harmless here [HW 2026-10-23]: its GPIO table names our
pins (`memory-map.md`, GPIO), it takes IRQs on GPIO 5 (`infrared`, IRQ 32) and GPIO 7 (`GPIO_X17_P31`, IRQ 34), its
`ce2-gpio-sr` driver clocks a shift register over GPIO 8-11 at boot (the "spi:MCUINT" messages) and registers LCD,
IDE, framebuffer and soft-DAC devices that have no hardware here. `reboot` works through timer 0 (the chip's
watchdog, `memory-map.md`).

## The MAC
`sulu_eth` comes up with 00:00:00:00:00:00 (Nokia set it from userspace) and `sulu_wlan` picks a random one without
`macaddr=`. Our OS takes both from PersParam MAC0: eth0 = MAC0 (init's `ethernet`), Wi-Fi =
MAC0 with the locally administered bit (`ec:66:d1:..` → `ee:66:d1:..`, passed by init to `sulu_wlan`). The device name ("Zeppelin
Air XXXXXX") comes from it too. `zd1-persparam` reads logical block 1 of `mtd0` (the read-only `vendor` partition
from NAND 0, `storage.md`; init makes `/dev/mtd0ro`), bad blocks skipped like stage 1, **raw**: the kernel's soft
ECC miscorrects normal MTD reads (`docs/module/boot-chain.md`, NAND). It reads no spare areas at boot (the PersParam
page has no valid ECC slot left). Read-only on purpose: writing the store is BL2's and the stock app's job (BL2 menu
"8 program MAC0", `persparam wrb MAC0` in the stock shell). On the unit the raw read returns the right block, eth0 carries MAC0 and wlan0 its locally administered variant
(`ee:66:d1:...`), and the MACB's specific-address register holds MAC0 [HW 2026-10-23]. `sulu_wlan`'s `macaddr=` takes
the `0xee,0x66,...` form, which is what `zd1-persparam mac --wlan` prints.

## Status

| | On the unit | Emulator |
|---|---|---|
| kernel init (clocks 480/80/240/160 MHz, VIC, timer 2, UARTs, NAND, USB host, RTC, ALSA) | yes | yes |
| our init (`zd1-init`: the early start, the services supervised), `zd1-board up`, the vendor modules | yes: `[os] up` ~16-22 s after power-on | yes |
| `zd1-linkd`: the MCU reaches READY, the native dialect, HCR 0x21 → BL2 | yes | yes |
| the system slots and the rescue (`storage.md`) | yes | yes |
| `sulu_eth`: 100 Mb/s, 169.254.1.1 at once, DHCP in the background | yes | DHCP, TCP 1 MiB both ways, md5 identical |
| `av-device`, `snd-sulu` (ALSA, `hw:0,0` = AV port 2 → the speaker's DSP) | plays (`memory-map.md`, Audio out) | bit-exact against the WAV the model records |
| `sulu_wlan` + `wpa_supplicant` 2.11 (`-Dwext`): WPA2-PSK, DHCP (`memory-map.md`, Wi-Fi) | yes | loads its firmware, then times out: no WILD core |
| the receivers (AirPlay, Spotify Connect, Snapcast: `module/os/README.md`, `docs/streaming.md`) | play | start |
| our OS with the stock MCU firmware | not supported: it answers none of the stock MCU's checks (`docs/protocol/spi-registers.md`, 4.2) | |

ALSA card `sulu-audio`: `hw:0,0` line out = AV port 2 ctrl bit 0 (our I2S to the DSP, as KnOS uses it), `hw:0,2`
spk out = port 2 bit 1, `hw:0,3` digital out = port 4 (S/PDIF), `hw:0,1` FM tuner in. Format S32_LE stereo 44.1
kHz, 256-frame periods; `speaker-test` programs the AV PLL, port 2 and SuluDMA contexts 33/34; each period is
block-moved from the ALSA buffer into an SRAM packet (header `0x26000804`, then L/R words with the sample in [31:8],
port format 0x29) [EMU].

How the driver plays [RE: `snd-sulu.ko`, `av-device.ko`, the kernel's `newdma_*` from `blobs/module/nokia-hd1/kernel.bin`
at 0xC0008000, symbols from `tools/zeppelin/jtag/kallsyms.py`]: SuluDMA moves two SRAM packets in turn to the PTU FIFO,
all the time, silence (a block move from 0xB2000000) while nothing plays; ALSA's start and stop only set a flag and
reset the period counter (`sulu_av_cmd`), so the I2S never stops. Each finished packet interrupts
(`sulu_av_out_dma_handler`, 173 a second): it reports period P to ALSA (`period_elapsed_cb`: the pointer is P x 256
frames, nothing finer), cleans the cache over ring period P, queues a block move of it into the free SRAM packet and
queues that packet. So ALSA counts the period that has just started playing as played (its delay is short by 0-256
frames, which the output's calibrated offset takes up), and from a start to the first interrupt it counts audio that
isn't even queued yet (up to a period plus the silence packet playing: the output measures only with a full buffer
for that). If the previous block move is still busy, the copy is skipped but the packet is queued anyway: the SRAM
packet's old contents (two periods earlier) would play again, unreported; not seen happen. The kernel's queue and
start calls are a few register writes under a short interrupts-off section, no polling.

What it costs [HW 2026-10-28, `soak` (idle-priority spare loop) against a writer of silence in four-period writes
through ALSA]: 5.6 % of the ARM with ALSA's default `avail_min` (the writer woken every period, 173 a second),
1.8 % with `avail_min` 4096 frames (10 wake-ups a second), the same 173 interrupts a second either way. Most of the
"ALSA cost" is the writer's wake-ups (on the ARM926 a switch between processes flushes the caches and TLB, and `soak`
itself is a process, so it overstates what a wake-up costs when the CPU is otherwise idle), not the driver; the
driver, the copy into the ring and the system calls together are at most ~1.8 %.

## Open
- The kernel's idle loop doesn't sleep (`arch_idle` is empty): the ARM runs flat out (`docs/backlog.md`).
- An out-of-tree module for anything kernel-side needs vanilla 2.6.24.7 headers and a matching config (no
  source for this build).
