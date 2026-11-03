// AT32UC3A1 system blocks: interrupt controller (INTC), power manager with RTC/WDT/EIC/GPLP (PM), flash
// controller with the flash array and user page (FLASHC).
using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using Antmicro.Renode.Core;
using Antmicro.Renode.Core.Structure;
using Antmicro.Renode.Exceptions;
using Antmicro.Renode.Logging;
using Antmicro.Renode.Peripherals.Bus;
using Antmicro.Renode.Peripherals.CPU;
using Antmicro.Renode.Peripherals.Memory;
using Antmicro.Renode.Peripherals.Timers;
using Antmicro.Renode.Time;

namespace Antmicro.Renode.Peripherals.UC3
{
    /// <summary>
    /// INTC (UC3A datasheet ch. 16): 64 groups x 32 request lines. GPIO input n = group n/32, line n%32. Each group's
    /// IPR holds INTLEVEL [31:30] and AUTOVECTOR [13:0]. For every level the lowest-numbered group with a request
    /// wins (the datasheet says both "lowest" and "highest"; lowest is what the QEMU model did and the firmware
    /// passed with). The core takes the highest unmasked level.
    /// </summary>
    public class AT32UC3_INTC : UC3Peripheral, IGPIOReceiver
    {
        public AT32UC3_INTC(IMachine machine, AVR32UC cpu)
            : base(machine, 0x400, "INTC")
        {
            this.cpu = cpu;
            TraceAccesses = false;
            Reset();
        }

        public override void Reset()
        {
            Array.Clear(ipr, 0, ipr.Length);
            Array.Clear(irr, 0, irr.Length);
            Update();
        }

        public void OnGPIO(int number, bool value)
        {
            if (number < 0 || number >= 64 * 32)
            {
                return;
            }
            var g = number / 32;
            var m = 1u << (number % 32);
            var old = irr[g];
            irr[g] = value ? old | m : old & ~m;
            if ((old != 0) != (irr[g] != 0))
            {
                Update();
            }
        }

        /// <summary>The group of the highest-priority request at a level (ICR), -1 if none.</summary>
        public int CauseOf(int level)
        {
            for (var g = 0; g < 64; g++)
            {
                if (irr[g] != 0 && (int)(ipr[g] >> 30) == level)
                {
                    return g;
                }
            }
            return -1;
        }

        protected override uint Read(long offset)
        {
            if (offset < 0x100)
            {
                return ipr[offset >> 2];
            }
            if (offset < 0x200)
            {
                return irr[(offset - 0x100) >> 2];
            }
            if (offset <= 0x20C)
            {
                var g = CauseOf(3 - (int)((offset - 0x200) >> 2));
                return g < 0 ? 0 : (uint)g;
            }
            return 0;
        }

        protected override void Write(long offset, uint value)
        {
            if (offset < 0x100)
            {
                ipr[offset >> 2] = value & 0xC0003FFF;
                Update();
            }
        }

        protected override uint Peek(long offset) => offset < 0x100 ? ipr[offset >> 2] : 0;

        private void Update()
        {
            var mask = 0;
            var av = new uint[4];
            for (var level = 0; level < 4; level++)
            {
                var g = CauseOf(level);
                if (g >= 0)
                {
                    mask |= 1 << level;
                    av[level] = ipr[g] & 0x3FFF;
                }
            }
            cpu.SetPendingInterrupts(mask, av[0], av[1], av[2], av[3]);
        }

        private readonly AVR32UC cpu;
        private readonly uint[] ipr = new uint[64];
        private readonly uint[] irr = new uint[64];
    }

    /// <summary>
    /// Power manager at 0xFFFF0C00..0xFFFF0FFF, with its sub-blocks: RTC (0xD00), WDT (0xD30), EIC (0xD80), GPLP
    /// (0xE00). Clocks lock instantly (POSCSR ready bits follow the enables). MCCTRL.MCSEL and PLL0 set the CPU clock.
    /// WDT: keyed CTRL (0x55, then 0xAA with the same value); enabled -> chip reset after 2^(PSEL+1) RC cycles
    /// (115.2 kHz) unless cleared (CLR). Reset causes: RCAUSE POR (0x01) or WDT (0x08). A WDT reset keeps the WDT
    /// running into the next boot (hardware 2026-10-17; WdtSurvives = false: the old model, reset clears it).
    /// </summary>
    public class AT32UC3A_PM : UC3Peripheral, INumberedGPIOOutput
    {
        public AT32UC3A_PM(IMachine machine, AVR32UC cpu)
            : base(machine, 0x400, "PM")
        {
            this.cpu = cpu;
            Connections = new Dictionary<int, IGPIO> { { 0, new GPIO() } }; // unused (no PM interrupt modelled)
            wdt = new LimitTimer(
                machine.ClockSource,
                1000000000,
                this,
                "wdt",
                ulong.MaxValue,
                Direction.Descending,
                false,
                WorkMode.OneShot,
                true
            );
            wdt.LimitReached += OnWdtTimeout;
            rcause = 0x01;
            ColdReset();
        }

        public IReadOnlyDictionary<int, IGPIO> Connections { get; }

        /// <summary>The machine's trace log ("mcu.trace") file, its line cap and quiet mode (UC3Trace).</summary>
        public string TraceFile
        {
            get => trace.File;
            set => trace.File = value;
        }
        public ulong TraceMaxLines
        {
            get => trace.MaxLines;
            set => trace.MaxLines = value;
        }
        public bool TraceQuiet
        {
            get => trace.Quiet;
            set => trace.Quiet = value;
        }
        public string TraceLoud
        {
            get => trace.Loud;
            set => trace.Loud = value;
        }

        /// <summary>GPLP0/1 at power-up (survive chip resets).</summary>
        public uint GPLP0
        {
            get => gplp[0];
            set => gplp[0] = value;
        }
        public uint GPLP1
        {
            get => gplp[1];
            set => gplp[1] = value;
        }

        /// <summary>false: watchdog timeouts are only logged (ZD1_WDT=ignore).</summary>
        public bool WdtResets { get; set; } = true;

        /// <summary>A WDT reset leaves the WDT running (ZD1_WDT_SURVIVES, default on).</summary>
        public bool WdtSurvives { get; set; } = true;

        public long CpuHz
        {
            get
            {
                switch (mcctrl & 3)
                {
                    case 1:
                        return Osc0Hz;
                    case 2:
                    {
                        var mul = (pll0 >> 16) & 0xF;
                        var div = (pll0 >> 8) & 0xF;
                        var opt = (pll0 >> 2) & 7;
                        var vco = div != 0 ? Osc0Hz * (mul + 1) / div : 2L * Osc0Hz * (mul + 1);
                        return (opt & 2) != 0 ? vco / 2 : vco;
                    }
                    default:
                        return 115200;
                }
            }
        }

        public long Osc0Hz { get; set; } = 12000000;

        /// <summary>Raised on a chip reset request (watchdog); the machine script/board performs the reset.</summary>
        public event Action<string> ResetRequested;

        public override void Reset()
        {
            // a chip reset of the whole machine: called by Renode on machine Reset; the cause was set before
            ChipReset();
        }

        /// <summary>Power-on: GPLP and the RC registers clear, RCAUSE = POR.</summary>
        public void ColdReset()
        {
            Array.Clear(regs, 0, regs.Length);
            mcctrl = pll0 = 0;
            wdtCtrl = 0x00001000;
            wdtKey = 0xFFFFFFFF;
            wdtLoops = 0;
            rcause = 0x01;
            pendingWdt = false;
            wdt.Enabled = false;
            UpdateClock();
        }

        private void ChipReset()
        {
            var wdtCause = pendingWdt;
            pendingWdt = false;
            rcause = wdtCause ? 0x08u : 0x01u;
            Array.Clear(regs, 0, regs.Length);
            mcctrl = pll0 = 0;
            wdtKey = 0xFFFFFFFF;
            wdt.Enabled = false;
            if (wdtCause && WdtSurvives && (wdtCtrl & 1) != 0)
            {
                ArmWdt();
                if (++wdtLoops > 1000)
                {
                    trace.Log("*** WDT RESET LOOP: >1000 consecutive WDT resets, stopping");
                    machine.PauseAndRequestEmulationPause();
                }
            }
            else
            {
                wdtCtrl = 0x00001000;
                wdtLoops = 0;
            }
            UpdateClock();
            if (wdtLoops < 5)
            {
                trace.Log(
                    string.Format(
                        "=== RESET rcause={0:x2} fgpfr={1:x8} gplp0={2:x8}",
                        rcause,
                        Fgpfr?.Invoke() ?? 0,
                        gplp[0]
                    )
                );
            }
        }

        /// <summary>FLASHC hooks the fuses in for the RESET line.</summary>
        public Func<uint> Fgpfr { get; set; }

        protected override bool IsSilent(long offset, bool write) =>
            (!write && offset == 0x54) || (write && offset == 0x134);

        protected override uint Read(long offset)
        {
            if (offset >= 0x130 && offset < 0x140)
            {
                return offset == 0x130 ? wdtCtrl : 0;
            }
            switch (offset)
            {
                case 0x00:
                    return mcctrl;
                case 0x20:
                    return pll0;
                case 0x54: // POSCSR
                {
                    var v = (1u << 5) | (1u << 6); // CKRDY, MSKRDY
                    if ((mcctrl & (1u << 2)) != 0)
                    {
                        v |= 1u << 7; // OSC0RDY
                    }
                    if ((mcctrl & (1u << 3)) != 0)
                    {
                        v |= 1u << 8;
                    }
                    if ((pll0 & 1) != 0)
                    {
                        v |= 1u << 0; // LOCK0
                    }
                    if ((regs[0x24 / 4] & 1) != 0)
                    {
                        v |= 1u << 1;
                    }
                    if ((regs[0x30 / 4] & 1) != 0)
                    {
                        v |= 1u << 9;
                    }
                    return v;
                }
                case 0x140:
                    return rcause;
                case 0x200:
                case 0x204:
                    return gplp[(offset - 0x200) / 4];
                default:
                    return regs[offset / 4];
            }
        }

        protected override void Write(long offset, uint value)
        {
            if (offset == 0x130)
            {
                WdtWrite(value);
                return;
            }
            if (offset == 0x134)
            {
                if ((wdtCtrl & 1) != 0 && WdtResets)
                {
                    lastKick = NowNs; // CLR: restart the count (the timer re-arms lazily)
                }
                return;
            }
            switch (offset)
            {
                case 0x00:
                {
                    var old = mcctrl;
                    mcctrl = value;
                    UpdateClock();
                    if ((old & 3) != (value & 3))
                    {
                        trace.Log(
                            string.Format(
                                "*** MAIN CLOCK MCSEL {0} -> {1} ({2} Hz)",
                                old & 3,
                                value & 3,
                                CpuHz
                            )
                        );
                    }
                    break;
                }
                case 0x20:
                    pll0 = value;
                    UpdateClock();
                    break;
                case 0x140:
                    break;
                case 0x200:
                case 0x204:
                    gplp[(offset - 0x200) / 4] = value;
                    break;
                default:
                    regs[offset / 4] = value;
                    break;
            }
        }

        protected override uint Peek(long offset) =>
            offset == 0 ? mcctrl
            : offset == 0x20 ? pll0
            : offset < 0x400 ? regs[offset / 4]
            : 0;

        private void UpdateClock()
        {
            cpu.ClockFrequency = CpuHz;
            ClockChanged?.Invoke(CpuHz);
        }

        /// <summary>Main clock changed (peripheral bus clocks follow it: PBA = PBB = main clock).</summary>
        public event Action<long> ClockChanged;

        private ulong WdtNs => (1ul << (int)(((wdtCtrl >> 8) & 0x1F) + 1)) * 1000000000ul / 115200;

        private void ArmWdt()
        {
            SyncTime();
            lastKick = armedAt = NowNs;
            Arm(WdtNs);
        }

        private void Arm(ulong ns)
        {
            SyncTime();
            wdt.Enabled = false;
            wdt.Limit = Math.Max(1, ns);
            wdt.Value = Math.Max(1, ns);
            wdt.Enabled = true;
        }

        private void WdtWrite(uint v)
        {
            var key = v >> 24;
            if (key == 0x55)
            {
                wdtKey = v & 0x00FFFFFF;
                return;
            }
            if (key == 0xAA && wdtKey == (v & 0x00FFFFFF))
            {
                wdtCtrl = v & 0x00FFFFFF;
                var psel = (v >> 8) & 0x1F;
                if ((v & 1) != 0)
                {
                    trace.Log(
                        string.Format(
                            "*** WDT ENABLED psel={0} timeout={1:F3} ms {2}",
                            psel,
                            WdtNs / 1e6,
                            WdtResets ? "" : "(reset modelling OFF)"
                        )
                    );
                    if (WdtResets)
                    {
                        ArmWdt();
                    }
                }
                else
                {
                    trace.Log("*** WDT disabled");
                    wdt.Enabled = false;
                }
            }
            else
            {
                trace.Log(string.Format("*** WDT bad key sequence {0:x8}", v));
            }
            wdtKey = 0xFFFFFFFF;
        }

        private void OnWdtTimeout()
        {
            if (lastKick > armedAt)
            {
                // kicked since the timer was armed: the real deadline is a full period after the last kick
                var due = lastKick + WdtNs;
                var now = NowNs;
                armedAt = lastKick;
                if (due > now)
                {
                    Arm(due - now);
                    return;
                }
            }
            trace.Log("*** WDT TIMEOUT -> system reset (RCAUSE=WDT)");
            pendingWdt = true;
            ResetRequested?.Invoke("wdt");
            machine.RequestResetInSafeState();
        }

        private readonly AVR32UC cpu;
        private readonly uint[] regs = new uint[0x100];
        private readonly uint[] gplp = new uint[2];
        private uint mcctrl,
            pll0,
            rcause,
            wdtCtrl,
            wdtKey;
        private bool pendingWdt;
        private int wdtLoops;
        private ulong lastKick,
            armedAt;
        private readonly LimitTimer wdt;
    }

    /// <summary>
    /// FLASHC (0xFFFE1400) in front of two AT32UC3_FlashArray peripherals: the flash array (256 KB at 0x80000000,
    /// mirrored over the 1 MB window: the stock firmware checksums all of it) and the user page (512 B at
    /// 0x80800000). Both behave like the hardware's page buffer: CPU writes into the array address space fill the
    /// 512-byte page buffer (reads still see the array), WP/WUP AND it into the page, EP/EUP/EA erase, CPB clears
    /// it. The 16 KB bootloader (BOOTPROT) is never erased. FGPFR = the general purpose fuses (WGPB/EGPB).
    /// </summary>
    public class AT32UC3_FLASHC : UC3Peripheral
    {
        public AT32UC3_FLASHC(
            IMachine machine,
            AT32UC3_FlashArray array,
            AT32UC3_FlashArray userPage
        )
            : base(machine, 0x400, "FLASHC")
        {
            this.array = array;
            this.userPage = userPage;
            array.Controller = this;
            userPage.Controller = this;
            Fgpfr = 0x7c04ffff;
            Reset();
        }

        public uint Fgpfr { get; set; }

        public long BootprotSize { get; set; } = 0x4000;

        public override void Reset()
        {
            ClearBuffer();
            qprr = false;
            fcr = 0;
        }

        protected override bool IsSilent(long offset, bool write) => !write && offset == 0x08;

        protected override uint Read(long offset)
        {
            switch (offset)
            {
                case 0x00:
                    return fcr;
                case 0x04:
                    return fcmd; // KEY reads 0; PAGEN follows page-buffer writes
                case 0x08:
                    return 1u | (qprr ? 1u << 5 : 0) | 3u << 13; // FRDY, QPRR, FSZ = 3 (256 KB)
                case 0x10:
                    return Fgpfr;
                default:
                    return 0;
            }
        }

        protected override void Write(long offset, uint value)
        {
            if (offset == 0x00)
            {
                fcr = value;
            }
            if (offset != 0x04)
            {
                return;
            }
            var key = value >> 24;
            var cmd = value & 0x3F;
            var pagen = (value >> 8) & 0xFFFF;
            fcmd = value & 0x00FFFF3F;
            trace.Log(
                string.Format("*** FLASHC FCMD key={0:x2} cmd={1} pagen={2}", key, cmd, pagen)
            );
            if (key != 0xA5)
            {
                return;
            }
            var a0 = (int)(pagen & 0x1FF) * 512;
            switch (cmd)
            {
                case 1: // WP
                    array.Program(a0, pageBuffer);
                    ClearBuffer();
                    break;
                case 2: // EP
                    if (a0 >= BootprotSize)
                    {
                        array.Erase(a0, 512);
                    }
                    break;
                case 3: // CPB
                    ClearBuffer();
                    break;
                case 4: // LP, UP: region locks (not modelled: never locked)
                case 5:
                    break;
                case 6: // EA
                    array.Erase((int)BootprotSize, array.Data.Length - (int)BootprotSize);
                    break;
                case 7: // WGPB
                    if (pagen < 32)
                    {
                        Fgpfr &= ~(1u << (int)pagen);
                    }
                    break;
                case 8: // EGPB
                    if (pagen < 32)
                    {
                        Fgpfr |= 1u << (int)pagen;
                    }
                    break;
                case 12: // QPR
                    qprr = array.IsErased(a0, 512);
                    break;
                case 13: // WUP
                    userPage.Program(0, pageBuffer);
                    ClearBuffer();
                    break;
                case 14: // EUP
                    userPage.Erase(0, 512);
                    break;
                case 15: // QPRUP
                    qprr = userPage.IsErased(0, 512);
                    break;
            }
            if (cmd == 7 || cmd == 8)
            {
                trace.Log(
                    string.Format("*** FGPFR now {0:x8} (ISP_FORCE={1})", Fgpfr, Fgpfr >> 31)
                );
            }
        }

        internal void BufferWrite(AT32UC3_FlashArray target, long offset, int len, uint value)
        {
            if (target == array)
            {
                // "PAGEN is updated with the page number of the page buffer write" (ASF's flashc_issue_command(WP, -1)
                // and the B&W bootloader read it back)
                fcmd = (fcmd & ~0x00FFFF00u) | (uint)((offset % array.Data.Length) / 512) << 8;
            }
            // the page buffer is addressed by the low 9 bits of the array address; the CPU is big-endian
            for (var i = 0; i < len; i++)
            {
                pageBuffer[(offset + i) & 0x1FF] = (byte)(value >> (8 * (len - 1 - i)));
            }
        }

        private void ClearBuffer()
        {
            for (var i = 0; i < pageBuffer.Length; i++)
            {
                pageBuffer[i] = 0xFF;
            }
        }

        private readonly AT32UC3_FlashArray array,
            userPage;
        private readonly byte[] pageBuffer = new byte[512];
        private bool qprr;
        private uint fcr,
            fcmd;
    }

    /// <summary>A flash array (main array or user page): reads straight from the array, writes into the FLASHC page
    /// buffer. Erased = 0xFF. Keeps its contents across resets (it is flash).</summary>
    public class AT32UC3_FlashArray
        : IAvr32DirectMemory,
            IBytePeripheral,
            IWordPeripheral,
            IDoubleWordPeripheral,
            IKnownSize
    {
        public AT32UC3_FlashArray(long size)
        {
            data = new byte[size];
            for (var i = 0; i < data.Length; i++)
            {
                data[i] = 0xFF;
            }
        }

        public byte[] Data => data;
        public bool CpuWritable => false;
        public long Size => data.Length;
        public event Action<int, int> Changed;

        internal AT32UC3_FLASHC Controller { get; set; }

        public void Reset() { }

        public void Load(string file)
        {
            var b = File.ReadAllBytes(file);
            System.Array.Copy(b, data, Math.Min(b.Length, data.Length));
            Changed?.Invoke(0, data.Length);
        }

        public void Save(string file)
        {
            File.WriteAllBytes(file, data);
        }

        public void Program(int offset, byte[] page)
        {
            for (var i = 0; i < page.Length && offset + i < data.Length; i++)
            {
                data[offset + i] &= page[i];
            }
            Changed?.Invoke(offset, page.Length);
        }

        public void Erase(int offset, int len)
        {
            for (var i = 0; i < len && offset + i < data.Length; i++)
            {
                data[offset + i] = 0xFF;
            }
            Changed?.Invoke(offset, len);
        }

        public bool IsErased(int offset, int len)
        {
            for (var i = 0; i < len && offset + i < data.Length; i++)
            {
                if (data[offset + i] != 0xFF)
                {
                    return false;
                }
            }
            return true;
        }

        public byte ReadByte(long offset) => data[offset % data.Length];

        public ushort ReadWord(long offset) =>
            (ushort)(data[offset % data.Length] << 8 | data[(offset + 1) % data.Length]);

        public uint ReadDoubleWord(long offset)
        {
            offset %= data.Length;
            return (uint)(
                data[offset] << 24
                | data[offset + 1] << 16
                | data[offset + 2] << 8
                | data[offset + 3]
            );
        }

        public void WriteByte(long offset, byte value) =>
            Controller?.BufferWrite(this, offset, 1, value);

        public void WriteWord(long offset, ushort value) =>
            Controller?.BufferWrite(this, offset, 2, value);

        public void WriteDoubleWord(long offset, uint value) =>
            Controller?.BufferWrite(this, offset, 4, value);

        private readonly byte[] data;
    }
}
