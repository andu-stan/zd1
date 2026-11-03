// Shared pieces of the AT32UC3A1 models: the machine trace log and the base class for big-endian register blocks.
//
// The trace log is zemu's mcu_trace.txt: one line per event, prefixed with virtual milliseconds ("%10.3f "). MMIO
// accesses are logged too when the block's TraceAccesses is on.
using System;
using System.Collections.Generic;
using System.IO;
using System.Text;
using Antmicro.Renode.Core;
using Antmicro.Renode.Logging;
using Antmicro.Renode.Peripherals.Bus;
using Antmicro.Renode.Peripherals.CPU;
using Antmicro.Renode.Time;

namespace Antmicro.Renode.Peripherals.UC3
{
    /// <summary>The per-machine trace log ("mcu.trace"). Set File from the platform or script; default: off.</summary>
    public class UC3Trace : IPeripheral
    {
        public UC3Trace(IMachine machine)
        {
            this.machine = machine;
            instances[machine] = this;
        }

        public static UC3Trace For(IMachine machine)
        {
            lock (instances)
            {
                if (!instances.TryGetValue(machine, out var t))
                {
                    t = new UC3Trace(machine);
                }
                return t;
            }
        }

        public string File
        {
            get => file;
            set
            {
                lock (sync)
                {
                    writer?.Dispose();
                    writer = null;
                    file = value;
                    if (!string.IsNullOrEmpty(value))
                    {
                        writer = new StreamWriter(value, true, Encoding.ASCII) { AutoFlush = true };
                    }
                }
            }
        }

        /// <summary>Stop after this many lines (0 = no limit), like ZD1_MAXLOG; Force lines pass anyway.</summary>
        public ulong MaxLines { get; set; } = 200000;

        /// <summary>Don't log polling reads of GPIO, USBB, ADC and the SPI status (ZD1_QUIET).</summary>
        public bool Quiet { get; set; }

        /// <summary>Virtual ms window where Quiet does not apply (ZD1_LOUD), e.g. "1000-2000".</summary>
        public string Loud
        {
            get => loud;
            set
            {
                loud = value;
                loudFrom = loudTo = 0;
                var p = (value ?? "").Split('-');
                if (
                    p.Length == 2
                    && double.TryParse(p[0], out var a)
                    && double.TryParse(p[1], out var b)
                )
                {
                    loudFrom = a;
                    loudTo = b;
                }
            }
        }

        public bool IsQuietNow
        {
            get
            {
                if (!Quiet)
                {
                    return false;
                }
                var t = NowMs;
                return !(t >= loudFrom && t < loudTo);
            }
        }

        public double NowMs => NowNs / 1e6;

        /// <summary>Virtual ns of the current instruction (the core's cheap clock), or the machine's time.</summary>
        public ulong NowNs =>
            machine.SystemBus.TryGetCurrentCPU(out var cpu) && cpu is AVR32UC u
                ? u.NowNs
                : machine.ElapsedVirtualTime.TimeElapsed.Ticks;

        public bool Enabled => writer != null;

        public void Log(string text, bool force = false)
        {
            lock (sync)
            {
                if (writer == null)
                {
                    return;
                }
                if (
                    MaxLines != 0
                    && lines >= MaxLines
                    && !force
                    && !text.StartsWith("***", StringComparison.Ordinal)
                )
                {
                    return;
                }
                lines++;
                writer.Write(string.Format("{0,10:F3} {1}\n", NowMs, text));
            }
        }

        public void Log(string format, params object[] args)
        {
            if (writer == null)
            {
                return;
            }
            Log(string.Format(format, args));
        }

        public void Reset() { }

        private readonly IMachine machine;
        private readonly object sync = new object();
        private string file,
            loud;
        private double loudFrom,
            loudTo;
        private StreamWriter writer;
        private ulong lines;

        private static readonly Dictionary<IMachine, UC3Trace> instances =
            new Dictionary<IMachine, UC3Trace>();
    }

    /// <summary>
    /// A block of 32-bit registers on the big-endian peripheral bus. Subclasses implement Read/Write at word offsets;
    /// byte and halfword accesses select lanes big-endian (offset 0 = bits 31..24).
    /// </summary>
    public abstract class UC3Peripheral
        : IDoubleWordPeripheral,
            IWordPeripheral,
            IBytePeripheral,
            IKnownSize
    {
        protected UC3Peripheral(IMachine machine, long size, string blockName)
        {
            this.machine = machine;
            this.size = size;
            BlockName = blockName;
            trace = UC3Trace.For(machine);
        }

        public long Size => size;

        public string BlockName { get; set; }

        /// <summary>Log every access as "R addr = value  BLOCK pc=..." / "W addr <- value".</summary>
        public bool TraceAccesses { get; set; } = true;

        public abstract void Reset();

        protected abstract uint Read(long offset);

        protected abstract void Write(long offset, uint value);

        /// <summary>Reads that firmware polls constantly; not logged when the trace is quiet.</summary>
        protected virtual bool IsPollRead(long offset) => false;

        /// <summary>Reads/writes never logged (too chatty even for QEMU's trace).</summary>
        protected virtual bool IsSilent(long offset, bool write) => false;

        public uint ReadDoubleWord(long offset)
        {
            var v = Read(offset & ~3);
            LogRead(offset & ~3, v);
            return v;
        }

        public void WriteDoubleWord(long offset, uint value)
        {
            LogWrite(offset & ~3, value);
            Write(offset & ~3, value);
        }

        public ushort ReadWord(long offset)
        {
            var v = ReadDoubleWord(offset & ~3);
            return (ushort)(v >> (int)((2 - (offset & 2)) * 8));
        }

        public void WriteWord(long offset, ushort value)
        {
            var sh = (int)((2 - (offset & 2)) * 8);
            WriteDoubleWord(
                offset & ~3,
                (Peek(offset & ~3) & ~(0xFFFFu << sh)) | (uint)value << sh
            );
        }

        public byte ReadByte(long offset)
        {
            var v = ReadDoubleWord(offset & ~3);
            return (byte)(v >> (int)((3 - (offset & 3)) * 8));
        }

        public void WriteByte(long offset, byte value)
        {
            var sh = (int)((3 - (offset & 3)) * 8);
            WriteDoubleWord(offset & ~3, (Peek(offset & ~3) & ~(0xFFu << sh)) | (uint)value << sh);
        }

        /// <summary>Current value of a register without side effects, for sub-word writes. Default 0.</summary>
        protected virtual uint Peek(long offset) => 0;

        protected uint Pc =>
            machine.SystemBus.TryGetCurrentCPU(out var cpu) ? (uint)cpu.PC.RawValue : 0;

        protected ulong NowNs => trace.NowNs;

        /// <summary>Bring virtual time up to the current instruction (the core does it before register writes, not
        /// before reads): call it before a read whose value depends on the time.</summary>
        protected void SyncTime()
        {
            if (machine.SystemBus.TryGetCurrentCPU(out var cpu))
            {
                cpu.SyncTime();
            }
        }

        /// <summary>After scheduling a new event (ScheduleAction): make the core look at its deadline now.</summary>
        protected void NewDeadline()
        {
            if (machine.SystemBus.TryGetCurrentCPU(out var cpu) && cpu is AVR32UC u)
            {
                u.RequestReturn();
            }
        }

        protected uint Lr =>
            machine.SystemBus.TryGetCurrentCPU(out var cpu) && cpu is AVR32UC u
                ? u.GetRegister(14)
                : 0;

        protected ulong BaseAddress
        {
            get
            {
                if (baseAddress == null)
                {
                    foreach (
                        var rp in machine.GetPeripheralRegistrationPoints(machine.SystemBus, this)
                    )
                    {
                        if (rp is BusRangeRegistration br)
                        {
                            baseAddress = br.Range.StartAddress;
                            break;
                        }
                    }
                    baseAddress = baseAddress ?? 0;
                }
                return baseAddress.Value;
            }
        }

        private void LogRead(long offset, uint v)
        {
            if (
                !TraceAccesses
                || !trace.Enabled
                || IsSilent(offset, false)
                || (IsPollRead(offset) && trace.IsQuietNow)
            )
            {
                return;
            }
            trace.Log(
                string.Format(
                    "R {0:x8} = {1:x8}  {2,-7} pc={3:x8}",
                    BaseAddress + (ulong)offset,
                    v,
                    BlockName,
                    Pc
                )
            );
        }

        private void LogWrite(long offset, uint v)
        {
            if (!TraceAccesses || !trace.Enabled || IsSilent(offset, true))
            {
                return;
            }
            trace.Log(
                string.Format(
                    "W {0:x8} <- {1:x8}  {2,-7} pc={3:x8}",
                    BaseAddress + (ulong)offset,
                    v,
                    BlockName,
                    Pc
                )
            );
        }

        protected readonly IMachine machine;
        protected readonly UC3Trace trace;
        private readonly long size;
        private ulong? baseAddress;
    }

    /// <summary>Registers nobody models: written values read back (QEMU's store). One instance per gap.</summary>
    public class UC3Store : UC3Peripheral
    {
        public UC3Store(IMachine machine, long size = 0x400, string blockName = "?")
            : base(machine, size, blockName) { }

        public override void Reset()
        {
            regs.Clear();
        }

        protected override uint Read(long offset) => regs.TryGetValue(offset, out var v) ? v : 0;

        protected override void Write(long offset, uint value) => regs[offset] = value;

        protected override uint Peek(long offset) => Read(offset);

        private readonly Dictionary<long, uint> regs = new Dictionary<long, uint>();
    }
}
