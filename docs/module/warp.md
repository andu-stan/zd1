# Warp and Sol: the DM870's coprocessors

Spec for the DM870's two Warp MIPS cores and the Sol DSP datapath: bring-up from the ARM, the cores' ISA and memory
map, Sol's programming model and encoding, and Warp0 as a compute coprocessor for our Linux. Sections 8 and 9 are for
whoever continues the reverse engineering.

Evidence (`docs/README.md`): **[HW]** measured on our unit (under stock KnOS 9112, 2026-10-21/22; under our OS,
2026-10-23), **[RE]** read from vendor code, **[EMU]** emulator only, **[INF]** inferred, untested. Untagged = [HW].
Where Sol's hardware differs from KnOS's ARM reference model of it, this spec gives the hardware. File names in
parentheses are raw probe results in `re/netmodule/sol-probes/`: `hw-2026-10-22/` and `hw-2026-10-22b/` (the first
two passes, under KnOS: only the results this spec still cites),
`fuzz-*.json` (the Sol fuzzer's ALU traces) and `hw-2026-10-23/` (third pass, our OS: register file, predication, loop
counters, unit 2, MAC timing, SuluDMA, contention; `p4-*.json` fourth pass; `p5-*.json` fifth pass, 2026-10-23 late:
the sequencer's byte ALU and wait words, the return, the control bytes as unit enables, the register-file window,
unit 2's window, the bit-reverse width, 0xFFFF030C). A later file supersedes an earlier one where they disagree.

## 1. Overview

| | Warp0 | Warp1 | Sol |
|---|---|---|---|
| What | scalar MIPS I core | scalar MIPS I core | dataflow VLIW DSP datapath: ALU (64-bit and 2 x 32 SIMD), two 32 x 16 MACs, 7 registers, a sequencer with 8 loop counters, a byte ALU on its control block, calls, waits and predicated execution |
| Clock | 160 MHz (`clock.json`) | 160 MHz | 160 MHz |
| Stock role | "SecureWarp": BWOS, Microsoft DRM (Nokia) | "WarpSol": runs the audio-effect program | executes the kernels Warp1 loads |
| Bus master (`sys umr`) | `SECWARPI` / `SECWARPD` | `SOLWARP` (shared with Sol) | |
| Debug block (ARM) | 0xE0014000 | 0xE0015000 | none: control block (section 5) |
| Local RAM | 48 KB, ARM 0xAC000000 | 10 KB = Sol data memory, ARM 0xA8000000 | Warp1's local RAM; address unit 2 has 1 KB of its own |
| Power gate (tgen) | bit 2 | bits 20-22 (Warp1/Sol) | |
| State under stock KnOS and our OS | held in reset (no BWOS on our unit) | held in reset | idle |

The DM870 is a BridgeCo "JukeBlox" tri-core: ARM926 (240 MHz), security processor (Warp0), audio engine (Warp1 +
Sol). Warp0 has no Sol: it is the plain integer core to use for our own compute (section 6). Sol runs only while
Warp1 is out of reset.

## 2. Control from the ARM

### Power and clock gates (tgen, 0xE0007000)

| reg | |
|---|---|
| +0x20 / +0x24 | reset set / clear |
| +0x28 | enable set (write `1 << gate`); reads the enable mask |
| +0x2C | disable (clear) |

`sulu_power_enable(n)` writes +0x28. Power-up state on our unit: 0x00FFFFED (gates 2 and 20-22 on), also with our OS
booted (2026-10-23). **Sol reset = a pulse on gate 21** (+0x20 = 1 << 21, then +0x24 = 1 << 21): it clears r1-r7, the
loop counters (0-7, 12, 13) and the Z/C flags (Z = 1, C = 0 afterwards), and returns the MAC to its power-on configuration
(the flags and unit 2's latch survive), and it recovers a wedged Sol (section 7) (`p4-wedge.json`). Pulses on gates 2,
20 and 22 left Warp0, Warp1 and Sol working.

### Debug block (0xE0014000 + core * 0x1000; Linux virt 0xF0014000 + core * 0x1000)

| offset | |
|---|---|
| +0x00 | write an instruction: the core executes it in debug mode |
| +0x04 | data to the core (`mfc1 $x, $22`) |
| +0x08 | data from the core (`mtc1 $x, $22`) |
| +0x0C | bit 0 halt request (rw), bit 1 in debug mode (r) |
| +0x10 | 0 = held in reset, 1 = released |
| +0x1C | busy: poll before and after each +0x00 write |

Debug registers (COP1): 20 debug mode (`mtc1 $0, $20` leaves it), 21 DebugPC (resume address), 22 RxTx, 23 PC
(reads 4 before the first resume).

| operation | writes |
|---|---|
| halt into debug mode | +0x0C = 1, +0x10 = 0, +0x10 = 1; +0x0C then reads 3 |
| set PC and start | +0x04 = pc; exec `mfc1 $t0,$22` (0x4408B000), `mtc1 $t0,$21` (0x4488A800); +0x0C = 0; exec `mtc1 $0,$20` (0x4480A000) |
| single step | halt request set, exec `mtc1 $0,$20`: one instruction, then debug mode again |
| read a value | exec an instruction that puts it in `$t0`, exec `mtc1 $t0,$22` (0x4488B000), read +0x08 |
| park (as KnOS leaves it) | +0x0C = 1, +0x10 = 1, +0x10 = 0, +0x0C = 0 |

Park also recovers a core hung on a bus access (limits in section 7).

### Vendor drivers (Nokia HD-1 Linux, in our OS rootfs, not loaded at boot)
- **`warp_debugger.ko`** [RE]: `warp_{stop,start,reset,step,setpc,getpc,getreg,...}` for both cores; debugfs
  `warp0`/`warp1`. The other two build on it.
- **`wsl.ko`** (BridgeCo, GPL): Warp1. `open("/dev/wsl_admin")` (misc 10:248; `/dev/wsl_queue` = 10:249) [RE]:
  `sulu_power_enable(20, 21, 22)`, `ioremap(0x61FC0000, 256 KB)` (the program window, also its `mmap`), `warp_stop(1)`,
  `warp_reset(1)`, an 84-byte mailbox in SRAM whose address goes to Warp1 local word 0, two SuluDMA block-move
  contexts (`newdma_blockmove_create`), IRQ 51. `munmap`: stop, reset, `warp_setpc(1, 0x61FC0000)`, `warp_start(1)`;
  `close` frees the contexts and the mailbox and leaves the core alone. On our unit under our OS [HW 2026-10-23]: the
  mailbox at SRAM 0x0040A750 holds the two block-move descriptors {context, client, handle} at +0x3C and +0x48:
  **context 35 / client 3, context 36 / client 4**. The kernel leaves them Flags0 0x188100A3 / 0x188100A4 (mode 3,
  next = itself), Flags1 0x01580000 (32-bit read and write), addresses 0xDEADBEEF. `/dev/wsl_queue` and `libwsl.so`
  carry Nokia's sample protocol (section 8).
- **`securewarp.ko`** [RE]: Warp0. Maps 0xAC000000 and an 8-byte mailbox in the 0xFFFF0300 page,
  `sulu_power_enable(2)`, stop, reset; release starts BWOS at 0x5C. It holds no BWOS code (Nokia's boot chain loads
  it).

### Our loader
`tools/zeppelin/warp/warprun.py`: either core, from a Mac through the stock KnOS shell or our OS's `zd1-memsh`
(section 8).

## 3. Warp core

### Instruction set
Little-endian **MIPS I integer**. Differences from a textbook R3000:
- **No divider**: `div`/`divu` leave HI/LO = 0. `mult`/`multu` give the full 64-bit result, ready 4 cycles after the
  multiply (an `mflo` right after it stalls ~3 cycles); a 32 x 32 -> 64 multiply-accumulate step (multu, mflo, mfhi,
  four adds) takes ~10 cycles, ~7 with two interleaved [HW 2026-10-24, Warp0, `w0probe.py mul`].
- **No MIPS II/32**, and nothing traps: **`movz`/`movn` (either condition) and SPECIAL2 `mul` and `clz` copy rs
  into rd**; `beql`/`bnel` neither branch nor skip their delay slot; `ll` does nothing (the target keeps its value);
  `sc` leaves its register alone and stores **0** [HW 2026-10-25, Warp1, `w0probe.py odd`, `unaligned`]. The rest
  untested. `lwl`/`lwr` leave the target unchanged.
- **Strict load delay**: the instruction after a load sees the load's *address* in the target register; if that
  instruction writes the register itself, its value stays and the load's is lost [HW 2026-10-25, `w0probe.py delay`].
  Fill every load-delay slot. Branch delay slots as MIPS I.
- **No exceptions or interrupts**: `syscall`, `break`, `teq`, `add` overflow and unknown opcodes continue.
  Unaligned `sw` drops the low address bits. Unaligned loads, from 0x44332211 0x88776655 [HW 2026-10-25,
  `w0probe.py unaligned`, one data pattern]: `lh`/`lhu` at +1 / +3 give the addressed byte twice (0x2222, 0x4444);
  `lw` at +1 gives 0x44332222 (byte 0 replaced by the addressed byte), at +2 0x66556655 and at +3 0x66556666 (bytes of
  the next word).
- COP0: reg 0 = 1, 1 = 1, 5 = 6 = 0xFFFFFFFF, 9 = 1, 13 = 1 (bit 30 set after a cache upset, cleared by reset), the
  rest 0 (no PRId, no cycle counter). COP1 = debug registers. COP2 reads 0. COP3 register 0 = cache control.

### Instruction cache (COP3 register 0)
- **4 KB, 2-way set-associative, 64 sets of 32-byte lines**, both cores [HW 2026-10-25, Warp1, `w0probe.py tags`: 128
  bytes of code at 0x61FCA800 filled sets 0-3, set 0 in the second way; the invalidate stub's tail at 0x61FC7F20-7F5F
  sets 57 and 58; by timing, `w0probe.py assoc`: two one-line functions 1, 2, 4 or 8 KB apart both stay cached, three
  2 KB apart miss on every call, A, A + 1 KB and A + 2 KB all hit]. **Replacement is (pseudo-)random and ignores an
  empty way** (`present`: after A then B, from empty, A was gone in 4 of 12 runs; after A B A C, A and B each stayed in
  6 of 12: not LRU, not FIFO). **Bit 0 or bit 2 turns it on**, each alone: COP3 = 1, 4 and 5 all run loops of 1 to 16 lines
  from SDRAM at 1.00-1.06 cycles per instruction and fill tags, 0 runs them at ~19.5 (`w0probe.py bit0`, with Wi-Fi
  down and up alike) [HW 2026-10-25]; 0 after a Warp reset. Bit 4 (0x10) maps the
  cache RAM. Code in the local RAM is never cached (`w0probe.py localcache`: rewritten there, it ran new with the
  cache on, while the same test in SDRAM ran stale).
- Cycles per instruction of a straight-line loop from SDRAM, cache on (`w0probe.py bigloop`, ~1 s runs): 1.0 up to
  4 KB, 2.72 at 5 KB, 3.90 at 6 KB, 4.70 at 8 KB, 5.07 at 12 KB, 5.31 at 16 KB, which random replacement with a
  **34-cycle line fill** reproduces within 3 %; uncached 19.5. A call that hits right after a miss in the same set costs
  more than a plain hit (the miss counts by timing came out above what any policy allows). Runs shorter than ~1 s are distorted by the 20 ms completion polling.
- **It survives a Warp reset**, and writing 0x10 then 5 does not invalidate it: new code at an address that already
  ran executes the stale instructions.
- **Invalidate (both cores)**: COP3 = 0x10, zero the tag RAM 0x02000000-0x020005FF, COP3 = 5 (`warprun.Warp.
  icache_invalidate()`; code at a reused address then runs new, `p4-icache.json`). The data RAM need not be cleared.
  Clearing only 0x10000 leaves Warp1 running wild; touching 0x10000 with mode 5 wedges it until park. Without an
  invalidate, `warprun.code_slot()` places code by its content, so one address always holds the same code.
- **Cache RAM view** (COP3 bit 4), the same on both cores: tags at 0x02000000-0x020005FF, the 0x600-byte window the
  same 0x200 bytes three times; an entry holds 0x300000 | address bits 30:11 (0x3C3F95 for 0x61FCA800); **bit 20 is
  the valid bit** (an entry written back with it clear refetches, with bit 21 clear stays valid: `tagbits`); data at
  0x10000-0x10FFF. Not settled: which word shows which way (a sequential dump put set 0's lines at words 0 and 64,
  single reads after A then B showed only B), and the data view's offset (an early probe saw each word 8 bytes early,
  2026-10-25's direct reads (`viewread`) saw them in place).

### Memory
**Data is never cached.** Access cost (cycles):

| | Warp0 | Warp1 |
|---|---|---|
| own local RAM: load / store | **1 / 1**, also back to back (our OS, `w0probe.py mem`, `hw-2026-10-24/w0probe.txt`) | **1 / 1** (our OS, Sol idle, `w0probe.py --core 1 mem`, `hw-2026-10-24/w1probe.txt`); 1.8 load under KnOS |
| SDRAM: load / store | **~19 / ~7** under our OS, Linux idle (`w0probe.py mem`; `contention2-alone.json`: 18); 31 load under KnOS (AV CTR) | ~19 / ~7 (our OS); 31 load (KnOS) |
| on-chip SRAM (Warp 0x04000000 = ARM 0x00400000, 64 KB): load / store | ~6 / 1 (`w0probe.py sram`, `bus`; `hw-2026-10-24/sram-map.txt`) | ~6 / 1 |

**Both Warps run code from their local RAM at one instruction per cycle**, with the I-cache on or off (COP3 = 5 or 0),
while loading from the same RAM at one cycle (`w0probe.py [--core 1] exec`): Warp0's 48 KB and Warp1's 10 KB are
full-speed code and data memory, as BWOS uses Warp0's (`securewarp.ko` starts it at 0x5C). Warp1's 10 KB are Sol's
data memory too: code there must keep out of the banks a Sol program uses. Code in SDRAM runs through the 4 KB
I-cache below. multu's result is readable after ~3 stall cycles on both cores (`w0probe.py mul`). Timing loops through the I-cache need an
invalidate first: before one, a Warp1 loop at a reused address ran at 79 cycles per iteration instead of 5, twice,
and never after it.

| Warp address | Warp1 | Warp0 |
|---|---|---|
| 0x00000000 | local RAM 10 KB (0x0000-0x27FF) = Sol data memory = ARM 0xA8000000; 0x2800-0x3FFF read 0xE and drop stores (also after address unit 2 stored: its memory is not there, `au2.json`); the 16 KB pattern repeats to 0xFFFF (tags at every 1 KB, our OS: `w0probe.py --core 1 map`, `sram-map.txt`) | local RAM 48 KB (0x0000-0xBFFF) = ARM 0xAC000000; 0xC000 read 0xE with COP3 bit 4 set, hangs without it |
| 0x00010000 | I-cache data RAM with COP3 bit 4, else **hangs** | I-cache data RAM with COP3 bit 4 (`warp0-cachemap.json`), else hangs |
| 0x01xxxxxx | Sol = ARM 0xD0xxxxxx (Warp = ARM - 0xCF000000) | **hangs** |
| 0x02000000-0x020005FF | I-cache tags with COP3 bit 4, else reads 0xF04A | reads 0x000CCCCC with or without bit 4 (cache never enabled) |
| 0x03C00000-0x03FFFFFF | ARM 0x00000000-0x003FFFFF (Warp = ARM + 0x03C00000) [RE]; 0x03C00014 hangs | unprobed |
| 0x04000000 | ARM SRAM 0x00400000, 64 KB (0x08000000: same or alias); loads ~6 cycles | same |
| 0x40000000 | reads 0x20012001 | |
| 0x6xxxxxxx / 0x7xxxxxxx | SDRAM and its alias, as on the ARM | same |
| 0xB5xxxxxx | SuluDMA | same |
| 0xE0xxxxxx | peripherals, both debug blocks; loads ~10 cycles, stores 1 (posted; `w0probe.py bus`). AV CTR 0xE001B004 (24.576 MHz) for timing under KnOS (it reads 0 under our OS: nothing starts it, `memory-map.md`) | same |
| 0xFFFF0000 page | ARM vector/VIC/timer page; hardware semaphores +0x308, +0x30C | same |
| everything else | 0x10000000-0x3FFFFFFF, 0x80000000-0xCFFFFFFF: read 0xFFFF0000 | |

No boot ROM (nothing at 0x1FC00000/0xBFC00000), no kseg mapping. Reading the ARM timer in the 0xFFFF0000 page
coincided with a module hang under KnOS: time with the AV CTR (KnOS) or by wall clock (our OS, section 8).

### Warp <-> ARM signalling
- **Shared SDRAM**, uncached on the ARM side (`ioremap`, `dma_alloc_coherent`, `O_SYNC` `/dev/mem`, or the shell's
  0x7xxxxxxx alias). Code written through a cached ARM view is invisible to the Warp. Through an `O_SYNC` `/dev/mem`
  mapping (also of RAM inside Linux's 31 MB) the ARM copies at ~80-130 MB/s writing, ~57-88 MB/s reading with
  multi-word bursts (`memcpy`), ~9 MB/s one volatile word at a time; `recv()` into such a mapping works [HW
  2026-10-24].
- **Hardware semaphore 0xFFFF0308**: writing a non-zero ID takes it only if it reads 0; writing 0 releases it (ARM
  and Warp0 checked).
- **0xFFFF030C = a one-shot latch that raises IRQ 51.** The first non-zero write after a module power-up sticks
  (bits 4:0 of the value) and raised IRQ 51 (`wsl.ko`'s `wsl_admin`, count 0 -> 1). Measured right after a power
  cycle (2026-10-23, `irq-semaphores.txt`): the ARM's own write of 0x15 latched it, and nothing clears it: writes of 0, of
  the latched ID, of other values, byte and halfword writes, from the ARM, Warp0 and Warp1; writes to the rest of the
  page 0xFFFF0300-0xFFFF03FC; the interrupt controller's acknowledge read (+0x90) and EOI (+0x98 := 51); writes to
  0xFFFF0308; resets of tgen gates 2/20/21/22. Only a module power cycle (`irq-semaphores.txt`). `wsl.ko` treats it
  as a second lock semaphore (`wsl_mbox_open` maps 0xFFFF0308 and 0xFFFF030C; `wsl_lock_semaphore` writes the caller's
  ID and reads it back, `wsl_unlock_semaphore` writes 0; the ARM's ID is 0, so its lock never writes) and prints
  "About to unlock locSema with wrong procId (lock is N)" when it is latched. For our own designs: signal with
  0xFFFF0308 or shared memory and poll; IRQ 51 is usable once per power cycle.
- 0xFFFF0300 / +0x304: `securewarp.ko`'s 8-byte mailbox [RE].

## 4. Memory reservations on our OS
Our OS boots with `mem=31M` (`module/os/os.toml`). The top MB, 0x61F00000-0x61FFFFFF, is outside Linux: the boot
stub at 0x61F00000 (used only before the kernel), then the Warp0 runtime's window **0x61F00000-0x61FBFFFF** once the
kernel runs (section 6), and the Warp1 window **0x61FC0000-0x61FFFFFF**. The kernel panics in `kmem_cache_init`
unless `mem=` is whole MB.

## 5. Sol

### Architecture
BridgeCo's own **dataflow VLIW**, US 8,458,443 B2 ("VLIW processor with multiple data queues"; lapsed 2021; it
covers only the queue mechanics). Units: three address (load/store) units, an ALU, a MAC, a sequencer, and a
register file of seven 32-bit registers. An instruction issues one word to every unit. A load's data is in its
source in the **next instruction only**: read later, the source gives 0 (`srcs.json`); an ALU
or MAC result is readable from two instructions after the op on and stays until the next result.

### Control registers

| ARM | Warp1 | |
|---|---|---|
| 0xD0720000 | 0x01720000 | program memory 0 (address units): 0x400 bytes, aliases every 0x400 |
| 0xD0760000 | 0x01760000 | program memory 1 (ALU, MAC, register field, sequencer): 0x400 bytes, aliases every 0x400 |
| 0xD0740000 | 0x01740000 | the control block (below) |
| 0xD0700018 | 0x01700018 | written 0 at init |
| 0xD0400000 | 0x01400000 | **the register file**: word n = rn (r0 reads 0), repeating every 0x20 (`p5-stallpoke.json`: 0 5 10 0 0x44444444 0 0x66666666 0x77777777 with r1 = 5, r2 = 10, r4, r6, r7 set); read-only, no effect on execution |
| 0xD0C00000 + n * 0x40000 | 0x01C00000 + n * 0x40000 | unit n window: a 16-bit slot word is written **in the address**, window + (word << 2); data ignored. 3 = ALU, 4 = address unit 0, 5 = address unit 1, **6 = address unit 2** (`p5-u2window.json`: 0xCC05 through window 6 set unit 2's load stride). Windows 0-2 and 7-15: no effect found (a sequencer immediate through window 0 changed no counter). A load word through a unit's window is a real load: it advances the pointer and can release a wait on that unit's wrap (below) |
| 0xA8000000 | 0x00000000 | data memory = Warp1 local RAM |

**Drive Sol from Warp1.** Driving it from the ARM (KnOS's `sys sol`) hung the whole module twice.

**Control block** (byte offsets; the sequencer's immediates write bytes +0x00-0x1F, section "Sequencer slot";
`p4-ctrl.json`, `p4-ctrl17.json`, `p4-cond08.json`):

| byte | | power-on |
|---|---|---|
| +0x00-0x07 | loop counters 0-7 | 0 |
| +0x08 | **the zero byte** of the sequencer's byte ALU (below): it reads 0 to the ALU and ignores writes, so `+0x08 += byte` tests a byte for zero. Words that seem to set condition bits are byte-ALU ops on it, whose Z and C flags the 0xE0xx/0xE4xx jumps test. KnOS's test program's 0x6408 = `+0x08 += 1`: Z = C = 0 | - |
| +0x0C, +0x0D | **counters 12 and 13**: scratch bytes with the same reach as counters 0-7 (immediates, the byte ALU, `0xA0mk` with k = 12/13); nothing in the datapath reads them (`p5-ctrlbits.json`) | 0 |
| +0x14 | per-unit enables: **bit 7 ALU, bit 6 MAC, bit 5 register file** (clear: the ALU outputs 2 / the MAC outputs 0 / register reads return the last value). Bits 0-4 and the byte +0x15: no effect (`p5-ctrlbits.json`) | 0 |
| +0x16 | the same three enables: **bit 3 ALU, bit 2 MAC, bit 1 register file**; bits 0, 4-7: no effect | 0 |
| +0x17 | four 2-bit fields, 01 = normal: bits 1:0 sequencer (01 or 10 = halt; written before a start: wedged, section 7); bits 3:2 register file (00: source reads return the source-1 register; 1x: off, last value); bits 5:4 MAC and address units (00: the MAC outputs garbage; 10: the MAC off; 11: the MAC off and every load reads 0, every store writes 0); bits 7:6 ALU (01 works, else constant 2). Vendors: 0x54 | 0xFC |
| +0x18 | write 0: run; bit 0 reads done; writing 1 (byte 24 := 1, the end word 0x0038) ends the program, also from the ARM/Warp while Sol waits | 1 |
| +0x1C | reads 0xFFFF0000, writes ignored (the ALU sees bytes 0x1C/0x1D as 0, 0x1E/0x1F as 0xFF); no effect on execution | 0xFFFF0000 |
| +0x20 | write: the start (instruction x 4); read: the instruction being fetched x 4 (3 ahead of the one executing); **bit 17 = Z, bit 18 = C** (the byte ALU's flags), bit 16 set after a run that executed a commit (0x88xx) (`p5-scratch.json`, `p5-loose.json`) | 0 |

The ARM reads back only the bytes above; the byte ALU sees +0x09-0x0B, +0x0E-0x13, +0x15 and +0x19-0x1D as 0
whatever was written (`p5-scratch.json`), so the control block has exactly ten scratch bytes (counters 0-7, 12, 13).

### Running a program
1. **Init** (once): +0x14 = 0xE0 (16-bit write), +0x16 = 0x0E, +0x17 = 0x54 (8-bit), 0xD0700018 = 0, +0x18 = 1,
   +0x20 = 4. Meaning of the bits: the control block above. **Do not set +0x17 bit 0 or 1 before a start**: Sol then
   never starts and stays wedged with 0x54 restored, until a Sol reset (`p4-wedge.json`); set from inside a program
   they only halt it (restoring 0x54 from the Warp resumes it, `p5-stallpoke.json`). **Do not write a start (+0x20,
   +0x18 = 0) while Sol runs or waits**: that wedges it the same way (`p5-ecbits.json`: a second case started while
   the first looped). Check done first; to abort a waiting program write +0x18 := 1 (done), then park Warp1.
2. **Load** the program into both memories: 128 instructions; usable 0-126 (an end word in 127 makes the whole
   program do nothing). Several programs can share the memories.
3. **Reset the address units** before every run: units 4 and 5 0xF00F and every register group with value 0;
   unit 4 0xE476, unit 5 0xE498 (default bank maps); unit 4 0xE200; unit 3 0x8C02 (`Modeset 2`).
   `warprun.sol_unit_clear()` has KnOS's list, which lacks 0xE200 (`solprobe.py` adds it) and does not reach unit 2.
4. **Start**: +0x20 = 4 x the first instruction, +0x18 = 0. **Poll +0x18 bit 0 with a bound.** The vendors write
   4 (Nokia: its offset + 4), so **instruction 0 never runs**: nothing placed there (KnOS's test program's 0xF00F,
   an immediate, a register word) takes effect (`p4-entry.json`). Put a no-op there.

**State that survives runs and the reset above**: registers r1-r7, the flags, loop counters 0-7 and 12/13, the Z/C flags,
the last non-zero register field, the ALU mode, the MAC configuration, accumulators and pending output shift, every
address unit's configuration registers, unit 2's memory and load latch, 0xE2xx. A Sol reset (section 2) clears part
of it. Each program sets what it relies on, as the vendors do: `Modeset`, MAC word 0x2C00 (configuration
0xB0), the address units' registers followed by 0xF00F, and the counts of its loops. Power-on state differs from that
set-up.

### Data memory
10 banks of 1 KB (256 words) at Warp1 0x0000-0x27FF. A 64-bit Sol value is two 32-bit lanes, each in its own bank.
Each address unit's **bank map** (0xE4xx) sets lane 0's bank (bits 3:0) and lane 1's bank (bits 7:4); defaults unit
4 = 0x76 (banks 6/7), unit 5 = 0x98 (banks 8/9). All banks are usable, together. A 512-point complex FFT in 32-bit
(2 x 512 words) takes 4 banks. Address unit 2 has a separate memory (below).

### Instruction word
Instruction i is the 64-bit word i of **both** program memories.

| memory | 63:48 | 47:32 | 31:16 | 15:0 |
|---|---|---|---|---|
| mem1 (0xD0760000) | ALU slot | MAC slot | register field | sequencer |
| mem0 (0xD0720000) | always 0 | address unit 2 | address unit 1 | address unit 0 |

No-op: mem1 = 0x000000000000A000, mem0 = 0.

### Register file and the register field (mem1 31:16)
Seven 32-bit registers r1-r7; **r0 reads 0 and ignores writes** (`regs.json`, `regwr.json`).

| bits | |
|---|---|
| 14:12 | the register read as **source 0** in this instruction (bit 15 ignored) |
| 10:8 | the register read as **source 1** (bit 11 ignored) |
| 7:4 | destination register of a write (0 = none) |
| 3:0 | the source written: any source number of the table below; the register takes its lane 0 |

- A register reads as the same 32-bit value in both lanes.
- **A register field of 0 keeps the last non-zero field's selections** (the write happens only in the instruction
  that has it). The selection also carries over into the next run: a program that reads a register sets the field.
- A read and a write in one instruction: the read sees the old value. An ALU result is written from two instructions
  after the op on (an earlier write takes the previous result).
- **Commit `0x88Nx`** (sequencer) writes rN := loop counters 3..0 as one 32-bit value (byte 0 = counter 0). This is
  how programs load constants: four immediates, then a commit (KnOS P1: r7 = 0x007FFFFF and r6 = 0xFF800000, the
  24-bit sample limits).

### Sources (ALU operands, MAC operands, register writes)

| source | |
|---|---|
| 0 | the register selected by the register field bits 14:12 |
| 1 | the register selected by bits 10:8 |
| 2 | the **MAC output** (the op two instructions back) |
| 3 | the **ALU result** (the op two instructions back) |
| 4 / 5 | the data address unit 0 / 1 loaded in the previous instruction (0 one instruction later) |
| 6 | address unit 2's **load latch**: the last value it loaded, kept until its next load, across runs (`p4-src67.json`) |
| 7 | source 3 OR source 6 (`p4-src67.json`); no vendor program uses it |

### ALU slot: `op[15:10] dst[9:6] src2[5:3] src1[2:0]`
- **Immediate ops** (0x23, 0x25-0x2B) take src1 only; operand bits 9:3 are the immediate (Modeset: bits 3:0).
- Values are 64-bit, two 32-bit lanes. A = src1, B = src2, n = shift count.
- **dst**: no effect on the 64-bit ops, the shifts and the rotates (0x14, 0x21, 0x22). On the **SIMD add/sub ops
  (0x18-0x20)** it picks operand A per lane (`lanes.json`, `p4-simd.json`): bits 7:6 = lane 0's A: 0 = A.lane0, 1 =
  A.lane1, 2 / 3 = 0;
  bits 9:8 = lane 1's A: 0 = A.lane0, 1 = A.lane1, 2 = A.lane0 >> 16, 3 = A.lane1 >> 16. **dst 4 is the plain
  per-lane op** (KnOS's test program); dst 0 applies lane 0 of A to both lanes (the vendors' clamp compare).

| op | name | result | op | name | result |
|---|---|---|---|---|---|
| 0x00 | - | constant 2 | 0x14 | Shift1 | per lane: A shifted by n (logical) |
| 0x01 | Not | ~A | 0x15 | Shift2 | the bits Shift1 moves across lanes; Shift1 \| Shift2 = 64-bit shift |
| 0x02 | Or | A \| B | 0x16 | Rot1 | = Shift1 |
| 0x03 | Nor | ~(A \| B) | 0x17 | Rot2 | the bits a 64-bit rotate moves across lanes; Rot1 \| Rot2 = 64-bit rotate |
| 0x04 | And | A & B | 0x18 | SimdAdd | lane k: A.k + (B.k >> modeshift), wrapping |
| 0x05 | Nand | ~(A & B) | 0x19 | SimdSub | lane k: A.k - (B.k >> modeshift), wrapping |
| 0x06 | Xor | A ^ B | 0x1A, 0x1C, 0x1E | SimdAddA/S/AS | = SimdAdd, in every mode (`alu4.json`) |
| 0x07-0x0A | - | 0 | 0x1B, 0x1D, 0x1F | SimdSubA/S/AS | = SimdSub, in every mode |
| 0x0B | Add | A + B (64-bit, wrapping; B shifted by the mode, like every add/sub op) | 0x20 | SimdComp | = SimdSub |
| 0x0C | Sub | A - B, wrapping | 0x21 | SimdShift | = Shift1 |
| 0x0D | AddA | (A + B) >> 1 of the 65-bit sum | 0x22 | SimdRot | per lane: A rotl (n mod 32) |
| 0x0E | SubA | A - B | 0x23 | Modeset | mode = operand bits 3:0 |
| 0x0F | AddS | A + B, 64-bit saturated: **unsigned with mode bit 0 clear, signed with it set** (`alu4.json`) | 0x24 | Moderead | mode (sign-extended) |
| 0x10 | SubS | A - B, wrapping (no saturation in any mode) | 0x25 / 0x26 | - | lane 0: A.lane0 ± imm (7 bits); lane 1 = A.lane1 |
| 0x11 | AddAS | AddS >> 1 | 0x27, 0x29, 0x2B | - | per lane: A shifted by imm (bits 9:3, signed): positive = left, **negative = logical right**; a count past the lane gives 0; -64 leaves A (`alu4.json`). Imm 0 = move |
| 0x12 | SubAS | A - B, wrapping | 0x28 | - | Shift2 of A by imm (bits 9:3, signed) |
| 0x13 | Comp | A - B, wrapping | 0x2A | - | Rot2 of A by imm (bits 9:3, signed) |
| | | | 0x2C | - | per lane: src1 rotated left by the **src2 field value** (0-7; the usual 0x12C operand = rotate by 5) (`alu3.json`) |
| | | | 0x2D-0x3F | - | 0 |

- **Shift count** n = B.lane0 bits 5:0, signed: positive left, negative right; |n| >= 32 gives 0 (Shift1).
- **Mode** (`Modeset`, power-on 2): bits 3:2 right-shift the second operand of **every add/sub op** (0x0B-0x13,
  0x18-0x20; the shifts and rotates ignore the mode) by 0/2/3/4 bits. **Bit 0** selects signed saturation for
  AddS/AddAS (clear: unsigned). Bit 1 changed nothing measured (`alu2.json`); operand bits 4-9 left moves unchanged
  (`clamp.json`).
- **Flags**: the arithmetic ops (Add, Sub, Comp and their SIMD forms, `flagops.json`) set per-lane flags: lane k > 0,
  < 0 (= 0 when neither). Logic ops (Or, Not), moves (0x2B) and the MAC leave them. They feed the predicates below.
- Names are KnOS's (tables at ARM 0x60901C2C ALU, 0x60901CC0 MAC).

### Predicated instructions
A sequencer word **0x9800-0x9FFF or 0xD000-0xDFFF** makes its whole instruction conditional: when the condition is
false the instruction's ALU op, MAC op, register write and stores do not happen (loads not checked) (`cond.json`,
`cond2.json`, `macpred.json`).

- Two terms, each true or false (`condsweep.json`, 544 words x 9 lane cases), each a source and a select:

  | | source | select |
  |---|---|---|
  | term A | bits 9:7 | bits 6:5 |
  | term B | bits 4:2 | bits 1:0 |

  Source 3 = **the ALU flags**: select 0 = lane 0 > 0, 1 = lane 0 < 0, 2 = lane 1 > 0, 3 = lane 1 < 0. The flags
  are those of the last flag-setting ALU op **three or more instructions before** (an op at F is seen by a word at
  F + 3; at F + 1 and F + 2 the word still sees the flags before it); they persist across runs. Sources 4 / 5 / 6 =
  **address unit 0 / 1 / 2**: select 0 = its lane-1 load pointer wrapped (modulo) at its last lane-1 load; selects
  1-3 never true: not a lane-0 wrap, stores, bit-reversed or lane-swapped loads, nor any of the unknown register
  groups (`p4-auwrap.json`, `p5-aupred.json`). Sources 0, 1, 2, 7: always false (not the MAC, the loads, register writes or
  the sequencer's Z/C flags, `flagops.json`).
- Combine (bits 14 and 11:10, `condcombine.json`):

  | word | 11:10 = 00 | 01 | 10 | 11 |
  |---|---|---|---|---|
  | 0x9xxx | (counters 1..0 := rN[15:0], below) | (counters 3..0 := rN, below) | **!A & B** | **A & B** |
  | 0xDxxx | !A \| B | **A \| B** | false | !(A \| B) |

- The vendors' words: **0xD58C** lane 0 > 0, **0xD5AD** lane 0 < 0, **0xDD8D** lane 0 = 0; and the clamp of x
  between limits after a SimdSub with dst 0 of x against B = {lane 1 max, lane 0 min}: **0x9D8F** (lane 1 < 0 and
  lane 0 > 0: min < x < max) moves x, **0x998F** (lane 1 < 0 and lane 0 <= 0: x <= min) moves min, **0x99EC** (lane 1
  >= 0 and lane 0 > 0: x >= max) moves max (KnOS P1-P5).
- **Words with bit 12 clear are waits, not predicates** (`p5-cwords.json`: every term pair under four flag states):

  | word | |
  |---|---|
  | 0x80xx | wait while A \| B |
  | 0xC0xx | wait while A & B |
  | 0xC4xx | wait while A & !B |
  | 0xC8xx | wait until A \| B (while neither) |
  | 0xCCxx | predicate: execute unless A & B |

  A wait re-evaluates its terms in place every cycle (fetch stays at J + 1, +0x20 bits 18:16 show the flags). Flag
  terms cannot change while Sol waits (nothing issues), so a wait on them is forever; **address-unit terms can: a
  load word written through the unit's window (section "Control registers") advances its pointer, and the wrap
  releases the wait** (`p5-waitwrap.json`: 0xC810 = until unit 0 wraps, released by Warp1's loads through window 4,
  not window 5). That is the Warp -> Sol handshake. Also released by: patching the word in program memory to 0xA000
  (Sol re-fetches it); +0x18 := 1 ends the run. Not released by: the control bytes, +0x08, unit 3's window, windows
  0-2/6/7, 0x01700018, the status words (`p5-stallpoke.json`). Parking Warp1 ends it; the next run's first store
  is then stale. 0x8xxx with bits 11:10 = 01/10/11 are the jump, commit and counter words of the sequencer table.

### MAC slot: `op[15:14] macmode[13:6] src2[5:3] src1[2:0]`

| op | |
|---|---|
| 0 | **configure**: macmode is the configuration; sources ignored; persists across runs |
| 1 | Mac: accumulators += products; outputs them |
| 2 | Mult: outputs the products; accumulators unchanged |
| 3 | InitMac: accumulators = products; outputs them |

KnOS's "signed" names (SMac/SMult/SInitMac) are ops 1-3 with macmode bit 7 set.

- **Two products** per op, each a 32-bit lane of src2 times a sign-extended short of src1 (short 0-3 = src1 bits
  15:0 ... 63:48), the selects in macmode: P1 = src2.lane[macmode bit 7] x src1.short[macmode bits 5:4] into
  accumulator A, P2 = src2.lane[bit 6] x src1.short[bits 3:2] into accumulator B. Sources as in the table above
  (registers, loads, the ALU result).
- **Timing**: fully pipelined, one op per instruction; the output is readable (source 2, or a store 0x60F2) **two
  instructions after the op**: InitMac, Mac, Mac back to back give their sums at +2, +3, +4 (`macpipe.json`).
- **Output shift: macmode bits 1:0 = 0, 4, 7, 16 bits** right, arithmetic; the output is the low 32 bits of the
  shifted value (`macshift.json`, `macneg.json`). **The shift in a MAC word applies to the next MAC op's output**,
  a configuration word's included: configure 0xB3 then a Mult with code 0 gives a >> 16 output (`macpipe.json`).
  Set the shift one op ahead, as Nokia's biquad does (Mac 0x13 at 27 for the output read at 30).
- **Configuration** (`maccfg.json`): **dual** (output lane 1 = A, lane 0 = B) unless bit 7 = 1 and bit 5 = 0, which
  is **combined**: dual when macmode bit 7 != bit 6, otherwise the 64-bit output is B + (A << 16) (with shorts 1
  and 0 selected: a 32 x 32 product, KnOS's reference-model form). **0x00 changes nothing**, not even the pending
  shift. The vendors use 0xB0. Power-on is combined.
- **Accumulators**: bits 47:0 are what any output shows; they **wrap modulo 2^48** (sixteen 2^44 products read back
  0, then -2^44 reads 0xF0000000 through >> 16; no saturation, `macpipe.json`). Bits above 47 cannot be observed.
  They persist across runs.
- The MAC sets no flags; predicates apply to it.
- Results leave through source 2 or store word `0x60F2` (lane 1 = A, lane 0 = B).

### Address unit slots (mem0)
A slot holds an access word or a register word (`group | value`, in program order; takes effect at the next
instruction).

**Access word** (load both lanes: 0x7800; lane 0 only: 0x6800):

| bits | |
|---|---|
| 15 | 1 = no access |
| 14 | changing it swaps the lanes |
| 13 | 1 = load, 0 = store |
| 12 / 11 | lane 1 / lane 0 |
| 9 | bit-reversed addressing: the 8-bit pointer is bit-reversed (stride 1: words 0, 0x80, 0x40, 0xC0, ...) |
| 10, 8:0 | no visible effect (no load format or saturation either, `clamp.json`) |

**Store word** `0x6pXY`: X bit 3 write lane 1, bit 2 write lane 0, bits 1:0 lane 1's value (11 = result lane 1, 01 =
result lane 0, else 0); Y bits 1:0 lane 0's value (11 = ALU, 10 = MAC, 00 / 01 = zero: nothing else can be stored,
`queues.json`), bit 2 = take the result's lane 1; **p bit 0 packs**: each written word = lane 0 bits 15:0 | lane 1
bits 15:0 << 16. ALU result: 0x60FB; MAC: 0x60F2.

**Registers** (9-bit value; pointers are 8-bit word indexes within the bank):

| group | |
|---|---|
| 0xC000 / 0xC200 | lane 0 / lane 1 load pointer |
| 0xC400 / 0xC600 | lane 0 / lane 1 store pointer |
| 0xC800 / 0xCE00 | lane 0 / lane 1 **start offset** |
| 0xD400 / 0xDA00 | lane 0 / lane 1 **base** |
| 0xD800 / 0xDE00 | lane 0 / lane 1 **base step** at a wrap |
| 0xCC00 / 0xD200 | lane 0 / lane 1 **load stride** (words added after each load; default 0) |
| 0xCA00 / 0xD000 | lane 0 / lane 1 **load modulo** (0 = none; 0x100 = whole bank) |
| 0xE400 | bank map |
| 0xE000 | **bit-reverse width** for bit 9 of the access word: 8 - (value & 7) bits (0 = 8 bits: 0, 0x80, 0x40, ...; 1 = 7 bits: 0, 0x40, 0x20, ...; 4 = 4 bits: 0, 8, 4, 0xC, ...; 7 = 1 bit: in order). For an N-point FFT write 8 - log2 N (`p5-augroups.json`) |
| 0xE200 | bit 0: stores write 0. **Sticky** |
| 0xE600 | bit 0: the two lanes of a store go to alternate stores: store 1 lane 0's bank word 0, store 2 lane 1's bank word 0, store 3 lane 0's bank word 0xFF, store 4 lane 1's bank word 0xFF, ... (the pointer counts down every second store; `p5-e600.json`). Not "outside the banks": the bank map still applies |
| 0xD600, 0xDC00, 0xE800-0xEE00 | no effect found: load sequence (stride/modulo), bit-reversed loads, store positions, the wrap predicates, a lane-1 load of 0x8000, with values 1-0x1FF (`p5-augroups.json`, `p5-aupred.json`) |
| 0xF00F (whole word) | reset the pointers; the configuration registers are kept (vendor order: registers, then 0xF00F; the reverse works too) |

- **Load addressing** (per lane, `p4-pointer.json`): after 0xF00F the offset is the start offset (C8) and the base
  is D4; each load reads base + offset, then offset += stride; when the offset reaches the modulo it is reloaded with
  the start offset and the base moves by the step (D8). Modulo 4, stride 1: start 0 base 0 step 0 gives 0 1 2 3 0 1
  ...; start 1 gives 1 2 3 1 2 3; step 3 gives 0 1 2 3 3 4 5 6 6 7 ... (rows of a matrix). Lane 1's wrap feeds the
  predicates (source 4/5/6, select 0).
- Register words in instruction 0 never take effect (it does not run, section 5 "Running a program"): the earlier
  "unit 2's lane-0 modulo did not apply" and lost strides came from that.

- **Stores always step +1**; there is no store stride or modulo. For other output orders rewrite the store pointer
  from the program. Stores issued in the three instructions before a store-pointer write still advance the pointer
  after it (C420 after three stores: the next store lands at 0x23).
- **Address unit 2** (bits 47:32) has **its own memory: 128 words x 2 lanes** (1 KB), with the same access, store
  and register words; 256 stores leave the last 128 and reads wrap every 128 (`au2.json`). It touches none of the
  ten banks and is not visible to Warp1 or the ARM. Its loads are source 6. Nokia keeps the biquad state there (a
  3-word circular buffer: stride 1, modulo 3); KnOS P1/P5 keep the {max, min} clamp limits there. KnOS's clear list
  never reaches it: configure it in-slot before an 0xF00F, or through its window (unit 6).

### Sequencer slot (mem1 15:0)

The low half of the space is a **byte ALU on the 32 control bytes** (`d` = byte index 0-31: 0-7 loop counters, 8 the
zero byte, 12-13 counters 12/13, 20/22/23 the enables, 24 done/end; `p5-cntalu.json`, `p5-c08cmp.json`,
`p5-scratch.json`). Every op sets **Z** (result 0) and **C** (carry/borrow/shifted-out bit), the flags the 0xE0/0xE4
jumps test (power-on and after a Sol reset: Z = 1, C = 0; moves and the OR/AND-NOT words do not touch C).

| word | fields | |
|---|---|---|
| 0x0000-0x1FFF | `000 imm8[12:5] d[4:0]` | **byte[d] := imm8** (counter k := n; 0x0038 = byte 24 := 1 = **end**: later instructions do not issue, a store in the same instruction writes garbage; bytes 20/22/23 switch units off for the rest of the run, e.g. 0x0037 = +0x17 := 1 halts) |
| 0x2000-0x3FFF | `001 imm8 d` | byte[d] \|= imm8 |
| 0x4000-0x5FFF | `010 imm8 d` | byte[d] &= ~imm8 |
| 0x6000-0x63FF | `011 000 s[9:5] d[4:0]` | byte[d] += byte[s] (`+0x08 += byte[s]`: Z = byte[s] == 0) |
| 0x6400-0x67FF | `011 001 imm7[9:3] d[2:0]` | counter d += imm7 (0x6408 = `+0x08 += 1`: clears Z and C) |
| 0x6800-0x6BFF | `011 010 s d` | byte[d] -= byte[s] (`+0x08 -= byte[s]`: Z = byte == 0, C = byte != 0) |
| 0x6C00-0x6FFF | `011 011 imm7 d[2:0]` | counter d -= imm7 |
| 0x7000-0x73FF | `011 100 n[9:5] d` | byte[d] <<= n (C = the bit shifted out) |
| 0x7400-0x77FF | `011 101 n d` | byte[d] >>= n, logical (C = the bit shifted out) |
| 0x7800-0x7BFF | `011 110 s d` | byte[d] &= byte[s] |
| 0x7C00-0x7FFF | `011 111 s d` | byte[d] ^= byte[s] (`+0x08 ^= byte[s]`: Z = byte == 0) |
| 0x80xx | | **wait** while A \| B (terms as the predicates; above) |
| 0x84xx | | **jump** to J + 1 + bits 9:0 (signed, the program memory wraps at 128): 0x8402 skips one instruction, 0x87FF loops on itself |
| 0x88Nx (N = bits 2:0) | | **commit**: rN := counters 3..0 (`counters2.json`); bits 9:3 change nothing (`p5-counterwords.json`) |
| 0x8C0N | | counter 0 := rN[7:0] (bits 9:4 set: 0) |
| 0x900N / 0x940N | | counters 1..0 := rN[15:0] / **counters 3..0 := rN** (N >= 8, or bits 9:8 set: 0) |
| 0x9800-0x9FFF, 0xD000-0xDFFF | | predicates (above) |
| 0xA0mk | `m = bits 7:5, k = bits 4:0` | **byte[k] := counter m** (k = 12/13 reach +0x0C/+0x0D; bits 9:8 set: := 0). 0xA000 = next (`p4-counterops.json`) |
| 0xA4xx | | **return**: jump to counter 6 (the address the call saved), one delay slot; bits 9:0 ignored (`p5-ret.json`: tested 0xA4C6, 0xA461, 0xA406). With counter 6 = 0xFF (as in the earlier "never finishes" runs) it jumps to 127 and wraps to 0 |
| 0xA8xx | | **wait for an unknown event** (section 9): never released by counters, flags, control bytes, window words or a SuluDMA block move; a program-memory patch releases it |
| 0xACmk | | counter k \|= counter m |
| 0xB0mk | | counter k &= ~counter m (bits 9:3 other than m: no effect) |
| 0xB4mk, 0xB8mk | `pairs p = m >> 1, q = k >> 1` | **16-bit counter moves**: pair q := pair p when k is even; pair p := pair q when m is even; both (a swap) when both are even; nothing when both are odd. The same for B4 and B8 (`p5-b4b8.json`) |
| 0xBCmk | | **swap** counters k and m |
| 0xC0xx / 0xC4xx / 0xC8xx | | **wait** while A & B / while A & !B / until A \| B (above) |
| 0xCCxx | | predicate: unless A & B |
| 0xE0xx / 0xE4xx | | **conditional jump** to J + 1 + bits 9:1 when the flag selected by bit 0 (0 = Z, 1 = C) is set (0xE0) / clear (0xE4) (`p5-c08cmp.json`). A displacement of 127 (0xE3FE) loops on itself |
| 0xE800-0xEBFF | | no effect found: no jump under Z/C/N/V flag states, no counter or control-byte change (`p5-e8flags.json`, `p5-ecwrite.json`) |
| 0xEC00-0xEFFF | | **jump** to J + 1 + signed(bits 9:5), unconditional; bits 4:0 no effect (`p5-ectrace.json`: 0xEC40 +2, 0xEC80 +4, 0xED00 +8, 0xEDAA +13, 0xEE00 -16) |
| 0xF000-0xF3FF | | **counted jump**: counter k = bits 2:0 is decremented; if it is not 0, execution continues at **J + 1 + signed(bits 9:3)**: 0xF0xx/0xF1xx forward, 0xF2xx/0xF3xx backward (J - 63 + bits 8:3) (`jumps.json`, `p4-seqwords.json`) |
| 0xF400-0xF7FF | | **wait for an unknown event**, like 0xA8xx (not the counters: tested with counter k zero and non-zero, not Z/C, not DMA; `p5-a8wait.json`, `p5-dmawait.json`, `p5-loose.json`) |
| 0xF8xx | | **call**: counter 6 := J + 2 (the return address), jump to J + 1 + bits 9:0 (mod 128) (`p4-call.json`); return with 0xA4xx |
| 0xFCxx-0xFFxx | | jump to J + 1 + bits 9:0 (mod 128), no link |

**Loops** (`counters.json`, `jumps.json`, `jumpslots.json`, `jumpslots2.json`):
- **Eight 8-bit loop counters** (control bytes +0x00-0x07) plus two scratch bytes (+0x0C/+0x0D, "counters 12/13"),
  set by immediates (counter k := n), the byte ALU (+=, -=, shifts, and, or, xor, and-not), 0xA0mk / 0xB4mk / 0xBCmk
  (from another counter), 0x940N / 0x900N / 0x8C0N (from a register). A value of 0 runs 256 passes. They persist across runs and count down in place: a run
  ending inside a loop leaves the remainder to the next run.
- **The jump has one delay slot**: every pass runs S .. J + 1, the instruction after the jump included. All the
  vendor loops put a store there.
- Bodies up to 64 instructions (d = 0 returns to J - 63). Nest loops on different counters (KnOS P5: 27-63 on
  counter 5 around 36-53 on counter 4).
- Recipe: an immediate `n << 5 | k` before the loop (not in it: an immediate on the loop's counter inside the body
  re-arms it every pass and the loop never ends), the body S..J+1, at J the word `0xF200 | (S - J + 63) << 3 | k`.
- Instruction 0 does not run with the usual start offset 4 (section 5 "Running a program"): start at instruction 1.

### Timing
A load at instruction i is usable at i + 1 only; an ALU or MAC result reaches a store, a register write or another
op at n if the op is at n - 2 or earlier; flags reach a predicate at F + 3. About 1 instruction per cycle: a
37-instruction loop run 256 times takes ~60 us.

### Samples in and out
Sol sees only its 10 KB; Warp1 fills and drains it.
- **Warp1 copies** between SDRAM and local RAM: ~31 cycles per word (KnOS), ~50 us per 256-word bank.
- **SuluDMA block moves started by a Warp** [HW 2026-10-23, our OS, `dma.json`; Warp0: `p4-dma0.json`, the same
  code, 1 KB copied]: through a context the ARM set up
  (`wsl.ko`'s, section 2). Warp1 writes context c +0x0C = source, +0x08 = destination (bus addresses; Sol data
  memory = 0xA8000000), then `words | client << 16` to **0xB5008400 + c * 4**, and polls 0xB5000000 bit `client`
  until clear (Nokia's blob 0x4BA0 / 0x4C44 [RE]). 1 KB SDRAM -> Sol bank 6 (context 35, client 3) and Sol bank 7 ->
  SDRAM (context 36, client 4) copied exactly, each done within 10 polls of an uncached loop (under ~10 us [INF];
  a Warp1 copy loop takes ~50 us). The context's addresses advance by the length; its other fields stay as the
  kernel set them. The two earlier attempts, on context 40 with its Flags as found after power-up, took the module
  down (section 7) [INF: the unconfigured context is the cause]. Engine: `docs/module/memory-map.md` (SuluDMA).
- Signalling: shared memory, the semaphores and IRQ 51 (section 3).

## 6. Warp0 as a compute coprocessor
Our integer code on Warp0, driven from our Linux, without BWOS or `securewarp.ko`: the receivers' ciphers and
decoders (AirPlay's ALAC and AAC, Spotify's Vorbis, Snapcast's FLAC: `module/warp0/`); Warp1 stays free for audio.

**What you get**: 160 MIPS integer-only (no FPU, SIMD, MAC or divide; `multu` result after 4 cycles), no
interrupts, and **48 KB of local RAM that holds code and data at one cycle per instruction, load and store**: put
the firmware, its tables, keys and stack there, and keep only the buffers the ARM exchanges in SDRAM (~19 cycles a
load, ~7 a store; a 4 KB I-cache for code in SDRAM). Built that way (`module/warp0/`) its ciphers run at about the
ARM926's speed (`module/warp0/README.md`, Results on the unit). The ARM's side of a hand-off is cheap when it copies
in bursts (`memcpy` through the uncached window, ~0.03 us per byte in and out; section 3), so what pays is a whole
packet on Warp0: decrypted and decoded in one job.

**Bring-up** (here with `warp_debugger.ko`'s calls; our runtime does the same through the debug block over
`/dev/mem`, `module/warp0/README.md`, Boot):
1. Power: tgen 0xE0007028 bit 2 is on after our OS boots (0x00FFFFED); `sulu_power_enable(2)` if clear.
2. `warp_stop(0)`, `warp_reset(0)`.
3. Copy the image into the Warp0 window through an uncached mapping.
4. `warp_setpc(0, base)`, `warp_start(0)`.
5. `_start`: COP3 register 0 = 5 for code in SDRAM (0 is as fast for code in local RAM), set `$sp`, call `main`,
   spin on return. Before loading new code at an address that ran cached, invalidate the I-cache (COP3 = 0x10, zero
   the tags 0x02000000-0x020005FF, COP3 = 5; section 3).
6. Warp0 can also start SuluDMA block moves through a context the ARM set up (section 5, `p4-dma0.json`): input
   and output buffers can move without the core copying them.

**Sharing the SDRAM** (Warp0 SDRAM loads, I-cache on, cycles per load):

| load | our OS (wall clock, `contention2-*.json`) |
|---|---|
| Linux idle | 18.0 |
| Ethernet receive (a TCP stream from the Mac on the 100 Mb/s link, through `sulu_eth`) | 17.7 |
| Warp1 running uncached (its instruction fetches) | 22.8 |
| ARM writing memory at 57 MB/s (`dd if=/dev/zero of=/dev/null bs=1M`) | 33.4 |

Audio-rate DMA (one stereo stream, ~0.35 MB/s) is far below the Ethernet load, which cost nothing [INF: not measured
with a stream playing]. Under KnOS (AV
CTR, `warp0-contention.json`, `warp-contention.json`), with Warp1 running anything an uncached Warp0 lost 25-50 % on
both loads and register code (64 loads ~900 -> ~1200 ticks, 768 instructions ~4200 -> ~6000); with the I-cache on
register code is untouched and 64 SDRAM loads go from ~330-600 ticks to ~650 while Warp1 copies SDRAM.

**Memory**: code, tables and stack in the local RAM (ARM 0xAC000000, written through `/dev/mem`); the job ring and
buffers in SDRAM outside Linux (0x61F00000-0x61FBFFFF, `module/warp0/w0abi.h`).

**ARM <-> Warp0**: a descriptor ring in the window. The ARM writes `{op, in, len, out, key, nonce}` and sets
"submitted"; Warp0 polls, runs, writes "done" + status; the ARM polls too (IRQ 51 through 0xFFFF030C latches once per
power cycle, section 3).

**Toolchain**: `module/warp0/mips.py`: clang 21 unwrapped (`nixpkgs#llvmPackages.clang-unwrapped`; the Darwin
wrapper injects host flags), `--target=mipsel -march=mips1 -mabi=32 -ffreestanding -fno-pic -mno-abicalls -nostdlib
-fno-builtin`, `ld.lld`, `llvm-objcopy`. It fills every load-delay slot itself (clang's mips1 filler skips inline
asm), puts two nops around HI/LO hazards, and rejects code with `div`, `movz`/`movn`, a load in a branch delay slot
or calls to `memcpy`/`memset`. LLVM has no `mtc3`:
`.word 0x4c880000` is `mtc3 $8, $0`.

**The runtime**: `module/warp0/` (four firmware images on one job loop, the ARM client in Rust, a Unicorn
harness): a job ring in a window at 0x61F00000 (outside Linux, below the Warp1 window; no image change); the crypto
image (an AirPlay packet decrypted, AES-128-CBC or ChaCha20-Poly1305, and ALAC-decoded), the AAC image and the Vorbis image (the decoders of module/codecs/aac and
module/codecs/vorbis, code from SDRAM, working set in local RAM), the FLAC image (module/codecs/flac, all in local RAM). Every job's pointers are checked against the buffer area
before they are touched (no exceptions here: a bad pointer is a bus hang); a job that doesn't finish is diagnosed
(the control block, the PC through debug mode and the function it is in, or "no debug mode") and logged. The
ciphers checked against the RFC 8439 / FIPS-197 / SP 800-38A vectors and random jobs, the decoders bit-identical to
their ARM builds [HW 2026-10-24/25]; the numbers: `module/warp0/README.md`, `docs/streaming.md`. **Compiler pitfall**: clang keeps
several 64-bit accumulators live and spills them to the stack, which is SDRAM here (Poly1305 in C ran at a quarter
of the hand-written `poly.S`): check the inner loops of `w0fw.s` for stack traffic.

## 7. Hardware safety
Probing needs the unit owner's OK (`AGENTS.md`). It touches only RAM and registers; nothing here needs sound.
- **Bus hangs** (section 3 map) stop the core; park recovers it. After a few, Warp0 stops responding even to trivial
  code (PC = 0) until a module power cycle: keep hangs few between power cycles.
  **Deliberate bus hangs, parked afterwards, upset nothing measurable** [HW 2026-10-25, `re/netmodule/sol-probes/hw-2026-10-25/hangprobe-*`: `lw` 0x10000
  with COP3 = 0, the core stuck with no debug mode on the halt request, then park; each session from a fresh module
  power cycle]: five Warp1 hangs of 1.5 s; five Warp0 hangs of 1.5 s; five Warp1 hangs under an AirPlay-like UDP
  stream over Wi-Fi (1200 B every 8 ms from the Mac, 8,544 datagrams counted by the module); three Warp1 hangs left
  stuck 30 s. After every one of the 18 hangs the core entered debug mode again (COP0 13 = 1, no bit 30), its SDRAM
  and local-RAM loads and a 64-word copy came back correct, an SDRAM load still cost ~20 cycles (23.7-24.5 per
  `w0probe` iteration, baseline 23.8-24.2; 24.3-25.0 with the Wi-Fi stream on, which is the stream's own bus load),
  Warp0's cipher vectors passed (0 hangs that boot), the WILD interrupt registers 0x30200580-0x590 read from the ARM as
  before (0, 0x8000, 0xE, 0, 0), tgen +0x20 / +0x28 stayed 0 / 0x00FFFFED, IRQ 15 kept counting, pings over Wi-Fi
  3/3, and the kernel log (shell after each hang, JTAG at the end) gained no line, but for one `wlan0: RX too short
  data frame payload` at the end of the first 30 s hang (kernel time 96 s), not repeated in the other 17 hangs
  (`re/netmodule/sol-probes/hw-2026-10-25/hangprobe-*.txt`). So a bus hang and the park that recovers it are clean by
  themselves, on either core, with or without Wi-Fi traffic, parked at once or after 30 s. The Warp0 episodes below
  (2026-10-24, before the pointer checks) are not reproduced by this; whatever caused them was not the hang itself.
- **SuluDMA**: start block moves only on contexts the ARM set up (`wsl.ko`'s). A Warp1 block move on context 40,
  never set up (its Flags as found after power-up), took the module down, under KnOS and under our OS.
- **Sol**: the wait words (0x80xx/0xC0xx/0xC4xx/0xC8xx on flag terms, 0xA8xx, 0xF4xx), a halt (+0x17 bit 0 or 1
  from a program, e.g. immediate 0x0037), a self-loop (0x87FF, 0xE3FE) and a loop whose counter is re-armed in its
  body run forever: bound every poll; +0x18 := 1 ends the run; parking Warp1 stops Sol and the next run recovers.
  **Control byte +0x17 bit 0 or 1 written before a start, or a start written while Sol runs or waits,** wedges Sol
  (it never starts again, +0x20 stays 4, `ctrl.json`, `p4-wedge.json`, `p5-ecbits.json`): recover with a **Sol
  reset** (tgen gate 21 pulse, section 2), not a power cycle. `solprobe.run_batch()` checks for this after every
  stall and resets. Persistent state (section 5) explains most "Sol misbehaves until a power cycle"
  episodes. One case stays unexplained: address unit 0's lane-1 loads returned values saturated to 16-bit signed
  (0x00008000 -> 0x7FFF; `lane1-clamp.json`) until a power cycle; no register group, access-word bit, Modeset bit
  or control-byte bit reproduces it (`clamp.json`). Check a known-answer case after long sessions.
- **Warp0 after several bus hangs in one power cycle** stopped answering mid-job running correct firmware (no debug
  mode: stuck on a bus access), at a different point in each run, until a module power cycle; then ~15,000 jobs
  without one, also under ARM memory load (`module/warp0/README.md`). Power-cycle the module after a hang session
  before trusting a result.
- **Wi-Fi**: once, during a Warp1 probe session under our OS with Wi-Fi associated (`w0probe.py cop3`, then `cachebit`,
  then an I-cache invalidate), `sulu_wlan`'s interrupt handler took an external abort reading the WILD core's register
  0x30200580 and the kernel panicked (2026-10-25). Not reproduced: the same probes three times afterwards, and the
  exact first sequence twice from fresh power cycles, all with the kernel log checked after each step. An external
  abort there means the WILD block refused the read (its clock gate 19 or its reset, tgen +0x20 bit 1, are the
  suspects): if it recurs, read 0xE0007020 and 0xE0007028 before anything else.
- **0xFFFF030C** latches the first non-zero write (from any master) until a module power cycle (section 3).
- **Module power cycle** (the MCU's host link: `zd1 module off`, `zd1 module power`); our OS is back on
  169.254.1.1 within the minute (ping it first); with the stock app, then its shell on telnet again (back in ~1.5 min; `docs/module/knos.md`) and
  off when done.
- Leave both cores parked, and unload `wsl.ko` / `warp_debugger.ko` after a DMA probe.

## 8. Reverse-engineering tools and methods
- **Access**: under stock KnOS our MCU firmware puts the KnOS shell on telnet (`zd1 reg write`, `docs/module/knos.md`); over the USB-Ethernet
  dongle the module is at its link-local 169.254.1.1 (KnOS and our OS alike). `wr`/`rd` access raw memory. Write what
  the Warp must see through the uncached alias 0x7xxxxxxx. **Under our OS** the debug system (`module/os`:
  `system-debug.cpio.gz`, with its debug services on: `zd1 debug on`, or `os-debug.bcd`'s default) runs `zd1-memsh`
  on port 23 with the same `wr`/`rd` dialect on `/dev/mem` and a shell on telnet 2323 (module loading, load
  generators), so every tool below works unchanged; this is how the third pass ran (in the emulator, whose dev boot
  is the debug system: `zemu run --fwd 4557:23 --keep`, HOST 127.0.0.1:4557).
- **`tools/zeppelin/warp/warprun.py`** drives either core over that shell (`Warp(host, core=0|1)`). `runner.S`, a job
  interpreter on the Warp (W32/W16/W8, R32/R16, POLL16 with timeout, COPY32, DELAY, STORELIST, FILL, DIFF, CALL a
  snippet), runs many bus operations per upload. CLI `selftest`, `alu`, `fuzz`; API `run`, `snippet` (assemble and
  call MIPS code), `timed_call`, `inspect`, `park`; `sol_cases_job`/`knos_case` build Sol cases. The shell moves ~1100 words/s;
  `write_block` resends only the words that changed in the session, so a probe case costs well under a second.
- **`re/netmodule/sol-probes/`** has the probes behind sections 3, 5 and 7 and their raw results (its `README.md`
  says how to repeat one). **`solprobe.py HOST NAME[,NAME...] OUT`** (`--list`) writes `OUT/NAME.json`. The probe
  sets behind section 5, first and second pass: `agu`, `alu`, `mac`, `loop`, `seq`, `macmode`, `program`, `nokia`,
  `loops2-4`, `trace`, `immfmt`, `seqwords`, `seqreg`, `seqreg2`, `queues`, `queues2`, `unit2b`, `unit2c`,
  `macshift`, `macneg`, `macacc`, `macacc2`, `alu2`, `alu3`, `ctrl` (their conclusions about the "third field",
  sources 0/2, unit 2, loop conditions and MAC latency are superseded by the third pass). Third pass: `regs`,
  `regwr`, `srcs`, `src7`, `lanes`, `cond`, `cond2`, `condbits`, `condsweep`, `condcombine`, `flagops`, `counters`,
  `counters2`, `jumps`, `jumpslots`, `jumpslots2`, `imm`, `au2`, `macpipe`, `macpred`, `maccfg`, `alu4`, `clamp`.
  The JSON keeps each case's program words. Fourth pass, the experiments `p4-entry`, `pointer`, `auwrap`, `src67`,
  `simd`, `ctrl`, `ctrl17`, `wedge`, `cond08`, `seqwords` (the word classifier: finished?, which instructions issued,
  counters and registers after), `counterops`, `call`, `icache`, `dma0`: they read the control block from inside the
  Warp1 job (`job_with_reads`) and use the Sol reset. Fifth pass, `p5-counterwords`, `ret`, `ejumps`, `c08bits`,
  `c08full`, `c08cmp`, `c08ops`, `cntalu`, `ctrlbits`, `scratch`, `cwords`, `ecbits`, `ectrace`, `ecwrite`,
  `e8flags`, `b4b8`, `stallpoke`, `waitwrap`, `a8wait`, `dmawait` (loads `wsl.ko`), `wedge2`, `augroups`, `aupred`,
  `u2window`, `e600`, `loose`; `flag_case()` (flags from a SimdSub of a load, the word under test, marker moves:
  stalled / executed / skipped / jumped), `run_batch()` (several cases per Warp job, re-runs after a stall, Sol
  reset when wedged), `rich_case()` (every unit in one program, for the control bytes). Kernels: `kern()`
  (registers committed, then ops whose results are stored one per word), `condmulti_case()` (all nine lane-sign
  cases of a predicate in one program), `trace_case()` / `trace_long()` (issue order), `store_loop()` /
  `counted()` (stores = passes), `au2_case()`. **`warpprobe.py HOST cachemap|cachemap2|contention|contention2|dma|
  clock`**: Warp0's cache, SDRAM contention (AV CTR under KnOS; wall clock under our OS), the SuluDMA block moves,
  the cores' clock. **`w0probe.py`**: a core's loads, stores, multiplier, I-cache and bus (section 3).
- **Execution trace** (`trace_case`): every instruction of a region loads with stride 1, adds its own number to
  lane 0 (op 0x25 from source 5) and stores; banks 6/7 then hold the issue order with load sequence numbers.
  `jumpslots` (one store at a chosen instruction) is the cleaner way to count how often one instruction runs.
- **Wall-clock timing** (our OS, the AV CTR not started): long runs (seconds), the DONE word polled every 20 ms, code placed by
  `warprun.code_slot()` (`Warp.timed_call()`); first run discarded.
- **Pitfalls**: DIFF reports at most 16 entries plus a count. Sol state carries between runs: each case sets what it
  relies on, and a result is repeated, also after a power cycle, before it is believed. A register field of 0 keeps
  the previous selection; a load is gone after one instruction; a read placed on an instruction overwrites its
  register field; instruction 0 does not run; a predicate at F + 1 / F + 2 still sees the older flags; after a stall
  the next run's first store is stale; KnOS's test program's 0x6408 clears the Z/C flags the E-class jumps test; a stall is usually a wait word. KnOS's
  reference model is not ground truth.
- **Static**: the KnOS app decompiled (`re/ghidra/setup.py module-app`). Warp blobs: `llvm-objcopy -I binary -O
  elf32-tradlittlemips`, then `llvm-objdump -d --triple=mipsel`; kernel modules `llvm-objdump -dr`; the Nokia kernel
  `blobs/module/nokia-hd1/kernel.bin` loads at 0xC0008000 (`newdma_blockmove_create` 0xC0031FD4).
  `python3 -I re/netmodule/sol_dump.py BLOB MEM1 MEM0 N` prints a Sol program slot by slot.
- **Emulator first**: `zemu run stock --keep` gives the KnOS shell on 127.0.0.1:4555, so `warprun.py`
  and the probes run unchanged. The Warp model hangs where section 3 says the unit does; addresses section 3
  doesn't cover read 0 there (`warpN: access to unprobed` in module.log): check those before hardware.

### Vendor programs (reference material) [RE]
Useful as working examples; we replace them. Loops below use the section 5 decode (body S..J+1).
- **Nokia's WarpSol** (`wslsepp_warp` in Nokia's `JukeBlox_Release_ELX`, 0x9BA148, 0x6A84 bytes; kept as
  `blobs/module/nokia-hd1/wslsepp_warp.bin`, extracted by `tools/zeppelin/warp/warpblob.py`; `solprobe.py` reads it): linked at 0x61FC0000, `$gp`
  0x61FC8000; `sp` = 0x400, cache invalidate, Sol init, then a mailbox loop. Sol driver: load 0x4E78, start 0x4DD8,
  wait 0x4EA8, unit register write 0x4D2C, block move 0x4BA0, DMA wait 0x4C44. Biquad EQ stage 0x4ED4: maps unit 4 =
  0xE490 | in, unit 5 = 0xE409 | out << 4, writes coefficients to a parameter block, runs; once per band, banks 3
  <-> 1. Its one Sol program (54 instructions, mem1 blob 0x5EC8, mem0 0x6078): 2-7 address-unit setup, 8 `Modeset 3`
  + MAC 0x2C00 + 0xF00F, 9-13 r1 := 1, 12-15 four coefficients loaded from the parameter block into r4, r5, r2,
  r3 (register fields 0x0045, 0x0055, 0x0025, 0x0035), 16 counter 4 := 0, **17-41 the loop on counter 4: 256 passes** (`0xF344` at 40,
  the unit-2 store at 41 in its delay slot; this is the "256 changed words whatever idx0 was" of `nokia-imm.json`):
  InitMac of a load with the unit-2 state (source 6), four Macs of loads against r4/r5/r2/r3, the delay line
  shifted through the register field (r4 := r2, r5 := r3, r2 := new sample), results scaled with 0x2B shifts and
  stored to unit 2 and banks; 51 end. The parameter block the Warp fills before each run is Sol bank 9 (Warp1 0x2400:
  12 coefficient halfwords, then state words; a control word 0x08121102 at 0x27C0).
- **KnOS's WarpSol** (image 0x605DA510, length at 0x605DA50C; in `sec0_app_main_0x60008000.bin` at file offset
  0x5D2510): linked at 0x61CE0000, Sol loader 0x1F98, five programs (mem1/mem0 blob offsets, instructions): P1
  0x6418/0x67D0 119, P2 0x3A08/0x3B88 48, P3 0x34F8/0x3780 81, P4 0x3D08/0x3F90 81, P5 0x30B8/0x32D8 68. Loops: P1
  28-73 (counter 4) and 84-112 (counter 5), both loaded at 19-22 by `0x9401` (counters := r1) and `0xA004`/`0xA005`;
  P2 23-44 (counter 4 := 0 at 17: 256 passes); P3 27-77 (counter 4 := 0 at 11); P4 22-70 (counter 4 := 0 at 2); P5
  27-63 on counter 5 around 36-53 on counter 4, both from registers (`0x9401` + `0xA005`, `0x9402` + `0xA004`). Every
  program builds the 24-bit limits 0x007FFFFF / 0xFF800000 in registers and clamps with 0x998F / 0x99EC (P2 and P3
  with 0x9D8F as well, section 5); P1 also picks register values with 0xD58C / 0xD5AD. KnOS does not patch its Sol programs at run time
  (its only halfword stores are sample conversion); the loader at 0x1F98 is a plain copy.
- **KnOS's Sol self test** `sys sol testInstr <runs> <size>` (`FUN_603282b4`): a 12-instruction program (mem1
  0x60585E28, mem0 0x60585E88) against an ARM reference model (ALU `FUN_603b46e4`, MAC `FUN_603b5190`). Passes on
  hardware with Warp1 running an idle loop. Its ALU operand 0x12C has dst 4: the plain per-lane SIMD form.
- **`wsl.ko` protocol**: an 84-byte SRAM mailbox (`sulu_umr_alloc`) whose address is in Warp1 local word 0, read by
  the Warp at 0x03C00000 + address; two semaphore-guarded linked-list queues in SRAM (ARM -> Warp, Warp -> ARM) and
  the two block-move descriptors {context, client, handle} at +0x3C.

### Emulator model
- `emu/renode/cr870/CR870Warp.cs`: both debug blocks and the cores as section 3 has them: each core's memory map
  (local RAM sizes, the fixed values, the bus hangs: the core stops and answers no halt request until parked), the
  ISA as measured (no divider, the strict load delay and its delay-slot rule, no traps, lwl/lwr and ll inert, sc
  storing 0, movz/movn/mul/clz as rd := rs, branch-likely inert, unaligned loads as sampled, the COP0 constants), the
  I-cache (2 ways x 64 sets x 32 bytes, pseudo-random replacement ignoring empty ways, a 34-cycle fill, bit 20 = valid
  in the tag view, stale lines across a Warp reset), and the measured cycle costs (local RAM 1, SDRAM load ~19 / ~24
  while the other Warp fetches uncached / store ~7 / uncached fetch ~23, SRAM ~6 / 1, peripherals ~10 / 1, multiply
  result after 4) at 160 MHz. The replacement is a deterministic xorshift (sessions stay repeatable). Not modelled:
  contention from the ARM and DMA (`docs/decisions.md`), the stall after a fill, the unit's flakiness after several
  hangs, untested non-MIPS-I instructions (a no-op, logged). The cipher benchmark [EMU]: ChaCha20 4.3 MB/s, Poly1305
  6.7, AES-128-CBC 2.0 (unit 4.1, 6.8, 2.0); a busy Warp costs host time (the bench: 15 virtual s in ~8 min).
- `emu/renode/cr870/CR870.cs`: tgen gates, power-up 0x00FFFFED.
- `emu/renode/cr870/CR870Sol.cs` transcribes KnOS's reference model (KnOS's `sys sol testInstr` passes on it). Differs from hardware
  (`q6.json` vs `q6-emu.json`, and section 5): no register file (the register field is ignored), no predication or
  flags, no loop counters (loops end by an iteration guard; its `0xF3xx` target is the word's low 6 bits, the
  hardware's is J - 63 + bits 8:3 with a delay slot), no unit-2 memory, no SIMD lane routing (dst), no MAC output
  shift lag; SIMD A/S variants average/saturate; Shift2/Rot2 return 0 and Rot1 rotates; 0x25-0x2C return
  accumulator B; the MAC is always combined (no op 0), ignores macmode bits 1:0, InitMac outputs the accumulator;
  no strides, modulos, start/base/step registers, bank maps, start offset (it runs instruction 0), 0xE2xx, store
  packing, the control-block byte ALU and its Z/C flags, counter ops, waits, calls and returns, the unit windows or
  the register-file window; source 6/7 as on hardware are missing.
  A vendor program does not run correctly in it. Noted in `docs/backlog.md`.

## 9. Open questions
Everything the vendor programs use, and nearly everything they don't, is decoded and seen on hardware. Left:

1. **0xA8xx and 0xF4xx**: waits for an event not found (not the counters, the flags, the control bytes, window
   words, or a SuluDMA block move; `p5-a8wait.json`, `p5-dmawait.json`, `p5-stallpoke.json`).
2. **0xE800-0xEBFF**: no effect found under any flag state.
3. **Address-unit groups** 0xD600, 0xDC00, 0xE800-0xEE00 and the predicate selects 1-3 of the address-unit sources:
   no effect found. **Unit windows 0-2 and 7-15**: no effect found.
4. **The lane-1 16-bit clamp** (section 7): seen once (2026-10-22), not reproduced in any later pass.
5. **0xFFFF030C**: no release other than a power cycle exists on our unit (section 3); whether Nokia's hardware
   differs is unknown.
