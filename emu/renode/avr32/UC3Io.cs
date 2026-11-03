// AT32UC3A1 I/O blocks: GPIO, USART, TC (channel 0), ADC, PWM.
using System;
using System.Collections.Generic;
using System.Linq;
using System.Text;
using Antmicro.Renode.Core;
using Antmicro.Renode.Core.Structure;
using Antmicro.Renode.Logging;
using Antmicro.Renode.Peripherals.Bus;
using Antmicro.Renode.Peripherals.CPU;
using Antmicro.Renode.Peripherals.Timers;
using Antmicro.Renode.Peripherals.UART;
using Antmicro.Renode.Time;

namespace Antmicro.Renode.Peripherals.UC3
{
    /// <summary>
    /// GPIO, 4 ports of 32 pins at 0x100 each (GPER, PMR0/1, ODER, OVR, PVR, PUER, IER, IMR0/1, GFER, IFR; each with
    /// set/clear/toggle at +4/+8/+C). Pin number n = port * 32 + bit.
    /// Inputs: OnGPIO(n, level) is what the board drives on a pin (default: high, pulled up). PVR = OVR on driven
    /// outputs, the input level elsewhere. Pin-change interrupts (IER, IMR0/1 edge mode, IFR) on IRQ outputs 0..13
    /// (one per 8 pins) -> INTC group 2.
    /// Outputs: PortChanged(port) after every write that may change a pin; OutHigh/LevelOut give the levels.
    /// </summary>
    public class AT32UC3_GPIO : UC3Peripheral, IGPIOReceiver, INumberedGPIOOutput
    {
        public AT32UC3_GPIO(IMachine machine)
            : base(machine, 0x400, "GPIO")
        {
            var c = new Dictionary<int, IGPIO>();
            for (var i = 0; i < 14; i++)
            {
                c[i] = new GPIO();
            }
            Connections = c;
            for (var p = 0; p < 4; p++)
            {
                inLevel[p] = 0xFFFFFFFF;
            }
            Reset();
        }

        public IReadOnlyDictionary<int, IGPIO> Connections { get; }

        /// <summary>A port changed (any register write); the board looks at the pins it cares about.</summary>
        public event Action<int> PortChanged;

        /// <summary>Level overrides for pins a peripheral drives (e.g. SSC TX_FS on PA14); null = no override.</summary>
        public Func<int, bool?> PeripheralLevel { get; set; }

        /// <summary>An input level computed when read (a board signal that depends on the time, e.g. a request line).</summary>
        public void SetExternalInput(int pin, Func<bool> level)
        {
            external[pin] = level;
            externalMask[pin / 32] |= 1u << (pin % 32);
        }

        public override void Reset()
        {
            for (var p = 0; p < 4; p++)
            {
                for (var i = 0; i < 64; i++)
                {
                    regs[p, i] = 0;
                }
            }
            for (var i = 0; i < 14; i++)
            {
                Connections[i].Unset();
            }
            for (var p = 0; p < 4; p++)
            {
                PortChanged?.Invoke(p);
            }
        }

        public void OnGPIO(int number, bool value)
        {
            var p = number / 32;
            var bit = number % 32;
            if (p < 0 || p > 3)
            {
                return;
            }
            var m = 1u << bit;
            var old = Pvr(p) & m;
            inLevel[p] = value ? inLevel[p] | m : inLevel[p] & ~m;
            var now = Pvr(p) & m;
            if (old != now && (regs[p, IER] & m) != 0)
            {
                var mode = ((regs[p, IMR1] >> bit) & 1) << 1 | ((regs[p, IMR0] >> bit) & 1);
                if (mode == 0 || (mode == 1 && now != 0) || (mode == 2 && now == 0))
                {
                    regs[p, IFR] |= m;
                }
                UpdateIrq(p);
            }
        }

        /// <summary>Pin is a GPIO output driven high.</summary>
        public bool OutHigh(int port, int bit)
        {
            var m = 1u << bit;
            return (regs[port, GPER] & m) != 0
                && (regs[port, ODER] & m) != 0
                && (regs[port, OVR] & m) != 0;
        }

        /// <summary>What the MCU puts on the line: the driven level, or high when it is an input/peripheral pin.</summary>
        public bool LevelOut(int port, int bit)
        {
            var m = 1u << bit;
            if ((regs[port, GPER] & m) == 0 || (regs[port, ODER] & m) == 0)
            {
                return true;
            }
            return (regs[port, OVR] & m) != 0;
        }

        public uint Pvr(int p)
        {
            var oder = regs[p, ODER];
            var ovr = regs[p, OVR];
            var inp = inLevel[p];
            if (externalMask[p] != 0)
            {
                foreach (var kv in external)
                {
                    if (kv.Key / 32 == p)
                    {
                        var b = kv.Key % 32;
                        inp = kv.Value() ? inp | 1u << b : inp & ~(1u << b);
                    }
                }
            }
            var v = (ovr & oder) | (inp & ~oder);
            if (PeripheralLevel != null)
            {
                var gper = regs[p, GPER];
                for (var b = 0; b < 32; b++)
                {
                    if ((gper >> b & 1) == 0)
                    {
                        var o = PeripheralLevel(p * 32 + b);
                        if (o.HasValue)
                        {
                            v = o.Value ? v | 1u << b : v & ~(1u << b);
                        }
                    }
                }
            }
            return v;
        }

        protected override bool IsPollRead(long offset) => true;

        protected override uint Read(long offset)
        {
            var p = (int)(offset >> 8);
            var r = (int)((offset & 0xF0) >> 2);
            if (r == PVR)
            {
                return Pvr(p);
            }
            return regs[p, r];
        }

        protected override void Write(long offset, uint value)
        {
            var p = (int)(offset >> 8);
            var r = (int)((offset & 0xF0) >> 2);
            var op = offset & 0xC;
            var g0 = regs[p, GPER];
            var d0 = regs[p, ODER];
            var o0 = regs[p, OVR];
            switch (op)
            {
                case 0:
                    regs[p, r] = value;
                    break;
                case 4:
                    regs[p, r] |= value;
                    break;
                case 8:
                    regs[p, r] &= ~value;
                    break;
                default:
                    regs[p, r] ^= value;
                    break;
            }
            LogPinChanges(p, g0, d0, o0);
            if (r == IER || r == IFR)
            {
                UpdateIrq(p);
            }
            PortChanged?.Invoke(p);
        }

        protected override uint Peek(long offset) =>
            regs[(int)(offset >> 8), (int)((offset & 0xF0) >> 2)];

        private void LogPinChanges(int p, uint gper0, uint oder0, uint ovr0)
        {
            if (!trace.Enabled)
            {
                return;
            }
            var gper = regs[p, GPER];
            var oder = regs[p, ODER];
            var ovr = regs[p, OVR];
            for (var b = 0; b < 32; b++)
            {
                var m = 1u << b;
                var was = (gper0 & m) != 0 ? ((oder0 & m) != 0 ? ((ovr0 & m) != 0 ? 1 : 0) : 2) : 3;
                var now = (gper & m) != 0 ? ((oder & m) != 0 ? ((ovr & m) != 0 ? 1 : 0) : 2) : 3;
                if (was != now)
                {
                    trace.Log(
                        string.Format(
                            "  PIN P{0}{1:00} {2} -> {3}  (pc={4:x8})",
                            (char)('A' + p),
                            b,
                            States[was],
                            States[now],
                            Pc
                        )
                    );
                }
            }
        }

        private void UpdateIrq(int p)
        {
            var pend = regs[p, IFR] & regs[p, IER];
            for (var g = 0; g < 4; g++)
            {
                var n = p * 4 + g;
                if (n < 14)
                {
                    Connections[n].Set(((pend >> (8 * g)) & 0xFF) != 0);
                }
            }
        }

        private const int GPER = 0x00 >> 2,
            PMR0 = 0x10 >> 2,
            PMR1 = 0x20 >> 2,
            ODER = 0x40 >> 2,
            OVR = 0x50 >> 2,
            PVR = 0x60 >> 2,
            IER = 0x90 >> 2,
            IMR0 = 0xA0 >> 2,
            IMR1 = 0xB0 >> 2,
            IFR = 0xD0 >> 2;
        private static readonly string[] States = { "LOW", "HIGH", "input", "periph" };

        private readonly uint[,] regs = new uint[4, 64];
        private readonly uint[] inLevel = new uint[4];
        private readonly uint[] externalMask = new uint[4];
        private readonly Dictionary<int, Func<bool>> external = new Dictionary<int, Func<bool>>();
    }

    /// <summary>
    /// USART: transmitter always ready (TXRDY|TXEMPTY), THR characters out as an IUART; received characters into RHR
    /// with RXRDY and the RXRDY interrupt (IER/IDR/IMR bit 0, TXRDY bit 1). IRQ 0 -> the USART's INTC group.
    /// </summary>
    public class AT32UC3_USART : UC3Peripheral, IUART, INumberedGPIOOutput
    {
        public AT32UC3_USART(IMachine machine, string blockName = "USART")
            : base(machine, 0x400, blockName)
        {
            Connections = new Dictionary<int, IGPIO> { { 0, new GPIO() } };
            Reset();
        }

        public IReadOnlyDictionary<int, IGPIO> Connections { get; }

        /// <summary>PBA clock for BRGR (CD = PBA / (16 * baud)): 48 MHz with our firmware, 66 MHz with stock.</summary>
        public uint PbaHz { get; set; } = 48000000;

        /// <summary>The standard rate within 3 % of PBA / (16 * CD) (a UART receiver tolerates that much, and a
        /// Renode UART hub only passes characters between ports whose rates are equal; 48 MHz / 416 = 115384).</summary>
        public uint BaudRate
        {
            get
            {
                if (brgr == 0)
                {
                    return 115200;
                }
                var raw = (double)PbaHz / (16.0 * brgr);
                foreach (
                    var std in new uint[]
                    {
                        9600,
                        19200,
                        38400,
                        57600,
                        115200,
                        230400,
                        460800,
                        921600,
                    }
                )
                {
                    if (Math.Abs(raw - std) <= std * 0.03)
                    {
                        return std;
                    }
                }
                return (uint)raw;
            }
        }
        public Bits StopBits => Bits.One;
        public Parity ParityBit => Parity.None;

        public event Action<byte> CharReceived;

        public override void Reset()
        {
            rx.Clear();
            imr = 0;
            brgr = 0;
            UpdateIrq();
        }

        public void WriteChar(byte value)
        {
            rx.Enqueue(value);
            UpdateIrq();
        }

        protected override bool IsSilent(long offset, bool write) =>
            (!write && offset == 0x14) || (write && offset == 0x1C);

        protected override uint Read(long offset)
        {
            switch (offset)
            {
                case 0x10:
                    return imr;
                case 0x14: // CSR
                    return (1u << 1) | (1u << 9) | (rx.Count > 0 ? 1u : 0);
                case 0x18: // RHR
                {
                    var v = rx.Count > 0 ? rx.Dequeue() : (byte)0;
                    UpdateIrq();
                    return v;
                }
                case 0x20:
                    return brgr;
                default:
                    return 0;
            }
        }

        protected override void Write(long offset, uint value)
        {
            switch (offset)
            {
                case 0x08:
                    imr |= value;
                    break;
                case 0x0C:
                    imr &= ~value;
                    break;
                case 0x1C:
                    CharReceived?.Invoke((byte)value);
                    break;
                case 0x20:
                    brgr = value & 0xFFFF;
                    break;
            }
            UpdateIrq();
        }

        private void UpdateIrq()
        {
            var st = (1u << 1) | (1u << 9) | (rx.Count > 0 ? 1u : 0);
            Connections[0].Set((st & imr) != 0);
        }

        private readonly Queue<byte> rx = new Queue<byte>();
        private uint imr,
            brgr;
    }

    /// <summary>TC channel 0: waveform mode up to RC, RC compare (CPCS) interrupt on IRQ 0 (-> IRQ 448). Clock: TIMER_CLOCK1
    /// = the 32 kHz oscillator, which neither firmware starts (stopped, as on the unit), 2..5 = PBA / 2, 8, 32, 128.</summary>
    public class AT32UC3_TC : UC3Peripheral, INumberedGPIOOutput
    {
        public AT32UC3_TC(IMachine machine, AT32UC3A_PM pm)
            : base(machine, 0x100, "TC")
        {
            this.pm = pm;
            Connections = new Dictionary<int, IGPIO> { { 0, new GPIO() } };
            timer = new LimitTimer(
                machine.ClockSource,
                1000000,
                this,
                "tc0",
                0xFFFF,
                Direction.Ascending,
                false,
                WorkMode.Periodic,
                true
            );
            timer.LimitReached += OnRc;
            pm.ClockChanged += _ => UpdateFrequency();
            TraceAccesses = false;
            Reset();
        }

        public IReadOnlyDictionary<int, IGPIO> Connections { get; }

        public override void Reset()
        {
            cmr = ra = rb = rc = sr = imr = 0;
            enabled = false;
            timer.Enabled = false;
            Connections[0].Unset();
        }

        protected override uint Read(long offset)
        {
            switch (offset)
            {
                case 0x04:
                    return cmr;
                case 0x10:
                    SyncTime();
                    return enabled && rc != 0 ? (uint)timer.Value & 0xFFFF : 0;
                case 0x14:
                    return ra;
                case 0x18:
                    return rb;
                case 0x1C:
                    return rc;
                case 0x20:
                {
                    var v = sr | (enabled ? 0x10000u : 0);
                    sr = 0;
                    UpdateIrq();
                    return v;
                }
                case 0x2C:
                    return imr;
                default:
                    return 0;
            }
        }

        protected override void Write(long offset, uint value)
        {
            switch (offset)
            {
                case 0x00:
                    if ((value & 2) != 0)
                    {
                        enabled = false;
                    }
                    if ((value & 1) != 0)
                    {
                        enabled = true;
                    }
                    if ((value & 4) != 0 && enabled)
                    {
                        timer.Value = 0; // SWTRG
                    }
                    Arm();
                    break;
                case 0x04:
                    cmr = value;
                    UpdateFrequency();
                    break;
                case 0x14:
                    ra = value & 0xFFFF;
                    break;
                case 0x18:
                    rb = value & 0xFFFF;
                    break;
                case 0x1C:
                    rc = value & 0xFFFF;
                    Arm();
                    break;
                case 0x24:
                    imr |= value & 0xFF;
                    break;
                case 0x28:
                    imr &= ~value;
                    break;
            }
            UpdateIrq();
        }

        private ulong Hz()
        {
            var cks = cmr & 7;
            if (cks == 0)
            {
                return 0; // TIMER_CLOCK1 is the 32 kHz oscillator: nothing starts it, on the unit TC0 never counts
            }
            if (cks <= 4)
            {
                var div = new ulong[] { 0, 2, 8, 32, 128 }[cks];
                return Math.Max(1, (ulong)pm.CpuHz / div);
            }
            return 0;
        }

        private void UpdateFrequency()
        {
            var hz = Hz();
            if (hz > 0)
            {
                timer.Frequency = hz;
            }
            Arm();
        }

        private void Arm()
        {
            SyncTime();
            if (enabled && rc != 0 && Hz() > 0)
            {
                if (timer.Limit != rc)
                {
                    timer.Limit = rc;
                }
                timer.Enabled = true;
            }
            else
            {
                timer.Enabled = false;
            }
        }

        private void OnRc()
        {
            sr |= 0x10; // CPCS
            UpdateIrq();
        }

        private void UpdateIrq()
        {
            Connections[0].Set((sr & imr) != 0);
        }

        private readonly AT32UC3A_PM pm;
        private readonly LimitTimer timer;
        private uint cmr,
            ra,
            rb,
            rc,
            sr,
            imr;
        private bool enabled;
    }

    /// <summary>ADC: all enabled channels convert at once on CR.START; CDRn read clears EOCn. Inputs: SetChannel.</summary>
    public class AT32UC3_ADC : UC3Peripheral
    {
        public AT32UC3_ADC(IMachine machine)
            : base(machine, 0x100, "ADC")
        {
            for (var i = 0; i < 8; i++)
            {
                value[i] = 0x200;
            }
            Reset();
        }

        public void SetChannel(int ch, int v)
        {
            if (ch >= 0 && ch < 8)
            {
                value[ch] = (ushort)(v & 0x3FF);
            }
        }

        public int GetChannel(int ch) => value[ch & 7];

        public override void Reset()
        {
            Array.Clear(regs, 0, regs.Length);
        }

        protected override bool IsPollRead(long offset) => true;

        protected override uint Read(long offset)
        {
            if (offset >= 0x30 && offset < 0x50)
            {
                var ch = (int)(offset - 0x30) / 4;
                regs[0x1C / 4] &= ~(1u << ch);
                return value[ch];
            }
            return offset < 0x100 ? regs[offset / 4] : 0;
        }

        protected override void Write(long offset, uint v)
        {
            if (offset == 0x00)
            {
                if ((v & 2) != 0)
                {
                    var chsr = regs[0x18 / 4];
                    uint last = 0;
                    for (var ch = 0; ch < 8; ch++)
                    {
                        if ((chsr & (1u << ch)) != 0)
                        {
                            last = value[ch];
                        }
                    }
                    regs[0x1C / 4] |= chsr | (1u << 16); // EOCx + DRDY
                    regs[0x20 / 4] = last;
                }
            }
            else if (offset == 0x10)
            {
                regs[0x18 / 4] |= v;
            }
            else if (offset == 0x14)
            {
                regs[0x18 / 4] &= ~v;
            }
            else if (offset < 0x100)
            {
                regs[offset / 4] = v;
            }
        }

        private readonly uint[] regs = new uint[0x40];
        private readonly ushort[] value = new ushort[8];
    }

    /// <summary>PWM: registers stored; a channel duty write (CDTY, or CUPD when CMR.CPD = 0) raises DutyWritten.</summary>
    public class AT32UC3_PWM : UC3Peripheral
    {
        public AT32UC3_PWM(IMachine machine)
            : base(machine, 0x400, "PWM")
        {
            TraceAccesses = false;
            Reset();
        }

        public event Action<int, uint> DutyWritten;

        public override void Reset()
        {
            Array.Clear(regs, 0, regs.Length);
        }

        protected override uint Read(long offset) => regs[offset / 4];

        protected override void Write(long offset, uint v)
        {
            regs[offset / 4] = v;
            if (offset >= 0x200 && offset < 0x2E0)
            {
                var ch = (int)(offset - 0x200) / 0x20;
                var r = (offset - 0x200) % 0x20;
                var cpd = ((regs[(0x200 + ch * 0x20) / 4] >> 10) & 1) != 0;
                if (r == 0x04 || (r == 0x10 && !cpd))
                {
                    DutyWritten?.Invoke(ch, v);
                }
            }
        }

        protected override uint Peek(long offset) => regs[offset / 4];

        private readonly uint[] regs = new uint[0x100];
    }
}
