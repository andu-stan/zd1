// AT32UC3A1 serial buses: TWI master (children: II2CPeripheral at 7-bit addresses), SPI master (children:
// ISPIPeripheral at chip selects 0..3).
using System;
using System.Collections.Generic;
using System.Linq;
using Antmicro.Renode.Core;
using Antmicro.Renode.Core.Structure;
using Antmicro.Renode.Logging;
using Antmicro.Renode.Peripherals.Bus;
using Antmicro.Renode.Peripherals.I2C;
using Antmicro.Renode.Peripherals.SPI;
using Antmicro.Renode.Time;

namespace Antmicro.Renode.Peripherals.UC3
{
    /// <summary>An I2C device that wants the transfer the way the bus carries it: the internal address of a read
    /// separately (repeated start), and every write transfer including its address bytes as one Write().</summary>
    public interface IUC3I2CDevice : II2CPeripheral
    {
        void ReadAddress(byte[] internalAddress);
    }

    /// <summary>
    /// TWI master (0xFFFF2C00). A write transfer is START by writing THR (after MMR/IADR), the IADR bytes go first;
    /// the device gets the whole transfer (IADR + data) as one Write() when it ends (STOP, MSDIS, a new MMR, the next
    /// START). A read transfer is START in CR: the IADR bytes are written to the device, then each RHR read is one
    /// Read(1); STOP after the next-to-last byte. No device at the address: NACK + TXCOMP. IRQ 0 -> INTC 352.
    /// BusHeld (board): the bus is stuck (no interrupts), e.g. while a device on it is unpowered.
    /// </summary>
    public class AT32UC3_TWI
        : UC3Peripheral,
            IPeripheralContainer<II2CPeripheral, NumberRegistrationPoint<int>>,
            INumberedGPIOOutput
    {
        public AT32UC3_TWI(IMachine machine)
            : base(machine, 0x400, "TWI")
        {
            children = new SimpleContainerHelper<II2CPeripheral>(machine, this);
            Connections = new Dictionary<int, IGPIO> { { 0, new GPIO() } };
            TraceAccesses = false;
            Reset();
        }

        public IReadOnlyDictionary<int, IGPIO> Connections { get; }

        public Func<bool> BusHeld { get; set; }

        public override void Reset()
        {
            Flush();
            mmr = iadr = imr = 0;
            sr = 0x0000F009;
            dev = null;
            UpdateIrq();
        }

        // ---- children ----
        public void Register(
            II2CPeripheral peripheral,
            NumberRegistrationPoint<int> registrationPoint
        )
        {
            children.Register(peripheral, registrationPoint);
            machine.RegisterAsAChildOf(this, peripheral, registrationPoint);
        }

        public void Unregister(II2CPeripheral peripheral)
        {
            children.Unregister(peripheral);
            machine.UnregisterAsAChildOf(this, peripheral);
        }

        public IEnumerable<NumberRegistrationPoint<int>> GetRegistrationPoints(
            II2CPeripheral peripheral
        ) => children.GetRegistrationPoints(peripheral);

        public IEnumerable<IRegistered<II2CPeripheral, NumberRegistrationPoint<int>>> Children =>
            children.Children;

        protected override uint Read(long offset)
        {
            switch (offset)
            {
                case 0x04:
                    return mmr;
                case 0x0C:
                    return iadr;
                case 0x20: // SR: reading clears NACK
                {
                    var v = sr;
                    sr &= ~(1u << 8);
                    UpdateIrq();
                    return v;
                }
                case 0x2C:
                    return imr;
                case 0x30: // RHR
                {
                    var v = rhr;
                    if (stopReq)
                    {
                        sr &= ~2u;
                        sr |= 1u; // that was the last byte
                        dev?.FinishTransmission();
                        active = false;
                        dev = null;
                    }
                    else
                    {
                        rhr = ReadByte();
                    }
                    UpdateIrq();
                    return v;
                }
                default:
                    return 0;
            }
        }

        protected override void Write(long offset, uint value)
        {
            switch (offset)
            {
                case 0x00: // CR
                    if ((value & (1u << 7)) != 0)
                    {
                        Flush();
                        sr = 0x0000F009 & ~(1u << 8);
                        imr = 0;
                    }
                    if ((value & (1u << 3)) != 0)
                    {
                        Flush(); // MSDIS
                    }
                    if ((value & 1u) != 0)
                    {
                        Begin(((mmr >> 12) & 1) != 0); // START (reads; writes start on THR)
                    }
                    if ((value & 2u) != 0)
                    {
                        stopReq = true;
                    }
                    break;
                case 0x04:
                    Flush();
                    mmr = value;
                    break;
                case 0x0C:
                    iadr = value;
                    break;
                case 0x24:
                    imr |= value;
                    break;
                case 0x28:
                    imr &= ~value;
                    break;
                case 0x34: // THR
                    if (!active)
                    {
                        Begin(false);
                    }
                    if (active)
                    {
                        buf.Add((byte)value);
                    }
                    sr |= 4u | 1u; // TXRDY; TXCOMP once the ISR stops feeding
                    break;
            }
            UpdateIrq();
        }

        private byte ReadByte()
        {
            if (dev == null)
            {
                return 0xFF;
            }
            var b = dev.Read(1);
            return b.Length > 0 ? b[0] : (byte)0xFF;
        }

        private void Begin(bool reading)
        {
            Flush();
            var a7 = (int)((mmr >> 16) & 0x7F);
            var iasz = (int)((mmr >> 8) & 3);
            active = true;
            this.reading = reading;
            stopReq = false;
            sr &= ~1u; // TXCOMP low while busy
            if (!children.TryGetByAddress(a7, out dev))
            {
                trace.Log(string.Format("  I2C {0:x2} NACK (no device)", a7));
                sr |= (1u << 8) | 1u; // NACK + TXCOMP
                active = false;
                dev = null;
                return;
            }
            buf.Clear();
            for (var i = iasz - 1; i >= 0; i--)
            {
                buf.Add((byte)(iadr >> (8 * i)));
            }
            if (reading)
            {
                if (dev is IUC3I2CDevice rich)
                {
                    rich.ReadAddress(buf.ToArray());
                }
                else if (buf.Count > 0)
                {
                    dev.Write(buf.ToArray()); // the sub-address, then a repeated start
                }
                buf.Clear();
                rhr = ReadByte();
                sr |= 2u; // RXRDY
            }
        }

        private void Flush()
        {
            if (active && dev != null)
            {
                if (!reading)
                {
                    dev.Write(buf.ToArray());
                }
                dev.FinishTransmission();
            }
            active = false;
            buf.Clear();
        }

        private void UpdateIrq()
        {
            var held = BusHeld != null && BusHeld();
            Connections[0].Set(!held && (sr & imr) != 0);
        }

        private readonly SimpleContainerHelper<II2CPeripheral> children;
        private readonly List<byte> buf = new List<byte>();
        private II2CPeripheral dev;
        private uint mmr,
            iadr,
            imr,
            sr,
            rhr;
        private bool reading,
            stopReq,
            active;
    }

    /// <summary>
    /// SPI master (0xFFFF2400). Fixed peripheral select (MR.PCS: 0b1110 = NPCS0, 0b1101 = 1, 0b1011 = 2,
    /// 0b0111 = 3). A TDR write exchanges one byte with the selected child (the reply is in RDR); the byte takes its
    /// wire time, 8 SPCK periods (SPCK = main clock / CSRn.SCBR): RDRF and TXEMPTY come when it is done, TDRE when the
    /// shifter took it. LASTXFER (CR bit 24) or a new PCS ends the child's transmission. IRQ 0 -> INTC 288.
    /// </summary>
    public class AT32UC3_SPI
        : UC3Peripheral,
            IPeripheralContainer<ISPIPeripheral, NumberRegistrationPoint<int>>,
            INumberedGPIOOutput
    {
        public AT32UC3_SPI(IMachine machine, AT32UC3A_PM pm, string blockName = "SPI0")
            : base(machine, 0x400, blockName)
        {
            this.pm = pm;
            children = new SimpleContainerHelper<ISPIPeripheral>(machine, this);
            Connections = new Dictionary<int, IGPIO> { { 0, new GPIO() } };
            Reset();
        }

        public IReadOnlyDictionary<int, IGPIO> Connections { get; }

        /// <summary>The chip select that is active (-1 none).</summary>
        public int Selected => cs;

        /// <summary>Chip select changes (cs, selected).</summary>
        public event Action<int, bool> ChipSelect;

        public override void Reset()
        {
            Select(-1);
            Array.Clear(regs, 0, regs.Length);
            busy = false;
            queued = false;
            rdrf = false;
            UpdateIrq();
        }

        public void Register(
            ISPIPeripheral peripheral,
            NumberRegistrationPoint<int> registrationPoint
        )
        {
            children.Register(peripheral, registrationPoint);
            machine.RegisterAsAChildOf(this, peripheral, registrationPoint);
        }

        public void Unregister(ISPIPeripheral peripheral)
        {
            children.Unregister(peripheral);
            machine.UnregisterAsAChildOf(this, peripheral);
        }

        public IEnumerable<NumberRegistrationPoint<int>> GetRegistrationPoints(
            ISPIPeripheral peripheral
        ) => children.GetRegistrationPoints(peripheral);

        public IEnumerable<IRegistered<ISPIPeripheral, NumberRegistrationPoint<int>>> Children =>
            children.Children;

        protected override bool IsPollRead(long offset) => offset == 0x10 || offset == 0x08;

        protected override bool IsSilent(long offset, bool write) => !write && offset == 0x10;

        protected override uint Read(long offset)
        {
            switch (offset)
            {
                case 0x08: // RDR
                    rdrf = false;
                    UpdateIrq();
                    return rdr;
                case 0x10:
                    return Status();
                default:
                    return regs[offset / 4];
            }
        }

        protected override void Write(long offset, uint value)
        {
            switch (offset)
            {
                case 0x00: // CR
                    if ((value & (1u << 7)) != 0)
                    {
                        Array.Clear(regs, 0, regs.Length); // SWRST
                        Select(-1);
                        busy = queued = rdrf = false;
                    }
                    if ((value & (1u << 24)) != 0)
                    {
                        // LASTXFER: deselect when the byte on the wire is done (now, if none is)
                        if (busy || queued)
                        {
                            lastXfer = true;
                        }
                        else
                        {
                            Select(-1);
                        }
                    }
                    break;
                case 0x04: // MR
                    regs[1] = value;
                    Select(PcsToCs((value >> 16) & 0xF));
                    break;
                case 0x0C: // TDR
                    if (busy)
                    {
                        queued = true;
                        queuedByte = (byte)value;
                    }
                    else
                    {
                        StartByte((byte)value);
                    }
                    break;
                case 0x14:
                    regs[0x1C / 4] |= value; // IER -> IMR
                    break;
                case 0x18:
                    regs[0x1C / 4] &= ~value;
                    break;
                default:
                    if (offset != 0x08 && offset != 0x10)
                    {
                        regs[offset / 4] = value;
                    }
                    break;
            }
            UpdateIrq();
        }

        protected override uint Peek(long offset) => regs[offset / 4];

        private uint Status()
        {
            var v = 1u << 16; // SPIENS
            if (!queued)
            {
                v |= 1u << 1; // TDRE
            }
            if (!busy && !queued)
            {
                v |= 1u << 9; // TXEMPTY
            }
            if (rdrf)
            {
                v |= 1u;
            }
            return v;
        }

        private static int PcsToCs(uint pcs)
        {
            if ((pcs & 1) == 0)
            {
                return 0;
            }
            if ((pcs & 2) == 0)
            {
                return 1;
            }
            if ((pcs & 4) == 0)
            {
                return 2;
            }
            if ((pcs & 8) == 0)
            {
                return 3;
            }
            return -1;
        }

        private void Select(int newCs)
        {
            if (newCs == cs)
            {
                return;
            }
            if (cs >= 0)
            {
                if (children.TryGetByAddress(cs, out var old))
                {
                    old.FinishTransmission();
                }
                ChipSelect?.Invoke(cs, false);
            }
            cs = newCs;
            if (cs >= 0)
            {
                ChipSelect?.Invoke(cs, true);
            }
        }

        private void StartByte(byte b)
        {
            busy = true;
            var reply = (byte)0xFF;
            if (cs >= 0 && children.TryGetByAddress(cs, out var dev))
            {
                reply = dev.Transmit(b);
            }
            trace.Log(
                string.Format(
                    "  {0} {1:x2} -> {2:x2}{3}",
                    BlockName,
                    b,
                    reply,
                    cs == 2 ? "" : string.Format("  (NPCS{0})", cs)
                )
            );
            pendingReply = reply;
            var scbr = (regs[(0x30 / 4) + Math.Max(0, cs)] >> 8) & 0xFF;
            var ns = 8ul * Math.Max(1u, scbr) * 1000000000ul / (ulong)Math.Max(1, pm.CpuHz);
            machine.ScheduleAction(
                TimeInterval.FromNanoseconds(Math.Max(1ul, ns)),
                _ => ByteDone(),
                "spi-byte"
            );
            NewDeadline();
        }

        private void ByteDone()
        {
            busy = false;
            rdr = pendingReply;
            rdrf = true;
            if (queued)
            {
                queued = false;
                StartByte(queuedByte);
            }
            else if (lastXfer)
            {
                lastXfer = false;
                Select(-1);
            }
            UpdateIrq();
        }

        private void UpdateIrq()
        {
            Connections[0].Set((Status() & regs[0x1C / 4]) != 0);
        }

        private readonly AT32UC3A_PM pm;
        private readonly SimpleContainerHelper<ISPIPeripheral> children;
        private readonly uint[] regs = new uint[0x100];
        private int cs = -1;
        private bool busy,
            queued,
            rdrf,
            lastXfer;
        private byte queuedByte,
            pendingReply;
        private uint rdr;
    }
}
