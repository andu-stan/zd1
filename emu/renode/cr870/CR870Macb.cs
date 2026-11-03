// Ethernet of the DM870: a Cadence MACB (10/100, rev 0x0001010d) at 0xE0017000, as Renode's network interface.
// Register layout as Linux drivers/net/ethernet/cadence/macb (non-GEM), confirmed by the live `sys macb reg` dump.
// Descriptors (2 words): RX w0 = addr[31:2] | wrap(1) | used(0), w1 = len[11:0] | SOF(14) | EOF(15), 128-byte
// buffers (NCFGR.RBOF = start offset in the first one); TX w0 = addr, w1 = len[10:0] | last(15) | wrap(30) | used(31).
// One PHY on every MDIO address, link up (100 FD). IRQ 0 = level line 9 (DM870_MACB_IRQ).
using System;
using System.Collections.Generic;
using System.Linq;
using Antmicro.Renode.Core;
using Antmicro.Renode.Core.Structure;
using Antmicro.Renode.Logging;
using Antmicro.Renode.Network;
using Antmicro.Renode.Peripherals.Bus;
using Antmicro.Renode.Peripherals.Network;
using Antmicro.Renode.Time;
using Antmicro.Renode.Utilities;

namespace Antmicro.Renode.Peripherals.CR870
{
    public class CR870_MACB : IDoubleWordPeripheral, IKnownSize, IMACInterface, INumberedGPIOOutput
    {
        public CR870_MACB(IMachine machine, CR870_Core core)
        {
            this.machine = machine;
            this.core = core;
            Connections = new Dictionary<int, IGPIO> { { 0, new GPIO() } };
            MAC = MACAddress.Parse("EC:66:D1:00:00:42");
            Reset();
        }

        public IReadOnlyDictionary<int, IGPIO> Connections { get; }

        public long Size => 0x1000;

        public MACAddress MAC { get; set; }

        public event Action<EthernetFrame> FrameReady;

        public void Reset()
        {
            ncr = ncfgr = tsr = rsr = isr = man = rbqp = tbqp = rxptr = txptr = 0;
            imr = 0x3fff;
            Array.Clear(sa, 0, sa.Length);
            Array.Clear(phy, 0, phy.Length);
            phy[0] = 0x3100; // BMCR: 100M, autoneg, full duplex
            phy[1] = 0x782d; // BMSR: 100/10, AN complete, link up
            phy[2] = 0x0007;
            phy[3] = 0xc0f1;
            phy[4] = 0x01e1;
            phy[5] = 0x45e1;
            Update();
        }

        public uint ReadDoubleWord(long off)
        {
            switch (off)
            {
                case 0x00:
                    return ncr;
                case 0x04:
                    return ncfgr;
                case 0x08:
                    return 0x6; // MDIO idle | link
                case 0x14:
                    return tsr;
                case 0x18:
                    return rbqp;
                case 0x1C:
                    return tbqp;
                case 0x20:
                    return rsr;
                case 0x24:
                {
                    var v = isr; // clear on read
                    isr = 0;
                    Update();
                    return v;
                }
                case 0x30:
                    return imr;
                case 0x34:
                    return man;
                case 0xFC:
                    return 0x0001010d;
            }
            if (off >= 0x98 && off <= 0xB4)
            {
                return sa[(off - 0x98) / 4];
            }
            return 0; // statistics
        }

        public void WriteDoubleWord(long off, uint v)
        {
            if (core.Log.Tracing)
            {
                core.Log.Trace(string.Format("macb W {0:x3} <- {1:x8} pc={2:x8}", off, v, core.Pc));
            }
            switch (off)
            {
                case 0x00:
                    ncr = v & ~0x660u; // CLRSTAT, INCSTAT, TSTART, THALT: write-only
                    if ((v & 4) == 0)
                    {
                        rxptr = rbqp;
                    }
                    if ((v & 8) == 0)
                    {
                        txptr = tbqp;
                    }
                    if ((v & (1u << 9)) != 0)
                    {
                        Transmit();
                    }
                    return;
                case 0x04:
                    ncfgr = v;
                    return;
                case 0x14:
                    tsr &= ~v;
                    return;
                case 0x18:
                    rbqp = rxptr = v & ~3u;
                    return;
                case 0x1C:
                    tbqp = txptr = v & ~3u;
                    return;
                case 0x20:
                    rsr &= ~v;
                    return;
                case 0x28:
                    imr &= ~v; // IER
                    Update();
                    return;
                case 0x2C:
                    imr |= v & 0x3fff; // IDR
                    Update();
                    return;
                case 0x34:
                {
                    var reg = (int)((v >> 18) & 31);
                    var rw = (v >> 28) & 3;
                    if ((v >> 30) != 1)
                    {
                        man = v; // not an MDIO frame
                        return;
                    }
                    if (rw == 1)
                    {
                        if (reg == 0 && (v & 0x8000) != 0)
                        {
                            v &= ~0x8000u; // reset self-clears
                        }
                        phy[reg] = (ushort)v;
                    }
                    else
                    {
                        v = (v & ~0xffffu) | (core.EthPhyReleased ? phy[reg] : 0xffffu); // PHY in reset: no answer
                    }
                    man = v;
                    core.Log.Trace(
                        string.Format(
                            "MDIO {0} phy {1} reg {2} = {3:x4}",
                            rw == 2 ? "rd" : "wr",
                            (v >> 23) & 31,
                            reg,
                            man & 0xffff
                        )
                    );
                    return;
                }
            }
            if (off >= 0x98 && off <= 0xB4)
            {
                sa[(off - 0x98) / 4] = v;
            }
        }

        /// <summary>Frames arrive at wire speed (100 Mb/s: 80 ns a byte + preamble and gap), one after the other, and
        /// wait while the RX ring has no free buffer instead of being dropped: the guest only sees interrupts at quantum
        /// boundaries (200 us when linked), slower than frames on the wire (121 us for 1.5 KB), so sulu_eth's RX ring
        /// (about two full frames) overflowed on every TCP burst, a drop the real chip wouldn't have, and under
        /// streaming load its TX queue wedged ("tx DMA queue full" panic).</summary>
        public void ReceiveFrame(EthernetFrame frame)
        {
            lock (rxq)
            {
                rxq.Enqueue(frame);
                if (rxq.Count > 1)
                {
                    return; // the delivery chain is running
                }
            }
            machine.ScheduleAction(
                TimeInterval.FromNanoseconds(1000),
                _ => RxNext(),
                "cr870-macb-rx"
            );
        }

        private void RxNext()
        {
            EthernetFrame f;
            lock (rxq)
            {
                f = rxq.Peek();
            }
            if (!Deliver(f))
            {
                machine.ScheduleAction(
                    TimeInterval.FromMicroseconds(50),
                    _ => RxNext(),
                    "cr870-macb-rx"
                );
                return; // the ring is full: try again soon
            }
            lock (rxq)
            {
                rxq.Dequeue();
                if (rxq.Count == 0)
                {
                    return;
                }
            }
            machine.ScheduleAction(
                TimeInterval.FromNanoseconds((ulong)(f.Bytes.Length + 24) * 80),
                _ => RxNext(),
                "cr870-macb-rx"
            );
        }

        private readonly Queue<EthernetFrame> rxq = new Queue<EthernetFrame>();

        /// <summary>False: no free RX buffer (the frame waits).</summary>
        private bool Deliver(EthernetFrame frame)
        {
            if ((ncr & 4) == 0 || rbqp == 0)
            {
                return true; // RX disabled: drop
            }
            var data = frame.Bytes.Take(1514).ToList();
            while (data.Count < 60)
            {
                data.Add(0);
            }
            data.AddRange(new byte[4]); // FCS (not checked by the driver)
            var size = data.Count;
            var rbof = (int)((ncfgr >> 14) & 3);
            var need = (size + rbof + RxBuf - 1) / RxBuf;
            var p = rxptr;
            for (var i = 0; i <= need; i++) // one more than needed must be free (see the QEMU model)
            {
                var w0 = Ld(p);
                if ((w0 & 1) != 0)
                {
                    if (rxq.Count > 256) // the guest stopped taking frames altogether: drop, like the chip
                    {
                        core.Log.Trace(
                            string.Format(
                                "MACB rx {0} bytes dropped: no free buffer at {1:x8} (desc {2} of {3})",
                                size,
                                p,
                                i,
                                need
                            )
                        );
                        rsr |= 1;
                        isr |= 1 << 10;
                        Update();
                        return true;
                    }
                    return false;
                }
                p = (w0 & 2) != 0 ? rbqp : p + 8;
            }
            var offset = 0;
            var bytes = data.ToArray();
            for (var i = 0; i < need; i++)
            {
                var w0 = Ld(rxptr);
                var skip = i == 0 ? rbof : 0;
                var n = Math.Min(RxBuf - skip, size - offset);
                machine.SystemBus.WriteBytes(bytes, (w0 & ~3u) + (ulong)skip, offset, n);
                var w1 = (i == 0 ? 1u << 14 : 0) | (i == need - 1 ? (1u << 15) | (uint)size : 0);
                St(rxptr + 4, w1);
                St(rxptr, w0 | 1); // hand to software
                rxptr = (w0 & 2) != 0 ? rbqp : rxptr + 8;
                offset += n;
            }
            core.Log.Trace(
                string.Format(
                    "MACB rx {0} bytes in {1} bufs, isr={2:x} imr={3:x}, next desc {4:x8}",
                    size,
                    need,
                    isr | 2,
                    imr,
                    rxptr
                )
            );
            rsr |= 2;
            isr |= 2; // REC / RCOMP
            Update();
            return true;
        }

        private void Transmit()
        {
            var frame = new List<byte>();
            var wire = 0;
            for (var guard = 0; guard < 512; guard++)
            {
                var w0 = Ld(txptr);
                var w1 = Ld(txptr + 4);
                if ((w1 & (1u << 31)) != 0)
                {
                    break; // used: nothing (more) to send
                }
                var n = (int)(w1 & 0x7ff);
                if (frame.Count + n <= 2048)
                {
                    frame.AddRange(machine.SystemBus.ReadBytes(w0, n));
                }
                St(txptr + 4, w1 | (1u << 31));
                txptr = (w1 & (1u << 30)) != 0 ? tbqp : txptr + 8;
                if ((w1 & (1u << 15)) != 0) // last buffer of the frame
                {
                    if (
                        Misc.TryCreateFrameOrLogWarning(
                            this,
                            frame.ToArray(),
                            out var f,
                            addCrc: true
                        )
                    )
                    {
                        FrameReady?.Invoke(f);
                    }
                    wire += frame.Count + 24; // + preamble, FCS, gap
                    frame.Clear();
                }
            }
            // TCOMP after the frames' time on the wire (100 Mb/s: 80 ns a byte), not inside the TSTART write: sulu_eth
            // writes TSTART and then updates its own TX ring, so a completion at once ran its TX-done handler on
            // half-updated state and wedged the TX queue ("sulu eth tx DMA queue full" panic under load)
            var ns = 2000 + (ulong)wire * 80;
            machine.ScheduleAction(
                TimeInterval.FromNanoseconds(ns),
                _ =>
                {
                    tsr |= 0x20 | 0x08; // COMP, UBR (end of ring)
                    isr |= 0x80; // TCOMP
                    Update();
                },
                "cr870-macb-tx"
            );
        }

        private uint Ld(uint a) => machine.SystemBus.ReadDoubleWord(a);

        private void St(uint a, uint v) => machine.SystemBus.WriteDoubleWord(a, v);

        private void Update()
        {
            Connections[0].Set((isr & ~imr & 0x3fff) != 0);
        }

        private const int RxBuf = 128;

        private readonly IMachine machine;
        private readonly CR870_Core core;
        private uint ncr,
            ncfgr,
            tsr,
            rsr,
            isr,
            imr,
            man,
            rbqp,
            tbqp,
            rxptr,
            txptr;
        private readonly uint[] sa = new uint[8];
        private readonly ushort[] phy = new ushort[32];
    }
}
