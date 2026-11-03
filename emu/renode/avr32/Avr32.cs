// AVR32UC CPU (AVR32A microarchitecture, architecture revision 2) for Renode, as an interpreter.
//
// Reference: AVR32 Architecture Manual 32000D (instruction semantics, flags), AVR32UC Technical Reference Manual 32002F
// (event handling, system registers, COUNT/COMPARE). Encodings: avr32/insn.decode (QEMU decodetree, our fixed copy)
// -> Avr32Decoder.g.cs. Where the QEMU CISPA translator differs from the manuals, the manuals win; the differences
// that matter are listed in emu/renode/README.md.
//
// Time: one slot of BaseCPU's budget is one nanosecond (PerformanceInMips = 1000). Every instruction costs one CPU
// clock (the UC core issues most instructions in one cycle), i.e. 1e9 / ClockFrequency slots; the clock follows the
// power manager (115 kHz RC, 12 MHz crystal, PLL), which sets ClockFrequency. COUNT runs at the CPU clock in every
// mode, sleep included. SLEEP returns WaitingForInterrupt, so idle time costs no host time.
using System;
using System.Collections.Generic;
using System.Linq;
using System.Text;
using Antmicro.Renode.Core;
using Antmicro.Renode.Core.Structure;
using Antmicro.Renode.Exceptions;
using Antmicro.Renode.Logging;
using Antmicro.Renode.Peripherals.Bus;
using Antmicro.Renode.Peripherals.Memory;
using Antmicro.Renode.Peripherals.Timers;
using Antmicro.Renode.Time;
using Antmicro.Renode.Utilities;
using Endianess = ELFSharp.ELF.Endianess;

namespace Antmicro.Renode.Peripherals.CPU
{
    public class AVR32UC : BaseCPU, IGPIOReceiver, INumberedGPIOOutput, IControllableCPU
    {
        public AVR32UC(
            IMachine machine,
            string cpuType = "uc3a",
            uint resetPC = 0x80000000,
            long clockFrequency = 115200
        )
            : base(0, cpuType, machine, Endianess.BigEndian)
        {
            this.resetPC = resetPC;
            PerformanceInMips = 1000; // 1 slot = 1 ns
            var gpios = new Dictionary<int, IGPIO>();
            gpios[0] = new GPIO(); // COMPARE interrupt request (INTC group 0 line 0)
            Connections = gpios;
            countTimer = new LimitTimer(
                machine.ClockSource,
                (ulong)clockFrequency,
                this,
                "count",
                uint.MaxValue,
                Direction.Ascending,
                true,
                workMode: WorkMode.Periodic,
                eventEnabled: false
            );
            compareTimer = new LimitTimer(
                machine.ClockSource,
                (ulong)clockFrequency,
                this,
                "compare",
                uint.MaxValue,
                Direction.Descending,
                false,
                workMode: WorkMode.OneShot,
                eventEnabled: true
            );
            compareTimer.LimitReached += OnCompareMatch;
            SetClock(clockFrequency);
            machine.PeripheralsChanged += (_, __) => regionsDirty = true;
            Reset();
        }

        public IReadOnlyDictionary<int, IGPIO> Connections { get; }

        // ---------------------------------------------------------------------------------------------------------
        // Public state

        public override string Architecture => "avr32";

        public override RegisterValue PC
        {
            get => r[15];
            set
            {
                r[15] = (uint)value.RawValue;
                sleeping = false;
            }
        }

        public override ulong ExecutedInstructions => executedInstructions;

        /// <summary>CPU clock in Hz. The power manager model sets it when the main clock changes.</summary>
        public long ClockFrequency
        {
            get => clockHz;
            set => SetClock(value);
        }

        public uint SR
        {
            get => sr;
            set => WriteSR(value);
        }

        public uint EVBA
        {
            get => sys[1];
            set => sys[1] = value;
        }

        /// <summary>Log every executed instruction (pc, raw, then the registers) to this file; empty = off.</summary>
        public string TraceFile
        {
            get => traceFile;
            set
            {
                traceWriter?.Dispose();
                traceWriter = null;
                traceFile = value;
                if (!string.IsNullOrEmpty(value))
                {
                    traceWriter = new System.IO.StreamWriter(value, false, Encoding.ASCII, 1 << 20);
                }
            }
        }

        /// <summary>Stop tracing after this many instructions (0 = no limit).</summary>
        public ulong TraceLimit { get; set; }

        /// <summary>Debug register DID (mfdr 0): the B&W bootloader checks the part number before it starts DFU.</summary>
        public uint DebugDID { get; set; } = (0x1EE0u << 12) | 0x03Fu;

        /// <summary>Statistics: accesses that went over the system bus (peripherals), ExecuteInstructions calls.</summary>
        public ulong BusReads { get; set; }
        public ulong BusWrites { get; set; }
        public ulong ExecuteCalls { get; set; }

        public uint GetRegister(int n)
        {
            if (n < 0 || n > 15)
            {
                throw new RecoverableException("register 0..15");
            }
            return r[n];
        }

        public void SetRegister(int n, uint value)
        {
            if (n < 0 || n > 15)
            {
                throw new RecoverableException("register 0..15");
            }
            r[n] = value;
        }

        public string DumpRegisters()
        {
            var sb = new StringBuilder();
            for (var i = 0; i < 16; i++)
            {
                sb.AppendFormat("{0}={1:X8} ", RegName(i), r[i]);
            }
            sb.AppendFormat("SR={0:X8} mode={1}", sr, Mode);
            return sb.ToString();
        }

        public override void Reset()
        {
            base.Reset();
            Array.Clear(r, 0, r.Length);
            Array.Clear(sys, 0, sys.Length);
            r[15] = resetPC;
            sr = SR_GM | SR_EM | (1u << 22); // supervisor, all masked
            spApp = spSys = 0;
            sys[3] = 0x00002110; // CPUCR: SPL 16, CPL 16 (not used), COP 8, SIE 1
            sys[64] = 0x01 << 24 | 2 << 10 | 3 << 7 | 1 << 3 | 1 << 1 | 1 << 0; // CONFIG0: AR rev 2, MPU, OCD, DSP, RMW
            sys[65] = 8 << 20; // CONFIG1: 8 MPU entries
            sleeping = false;
            debt = 0;
            countTimer.Value = 0;
            countTimer.Enabled = true;
            compareTimer.Enabled = false;
            Connections[0].Unset();
            regionsDirty = true;
        }

        // ---------------------------------------------------------------------------------------------------------
        // Interrupt inputs

        /// <summary>From the interrupt controller: which levels have a pending request, and each level's
        /// autovector (handler offset from EVBA). Called whenever that changes.</summary>
        public void SetPendingInterrupts(int levelMask, uint av0, uint av1, uint av2, uint av3)
        {
            autovector[0] = av0;
            autovector[1] = av1;
            autovector[2] = av2;
            autovector[3] = av3;
            pendingLevels = levelMask & 0xF;
        }

        /// <summary>GPIO 0 = NMI request.</summary>
        public void OnGPIO(int number, bool value)
        {
            if (number == 0)
            {
                nmiPending = value;
            }
        }

        // ---------------------------------------------------------------------------------------------------------
        // Execution

        public override ExecutionResult ExecuteInstructions(
            ulong numberOfInstructionsToExecute,
            out ulong numberOfExecutedInstructions
        )
        {
            if (regionsDirty)
            {
                RebuildRegions();
            }
            ExecuteCalls++;
            chunkStartNs = machine.ElapsedVirtualTime.TimeElapsed.Ticks;
            inChunk = true;
            budget = numberOfInstructionsToExecute;
            reported = 0;
            used = 0;
            if (debt > 0)
            {
                var d = Math.Min(debt, budget);
                debt -= d;
                used = d;
            }
            var result = ExecutionResult.Ok;
            while (used < budget)
            {
                if (sleeping)
                {
                    if (!CanTakeInterrupt())
                    {
                        // asleep: the budget ends at the next clock event (BaseCPU caps it there), so spend it
                        // without executing; returning WaitingForInterrupt would skip to the end of the quantum and
                        // take an event's interrupt up to a quantum late. Costs nothing: no instructions run.
                        used = budget;
                        break;
                    }
                    sleeping = false;
                }
                if (TakePendingInterrupt())
                {
                    continue;
                }
                Step1();
                if (returnEarly)
                {
                    // a peripheral was touched or a timer reprogrammed: give BaseCPU the chance to recompute the
                    // nearest timer deadline (it calls us again right away), so events land on time
                    returnEarly = false;
                    break;
                }
                if (sleeping)
                {
                    continue;
                }
            }
            if (used > budget)
            {
                debt = used - budget;
                used = budget;
            }
            numberOfExecutedInstructions = used - reported;
            inChunk = false;
            return result;
        }

        public override void SyncTime()
        {
            if (!inChunk)
            {
                return; // not executing (e.g. a reset in safe state): nothing to report
            }
            var u = Math.Min(used, budget);
            if (u > reported)
            {
                ReportProgress(u - reported);
                reported = u;
            }
        }

        /// <summary>Virtual time of the instruction being executed, in ns, without synchronising the machine's clock
        /// (for timestamps; code that reprograms a timer calls SyncTime instead).</summary>
        public ulong NowNs =>
            inChunk
                ? chunkStartNs + Math.Min(used, budget)
                : machine.ElapsedVirtualTime.TimeElapsed.Ticks;

        /// <summary>Ask the core to leave ExecuteInstructions after the current instruction (models that must
        /// act on the machine, e.g. a reset request).</summary>
        public void RequestStop()
        {
            returnEarly = true;
        }

        /// <summary>IControllableCPU: a timer was reprogrammed, leave ExecuteInstructions so the next deadline is
        /// taken into account.</summary>
        public void RequestReturn()
        {
            returnEarly = true;
        }

        private void Step1()
        {
            var pc = r[15];
            Avr32Insn d;
            if (!Fetch(pc, out d))
            {
                return; // exception already taken
            }
            nextPc = pc + (uint)d.Len;
            jumped = false;
            if (traceWriter != null)
            {
                Trace(pc, d);
            }
            try
            {
                Execute(d, pc);
            }
            catch (Avr32Exception e)
            {
                TakeException(e.Offset, pc, e.Cause, e.Address);
                AccountInstruction();
                return;
            }
            r[15] = nextPc;
            AccountInstruction();
        }

        private void AccountInstruction()
        {
            executedInstructions++;
            frac += slotPerInsnQ16;
            used += frac >> 16;
            frac &= 0xFFFF;
        }

        private void Trace(uint pc, Avr32Insn d)
        {
            if (TraceLimit != 0 && executedInstructions >= TraceLimit)
            {
                traceWriter.Flush();
                return;
            }
            traceWriter.Write("{0:x8} {1:x8} {2}", pc, d.Len == 2 ? d.Raw >> 16 : d.Raw, d.Op);
            for (var i = 0; i < 15; i++)
            {
                traceWriter.Write(" {0:x}", r[i]);
            }
            traceWriter.Write(" sr={0:x}\n", sr);
        }

        // ---------------------------------------------------------------------------------------------------------
        // Status register, modes, stack banking

        private const uint SR_C = 1,
            SR_Z = 2,
            SR_N = 4,
            SR_V = 8,
            SR_Q = 16,
            SR_L = 32,
            SR_GM = 1u << 16,
            SR_EM = 1u << 21;
        private const uint SR_MODE = 7u << 22,
            SR_D = 1u << 26,
            SR_DM = 1u << 27;

        private int Mode => (int)((sr >> 22) & 7);
        private bool Privileged => Mode != 0;

        private void WriteSR(uint value)
        {
            var oldApp = Mode == 0;
            sr = value;
            var newApp = Mode == 0;
            if (oldApp != newApp)
            {
                if (oldApp)
                {
                    spApp = r[13];
                    r[13] = spSys;
                }
                else
                {
                    spSys = r[13];
                    r[13] = spApp;
                }
            }
        }

        /// <summary>The system stack pointer whatever the mode (events push to SP_SYS).</summary>
        private uint SpSys
        {
            get => Mode == 0 ? spSys : r[13];
            set
            {
                if (Mode == 0)
                {
                    spSys = value;
                }
                else
                {
                    r[13] = value;
                }
            }
        }

        private void PushSys(uint v)
        {
            var sp = SpSys - 4;
            Store32(sp, v);
            SpSys = sp;
        }

        private uint PopSys()
        {
            var sp = SpSys;
            var v = Load32(sp);
            SpSys = sp + 4;
            return v;
        }

        private bool Cond(int c)
        {
            var s = sr;
            var C = (s & SR_C) != 0;
            var Z = (s & SR_Z) != 0;
            var N = (s & SR_N) != 0;
            var V = (s & SR_V) != 0;
            switch (c)
            {
                case 0:
                    return Z;
                case 1:
                    return !Z;
                case 2:
                    return !C;
                case 3:
                    return C;
                case 4:
                    return N == V;
                case 5:
                    return N != V;
                case 6:
                    return N;
                case 7:
                    return !N;
                case 8:
                    return C || Z;
                case 9:
                    return !Z && N == V;
                case 10:
                    return Z || N != V;
                case 11:
                    return !C && !Z;
                case 12:
                    return V;
                case 13:
                    return !V;
                case 14:
                    return (s & SR_Q) != 0;
                default:
                    return true;
            }
        }

        private void SetNZ(uint res)
        {
            sr = (sr & ~(SR_N | SR_Z)) | (res >> 31) * SR_N | (res == 0 ? SR_Z : 0);
        }

        private void SetZ(bool z)
        {
            sr = z ? sr | SR_Z : sr & ~SR_Z;
        }

        private void SetC(bool c)
        {
            sr = c ? sr | SR_C : sr & ~SR_C;
        }

        private void FlagsAdd(uint op1, uint op2, uint res, bool zKeep = false)
        {
            var v = ((op1 & op2 & ~res) | (~op1 & ~op2 & res)) >> 31;
            var c = ((op1 & op2) | (op1 & ~res) | (op2 & ~res)) >> 31;
            var z = res == 0 && (!zKeep || (sr & SR_Z) != 0);
            sr = (sr & ~0xFu) | c | (z ? SR_Z : 0) | (res >> 31) * SR_N | v * SR_V;
        }

        private void FlagsSub(uint op1, uint op2, uint res, bool zKeep = false)
        {
            var v = ((op1 & ~op2 & ~res) | (~op1 & op2 & res)) >> 31;
            var c = ((~op1 & op2) | (op2 & res) | (~op1 & res)) >> 31;
            var z = res == 0 && (!zKeep || (sr & SR_Z) != 0);
            sr = (sr & ~0xFu) | c | (z ? SR_Z : 0) | (res >> 31) * SR_N | v * SR_V;
        }

        /// <summary>Flags as for "cp R12, 0" after a return value is set (ret, popm/ldm with PC).</summary>
        private void FlagsRet()
        {
            var res = r[12];
            sr = (sr & ~0xFu) | (res == 0 ? SR_Z : 0) | (res >> 31) * SR_N;
        }

        // ---------------------------------------------------------------------------------------------------------
        // Events (interrupts, exceptions)

        private sealed class Avr32Exception : Exception
        {
            public Avr32Exception(uint offset, string cause, uint address = 0)
            {
                Offset = offset;
                Cause = cause;
                Address = address;
            }

            public readonly uint Offset;
            public readonly string Cause;
            public readonly uint Address;
        }

        private const uint EX_UNRECOVERABLE = 0x00,
            EX_BUS_DATA = 0x08,
            EX_BUS_INSN = 0x0C,
            EX_NMI = 0x10,
            EX_INSN_ADDR = 0x14,
            EX_BREAKPOINT = 0x1C,
            EX_ILLEGAL = 0x20,
            EX_UNIMPL = 0x24,
            EX_PRIV = 0x28,
            EX_DADDR_R = 0x34,
            EX_DADDR_W = 0x38,
            EX_SCALL = 0x100;

        private bool CanTakeInterrupt()
        {
            if (nmiPending && Mode != 7)
            {
                return true;
            }
            if ((sr & SR_GM) != 0)
            {
                return false;
            }
            return (pendingLevels & ~(int)(sr >> 17) & 0xF) != 0;
        }

        private bool TakePendingInterrupt()
        {
            if (nmiPending && Mode != 7)
            {
                // NMI: like an exception, SR and PC only
                var old = sr;
                WriteSR((sr & ~SR_MODE) | (7u << 22));
                PushSys(r[15]);
                PushSys(old);
                sr |= SR_EM | SR_GM;
                r[15] = sys[1] | EX_NMI;
                return true;
            }
            if (pendingLevels == 0 || (sr & SR_GM) != 0)
            {
                return false;
            }
            var acceptable = pendingLevels & ~(int)(sr >> 17) & 0xF;
            if (acceptable == 0)
            {
                return false;
            }
            var level =
                acceptable >= 8 ? 3
                : acceptable >= 4 ? 2
                : acceptable >= 2 ? 1
                : 0;
            var oldSr = sr;
            var oldApp = Mode == 0;
            if (oldApp)
            {
                spApp = r[13];
                r[13] = spSys;
            }
            sr = (sr & ~SR_MODE) | ((uint)(2 + level) << 22); // mode INT0..INT3 (privileged: SP_SYS active)
            var sp = r[13];
            Store32(sp - 4, r[8]);
            Store32(sp - 8, r[9]);
            Store32(sp - 12, r[10]);
            Store32(sp - 16, r[11]);
            Store32(sp - 20, r[12]);
            Store32(sp - 24, r[14]);
            Store32(sp - 28, r[15]);
            Store32(sp - 32, oldSr);
            r[13] = sp - 32;
            for (var i = 0; i <= level; i++)
            {
                sr |= 1u << (17 + i);
            }
            var av = autovector[level] & 0x3FFF;
            if ((sys[1] & av) != 0 && !warnedEvbaOr)
            {
                warnedEvbaOr = true;
                this.Log(
                    LogLevel.Warning,
                    "EVBA 0x{0:X8} and autovector 0x{1:X} overlap: handler at EVBA|autovector = 0x{2:X8} (EVBA+autovector would be 0x{3:X8})",
                    sys[1],
                    av,
                    sys[1] | av,
                    sys[1] + av
                );
            }
            r[15] = sys[1] | av;
            sys[4] = av >> 2; // ECR
            AccountInstruction(); // entry takes time; also guarantees progress
            return true;
        }

        private void TakeException(uint offset, uint pc, string cause, uint address)
        {
            this.Log(
                LogLevel.Warning,
                "exception {0} (EVBA+0x{1:X}) at pc 0x{2:X8}{3}; {4}",
                cause,
                offset,
                pc,
                address != 0 ? string.Format(", address 0x{0:X8}", address) : "",
                DumpRegisters()
            );
            if (offset == EX_SCALL)
            {
                return; // not used: scall is executed inline
            }
            if ((sr & SR_EM) != 0 && offset != EX_NMI)
            {
                offset = EX_UNRECOVERABLE;
            }
            var old = sr;
            WriteSR((sr & ~SR_MODE) | (6u << 22));
            PushSys(pc);
            PushSys(old);
            sr |= SR_EM | SR_GM;
            if (offset != EX_UNRECOVERABLE && offset != EX_BREAKPOINT)
            {
                sys[4] = offset >> 2;
            }
            if (offset == EX_DADDR_R || offset == EX_DADDR_W)
            {
                sys[79] = address; // BEAR
            }
            r[15] = sys[1] | offset;
        }

        // ---------------------------------------------------------------------------------------------------------
        // Memory

        private sealed class Region
        {
            public uint Start,
                End; // [Start, End)
            public byte[] Data;
            public bool Writable;
            public Avr32Insn[] Code;
            public IAvr32DirectMemory Owner;
        }

        private void RebuildRegions()
        {
            regionsDirty = false;
            var list = new List<Region>();
            foreach (var mem in machine.GetPeripheralsOfType<IAvr32DirectMemory>())
            {
                foreach (var rp in machine.GetPeripheralRegistrationPoints(machine.SystemBus, mem))
                {
                    if (rp is BusRangeRegistration br && br.Range.Size == (ulong)mem.Data.Length)
                    {
                        var reg = new Region
                        {
                            Start = (uint)br.Range.StartAddress,
                            End = (uint)(br.Range.StartAddress + br.Range.Size),
                            Data = mem.Data,
                            Writable = mem.CpuWritable,
                            Code = new Avr32Insn[mem.Data.Length / 2],
                            Owner = mem,
                        };
                        list.Add(reg);
                    }
                }
            }
            foreach (var old in regions)
            {
                old.Owner.Changed -= OnMemoryChanged;
            }
            regions = list.ToArray();
            foreach (var reg in regions.Select(x => x.Owner).Distinct())
            {
                reg.Changed += OnMemoryChanged;
            }
            lastData = lastCode = null;
            // peripheral space 0xFFFE0000..0xFFFFFFFF in 256-byte pages -> the model objects, bypassing the system
            // bus lookup (which costs ~1 us per access; firmware polls registers millions of times per second)
            pages = new PageEntry[512];
            foreach (var per in machine.GetPeripheralsOfType<IBusPeripheral>())
            {
                if (per is IAvr32DirectMemory)
                {
                    continue;
                }
                foreach (var rp in machine.GetPeripheralRegistrationPoints(machine.SystemBus, per))
                {
                    if (
                        !(rp is BusRangeRegistration br)
                        || br.Range.StartAddress < PeriphBase
                        || br.Range.Size == 0
                    )
                    {
                        continue;
                    }
                    var e = new PageEntry
                    {
                        Start = (uint)br.Range.StartAddress,
                        End = (uint)(br.Range.StartAddress + br.Range.Size),
                        Dword = per as IDoubleWordPeripheral,
                        Word = per as IWordPeripheral,
                        Byte = per as IBytePeripheral,
                    };
                    for (var a = (ulong)e.Start & ~0xFFul; a < e.End; a += 0x100)
                    {
                        var i = (int)((a - PeriphBase) >> 8);
                        if (i >= 0 && i < pages.Length && a >= e.Start)
                        {
                            pages[i] = e;
                        }
                    }
                }
            }
            this.Log(
                LogLevel.Debug,
                "direct regions: {0}",
                string.Join(
                    ", ",
                    regions.Select(x =>
                        string.Format("{0:X8}-{1:X8}{2}", x.Start, x.End, x.Writable ? "" : " ro")
                    )
                )
            );
        }

        private void OnMemoryChanged(int offset, int length)
        {
            foreach (var reg in regions)
            {
                // Changed carries the owner's offset; every region of that owner shares Data
                var a = Math.Max(0, (offset >> 1) - 1);
                var b = Math.Min(reg.Code.Length, (offset + length + 1) >> 1);
                for (var i = a; i < b; i++)
                {
                    reg.Code[i] = null;
                }
            }
        }

        private sealed class PageEntry
        {
            public uint Start,
                End;
            public IDoubleWordPeripheral Dword;
            public IWordPeripheral Word;
            public IBytePeripheral Byte;
        }

        private const ulong PeriphBase = 0xFFFE0000;

        private PageEntry Page(uint a)
        {
            if (a < PeriphBase || pages == null)
            {
                return null;
            }
            var e = pages[(a - PeriphBase) >> 8];
            return e != null && a >= e.Start && a < e.End ? e : null;
        }

        private Region FindRegion(uint a)
        {
            foreach (var reg in regions)
            {
                if (a >= reg.Start && a < reg.End)
                {
                    return reg;
                }
            }
            return null;
        }

        private bool Fetch(uint pc, out Avr32Insn d)
        {
            if ((pc & 1) != 0)
            {
                TakeException(EX_INSN_ADDR, pc, "instruction address", pc);
                AccountInstruction();
                d = null;
                return false;
            }
            var reg = lastCode;
            if (reg == null || pc < reg.Start || pc >= reg.End)
            {
                reg = FindRegion(pc);
                lastCode = reg;
            }
            if (reg != null)
            {
                var off = pc - reg.Start;
                d = reg.Code[off >> 1];
                if (d != null)
                {
                    return true;
                }
                var data = reg.Data;
                uint w = (uint)(data[off] << 24 | data[off + 1] << 16);
                if (Avr32Decoder.Length(w >> 16) == 4 && off + 3 < data.Length)
                {
                    w |= (uint)(data[off + 2] << 8 | data[off + 3]);
                }
                d = new Avr32Insn();
                Avr32Decoder.Decode(w, d);
                reg.Code[off >> 1] = d;
                return true;
            }
            // code outside the direct memories (e.g. the HSB RAM): decode every time
            uint hw = machine.SystemBus.ReadWord(pc, this);
            uint insn = hw << 16;
            if (Avr32Decoder.Length(hw) == 4)
            {
                insn |= machine.SystemBus.ReadWord(pc + 2, this);
            }
            d = scratch;
            Avr32Decoder.Decode(insn, d);
            return true;
        }

        private Region DataRegion(uint a)
        {
            var reg = lastData;
            if (reg != null && a >= reg.Start && a < reg.End)
            {
                return reg;
            }
            reg = FindRegion(a);
            if (reg != null)
            {
                lastData = reg;
            }
            return reg;
        }

        private uint Load32(uint a)
        {
            if ((a & 3) != 0)
            {
                throw new Avr32Exception(EX_DADDR_R, "data address (read)", a);
            }
            var reg = DataRegion(a);
            if (reg != null && a + 3 < reg.End)
            {
                var o = a - reg.Start;
                var m = reg.Data;
                return (uint)(m[o] << 24 | m[o + 1] << 16 | m[o + 2] << 8 | m[o + 3]);
            }
            BusReads++;
            var pg = Page(a);
            if (pg != null && pg.Dword != null)
            {
                return pg.Dword.ReadDoubleWord(a - pg.Start);
            }
            return machine.SystemBus.ReadDoubleWord(a, this);
        }

        private uint Load16(uint a)
        {
            if ((a & 1) != 0)
            {
                throw new Avr32Exception(EX_DADDR_R, "data address (read)", a);
            }
            var reg = DataRegion(a);
            if (reg != null)
            {
                var o = a - reg.Start;
                return (uint)(reg.Data[o] << 8 | reg.Data[o + 1]);
            }
            BusReads++;
            var pg = Page(a);
            if (pg != null && pg.Word != null)
            {
                return pg.Word.ReadWord(a - pg.Start);
            }
            return machine.SystemBus.ReadWord(a, this);
        }

        private uint Load8(uint a)
        {
            var reg = DataRegion(a);
            if (reg != null)
            {
                return reg.Data[a - reg.Start];
            }
            BusReads++;
            var pg = Page(a);
            if (pg != null && pg.Byte != null)
            {
                return pg.Byte.ReadByte(a - pg.Start);
            }
            return machine.SystemBus.ReadByte(a, this);
        }

        private void Invalidate(Region reg, uint o)
        {
            var i = o >> 1;
            reg.Code[i] = null;
            if (i > 0)
            {
                reg.Code[i - 1] = null;
            }
        }

        private void Store32(uint a, uint v)
        {
            if ((a & 3) != 0)
            {
                throw new Avr32Exception(EX_DADDR_W, "data address (write)", a);
            }
            var reg = DataRegion(a);
            if (reg != null && reg.Writable && a + 3 < reg.End)
            {
                var o = a - reg.Start;
                var m = reg.Data;
                m[o] = (byte)(v >> 24);
                m[o + 1] = (byte)(v >> 16);
                m[o + 2] = (byte)(v >> 8);
                m[o + 3] = (byte)v;
                Invalidate(reg, o);
                Invalidate(reg, o + 2);
                return;
            }
            BusWrites++;
            var pg = Page(a);
            if (pg != null && pg.Dword != null)
            {
                pg.Dword.WriteDoubleWord(a - pg.Start, v);
                return;
            }
            machine.SystemBus.WriteDoubleWord(a, v, this);
        }

        private void Store16(uint a, uint v)
        {
            if ((a & 1) != 0)
            {
                throw new Avr32Exception(EX_DADDR_W, "data address (write)", a);
            }
            var reg = DataRegion(a);
            if (reg != null && reg.Writable)
            {
                var o = a - reg.Start;
                reg.Data[o] = (byte)(v >> 8);
                reg.Data[o + 1] = (byte)v;
                Invalidate(reg, o);
                return;
            }
            BusWrites++;
            var pg = Page(a);
            if (pg != null && pg.Word != null)
            {
                pg.Word.WriteWord(a - pg.Start, (ushort)v);
                return;
            }
            machine.SystemBus.WriteWord(a, (ushort)v, this);
        }

        private void Store8(uint a, uint v)
        {
            var reg = DataRegion(a);
            if (reg != null && reg.Writable)
            {
                var o = a - reg.Start;
                reg.Data[o] = (byte)v;
                Invalidate(reg, o & ~1u);
                return;
            }
            BusWrites++;
            var pg = Page(a);
            if (pg != null && pg.Byte != null)
            {
                pg.Byte.WriteByte(a - pg.Start, (byte)v);
                return;
            }
            machine.SystemBus.WriteByte(a, (byte)v, this);
        }

        // ---------------------------------------------------------------------------------------------------------
        // System registers, COUNT/COMPARE

        private uint ReadSys(int n)
        {
            switch (n)
            {
                case 0:
                    return sr;
                case 66:
                    SyncTime();
                    return (uint)countTimer.Value;
                default:
                    return sys[n & 0xFF];
            }
        }

        private void WriteSys(int n, uint v)
        {
            switch (n)
            {
                case 0:
                    WriteSR(v);
                    break;
                case 64:
                case 65:
                    break; // CONFIG0/1 read-only
                case 66: // COUNT
                    SyncTime();
                    countTimer.Value = v;
                    ArmCompare();
                    break;
                case 67: // COMPARE: also clears a pending compare interrupt
                    SyncTime();
                    sys[67] = v;
                    Connections[0].Unset();
                    ArmCompare();
                    break;
                case 79:
                    break; // BEAR read-only
                default:
                    sys[n & 0xFF] = v;
                    break;
            }
        }

        private void ArmCompare()
        {
            var cmp = sys[67];
            if (cmp == 0)
            {
                compareTimer.Enabled = false;
                return;
            }
            var count = (uint)countTimer.Value;
            var delta = unchecked(cmp - count);
            if (delta == 0)
            {
                delta = uint.MaxValue; // equal right now: the next match is a full wrap away
            }
            compareTimer.Enabled = false;
            compareTimer.Limit = delta;
            compareTimer.Value = delta;
            compareTimer.Enabled = true;
        }

        private void OnCompareMatch()
        {
            Connections[0].Set();
            if ((sys[3] & (1u << 17)) == 0) // CPUCR.NOCOMPRES clear: COUNT restarts from 0
            {
                countTimer.Value = 0;
            }
            ArmCompare();
        }

        private void SetClock(long hz)
        {
            if (hz <= 0)
            {
                hz = 1;
            }
            if (countTimer != null && clockHz != hz)
            {
                // keep COUNT continuous across the rate change
                SyncTime();
                var c = countTimer.Value;
                countTimer.Frequency = (ulong)hz;
                countTimer.Value = c;
                compareTimer.Frequency = (ulong)hz;
                if (sys[67] != 0)
                {
                    ArmCompare();
                }
            }
            clockHz = hz;
            slotPerInsnQ16 = (ulong)((1000000000.0 * 65536.0) / hz);
            if (slotPerInsnQ16 == 0)
            {
                slotPerInsnQ16 = 1;
            }
        }

        // ---------------------------------------------------------------------------------------------------------
        // Instructions

        private static uint SE(uint v, int bits)
        {
            var s = 32 - bits;
            return (uint)((int)(v << s) >> s);
        }

        private uint Rv(int n) => r[n];

        private void Wr(int n, uint v)
        {
            if (n == 15)
            {
                Jump(v);
            }
            else
            {
                r[n] = v;
            }
        }

        private void Jump(uint target)
        {
            nextPc = target;
            jumped = true;
        }

        private uint Imm21(Avr32Insn d) => SE((uint)(d.immu << 17 | d.immm << 16 | d.imml), 21);

        private static uint Half(uint v, int top) =>
            top != 0 ? SE(v >> 16, 16) : SE(v & 0xFFFF, 16);

        private void LoadD(int rd, uint a)
        {
            // Rd+1:Rd <- *(a): the most significant word (Rd+1) is at the lower address. Word-aligned is enough.
            if ((a & 3) != 0)
            {
                throw new Avr32Exception(EX_DADDR_R, "data address (read)", a);
            }
            var hi = Load32(a);
            var lo = Load32(a + 4);
            r[rd + 1] = hi;
            r[rd] = lo;
        }

        private void StoreD(int rs, uint a)
        {
            if ((a & 3) != 0)
            {
                throw new Avr32Exception(EX_DADDR_W, "data address (write)", a);
            }
            Store32(a, r[rs + 1]);
            Store32(a + 4, r[rs]);
        }

        private void Unimplemented(Avr32Insn d, uint pc)
        {
            throw new Avr32Exception(
                EX_ILLEGAL,
                string.Format("illegal opcode {0:X8} ({1})", d.Raw, d.Op)
            );
        }

        private void Execute(Avr32Insn d, uint pc)
        {
            uint a,
                b,
                res;
            switch (d.Op)
            {
                // ---- arithmetic ----
                case Avr32Op.ABS:
                    a = r[d.rd];
                    res = (a & 0x80000000) != 0 && a != 0x80000000 ? (uint)-(int)a : a;
                    Wr(d.rd, res);
                    SetZ(res == 0);
                    break;
                case Avr32Op.ACR:
                    a = r[d.rd];
                    res = a + (sr & SR_C);
                    Wr(d.rd, res);
                    sr =
                        (sr & ~0xFu)
                        | ((~res & a) >> 31)
                        | (res == 0 && (sr & SR_Z) != 0 ? SR_Z : 0)
                        | (res >> 31) * SR_N
                        | ((res & ~a) >> 31) * SR_V;
                    break;
                case Avr32Op.ADC:
                    a = r[d.rx];
                    b = r[d.ry];
                    res = a + b + (sr & SR_C);
                    Wr(d.rd, res);
                    FlagsAdd(a, b, res, true);
                    break;
                case Avr32Op.ADD_f1:
                    a = r[d.rd];
                    b = r[d.rs];
                    res = a + b;
                    Wr(d.rd, res);
                    FlagsAdd(a, b, res);
                    break;
                case Avr32Op.ADD_f2:
                    a = r[d.rx];
                    b = r[d.ry] << d.sa;
                    res = a + b;
                    Wr(d.rd, res);
                    FlagsAdd(a, b, res);
                    break;
                case Avr32Op.ADD_cond:
                    if (Cond(d.cond))
                    {
                        Wr(d.rd, r[d.rx] + r[d.ry]);
                    }
                    break;
                case Avr32Op.ADDABS:
                    b = r[d.ry];
                    b = (b & 0x80000000) != 0 ? (uint)-(int)b : b;
                    res = r[d.rx] + b;
                    Wr(d.rd, res);
                    SetZ(res == 0);
                    break;
                case Avr32Op.ADDHHW:
                    a = Half(r[d.rx], d.x);
                    b = Half(r[d.ry], d.y);
                    res = a + b;
                    Wr(d.rd, res);
                    FlagsAdd(a, b, res);
                    break;
                case Avr32Op.SUB_rd_rs:
                    a = r[d.rd];
                    b = r[d.rs];
                    res = a - b;
                    Wr(d.rd, res);
                    FlagsSub(a, b, res);
                    break;
                case Avr32Op.SUB_rd_rx_ry_sa:
                    a = r[d.rx];
                    b = r[d.ry] << d.sa;
                    res = a - b;
                    Wr(d.rd, res);
                    FlagsSub(a, b, res);
                    break;
                case Avr32Op.SUB_rd_imm8:
                    a = r[d.rd];
                    b = d.rd == 13 ? SE((uint)d.imm8 << 2, 10) : SE((uint)d.imm8, 8);
                    res = a - b;
                    Wr(d.rd, res);
                    FlagsSub(a, b, res);
                    break;
                case Avr32Op.SUB_rd_imm21:
                    a = r[d.rd];
                    b = Imm21(d);
                    res = a - b;
                    Wr(d.rd, res);
                    FlagsSub(a, b, res);
                    break;
                case Avr32Op.SUB_rs_rd_imm:
                    a = r[d.rs];
                    b = SE((uint)d.imm16, 16);
                    res = a - b;
                    Wr(d.rd, res);
                    FlagsSub(a, b, res);
                    break;
                case Avr32Op.SUBc_f1:
                    if (Cond(d.cond4))
                    {
                        a = r[d.rd];
                        b = SE((uint)d.imm8, 8);
                        res = a - b;
                        Wr(d.rd, res);
                        if (d.f != 0)
                        {
                            FlagsSub(a, b, res);
                        }
                    }
                    break;
                case Avr32Op.SUBc_f2:
                    if (Cond(d.cond4))
                    {
                        Wr(d.rd, r[d.rx] - r[d.ry]);
                    }
                    break;
                case Avr32Op.RSUB_f1:
                    a = r[d.rs];
                    b = r[d.rd];
                    res = a - b;
                    Wr(d.rd, res);
                    FlagsSub(a, b, res);
                    break;
                case Avr32Op.RSUB_f2:
                    a = SE((uint)d.imm8, 8);
                    b = r[d.rs];
                    res = a - b;
                    Wr(d.rd, res);
                    FlagsSub(a, b, res);
                    break;
                case Avr32Op.RSUBc:
                    if (Cond(d.cond4))
                    {
                        Wr(d.rd, SE((uint)d.imm8, 8) - r[d.rd]);
                    }
                    break;
                case Avr32Op.SBC:
                    a = r[d.rx];
                    b = r[d.ry];
                    res = a - b - (sr & SR_C);
                    Wr(d.rd, res);
                    FlagsSub(a, b, res, true);
                    break;
                case Avr32Op.SCR:
                    a = r[d.rd];
                    res = a - (sr & SR_C);
                    Wr(d.rd, res);
                    sr =
                        (sr & ~0xFu)
                        | ((~a & res) >> 31)
                        | (res == 0 && (sr & SR_Z) != 0 ? SR_Z : 0)
                        | (res >> 31) * SR_N
                        | ((a & ~res) >> 31) * SR_V;
                    break;
                case Avr32Op.NEG:
                    a = r[d.rd];
                    res = 0 - a;
                    Wr(d.rd, res);
                    sr =
                        (sr & ~0xFu)
                        | ((a | res) >> 31)
                        | (res == 0 ? SR_Z : 0)
                        | (res >> 31) * SR_N
                        | ((a & res) >> 31) * SR_V;
                    break;
                case Avr32Op.CPW_f1:
                    a = r[d.rd];
                    b = r[d.rs];
                    FlagsSub(a, b, a - b);
                    break;
                case Avr32Op.CPW_f2:
                    a = r[d.rd];
                    b = SE((uint)d.imm6, 6);
                    FlagsSub(a, b, a - b);
                    break;
                case Avr32Op.CPW_f3:
                    a = r[d.rd];
                    b = Imm21(d);
                    FlagsSub(a, b, a - b);
                    break;
                case Avr32Op.CPH:
                    a = r[d.rd] << 16;
                    b = r[d.rs] << 16;
                    FlagsSub(a, b, a - b); // the 16-bit compare done in the top half: same flags
                    break;
                case Avr32Op.CPB:
                    a = r[d.rd] << 24;
                    b = r[d.rs] << 24;
                    FlagsSub(a, b, a - b);
                    break;
                case Avr32Op.CPC_f1:
                    a = r[d.rd];
                    b = r[d.rs];
                    FlagsSub(a, b, a - b - (sr & SR_C), true);
                    break;
                case Avr32Op.CPC_f2:
                    a = r[d.rd];
                    FlagsSub(a, 0, a - (sr & SR_C), true);
                    break;
                case Avr32Op.TST:
                    SetNZ(r[d.rd] & r[d.rs]);
                    break;
                case Avr32Op.MAX:
                    Wr(d.rd, (int)r[d.rx] > (int)r[d.ry] ? r[d.rx] : r[d.ry]);
                    break;
                case Avr32Op.MIN:
                    Wr(d.rd, (int)r[d.rx] < (int)r[d.ry] ? r[d.rx] : r[d.ry]);
                    break;
                case Avr32Op.SATU:
                case Avr32Op.SATRNDU:
                    {
                        var t = (uint)((int)r[d.rd] >> d.sa5);
                        if (d.Op == Avr32Op.SATRNDU && d.sa5 != 0)
                            t += (r[d.rd] >> (d.sa5 - 1)) & 1; // rounding: the last bit shifted out
                        var bp = d.bp5;
                        if (bp != 0 && (bp >= 32 ? false : (t >> bp) != 0))
                        {
                            t = (t & 0x80000000) != 0 ? 0 : (1u << bp) - 1;
                            sr |= SR_Q;
                        }
                        Wr(d.rd, t);
                    }
                    break;
                case Avr32Op.SATS:
                case Avr32Op.SATRNDS: // GCC's (x + 2^(sa-1)) >> sa (mcu/src/usbaudio.c host_step)
                    {
                        var t = (int)r[d.rd] >> d.sa5;
                        if (d.Op == Avr32Op.SATRNDS && d.sa5 != 0)
                            t += (int)((r[d.rd] >> (d.sa5 - 1)) & 1);
                        var bp = d.bp5;
                        if (bp != 0)
                        {
                            int hi = (1 << (bp - 1)) - 1,
                                lo = -(1 << (bp - 1));
                            if (t > hi || t < lo)
                            {
                                t = t < 0 ? lo : hi;
                                sr |= SR_Q;
                            }
                        }
                        Wr(d.rd, (uint)t);
                    }
                    break;

                // ---- multiply / divide ----
                case Avr32Op.MUL_f1:
                    Wr(d.rd, r[d.rd] * r[d.rs]);
                    break;
                case Avr32Op.MUL_f2:
                    Wr(d.rd, r[d.rx] * r[d.ry]);
                    break;
                case Avr32Op.MUL_f3:
                    Wr(d.rd, r[d.rs] * SE((uint)d.imm8, 8));
                    break;
                case Avr32Op.MULHHW:
                    Wr(d.rd, Half(r[d.rx], d.x) * Half(r[d.ry], d.y));
                    break;
                case Avr32Op.MULSD:
                    {
                        var p = (long)(int)r[d.rx] * (int)r[d.ry];
                        r[d.rd] = (uint)p;
                        r[d.rd + 1] = (uint)(p >> 32);
                    }
                    break;
                case Avr32Op.MULUD:
                    {
                        var p = (ulong)r[d.rx] * r[d.ry];
                        r[d.rd] = (uint)p;
                        r[d.rd + 1] = (uint)(p >> 32);
                    }
                    break;
                case Avr32Op.MAC:
                    Wr(d.rd, r[d.rx] * r[d.ry] + r[d.rd]);
                    break;
                case Avr32Op.MACHHW:
                    Wr(d.rd, Half(r[d.rx], d.x) * Half(r[d.ry], d.y) + r[d.rd]);
                    break;
                case Avr32Op.MACHHD:
                    {
                        var p = (uint)(Half(r[d.rx], d.x) * Half(r[d.ry], d.y));
                        var acc = ((ulong)r[d.rd + 1] << 32 | r[d.rd]) >> 16; // the 48 accumulator bits
                        acc = (acc + (ulong)(long)(int)p) << 16;
                        r[d.rd] = (uint)acc & 0xFFFF0000u;
                        r[d.rd + 1] = (uint)(acc >> 32);
                    }
                    break;
                case Avr32Op.MACSD:
                    {
                        var acc = (long)((ulong)r[d.rd + 1] << 32 | r[d.rd]);
                        acc += (long)(int)r[d.rx] * (int)r[d.ry];
                        r[d.rd] = (uint)acc;
                        r[d.rd + 1] = (uint)((ulong)acc >> 32);
                    }
                    break;
                case Avr32Op.MACUD:
                    {
                        var acc = (ulong)r[d.rd + 1] << 32 | r[d.rd];
                        acc += (ulong)r[d.rx] * r[d.ry];
                        r[d.rd] = (uint)acc;
                        r[d.rd + 1] = (uint)(acc >> 32);
                    }
                    break;
                case Avr32Op.DIVS:
                    {
                        var x = (int)r[d.rx];
                        var y = (int)r[d.ry];
                        if (y == 0)
                        {
                            r[d.rd] = 0; // UNDEFINED on hardware
                            r[d.rd + 1] = (uint)x;
                        }
                        else if (x == int.MinValue && y == -1)
                        {
                            r[d.rd] = (uint)x;
                            r[d.rd + 1] = 0;
                        }
                        else
                        {
                            r[d.rd] = (uint)(x / y);
                            r[d.rd + 1] = (uint)(x % y);
                        }
                    }
                    break;
                case Avr32Op.DIVU:
                    {
                        var x = r[d.rx];
                        var y = r[d.ry];
                        r[d.rd] = y == 0 ? 0 : x / y;
                        r[d.rd + 1] = y == 0 ? x : x % y;
                    }
                    break;

                // ---- logic ----
                case Avr32Op.AND_f1:
                    res = r[d.rd] & r[d.rs];
                    Wr(d.rd, res);
                    SetNZ(res);
                    break;
                case Avr32Op.AND_f2:
                    res = r[d.rx] & (r[d.ry] << d.sa5);
                    Wr(d.rd, res);
                    SetNZ(res);
                    break;
                case Avr32Op.AND_f3:
                    res = r[d.rx] & (r[d.ry] >> d.sa5);
                    Wr(d.rd, res);
                    SetNZ(res);
                    break;
                case Avr32Op.AND_cond:
                    if (Cond(d.cond))
                    {
                        Wr(d.rd, r[d.rx] & r[d.ry]);
                    }
                    break;
                case Avr32Op.ANDN:
                    res = r[d.rd] & ~r[d.rs];
                    Wr(d.rd, res);
                    SetNZ(res);
                    break;
                case Avr32Op.ANDH:
                    res = r[d.rd] & ((uint)d.imm << 16 | 0xFFFF);
                    if (d.coh != 0)
                    {
                        res &= 0xFFFF0000;
                    }
                    Wr(d.rd, res);
                    SetNZ(res);
                    break;
                case Avr32Op.ANDL:
                    res = r[d.rd] & ((uint)d.imm | 0xFFFF0000);
                    if (d.coh != 0)
                    {
                        res &= 0xFFFF;
                    }
                    Wr(d.rd, res);
                    SetNZ(res);
                    break;
                case Avr32Op.OR_f1:
                    res = r[d.rd] | r[d.rs];
                    Wr(d.rd, res);
                    SetNZ(res);
                    break;
                case Avr32Op.OR_f2:
                    res = r[d.rx] | (r[d.ry] << d.sa5);
                    Wr(d.rd, res);
                    SetNZ(res);
                    break;
                case Avr32Op.OR_f3:
                    res = r[d.rx] | (r[d.ry] >> d.sa5);
                    Wr(d.rd, res);
                    SetNZ(res);
                    break;
                case Avr32Op.OR_cond:
                    if (Cond(d.cond4))
                    {
                        Wr(d.rd, r[d.rx] | r[d.ry]);
                    }
                    break;
                case Avr32Op.ORH:
                    res = r[d.rd] | (uint)d.imm16 << 16;
                    Wr(d.rd, res);
                    SetNZ(res);
                    break;
                case Avr32Op.ORL:
                    res = r[d.rd] | (uint)d.imm16;
                    Wr(d.rd, res);
                    SetNZ(res);
                    break;
                case Avr32Op.EOR_f1:
                    res = r[d.rd] ^ r[d.rs];
                    Wr(d.rd, res);
                    SetNZ(res);
                    break;
                case Avr32Op.EOR_f2:
                    res = r[d.rx] ^ (r[d.ry] << d.sa5);
                    Wr(d.rd, res);
                    SetNZ(res);
                    break;
                case Avr32Op.EOR_f3:
                    res = r[d.rx] ^ (r[d.ry] >> d.sa5);
                    Wr(d.rd, res);
                    SetNZ(res);
                    break;
                case Avr32Op.EOR_cond:
                    if (Cond(d.cond))
                    {
                        Wr(d.rd, r[d.rx] ^ r[d.ry]);
                    }
                    break;
                case Avr32Op.EORH:
                    res = r[d.rd] ^ (uint)d.imm16 << 16;
                    Wr(d.rd, res);
                    SetNZ(res);
                    break;
                case Avr32Op.EORL:
                    res = r[d.rd] ^ (uint)d.imm16;
                    Wr(d.rd, res);
                    SetNZ(res);
                    break;
                case Avr32Op.COM:
                    res = ~r[d.rd];
                    Wr(d.rd, res);
                    SetZ(res == 0);
                    break;
                case Avr32Op.TNBZ:
                    a = r[d.rd];
                    SetZ(
                        (a & 0xFF000000) == 0
                            || (a & 0xFF0000) == 0
                            || (a & 0xFF00) == 0
                            || (a & 0xFF) == 0
                    );
                    break;

                // ---- shifts ----
                case Avr32Op.ASR_f1:
                    Asr(d.rd, r[d.rx], (int)(r[d.ry] & 31));
                    break;
                case Avr32Op.ASR_f2:
                    Asr(d.rd, r[d.rd], d.bp4 << 1 | d.bp1);
                    break;
                case Avr32Op.ASR_f3:
                    Asr(d.rd, r[d.rs], d.sa5);
                    break;
                case Avr32Op.LSL_f1:
                    Lsl(d.rd, r[d.rx], (int)(r[d.ry] & 31));
                    break;
                case Avr32Op.LSL_f2:
                    Lsl(d.rd, r[d.rd], d.bp4 << 1 | d.bp1);
                    break;
                case Avr32Op.LSL_f3:
                    Lsl(d.rd, r[d.rs], d.sa5);
                    break;
                case Avr32Op.LSR_f1:
                    Lsr(d.rd, r[d.rx], (int)(r[d.ry] & 31));
                    break;
                case Avr32Op.LSR_f2:
                    Lsr(d.rd, r[d.rd], d.bp4 << 1 | d.bp1);
                    break;
                case Avr32Op.LSR_f3:
                    Lsr(d.rd, r[d.rs], d.sa5);
                    break;
                case Avr32Op.ROL:
                    a = r[d.rd];
                    res = a << 1 | (sr & SR_C);
                    Wr(d.rd, res);
                    SetNZ(res);
                    SetC((a >> 31) != 0);
                    break;
                case Avr32Op.ROR:
                    a = r[d.rd];
                    res = a >> 1 | (sr & SR_C) << 31;
                    Wr(d.rd, res);
                    SetNZ(res);
                    SetC((a & 1) != 0);
                    break;

                // ---- bit operations ----
                case Avr32Op.BFEXTU:
                    res = d.w5 == 0 ? 0 : (r[d.rs] >> d.bp5) & (uint)((1ul << d.w5) - 1);
                    Wr(d.rd, res);
                    SetNZ(res);
                    SetC((res >> 31) != 0);
                    break;
                case Avr32Op.BFEXTS:
                    res = d.w5 == 0 ? 0 : SE((r[d.rs] >> d.bp5) & (uint)((1ul << d.w5) - 1), d.w5);
                    Wr(d.rd, res);
                    SetNZ(res);
                    SetC((res >> 31) != 0);
                    break;
                case Avr32Op.BFINS:
                    {
                        var mask = (uint)((1ul << d.w5) - 1) << d.bp5;
                        res = (r[d.rd] & ~mask) | ((r[d.rs] << d.bp5) & mask);
                        Wr(d.rd, res);
                        SetNZ(res);
                        SetC((res >> 31) != 0);
                    }
                    break;
                case Avr32Op.BLD:
                    {
                        var bit = ((r[d.rd] >> d.bp5) & 1) != 0;
                        SetC(bit);
                        SetZ(bit);
                    }
                    break;
                case Avr32Op.BST:
                    res = (r[d.rd] & ~(1u << d.bp5)) | (sr & SR_C) << d.bp5;
                    Wr(d.rd, res);
                    break;
                case Avr32Op.CBR:
                    res = r[d.rd] & ~(1u << (d.bp4 << 1 | d.bp1));
                    Wr(d.rd, res);
                    SetZ(res == 0);
                    break;
                case Avr32Op.SBR:
                    res = r[d.rd] | 1u << (d.bp4 << 1 | d.bp1);
                    Wr(d.rd, res);
                    SetZ(false);
                    break;
                case Avr32Op.BREV_r:
                    {
                        var v = r[d.rd];
                        v = (v >> 1 & 0x55555555) | (v & 0x55555555) << 1;
                        v = (v >> 2 & 0x33333333) | (v & 0x33333333) << 2;
                        v = (v >> 4 & 0x0F0F0F0F) | (v & 0x0F0F0F0F) << 4;
                        v = (v >> 8 & 0x00FF00FF) | (v & 0x00FF00FF) << 8;
                        v = v >> 16 | v << 16;
                        Wr(d.rd, v);
                        SetZ(v == 0);
                    }
                    break;
                case Avr32Op.CLZ:
                    {
                        var v = r[d.rs];
                        var n = 0u;
                        while (n < 32 && (v & 0x80000000) == 0)
                        {
                            v <<= 1;
                            n++;
                        }
                        Wr(d.rd, n);
                        SetZ(n == 0);
                        SetC(n == 32);
                    }
                    break;
                case Avr32Op.CASTSB:
                    res = SE(r[d.rd] & 0xFF, 8);
                    Wr(d.rd, res);
                    SetNZ(res);
                    SetC((res >> 31) != 0);
                    break;
                case Avr32Op.CASTSH:
                    res = SE(r[d.rd] & 0xFFFF, 16);
                    Wr(d.rd, res);
                    SetNZ(res);
                    SetC((res >> 31) != 0);
                    break;
                case Avr32Op.CASTUB:
                    res = r[d.rd] & 0xFF;
                    Wr(d.rd, res);
                    SetNZ(res);
                    SetC(false);
                    break;
                case Avr32Op.CASTUH:
                    res = r[d.rd] & 0xFFFF;
                    Wr(d.rd, res);
                    SetNZ(res);
                    SetC(false);
                    break;
                case Avr32Op.SWAP_B:
                    a = r[d.rd];
                    Wr(d.rd, a >> 24 | (a >> 8 & 0xFF00) | (a << 8 & 0xFF0000) | a << 24);
                    break;
                case Avr32Op.SWAP_BH:
                    a = r[d.rd];
                    Wr(d.rd, (a >> 8 & 0x00FF00FF) | (a << 8 & 0xFF00FF00));
                    break;
                case Avr32Op.SWAP_H:
                    a = r[d.rd];
                    Wr(d.rd, a >> 16 | a << 16);
                    break;

                // ---- moves ----
                case Avr32Op.MOV_f1:
                    Wr(d.rd, SE((uint)d.imm8, 8));
                    break;
                case Avr32Op.MOV_f2:
                    Wr(d.rd, Imm21(d));
                    break;
                case Avr32Op.MOV_f3:
                    Wr(d.rd, r[d.rs]);
                    break;
                case Avr32Op.MOVc_f1:
                    if (Cond(d.cond4))
                    {
                        Wr(d.rd, r[d.rs]);
                    }
                    break;
                case Avr32Op.MOVc_f2:
                    if (Cond(d.cond4))
                    {
                        Wr(d.rd, SE((uint)d.imm8, 8));
                    }
                    break;
                case Avr32Op.MOVH:
                    Wr(d.rd, (uint)d.imm16 << 16);
                    break;
                case Avr32Op.SR:
                    Wr(d.rd, Cond(d.cond4) ? 1u : 0u);
                    break;

                // ---- status register ----
                case Avr32Op.CSRF:
                    if (d.bp5 > 15 && !Privileged)
                    {
                        throw new Avr32Exception(EX_PRIV, "privilege violation (csrf)");
                    }
                    WriteSR(sr & ~(1u << d.bp5));
                    break;
                case Avr32Op.SSRF:
                    if (d.bp5 > 15 && !Privileged)
                    {
                        throw new Avr32Exception(EX_PRIV, "privilege violation (ssrf)");
                    }
                    WriteSR(sr | 1u << d.bp5);
                    break;
                case Avr32Op.CSRFCZ:
                    {
                        var bit = ((sr >> d.bp5) & 1) != 0;
                        SetC(bit);
                        SetZ(bit);
                    }
                    break;
                case Avr32Op.MUSFR:
                    sr = (sr & ~0xFu) | (r[d.rs] & 0xF);
                    break;
                case Avr32Op.MUSTR:
                    Wr(d.rd, sr & 0xF);
                    break;
                case Avr32Op.MFSR:
                    if (!Privileged)
                    {
                        throw new Avr32Exception(EX_PRIV, "privilege violation (mfsr)");
                    }
                    Wr(d.rd, ReadSys(d.sr));
                    break;
                case Avr32Op.MTSR:
                    if (!Privileged)
                    {
                        throw new Avr32Exception(EX_PRIV, "privilege violation (mtsr)");
                    }
                    WriteSys(d.sr, r[d.rs]);
                    break;
                case Avr32Op.MFDR:
                    Wr(d.rs, d.addr == 0 ? DebugDID : 0);
                    break;
                case Avr32Op.MTDR:
                    break; // OCD registers: not modelled

                // ---- branches, calls ----
                case Avr32Op.BR_f1:
                    if (Cond(d.rd & 7))
                    {
                        Jump(pc + (SE((uint)d.disp, 8) << 1));
                    }
                    break;
                case Avr32Op.BR_f2:
                    if (Cond(d.cond))
                    {
                        Jump(pc + (SE((uint)(d.disp2 << 17 | d.disp1 << 16 | d.disp0), 21) << 1));
                    }
                    break;
                case Avr32Op.RJMP:
                    Jump(pc + (SE((uint)(d.disp2 << 8 | d.disp8), 10) << 1));
                    break;
                case Avr32Op.RCALL_f1:
                    r[14] = pc + 2;
                    Jump(pc + (SE((uint)(d.disp2 << 8 | d.disp8), 10) << 1));
                    break;
                case Avr32Op.RCALL_f2:
                    r[14] = pc + 4;
                    Jump(pc + (Imm21(d) << 1));
                    break;
                case Avr32Op.ICALL:
                    a = r[d.rd];
                    r[14] = pc + 2;
                    Jump(a);
                    break;
                case Avr32Op.MCALL:
                    a = Load32((r[d.rp] & ~3u) + (SE((uint)d.disp, 16) << 2));
                    r[14] = pc + 4;
                    Jump(a);
                    break;
                case Avr32Op.ACALL:
                    a = Load32(sys[2] + ((uint)d.disp << 2));
                    r[14] = pc + 2;
                    Jump(a);
                    break;
                case Avr32Op.RET:
                    if (Cond(d.cond4))
                    {
                        var rs = d.rd;
                        r[12] =
                            rs == 14 ? 0xFFFFFFFF
                            : rs == 13 ? 0
                            : rs == 15 ? 1
                            : r[rs];
                        FlagsRet();
                        Jump(r[14]);
                    }
                    break;
                case Avr32Op.RETE:
                    Rete();
                    break;
                case Avr32Op.RETS:
                    Rets(pc);
                    break;
                case Avr32Op.SCALL:
                    if (Mode <= 1)
                    {
                        var old = sr;
                        WriteSR((sr & ~SR_MODE) | (1u << 22));
                        PushSys(pc + 2);
                        PushSys(old);
                        Jump(sys[1] + 0x100);
                    }
                    else
                    {
                        r[14] = pc + 2;
                        Jump(sys[1] + 0x100);
                    }
                    break;
                case Avr32Op.BREAKPOINT:
                    break; // no OCD: executes as nop
                case Avr32Op.SLEEP:
                    if (!Privileged)
                    {
                        throw new Avr32Exception(EX_PRIV, "privilege violation (sleep)");
                    }
                    if ((d.op8 & 0x80) != 0)
                    {
                        sr &= ~SR_GM;
                    }
                    sleeping = true;
                    SleepMode = d.op8 & 0x7F;
                    SleepEntered?.Invoke(SleepMode);
                    break;
                case Avr32Op.NOP:
                case Avr32Op.FRS:
                case Avr32Op.CACHE:
                    break;
                case Avr32Op.COP:
                    throw new Avr32Exception(EX_UNIMPL, "coprocessor absent");

                // ---- loads ----
                case Avr32Op.LDW_f1:
                    a = r[d.rp];
                    b = Load32(a);
                    if (d.rp != d.rd)
                    {
                        r[d.rp] = a + 4;
                    }
                    Wr(d.rd, b);
                    break;
                case Avr32Op.LDW_f2:
                    a = r[d.rp] - 4;
                    b = Load32(a);
                    r[d.rp] = a;
                    Wr(d.rd, b);
                    break;
                case Avr32Op.LDW_f3:
                    Wr(d.rd, Load32(r[d.rp] + ((uint)d.disp5 << 2)));
                    break;
                case Avr32Op.LDW_f4:
                    Wr(d.rd, Load32(r[d.rp] + SE((uint)d.disp16, 16)));
                    break;
                case Avr32Op.LDW_f5:
                    Wr(d.rd, Load32(r[d.rx] + (r[d.ry] << d.sa)));
                    break;
                case Avr32Op.LDW_f6:
                    Wr(d.rd, Load32(r[d.rx] + (((r[d.ry] >> (8 * (d.x << 1 | d.y))) & 0xFF) << 2)));
                    break;
                case Avr32Op.LDW_cond:
                    if (Cond(d.cond4))
                    {
                        Wr(d.rd, Load32(r[d.rp] + ((uint)d.disp9 << 2)));
                    }
                    break;
                case Avr32Op.LDDPC:
                    Wr(d.rd, Load32((pc & ~3u) + ((uint)d.disp << 2)));
                    break;
                case Avr32Op.LDDSP:
                    Wr(d.rd, Load32((r[13] & ~3u) + ((uint)d.disp << 2)));
                    break;
                case Avr32Op.LDUH_f1:
                    a = r[d.rp];
                    b = Load16(a);
                    r[d.rp] = a + 2;
                    Wr(d.rd, b);
                    break;
                case Avr32Op.LDUH_f2:
                    a = r[d.rp] - 2;
                    b = Load16(a);
                    r[d.rp] = a;
                    Wr(d.rd, b);
                    break;
                case Avr32Op.LDUH_f3:
                    Wr(d.rd, Load16(r[d.rp] + ((uint)d.disp3 << 1)));
                    break;
                case Avr32Op.LDUH_f4:
                    Wr(d.rd, Load16(r[d.rp] + SE((uint)d.disp16, 16)));
                    break;
                case Avr32Op.LDUH_f5:
                    Wr(d.rd, Load16(r[d.rx] + (r[d.ry] << d.sa)));
                    break;
                case Avr32Op.LDUH_cond:
                    if (Cond(d.cond4))
                    {
                        Wr(d.rd, Load16(r[d.rp] + ((uint)d.disp9 << 1)));
                    }
                    break;
                case Avr32Op.LDSH_f1:
                    a = r[d.rp];
                    b = SE(Load16(a), 16);
                    r[d.rp] = a + 2;
                    Wr(d.rd, b);
                    break;
                case Avr32Op.LDSH_f2:
                    a = r[d.rp] - 2;
                    b = SE(Load16(a), 16);
                    r[d.rp] = a;
                    Wr(d.rd, b);
                    break;
                case Avr32Op.LDSH_f3:
                    Wr(d.rd, SE(Load16(r[d.rp] + ((uint)d.disp3 << 1)), 16));
                    break;
                case Avr32Op.LDSH_f4:
                    Wr(d.rd, SE(Load16(r[d.rp] + SE((uint)d.disp16, 16)), 16));
                    break;
                case Avr32Op.LDSH_f5:
                    Wr(d.rd, SE(Load16(r[d.rx] + (r[d.ry] << d.sa)), 16));
                    break;
                case Avr32Op.LDSH_cond:
                    if (Cond(d.cond4))
                    {
                        Wr(d.rd, SE(Load16(r[d.rp] + ((uint)d.disp9 << 1)), 16));
                    }
                    break;
                case Avr32Op.LDub_f1:
                    a = r[d.rp];
                    b = Load8(a);
                    r[d.rp] = a + 1;
                    Wr(d.rd, b);
                    break;
                case Avr32Op.LDub_f2:
                    a = r[d.rp] - 1;
                    b = Load8(a);
                    r[d.rp] = a;
                    Wr(d.rd, b);
                    break;
                case Avr32Op.LDub_f3:
                    Wr(d.rd, Load8(r[d.rp] + (uint)d.disp3));
                    break;
                case Avr32Op.LDUB_f4:
                    Wr(d.rd, Load8(r[d.rp] + SE((uint)d.disp16, 16)));
                    break;
                case Avr32Op.LDub_f5:
                    Wr(d.rd, Load8(r[d.rx] + (r[d.ry] << d.sa)));
                    break;
                case Avr32Op.LDUB_cond:
                    if (Cond(d.cond4))
                    {
                        Wr(d.rd, Load8(r[d.rp] + (uint)d.disp9));
                    }
                    break;
                case Avr32Op.LDsb_f1:
                    Wr(d.rd, SE(Load8(r[d.rp] + SE((uint)d.disp16, 16)), 8));
                    break;
                case Avr32Op.LDsb_f2:
                    Wr(d.rd, SE(Load8(r[d.rx] + (r[d.ry] << d.sa)), 8));
                    break;
                case Avr32Op.LDsb_cond:
                    if (Cond(d.cond4))
                    {
                        Wr(d.rd, SE(Load8(r[d.rp] + (uint)d.disp9), 8));
                    }
                    break;
                case Avr32Op.LDD_f1:
                    a = r[d.rp];
                    LoadD(d.rd * 2, a);
                    r[d.rp] = a + 8;
                    break;
                case Avr32Op.LDD_f2:
                    a = r[d.rp] - 8;
                    LoadD(d.rd * 2, a);
                    r[d.rp] = a;
                    break;
                case Avr32Op.LDD_f3:
                    LoadD(d.rd * 2, r[d.rp]);
                    break;
                case Avr32Op.LDD_f4:
                    LoadD(d.rs * 2, r[d.rp] + SE((uint)d.disp16, 16));
                    break;
                case Avr32Op.LDD_f5:
                    LoadD(d.rd & ~1, r[d.rx] + (r[d.ry] << d.sa));
                    break;
                case Avr32Op.LDINSB:
                    {
                        var v = Load8(r[d.rp] + SE((uint)d.disp12, 12));
                        var sh = 8 * d.part;
                        Wr(d.rd, (r[d.rd] & ~(0xFFu << sh)) | v << sh);
                    }
                    break;
                case Avr32Op.LDINSH:
                    {
                        var v = Load16(r[d.rp] + (SE((uint)d.disp12, 12) << 1));
                        var sh = 16 * d.part;
                        Wr(d.rd, (r[d.rd] & ~(0xFFFFu << sh)) | v << sh);
                    }
                    break;
                case Avr32Op.LDSWPSH:
                    {
                        var v = Load16(r[d.rp] + (SE((uint)d.disp12, 12) << 1));
                        Wr(d.rd, SE((v >> 8 | v << 8) & 0xFFFF, 16));
                    }
                    break;
                case Avr32Op.LDSWPUH:
                    {
                        var v = Load16(r[d.rp] + (SE((uint)d.disp12, 12) << 1));
                        Wr(d.rd, (v >> 8 | v << 8) & 0xFFFF);
                    }
                    break;
                case Avr32Op.LDSWPW:
                    {
                        var v = Load32(r[d.rp] + (SE((uint)d.disp12, 12) << 2));
                        Wr(d.rd, v >> 24 | (v >> 8 & 0xFF00) | (v << 8 & 0xFF0000) | v << 24);
                    }
                    break;
                case Avr32Op.LDM:
                    Ldm(d);
                    break;
                case Avr32Op.LDMTS:
                    {
                        var addr = r[d.rp];
                        for (var i = 0; i <= 14; i++)
                        {
                            if ((d.list >> i & 1) != 0)
                            {
                                var v = Load32(addr);
                                addr += 4;
                                if (i == 13)
                                {
                                    if (Mode == 0)
                                    {
                                        r[13] = v;
                                    }
                                    else
                                    {
                                        spApp = v;
                                    }
                                }
                                else
                                {
                                    r[i] = v;
                                }
                            }
                        }
                        if (d.op != 0)
                        {
                            r[d.rp] = addr;
                        }
                    }
                    break;
                case Avr32Op.POPM:
                    Popm(d);
                    break;

                // ---- stores ----
                case Avr32Op.STW_f1:
                    a = r[d.rp];
                    Store32(a, r[d.rs]);
                    r[d.rp] = a + 4;
                    break;
                case Avr32Op.STW_f2:
                    a = r[d.rp] - 4;
                    Store32(a, r[d.rs]);
                    r[d.rp] = a;
                    break;
                case Avr32Op.STW_f3:
                    Store32(r[d.rp] + ((uint)d.disp4 << 2), r[d.rs]);
                    break;
                case Avr32Op.STW_f4:
                    Store32(r[d.rp] + SE((uint)d.imm16, 16), r[d.rs]);
                    break;
                case Avr32Op.STW_f5:
                    Store32(r[d.rx] + (r[d.ry] << d.sa), r[d.rd]);
                    break;
                case Avr32Op.STWcond:
                    if (Cond(d.cond4))
                    {
                        Store32(r[d.rp] + ((uint)d.disp9 << 2), r[d.rd]);
                    }
                    break;
                case Avr32Op.STDSP:
                    Store32((r[13] & ~3u) + ((uint)d.disp << 2), r[d.rd]);
                    break;
                case Avr32Op.STH_f1:
                    a = r[d.rp];
                    Store16(a, r[d.rs]);
                    r[d.rp] = a + 2;
                    break;
                case Avr32Op.STH_f2:
                    a = r[d.rp] - 2;
                    Store16(a, r[d.rs]);
                    r[d.rp] = a;
                    break;
                case Avr32Op.STH_f3:
                    Store16(r[d.rp] + ((uint)d.disp3 << 1), r[d.rd]);
                    break;
                case Avr32Op.STH_f4:
                    Store16(r[d.rp] + SE((uint)d.imm16, 16), r[d.rs]);
                    break;
                case Avr32Op.STH_f5:
                    Store16(r[d.rx] + (r[d.ry] << d.sa), r[d.rd]);
                    break;
                case Avr32Op.STHc:
                    if (Cond(d.cond4))
                    {
                        Store16(r[d.rp] + ((uint)d.disp9 << 1), r[d.rd]);
                    }
                    break;
                case Avr32Op.STB_f1:
                    a = r[d.rp];
                    Store8(a, r[d.rs]);
                    r[d.rp] = a + 1;
                    break;
                case Avr32Op.STB_f2:
                    a = r[d.rp] - 1;
                    Store8(a, r[d.rs]);
                    r[d.rp] = a;
                    break;
                case Avr32Op.STB_f3:
                    Store8(r[d.rp] + (uint)d.disp3, r[d.rd]);
                    break;
                case Avr32Op.STB_f4:
                    Store8(r[d.rp] + SE((uint)d.imm16, 16), r[d.rs]);
                    break;
                case Avr32Op.STB_f5:
                    Store8(r[d.rx] + (r[d.ry] << d.sa), r[d.rd]);
                    break;
                case Avr32Op.STBc:
                    if (Cond(d.cond4))
                    {
                        Store8(r[d.rp] + (uint)d.disp9, r[d.rd]);
                    }
                    break;
                case Avr32Op.STD_f1:
                    a = r[d.rp];
                    StoreD(d.rs * 2, a);
                    r[d.rp] = a + 8;
                    break;
                case Avr32Op.STD_f2:
                    a = r[d.rp] - 8;
                    StoreD(d.rs * 2, a);
                    r[d.rp] = a;
                    break;
                case Avr32Op.STD_f3:
                    StoreD(d.rs * 2, r[d.rp]);
                    break;
                case Avr32Op.STD_f4:
                    StoreD(d.rs * 2, r[d.rp] + SE((uint)d.disp16, 16));
                    break;
                case Avr32Op.STM:
                    Stm(d);
                    break;
                case Avr32Op.PUSHM:
                    Pushm(d);
                    break;

                // ---- read-modify-write on memory ----
                case Avr32Op.MEMC:
                case Avr32Op.MEMS_bp5_imm15:
                case Avr32Op.MEMT_bp5_imm15:
                    {
                        var addr = SE((uint)d.imm15 << 2, 17);
                        var v = Load32(addr);
                        var m = 1u << d.bp5;
                        v =
                            d.Op == Avr32Op.MEMC ? v & ~m
                            : d.Op == Avr32Op.MEMS_bp5_imm15 ? v | m
                            : v ^ m;
                        Store32(addr, v);
                    }
                    break;

                default:
                    Unimplemented(d, pc);
                    break;
            }
        }

        private void Asr(int rd, uint op, int sh)
        {
            var res = (uint)((int)op >> sh);
            Wr(rd, res);
            SetNZ(res);
            SetC(sh != 0 && ((op >> (sh - 1)) & 1) != 0);
        }

        private void Lsl(int rd, uint op, int sh)
        {
            var res = op << sh;
            Wr(rd, res);
            SetNZ(res);
            SetC(sh != 0 && ((op >> (32 - sh)) & 1) != 0);
        }

        private void Lsr(int rd, uint op, int sh)
        {
            var res = op >> sh;
            Wr(rd, res);
            SetNZ(res);
            SetC(sh != 0 && ((op >> (sh - 1)) & 1) != 0);
        }

        private void Pushm(Avr32Insn d)
        {
            // list bits: 0 R0-R3, 1 R4-R7, 2 R8-R9, 3 R10, 4 R11, 5 R12, 6 LR, 7 PC
            var l = d.list;
            var sp = r[13];
            if ((l & 1) != 0)
            {
                Store32(sp -= 4, r[0]);
                Store32(sp -= 4, r[1]);
                Store32(sp -= 4, r[2]);
                Store32(sp -= 4, r[3]);
            }
            if ((l & 2) != 0)
            {
                Store32(sp -= 4, r[4]);
                Store32(sp -= 4, r[5]);
                Store32(sp -= 4, r[6]);
                Store32(sp -= 4, r[7]);
            }
            if ((l & 4) != 0)
            {
                Store32(sp -= 4, r[8]);
                Store32(sp -= 4, r[9]);
            }
            if ((l & 8) != 0)
            {
                Store32(sp -= 4, r[10]);
            }
            if ((l & 16) != 0)
            {
                Store32(sp -= 4, r[11]);
            }
            if ((l & 32) != 0)
            {
                Store32(sp -= 4, r[12]);
            }
            if ((l & 64) != 0)
            {
                Store32(sp -= 4, r[14]);
            }
            if ((l & 128) != 0)
            {
                Store32(sp -= 4, r[15]);
            }
            r[13] = sp;
        }

        private void Popm(Avr32Insn d)
        {
            // list bits: 0 k, 1 R0-R3, 2 R4-R7, 3 R8-R9, 4 R10, 5 R11, 6 R12, 7 LR, 8 PC
            var l = d.list;
            var sp = r[13];
            var pcLoaded = false;
            uint newPc = 0;
            if ((l & 256) != 0 && (l & 1) != 0)
            {
                newPc = Load32(sp);
                sp += 4;
                pcLoaded = true;
                var lr = (l >> 7) & 1;
                var r12 = (l >> 6) & 1;
                r[12] =
                    lr == 0 && r12 == 0 ? 0
                    : lr == 0 && r12 == 1 ? 1u
                    : 0xFFFFFFFF;
            }
            else
            {
                if ((l & 256) != 0)
                {
                    newPc = Load32(sp);
                    sp += 4;
                    pcLoaded = true;
                }
                if ((l & 128) != 0)
                {
                    r[14] = Load32(sp);
                    sp += 4;
                }
                if ((l & 64) != 0)
                {
                    r[12] = Load32(sp);
                    sp += 4;
                }
            }
            if ((l & 32) != 0)
            {
                r[11] = Load32(sp);
                sp += 4;
            }
            if ((l & 16) != 0)
            {
                r[10] = Load32(sp);
                sp += 4;
            }
            if ((l & 8) != 0)
            {
                r[9] = Load32(sp);
                r[8] = Load32(sp + 4);
                sp += 8;
            }
            if ((l & 4) != 0)
            {
                r[7] = Load32(sp);
                r[6] = Load32(sp + 4);
                r[5] = Load32(sp + 8);
                r[4] = Load32(sp + 12);
                sp += 16;
            }
            if ((l & 2) != 0)
            {
                r[3] = Load32(sp);
                r[2] = Load32(sp + 4);
                r[1] = Load32(sp + 8);
                r[0] = Load32(sp + 12);
                sp += 16;
            }
            r[13] = sp;
            if (pcLoaded)
            {
                FlagsRet();
                Jump(newPc);
            }
        }

        private void Ldm(Avr32Insn d)
        {
            var l = d.list;
            var rp = d.rp;
            var addr = r[rp];
            var pcLoaded = false;
            uint newPc = 0;
            if ((l & 0x8000) != 0)
            {
                if (rp == 15)
                {
                    addr = r[13];
                }
                newPc = Load32(addr);
                addr += 4;
                pcLoaded = true;
                if (rp == 15)
                {
                    var lr = (l >> 14) & 1;
                    var r12 = (l >> 12) & 1;
                    r[12] =
                        lr == 0 && r12 == 0 ? 0
                        : lr == 0 && r12 == 1 ? 1u
                        : 0xFFFFFFFF;
                }
                else
                {
                    if ((l & 0x4000) != 0)
                    {
                        r[14] = Load32(addr);
                        addr += 4;
                    }
                    if ((l & 0x2000) != 0)
                    {
                        r[13] = Load32(addr);
                        addr += 4;
                    }
                    if ((l & 0x1000) != 0)
                    {
                        r[12] = Load32(addr);
                        addr += 4;
                    }
                }
            }
            else
            {
                if ((l & 0x4000) != 0)
                {
                    r[14] = Load32(addr);
                    addr += 4;
                }
                if ((l & 0x2000) != 0)
                {
                    r[13] = Load32(addr);
                    addr += 4;
                }
                if ((l & 0x1000) != 0)
                {
                    r[12] = Load32(addr);
                    addr += 4;
                }
            }
            for (var i = 11; i >= 0; i--)
            {
                if ((l >> i & 1) != 0)
                {
                    r[i] = Load32(addr);
                    addr += 4;
                }
            }
            if (d.op != 0)
            {
                if (rp == 15)
                {
                    r[13] = addr;
                }
                else
                {
                    r[rp] = addr;
                }
            }
            if (pcLoaded)
            {
                FlagsRet();
                Jump(newPc);
            }
        }

        private void Stm(Avr32Insn d)
        {
            var l = d.list;
            var addr = r[d.rp];
            if (d.op != 0)
            {
                for (var i = 0; i <= 15; i++)
                {
                    if ((l >> i & 1) != 0)
                    {
                        addr -= 4;
                        Store32(addr, r[i]);
                    }
                }
                r[d.rp] = addr;
            }
            else
            {
                for (var i = 15; i >= 0; i--)
                {
                    if ((l >> i & 1) != 0)
                    {
                        Store32(addr, r[i]);
                        addr += 4;
                    }
                }
            }
        }

        private void Rete()
        {
            var mode = Mode;
            if (mode <= 1)
            {
                throw new Avr32Exception(EX_PRIV, "privilege violation (rete)");
            }
            // privileged mode: r[13] is SP_SYS
            var sp = r[13];
            var newSr = Load32(sp);
            var newPc = Load32(sp + 4);
            sp += 8;
            if (mode >= 2 && mode <= 5)
            {
                r[14] = Load32(sp);
                r[12] = Load32(sp + 4);
                r[11] = Load32(sp + 8);
                r[10] = Load32(sp + 12);
                r[9] = Load32(sp + 16);
                r[8] = Load32(sp + 20);
                sp += 24;
            }
            r[13] = sp;
            WriteSR(newSr & ~SR_L);
            Jump(newPc);
        }

        private void Rets(uint pc)
        {
            var mode = Mode;
            if (mode == 0)
            {
                throw new Avr32Exception(EX_PRIV, "privilege violation (rets)");
            }
            if (mode == 1)
            {
                var sp = r[13];
                var newSr = Load32(sp);
                var newPc = Load32(sp + 4);
                r[13] = sp + 8;
                WriteSR(newSr);
                Jump(newPc);
            }
            else
            {
                Jump(r[14]);
            }
        }

        private static string RegName(int i) =>
            i == 13 ? "SP"
            : i == 14 ? "LR"
            : i == 15 ? "PC"
            : "R" + i;

        /// <summary>The op8 of the last sleep instruction (0..6: idle, frozen, standby, stop, deep stop, static).</summary>
        public int SleepMode { get; private set; }

        /// <summary>Raised when the core executes sleep (the power manager model gates clocks by mode).</summary>
        public event Action<int> SleepEntered;

        private readonly uint[] r = new uint[16];
        private readonly uint[] sys = new uint[256];
        private uint sr,
            spApp,
            spSys;
        private readonly uint resetPC;
        private uint nextPc;
        private bool jumped;
        private bool sleeping;
        private bool returnEarly;
        private volatile int pendingLevels;
        private volatile bool nmiPending;
        private readonly uint[] autovector = new uint[4];
        private bool warnedEvbaOr;

        private ulong executedInstructions;
        private ulong budget,
            used,
            reported,
            debt;
        private ulong chunkStartNs;
        private bool inChunk;
        private ulong frac,
            slotPerInsnQ16;
        private long clockHz;

        private Region[] regions = new Region[0];
        private PageEntry[] pages;
        private Region lastCode,
            lastData;
        private bool regionsDirty = true;
        private readonly Avr32Insn scratch = new Avr32Insn();

        private readonly LimitTimer countTimer,
            compareTimer;

        private string traceFile;
        private System.IO.StreamWriter traceWriter;
    }
}
