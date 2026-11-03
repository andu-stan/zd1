// AT32UC3A1 USBB in device mode: endpoint 0 control transfers, enough for the B&W DFU bootloader and our
// firmware's DFU paths.
//
// EP1-EP6 configure (UESTAn.CFGOK) and stay idle: a firmware with more endpoints (ours: audio, HID, the pipe) enumerates.
// Registers polled by firmware: UDCON 0x000 (DETACH bit 8: set at reset; the host's requests wait while it is), UDINT 0x004 (EORST bit 3, EP0INT bit 12) / CLR 0x008 /
// SET 0x00C / UDINTE 0x010 / ECLR 0x014 / ESET 0x018, UECFG0 0x100 (EPSIZE [6:4]), UESTA0 0x130 (TXINI 0, RXOUTI 1,
// RXSTPI 2, NAKOUTI 3, STALLEDI 6, CTRLDIR 17, CFGOK 18, BYCT [30:20]) / CLR 0x160 / SET 0x190, UECON0 0x1C0 / SET
// 0x1F0 (STALLRQ bit 19) / CLR 0x220, USBSTA 0x804 (VBUS 11, CLKUSABLE 14, ID 10, VBUSTI 1) / CLR 0x808 / SET 0x80C,
// USBCON 0x800 (VBUSTE 1: VBUSTI interrupts; ASF's device driver attaches from that interrupt, which it raises itself
// through USBSTASET when VBUS is already high). EP0 FIFO = the HSB window at
// 0xE0000000 (AT32UC3_USBFifo), byte offset = index, big-endian CPU. IRQ 0 = UDINT & UDINTE -> INTC 544.
//
// Host side: a TCP server (Port; 0 = none) for zd1 (host/cli) with ZD1DFU_EMU=host:port.
// What the host sends is taken on the firmware's next USBB register read, and at least every 250 us of virtual time
// (an interrupt-driven firmware, ours, reads nothing until an interrupt; the B&W bootloader polls).
// Host -> device: 'R' bus reset | 'S' setup[8] + OUT data (wLength bytes when bmRequestType bit 7 = 0) | 'Z' us32
// (host sleeps for us of virtual time, answered 'A'). Device -> host: 'A' (done, no data) | 'D' len16le data (IN) |
// 'X' (stalled). Sync (default on): the host's requests happen in virtual time: after each reply the next request
// is due one frame later (LatencyUs); while one is owed and has not arrived, the device's idle UESTA0 polls wait
// for it with the CPU (and virtual time) stopped, so host speed does not show in emulated time.
using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.Linq;
using System.Net;
using System.Net.Sockets;
using System.Threading;
using Antmicro.Renode.Core;
using Antmicro.Renode.Logging;
using Antmicro.Renode.Peripherals.Bus;
using Antmicro.Renode.Peripherals.Timers;
using Antmicro.Renode.Time;

namespace Antmicro.Renode.Peripherals.UC3
{
    public class AT32UC3_USBB : UC3Peripheral, INumberedGPIOOutput
    {
        public AT32UC3_USBB(IMachine machine)
            : base(machine, 0x1000, "USBB")
        {
            Connections = new Dictionary<int, IGPIO> { { 0, new GPIO() } };
            hostTick = new LimitTimer(
                machine.ClockSource,
                1000000,
                this,
                "usb-host-tick",
                250,
                Direction.Descending,
                false,
                WorkMode.Periodic,
                true
            );
            hostTick.LimitReached += () =>
            {
                if (connectedFresh || disconnected || rxPending != 0)
                {
                    Poll();
                }
            };
            Reset();
        }

        public IReadOnlyDictionary<int, IGPIO> Connections { get; }

        /// <summary>USBSTA.VBUS (a PC is connected).</summary>
        public bool Vbus { get; set; } = true;

        public bool Sync { get; set; } = true;

        public ulong LatencyUs { get; set; } = 1000;

        /// <summary>TCP port of the host bridge (127.0.0.1); 0 = no host.</summary>
        public int Port
        {
            get => port;
            set
            {
                port = value;
                StartServer();
            }
        }

        public string Host { get; set; } = "127.0.0.1";

        public override void Reset()
        {
            if (st != State.Idle)
            {
                Reply('X', null); // transfer cut by the chip reset
            }
            st = State.Idle;
            sta = con = udint = udinte = 0;
            vbusti = false;
            flen = 0;
            Array.Clear(regs, 0, regs.Length);
            regs[0] = Detach; // UDCON resets detached, as the chip's
            UpdateIrq();
        }

        internal byte[] Fifo => fifo;

        internal void FifoWritten(int end)
        {
            if (end > flen && end <= fifo.Length)
            {
                flen = end;
            }
        }

        protected override bool IsPollRead(long offset) => true;

        protected override uint Read(long offset)
        {
            Poll();
            switch (offset)
            {
                case 0x004:
                    return Udint;
                case 0x010:
                    return udinte;
                case 0x130:
                    if (Sync && (sta & 6) == 0)
                    {
                        WaitHost();
                        Poll();
                    }
                    return sta | (1u << 18) | ((uint)Math.Min(flen, 0x7FF) << 20);
                case 0x1C0:
                    return con;
                case 0x134:
                case 0x138:
                case 0x13C:
                case 0x140:
                case 0x144:
                case 0x148:
                    return regs[offset / 4] | (1u << 18); // EP1-6 UESTA: CFGOK (they move no data: no host traffic on them)
                case 0x804:
                    return regs[0x804 / 4]
                        | (Vbus ? 1u << 11 : 0)
                        | (1u << 14)
                        | (1u << 10)
                        | (vbusti ? 1u << 1 : 0);
                default:
                    return regs[offset / 4];
            }
        }

        protected override void Write(long offset, uint v)
        {
            regs[offset / 4] = v;
            switch (offset)
            {
                case 0x008:
                    udint &= ~v;
                    break;
                case 0x00C:
                    udint |= v;
                    break;
                case 0x014:
                    udinte &= ~v;
                    break;
                case 0x018:
                    udinte |= v;
                    break;
                case 0x010:
                    udinte = v;
                    break;
                case 0x160:
                    StaClear(v);
                    return;
                case 0x808:
                    if ((v & 2) != 0)
                    {
                        vbusti = false;
                    }
                    break;
                case 0x80C:
                    if ((v & 2) != 0)
                    {
                        vbusti = true;
                    }
                    break;
                case 0x190:
                    sta |= v;
                    break;
                case 0x1F0:
                    con |= v;
                    if ((v & (1u << 19)) != 0 && st != State.Idle)
                    {
                        st = State.Idle; // STALLRQ during a transfer: fail it for the host
                        sta |= 1u << 6;
                        Reply('X', null);
                    }
                    break;
                case 0x220:
                    con &= ~v;
                    break;
                case 0x1C0:
                    con = v;
                    break;
            }
            UpdateIrq();
        }

        protected override uint Peek(long offset) => regs[offset / 4];

        // ---------------------------------------------------------------------------------------------------------

        private enum State
        {
            Idle,
            Setup,
            Out,
            OutStatus,
            In,
            InStatus,
        }

        private uint Ep0Size => 8u << (int)((regs[0x100 / 4] >> 4) & 7);

        private uint Udint => udint | ((sta & con & 0xFF) != 0 ? 1u << 12 : 0);

        private void UpdateIrq()
        {
            Connections[0].Set((Udint & udinte) != 0 || (vbusti && (regs[0x800 / 4] & 2) != 0));
        }

        private void OutChunk()
        {
            var n = (int)Math.Min(Ep0Size, (uint)(outLen - outPos));
            Array.Copy(outData, outPos, fifo, 0, n);
            flen = n;
            chunk = n;
            sta |= 1u << 1; // RXOUTI
        }

        private void HostMessage(byte[] m)
        {
            if (m[0] == (byte)'R')
            {
                st = State.Idle;
                sta = 0;
                flen = 0;
                udint |= 1u << 3; // EORST
                Reply('A', null);
            }
            else
            {
                Array.Copy(m, 1, setup, 0, 8);
                dirIn = (setup[0] & 0x80) != 0;
                wlen = setup[6] | setup[7] << 8;
                outLen = dirIn ? 0 : wlen;
                outPos = 0;
                inLen = 0;
                Array.Copy(m, 9, outData, 0, outLen);
                Array.Copy(setup, fifo, 8);
                flen = 8;
                con &= ~(1u << 19); // a SETUP clears STALLRQ
                sta =
                    (sta & ~((1u << 0) | (1u << 6) | (1u << 17)))
                    | (1u << 2)
                    | (dirIn ? 1u << 17 : 0);
                st = State.Setup;
                trace.Log(
                    string.Format(
                        "  USB SETUP {0:x2} {1:x2} {2:x2}{3:x2} {4:x2}{5:x2} len {6}",
                        setup[0],
                        setup[1],
                        setup[3],
                        setup[2],
                        setup[5],
                        setup[4],
                        wlen
                    )
                );
            }
            UpdateIrq();
        }

        private void StaClear(uint v)
        {
            v &= sta; // clearing a bit that isn't set is a no-op
            sta &= ~v;
            if ((v & (1u << 2)) != 0 && st == State.Setup) // firmware took the SETUP
            {
                flen = 0;
                if (dirIn)
                {
                    st = State.In;
                    sta |= 1u << 0;
                }
                else if (outLen != 0)
                {
                    st = State.Out;
                    OutChunk();
                    sta |= 1u << 0;
                }
                else
                {
                    st = State.OutStatus;
                    sta |= 1u << 0;
                }
            }
            if ((v & (1u << 1)) != 0) // firmware read an OUT packet
            {
                if (st == State.Out)
                {
                    outPos += chunk;
                    flen = 0;
                    if (outPos < outLen)
                    {
                        OutChunk();
                    }
                    else
                    {
                        st = State.OutStatus;
                        sta |= 1u << 0;
                    }
                }
                else if (st == State.InStatus) // host's status ZLP consumed
                {
                    st = State.Idle;
                    Reply('D', inData.Take(inLen).ToArray());
                }
            }
            if ((v & 1u) != 0 && st == State.Out)
            {
                trace.Log(
                    string.Format(
                        "  USB OUT data stage ended early by the device ({0} of {1} B)",
                        outPos,
                        outLen
                    )
                );
                st = State.Idle;
                flen = 0;
                sta &= ~(1u << 1);
                sta |= 1u << 0;
                Reply('A', null);
            }
            else if ((v & 1u) != 0 && st != State.In && st != State.OutStatus)
            {
                sta |= 1u << 0; // not a packet: TXINI comes straight back
            }
            else if ((v & 1u) != 0)
            {
                var n = flen;
                flen = 0;
                if (st == State.In)
                {
                    var k = Math.Min(n, inData.Length - inLen);
                    Array.Copy(fifo, 0, inData, inLen, k);
                    inLen += k;
                    if (n < Ep0Size || inLen >= wlen)
                    {
                        st = State.InStatus;
                        sta |= (1u << 1) | (1u << 3); // host ZLP: RXOUTI + NAKOUTI
                    }
                    sta |= 1u << 0;
                }
                else if (st == State.OutStatus)
                {
                    st = State.Idle;
                    sta |= 1u << 0;
                    Reply('A', null);
                }
                else
                {
                    sta |= 1u << 0;
                }
            }
            UpdateIrq();
        }

        private void Reply(char c, byte[] d)
        {
            if (Sync)
            {
                owed = true;
                due = NowNs + LatencyUs * 1000;
            }
            var n = d?.Length ?? 0;
            trace.Log(
                string.Format(
                    "  USB {0} {1:x2}{2:x2} {3} B -> host {4}",
                    dirIn ? 'I' : 'O',
                    setup[0],
                    setup[1],
                    c == 'D' ? n : outLen,
                    c
                )
            );
            var s = client;
            if (s == null)
            {
                return;
            }
            try
            {
                if (c == 'D')
                {
                    var b = new byte[3 + n];
                    b[0] = (byte)'D';
                    b[1] = (byte)n;
                    b[2] = (byte)(n >> 8);
                    Array.Copy(d, 0, b, 3, n);
                    s.Send(b);
                }
                else
                {
                    s.Send(new[] { (byte)c });
                }
            }
            catch (Exception)
            {
                client = null;
            }
        }

        // ---- the host bridge ----

        private void StartServer()
        {
            listener?.Stop();
            listener = null;
            hostTick.Enabled = port != 0;
            if (port == 0)
            {
                return;
            }
            listener = new TcpListener(IPAddress.Parse(Host), port);
            listener.Start();
            var t = new Thread(AcceptLoop) { IsBackground = true, Name = "usbb-host" };
            t.Start();
        }

        private void AcceptLoop()
        {
            while (true)
            {
                Socket s;
                try
                {
                    s = listener.AcceptSocket();
                }
                catch (Exception)
                {
                    return;
                }
                s.NoDelay = true;
                lock (rxLock)
                {
                    rxQueue.Clear();
                    rxPending = 0;
                    partial.Clear();
                }
                client = s;
                connectedFresh = true;
                var buf = new byte[65536];
                while (true)
                {
                    int n;
                    try
                    {
                        n = s.Receive(buf);
                    }
                    catch (Exception)
                    {
                        n = 0;
                    }
                    if (n <= 0)
                    {
                        break;
                    }
                    lock (rxLock)
                    {
                        for (var i = 0; i < n; i++)
                        {
                            partial.Add(buf[i]);
                            var need = Need(partial);
                            if (need > 0 && partial.Count >= need)
                            {
                                rxQueue.Enqueue(partial.ToArray());
                                rxPending = rxQueue.Count;
                                partial.Clear();
                            }
                        }
                        Monitor.PulseAll(rxLock);
                    }
                }
                if (client == s)
                {
                    client = null;
                    disconnected = true;
                }
                try
                {
                    s.Close();
                }
                catch (Exception) { }
            }
        }

        private static int Need(List<byte> m)
        {
            if (m[0] == (byte)'R')
            {
                return 1;
            }
            if (m[0] == (byte)'Z')
            {
                return 5;
            }
            if (m.Count < 9)
            {
                return 0;
            }
            return 9 + ((m[1] & 0x80) != 0 ? 0 : (m[7] | m[8] << 8));
        }

        /// <summary>Deliver what the host sent, honouring virtual time (sync mode).</summary>
        private void Poll()
        {
            if (!connectedFresh && !disconnected && rxPending == 0)
            {
                return; // every register read polls: nothing from the host (no lock)
            }
            if (connectedFresh)
            {
                connectedFresh = false;
                owed = Sync;
                due = NowNs;
            }
            if (disconnected)
            {
                disconnected = false;
                owed = false;
            }
            while (true)
            {
                byte[] m;
                lock (rxLock)
                {
                    if (rxQueue.Count == 0)
                    {
                        return;
                    }
                    m = rxQueue.Peek();
                    if (m[0] == (byte)'Z')
                    {
                        rxQueue.Dequeue();
                        rxPending = rxQueue.Count;
                        var us = (ulong)(m[1] | m[2] << 8 | m[3] << 16 | m[4] << 24);
                        owed = false;
                        sleepUntil = NowNs + us * 1000;
                        machine.ScheduleAction(
                            TimeInterval.FromMicroseconds(us),
                            _ => SleepDone(),
                            "usb-host-sleep"
                        );
                        NewDeadline();
                        continue;
                    }
                    if ((regs[0] & Detach) != 0)
                    {
                        // detached: no host reaches the device; what the host sent waits for the attach (a firmware that
                        // has just started gets its first reset and requests once it is on the bus, not while it sets up)
                        return;
                    }
                    if (Sync && NowNs < due)
                    {
                        // not due yet: deliver it at its time
                        if (!deliveryScheduled)
                        {
                            deliveryScheduled = true;
                            machine.ScheduleAction(
                                TimeInterval.FromNanoseconds(due - NowNs),
                                _ =>
                                {
                                    deliveryScheduled = false;
                                    Poll();
                                },
                                "usb-host-deliver"
                            );
                            NewDeadline();
                        }
                        return;
                    }
                    rxQueue.Dequeue();
                    rxPending = rxQueue.Count;
                }
                owed = false;
                HostMessage(m);
            }
        }

        private void SleepDone()
        {
            owed = true;
            due = NowNs;
            try
            {
                client?.Send(new[] { (byte)'A' });
            }
            catch (Exception) { }
        }

        private void WaitHost()
        {
            // a request is owed and not here: wait for it with virtual time stopped (we are on the CPU thread)
            if (!owed || client == null)
            {
                return;
            }
            var t0 = DateTime.UtcNow;
            lock (rxLock)
            {
                while (owed && rxQueue.Count == 0 && client != null)
                {
                    if ((DateTime.UtcNow - t0).TotalSeconds > 120)
                    {
                        this.Log(LogLevel.Warning, "USB host silent for 120 s (real); running on");
                        owed = false;
                        break;
                    }
                    Monitor.Wait(rxLock, 100);
                }
            }
        }

        private const uint Detach = 1u << 8; // UDCON.DETACH
        private readonly uint[] regs = new uint[0x400];
        private readonly byte[] fifo = new byte[1024];
        private readonly byte[] setup = new byte[8];
        private readonly byte[] outData = new byte[4096];
        private readonly byte[] inData = new byte[4096];
        private int flen,
            chunk,
            wlen,
            outLen,
            outPos,
            inLen;
        private bool dirIn;
        private uint sta,
            con,
            udint,
            udinte;
        private bool vbusti;
        private State st;

        private int port;
        private TcpListener listener;
        private volatile Socket client;
        private volatile bool connectedFresh,
            disconnected;
        private readonly object rxLock = new object();
        private readonly Queue<byte[]> rxQueue = new Queue<byte[]>();
        private volatile int rxPending; // rxQueue.Count, readable without the lock
        private readonly List<byte> partial = new List<byte>();
        private bool owed,
            deliveryScheduled;
        private ulong due,
            sleepUntil;
        private readonly LimitTimer hostTick;
    }

    /// <summary>The USB endpoint FIFO window on the HSB (0xE0000000): byte offset = FIFO index, big-endian.</summary>
    public class AT32UC3_USBFifo
        : IBytePeripheral,
            IWordPeripheral,
            IDoubleWordPeripheral,
            IKnownSize
    {
        public AT32UC3_USBFifo(AT32UC3_USBB usb)
        {
            this.usb = usb;
        }

        public long Size => 0x10000;

        public void Reset() { }

        public byte ReadByte(long offset) => offset < usb.Fifo.Length ? usb.Fifo[offset] : (byte)0;

        public ushort ReadWord(long offset) =>
            (ushort)(ReadByte(offset) << 8 | ReadByte(offset + 1));

        public uint ReadDoubleWord(long offset) =>
            (uint)(ReadWord(offset) << 16 | ReadWord(offset + 2));

        public void WriteByte(long offset, byte value)
        {
            if (offset < usb.Fifo.Length)
            {
                usb.Fifo[offset] = value;
                usb.FifoWritten((int)offset + 1);
            }
        }

        public void WriteWord(long offset, ushort value)
        {
            WriteByte(offset, (byte)(value >> 8));
            WriteByte(offset + 1, (byte)value);
        }

        public void WriteDoubleWord(long offset, uint value)
        {
            WriteWord(offset, (ushort)(value >> 16));
            WriteWord(offset + 2, (ushort)value);
        }

        private readonly AT32UC3_USBB usb;
    }
}
