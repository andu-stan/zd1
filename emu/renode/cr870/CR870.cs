// BridgeCo CR870 network module (DM870 "Sulu" SoC, ARM926EJ-S) for Renode. Where each register's meaning comes
// from: docs/module/memory-map.md.
//
// The CR870 is a plug-in module (DM870 + 32 MB SDRAM + 128 MB NAND + Wi-Fi RF) used by several AirPlay 1 era
// speakers; this machine is the module alone. Whatever it is plugged into talks to it through the J2 connector:
// here the SPI slave (CR870_SPISlave) and the power input (CR870_Core.Power), the UARTs, Ethernet, the I2S out.
//
// Memory (platforms/cr870.repl): low RAM 64 KB at 0, SRAM 64 KB 0x00400000 (uncached alias 0x00500000), SDRAM 32 MB
// 0x60000000 (uncached alias 0x70000000). Everything else is I/O: CR870_Core covers it in windows (CR870_Window)
// around the blocks that have their own models, reads back what was written ("sticky" registers, presets from live
// dumps) and models the VIC + timers + GPIO (0xFFFF0080), clocks/AV PLLs, the AV CTR and the 8 kHz stream tick,
// SuluDMA packet and block moves, the AV port 2 audio sink, the SDRAM controller, the watchdog / chip reset with the
// BL2 mailbox (0xB1005000) and the boot from NAND (BL1 stand-in, or the real decrypted BL1).
using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Text;
using Antmicro.Renode.Core;
using Antmicro.Renode.Core.Structure;
using Antmicro.Renode.Logging;
using Antmicro.Renode.Peripherals.Bus;
using Antmicro.Renode.Peripherals.CPU;
using Antmicro.Renode.Peripherals.UART;
using Antmicro.Renode.Time;

namespace Antmicro.Renode.Peripherals.CR870
{
    /// <summary>The module's log (zemu's module.log: the "dm870: ..." lines) and its MMIO trace (module.trace).</summary>
    public class CR870_Log
    {
        public CR870_Log(IMachine machine)
        {
            this.machine = machine;
        }

        public string LogFile
        {
            get => logFile;
            set
            {
                log?.Dispose();
                logFile = value;
                log = string.IsNullOrEmpty(value)
                    ? null
                    : new StreamWriter(value, true, Encoding.ASCII) { AutoFlush = true };
            }
        }

        public string TraceFile
        {
            get => traceFile;
            set
            {
                trace?.Dispose();
                traceFile = value;
                trace = string.IsNullOrEmpty(value)
                    ? null
                    : new StreamWriter(value, false, Encoding.ASCII, 1 << 20);
            }
        }

        public bool Tracing => trace != null;

        /// <summary>Only trace addresses whose hex starts with one of these prefixes ("b500,e0017"); empty = all.</summary>
        public string TraceOnly { get; set; } = "";

        public void Log(string s)
        {
            lock (this)
            {
                log?.Write("dm870: " + s + "\n");
            }
        }

        public void Trace(string s)
        {
            lock (this)
            {
                trace?.Write(s + "\n");
            }
        }

        public void Access(char kind, ulong a, ulong v, int size, uint pc)
        {
            if (trace == null)
            {
                return;
            }
            if (!string.IsNullOrEmpty(TraceOnly))
            {
                var hex = a.ToString("x8");
                if (
                    !TraceOnly
                        .Split(',')
                        .Any(p => p.Length > 0 && hex.StartsWith(p, StringComparison.Ordinal))
                )
                {
                    return;
                }
            }
            lock (this)
            {
                if (a == lastAddr && v == lastVal && kind == lastKind)
                {
                    if (++repeat == 1000000)
                    {
                        trace.Write(
                            string.Format(
                                "   ... {0} {1:x8} x1M (stuck poll?) pc={2:x8}\n",
                                kind,
                                a,
                                pc
                            )
                        );
                    }
                    return;
                }
                if (repeat >= 1)
                {
                    trace.Write(string.Format("   ... x{0}\n", repeat));
                }
                repeat = 0;
                lastAddr = a;
                lastVal = v;
                lastKind = kind;
                trace.Write(
                    string.Format("{0}{1} {2:x8} = {3:x8}  pc={4:x8}\n", kind, size, a, v, pc)
                );
            }
        }

        public void Flush()
        {
            lock (this)
            {
                trace?.Flush();
            }
        }

        private readonly IMachine machine;
        private string logFile,
            traceFile;
        private StreamWriter log,
            trace;
        private ulong lastAddr = ulong.MaxValue,
            lastVal;
        private char lastKind;
        private uint repeat;
    }

    /// <summary>
    /// The DM870's catch-all I/O plus the blocks without their own peripheral. See the file header; the register
    /// meanings and their evidence are in docs/module/memory-map.md.
    /// </summary>
    public class CR870_Core
        : IDoubleWordPeripheral,
            IWordPeripheral,
            IBytePeripheral,
            IKnownSize,
            IGPIOReceiver,
            INumberedGPIOOutput,
            IExecutableIO
    {
        public CR870_Core(IMachine machine, CR870_NAND nand)
        {
            this.machine = machine;
            this.nand = nand;
            Log = new CR870_Log(machine);
            Connections = new Dictionary<int, IGPIO> { { 0, new GPIO() } }; // -> cpu IRQ
            warp = new CR870_Warp(machine, Log);
            sol = new CR870_Sol(this);
            ResetState();
        }

        public IReadOnlyDictionary<int, IGPIO> Connections { get; }

        public CR870_Log Log { get; }

        // the core itself is registered as the 0xFFFF0000 window (vectors + VIC + timers + GPIO)
        public long Size => 0x10000;

        // ---- knobs (monitor: core ...; zemu sets most of them) ----
        public string LogFile
        {
            get => Log.LogFile;
            set => Log.LogFile = value;
        }
        public string TraceFile
        {
            get => Log.TraceFile;
            set => Log.TraceFile = value;
        }
        public string TraceOnly
        {
            get => Log.TraceOnly;
            set => Log.TraceOnly = value;
        }

        /// <summary>Boot from NAND like the hardware (DM870_BL1); false = the stage 2 image is loaded by the script.</summary>
        public bool BootFromNand { get; set; }

        /// <summary>Without BootFromNand: the stage 2 image loaded at Stage2Address on every power-up (boot = "app").</summary>
        public string Stage2Image { get; set; }

        public uint Stage2Address { get; set; } = 0x60008000;

        /// <summary>The real decrypted BL1 instead of the stand-in (DM870_BL1_IMAGE); implies TimerFreerun.</summary>
        public string Bl1Image { get; set; }

        /// <summary>ASSUMPTION (DM870_BL1_FALLBACK): no bootable app -> BL2.</summary>
        public bool Bl1Fallback { get; set; }

        /// <summary>Timer counters do not wrap at the threshold (the real BL1's timeouts; DM870_TIMER_FREERUN).</summary>
        public bool TimerFreerun { get; set; }

        /// <summary>RAM pokes applied on the first MMIO write: "addr=word,..." (DM870_PATCH).</summary>
        public string Patch { get; set; }

        /// <summary>
        /// Busy-wait loops on the 80 MHz timestamp at 0xFFFF0230, "pc:startReg:ticksReg,...": a counter read at pc (the
        /// loop's `ldr`, while `counter - r[start] < r[ticks]`) halts the CPU until the deadline or the next IRQ, so
        /// the spin costs no host time; virtual time and the loop's result are unchanged. Stock app: 0x60179848:4:0.
        /// </summary>
        public string DelayLoops
        {
            get => delayLoops;
            set
            {
                delayLoops = value;
                delayPcs.Clear();
                foreach (
                    var spec in (value ?? "").Split(
                        new[] { ',' },
                        StringSplitOptions.RemoveEmptyEntries
                    )
                )
                {
                    var p = spec.Split(':');
                    delayPcs[Convert.ToUInt32(p[0], 16)] = Tuple.Create(
                        int.Parse(p[1]),
                        int.Parse(p[2])
                    );
                }
            }
        }

        /// <summary>
        /// KnOS idle loop to WFI (default on): KnOS (app and BL2) copies its idle loop to SRAM 0x4003a0, which spins
        /// while a tick counter is non-zero instead of sleeping; only interrupts change what it polls, so going to
        /// WFI at once is equivalent. Checked on the OS tick; the `bne` at 0x4003ac becomes a NOP when the 6 words
        /// match. Saves the 240 MIPS the module otherwise spends idling.
        /// </summary>
        public bool IdleWfi { get; set; } = true;

        /// <summary>Delay-loop halts taken (statistics).</summary>
        public ulong DelayHalts { get; private set; }

        /// <summary>
        /// Busy-wait loops on a timer counter (+0x210/4/8) against its threshold, "pc,pc,...": a counter read at pc
        /// while `threshold > counter >> 4` halts the CPU until the counter gets there (or the next IRQ). The real
        /// BL1's timeouts: its helper 0x405828 (counter read at 0x405830) is what its 2 s serial window, the flash
        /// waits and the start-up delays spin on; at Renode's MMIO rate that window alone costs minutes of host time.
        /// </summary>
        public string TimerWaitPcs
        {
            get => timerWaitPcs;
            set
            {
                timerWaitPcs = value;
                timerWaitPc.Clear();
                foreach (
                    var spec in (value ?? "").Split(
                        new[] { ',' },
                        StringSplitOptions.RemoveEmptyEntries
                    )
                )
                {
                    timerWaitPc.Add(Convert.ToUInt32(spec, 16));
                }
            }
        }

        /// <summary>The 8 kHz stream tick (DM870_AV8K, default on).</summary>
        public bool Av8k { get; set; } = true;

        /// <summary>Request line GPIO (DM870_REQ_GPIO, default 6).</summary>
        public int RequestGpio { get; set; } = 6;

        /// <summary>AV PLL 0 frequency (DM870_PLL0_HZ): the module's I2S master clock, 512 x 44.1 kHz.</summary>
        public double Pll0Hz { get; set; } = 22579200.0;

        /// <summary>AV port 2 audio as a 24-bit stereo WAV (DM870_AUDIO_WAV).</summary>
        public string AudioWav { get; set; }

        /// <summary>Fill SRAM with addr ^ 0x5A5A5A5A before stage 1 (DM870_SRAM_PATTERN).</summary>
        public bool SramPattern { get; set; }

        /// <summary>The module's I2S output (AV port 2): stereo frames (24-bit samples) at the module's pace.</summary>
        public event Action<int, int> I2SFrame;

        /// <summary>Request line level (GPIO RequestGpio) changed.</summary>
        public event Action<bool> RequestLine;

        /// <summary>The request line as the MCU sees it: driven high by the module (released, the line's pull-down wins).</summary>
        public bool RequestLevel => RequestGpio >= 0 && ((GpioLevels() >> RequestGpio) & 1) != 0;

        /// <summary>The pad switch matrix (0xE0013000, 16 groups): until BL2/KnOS/our code write it, the SPI-slave pads,
        /// the request line and the GPIO inputs are disconnected (the Nokia kernel never writes it).</summary>
        internal bool PadsRouted => Get(0xE0013024) == 1; // MtxEthernet (the block repeats 16 times: one copy here)

        /// <summary>The factory-reset strap (GPIO 4, active high, internal pull-down): MCU PB01 on the Zeppelin [HW].</summary>
        public bool FactoryResetStrap { get; set; }

        /// <summary>Raised on a chip reset (watchdog, power-up) after the peripherals are back to their reset state.</summary>
        public event Action ChipReset;

        // ---- power (the J2 power input: on the Zeppelin PA04 on and PA17 released) ----

        /// <summary>The J2 power input switches the module (default). False: it is powered on its own and ignores the
        /// host's power pin (zemu without the main board).</summary>
        public bool PowerGated { get; set; } = true;

        /// <summary>Module power. Off: the CPU is halted and the chip held in reset. On: a cold boot (RAM cleared).</summary>
        public bool Power
        {
            get => powered;
            set
            {
                if (value == powered || !PowerGated)
                {
                    return;
                }
                powered = value;
                Log.Log(string.Format("module power {0}", value ? "ON (reset released)" : "OFF"));
                Log.Trace(string.Format("POWER {0}", value ? "on" : "off"));
                ResetChip(cold: true, run: value);
            }
        }

        /// <summary>Powered at start (no host controls the power pin, e.g. a module-only run).</summary>
        public void PowerOnAtStart()
        {
            powered = true;
            Boot(cold: true);
        }

        public void Reset()
        {
            // Renode's machine reset: same as a chip reset without the boot (the script or Power does the boot)
            ResetState();
        }

        // ---- interrupts in (level lines: UARTs, SPI slave, MACB, block-move tags) ----
        public void OnGPIO(int number, bool value)
        {
            if (number < 0 || number >= 128)
            {
                return;
            }
            SetBit(level, number, value);
            VicUpdate();
        }

        // ---- bus ----
        public uint ReadDoubleWord(long offset) => (uint)Read(0xFFFF0000u + (ulong)offset, 4);

        public void WriteDoubleWord(long offset, uint value) =>
            Write(0xFFFF0000u + (ulong)offset, value, 4);

        public ushort ReadWord(long offset) => (ushort)Read(0xFFFF0000u + (ulong)offset, 2);

        public void WriteWord(long offset, ushort value) =>
            Write(0xFFFF0000u + (ulong)offset, value, 2);

        public byte ReadByte(long offset) => (byte)Read(0xFFFF0000u + (ulong)offset, 1);

        public void WriteByte(long offset, byte value) =>
            Write(0xFFFF0000u + (ulong)offset, value, 1);

        public ulong ReadQuadWord(long offset) =>
            ReadDoubleWord(offset) | (ulong)ReadDoubleWord(offset + 4) << 32;

        public void WriteQuadWord(long offset, ulong value)
        {
            WriteDoubleWord(offset, (uint)value);
            WriteDoubleWord(offset + 4, (uint)(value >> 32));
        }

        // IExecutableIO: the vector table lives in this page (the CPU fetches it through these)
        public byte[] ReadBytes(long offset, int count, IPeripheral context = null)
        {
            var r = new byte[count];
            for (var i = 0; i < count; i++)
            {
                r[i] = ReadByte(offset + i);
            }
            return r;
        }

        public void WriteBytes(
            long offset,
            byte[] array,
            int startingIndex,
            int count,
            IPeripheral context = null
        )
        {
            for (var i = 0; i < count; i++)
            {
                WriteByte(offset + i, array[startingIndex + i]);
            }
        }

        /// <summary>Statistics: accesses per address (development aid; off by default).</summary>
        public bool CountAccesses { get; set; }

        public bool CountAll
        {
            get => CR870_Stats.On;
            set => CR870_Stats.On = value;
        }

        public string OtherStats()
        {
            lock (CR870_Stats.Counts)
            {
                return string.Join(
                    "\n",
                    CR870_Stats
                        .Counts.OrderByDescending(x => x.Value)
                        .Take(20)
                        .Select(x => x.Key + " " + x.Value)
                );
            }
        }

        public string AccessStats()
        {
            return string.Join(
                "\n",
                counts
                    .OrderByDescending(x => x.Value)
                    .Take(30)
                    .Select(x => string.Format("{0:x8} {1}", x.Key, x.Value))
            );
        }

        private readonly Dictionary<ulong, ulong> counts = new Dictionary<ulong, ulong>();

        /// <summary>A Warp core's state (monitor: `core WarpStatus 1`).</summary>
        public string WarpStatus(int n) => warp.Status(n);

        private readonly CR870_Warp warp;
        private readonly CR870_Sol sol;

        internal ulong Read(ulong a, int size)
        {
            if ((a & ~0xFFFul) == 0xE0013000)
            {
                a &= ~0xF00ul; // the pad switch matrix repeats every 0x100 [HW]
            }
            if (a >= CR870_Warp.Base && a < CR870_Warp.End)
            {
                var w = warp.Read(a & ~3ul);
                Access('R', a, w, size);
                return w;
            }
            if (a >= 0xFFFF000C && a <= 0xFFFF0014 && size == 4 && Get(a) == 0)
            {
                return DeadVector(a);
            }

            if (!vectorsChecked)
            {
                CheckVectors();
            }
            if (CountAccesses)
            {
                counts[a] = counts.TryGetValue(a, out var k) ? k + 1 : 1;
            }
            ulong v;
            if (a >= 0xFFFF0080 && a < 0xFFFF1000 && VicRead(a - 0xFFFF0000, out v))
            {
                if (a != 0xFFFF0230)
                {
                    Access('R', a, v, size);
                }
                return v;
            }
            if (a == 0xB5000004)
            {
                return bmoveBusy;
            }
            if (a == 0xB5001C44)
            {
                v = bmoveTh != bmoveTt ? bmoveTag[bmoveTh & (BMQ - 1)] : 0x3fu;
                Access('R', a, v, size);
                return v;
            }
            if (a == 0xB5001C48 || a == 0xB5001C4C)
            {
                var g = (int)((a - 0xB5001C40) / 4);
                v = isoTh[g] != isoTt[g] ? isoTag[g, isoTh[g] & 15] : 0x3fu;
                Access('R', a, v, size);
                return v;
            }
            if (a >= 0xE0007800 && a < 0xE0007840 && (a & 0xF) <= 8)
            {
                // AV PLL n status: InLock +0, PhErr +4, IIRStat +8. On the unit [HW 2026-10-25] all 0 while the PLL
                // isn't programmed, and 0 / 0xFFFF / 0 with av-device's PLL (+0x140.. 0x80220849: bit 31 set) running.
                // KnOS's mode (bit 31 clear) reports lock and the IIR state its monitor expects [RE, not seen on the unit].
                var n = (int)((a - 0xE0007800) >> 4);
                var r = 0xE0007140u + (uint)n * 0x80;
                if (Get(r) == 0 && Get(r + 4) == 0 && Get(r + 8) == 0)
                {
                    return 0u;
                }
                if ((Get(r) & 0x80000000u) != 0)
                {
                    return (a & 0xF) == 4 ? 0xFFFFu : 0u;
                }
                return (a & 0xF) == 0 ? 1u
                    : (a & 0xF) == 8 ? PllIirStat(n)
                    : 0u;
            }
            if (a == 0xE001B000 || a == 0xE001B004)
            {
                // the AV cycle timer counts only once config +0x08 bit 0 is set: KnOS writes 1 there; under our OS
                // (Linux's av-device never does) it reads 0, also while audio plays [HW 2026-10-25]
                if ((Get(0xE001B008) & 1) == 0)
                {
                    return 0u;
                }
                return a == 0xE001B000 ? AvCtr() : (uint)((PollNs * 3072 / 125000) % 24576000);
            }
            // tgen clock/power gates (NOKIA_HD1, docs/module/warp.md): +0x28 enable set, +0x2C disable clear; +0x20/+0x24 reset
            // set/clear. sulu_power_enable(n) writes 1<<n to +0x28, sulu_power_disable(n) to +0x2C. Gate 2 = Warp0,
            // 20/21/22 = Warp1/Sol, 19 = WLAN ARM7. Preset to the unit's stock state (0xE0007028 read 0x00FFFFED).
            if (a == 0xE0007028 || a == 0xE000702C)
            {
                // the enable mask, except bits 1, 4 and 24 up, which read 0 whatever is written [HW 2026-10-25]
                Access('R', a, tgenGate & 0x00FFFFEDu, size);
                return tgenGate & 0x00FFFFEDu;
            }
            if (a == 0xE0007020 || a == 0xE0007024)
            {
                Access('R', a, tgenReset, size);
                return tgenReset;
            }
            if (
                (a & 0xFFFF0000) == 0xE0000000
                && ((a >> 12) & 0xF) >= 8
                && ((a >> 12) & 0xF) <= 0xC
                && ((a & 0xFFF) == 0x000 || (a & 0xFFF) == 0x808 || (a & 0xFFF) == 0x810)
            )
            {
                var b = a & ~0xFFFul;
                var ctrl = Get(b);
                var cfg = sticky.ContainsKey(b + 8);
                v =
                    (a & 0xFFF) == 0x000
                        ? (ctrl & ~0x0F000000u) | ((ctrl & 0xF) != 0 ? 0x01000000u : 0)
                    : (a & 0xFFF) == 0x808 ? (cfg ? 1u : 0)
                    : (cfg ? 0x716u : 0);
                Access('R', a, v, size);
                return v;
            }
            v =
                size >= 4
                    ? Get(a)
                    : (Get(a & ~3ul) >> (int)(8 * (a & 3))) & (size == 1 ? 0xFFu : 0xFFFFu);
            Access('R', a, v, size); // sub-word: byte lanes of the (little-endian) word
            return v;
        }

        internal void Write(ulong a, ulong value, int size)
        {
            if ((a & ~0xFFFul) == 0xE0013000)
            {
                a &= ~0xF00ul;
            }
            if (a >= CR870_Warp.Base && a < CR870_Warp.End)
            {
                Access('W', a, value, size);
                warp.Write(a & ~3ul, (uint)value);
                return;
            }
            if (!vectorsChecked)
            {
                CheckVectors();
            }
            ApplyPatches();
            var v = (uint)value;
            if (a >= 0xB5003000 && a < 0xB5003040 && (v & 0x800) != 0)
            {
                SdmaMove((int)((a - 0xB5003000) / 4), v); // packet move, done at once (~5500/s: not traced)
                sticky[a] = v & ~0x800u;
                return;
            }
            if (a == 0xE001D004)
            {
                v &= ~0x180000u; // SDRAM controller command bits self-clear when done
            }
            if (a == 0xE0007028 || a == 0xE000702C || a == 0xE0007020 || a == 0xE0007024)
            {
                Access('W', a, v, size);
                var set = (a & 0xF) == 0x8 || (a & 0xF) == 0x0; // +0x28/+0x20 set bits, +0x2C/+0x24 clear bits
                if (a < 0xE0007028) // +0x20/+0x24 = reset domain
                {
                    tgenReset = set ? tgenReset | v : tgenReset & ~v;
                }
                else
                {
                    tgenGate = set ? tgenGate | v : tgenGate & ~v;
                }
                return;
            }
            if (a == 0xB5000000)
            {
                Access('W', a, v, size);
                BmoveStart(v);
                return;
            }
            if (a == 0xB5001C44)
            {
                if (bmoveTh != bmoveTt)
                {
                    bmoveTh++;
                }
                BmoveIrq();
                return;
            }
            if (a == 0xB5001C48 || a == 0xB5001C4C)
            {
                var g = (int)((a - 0xB5001C40) / 4);
                Access('W', a, v, size);
                if (isoTh[g] != isoTt[g])
                {
                    isoTh[g]++;
                }
                IsoIrq(g);
                return;
            }
            if (a >= 0xB5009000 && a < 0xB5009400 && (a & 3) == 0)
            {
                // the fast queue (newdma_packet_queue_fast): `(address & 0xFFFF) << 8 | ring` into +0x9000 + ctx * 4
                // for a stream that reads memory (packet type 0x78000000: the new source, as the slow path's +0x0C),
                // +0x9200 + ctx * 4 for one that writes it (the destination): the context is full again
                Access('W', a, v, size);
                var ctx = (uint)((a - 0xB5009000) & 0x1FF) / 4;
                var c = 0xB5004000u + ctx * 0x40;
                Set(c + (a < 0xB5009200 ? 0x0Cu : 0x08u), (v >> 8) & 0xFFFF);
                Set(c + 0x24, Get(c + 0x24) & ~0x80u);
                return;
            }
            if (a >= 0xB5006000 && a < 0xB5007000 && (a & 0x3F) == 0)
            {
                Access('W', a, v, size);
                sticky[a] = v;
                IsoStart((int)((a - 0xB5006000) / 0x40));
                return;
            }
            Access('W', a, v, size);
            if (a == 0xD0740018 && sol.ControlWrite(a, v))
            {
                return; // Sol ran the program and set the done bit itself
            }
            if (a >= 0xD0CC0000 && a < 0xD0D00000)
            {
                sol.UnitWrite(a); // Sol unit-3 window: the ALU/SIMD mode register
            }
            if (a >= 0xFFFF0080 && a < 0xFFFF1000 && VicWrite(a - 0xFFFF0000, v))
            {
                return;
            }
            if (size >= 4)
            {
                sticky[a] = v;
                return;
            }
            var sh = (int)(8 * (a & 3));
            var m = (size == 1 ? 0xFFu : 0xFFFFu) << sh;
            sticky[a & ~3ul] = (Get(a & ~3ul) & ~m) | ((v << sh) & m);
        }

        // ---------------------------------------------------------------------------------------------------------

        private void Access(char kind, ulong a, ulong v, int size)
        {
            if (Log.Tracing)
            {
                Log.Access(kind, a, v, size, Pc);
            }
        }

        internal uint Pc =>
            machine.SystemBus.TryGetCurrentCPU(out var cpu) ? (uint)cpu.PC.RawValue : 0;

        internal uint Get(ulong a) => sticky.TryGetValue(a, out var v) ? v : 0;

        internal void Set(ulong a, uint v) => sticky[a] = v;

        /// <summary>Exact virtual time (syncs the CPU: a call into tlib, costly; use when programming an event).</summary>
        internal ulong NowNs
        {
            get
            {
                if (machine.SystemBus.TryGetCurrentCPU(out var cpu))
                {
                    cpu.SyncTime();
                }
                return machine.ElapsedVirtualTime.TimeElapsed.Ticks;
            }
        }

        /// <summary>Virtual time as of the start of the CPU's current execution chunk (bounded by the next timer event
        /// and the quantum; deterministic), for registers firmware polls constantly (AV CTR, timestamp, counters).</summary>
        private string delayLoops;
        private readonly Dictionary<uint, Tuple<int, int>> delayPcs =
            new Dictionary<uint, Tuple<int, int>>();
        private string timerWaitPcs;
        private readonly HashSet<uint> timerWaitPc = new HashSet<uint>();
        private ulong delayGen;
        private TranslationCPU delayCpu;
        private bool delayMasked;

        private ulong av8kNext;

        internal ulong PollNs => machine.ElapsedVirtualTime.TimeElapsed.Ticks;

        private void At(ulong ns, Action a, string name)
        {
            if (CR870_Stats.On)
            {
                CR870_Stats.Hit("at " + name);
            }
            var now = NowNs;
            machine.ScheduleAction(
                TimeInterval.FromNanoseconds(ns > now ? ns - now : 0),
                _ => a(),
                name
            );
        }

        private static void SetBit(uint[] w, int i, bool v)
        {
            if (v)
            {
                w[i / 32] |= 1u << (i % 32);
            }
            else
            {
                w[i / 32] &= ~(1u << (i % 32));
            }
        }

        private static bool Bit(uint[] w, int i) => (w[i / 32] & (1u << (i % 32))) != 0;

        // ---- presets: live values from the unit (block IDs: re/netmodule/cr870-live-block-ids.txt; the rest
        // docs/module/memory-map.md: tgen clock ratios, SSM, the SDRAM controller, the UMR arbiter, the soft-DAC) ----
        private static readonly (uint a, uint v)[] Presets =
        {
            (0xE0007000, 0x00022800),
            (0xE0007004, 0x00000001),
            (0xE0007010, 0x00000006),
            (0xE0007014, 0x22221223),
            (0xE0007840, 0x00000001),
            (0xE0007844, 0x80010100),
            (0xB1000FF8, 0x06110801),
            (0xB1001FF8, 0x06110801),
            (0xB1002FF8, 0x06110801),
            (0xB1003FF8, 0x06110801),
            (0xB1004FF8, 0x06110801),
            (0xB1006FF8, 0x06110801),
            (0xB5001FF8, 0x06110801),
            (0xE0004FF8, 0x06102701),
            (0xE0007FF8, 0x06091401),
            (0xE0008FF8, 0x06111701),
            (0xE000AFF8, 0x06111701),
            (0xE000BFF8, 0x06111701),
            (0xE000CFF8, 0x06111701),
            (0xE0010800, 0x00000001),
            (0xE0010804, 0x0000001d),
            (0xE0010808, 0x00000010),
            (0xE0010810, 0x00000018),
            (0xE0010FF8, 0x06111701),
            (0xE0012FF8, 0x06111701),
            (0xE0014FF8, 0x06110701),
            (0xE0015FF8, 0x06110701),
            (0xE0017FFC, 0x0001010d),
            (0xE0018FF4, 0x00000080),
            (0xE0018FF8, 0x04032200),
            (0xE0019FF4, 0x01ff01ff),
            (0xE0019FF8, 0x05112900),
            (0xE001BFF8, 0x06090100),
            (0xE001DFF0, 0x00000001),
            (0xE001DFF4, 0x00000001),
            (0xE001DFF8, 0x06110801),
            (0xFFFF0FF8, 0x06110801),
            (0xB1004010, 0xFFFFFFFF),
            (0xB1004014, 0xFFFFFFFF),
            (0xB1004018, 0xFFFFFFFF),
            (0xB100401C, 0xFFFFFFFF),
            (0xB1004024, 0x61FB0000),
            (0xB1004028, 0x00000E00),
            (0xB1004040, 0x01A09999),
            (0xB1004050, 0x00000040),
            (0xB1004058, 0x00020000),
            (0xE001D004, 0xFE207C60),
            (0xE001D008, 0x00460000),
            (0xE001D00C, 0x0100046D),
            (0xE001D014, 0x00000005),
            (0xE0018004, 0x00000002),
            (0xE0018008, 0x00000006),
            (0xE0018014, 0x00000002),
            (0xE0018018, 0x00000006),
            (0xE0018020, 8),
            (0xE0018030, 8),
            (0xE0018040, 8),
            (0xE0018050, 8),
            (0xE0018060, 8),
            (0xE0018070, 8),
            (0xE0018080, 8),
            (0xE0013020, 0x00000900),
            (0xE0013028, 0x00000001), // the pad matrix at reset [HW 2026-10-25]
            (0xE001E000, 0x00004020),
            (0xE001E00C, 0x00004001),
            (0xE001E010, 0x00000003),
            (0xE001E014, 0x40000000),
        };

        private uint tgenGate,
            tgenReset;

        private void ResetState()
        {
            warp.Reset();
            sol.Reset();
            tgenGate = 0x00FFFFED; // stock power-up gate state (0xE0007028); bit 2 = Warp0 on
            tgenReset = 0x00000002; // 0xE0007020 observed on the unit
            var mb = new[] { Get(0xB1005000), Get(0xB1005004), Get(0xB1005008) };
            timerGen++;
            Array.Clear(pend, 0, 4);
            Array.Clear(insvc, 0, 4);
            Array.Clear(mask, 0, 4);
            Array.Clear(level, 0, 4);
            vicCfg = hpMask = tcfg = 0;
            Array.Clear(thd, 0, 3);
            bmoveTh = bmoveTt = bmovePh = bmovePt = 0;
            bmoveBusy = 0;
            gpioA = 0x000FFFFF; // reset: every pad released, B clear [HW 2026-10-25]
            gpioB = 0;
            sticky.Clear();
            foreach (var p in Presets)
            {
                sticky[p.a] = p.v;
            }
            for (var i = 0; i < 3; i++)
            {
                sticky[0xB1005000u + 4u * (uint)i] = mb[i]; // the BL2 mailbox survives the reset
            }
            patched = false;
            vectorsChecked = false;
            av8kLast = 0;
            lastIrq = -2;
            lineIrq = -1;
            Connections[0].Unset();
        }

        /// <summary>A chip reset (power-up, watchdog): peripherals back to reset (the mailbox survives), then boot.</summary>
        private void ResetChip(bool cold, bool run)
        {
            delayGen++; // a pending delay wake must not restart a stopped chip
            delayCpu = null;
            machine.RequestResetInSafeState(() =>
            {
                if (machine.SystemBus.GetCPUs().FirstOrDefault() is BaseCPU c)
                {
                    c.IsHalted = !run;
                }
                if (run)
                {
                    Boot(cold);
                }
            });
        }

        private void Boot(bool cold)
        {
            var cpu = machine.SystemBus.GetCPUs().FirstOrDefault() as BaseCPU;
            if (cold)
            {
                foreach (
                    var m in machine.GetPeripheralsOfType<Antmicro.Renode.Peripherals.Memory.MappedMemory>()
                )
                {
                    m.ZeroAll(); // power-up: no RAM contents survive
                }
            }
            ChipReset?.Invoke();
            StartTimers();
            if (cpu is Arm arm)
            {
                // the chip boots with high vectors (pin strap; platforms/cr870.repl sets it once): a machine reset puts
                // tlib back to 0x0, where the first exception of BL2 jumped into low RAM and looped on an abort
                arm.ExceptionVectorAddress = 0xFFFF0000;
            }
            if (cpu != null)
            {
                cpu.IsHalted = false;
            }
            if (!BootFromNand || nand == null)
            {
                if (!string.IsNullOrEmpty(Stage2Image) && cpu != null)
                {
                    machine.SystemBus.WriteBytes(File.ReadAllBytes(Stage2Image), Stage2Address);
                    cpu.PC = Stage2Address; // stage 2 straight into SDRAM (ROM/BL1/preloader skipped)
                }
                return;
            }
            if (SramPattern)
            {
                for (uint a = 0x400000; a < 0x410000; a += 4)
                {
                    machine.SystemBus.WriteDoubleWord(a, a ^ 0x5A5A5A5Au);
                }
            }
            var mb = new[] { Get(0xB1005000), Get(0xB1005004), Get(0xB1005008) };
            var toBl2 = mb[0] == 0xDEADBEEF && mb[1] == 0 && mb[2] == ~0xDEADBEEFu;
            if (toBl2 && string.IsNullOrEmpty(Bl1Image))
            {
                for (uint i = 0; i < 3; i++) // BL1 copies and clears the mailbox (docs/module/boot-chain.md):
                { // BL2's reset after an update boots the new app, not BL2 again
                    sticky[0xB1005000u + 4u * i] = 0;
                    machine.SystemBus.WriteDoubleWord(0xB1005000u + 4u * i, 0);
                }
            }
            if (!string.IsNullOrEmpty(Bl1Image))
            {
                var b = File.ReadAllBytes(Bl1Image);
                machine.SystemBus.WriteBytes(b.Take(0x10000).ToArray(), 0x400000);
                cpu.PC = 0x400000;
                Log.Log(
                    string.Format(
                        "real BL1 ({0} B) at 0x400000, mailbox {1:x8} {2:x8} {3:x8}",
                        Math.Min(b.Length, 0x10000),
                        mb[0],
                        mb[1],
                        mb[2]
                    )
                );
                return;
            }
            var want = toBl2 ? 1u : 2u;
            while (true)
            {
                var d = nand.Data;
                for (uint off = 0; off < d.Length; off += 0x20000)
                {
                    if (Encoding.ASCII.GetString(d, (int)off + 0x30, 8) != "MetaInfo")
                    {
                        continue;
                    }
                    var pid = BitConverter.ToUInt32(d, (int)off + 0x18);
                    var b = BitConverter.ToUInt32(d, (int)off + 0x20);
                    if (
                        pid != want
                        || b + 0x20 > d.Length
                        || BitConverter.ToUInt32(d, (int)b) != 0xAAAA5555
                    )
                    {
                        continue;
                    }
                    var addr = BitConverter.ToUInt32(d, (int)b + 0x0C);
                    var len = BitConverter.ToUInt32(d, (int)b + 0x10) * 8;
                    if (len > 0x10000)
                    {
                        break;
                    }
                    machine.SystemBus.WriteBytes(
                        d.Skip((int)b + 0x18).Take((int)len).ToArray(),
                        addr
                    );
                    cpu.PC = addr;
                    Log.Trace(
                        string.Format(
                            "BL1 stand-in: mailbox {0:x8} {1:x8} {2:x8} -> {3} partition @0x{4:x}, stage1 {5} B -> {6:x8}",
                            mb[0],
                            mb[1],
                            mb[2],
                            toBl2 ? "BL2" : "app",
                            off,
                            len,
                            addr
                        )
                    );
                    Log.Log(
                        string.Format(
                            "BL1 stand-in -> {0} (stage1 at {1:x8})",
                            toBl2 ? "BL2" : "app",
                            addr
                        )
                    );
                    return;
                }
                Log.Log(string.Format("BL1 stand-in: no bootable type-{0} partition", want));
                if (want == 2 && Bl1Fallback)
                {
                    Log.Log("BL1 stand-in: DM870_BL1_FALLBACK -> BL2");
                    want = 1;
                    toBl2 = true;
                    continue;
                }
                if (cpu != null)
                {
                    cpu.IsHalted = true; // the ROM finds nothing to boot and waits (until a reset)
                }
                return;
            }
        }

        // Abort vectors the software left empty (KnOS installs none): the CPU would run 0s into the IRQ vector in
        // abort mode and wreck its state (tlib then stops the whole emulation: "Bad mode"). A real chip just hangs
        // until its watchdog; so do we: 0x0C/0x10 = WFI, 0x14 = b 0x10 (IRQs are masked in abort mode).
        private ulong DeadVector(ulong a)
        {
            if (
                machine.SystemBus.TryGetCurrentCPU(out var c)
                && c is Arm arm
                && (uint)arm.PC.RawValue == a
                && crashes++ < 5
            )
            {
                var regs = Enumerable
                    .Range(0, 15)
                    .Select(i =>
                        string.Format("r{0}={1:x8}", i, (uint)arm.GetRegisterUnsafe(i).RawValue)
                    );
                Log.Log(
                    string.Format(
                        "CRASH: {0} with no handler installed, the module hangs until a reset (cpsr {1:x8} {2})",
                        a == 0xFFFF0010 ? "data abort" : "prefetch abort",
                        (uint)arm.CPSR.RawValue,
                        string.Join(" ", regs)
                    )
                );
            }
            if (
                machine.SystemBus.TryGetCurrentCPU(out var h)
                && h is BaseCPU hc
                && (uint)hc.PC.RawValue == a
            )
            {
                hc.IsHalted = true; // a pending (masked) IRQ ends every WFI at once: halt instead
            }
            return a == 0xFFFF0014 ? 0xEAFFFFFDu : 0xEE070F90u;
        }

        // high vectors are a pin strap: a machine reset (power, watchdog) can leave tlib at 0x0 after Boot() set them,
        // so check again on the first I/O access of the new boot
        private void CheckVectors()
        {
            vectorsChecked = true;
            if (
                machine.SystemBus.GetCPUs().FirstOrDefault() is Arm arm
                && arm.ExceptionVectorAddress != 0xFFFF0000
            )
            {
                Log.Log(
                    string.Format(
                        "vectors were at {0:x8} after the reset: back to 0xffff0000",
                        arm.ExceptionVectorAddress
                    )
                );
                arm.ExceptionVectorAddress = 0xFFFF0000;
            }
        }

        private void ApplyPatches()
        {
            if (patched || string.IsNullOrEmpty(Patch))
            {
                return;
            }
            patched = true;
            foreach (var p in Patch.Split(','))
            {
                var kv = p.Split('=');
                if (kv.Length != 2)
                {
                    continue;
                }
                var a = Convert.ToUInt32(kv[0], 16);
                var w = Convert.ToUInt32(kv[1], 16);
                machine.SystemBus.WriteDoubleWord(a, w);
                Log.Trace(string.Format("PATCH {0:x8} = {1:x8}", a, w));
            }
        }

        // ---- VIC + timers + GPIO at 0xFFFF0080 ----
        private const ulong THz = 80000000;

        // the timestamp at +0x230 counts at the timer clock too (80 MHz, 12.5 ns): Linux registers it as an 80 MHz
        // clocksource (sulu_timer_read, mult for 80 MHz), KnOS's delay computes ticks = clock * us / 1e6
        private ulong TsTicks(ulong t) => t > tsbase ? (t - tsbase) * 2 / 25 : 0;

        private ulong TimerHz(int n) => (tcfg & (0x100u << n)) != 0 ? THz / 16 : THz;

        private bool LineEnabled(int irq)
        {
            for (var k = 0; k < 8; k++)
            {
                if (((hpSel[k / 4] >> (8 * (k % 4))) & 0x7f) == (uint)irq)
                {
                    return (hpMask & (1u << k)) != 0;
                }
            }
            return Bit(mask, irq);
        }

        private int NextIrq()
        {
            for (var k = 0; k < 8; k++)
            {
                var irq = (int)((hpSel[k / 4] >> (8 * (k % 4))) & 0x7f);
                if (
                    irq != 0x7f
                    && (hpMask & (1u << k)) != 0
                    && (Bit(pend, irq) || Bit(level, irq))
                    && !Bit(insvc, irq)
                )
                {
                    return irq;
                }
            }
            for (var irq = 0; irq < 128; irq++)
            {
                if ((Bit(pend, irq) || Bit(level, irq)) && !Bit(insvc, irq) && LineEnabled(irq))
                {
                    return irq;
                }
            }
            return -1;
        }

        // Nokia's 2.6.24 mach-sulu kernel (blobs/module/nokia-hd1): default_idle disables IRQs, loads TIF_NEED_RESCHED, ignores
        // it and returns (arch_idle is empty): cpu_idle spins at 240 MIPS. Patched to mainline's arch_idle =
        // cpu_arm926_do_idle (WFI; it wakes on a masked IRQ): tst need_resched; bleq do_idle; IRQs on. Only on the
        // exact instruction pattern of that build (PA = VA - 0x60000000).
        private void PatchLinuxIdle()
        {
            var bus = machine.SystemBus;
            if (
                bus.ReadDoubleWord(0x600275a8) != 0xe10f3000
                || bus.ReadDoubleWord(0x600275ac) != 0xe3c33080
                || bus.ReadDoubleWord(0x600275b0) != 0xe121f003
                || bus.ReadDoubleWord(0x600275a4) != 0xe5933000
                || bus.ReadDoubleWord(0x60027598) != 0xe121f003
                || bus.ReadDoubleWord(0x6002f020) != 0xee070f90
            )
            {
                return;
            }
            bus.WriteDoubleWord(0x600275a8, 0xe3130002); // tst r3, #2 (TIF_NEED_RESCHED)
            bus.WriteDoubleWord(0x600275ac, 0x0b001e93); // bleq cpu_arm926_do_idle (0xc002f000)
            bus.WriteDoubleWord(0x600275b0, 0xe321f013); // msr CPSR_c, #0x13: SVC, IRQ + FIQ on
            foreach (var c in bus.GetCPUs().OfType<TranslationCPU>())
            {
                c.InvalidateTranslationBlocks();
            }
            Log.Log("Linux default_idle -> cpu_arm926_do_idle (WFI)");
        }

        private void PatchIdle()
        {
            var bus = machine.SystemBus;
            if (
                bus.ReadDoubleWord(0x4003ac) != 0x1affffe4
                || bus.ReadDoubleWord(0x4003b4) != 0xee070f90
                || bus.ReadDoubleWord(0x4003a0) != 0xe59f0020
                || bus.ReadDoubleWord(0x4003a4) != 0xe5900000
                || bus.ReadDoubleWord(0x4003a8) != 0xe3500000
                || bus.ReadDoubleWord(0x4003b0) != 0xe3a00000
            )
            {
                return;
            }
            bus.WriteDoubleWord(0x4003ac, 0xe1a00000);
            foreach (var c in bus.GetCPUs().OfType<TranslationCPU>())
            {
                c.InvalidateTranslationBlocks(); // once per copy of the loop
            }
            Log.Trace("KnOS idle loop at 0x4003a0 -> WFI");
        }

        // a counter read in a known delay loop: sleep (halted) until the loop would see its deadline
        private void DelayLoop(uint counter)
        {
            if (
                !machine.SystemBus.TryGetCurrentCPU(out var c)
                || !(c is TranslationCPU cpu)
                || !delayPcs.TryGetValue((uint)cpu.PC.RawValue, out var regs)
            )
            {
                return;
            }
            var masked = (cpu is Arm arm && (arm.CPSR.RawValue & 0x80) != 0);
            if (lastIrq >= 0 && !masked)
            {
                return; // an IRQ is about to be taken: let it
            }
            var start = (uint)cpu.GetRegisterUnsafe(regs.Item1).RawValue;
            var ticks = (uint)cpu.GetRegisterUnsafe(regs.Item2).RawValue;
            var elapsed = counter - start;
            if (elapsed >= ticks)
            {
                return;
            }
            // the counter is (NowNs - tsbase) / 12.5: wake when it reads `ticks - elapsed` more
            var full = TsTicks(NowNs);
            var due = tsbase + ((full + ticks - elapsed) * 25 + 1) / 2;
            var now = NowNs;
            DelayHalts++;
            var gen = ++delayGen;
            delayCpu = cpu;
            delayMasked = masked; // IRQs masked: nothing can end the loop early
            cpu.IsHalted = true;
            machine.ScheduleAction(
                TimeInterval.FromNanoseconds(due > now ? due - now : 1),
                _ => DelayWake(gen),
                "cr870-delay"
            );
        }

        // a timer counter read in a known `threshold <= counter >> 4` wait loop: sleep (halted) until it is
        private void TimerWait(int n, uint counter)
        {
            if (
                !machine.SystemBus.TryGetCurrentCPU(out var c)
                || !(c is TranslationCPU cpu)
                || !timerWaitPc.Contains((uint)cpu.PC.RawValue)
            )
            {
                return;
            }
            if ((tcfg & (1u << n)) == 0 || thd[n] == 0 || thd[n] <= counter >> 4)
            {
                return;
            }
            var masked = (cpu is Arm arm && (arm.CPSR.RawValue & 0x80) != 0);
            if (lastIrq >= 0 && !masked)
            {
                return;
            }
            // counter = c << 4 with the /16 prescaler (TimerFreerun), else c; c = (now - tbase) * Hz / 1e9
            var ticks =
                (TimerFreerun && (tcfg & (0x100u << n)) != 0) ? (ulong)thd[n] : (ulong)thd[n] << 4;
            var due = tbase[n] + ticks * 1000000000ul / TimerHz(n);
            var now = NowNs;
            DelayHalts++;
            var gen = ++delayGen;
            delayCpu = cpu;
            delayMasked = masked;
            cpu.IsHalted = true;
            machine.ScheduleAction(
                TimeInterval.FromNanoseconds(due > now ? due - now + 1 : 1),
                _ => DelayWake(gen),
                "cr870-timerwait"
            );
        }

        private void DelayWake(ulong gen)
        {
            if (gen == delayGen && delayCpu != null)
            {
                delayGen++;
                var cpu = delayCpu;
                delayCpu = null;
                cpu.IsHalted = false;
            }
        }

        private bool InIrqMode() =>
            machine.SystemBus.TryGetCurrentCPU(out var c)
            && c is Arm arm
            && (arm.CPSR.RawValue & 0x1f) == 0x12;

        private void VicUpdate()
        {
            var i = NextIrq();
            if (i >= 0 && delayCpu != null && !delayMasked)
            {
                DelayWake(delayGen); // an interrupt ends the sleep (the loop takes it, re-checks)
            }
            if (i != lastIrq && Log.Tracing)
            {
                Log.Trace(string.Format("IRQ next={0} pc={1:x8}", i, Pc));
            }
            lastIrq = i;
            if (i >= 0)
            {
                lineIrq = i;
            }
            Connections[0].Set(i >= 0);
        }

        private void Raise(int irq)
        {
            SetBit(pend, irq, true);
            VicUpdate();
        }

        private void StartTimers()
        {
            if (Av8k)
            {
                var gen = timerGen;
                av8kNext = (NowNs / 125000 + 1) * 125000;
                At(av8kNext, () => Av8kFire(gen), "cr870-av8k");
            }
        }

        private void TimerArm(int n)
        {
            var gen = ++timerSeq[n];
            if ((tcfg & (1u << n)) == 0 || thd[n] == 0)
            {
                return;
            }
            var due = tbase[n] + thd[n] * 1000000000ul / TimerHz(n);
            var g = timerGen;
            At(
                due,
                () =>
                {
                    if (gen == timerSeq[n] && g == timerGen)
                    {
                        TimerFire(n);
                    }
                },
                "cr870-timer"
            );
        }

        private void TimerFire(int n)
        {
            if (CR870_Stats.On)
            {
                CR870_Stats.Hit("fire timer" + n + " thd " + thd[n] + " cfg " + tcfg.ToString("x"));
            }
            if (n == 0 && (tcfg & 0x80000000u) != 0)
            {
                WdtExpire();
                return;
            }
            Raise(5 + n);
            if (IdleWfi)
            {
                PatchIdle();
                PatchLinuxIdle();
            }
            if ((tcfg & (0x10u << n)) != 0 && TimerFreerun)
            {
                return; // one-shot: the counter runs on past the threshold (the
            } // real BL1 polls `threshold <= counter >> 4` after it)
            tbase[n] += thd[n] * 1000000000ul / TimerHz(n);
            var now = NowNs;
            if (tbase[n] + 1000000000ul < now)
            {
                tbase[n] = now; // don't storm after a stall
            }
            if ((tcfg & (0x10u << n)) != 0)
            {
                tcfg &= ~(1u << n); // one-shot
            }
            TimerArm(n);
        }

        private uint TimerCount(int n)
        {
            if ((tcfg & (1u << n)) == 0)
            {
                return 0;
            }
            var now = PollNs;
            var c = now > tbase[n] ? (now - tbase[n]) * TimerHz(n) / 1000000000ul : 0;
            if (TimerFreerun)
            {
                return (uint)((tcfg & (0x100u << n)) != 0 ? c << 4 : c);
            }
            return thd[n] != 0 ? (uint)(c % thd[n]) : (uint)c;
        }

        private void WdtExpire()
        {
            var m0 = Get(0xB1005000);
            var m1 = Get(0xB1005004);
            var m2 = Get(0xB1005008);
            Log.Trace(string.Format("WATCHDOG reset, mailbox {0:x8} {1:x8} {2:x8}", m0, m1, m2));
            Log.Flush();
            Log.Log(string.Format("watchdog reset, mailbox {0:x8} {1:x8} {2:x8}", m0, m1, m2));
            tcfg = 0;
            ResetChip(cold: false, run: true);
        }

        private void Av8kFire(uint gen)
        {
            if (gen != timerGen)
            {
                return;
            }
            Raise(4); // ticks merge while pending (QEMU default, no backlog)
            av8kLast = (long)(av8kNext / 125000);
            av8kNext += 125000; // a fixed 8 kHz grid (an action's clock can lag its due time)
            At(av8kNext, () => Av8kFire(gen), "cr870-av8k");
        }

        private bool VicRead(ulong off, out ulong v)
        {
            v = 0;
            switch (off)
            {
                case 0x080:
                case 0x084:
                    v = vicCfg;
                    return true;
                case 0x088:
                case 0x08C:
                    v = hpMask;
                    return true;
                case 0x090:
                {
                    var i = NextIrq();
                    if (i < 0 && lineIrq >= 0 && InIrqMode())
                    {
                        // the source that raised the line dropped it again before the handler got here (a level
                        // line changed by an event between the CPU taking the IRQ and this read): its handler sees an
                        // idle device. KnOS indexes its handler table with this value; 0xdeadbeef crashed it.
                        spuriousIrqs++;
                        if (spuriousIrqs <= 20)
                        {
                            Log.Log(
                                string.Format(
                                    "VIC: spurious IRQ (irq {0} dropped before the vector read, #{1})",
                                    lineIrq,
                                    spuriousIrqs
                                )
                            );
                        }
                        i = lineIrq;
                    }
                    if (i < 0)
                    {
                        if (deadbeefs++ < 10)
                        {
                            var cpsr =
                                machine.SystemBus.TryGetCurrentCPU(out var cc) && cc is Arm aa
                                    ? (uint)aa.CPSR.RawValue
                                    : 0;
                            Log.Log(
                                string.Format(
                                    "VIC: vector read with nothing pending: cpsr {0:x8} pc {1:x8} lineIrq {2} lastIrq {3}",
                                    cpsr,
                                    Pc,
                                    lineIrq,
                                    lastIrq
                                )
                            );
                        }
                        v = 0xdeadbeef;
                        return true;
                    }
                    v = (ulong)i;
                    var acks = ++irqAcks[i];
                    if (acks >= 1000 && (acks & (acks - 1)) == 0 && acks >= 1024)
                    {
                        Log.Log(
                            string.Format(
                                "VIC: irq {0} acknowledged {1} times (t={2:F3} s)",
                                i,
                                acks,
                                PollNs / 1e9
                            )
                        );
                    }
                    SetBit(pend, i, false); // acknowledge: latch cleared, in service until SelClr
                    SetBit(insvc, i, true);
                    VicUpdate();
                    return true;
                }
                case 0x100:
                case 0x104:
                    v = hpSel[(off - 0x100) / 4];
                    return true;
                case 0x200:
                case 0x204:
                    v = tcfg;
                    return true;
                case 0x230:
                    {
                        var t = (tcfg & 0x1000) != 0 ? NowNs : 0; // exact (busy waits on it are DelayLoops)
                        v = TsTicks(t);
                    }
                    v &= 0xFFFFFFFF;
                    if (delayPcs.Count != 0 && (tcfg & 0x1000) != 0)
                    {
                        DelayLoop((uint)v);
                    }
                    return true;
                case 0xFF8:
                    v = 0x06110801;
                    return true;
                case 0x400:
                case 0x404:
                    v = gpioA;
                    return true;
                case 0x408:
                    v = GpioLevels();
                    return true;
                case 0x40C:
                    v = gpioB;
                    return true;
            }
            if (off >= 0x120 && off <= 0x12C || off >= 0x140 && off <= 0x14C)
            {
                v = pend[(off & 0xF) / 4];
                return true;
            }
            if (off >= 0x180 && off <= 0x18C || off >= 0x190 && off <= 0x19C)
            {
                v = mask[(off & 0xF) / 4];
                return true;
            }
            if (off >= 0x210 && off <= 0x218)
            {
                var n = (int)(off - 0x210) / 4;
                v = TimerCount(n);
                if (timerWaitPc.Count != 0)
                {
                    TimerWait(n, (uint)v);
                }
                return true;
            }
            if (off >= 0x220 && off <= 0x228)
            {
                v = thd[(off - 0x220) / 4];
                return true;
            }
            return false;
        }

        private bool VicWrite(ulong off, uint v)
        {
            switch (off)
            {
                case 0x080:
                    vicCfg |= v;
                    break;
                case 0x084:
                    vicCfg &= ~v;
                    break;
                case 0x088:
                    hpMask |= v & 0xff;
                    break;
                case 0x08C:
                    hpMask &= ~v;
                    break;
                case 0x094:
                case 0x098: // irqRunClr / irqSelClr
                    if (v < 128)
                    {
                        SetBit(insvc, (int)v, false);
                    }
                    break;
                case 0x100:
                case 0x104:
                    hpSel[(off - 0x100) / 4] = v;
                    break;
                case 0x200:
                case 0x204:
                {
                    var old = tcfg;
                    tcfg = off == 0x200 ? old | v : old & ~v;
                    var now = NowNs;
                    for (var n = 0; n < 3; n++)
                    {
                        if ((old & (1u << n)) == 0 && (tcfg & (1u << n)) != 0)
                        {
                            tbase[n] = now; // start: count from 0
                        }
                        TimerArm(n);
                    }
                    if ((old & 0x1000) == 0 && (tcfg & 0x1000) != 0)
                    {
                        tsbase = now;
                    }
                    return true;
                }
                case 0x230:
                    tsbase = NowNs - (ulong)v * 25 / 2;
                    return true;
                default:
                    if (off >= 0x120 && off <= 0x12C)
                    {
                        pend[(off & 0xF) / 4] |= v;
                        break;
                    }
                    if (off >= 0x140 && off <= 0x14C)
                    {
                        pend[(off & 0xF) / 4] &= ~v;
                        break;
                    }
                    if (off >= 0x180 && off <= 0x18C)
                    {
                        mask[(off & 0xF) / 4] |= v;
                        break;
                    }
                    if (off >= 0x190 && off <= 0x19C)
                    {
                        mask[(off & 0xF) / 4] &= ~v;
                        break;
                    }
                    if (off >= 0x210 && off <= 0x218)
                    {
                        var n = (int)(off - 0x210) / 4;
                        tbase[n] = NowNs - (ulong)v * 1000000000ul / TimerHz(n);
                        TimerArm(n);
                        return true;
                    }
                    if (off >= 0x220 && off <= 0x228)
                    {
                        var n = (int)(off - 0x220) / 4;
                        thd[n] = v;
                        TimerArm(n);
                        return true;
                    }
                    if (off >= 0x400 && off <= 0x40C)
                    {
                        var before = GpioLevels(false);
                        if (off == 0x400)
                        {
                            gpioA |= v;
                        }
                        else if (off == 0x404)
                        {
                            gpioA &= ~v;
                        }
                        else if (off == 0x408)
                        {
                            gpioB |= v;
                        }
                        else
                        {
                            gpioB &= ~v;
                        }
                        GpioTrace(before);
                        var l = RequestLevel;
                        if (l != lastReq)
                        {
                            lastReq = l;
                            RequestLine?.Invoke(l);
                        }
                        return true;
                    }
                    return false;
            }
            VicUpdate();
            return true;
        }

        // ---- GPIO 0xFFFF0400 and the bit-banged I2C on SCL 13 / SDA 14 ----
        // The only device on the unit's bus is the Apple auth coprocessor 2.0B at 0x10 (a scan of 0x08-0x77 from our OS,
        // 2026-10-25: KnOS's TVService probes a video encoder at 0x44, which NAKs). Registers 0-8 as read on the unit:
        // device version 3, firmware 1, protocol 2.0 (KnOS wants major 2: FUN_602558a4), device ID 0x200, error code 0.
        // The certificate and signatures aren't modelled (per-unit; reads 0). The real chip NAKs while waking (KnOS
        // retries); this one doesn't.
        // KnOS bit-bangs it open-drain: B clear, A set = released (the bus pull-ups: high), A clear = driven low.
        // The GPIO block (docs/module/memory-map.md, measured over JTAG on the unit 2026-10-25): A (+0x400 set, +0x404
        // clear, read at either) 1 = the pad is not driven, 0 = it drives B; B (+0x408 set, +0x40C clear, read at
        // +0x40C) is the driven level, or the pull-up while A is set; +0x408 reads the levels. A released pad reads
        // what is outside: on this carrier pulled low on 4 (the strap's pull-down, high while MCU PB01 drives it), 6
        // (the request line, also against B's pull-up), 12 (the PHY's reset), 15 and 17; high on the rest (the I2C
        // pull-ups on 13/14; the unconnected pads float up). Without the pad switch matrix every level reads 0.
        private const uint PulledLow = 1u << 4 | 1u << 6 | 1u << 12 | 1u << 15 | 1u << 17;

        private uint GpioLevels(bool slaves = true)
        {
            if (!PadsRouted)
            {
                return 0;
            }
            var released =
                (~PulledLow | (FactoryResetStrap ? 1u << 4 : 0))
                & ~(slaves && i2c.SdaLow ? 1u << 14 : 0);
            return ((gpioB & ~gpioA) | (released & gpioA)) & 0xfffff;
        }

        /// <summary>GPIO-12 = ETH NRESET (J2 pin 72): the Ethernet PHY sits on the product board and is held in reset
        /// until the module drives this high (A clear, B set); the Nokia kernel only releases it, so Linux found
        /// "no MII PHY" on the unit while the model answered anyway. It also needs its clock, the AV1 PLL.</summary>
        internal bool EthPhyReleased =>
            ((GpioLevels() >> 12) & 1) != 0 && (Get(0xE0007240) & 0x7fffffffu) != 0;

        private void GpioTrace(uint before)
        {
            var now = GpioLevels(false); // the master's side: the I2C slave's own drive is not an edge
            if (((before ^ now) & 0x6000) != 0)
            {
                I2cEdge(
                    (before >> 13 & 1) != 0,
                    (before >> 14 & 1) != 0,
                    (now >> 13 & 1) != 0,
                    (now >> 14 & 1) != 0
                );
                if (Log.Tracing)
                {
                    Log.Trace(
                        string.Format(
                            "GPIO 13={0} 14={1}",
                            (now >> 13) & 1,
                            (GpioLevels() >> 14) & 1
                        )
                    );
                }
            }
        }

        private sealed class BitI2c
        {
            public bool Active,
                Addressed,
                Rd,
                AckPhase,
                MasterAck,
                SdaLow;
            public byte Sh,
                Bits,
                Ptr;
            public byte[] Regs;
            public readonly Dictionary<int, byte[]> Devices = new Dictionary<int, byte[]>
            {
                { 0x10, AuthChip() },
            };

            private static byte[] AuthChip()
            {
                var r = new byte[256];
                r[0x00] = 0x03; // device version (2.0B) [HW]
                r[0x01] = 0x01; // firmware version [HW]
                r[0x02] = 0x02; // protocol 2.0 [HW]
                r[0x04 + 2] = 0x02; // device ID (4 bytes, big-endian) 0x00000200 [HW]
                return r; // then the error code (0); the rest unknown, reads 0
            }

            public int NBytes;
            public StringBuilder Line = new StringBuilder();
        }

        private void I2cFlush()
        {
            if (i2c.Line.Length > 0)
            {
                Log.Trace("I2C " + i2c.Line);
            }
            i2c.Line.Clear();
        }

        private void I2cEdge(bool scl0, bool sda0, bool scl1, bool sda1)
        {
            if (scl0 && scl1 && sda0 && !sda1) // START / repeated START
            {
                if (i2c.Active)
                {
                    i2c.Line.Append(" Sr");
                }
                else
                {
                    I2cFlush();
                }
                i2c.Active = true;
                i2c.Addressed = false;
                i2c.Bits = 0;
                i2c.Sh = 0;
                i2c.AckPhase = false;
                i2c.MasterAck = false;
                i2c.SdaLow = false;
                i2c.NBytes = -1;
                return;
            }
            if (scl0 && scl1 && !sda0 && sda1) // STOP
            {
                if (i2c.Active)
                {
                    i2c.Line.Append(" P");
                    I2cFlush();
                }
                i2c.Active = false;
                i2c.SdaLow = false;
                return;
            }
            if (!i2c.Active)
            {
                return;
            }
            if (!scl0 && scl1) // SCL rising: sample
            {
                if (i2c.AckPhase)
                {
                    if (i2c.MasterAck && sda1) // the master NAKs the byte we sent: the read ends
                    {
                        i2c.Active = false;
                        i2c.Line.Append(" N");
                    }
                    return;
                }
                if (!i2c.Rd || i2c.NBytes < 0)
                {
                    i2c.Sh = (byte)(i2c.Sh << 1 | (sda1 ? 1 : 0));
                }
                i2c.Bits++;
                return;
            }
            if (scl0 && !scl1) // SCL falling: slave drives SDA
            {
                if (i2c.AckPhase)
                {
                    i2c.AckPhase = false;
                    i2c.MasterAck = false;
                    i2c.SdaLow = false;
                    i2c.Bits = 0;
                    i2c.Sh = 0;
                    if (!i2c.Addressed)
                    {
                        i2c.Active = false;
                        return;
                    }
                    if (i2c.Rd)
                    {
                        i2c.Sh = i2c.Regs[i2c.Ptr++];
                        i2c.Line.AppendFormat(" <{0:x2}", i2c.Sh);
                        i2c.SdaLow = (i2c.Sh & 0x80) == 0;
                    }
                    return;
                }
                if (i2c.Bits == 8)
                {
                    if (i2c.NBytes < 0)
                    {
                        i2c.Addressed = i2c.Devices.TryGetValue(i2c.Sh >> 1, out i2c.Regs);
                        i2c.Rd = (i2c.Sh & 1) != 0;
                        i2c.Line.AppendFormat("{0:x2}{1}", i2c.Sh, i2c.Addressed ? "A" : "N");
                        i2c.NBytes = 0;
                        if (!i2c.Addressed)
                        {
                            i2c.Active = false;
                            return;
                        }
                        i2c.SdaLow = true;
                        i2c.AckPhase = true;
                    }
                    else if (!i2c.Rd)
                    {
                        if (i2c.NBytes == 0)
                        {
                            i2c.Ptr = i2c.Sh;
                        }
                        else
                        {
                            i2c.Regs[i2c.Ptr++] = i2c.Sh;
                        }
                        i2c.Line.AppendFormat(" {0:x2}", i2c.Sh);
                        i2c.NBytes++;
                        i2c.SdaLow = true;
                        i2c.AckPhase = true;
                    }
                    else
                    {
                        i2c.NBytes++;
                        i2c.SdaLow = false;
                        i2c.AckPhase = true;
                        i2c.MasterAck = true;
                    }
                    return;
                }
                if (i2c.Rd && i2c.NBytes >= 0)
                {
                    i2c.SdaLow = ((i2c.Sh << i2c.Bits) & 0x80) == 0;
                }
            }
        }

        // ---- AV timing ----
        private uint AvCtr()
        {
            var t = PollNs * 3072 / 125000; // 24.576 MHz ticks
            var off = t % 3072;
            var cyc = (t / 3072) % 8000;
            var sec = (t / 3072 / 8000) % 128;
            return (uint)(sec << 25 | cyc << 12 | off);
        }

        private uint PllIirStat(int n)
        {
            var r140 = Get(0xE0007140u + (uint)n * 0x80);
            var r148 = Get(0xE0007148u + (uint)n * 0x80);
            if (n != 0 || (r140 & 0x80000000u) != 0)
            {
                return r140 & 0x3fffff;
            }
            var div = (r148 & 0x3fff) != 0 ? (r148 & 0x3fff) : 0x4000u;
            var x2 = (r148 >> 16) & 3;
            var t = 480e6 / (Pll0Hz * div) - 3 - x2;
            if (t < 0)
            {
                t = 0;
            }
            var q = (uint)(t * 2048 + 0.5);
            var i = q >> 11;
            var x3 = 0u;
            if (i > 15)
            {
                x3 = i - 15;
                i = 15;
            }
            if (x3 > 63)
            {
                x3 = 63;
            }
            return x3 << 15 | i << 11 | (q & 0x7ff);
        }

        // ---- SuluDMA packet moves + the AV port 2 sink ----
        private void SdmaMove(int client, uint v)
        {
            var ctx = v & 0x3f;
            var c = 0xB5004000u + ctx * 0x40;
            var src = Get(c + 0x0C);
            var dst = Get(c + 0x08);
            var len = (int)Math.Min(Get(c + 0x28) & 0x1fff, 0x2000);
            var pkt = machine.SystemBus.ReadBytes(src, len);
            sdmaMoves++;
            var nz = false;
            for (var i = 8; i < len; i++)
            {
                nz |= pkt[i] != 0;
            }
            if (nz)
            {
                sdmaLoud++;
            }
            if (Log.Tracing && (sdmaMoves <= 10 || sdmaMoves % 5000 == 0 || (nz && sdmaLoud <= 30)))
            {
                var sb = new StringBuilder();
                for (var i = 0; i < len && i < 96; i++)
                {
                    sb.Append(i % 4 != 0 ? "" : " ").AppendFormat("{0:x2}", pkt[i]);
                }
                Log.Trace(
                    string.Format(
                        "DMA move #{0} client {1} ctx {2} {3:x8} -> {4:x8} len {5}:{6}",
                        sdmaMoves,
                        client,
                        ctx,
                        src,
                        dst,
                        len,
                        sb
                    )
                );
            }
            if (ctx == 58 && dst == 0xB4000000 && len >= 8)
            {
                AvOutPacket(pkt, len);
            }
        }

        private void AvOutPacket(byte[] p, int len)
        {
            if (wav == null && !string.IsNullOrEmpty(AudioWav))
            {
                wav = new FileStream(AudioWav, FileMode.Create);
                WavHeader();
            }
            var w0 = BitConverter.ToUInt32(p, 0);
            var ctrl = Get(0xE000A000);
            var act = Get(0xE0010000);
            var cnt = (int)(w0 & 0xffff);
            var on = (act & 4) != 0 && (ctrl & 0x100) == 0;
            if (cnt < 4 || cnt + 4 > len)
            {
                return;
            }
            var n = (cnt - 4) / 4;
            // the port's memory data format, AV Kind (+0x08) [3:0] (KnOS's `sys av port` decode): 2 = AM824_24 (KnOS:
            // label byte in [31:24], the sample in [23:0]), 9 = FP32 (Linux's av-device: a 32-bit fixed-point word,
            // the sample in [31:8]); AM824_20/16 and the packed formats aren't used by either
            var am824 = (Get(0xE000A008) & 0xF) != 9;
            for (var i = 0; i + 1 < n; i += 2)
            {
                var xs = new int[2];
                var o = new byte[6];
                for (var ch = 0; ch < 2; ch++)
                {
                    var word = BitConverter.ToUInt32(p, 8 + (i + ch) * 4);
                    var raw = am824 ? (int)(word << 8) >> 8 : (int)word >> 8;
                    var x = on ? raw : 0;
                    if (raw != 0)
                    {
                        avRawNz++;
                    }
                    if (x != 0)
                    {
                        avNz++;
                    }
                    avPeak[ch] = Math.Max(avPeak[ch], Math.Abs(x));
                    o[ch * 3] = (byte)x;
                    o[ch * 3 + 1] = (byte)(x >> 8);
                    o[ch * 3 + 2] = (byte)(x >> 16);
                    xs[ch] = x;
                }
                if (!on)
                {
                    avGated++;
                }
                wav?.Write(o, 0, 6);
                I2SFrame?.Invoke(xs[0], xs[1]);
                avFrames++;
            }
            if (avFrames - avSfr >= 44100)
            {
                Log.Trace(
                    string.Format(
                        "AUDIO {0} frames: peak L {1} R {2} (of 8388607), nonzero {3} (before gating {4}), gated {5}, avActivate {6:x} port2 ctrl {7:x8}",
                        avFrames,
                        avPeak[0],
                        avPeak[1],
                        avNz,
                        avRawNz,
                        avGated,
                        act,
                        ctrl
                    )
                );
                if (wav != null)
                {
                    WavHeader();
                    wav.Flush();
                }
                avSfr = avFrames;
                avPeak[0] = avPeak[1] = 0;
                avNz = avRawNz = avGated = 0;
            }
        }

        private void WavHeader()
        {
            var data = (uint)(avFrames * 6);
            var pos = wav.Position;
            wav.Seek(0, SeekOrigin.Begin);
            foreach (
                var x in new uint[]
                {
                    0x46464952,
                    36 + data,
                    0x45564157,
                    0x20746d66,
                    16,
                    1 | 2 << 16,
                    44100,
                    44100 * 6,
                    6 | 24 << 16,
                    0x61746164,
                    data,
                }
            )
            {
                wav.Write(BitConverter.GetBytes(x), 0, 4);
            }
            wav.Seek(Math.Max(pos, 44), SeekOrigin.Begin);
        }

        // ---- SuluDMA packet streams (Linux av-device): start at +0x6000 + ctx * 0x40, chained contexts ----
        // The Nokia kernel's newdma_packet_start writes ctx | ctx << 16 there (and the bitmaps at +0x2810/+0x2830);
        // the engine then moves one packet per context along the Flags0[7:0] chain (ping-pong 33 <-> 34 for audio).
        // Flags0 bit 7 = the context is empty: newdma_packet_queue(_fast) fills a context only while bit 7 is set
        // ("queue is full" otherwise); the slow path writes the context and clears it, the fast path writes the fast
        // window +0x9000 / +0x9200 + ctx * 4 (below). The engine sets bit 7 again when the packet is done, and waits at an empty
        // context (an underrun: nothing is moved) until the driver queues the next one. Without bit 7 the driver
        // logged "queue is full" every period (an emulator artifact, 2026-10-21).
        // A packet to the AV FIFO (0xB4000000) takes its frames / 44.1 kHz; then the context number goes into the
        // tag FIFO of its IRQ group (Flags0[31:30]; read +0x1C40 + 4 * group, write = acknowledge: sulu_newdma_isr)
        // and IRQ 10 + group is raised while the group is enabled (+0x1C08 + 0x10 * group). The stream stops when
        // nothing on the AV block is active any more (av-device stops a stream by turning the port off).
        private void IsoStart(int ctx)
        {
            isoGen++;
            isoStartNs = NowNs + 1000;
            isoSamples = 0;
            if (Log.Tracing)
            {
                Log.Trace(string.Format("ISO start ctx {0}", ctx));
            }
            var gen = isoGen;
            var tg = timerGen;
            At(isoStartNs, () => IsoPacket(ctx, gen, tg), "cr870-iso");
        }

        private void IsoPacket(int ctx, ulong gen, uint tg)
        {
            if (gen != isoGen || tg != timerGen)
            {
                return;
            }
            if (Get(0xE0010000) == 0)
            {
                if (Log.Tracing)
                {
                    Log.Trace(
                        string.Format("ISO stop (AV inactive) after {0} packets", isoPackets)
                    );
                }
                return;
            }
            var c = 0xB5004000u + (uint)ctx * 0x40;
            var flags0 = Get(c + 0x24);
            if ((flags0 & 0x80) != 0) // empty: nothing queued yet, look again a packet later
            {
                isoUnderruns++;
                isoSamples += 256; // the time passes on the grid as silence would
                At(
                    isoStartNs + isoSamples * 1000000000ul / 44100,
                    () => IsoPacket(ctx, gen, tg),
                    "cr870-iso"
                );
                return;
            }
            var src = BmoveAddr(Get(c + 0x0C));
            var dst = Get(c + 0x08);
            var w0 = machine.SystemBus.ReadDoubleWord(src);
            var cnt = (int)(w0 & 0xffff);
            var frames = 8;
            if (cnt >= 12 && cnt + 4 <= 0x2000)
            {
                var pkt = machine.SystemBus.ReadBytes(src, cnt + 4);
                if (dst == 0xB4000000)
                {
                    AvOutPacket(pkt, cnt + 4);
                }
                frames = Math.Max(1, (cnt - 4) / 8);
            }
            isoPackets++;
            if (Log.Tracing && (isoPackets <= 6 || isoPackets % 2000 == 0))
            {
                Log.Trace(
                    string.Format(
                        "ISO packet #{0} ctx {1} {2:x8} -> {3:x8} count {4:x} ({5} frames) flags0 {6:x8}, underruns {7}",
                        isoPackets,
                        ctx,
                        src,
                        dst,
                        cnt,
                        frames,
                        flags0,
                        isoUnderruns
                    )
                );
            }
            var next = (int)(flags0 & 0x3f);
            var g = (int)(flags0 >> 30);
            isoSamples += (ulong)frames; // a fixed 44.1 kHz grid from the stream's start
            isoNext = isoStartNs + isoSamples * 1000000000ul / 44100;
            At(
                isoNext,
                () =>
                {
                    if (gen != isoGen || tg != timerGen)
                    {
                        return;
                    }
                    Set(c + 0x24, Get(c + 0x24) | 0x80); // done: the context is free for the driver again
                    if (g >= 2 && isoTt[g] - isoTh[g] < 15)
                    {
                        isoTag[g, isoTt[g]++ & 15] = (byte)ctx;
                        IsoIrq(g);
                    }
                    IsoPacket(next, gen, tg);
                },
                "cr870-iso"
            );
        }

        private void IsoIrq(int g)
        {
            SetBit(
                level,
                10 + g,
                isoTh[g] != isoTt[g] && (Get(0xB5001C08u + (uint)g * 0x10) & 1) != 0
            );
            VicUpdate();
        }

        // ---- SuluDMA block moves (completion 2 us later, into the tag FIFO of the context's IRQ group: sulu_eth's
        // contexts are group 1 (+0x1C44, IRQ 11), av-device's period copies group 2 (+0x1C48, IRQ 12); one FIFO for all
        // fed the audio's completions to sulu_eth's handler, which wedged its TX queue: "tx DMA queue full" panic) ----
        private void BmoveIrq()
        {
            SetBit(level, 11, bmoveTh != bmoveTt && (Get(0xB5001C18) & 1) != 0);
            VicUpdate();
        }

        private static ulong BmoveAddr(uint a) => a < 0x10000 ? 0x00400000u + a : a;

        private void BmoveStart(uint bits)
        {
            for (var n = 0; n < 32; n++)
            {
                if ((bits & (1u << n)) == 0)
                {
                    continue;
                }
                var ctx = Get(0xB5000080u + (uint)n * 4) & 0x3f;
                var hops = 0;
                while (true)
                {
                    var c = 0xB5004000u + ctx * 0x40;
                    var src = BmoveAddr(Get(c + 0x0C));
                    var dst = BmoveAddr(Get(c + 0x08));
                    var len = Get(c + 0x10);
                    var next = Get(c + 0x24) & 0x3f;
                    // the length unit: newdma_blockmove_queue shifts the byte count by the client's width, which the
                    // context doesn't show plainly. Observed (Flags1 -> unit): sulu_eth TX 0x01481000 -> bytes (DHCP
                    // frames of 344; quadlets overrun its SRAM rings: "tx DMA queue full" panic), sulu_eth RX
                    // 0x01501000 (bit 20) -> quadlets (linux/tcp md5), av-device 0x00480000 -> quadlets (os/audio
                    // bit-exact). So: quadlets unless bit 12 is set without bit 20.
                    var f1 = Get(c + 0x28);
                    if ((f1 & 0x00101000) != 0x00001000)
                    {
                        len *= 4;
                    }
                    len = Math.Min(len, 0x10000u);
                    var buf = machine.SystemBus.ReadBytes(src, (int)len);
                    machine.SystemBus.WriteBytes(buf, dst);
                    if (Log.Tracing && ++bmoveMoves <= 4000)
                    {
                        Log.Trace(
                            string.Format(
                                "BMOVE #{0} client {1} ctx {2} {3:x8} -> {4:x8} len {5} flags {6:x8} {7:x8}",
                                bmoveMoves,
                                n,
                                ctx,
                                src,
                                dst,
                                len,
                                Get(c + 0x24),
                                Get(c + 0x28)
                            )
                        );
                    }
                    // every context of a chain completes with its own tag (sulu_eth moves a frame that wraps its SRAM
                    // ring as ctx 0 -> ctx 1 and does its RX bookkeeping on ctx 0's tag: one tag per chain leaked an
                    // RX buffer per wrapped frame until RX stopped and TX filled up: "tx DMA queue full" panic)
                    if (bmovePt - bmovePh < BMQ)
                    {
                        bmovePend[bmovePt & (BMQ - 1)] = (byte)ctx;
                        bmovePClient[bmovePt++ & (BMQ - 1)] = n;
                    }
                    else
                    {
                        Log.Log("SuluDMA: block move dropped, pending queue full");
                    }
                    if (next == ctx || ++hops >= 4)
                    {
                        break;
                    }
                    ctx = next;
                }
                bmoveBusy |= 1u << n;
            }
            if (bmovePh != bmovePt && !bmoveScheduled)
            {
                bmoveScheduled = true;
                var gen = timerGen;
                At(NowNs + 2000, () => BmoveDone(gen), "cr870-bmove");
            }
        }

        private void BmoveDone(uint gen)
        {
            bmoveScheduled = false;
            if (gen != timerGen || bmovePh == bmovePt)
            {
                return;
            }
            var i = bmovePh++ & (BMQ - 1);
            bmoveBusy &= ~(1u << bmovePClient[i]);
            var ctx = bmovePend[i];
            var g = (int)(Get(0xB5004000u + ctx * 0x40u + 0x24) >> 30); // the context's IRQ group (Flags0[31:30])
            if (g >= 2) // newdma clients (av-device: group 2): their own FIFO + IRQ
            {
                if (isoTt[g] - isoTh[g] < 15)
                {
                    isoTag[g, isoTt[g]++ & 15] = ctx;
                }
                IsoIrq(g);
            }
            else if (bmoveTt - bmoveTh < BMQ)
            {
                bmoveTag[bmoveTt++ & (BMQ - 1)] = ctx;
            }
            else
            {
                Log.Log("SuluDMA: completion tag dropped, tag FIFO full");
            }
            BmoveIrq();
            if (bmovePh != bmovePt)
            {
                bmoveScheduled = true;
                At(NowNs + 2000, () => BmoveDone(gen), "cr870-bmove");
            }
        }

        private readonly IMachine machine;
        private readonly CR870_NAND nand;
        private readonly Dictionary<ulong, uint> sticky = new Dictionary<ulong, uint>();
        private bool powered,
            patched,
            lastReq,
            vectorsChecked;
        private int lineIrq = -1;
        private ulong spuriousIrqs,
            deadbeefs;
        private int crashes;
        private readonly ulong[] irqAcks = new ulong[128];
        private uint timerGen;
        private readonly uint[] timerSeq = new uint[3];

        private readonly uint[] pend = new uint[4],
            insvc = new uint[4],
            mask = new uint[4],
            level = new uint[4];
        private readonly uint[] hpSel = new uint[2];
        private uint vicCfg,
            hpMask,
            tcfg;
        private readonly uint[] thd = new uint[3];
        private readonly ulong[] tbase = new ulong[3];
        private ulong tsbase;
        private int lastIrq = -2;
        private long av8kLast;

        private uint gpioA,
            gpioB;
        private readonly BitI2c i2c = new BitI2c();

        private ulong sdmaMoves,
            sdmaLoud;
        private FileStream wav;
        private ulong avFrames,
            avSfr,
            avNz,
            avRawNz,
            avGated;
        private readonly int[] avPeak = new int[2];

        private const int BMQ = 64; // block-move pending queue and tag FIFO depth
        private readonly byte[] bmoveTag = new byte[BMQ],
            bmovePend = new byte[BMQ];
        private readonly int[] bmovePClient = new int[BMQ];
        private uint bmoveTh,
            bmoveTt,
            bmovePh,
            bmovePt,
            bmoveBusy;
        private ulong bmoveMoves;
        private readonly byte[,] isoTag = new byte[4, 16];
        private readonly uint[] isoTh = new uint[4],
            isoTt = new uint[4];
        private ulong isoGen,
            isoPackets,
            isoStartNs,
            isoSamples,
            isoNext,
            isoUnderruns;
        private bool bmoveScheduled;
    }

    /// <summary>A window of the catch-all I/O space at BaseAddress (see CR870_Core).</summary>
    public class CR870_Window : IDoubleWordPeripheral, IWordPeripheral, IBytePeripheral, IKnownSize
    {
        public CR870_Window(CR870_Core core, ulong baseAddress, long size)
        {
            this.core = core;
            this.baseAddress = baseAddress;
            this.size = size;
        }

        public long Size => size;

        public void Reset() { }

        public uint ReadDoubleWord(long offset) => (uint)core.Read(baseAddress + (ulong)offset, 4);

        public void WriteDoubleWord(long offset, uint value) =>
            core.Write(baseAddress + (ulong)offset, value, 4);

        public ushort ReadWord(long offset) => (ushort)core.Read(baseAddress + (ulong)offset, 2);

        public void WriteWord(long offset, ushort value) =>
            core.Write(baseAddress + (ulong)offset, value, 2);

        public byte ReadByte(long offset) => (byte)core.Read(baseAddress + (ulong)offset, 1);

        public void WriteByte(long offset, byte value) =>
            core.Write(baseAddress + (ulong)offset, value, 1);

        private readonly CR870_Core core;
        private readonly ulong baseAddress;
        private readonly long size;
    }

    /// <summary>
    /// UART0/1 (0xB1000000/0xB1001000): TX data +0xE80, RX data +0xE00, RX fill +0xF18, TX free +0xF1C, status +0xF44
    /// (0x100 RX, 0x400 TX space; TX never full), interrupt mask +0xF50 set / +0xF54 clr. IRQ 0 = level line (21 / 22).
    /// Other registers read back what was written.
    /// </summary>
    public static class CR870_Stats
    {
        public static readonly Dictionary<string, ulong> Counts = new Dictionary<string, ulong>();
        public static bool On;

        public static void Hit(string k)
        {
            if (On)
            {
                lock (Counts)
                {
                    Counts[k] = Counts.TryGetValue(k, out var v) ? v + 1 : 1;
                }
            }
        }
    }

    public class CR870_UART
        : IDoubleWordPeripheral,
            IWordPeripheral,
            IBytePeripheral,
            IKnownSize,
            IUART,
            INumberedGPIOOutput
    {
        public CR870_UART(IMachine machine, CR870_Core core, int index)
        {
            this.machine = machine;
            this.core = core;
            this.index = index;
            Connections = new Dictionary<int, IGPIO> { { 0, new GPIO() } };
            Reset();
        }

        public IReadOnlyDictionary<int, IGPIO> Connections { get; }

        public long Size => 0x1000;
        public uint BaudRate => 115200;
        public Bits StopBits => Bits.One;
        public Parity ParityBit => Parity.None;
        public event Action<byte> CharReceived;

        public void Reset()
        {
            rx.Clear();
            imask = 0;
            regs.Clear();
            regs[0xFF8] = 0x06110801;
            Update();
        }

        /// <summary>A received byte. The line has no rate here (a socket delivers at once): the queue holds 4 KB, more
        /// than zd1-update's window (the kernel's tty buffer, the real limit on the unit, is the guest's own); a
        /// sender that ignores flow control still loses bytes past it.</summary>
        public void WriteChar(byte value)
        {
            if (rx.Count < RxQueue)
            {
                rx.Enqueue(value);
            }
            Update();
        }

        private const int RxQueue = 4096;

        /// <summary>Type text into the RX side, one character every GapUs of virtual time (the KnOS FIFO is tiny).</summary>
        public void Type(string text, uint gapUs = 2000)
        {
            text = System.Text.RegularExpressions.Regex.Replace(
                text.Replace("\\r", "\r").Replace("\\n", "\n"),
                @"\\x([0-9A-Fa-f]{2})",
                m => ((char)Convert.ToInt32(m.Groups[1].Value, 16)).ToString()
            ); // "\x02": raw bytes (BL1's serial keys)
            var bytes = Encoding.ASCII.GetBytes(text);
            // schedule from inside the time domain (the monitor thread must not add clock entries mid-emulation)
            machine.LocalTimeSource.ExecuteInNearestSyncedState(_ =>
            {
                var t = 0ul;
                foreach (var b in bytes)
                {
                    var c = b;
                    machine.ScheduleAction(
                        TimeInterval.FromMicroseconds(t),
                        __ => WriteChar(c),
                        "uart-type"
                    );
                    t += gapUs;
                }
            });
        }

        public uint ReadDoubleWord(long offset) => (uint)Read(offset);

        public ushort ReadWord(long offset) => (ushort)Read(offset);

        public byte ReadByte(long offset) => (byte)Read(offset);

        public void WriteDoubleWord(long offset, uint value) => Write(offset, value);

        public void WriteWord(long offset, ushort value) => Write(offset, value);

        public void WriteByte(long offset, byte value) => Write(offset, value);

        private uint Status => (rx.Count > 0 ? 0x100u : 0) | 0x400u;

        private void Update()
        {
            Connections[0].Set((Status & imask & 0x700) != 0);
        }

        private uint Read(long off)
        {
            if (CR870_Stats.On)
            {
                CR870_Stats.Hit("uart" + index + " r " + off.ToString("x"));
            }
            var v = ReadReg(off);
            if (core.Log.Tracing && off != 0xF18 && off != 0xF44)
            {
                core.Log.Trace(
                    string.Format("uart{0} R {1:x3} = {2:x8} pc={3:x8}", index, off, v, core.Pc)
                );
            }
            return v;
        }

        private uint ReadReg(long off)
        {
            switch (off)
            {
                case 0xE00:
                {
                    var v = rx.Count > 0 ? rx.Dequeue() : (byte)0;
                    Update();
                    return v;
                }
                case 0xF18:
                    return (uint)rx.Count;
                case 0xF1C:
                    return 64;
                case 0xF44:
                    return Status;
                case 0xF50:
                case 0xF54:
                    return imask;
                default:
                    return regs.TryGetValue(off, out var v2) ? v2 : 0;
            }
        }

        private void Write(long off, uint v)
        {
            if (CR870_Stats.On)
            {
                CR870_Stats.Hit("uart" + index + " w " + off.ToString("x"));
            }
            if (core.Log.Tracing && off != 0xE80)
            {
                core.Log.Trace(
                    string.Format("uart{0} W {1:x3} <- {2:x8} pc={3:x8}", index, off, v, core.Pc)
                );
            }
            switch (off)
            {
                case 0xE80:
                    CharReceived?.Invoke((byte)v);
                    ConsoleLine((byte)v);
                    return;
                case 0xF50:
                    imask |= v;
                    Update();
                    return;
                case 0xF54:
                    imask &= ~v;
                    Update();
                    return;
                default:
                    regs[off] = v;
                    return;
            }
        }

        private void ConsoleLine(byte c)
        {
            if (!core.Log.Tracing)
            {
                return;
            }
            if (c == '\n' || line.Length >= 255)
            {
                core.Log.Trace(string.Format("CON{0} {1}", index, line));
                line.Clear();
            }
            else if (c >= 0x20 && c < 0x7f)
            {
                line.Append((char)c);
            }
        }

        private readonly IMachine machine;
        private readonly CR870_Core core;
        private readonly int index;
        private readonly Queue<byte> rx = new Queue<byte>();
        private readonly Dictionary<long, uint> regs = new Dictionary<long, uint>();
        private readonly StringBuilder line = new StringBuilder();
        private uint imask;
    }

    /// <summary>
    /// NAND controller (0xB1003000) + the HY27U1G8F2B array (128 MiB, 2 KiB pages + 64 B spare, 128 KiB blocks).
    /// Chip select +0x000, command +0x200/+0x204, address cycles +0x210..+0x220, read N -> RX FIFO +0x250/+0x254,
    /// RX FIFO +0xE00 (fill +0xF18), TX data +0xE80 (free +0xF1C), status 0x70; Linux sulu_nand: +0x020, +0x088.
    /// Image: Load (logical, no spare area); spare areas read 0xFF (no bad blocks; ECC isn't modelled). Save writes it back.
    /// </summary>
    /// <summary>
    /// The NAND controller and the HY27U1G8F2B (2 KB pages + 64 B spare). The data is a 128 MB logical image (no bad
    /// blocks); the spare areas are a sidecar FILE.oob (65536 x 64 B). An image without one gets the spare areas BL2 leaves
    /// after a single write (docs/module/boot-chain.md, NAND [HW]): for every page that holds data, byte 0x0C = 00,
    /// byte 0x0F = 3F (one write) and the page's four 3-byte ECC codes at 0x10 (tools/zeppelin/nand/ecc.py); blank
    /// pages all FF. Programs commit the spare bytes the software writes, so in-place rewrites (the PersParam page)
    /// age like on the unit. The kernel's soft-ECC bytes (spare 40-63) are FF, as on the unit outside BL1/BL2, so normal
    /// MTD reads are miscorrected the same way, except in BL1's and BL2's blocks, which carry 256-byte SmartMedia
    /// codes there (SmEcc), as on the unit.
    /// </summary>
    public class CR870_NAND : IDoubleWordPeripheral, IWordPeripheral, IBytePeripheral, IKnownSize
    {
        public CR870_NAND()
        {
            data = new byte[128 << 20];
            spare = new byte[(data.Length / 2048) * 64];
            for (var i = 0; i < data.Length; i++)
            {
                data[i] = 0xFF;
            }
            for (var i = 0; i < spare.Length; i++)
            {
                spare[i] = 0xFF;
            }
        }

        public long Size => 0x1000;

        public byte[] Data => data;

        public void Load(string file)
        {
            Fill(file, data);
            if (File.Exists(file + ".oob"))
            {
                Fill(file + ".oob", spare);
                return;
            }
            for (var pg = 0; pg < data.Length / 2048; pg++)
            {
                var o = pg * 2048;
                var blank = true;
                for (var i = 0; i < 2048 && blank; i++)
                {
                    blank = data[o + i] == 0xFF;
                }
                var sp = pg * 64;
                for (var i = 0; i < 64; i++)
                {
                    spare[sp + i] = 0xFF;
                }
                if (blank)
                {
                    continue;
                }
                spare[sp + 0x0C] = 0x00;
                spare[sp + 0x0F] = 0x3F;
                for (var sector = 0; sector < 4; sector++)
                {
                    var code = Ecc(data, o + 512 * sector);
                    for (var i = 0; i < 3; i++)
                    {
                        spare[sp + 0x10 + 3 * sector + i] = (byte)(code >> (8 * i));
                    }
                }
                var block = pg / 64;
                if (block == 0 || (block >= 5 && block < 16)) // BL1 and BL2: the factory-programmed blocks
                {
                    for (var half = 0; half < 8; half++)
                    {
                        var code = SmEcc(data, o + 256 * half);
                        for (var i = 0; i < 3; i++)
                        {
                            spare[sp + 40 + 3 * half + i] = (byte)(code >> (8 * i));
                        }
                    }
                }
            }
        }

        public void Save(string file)
        {
            File.WriteAllBytes(file, data);
            File.WriteAllBytes(file + ".oob", spare);
        }

        private static void Fill(string file, byte[] buf)
        {
            using (var f = File.OpenRead(file))
            {
                var n = 0;
                while (n < buf.Length)
                {
                    var k = f.Read(buf, n, buf.Length - n);
                    if (k <= 0)
                    {
                        break;
                    }
                    n += k;
                }
            }
        }

        /// <summary>
        /// The DM870's ECC of a 512-byte sector (tools/zeppelin/nand/ecc.py): 12 (P, P') pairs, P in the odd bit, in the
        /// order byte-address bits 4-7, 0-3, 8, then bit-in-byte 0-2; P_k = parity of the bits whose address has bit k
        /// set, P' the rest. Not inverted: a blank sector's code is 0.
        /// </summary>
        public static uint Ecc(byte[] d, int off)
        {
            var x = 0; // XOR of the bytes: the column parities
            var a = 0; // XOR of the addresses of odd-parity bytes: the row parities
            for (var i = 0; i < 512; i++)
            {
                var b = d[off + i];
                x ^= b;
                var odd = b ^ (b >> 4);
                odd ^= odd >> 2;
                odd ^= odd >> 1;
                if ((odd & 1) != 0)
                {
                    a ^= i;
                }
            }
            var t = Parity(x);
            var v = 0u;
            var order = new[] { 4, 5, 6, 7, 0, 1, 2, 3, 8, 9, 10, 11 };
            var masks = new[] { 0xAA, 0xCC, 0xF0 };
            for (var i = 0; i < 12; i++)
            {
                var k = order[i];
                var p = k < 9 ? (a >> k) & 1 : Parity(x & masks[k - 9]);
                var pp = p ^ t;
                v |= (uint)(p << 1 | pp) << (2 * (i % 4) + 8 * (i / 4));
            }
            return v;
        }

        /// <summary>
        /// The SmartMedia ECC of a 256-byte half (Linux's nand_calculate_ecc, its first two bytes swapped): what the unit's
        /// BL1 and BL2 pages carry in spare bytes 40-63, all 3,208 halves [HW, `units/` raw dump 2026-10-23]. Who checks
        /// them is not known (the boot ROM?); the kernel's soft ECC reads them in Linux's order.
        /// </summary>
        public static uint SmEcc(byte[] d, int off)
        {
            int reg1 = 0,
                reg2 = 0,
                reg3 = 0;
            for (var i = 0; i < 256; i++)
            {
                int b = d[off + i];
                var cp =
                    Parity(b & 0x55)
                    | Parity(b & 0xAA) << 1
                    | Parity(b & 0x33) << 2
                    | Parity(b & 0xCC) << 3
                    | Parity(b & 0x0F) << 4
                    | Parity(b & 0xF0) << 5;
                reg1 ^= cp;
                if (Parity(b) != 0)
                {
                    reg3 ^= i;
                    reg2 ^= ~i & 0xFF;
                }
            }
            int t1 = 0,
                t2 = 0;
            for (var k = 0; k < 4; k++)
            {
                t1 |=
                    ((reg3 >> (7 - k)) & 1) << (7 - 2 * k) | ((reg2 >> (7 - k)) & 1) << (6 - 2 * k);
                t2 |=
                    ((reg3 >> (3 - k)) & 1) << (7 - 2 * k) | ((reg2 >> (3 - k)) & 1) << (6 - 2 * k);
            }
            return (uint)(~t2 & 0xFF)
                | (uint)(~t1 & 0xFF) << 8
                | (uint)((~reg1 << 2 | 3) & 0xFF) << 16;
        }

        private static int Parity(int v)
        {
            v ^= v >> 4;
            v ^= v >> 2;
            v ^= v >> 1;
            return v & 1;
        }

        public void Reset()
        {
            src = Src.Idle;
            flen = fpos = 0;
            regs.Clear();
            regs[0xFF8] = 0x06110801;
        }

        public uint ReadDoubleWord(long offset) => Read(offset, 4);

        public ushort ReadWord(long offset) => (ushort)Read(offset, 2);

        public byte ReadByte(long offset) => (byte)Read(offset, 1);

        public void WriteDoubleWord(long offset, uint value) => Write(offset, value, 4);

        public void WriteWord(long offset, ushort value) => Write(offset, value, 2);

        public void WriteByte(long offset, byte value) => Write(offset, value, 1);

        private enum Src
        {
            Idle,
            Id,
            Page,
            Status,
        }

        private uint Read(long off, int size)
        {
            if (CR870_Stats.On)
            {
                CR870_Stats.Hit("nand r " + off.ToString("x"));
            }
            switch (off)
            {
                case 0xE00:
                {
                    var w = 0u;
                    for (var i = 0; i < size; i++)
                    {
                        w |= (uint)(fpos < flen ? fifo[fpos++] : 0xff) << (8 * i);
                    }
                    return w;
                }
                case 0xF18:
                    return (uint)(flen - fpos);
                case 0xF1C:
                    return 64;
                case 0x020:
                    return 0x2010;
                case 0x088:
                    return 16;
                default:
                    return regs.TryGetValue(off, out var v) ? v : 0;
            }
        }

        private void Write(long off, uint v, int size)
        {
            regs[off] = v;
            if (off == 0x200 || off == 0x204)
            {
                Command((byte)v);
            }
            else if (off >= 0x210 && off <= 0x220 && !Absent)
            {
                Address((byte)v);
            }
            else if (off == 0x250 || off == 0x254)
            {
                Fill(v);
            }
            else if (off == 0xE80)
            {
                for (var i = 0; i < size; i++)
                {
                    if (col < page.Length)
                    {
                        page[col++] = (byte)(v >> (8 * i));
                    }
                }
            }
        }

        private void LoadPage()
        {
            var o = (long)row * 2048;
            if (o + 2048 <= data.Length)
            {
                Array.Copy(data, o, page, 0, 2048);
                Array.Copy(spare, (long)row * 64, page, 2048, 64);
            }
            else
            {
                for (var i = 0; i < page.Length; i++)
                {
                    page[i] = 0xff;
                }
            }
        }

        /// <summary>Chip enables at +0x004 (bit 16 = CE0, 17 = CE1, 18 = CE2; Linux sulu_nand probes all three): the
        /// CR870 has one chip, so with only another CE set nothing answers (the bus reads 0xFF, programs do nothing).</summary>
        private bool Absent =>
            regs.TryGetValue(4, out var ce) && (ce & 0x70000) != 0 && (ce & 0x10000) == 0;

        private void Command(byte c)
        {
            if (Absent)
            {
                src = Src.Idle;
                return;
            }
            cmd = c;
            an = 0;
            switch (c)
            {
                case 0xFF:
                    src = Src.Idle;
                    break;
                case 0x70:
                    src = Src.Status;
                    break;
                case 0x90:
                    src = Src.Id;
                    idpos = 0;
                    break;
                case 0x30:
                    LoadPage();
                    src = Src.Page;
                    break;
                case 0xE0:
                    src = Src.Page;
                    break;
                case 0x10:
                {
                    var o = (long)row * 2048;
                    if (o + 2048 <= data.Length)
                    {
                        for (var i = 0; i < 2048; i++)
                        {
                            data[o + i] &= page[i];
                        }
                        for (var i = 0; i < 64; i++)
                        {
                            spare[row * 64 + i] &= page[2048 + i];
                        }
                    }
                    src = Src.Status;
                    break;
                }
                case 0xD0:
                {
                    var o = (long)(row & ~63u) * 2048;
                    if (o + 64 * 2048 <= data.Length)
                    {
                        for (var i = 0; i < 64 * 2048; i++)
                        {
                            data[o + i] = 0xff;
                        }
                        for (var i = 0; i < 64 * 64; i++)
                        {
                            spare[(row & ~63u) * 64 + i] = 0xff;
                        }
                    }
                    src = Src.Status;
                    break;
                }
                case 0x80:
                    for (var i = 0; i < page.Length; i++)
                    {
                        page[i] = 0xff;
                    }
                    break;
            }
        }

        private void Address(byte v)
        {
            if (an < 8)
            {
                abuf[an] = v;
            }
            an++;
            switch (cmd)
            {
                case 0x00:
                case 0x80:
                    if (an == 2)
                    {
                        col = abuf[0] | abuf[1] << 8;
                    }
                    if (an >= 3)
                    {
                        row = (uint)(
                            abuf[2] | (an > 3 ? abuf[3] << 8 : 0) | (an > 4 ? abuf[4] << 16 : 0)
                        );
                    }
                    break;
                case 0x05:
                    if (an == 2)
                    {
                        col = abuf[0] | abuf[1] << 8;
                    }
                    break;
                case 0x60:
                    row = (uint)(
                        abuf[0] | (an > 1 ? abuf[1] << 8 : 0) | (an > 2 ? abuf[2] << 16 : 0)
                    );
                    break;
                case 0x90:
                    idpos = 0;
                    break;
            }
        }

        private byte DataOut()
        {
            switch (src)
            {
                case Src.Id:
                    return idpos < 4 ? Id[idpos++] : (byte)0;
                case Src.Status:
                    return 0xE0;
                case Src.Page:
                    return col < page.Length ? page[col++] : (byte)0xff;
                default:
                    return 0xff;
            }
        }

        private void Fill(uint n)
        {
            if (fpos == flen)
            {
                fpos = flen = 0;
            }
            while (n-- > 0 && flen < fifo.Length)
            {
                fifo[flen++] = DataOut();
            }
        }

        private static readonly byte[] Id = { 0xAD, 0xF1, 0x80, 0x1D }; // Hynix HY27U1G8F2B

        private readonly byte[] data,
            spare;
        private readonly byte[] page = new byte[2048 + 64];
        private readonly byte[] fifo = new byte[2048 + 64 + 16];
        private readonly byte[] abuf = new byte[8];
        private readonly Dictionary<long, uint> regs = new Dictionary<long, uint>();
        private int flen,
            fpos,
            an,
            col,
            idpos;
        private byte cmd;
        private uint row;
        private Src src;
    }
}
