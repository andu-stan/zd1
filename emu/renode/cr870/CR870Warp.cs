// The DM870's Warp cores: little-endian MIPS I integer cores at 160 MHz (docs/module/warp.md sections 2 and 3, measured
// on the unit). Warp1 runs the "WarpSol" audio program and drives the Sol DSP; Warp0 is the core our OS uses for
// compute (module/warp0).
//
// What is modelled, and where it comes from:
// - The debug block per core at 0xE0014000 + n * 0x1000 (Nokia's warp_debugger.ko, virt 0xF0014000): +0x00 write =
//   execute that instruction in debug mode, +0x04 data to the core, +0x08 data from it, +0x0C bit 0 halt request /
//   bit 1 in debug mode, +0x10 reset (0 = held, 1 = released), +0x1C busy (always 0 here: debug instructions run at
//   once). Debug coprocessor (COP1) registers: 20 debug mode (writing it leaves debug mode; with the halt request set
//   the core runs one instruction and stops again = step), 21 DebugPC (where it resumes), 22 RxTx (mfc1 reads +0x04,
//   mtc1 writes +0x08), 23 PC.
// - The core as measured (section 3; `re/netmodule/sol-probes/w0probe.py`): MIPS I integer, branch delay slots; **no
//   divider** (div/divu leave HI/LO = 0); **strict load delay** (the instruction after a load sees the load's address
//   in the target register; if it writes that register itself, its value wins); lwl/lwr leave the target unchanged;
//   no exceptions or interrupts: syscall, break and overflow don't trap. Outside MIPS I: movz/movn and SPECIAL2
//   mul/clz copy rs to rd, branch-likely and ll do nothing, sc stores 0 and leaves rt; others untested (a no-op here,
//   logged). Unaligned sw/sh drop the low address bits; unaligned lw/lh/lhu as sampled (WordLoad, HalfLoad). COP0
//   reads the unit's constants, COP2 reads 0, COP3 register 0 is the cache control (bit 0 or 2 = on, bit 4 maps the
//   cache RAM; 0 after a Warp reset).
// - The I-cache: 4 KB, 2 ways x 64 sets x 32-byte lines, pseudo-random replacement that ignores empty ways, a 34-cycle
//   line fill; for code outside the local RAM (code in the local RAM is never cached). It keeps its lines through a
//   Warp reset: new code at an address that already ran executes the stale instructions until the tags are zeroed
//   (COP3 = 0x10, zero 0x02000000-0x020005FF), as on the unit. With COP3 bit 4 the cache RAM is visible: tags at
//   0x02000000 (0x300000 | address bits 30:11, bit 20 = valid), data at 0x10000; which word shows which way is
//   inferred, not measured.
// - Each core's memory map (section 3's table): its local RAM (Warp0 48 KB = ARM 0xAC000000, Warp1 10 KB = Sol's data
//   memory = ARM 0xA8000000), the fixed values the unit reads elsewhere (0xE, 0xF04A, 0x000CCCCC, 0x20012001,
//   0xFFFF0000), Sol at 0x01xxxxxx (Warp1), the ARM's low 4 MB at 0x03C00000, the SRAM at 0x04000000, SDRAM,
//   SuluDMA, the peripherals and the 0xFFFF0000 page through the ARM's bus. **Bus hangs**: an access the unit hangs on
//   stops the core here too: it neither runs nor enters debug mode until it is parked (reset), and module.log says
//   where. Addresses section 3 doesn't cover ("unprobed") read 0 and drop stores, logged once per page.
// - Time: the core runs in slices on the module's virtual clock, 160 cycles per microsecond, with the unit's measured
//   costs: local RAM fetch, load and store 1 cycle; SDRAM load ~19 (~24 while the other Warp fetches code from SDRAM
//   uncached), store ~7, uncached fetch ~23; SRAM load ~6, store 1; peripheral load ~10, store 1; a multiply's result
//   4 cycles after it (mflo/mfhi stall); a cache line fill 24 (fitted: a 6 KB loop ran at 4.0 cycles an instruction).
//   Not modelled: contention with the ARM and DMA (up to ~2x on SDRAM loads), the unit's flakiness after several hangs
//   in one power cycle. A core that only polls (no store that changes anything, few distinct instructions and
//   addresses in a slice) gets short slices until it changes something again: same behaviour, fewer spin iterations,
//   little host time.
using System;
using System.Collections.Generic;
using Antmicro.Renode.Core;
using Antmicro.Renode.Peripherals.Bus;
using Antmicro.Renode.Time;

namespace Antmicro.Renode.Peripherals.CR870
{
    public class CR870_Warp
    {
        public const ulong Base = 0xE0014000;
        public const ulong End = 0xE0016000;

        public CR870_Warp(IMachine machine, CR870_Log log)
        {
            this.machine = machine;
            this.log = log;
            cores = new[] { new Core(this, 0), new Core(this, 1) };
        }

        /// <summary>Cycles per microsecond (the Warp clock in MHz).</summary>
        public int Mips { get; set; } = 160;

        /// <summary>Chip reset (power-up, watchdog): both cores stopped, their caches empty.</summary>
        public void Reset()
        {
            foreach (var c in cores)
            {
                c.PowerOn();
            }
            slicing = false;
        }

        public string Status(int n) => cores[n].Status();

        internal ulong Read(ulong a)
        {
            var c = cores[(a - Base) >> 12];
            return c.ReadReg((uint)(a & 0xFFF));
        }

        internal void Write(ulong a, uint v)
        {
            var c = cores[(a - Base) >> 12];
            c.WriteReg((uint)(a & 0xFFF), v);
            Schedule();
        }

        // ---- time slices ----

        private void Schedule()
        {
            if (slicing || (!cores[0].Running && !cores[1].Running))
            {
                return;
            }
            slicing = true;
            lastNs = machine.ElapsedVirtualTime.TimeElapsed.Ticks;
            machine.ScheduleAction(
                TimeInterval.FromMicroseconds(SliceUs),
                _ => Slice(),
                "cr870-warp"
            );
        }

        private void Slice()
        {
            slicing = false;
            var now = machine.ElapsedVirtualTime.TimeElapsed.Ticks;
            var budget = (long)((now - lastNs) * (ulong)Mips / 1000);
            if (budget > MaxSlice)
            {
                budget = MaxSlice;
            }
            foreach (var c in cores)
            {
                if (c.Running)
                {
                    c.Run(c.Waiting ? Math.Min(budget, WaitSlice) : budget);
                    if (now >= c.NextReportNs && c.Reports < 10)
                    {
                        c.Reports++;
                        c.NextReportNs = now + 1000000000ul;
                        Log(c.Status());
                    }
                }
            }
            Schedule();
        }

        internal void Log(string s) => log.Log(s);

        // ---- the Warp's view of memory (docs/module/warp.md section 3, "Memory") ----

        private enum Kind
        {
            Local,
            Sdram,
            Sram,
            Bus,
            Value,
            Hang,
            CacheData,
            CacheTags,
            Unprobed,
        }

        /// <summary>What a Warp address is for this core: the kind, the ARM address for the bus kinds, the value a fixed
        /// region reads.</summary>
        private static Kind Decode(Core c, uint a, out ulong p, out uint value)
        {
            p = a;
            value = 0;
            var map = (c.Cop3 & 0x10) != 0; // COP3 bit 4: the cache RAM is mapped
            if (a < 0x10000)
            {
                if (c.Index == 1)
                {
                    var o = a & 0x3FFF; // 10 KB, the 16 KB pattern repeats to 0xFFFF
                    if (o < 0x2800)
                    {
                        p = 0xA8000000u + o;
                        return Kind.Local;
                    }
                    value = 0xE; // 0x2800-0x3FFF read 0xE and drop stores
                    return Kind.Value;
                }
                if (a < 0xC000)
                {
                    p = 0xAC000000u + a;
                    return Kind.Local;
                }
                value = 0xE; // Warp0 0xC000: 0xE with the cache RAM mapped,
                return map ? Kind.Value : Kind.Hang; // else it hangs
            }
            if (a < 0x20000)
            {
                return map ? Kind.CacheData : Kind.Hang; // the I-cache data RAM, 4 KB repeating
            }
            if (a < 0x01000000)
            {
                return Kind.Unprobed;
            }
            if (a < 0x02000000)
            {
                if (c.Index == 0)
                {
                    return Kind.Hang; // Warp0 has no Sol
                }
                p = 0xCF000000ul + a; // Sol: Warp 0x01xxxxxx = ARM 0xD0xxxxxx
                return Kind.Bus;
            }
            if (a < 0x02000600)
            {
                if (map)
                {
                    return Kind.CacheTags;
                }
                value = c.Index == 1 ? 0xF04Au : 0x000CCCCCu;
                return Kind.Value;
            }
            if (a >= 0x03C00000 && a < 0x04000000)
            {
                if (a == 0x03C00014)
                {
                    return Kind.Hang;
                }
                p = a - 0x03C00000u; // the ARM's 0x00000000-0x003FFFFF
                return p >= 0x400000 ? Kind.Unprobed : Kind.Bus;
            }
            if ((a >= 0x04000000 && a < 0x04010000) || (a >= 0x08000000 && a < 0x08010000))
            {
                p = 0x00400000u + (a & 0xFFFF); // the on-chip SRAM (0x08000000: the same or an alias)
                return Kind.Sram;
            }
            if (a < 0x10000000)
            {
                return Kind.Unprobed;
            }
            if (a < 0x40000000)
            {
                value = 0xFFFF0000;
                return Kind.Value;
            }
            if (a == 0x40000000)
            {
                value = 0x20012001;
                return Kind.Value;
            }
            if (a < 0x60000000)
            {
                return Kind.Unprobed;
            }
            if (a < 0x80000000)
            {
                return Kind.Sdram;
            }
            if (a >= 0xB5000000 && a < 0xB6000000)
            {
                return Kind.Bus; // SuluDMA
            }
            if (a < 0xD0000000)
            {
                value = 0xFFFF0000;
                return Kind.Value;
            }
            if ((a >= 0xE0000000 && a < 0xF0000000) || a >= 0xFFFF0000)
            {
                return Kind.Bus; // peripherals, both debug blocks; the vector/VIC page
            }
            return Kind.Unprobed;
        }

        /// <summary>A data load: the value, or the core hangs (Hung set, 0 returned). Adds the access's cycles.</summary>
        internal uint Load(Core c, uint a, int size)
        {
            c.NoteLoad(a);
            var k = Decode(c, a, out var p, out var value);
            switch (k)
            {
                case Kind.Local:
                    return BusRead(p, size);
                case Kind.Sdram:
                    c.Cost += cores[1 - c.Index].FetchingSdram ? 23 : 18; // the other Warp fetching uncached: 22.8 [HW]
                    return BusRead(p, size);
                case Kind.Sram:
                    c.Cost += 5;
                    return BusRead(p, size);
                case Kind.Bus:
                    c.Cost += 9;
                    NoteRegion(c, p, a);
                    return BusRead(p, size);
                case Kind.Value:
                    return Lane(value, a, size);
                case Kind.CacheData:
                    return Lane(c.CacheDataWord(a), a, size);
                case Kind.CacheTags:
                    return Lane(c.CacheTagWord(a - 0x02000000), a, size);
                case Kind.Hang:
                    c.Hang(a, "load");
                    return 0;
                default:
                    NoteUnprobed(c, a);
                    return 0;
            }
        }

        internal void Store(Core c, uint a, uint v, int size)
        {
            var k = Decode(c, a, out var p, out var value);
            switch (k)
            {
                case Kind.Local:
                    if (
                        BusRead(p, size)
                        != (
                            size == 4 ? v
                            : size == 2 ? v & 0xFFFF
                            : v & 0xFF
                        )
                    )
                    {
                        c.Changes++;
                    }
                    BusWrite(p, v, size);
                    return;
                case Kind.Sdram:
                case Kind.Bus:
                    c.Cost += k == Kind.Sdram ? 6 : 0; // peripheral stores are posted: 1 cycle [HW]
                    c.Changes++; // a bus store always counts (registers, SDRAM)
                    if (k == Kind.Bus)
                    {
                        NoteRegion(c, p, a);
                    }
                    BusWrite(p, v, size);
                    return;
                case Kind.Sram:
                    c.Changes++; // 1 cycle (posted) [HW]
                    BusWrite(p, v, size);
                    return;
                case Kind.Value:
                    return; // read-only: the store goes nowhere
                case Kind.CacheData:
                    c.Changes++;
                    c.CacheDataStore(a, v, size);
                    return;
                case Kind.CacheTags:
                    c.Changes++;
                    c.CacheTagStore(a - 0x02000000, v);
                    return;
                case Kind.Hang:
                    c.Hang(a, "store");
                    return;
                default:
                    NoteUnprobed(c, a);
                    return;
            }
        }

        /// <summary>An instruction fetch: local RAM at one cycle; elsewhere through the I-cache when it is on, else
        /// uncached at the region's cost.</summary>
        internal uint Fetch(Core c, uint a)
        {
            var k = Decode(c, a, out var p, out var value);
            switch (k)
            {
                case Kind.Local:
                    return BusRead(p, 4);
                case Kind.Sdram:
                case Kind.Sram:
                case Kind.Bus:
                    if (c.CacheOn)
                    {
                        return c.CachedFetch(a, p);
                    }
                    c.Cost += k == Kind.Sram ? 5 : 22;
                    if (k == Kind.Sdram)
                    {
                        c.UncachedSdramFetches++;
                    }
                    return BusRead(p, 4);
                case Kind.Value:
                    return value;
                case Kind.CacheData:
                    return c.CacheDataWord(a);
                case Kind.CacheTags:
                    return c.CacheTagWord(a - 0x02000000);
                case Kind.Hang:
                    c.Hang(a, "fetch");
                    return 0;
                default:
                    NoteUnprobed(c, a);
                    return 0;
            }
        }

        internal uint BusRead(ulong p, int size)
        {
            var bus = machine.SystemBus;
            return size == 4 ? bus.ReadDoubleWord(p)
                : size == 2 ? bus.ReadWord(p)
                : bus.ReadByte(p);
        }

        private void BusWrite(ulong p, uint v, int size)
        {
            var bus = machine.SystemBus;
            if (size == 4)
            {
                bus.WriteDoubleWord(p, v);
            }
            else if (size == 2)
            {
                bus.WriteWord(p, (ushort)v);
            }
            else
            {
                bus.WriteByte(p, (byte)v);
            }
        }

        private static uint Lane(uint w, uint a, int size) =>
            size == 4 ? w
            : size == 2 ? (w >> (int)(8 * (a & 2))) & 0xFFFF
            : (w >> (int)(8 * (a & 3))) & 0xFF;

        private void NoteRegion(Core c, ulong p, uint a)
        {
            var region = (uint)(p >> 20);
            if (seenRegions.Add(region))
            {
                Log(
                    string.Format(
                        "warp{0}: new region {1:x8} (warp address {2:x8}, pc={3:x8})",
                        c.Index,
                        p,
                        a,
                        c.Pc
                    )
                );
            }
        }

        /// <summary>Log, once per 4 KB page per core, an access where the unit wasn't probed: here it reads 0 and the
        /// store is dropped; on the unit it may hang the core (check docs/module/warp.md section 3).</summary>
        private void NoteUnprobed(Core c, uint a)
        {
            if (deadSeen.Add(((ulong)c.Index << 32) | (a >> 12)))
            {
                Log(
                    string.Format(
                        "warp{0}: access to unprobed {1:x8} (pc={2:x8}): reads 0 here, may hang the unit",
                        c.Index,
                        a,
                        c.Pc
                    )
                );
            }
        }

        private const int SliceUs = 50;
        private const long WaitSlice = 500; // a waiting core still polls every slice (~3 us of its time)
        private const long MaxSlice = 160 * 1000; // at most 1 ms of Warp time per slice (after a long gap)

        private readonly IMachine machine;
        private readonly CR870_Log log;
        private readonly Core[] cores;
        private readonly HashSet<uint> seenRegions = new HashSet<uint>();
        private readonly HashSet<ulong> deadSeen = new HashSet<ulong>();
        private bool slicing;
        private ulong lastNs;

        // =============================================================================================================

        internal sealed class Core
        {
            public Core(CR870_Warp warp, int index)
            {
                this.warp = warp;
                Index = index;
                PowerOn();
            }

            public int Index { get; }

            public uint Pc => pc;

            public bool Running => !debug && !inReset && !hung;

            /// <summary>COP3 register 0: the cache control.</summary>
            public uint Cop3 => cp3[0];

            /// <summary>Cycles the current instruction costs beyond its first.</summary>
            public long Cost;

            /// <summary>Status lines in module.log while running: once a virtual second, the first ten.</summary>
            public ulong NextReportNs;
            public int Reports;

            public void PowerOn()
            {
                Array.Clear(r, 0, 32);
                Array.Clear(cp3, 0, 32);
                hi = lo = 0;
                pc = ResetVector;
                npc = pc + 4;
                debug = true; // ASSUMPTION: a core comes up stopped (Linux stops and resets it first)
                inReset = false;
                hung = false;
                haltReq = true;
                dataIn = dataOut = 0;
                executed = 0;
                cycles = 0;
                multReady = 0;
                pendReg = -1;
                Array.Clear(lineValid, 0, lineValid.Length); // power-up: no valid lines (a Warp reset keeps them)
                victim = 0x2545F491u + (uint)Index;
            }

            public string Status()
            {
                return string.Format(
                    "warp{0}: {1} pc={2:x8} executed={3} cycles={4} cop3={5:x} sp={6:x8} ra={7:x8} v0={8:x8}",
                    Index,
                    inReset ? "reset"
                        : hung ? "hung on a bus access"
                        : debug ? "debug"
                        : "running",
                    pc,
                    executed,
                    cycles,
                    cp3[0],
                    r[29],
                    r[31],
                    r[2]
                );
            }

            /// <summary>A bus hang: the core stops on the access, answers no halt request and runs no debug
            /// instruction until it is reset (park).</summary>
            public void Hang(uint a, string what)
            {
                if (hung)
                {
                    return;
                }
                hung = true;
                warp.Log(
                    string.Format(
                        "warp{0}: bus hang: {1} at {2:x8} (pc={3:x8}): the core stops until it is parked (reset)",
                        Index,
                        what,
                        a,
                        pc
                    )
                );
            }

            // ---- the debug block ----

            public uint ReadReg(uint off)
            {
                switch (off)
                {
                    case 0x04:
                        return dataIn;
                    case 0x08:
                        return dataOut;
                    case 0x0C:
                        return (haltReq ? 1u : 0) | (debug && !hung ? 2u : 0);
                    case 0x10:
                        return inReset ? 0u : 1u;
                    default:
                        return 0; // +0x1C busy: debug instructions complete at once
                }
            }

            public void WriteReg(uint off, uint v)
            {
                switch (off)
                {
                    case 0x00:
                        DebugExec(v);
                        break;
                    case 0x04:
                        dataIn = v;
                        break;
                    case 0x0C:
                        haltReq = (v & 1) != 0;
                        if (haltReq && !debug && !hung)
                        {
                            EnterDebug("halt request");
                        }
                        break;
                    case 0x10:
                        if ((v & 1) == 0)
                        {
                            if (hung)
                            {
                                warp.Log(
                                    string.Format(
                                        "warp{0}: held in reset: the bus hang is over",
                                        Index
                                    )
                                );
                            }
                            inReset = true;
                            hung = false;
                        }
                        else if (inReset)
                        {
                            inReset = false;
                            Array.Clear(r, 0, 32);
                            cp3[0] = 0; // 0 after a reset [HW] (the I-cache keeps its lines)
                            pendReg = -1;
                            pc = ResetVector;
                            npc = pc + 4;
                            debug = haltReq;
                            warp.Log(
                                string.Format(
                                    "warp{0}: reset released ({1})",
                                    Index,
                                    debug ? "halted" : "running"
                                )
                            );
                        }
                        break;
                }
            }

            private void DebugExec(uint insn)
            {
                if (!debug || hung)
                {
                    warp.Log(
                        string.Format(
                            "warp{0}: debug instruction {1:x8} while {2} (ignored)",
                            Index,
                            insn,
                            hung ? "hung" : "running"
                        )
                    );
                    return;
                }
                var op = insn >> 26;
                var rs = (int)(insn >> 21) & 31;
                var rt = (int)(insn >> 16) & 31;
                var rd = (int)(insn >> 11) & 31;
                if (op == 0x11 && rs == 0) // mfc1 rt, rd
                {
                    Set(rt, Cp1Read(rd));
                    return;
                }
                if (op == 0x11 && rs == 4) // mtc1 rt, rd
                {
                    Cp1Write(rd, r[rt]);
                    return;
                }
                // anything else runs on the core's registers without moving the PC; a load completes at once
                var savePc = pc;
                var saveNpc = npc;
                Exec(insn, pc);
                CommitLoad();
                pc = savePc;
                npc = saveNpc;
            }

            private uint Cp1Read(int n)
            {
                switch (n)
                {
                    case 20:
                        return debug ? 1u : 0;
                    case 21:
                        return pc; // DebugPC: where the core resumes
                    case 22:
                        return dataIn;
                    case 23:
                        return pc;
                    default:
                        return 0;
                }
            }

            private void Cp1Write(int n, uint v)
            {
                switch (n)
                {
                    case 20:
                        if (v == 0 && debug)
                        {
                            debug = false;
                            if (haltReq)
                            {
                                Step(); // step: one instruction, then stop again
                                debug = true;
                            }
                            else
                            {
                                warp.Log(string.Format("warp{0}: started at {1:x8}", Index, pc));
                                NextReportNs = 0;
                                Reports = 0;
                            }
                        }
                        break;
                    case 21:
                        pc = v;
                        npc = v + 4;
                        break;
                    case 22:
                        dataOut = v;
                        break;
                }
            }

            private void EnterDebug(string why)
            {
                debug = true;
                warp.Log(
                    string.Format(
                        "warp{0}: stopped ({1}) pc={2:x8} after {3} instructions",
                        Index,
                        why,
                        pc,
                        executed
                    )
                );
            }

            // ---- the I-cache: 2 ways x 64 sets x 32-byte lines (4 KB) [HW 2026-10-25] ----

            /// <summary>The I-cache is on with COP3 bit 0 or bit 2, either alone (1, 4 and 5 run the same) [HW].</summary>
            public bool CacheOn => (cp3[0] & 5) != 0;

            public uint CachedFetch(uint a, ulong p)
            {
                var set = (int)(a >> 5) & 63;
                var tag = a >> 11;
                var way = -1;
                for (var w = 0; w < 2; w++)
                {
                    if (lineValid[w, set] && lineTag[w, set] == tag)
                    {
                        way = w;
                    }
                }
                if (way < 0)
                {
                    // the victim is (pseudo-)random, also when the other way is empty: after A then B (same set, from
                    // empty) A was gone in 4 of 12 runs, after A B A C A or B stayed 6 of 12 each [HW 2026-10-25];
                    // a deterministic xorshift here keeps sessions repeatable
                    victim ^= victim << 13;
                    victim ^= victim >> 17;
                    victim ^= victim << 5;
                    way = (int)(victim >> 7) & 1;
                    var b = p & ~0x1Ful;
                    for (var i = 0; i < 8; i++)
                    {
                        var v = warp.BusRead(b + (ulong)(4 * i), 4);
                        var o = way * 2048 + set * 32 + 4 * i;
                        line[o] = (byte)v;
                        line[o + 1] = (byte)(v >> 8);
                        line[o + 2] = (byte)(v >> 16);
                        line[o + 3] = (byte)(v >> 24);
                    }
                    lineTag[way, set] = tag;
                    lineValid[way, set] = true;
                    Cost += LineFill;
                }
                return DataWord(way * 2048 + (int)(a & 0x7FC));
            }

            private uint DataWord(int o) =>
                (uint)(line[o] | line[o + 1] << 8 | line[o + 2] << 16 | line[o + 3] << 24);

            /// <summary>The data RAM through COP3 bit 4: way 0 at +0x000, way 1 at +0x800 [INF]. A direct read of a line's
            /// words gave them in place (2026-10-25); an earlier probe saw each word 8 bytes early; modelled in place.</summary>
            public uint CacheDataWord(uint a) => DataWord((int)(a & 0xFFC));

            public void CacheDataStore(uint a, uint v, int size)
            {
                var o = (int)(a & 0xFFF);
                for (var i = 0; i < size; i++)
                {
                    line[(o + i) & 0xFFF] = (byte)(v >> (8 * i));
                }
            }

            /// <summary>The tag RAM through COP3 bit 4: an entry reads 0x300000 | address bits 30:11 (0x3C3F95 for code at
            /// 0x61FCA800); bit 20 is the valid bit, bit 21 doesn't matter (writes) [HW 2026-10-25]. Which word shows
            /// which way is not settled: here word n of every 0x200 bytes = way n / 64, set n % 64 [INF].</summary>
            public uint CacheTagWord(uint o)
            {
                var n = (int)(o & 0x1FF) / 4;
                return lineValid[n >> 6, n & 63]
                    ? 0x300000u | (lineTag[n >> 6, n & 63] & 0xFFFFF)
                    : 0;
            }

            public void CacheTagStore(uint o, uint v)
            {
                var n = (int)(o & 0x1FF) / 4;
                lineValid[n >> 6, n & 63] = (v & 0x100000) != 0; // bit 20 = valid [HW]
                lineTag[n >> 6, n & 63] = v & 0xFFFFF;
            }

            // ---- execution ----

            /// <summary>The last slice only polled: no store changed anything, few distinct instructions, and either a
            /// few addresses read over and over or a tight loop with no loads at all. The next slices run a short budget
            /// (the program still sees every change within a slice; only its spin count differs), so a Warp program
            /// waiting on the ARM or on Sol costs little host time. A compute loop that streams its input (many
            /// addresses) runs at full speed even while it stores nothing.</summary>
            public bool Waiting { get; private set; }

            public int Changes;

            /// <summary>The last slice fetched code from SDRAM uncached (it slows the other Warp's SDRAM loads).</summary>
            public bool FetchingSdram => Running && lastUncached > 0;
            public long UncachedSdramFetches;
            private long lastUncached;

            public void NoteLoad(uint a)
            {
                if (loadAddrs.Count <= 16)
                {
                    loadAddrs.Add(a & ~3u);
                }
            }

            /// <summary>Run for this many cycles.</summary>
            public void Run(long n)
            {
                Changes = 0;
                pcs.Clear();
                loadAddrs.Clear();
                UncachedSdramFetches = 0;
                var full = n >= WaitSlice;
                while (n > 0 && Running)
                {
                    if (pcs.Count < 256)
                    {
                        pcs.Add(pc);
                    }
                    n -= Step();
                }
                lastUncached = UncachedSdramFetches;
                var was = Waiting;
                Waiting =
                    full
                    && Changes == 0
                    && pcs.Count < 256
                    && (pcs.Count <= 4 || (loadAddrs.Count > 0 && loadAddrs.Count <= 16));
                if (Waiting != was && waitLogs++ < 20)
                {
                    warp.Log(
                        string.Format(
                            "warp{0}: {1} pc={2:x8}",
                            Index,
                            Waiting ? "waiting (polls only)" : "busy again",
                            pc
                        )
                    );
                }
            }

            private readonly HashSet<uint> pcs = new HashSet<uint>();
            private readonly HashSet<uint> loadAddrs = new HashSet<uint>();
            private int waitLogs;

            /// <summary>One instruction; returns its cycles.</summary>
            private long Step()
            {
                Cost = 1;
                var at = pc;
                var insn = warp.Fetch(this, at);
                if (hung)
                {
                    return Cost;
                }
                pc = npc;
                npc = pc + 4;
                executed++;
                // strict load delay: the previous load's register holds its address for this instruction, the value after
                var pr = pendReg;
                var pv = pendVal;
                pendReg = -1;
                written = 0;
                Exec(insn, at);
                if (pr >= 0 && pendReg != pr && (written & (1u << pr)) == 0)
                {
                    r[pr] = pv; // a write by this instruction wins over the load [HW]
                }
                if (hung)
                {
                    pc = at; // stuck on the access
                    npc = at + 4;
                }
                cycles += (ulong)Cost;
                return Cost;
            }

            private void CommitLoad()
            {
                if (pendReg >= 0)
                {
                    Set(pendReg, pendVal);
                    pendReg = -1;
                }
            }

            private void Set(int n, uint v)
            {
                if (n != 0)
                {
                    r[n] = v;
                    written |= 1u << n;
                }
            }

            /// <summary>A load into rt: the address now, the value after the next instruction.</summary>
            private void LoadTo(int rt, uint ea, uint v)
            {
                if (hung || rt == 0)
                {
                    return;
                }
                r[rt] = ea;
                pendReg = rt;
                pendVal = v;
            }

            private void Branch(bool taken, uint at, uint insn)
            {
                if (taken)
                {
                    npc = at + 4 + (uint)((int)(short)insn << 2);
                }
            }

            /// <summary>Outside MIPS I: the unit doesn't trap. movz/movn, mul and clz copy rs to rd, branch-likely does
            /// nothing (measured, `w0probe.py odd`); anything else is untested and a no-op here. Logged once per kind.</summary>
            private void Odd(uint insn, uint at, string what, bool measured = false)
            {
                if (oddSeen.Add(what) && oddSeen.Count < 64)
                {
                    warp.Log(
                        string.Format(
                            "warp{0}: {1} ({2:x8}) at {3:x8}: not MIPS I; {4}",
                            Index,
                            what,
                            insn,
                            at,
                            measured ? "runs as on the unit" : "untested on the unit, a no-op here"
                        )
                    );
                }
            }

            private void Exec(uint insn, uint at)
            {
                var op = insn >> 26;
                var rs = (int)(insn >> 21) & 31;
                var rt = (int)(insn >> 16) & 31;
                var rd = (int)(insn >> 11) & 31;
                var sa = (int)(insn >> 6) & 31;
                var imm = (uint)(int)(short)insn; // sign-extended
                var uimm = insn & 0xFFFF;
                var s = r[rs];
                var t = r[rt];
                uint ea;
                switch (op)
                {
                    case 0x00:
                        switch (insn & 0x3F)
                        {
                            case 0x00:
                                Set(rd, t << sa);
                                return;
                            case 0x02:
                                Set(rd, t >> sa);
                                return;
                            case 0x03:
                                Set(rd, (uint)((int)t >> sa));
                                return;
                            case 0x04:
                                Set(rd, t << (int)(s & 31));
                                return;
                            case 0x06:
                                Set(rd, t >> (int)(s & 31));
                                return;
                            case 0x07:
                                Set(rd, (uint)((int)t >> (int)(s & 31)));
                                return;
                            case 0x08:
                                npc = s;
                                return; // jr
                            case 0x09:
                                Set(rd, at + 8);
                                npc = s;
                                return; // jalr
                            case 0x0C:
                                Odd(insn, at, "syscall");
                                return;
                            case 0x0D:
                                Odd(insn, at, "break");
                                return;
                            case 0x10:
                                MultWait();
                                Set(rd, hi);
                                return;
                            case 0x11:
                                hi = s;
                                return;
                            case 0x12:
                                MultWait();
                                Set(rd, lo);
                                return;
                            case 0x13:
                                lo = s;
                                return;
                            case 0x18:
                            {
                                var p = (long)(int)s * (int)t;
                                lo = (uint)p;
                                hi = (uint)(p >> 32);
                                multReady = cycles + (ulong)Cost + 3;
                                return;
                            }
                            case 0x19:
                            {
                                var p = (ulong)s * t;
                                lo = (uint)p;
                                hi = (uint)(p >> 32);
                                multReady = cycles + (ulong)Cost + 3;
                                return;
                            }
                            case 0x1A:
                            case 0x1B:
                                hi = lo = 0; // no divider
                                if (!divLogged)
                                {
                                    divLogged = true;
                                    warp.Log(
                                        string.Format(
                                            "warp{0}: div at {1:x8}: the Warp has no divider (HI = LO = 0)",
                                            Index,
                                            at
                                        )
                                    );
                                }
                                return;
                            case 0x20:
                            case 0x21:
                                Set(rd, s + t);
                                return; // add (no overflow trap), addu
                            case 0x22:
                            case 0x23:
                                Set(rd, s - t);
                                return;
                            case 0x24:
                                Set(rd, s & t);
                                return;
                            case 0x25:
                                Set(rd, s | t);
                                return;
                            case 0x26:
                                Set(rd, s ^ t);
                                return;
                            case 0x27:
                                Set(rd, ~(s | t));
                                return;
                            case 0x2A:
                                Set(rd, (int)s < (int)t ? 1u : 0);
                                return;
                            case 0x2B:
                                Set(rd, s < t ? 1u : 0);
                                return;
                            case 0x0A:
                            case 0x0B:
                                Odd(insn, at, "movz/movn", true);
                                Set(rd, s);
                                return; // rd := rs [HW]
                        }
                        Odd(insn, at, string.Format("SPECIAL function {0:x2}", insn & 0x3F));
                        return;
                    case 0x01:
                        switch (rt)
                        {
                            case 0x00:
                                Branch((int)s < 0, at, insn);
                                return;
                            case 0x01:
                                Branch((int)s >= 0, at, insn);
                                return;
                            case 0x10:
                                Set(31, at + 8);
                                Branch((int)s < 0, at, insn);
                                return;
                            case 0x11:
                                Set(31, at + 8);
                                Branch((int)s >= 0, at, insn);
                                return;
                        }
                        Odd(insn, at, string.Format("REGIMM {0:x2}", rt));
                        return;
                    case 0x1C: // SPECIAL2: mul, clz give rd := rs [HW]
                        if ((insn & 0x3F) == 0x02 || (insn & 0x3F) == 0x20)
                        {
                            Odd(insn, at, string.Format("SPECIAL2 {0:x2}", insn & 0x3F), true);
                            Set(rd, s);
                            return;
                        }
                        Odd(insn, at, string.Format("SPECIAL2 {0:x2}", insn & 0x3F));
                        return;
                    case 0x14:
                    case 0x15:
                    case 0x16:
                    case 0x17: // branch-likely: neither branch nor skip [HW]
                        Odd(insn, at, "branch-likely", true);
                        return;
                    case 0x02:
                        npc = (at + 4 & 0xF0000000) | (insn & 0x03FFFFFF) << 2;
                        return;
                    case 0x03:
                        Set(31, at + 8);
                        npc = (at + 4 & 0xF0000000) | (insn & 0x03FFFFFF) << 2;
                        return;
                    case 0x04:
                        Branch(s == t, at, insn);
                        return;
                    case 0x05:
                        Branch(s != t, at, insn);
                        return;
                    case 0x06:
                        Branch((int)s <= 0, at, insn);
                        return;
                    case 0x07:
                        Branch((int)s > 0, at, insn);
                        return;
                    case 0x08:
                    case 0x09:
                        Set(rt, s + imm);
                        return; // addi (no trap), addiu
                    case 0x0A:
                        Set(rt, (int)s < (int)imm ? 1u : 0);
                        return;
                    case 0x0B:
                        Set(rt, s < imm ? 1u : 0);
                        return;
                    case 0x0C:
                        Set(rt, s & uimm);
                        return;
                    case 0x0D:
                        Set(rt, s | uimm);
                        return;
                    case 0x0E:
                        Set(rt, s ^ uimm);
                        return;
                    case 0x0F:
                        Set(rt, uimm << 16);
                        return;
                    case 0x10: // COP0: the unit's constants
                        if (rs == 0)
                        {
                            Set(rt, Cp0Read(rd));
                        }
                        return;
                    case 0x11: // COP1 = the debug registers
                        if (rs == 0)
                        {
                            Set(rt, Cp1Read(rd));
                            return;
                        }
                        if (rs == 4)
                        {
                            Cp1Write(rd, t);
                        }
                        return;
                    case 0x12: // COP2 reads 0
                        if (rs == 0 || rs == 2)
                        {
                            Set(rt, 0);
                        }
                        return;
                    case 0x13: // COP3: register 0 = cache control
                        if (rs == 0)
                        {
                            Set(rt, cp3[rd]);
                            return;
                        }
                        if (rs == 4)
                        {
                            cp3[rd] = t;
                        }
                        return;
                    case 0x20:
                        ea = s + imm;
                        LoadTo(rt, ea, (uint)(int)(sbyte)warp.Load(this, ea, 1));
                        return;
                    case 0x21:
                        ea = s + imm;
                        LoadTo(rt, ea, (uint)(int)(short)HalfLoad(ea));
                        return;
                    case 0x23:
                        ea = s + imm;
                        LoadTo(rt, ea, WordLoad(ea));
                        return;
                    case 0x24:
                        ea = s + imm;
                        LoadTo(rt, ea, warp.Load(this, ea, 1));
                        return;
                    case 0x25:
                        ea = s + imm;
                        LoadTo(rt, ea, HalfLoad(ea));
                        return;
                    case 0x30:
                        Odd(insn, at, "ll", true);
                        return; // ll: nothing [HW]
                    case 0x38:
                        Odd(insn, at, "sc", true);
                        ea = s + imm;
                        warp.Store(this, ea & ~3u, 0, 4);
                        return; // sc: stores 0 [HW]
                    case 0x22:
                    case 0x26:
                        return; // lwl/lwr: the target stays as it was
                    case 0x28:
                        ea = s + imm;
                        warp.Store(this, ea, t, 1);
                        return;
                    case 0x29:
                        ea = s + imm;
                        warp.Store(this, ea & ~1u, t, 2);
                        return;
                    case 0x2A:
                        ea = s + imm;
                        Swl(ea, t);
                        return;
                    case 0x2B:
                        ea = s + imm;
                        warp.Store(this, ea & ~3u, t, 4);
                        return;
                    case 0x2E:
                        ea = s + imm;
                        Swr(ea, t);
                        return;
                }
                Odd(insn, at, string.Format("opcode {0:x2}", op));
            }

            // Unaligned loads as sampled on the unit (one data pattern, 0x44332211 0x88776655, 2026-10-25): lh/lhu at an
            // odd address give the addressed byte twice; lw at +1 the aligned word with byte 0 replaced by the addressed
            // byte; lw at +2 the next word's low half twice; lw at +3 the next word's bytes 1, 1, 0, 1.
            private uint HalfLoad(uint ea)
            {
                if ((ea & 1) == 0)
                {
                    return warp.Load(this, ea, 2);
                }
                var b = warp.Load(this, ea, 1);
                return b | b << 8;
            }

            private uint WordLoad(uint ea)
            {
                var w = warp.Load(this, ea & ~3u, 4);
                switch (ea & 3)
                {
                    case 1:
                        return (w & ~0xFFu) | ((w >> 8) & 0xFF);
                    case 2:
                    {
                        var n = warp.Load(this, (ea & ~3u) + 4, 4) & 0xFFFF;
                        return n | n << 16;
                    }
                    case 3:
                    {
                        var n = warp.Load(this, (ea & ~3u) + 4, 4);
                        var b0 = n & 0xFF;
                        var b1 = (n >> 8) & 0xFF;
                        return b1 | b1 << 8 | b0 << 16 | b1 << 24;
                    }
                    default:
                        return w;
                }
            }

            private void MultWait()
            {
                if (multReady > cycles + (ulong)Cost - 1)
                {
                    Cost += (long)(multReady - cycles - (ulong)Cost + 1); // ready 4 cycles after the multiply
                }
            }

            private static uint Cp0Read(int n)
            {
                switch (n)
                {
                    case 0:
                    case 1:
                    case 9:
                    case 13:
                        return 1;
                    case 5:
                    case 6:
                        return 0xFFFFFFFF;
                    default:
                        return 0;
                }
            }

            private void Swl(uint a, uint t)
            {
                var n = (int)(a & 3) + 1; // bytes a-(n-1) .. a get the top n bytes of t
                for (var i = 0; i < n; i++)
                {
                    warp.Store(this, (a & ~3u) + (uint)i, t >> (8 * (4 - n + i)), 1);
                }
            }

            private void Swr(uint a, uint t)
            {
                var n = 4 - (int)(a & 3); // bytes a .. a+n-1 get the low n bytes of t
                for (var i = 0; i < n; i++)
                {
                    warp.Store(this, a + (uint)i, t >> (8 * i), 1);
                }
            }

            private const uint ResetVector = 0xBFC00000; // ASSUMPTION (the MIPS one; no boot ROM there): the drivers always set the PC
            private const long LineFill = 34; // a 32-byte line: fitted to loops of 5-16 KB (2.7-5.3 cycles an

            // instruction) with random replacement [HW 2026-10-25]

            private readonly CR870_Warp warp;
            private readonly uint[] r = new uint[32];
            private readonly uint[] cp3 = new uint[32];
            private readonly byte[] line = new byte[4096];
            private readonly uint[,] lineTag = new uint[2, 64];
            private readonly bool[,] lineValid = new bool[2, 64];
            private uint victim = 1;
            private readonly HashSet<string> oddSeen = new HashSet<string>();
            private uint pc,
                npc,
                hi,
                lo,
                pendVal;
            private int pendReg = -1;
            private uint written;
            private bool debug,
                inReset,
                hung,
                haltReq,
                divLogged;
            private uint dataIn,
                dataOut;
            private ulong executed,
                cycles,
                multReady;
        }
    }
}
