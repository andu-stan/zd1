// AT32UC3A1 audio path: PDCA channel 0 and the SSC transmitter (USB audio: ring -> PDCA -> SSC -> DSP).
// As in the QEMU model: once TX is enabled the SSC pulls 2 x fs halfwords per second from PDCA channel 0 (ring with
// reload, RCZ/TRC interrupt -> INTC 96); fs comes from the board (the CS2000 ratio); TX_FS on PA14 toggles at fs.
// Every 500 ms an "SSC fs ..." trace line: words, nonzero, peak L/R, starved (DMA empty with no reload interrupt
// pending), crc32 of the samples.
using System;
using System.Collections.Generic;
using Antmicro.Renode.Core;
using Antmicro.Renode.Logging;
using Antmicro.Renode.Peripherals.Bus;
using Antmicro.Renode.Peripherals.Timers;
using Antmicro.Renode.Time;

namespace Antmicro.Renode.Peripherals.UC3
{
    /// <summary>What drives the SSC's I2S clocks on the board (the frame rate in Hz).</summary>
    public interface IUC3I2SClock : IPeripheral
    {
        double SscFs { get; }
    }

    /// <summary>PDCA (0xFFFF0000): channel 0 only (MAR, PSR, TCR, MARR, TCRR, CR, MR, SR, IER/IDR/IMR, ISR).</summary>
    public class AT32UC3_PDCA : UC3Peripheral, INumberedGPIOOutput
    {
        public AT32UC3_PDCA(IMachine machine)
            : base(machine, 0x800, "PDCA")
        {
            Connections = new Dictionary<int, IGPIO> { { 0, new GPIO() } };
            TraceAccesses = false;
            Reset();
        }

        public IReadOnlyDictionary<int, IGPIO> Connections { get; }

        public override void Reset()
        {
            mar = psr = tcr = marr = tcrr = mr = imr = 0;
            en = false;
            UpdateIrq();
        }

        public bool Enabled => en;
        public uint Psr => psr;
        public uint Tcr => tcr;

        /// <summary>RCZ (reload counter zero), TRC (transfer complete).</summary>
        public uint Isr => (tcrr == 0 ? 1u : 0) | (tcr == 0 && tcrr == 0 ? 2u : 0);

        public bool IrqPending => (Isr & imr) != 0;

        /// <summary>The SSC takes one halfword (big-endian from memory); false if the channel is empty.</summary>
        public bool TakeHalfword(out int sample, out byte hi, out byte lo)
        {
            sample = 0;
            hi = lo = 0;
            if (!en || psr != 9 || tcr == 0)
            {
                return false;
            }
            hi = machine.SystemBus.ReadByte(mar);
            lo = machine.SystemBus.ReadByte(mar + 1);
            mar += 2;
            tcr--;
            ReloadIfEmpty();
            sample = (short)(hi << 8 | lo);
            return true;
        }

        public void UpdateIrq()
        {
            Connections[0].Set(IrqPending);
        }

        protected override uint Read(long offset)
        {
            switch (offset)
            {
                case 0x00:
                    return mar;
                case 0x04:
                    return psr;
                case 0x08:
                    return tcr;
                case 0x0C:
                    return marr;
                case 0x10:
                    return tcrr;
                case 0x18:
                    return mr;
                case 0x1C:
                    return en ? 1u : 0;
                case 0x28:
                    return imr;
                case 0x2C:
                    return Isr;
                default:
                    return 0;
            }
        }

        protected override void Write(long offset, uint v)
        {
            switch (offset)
            {
                case 0x00:
                    mar = v;
                    break;
                case 0x04:
                    psr = v;
                    break;
                case 0x08:
                    tcr = v & 0xFFFF;
                    break;
                case 0x0C:
                    marr = v;
                    break;
                case 0x10:
                    tcrr = v & 0xFFFF;
                    break;
                case 0x14:
                    if ((v & 1) != 0)
                    {
                        en = true;
                    }
                    if ((v & 2) != 0)
                    {
                        en = false;
                    }
                    break;
                case 0x18:
                    mr = v;
                    break;
                case 0x20:
                    imr |= v & 7;
                    break;
                case 0x24:
                    imr &= ~v;
                    break;
            }
            ReloadIfEmpty();
            UpdateIrq();
        }

        private void ReloadIfEmpty()
        {
            if (tcr == 0 && tcrr != 0)
            {
                mar = marr;
                tcr = tcrr;
                tcrr = 0;
            }
        }

        private uint mar,
            psr,
            tcr,
            marr,
            tcrr,
            mr,
            imr;
        private bool en;
    }

    /// <summary>SSC (0xFFFF3400): only CR.TXEN/TXDIS/SWRST matter; the rest of the registers are stored.</summary>
    public class AT32UC3_SSC : UC3Peripheral
    {
        public AT32UC3_SSC(IMachine machine, AT32UC3_PDCA pdca, AT32UC3_GPIO gpio)
            : base(machine, 0x400, "SSC")
        {
            this.pdca = pdca;
            tick = new LimitTimer(
                machine.ClockSource,
                1000000000,
                this,
                "ssc-tick",
                TickNs,
                Direction.Descending,
                false,
                WorkMode.Periodic,
                true
            );
            tick.LimitReached += Tick;
            var prev = gpio.PeripheralLevel;
            gpio.PeripheralLevel = pin => pin == 14 ? FsLevel() : prev?.Invoke(pin);
            Reset();
        }

        /// <summary>The board's I2S clock source (CS2000).</summary>
        public IUC3I2SClock Clock { get; set; }

        /// <summary>Slow the I2S clock (ZD1_SSC_SCALE; the stock firmware's DMA interrupt rate).</summary>
        public double Scale { get; set; } = 1.0;

        public override void Reset()
        {
            Array.Clear(regs, 0, regs.Length);
            txen = false;
            tick.Enabled = false;
        }

        protected override uint Read(long offset) => regs[offset / 4];

        protected override void Write(long offset, uint v)
        {
            regs[offset / 4] = v;
            if (offset != 0)
            {
                return;
            }
            var was = txen;
            if ((v & (1u << 15)) != 0 || (v & (1u << 9)) != 0)
            {
                txen = false; // SWRST / TXDIS
            }
            if ((v & (1u << 8)) != 0)
            {
                txen = true;
            }
            if (txen != was)
            {
                trace.Log(
                    string.Format("  SSC TX {0} at fs {1:F2} Hz", txen ? "ENABLED" : "disabled", Fs)
                );
                if (txen)
                {
                    SyncTime();
                    tLast = tLog = NowNs;
                    acc = 0;
                    tick.Value = TickNs;
                    tick.Enabled = true;
                }
                else
                {
                    tick.Enabled = false;
                }
            }
        }

        private double Fs => (Clock?.SscFs ?? 0) * Scale;

        private bool? FsLevel()
        {
            var fs = Fs;
            if (!txen || fs <= 0)
            {
                return false;
            }
            return ((long)(NowNs * 2.0 * fs / 1e9) & 1) != 0;
        }

        private void Tick()
        {
            var now = NowNs;
            var fs = Fs;
            if (!txen)
            {
                return;
            }
            acc += 2.0 * fs * (now - tLast) / 1e9;
            tLast = now;
            if (acc > 2.0 * fs * 0.02)
            {
                acc = 2.0 * fs * 0.02;
            }
            while (acc >= 1.0)
            {
                if (pdca.Enabled && pdca.Tcr == 0 && pdca.IrqPending)
                {
                    break; // backlog: the reload ISR has not run yet
                }
                Word();
                acc -= 1.0;
            }
            pdca.UpdateIrq();
            if (now - tLog >= 500000000)
            {
                trace.Log(
                    string.Format(
                        "  SSC fs {0:F2} Hz: {1} words ({2} nonzero) peak L {3} R {4} starved {5} crc {6:x8}",
                        fs,
                        hw - wHw,
                        nz - wNz,
                        peak[0],
                        peak[1],
                        starved - wStarved,
                        crc ^ 0xFFFFFFFF
                    ),
                    true
                );
                wHw = hw;
                wNz = nz;
                wStarved = starved;
                peak[0] = peak[1] = 0;
                tLog = now;
            }
        }

        private void Word()
        {
            if (!pdca.TakeHalfword(out var v, out var b0, out var b1))
            {
                starved++;
                hw++;
                return;
            }
            var ch = (int)(hw & 1);
            if (v != 0)
            {
                nz++;
            }
            if (Math.Abs(v) > peak[ch])
            {
                peak[ch] = Math.Abs(v);
            }
            crc = Crc(crc, b0);
            crc = Crc(crc, b1);
            hw++;
        }

        private static uint Crc(uint c, byte b)
        {
            c ^= b;
            for (var k = 0; k < 8; k++)
            {
                c = (c >> 1) ^ (0xEDB88320u & (uint)-(int)(c & 1));
            }
            return c;
        }

        private const ulong TickNs = 125000;

        private readonly AT32UC3_PDCA pdca;
        private readonly LimitTimer tick;
        private readonly uint[] regs = new uint[0x100];
        private bool txen;
        private double acc;
        private ulong tLast,
            tLog;
        private ulong hw,
            nz,
            starved,
            wHw,
            wNz,
            wStarved;
        private readonly int[] peak = new int[2];
        private uint crc = 0xFFFFFFFF;
    }
}
