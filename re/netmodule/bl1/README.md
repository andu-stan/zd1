# BL1 (DM870 first-stage bootloader), decrypted

Captured 2026-10-19 on our unit with a capture image (the stock app plus a stub that copied SRAM to SDRAM before the
app started; installed through BL2's web updater, read back with the module shell). BL1 sits in NAND block
0 encrypted (`blobs/module/bl1.enc`, = the archive.org unit's); the boot ROM decrypts it into SRAM at 0x400000. The
summary of what it does: `docs/module/boot-chain.md`.

| File | What |
|---|---|
| `blobs/module/bl1.plain` | SRAM 0x400000-0x40869F as BL1 left it (0x869C = BL1's used length in NAND): code + its data/bss as of the hand-over (variables reflect that boot) |
| `sram_at_stage2_entry.bin` | the whole 64 KB SRAM at the app's stage-2 entry: BL1, then the app preloader at 0x40DA00 (its stack 0x40D8CC-0x40D92C) |

No per-unit data in either (checked: no MAC, no Wi-Fi strings).

## Strings
- "Starting Bootloader Level 1", build "Sep 30 2010 17:52:25"; "FLB running @ %uHz; Flash Drivers:".
- Serial menu "Bootloader Level 1 Menu": 1) Download Bootloader Level 2 ("Start download with Xmodem ..."),
  2) Show Meta Info, 3) Reset, 4) Start Bootloader Level 2, 5) Start Application, 6) Erase Flash.
- "Forced start of Bootloader Level 2" next to the PersParam key "BLDb" (stored "bDLB"): BL1 reads PersParam.
- "Bootstrap failed: More than 1 frame / Header incorrect in frame 1", "Invalid image CRC", MetaInfo dump
  (Partition ID/Size, Base Address, Image Size/CRC), BCD header dump.
- NAND driver: chip table, bad-block scan ("Bad Block found"), ECC ("fixpage result").

## Boot logic (disassembly, ARM at 0x400000)
- Entry 0x400000: SP 0x40DA00, ARM C-runtime scatter-load (region table 0x40843C), C entry 0x400688, main 0x40453C.
- main: puts "Starting Bootloader Level 1"; 2 s timer; flash init; **forced-BL2 check** (0x404418); if not forced and
  the app image is valid, start it; else start BL2; a key in the serial window opens the menu instead.
- Forced BL2 (0x404418) iff: the RAM mailbox copy is {0xDEADBEEF, x, ~0xDEADBEEF} (0x4071F0; BL1 copies the mailbox
  at 0xB1005000 and clears word 0, 0x40778C), or PersParam **BLDb** low 16 bits = 1 (0x4070EC). A third source,
  0x40767C, is a stub returning 0 (no rescue-pin check in this build).
- Image valid (0x403F98): descriptor from MetaInfo, CRC32 over the partition data = MetaInfo CRC. Start (0x403D0C):
  boot header 0xAAAA5555, 1 frame, 0xA5A5A5A5, stage 1 copied to its load address, stage-1 CRC, "Starting %s", jump.
- Serial: **UART1** (0x407610 returns 1; switch matrix +0x30), **115200** (divisor from 0x1C200, 0x403AD0), RX
  through the FIFO window (+0xE00, read by its RX ISR at 0x403608 via VIC 22; init writes +0x000/4/8, +0xF10..F34,
  interrupt mask +0xF50/F54 = 0x300). The 2 s window (0x4044DC) is polled again just before an image is started. A
  **0x01 or 0x02** in the window opens the **menu** (0x02: on the unit with `zd1 module esc`, 2026-10-23; 0x01: in the
  emulator); every other byte is ignored. The +0x24 poll (0x404B98, bit 0x100) is the TX flush before a reset.
- Menu (0x4045D0): 1 download BL2 (XModem; the received .bcd header is printed by 0x405B60; the write path not
  traced), 2 meta info, 3 reset, 4 start BL2, 5 start app, 6 erase flash (y/Y confirm; range not traced: avoid).
- Timer 1 is programmed one-shot with /16 prescale; timeouts are "threshold <= counter >> 4" (helper 0x405828).
- **Encrypted, and partly compressed.** The NAND block-0 copy is ROM-encrypted (length-preserving: 0x869C used both
  on NAND and in SRAM). The decrypted image runs an ARM scatter-load at 0x400018 over a region table at 0x40843C with
  three handlers: 0x4000C4 block copy, 0x4000EC zero-fill (bss), and **0x400050 an LZ/RLE decompressor** (control
  byte: low bits = literal-run, bit 3 = back-reference, high nibble = copy length). One initialised-data region is
  routed through the decompressor and unpacked at start-up. The code is stored decompressed and runs in place, so the
  SRAM capture at stage-2 entry is the fully unpacked BL1.

## Running it in the emulator
`bl1_plain` can't be re-run as is (its packed data was overwritten by the unpacked copy). A re-runnable image =
`sram_at_stage2_entry.bin[:0x8A08]` with 0x400010 patched to branch to 0x400688 (data in its post-boot state):
`blobs/module/bl1.run`. `zemu run --nand --real-bl1` runs it and sets:
- `core TimerFreerun true`: one-shot timers fire once and keep counting, so BL1's `threshold <= counter >> 4` helper
  sees its deadline;
- `core TimerWaitPcs "405830"`: a counter read at 0x405830 with the deadline ahead halts the CPU until it (BL1's 2 s
  window, flash waits and start-up delays spin on that helper; at Renode's MMIO rate the window alone would cost
  minutes).

Run with the real BL1 [EMU, the scenario suite up to b31219b]: the stock app and our images from NAND, HCR 0x21 →
mailbox → BL2, a web burn, BL2's reset, BL1 validating and starting the burned image (also after a failed burn in
the same BL2 session), the serial window (0x01, 0x02, the menu into BL2, a blind repeat of `0x02 '4' CR` from reset
release), an erased app partition → BL2, and HCR 0x21 from the app → "Forced start of Bootloader Level 2" → BL2. An idle-high line is the normal boot; a line
held low would be a break, which the model does not represent.
