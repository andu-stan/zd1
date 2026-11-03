# SuluDMA: register map and context format

The DM870's shared DMA engine: Ethernet frames (Linux `sulu_eth` block moves), Wi-Fi frames (the WILD MAC, under
KnOS), audio and video streams (the AV ports). This is the register-level reference; how KnOS and the Nokia drivers
use it is summarised in `docs/module/memory-map.md` (SuluDMA).

Sources: the KnOS app's decompilation (`re/ghidra/setup.py module-app`: the `sys dma` shell handlers), live `sys dma`
output on the unit, and the Nokia kernel's `newdma_*` / `sulu_dma_*`, which agree. Tags: `docs/README.md`.

RTTI classes in the app: `7SuluDMA`, `11SuluDMAFifo`, `11SuluDMAWarp`, `12SuluDMABlock`, `13SuluDMAPacket`: a base
with Fifo / Warp / Block / Packet variants. Shell handlers: `sys dma reg {cfg|stat|irq|fast|misc}` → FUN_6031e870;
`sys dma ctx <n>` → FUN_60375d7c (formatter FUN_60375344); `sys dma lookup` → FUN_6031e598; `sys dma
error`/`extinfo` → FUN_6031e738; context read FUN_603982b0; lookup read FUN_603d736c.

## Controller registers
Base **0xB5000000** (Linux maps 64 KiB there; `sys dma reg cfg` prints `lockEngine` at 0xB5001200) [HW]. Offsets
from the base [RE]:

| Offset | Register | |
|---|---|---|
| +0x0000 / +0x0004 / +0x0008 | blockMvSet / blockMvClr / blockMvID | block-move start bitmap / busy |
| +0x0080 + n·4 (n < 16) | blockMvCtx[n] | |
| +0x100C | genCtxSwitch | |
| +0x1010 / +0x1014 | ctrlGeneralSet / Clr | |
| +0x1018 / +0x101C | irqGenSet / Clr | general IRQ enable |
| +0x1020 / +0x1024 | bmExcl0 / bmExcl1 | bus-master exclusion |
| +0x10B0, +0x10C0 | ctxReleaseID, clientRelease | |
| +0x10C4 / +0x10C8 | stallSet / stallClr | |
| +0x10CC | ctxSup | context supervision select: written before reading a context's group IRQ |
| +0x10D0 | testBusAddr | |
| +0x1200 / +0x1204 | lockEngine / highPriority | |
| +0x1220 + n·4 (n < 16) | bmClientDma[n] | bus-master client → DMA map |
| +0x1260 + n·4 (n < 5) | hwClientDma[n] | hardware client → DMA map |
| +0x1320 + n·4 / +0x1390 + n·4 | bmClientStat[n] / hwClientStat[n] | |
| +0x1900 | hsErrorClr | handshake error |
| +0x1920 / +0x1930 + w·4 | irqStatusWrSet / WrClr[w] | |
| +0x1940 / +0x1950 + w·4 | irqStatusRdSet / RdClr[w] | |
| +0x1960, +0x1970/74/78, +0x1980, +0x1990 | mismatchStatus, supCfg / supRefCnt / supCnt, discardCtx, actSup | |
| +0x1A00 + n·4 (n < 2) | dmaStatus[n] | |
| +0x1C00 + g·0x10 | irqGrp{g}Set | group status (read) |
| +0x1C04 + g·0x10 | irqGrp{g}Clr | acknowledge: write 0xFFFFFFFF |
| +0x1C08 / +0x1C0C + g·0x10 | irqGrp{g}MaskSet / MaskClr | |
| +0x1C40 + g·4 (g < 4) | irqGrpID[g] | the group's completion tag FIFO |
| +0x1FF8 | version | `sys versulu`: dma 2006-11-08 rev 1 |
| +0x2800 / +0x2810 + w·4 | emptyFlagSet / Clr[w] | per-context bitmaps: word `c >> 5`, bit `1 << (c & 31)` |
| +0x2820 / +0x2830 + w·4 | fullFlagSet / Clr[w] | |
| +0x3000 + client·4 (client < 16) | client start | `ctx \| 0x800` starts a packet move; 0x800 reads back set while it runs [HW] |
| +0x3100 / +0x3104 / +0x3108 | keepArb / vClientFlush / vClientID | |
| +0x3800..+0x380C | errorSet / Clr / MaskSet / MaskClr | |
| +0x3C00..+0x3C0C | irqSet / Clr / MaskSet / MaskClr | global IRQ |
| +0x4000 + c·0x40 | context RAM (64 contexts), quadlet-addressable | |
| +0x6000 + c·0x40 | lookup RAM (context → group) | |
| +0x8000 / +0x8200 | ctxBmFastRd / ctxBmFastWr [c] | |
| +0x8400 + c·4 | Warp block-move start [c] | write `quadlets \| client << 16`: moves ctx +0x0C -> +0x08; busy = +0x0000 bit `client` [HW: Warp1 and Warp0, 2026-10-23; RE: Nokia's WarpSol blob 0x4BA0 / 0x4C44] |
| +0x9000 / +0x9200 + c·4 | ctxPktFastRd / ctxPktFastWr [c] | packet fast path: `(address & 0xFFFF) << 8 \| next ctx` |
| +0x9800 | ctxSwitchCfg[0] | |
| +0xA000 / +0xA100 / +0xA200 | vFifoInit / vFifoAdd / vFifoFlush | virtual FIFOs |

The app also programs the lookup table through an indirect window in its device object (object +0x970/+0x974/+0x978:
`(ctx & 0x7F) | (group & 0xF) << 8 | 0x4000` = go; bit 0x10000 = busy/fail) [RE].

## Context (0x40 bytes) [RE, checked against live samples]

| Offset | Field | |
|---|---|---|
| +0x00 / +0x04 | old write / read pointer | the engine's last committed positions |
| +0x08 | **AHB write address** | the endpoint the engine writes to |
| +0x0C | **AHB read address** | the endpoint it reads from |
| +0x10 | counters | low 16 = write counter, high 16 = read counter |
| +0x14 / +0x18 | write / read pointer | ring producer / consumer offsets |
| +0x1C / +0x20 | lower / upper FIFO threshold | (Warp modes; packet mode: a packet count at `(x & 0xffff) >> 6`) |
| +0x24 | **Flags0** | below |
| +0x28 | **Flags1** | below |
| +0x2C..+0x3F | not read by the dumper | |

**Flags0**: [31:30] **Tag = IRQ group** (0..3; also the completion group), [29:27] **mode** (7 = packet, 4 = alternate
packet, 0..3 = Warp block variants), [26:21] **CtxSwitch**, [20:8] single-bit flags D I G H X B (only **I = bit 12**
pinned), [7:0] **next context** (the chain; Linux block moves use [5:0]). For Linux's packet streams bit 7 is the
**empty** flag (the engine sets it when it has moved the packet; the driver queues only into an empty context) [RE:
`newdma_packet_queue`]; how that squares with an 8-bit next-context field (64 contexts need 6 bits) is open.
**Flags1**: [12:0] **MaxPktLen**, read width [22:21] and write width [24:23] (2 = 32-bit, 1 = 16-bit, 0 = 8-bit), and
ring/counter fields Z / WZ / RZ / W / R / S / N / T whose bit positions are not isolated.

Live samples [HW]:
- Context 58 (audio out, AV port 2) at boot under KnOS: old/current pointers 0x508fc0, thresholds
  0x508fc0/0x509038, Flags0 0x20800080 (mode 4), Flags1 0x01400054 (MaxPktLen 84), AHB write 0xB4000000.
- Context 60 (video, AV port 0): Flags0 0x3900103D (Tag 0, mode 7, CtxSwitch 8, I, next 61), Flags1 0x014006BC
  (MaxPktLen 1724), AHB write 0xB4000000, read pointer 0x8800; 60 ↔ 61 ping-pong.
- Contexts 35 / 36 as `wsl.ko`'s `newdma_blockmove_create` leaves them under our OS (2026-10-23; clients 3 / 4):
  +0x00 0xBEEF, +0x08 / +0x0C 0xDEADBEEF, +0x14 / +0x18 / +0x1C an SRAM offset (0xA7DC / 0xA85C), +0x20 0x20,
  Flags0 0x188100A3 / 0x188100A4 (Tag 0, mode 3, next = itself, bit 7), Flags1 0x01580000 (32-bit read and write).
  After a Warp1 move of 256 quadlets (`re/netmodule/sol-probes/hw-2026-10-23/dma.json`): +0x00 0, both addresses
  advanced by 0x400, +0x10 0x0000FF00, +0x14 / +0x18 advanced by 0x80, Flags0 0x188502A3 / 0x188502A4 (bit 10 set).
- Contexts 58-62 are set up under KnOS; 0-57 and 63 hold uninitialised values; the busy mask is 0
  (`re/netmodule/sol-probes/hw-2026-10-22b/dma-survey.txt`).

## Completion
- **Packet mode**: per group. The context's Tag selects one of 4 groups; the full/empty bitmaps show per-context
  state; `irqGrp{g}Set` holds up to 15 cause bits (HdrSupError, FifoFlush, ...), acknowledged by writing
  0xFFFFFFFF to `irqGrp{g}Clr` (the diagnostic path first selects the context through ctxSup). Linux reads the
  completed context numbers from the group's tag FIFO (+0x1C40 + 4·group); IRQ 10 + group.
- **Block mode** (`SuluDMABlock`): set `1 << ctx` in the start bitmap, poll the busy bit until it clears
  (FUN_603d68a4, FUN_603d685c = `waitForCompletion()`); Linux takes the completion tag from group 1, IRQ 11.
