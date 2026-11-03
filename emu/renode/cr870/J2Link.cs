// The J2 connector between a host board and the CR870 module, as two peripherals in two machines of one emulation:
//   CR870_SPISlave (module machine, 0xB1002000): the DM870's SPI slave block + what the host drives on J2;
//   J2HostPort     (host machine, an ISPIPeripheral on the host's SPI master + its power and request pins).
//
// Messages between them carry the sender's virtual time and take effect exactly one global quantum later on the
// other side. Renode delivers a cross-machine event at the receiver's next synchronisation point, which is the end
// of the quantum the sender was in; adding the quantum to the timestamp gives a constant latency that preserves
// the spacing of bytes on the wire and does not depend on how fast either machine ran. Both machines are
// therefore deterministic together, whatever the host load.
//
// The protocol, as method calls:
//   host -> module:  Byte(b, t) (enters the RX FIFO at t + Q), Credit(n) (the host clocked out n TX bytes),
//                    Power(on)
//   module -> host:  Tx(b) (a byte the firmware put in its TX FIFO), Idle(b) (sent while the FIFO is empty),
//                    Request(level), Reset() (chip reset: FIFOs gone)
// The host side mirrors the module's TX FIFO and answers every SPI byte from it at once; a TX byte keeps its FIFO
// slot on the module until the host reports it clocked out, so BL2's flow control (waiting for TX space) works.
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
using Antmicro.Renode.Peripherals.SPI;
using Antmicro.Renode.Peripherals.UC3;
using Antmicro.Renode.Time;

namespace Antmicro.Renode.Peripherals.CR870
{
    /// <summary>What the module end of the link talks to: the host board's port, or a test host.</summary>
    public interface IJ2Host
    {
        IMachine Machine { get; }
        void Tx(byte b);
        void Idle(byte b);
        void Request(bool level);
        void ModuleReset(byte idle, bool level);
        void Audio(int[] lr); // I2S frames from the module (L, R pairs, 24-bit)
    }

    /// <summary>Finds the two ends of a J2 link by name (one emulation per process).</summary>
    public static class J2Registry
    {
        public static void Add(string name, CR870_SPISlave module)
        {
            lock (Modules)
            {
                Modules[name] = module;
            }
        }

        public static void Add(string name, IJ2Host host)
        {
            lock (Hosts)
            {
                Hosts[name] = host;
            }
        }

        public static CR870_SPISlave Module(string name)
        {
            lock (Modules)
            {
                return Modules.TryGetValue(name, out var m) ? m : null;
            }
        }

        public static IJ2Host Host(string name)
        {
            lock (Hosts)
            {
                return Hosts.TryGetValue(name, out var h) ? h : null;
            }
        }

        /// <summary>Global quantum of the emulation in ns: the link latency.</summary>
        public static ulong QuantumNs =>
            EmulationManager.Instance.CurrentEmulation.MasterTimeSource.Quantum.Ticks;

        internal static ulong Now(IMachine machine)
        {
            if (machine.SystemBus.TryGetCurrentCPU(out var cpu))
            {
                cpu.SyncTime();
            }
            return machine.ElapsedVirtualTime.TimeElapsed.Ticks;
        }

        /// <summary>Run on the other machine at the sender's time + one quantum.</summary>
        internal static void Post(IMachine from, IMachine to, Action action, string name)
        {
            var t = Now(from) + QuantumNs;
            to.HandleTimeDomainEvent<object>(
                _ =>
                {
                    var now = to.ElapsedVirtualTime.TimeElapsed.Ticks;
                    if (t <= now)
                    {
                        action();
                    }
                    else
                    {
                        to.ScheduleAction(
                            TimeInterval.FromNanoseconds(t - now),
                            __ => action(),
                            name
                        );
                    }
                },
                null,
                new TimeStamp(from.ElapsedVirtualTime.TimeElapsed, from.LocalTimeSource.Domain)
            );
        }

        private static readonly Dictionary<string, CR870_SPISlave> Modules =
            new Dictionary<string, CR870_SPISlave>();
        private static readonly Dictionary<string, IJ2Host> Hosts =
            new Dictionary<string, IJ2Host>();
    }

    /// <summary>
    /// The DM870 SPI slave (0xB1002000): RX +0xE00 / fill +0xF18, TX +0xE80 / free +0xF1C, status +0xF44 (0x100 RX,
    /// 0x400 TX space), mask set/clr +0xF50/+0xF54, +0x00C = byte sent while the TX FIFO is empty (0xFF). IRQ 0 =
    /// level line 20. Also the module end of the J2 link named Link.
    /// </summary>
    public class CR870_SPISlave
        : IDoubleWordPeripheral,
            IWordPeripheral,
            IBytePeripheral,
            IKnownSize,
            INumberedGPIOOutput,
            IRegisterablePeripheral<J2TestHost, NullRegistrationPoint>
    {
        /// <summary>A test host registered under the slave (module-only runs): spis.testhost.</summary>
        public void Register(J2TestHost peripheral, NullRegistrationPoint registrationPoint)
        {
            machine.RegisterAsAChildOf(this, peripheral, registrationPoint);
        }

        public void Unregister(J2TestHost peripheral)
        {
            machine.UnregisterAsAChildOf(this, peripheral);
        }

        public CR870_SPISlave(IMachine machine, CR870_Core core, string link = "j2")
        {
            this.machine = machine;
            this.core = core;
            Link = link;
            J2Registry.Add(link, this);
            Connections = new Dictionary<int, IGPIO> { { 0, new GPIO() } };
            core.RequestLine += lvl => ToHost(h => h.Request(lvl));
            core.ChipReset += OnChipReset;
            core.I2SFrame += (l, r) =>
            {
                // the I2S lines on J2: batched per ~1.5 ms (one event per batch, the frames keep their order)
                audio.Add(l);
                audio.Add(r);
                if (audio.Count >= 128)
                {
                    var b = audio.ToArray();
                    audio.Clear();
                    ToHost(h => h.Audio(b));
                }
            };
            Reset();
        }

        public IReadOnlyDictionary<int, IGPIO> Connections { get; }

        public string Link { get; }

        public long Size => 0x1000;

        /// <summary>
        /// A CPU that keeps finding the slave idle in a tight loop (16 status/fill reads with RX empty, each within 3 us
        /// of the last) sleeps until the next link event (byte, credit, power, reset) or 1 ms: only those change what
        /// it polls (our module/boot images, zd1-linkd and BL2 poll; the stock app uses the IRQ). Default on.
        /// </summary>
        public bool PollHalt { get; set; } = true;

        public ulong PollHalts { get; private set; }

        public void Reset()
        {
            Wake();
            rx.Clear();
            txUsed = 0;
            imask = 0;
            idle = 0xFF;
            regs.Clear();
            regs[0xFF8] = 0x06110801;
            Update();
        }

        public uint ReadDoubleWord(long offset) => Read(offset);

        public ushort ReadWord(long offset) => (ushort)Read(offset);

        public byte ReadByte(long offset) => (byte)Read(offset);

        public void WriteDoubleWord(long offset, uint value) => Write(offset, value);

        public void WriteWord(long offset, ushort value) => Write(offset, value);

        public void WriteByte(long offset, byte value) => Write(offset, value);

        private void IdlePoll()
        {
            if (!PollHalt || rx.Count > 0)
            {
                pollStreak = 0;
                return;
            }
            var now = core.NowNs;
            pollStreak = now - lastPoll < 3000 ? pollStreak + 1 : 0;
            lastPoll = now;
            if (
                pollStreak < 16
                || !(machine.SystemBus.TryGetCurrentCPU(out var c) && c is BaseCPU cpu)
            )
            {
                return;
            }
            pollStreak = 0;
            PollHalts++;
            haltedCpu = cpu;
            var gen = ++pollGen;
            cpu.IsHalted = true;
            machine.ScheduleAction(
                TimeInterval.FromMicroseconds(1000),
                _ =>
                {
                    if (gen == pollGen)
                    {
                        Wake();
                    }
                },
                "spis-poll"
            );
        }

        private void Wake()
        {
            pollGen++;
            if (haltedCpu != null)
            {
                var c = haltedCpu;
                haltedCpu = null;
                if (core.Power)
                {
                    c.IsHalted = false;
                }
            }
        }

        // ---- from the host (already at their time) ----
        public void HostByte(byte b)
        {
            Wake();
            if (!core.PadsRouted)
            {
                return; // the SPI-slave pads are not routed yet (switch matrix): nothing arrives
            }
            if (rx.Count < 255)
            {
                rx.Enqueue(b);
            }
            else if (++overruns <= 20)
            {
                core.Log.Log(
                    string.Format("SPI RX FIFO overrun, byte {0:x2} lost (#{1})", b, overruns)
                );
            }
            Update();
        }

        public void HostCredit(int n)
        {
            Wake();
            txUsed = Math.Max(0, txUsed - n);
            Update();
        }

        public void HostPower(bool on)
        {
            core.Power = on;
        }

        /// <summary>The factory-reset strap (module GPIO 4, J2 pin 68) as the host drives it (MCU PB01).</summary>
        public void HostStrap(bool high)
        {
            core.FactoryResetStrap = high;
        }

        private void OnChipReset()
        {
            pollGen++;
            haltedCpu = null; // the reset decides whether the CPU runs
            Reset();
            ToHost(h => h.ModuleReset(idle, core.RequestLevel));
        }

        private void ToHost(Action<IJ2Host> a)
        {
            var h = J2Registry.Host(Link);
            if (h != null)
            {
                J2Registry.Post(machine, h.Machine, () => a(h), "j2-to-host");
            }
        }

        private uint Status => (rx.Count > 0 ? 0x100u : 0) | (txUsed < 63 ? 0x400u : 0);

        private void Update()
        {
            Connections[0].Set((Status & imask & 0x700) != 0);
        }

        private uint Read(long off)
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
                    IdlePoll();
                    return (uint)rx.Count;
                case 0xF1C:
                    return (uint)(63 - txUsed);
                case 0xF44:
                    IdlePoll();
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
            if (core.Log.Tracing)
            {
                core.Log.Trace(string.Format("spis W {0:x3} <- {1:x8} pc={2:x8}", off, v, core.Pc));
            }
            switch (off)
            {
                case 0x00C:
                    idle = (byte)v;
                    regs[off] = v;
                    ToHost(h => h.Idle((byte)v));
                    return;
                case 0xE80:
                    if (txUsed < 63)
                    {
                        txUsed++;
                        var b = (byte)v;
                        if (core.PadsRouted)
                        {
                            ToHost(h => h.Tx(b)); // pads not routed: the host sees the idle byte only
                        }
                    }
                    Update();
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

        public IMachine Machine => machine;

        private readonly IMachine machine;
        private BaseCPU haltedCpu;
        private ulong pollGen,
            lastPoll;
        private int pollStreak;
        private readonly List<int> audio = new List<int>();
        private readonly CR870_Core core;
        private readonly Queue<byte> rx = new Queue<byte>();
        private readonly Dictionary<long, uint> regs = new Dictionary<long, uint>();
        private int txUsed;
        private uint imask,
            overruns;
        private byte idle;
    }

    /// <summary>
    /// The host end of the J2 link (on the Zeppelin: SPI0/NPCS2, power PA04 + reset PA17 released, request line PA20).
    /// Register it on the host's SPI master at its chip select; it watches the host's GPIO for the power pins.
    /// </summary>
    public class J2HostPort : ISPIPeripheral, IJ2Host
    {
        public J2HostPort(IMachine machine, AT32UC3_GPIO gpio, string link = "j2")
        {
            this.machine = machine;
            this.gpio = gpio;
            Link = link;
            trace = UC3Trace.For(machine);
            J2Registry.Add(link, this);
            gpio.PortChanged += p =>
            {
                if (p == 0)
                {
                    Pins();
                }
                else if (p == 1)
                {
                    Strap();
                }
            };
        }

        public string Link { get; }

        public IMachine Machine => machine;

        public void Reset()
        {
            // the host chip reset: its pins go back to inputs (power off follows from the GPIO reset)
        }

        public byte Transmit(byte b)
        {
            if (!running)
            {
                return 0xFF;
            }
            byte o;
            var credit = false;
            if (mirror.Count > 0)
            {
                o = mirror.Dequeue();
                credit = true;
            }
            else
            {
                o = idle;
            }
            ToModule(m =>
            {
                m.HostByte(b);
                if (credit)
                {
                    m.HostCredit(1);
                }
            });
            return o;
        }

        public void FinishTransmission() { }

        // ---- from the module (already at their time) ----
        public void Tx(byte b)
        {
            if (running && mirror.Count < 65536)
            {
                mirror.Enqueue(b);
            }
        }

        public void Idle(byte b)
        {
            idle = b;
        }

        public void Request(bool level)
        {
            request = level;
            gpio.OnGPIO(20, running && level);
        }

        public void ModuleReset(byte idleByte, bool level)
        {
            mirror.Clear();
            idle = idleByte;
            Request(level);
        }

        /// <summary>The DSP the module's I2S goes into (on the board: straight into the ADAU1445).</summary>
        public ZD1.ZD1_DSP Dsp { get; set; }

        public void Audio(int[] lr)
        {
            if (running && Dsp != null)
            {
                for (var i = 0; i + 1 < lr.Length; i += 2)
                {
                    Dsp.Frame(lr[i], lr[i + 1]);
                }
            }
        }

        private void Pins()
        {
            var run = gpio.OutHigh(0, 4) && gpio.OutHigh(0, 17);
            if (run == running)
            {
                return;
            }
            running = run;
            trace.Log(
                run
                    ? "*** DM870 running (PA04 on, PA17 released): module powered over J2"
                    : string.Format(
                        "*** DM870 stopped ({0})",
                        gpio.OutHigh(0, 4) ? "PA17 reset asserted" : "PA04 power off"
                    ),
                true
            );
            if (run)
            {
                gpio.OnGPIO(20, request); // a module that was already up (boot-first) keeps its line
            }
            else
            {
                mirror.Clear(); // module off: its FIFO and line are gone
                idle = 0xFF;
                request = false;
                gpio.OnGPIO(20, false);
            }
            ToModule(m => m.HostPower(run));
        }

        // PB01 is wired to the module's factory-reset strap (GPIO 4, active high, sampled at boot) [HW 2026-10-23]
        private void Strap()
        {
            var high = gpio.OutHigh(1, 1);
            if (high == strap)
            {
                return;
            }
            strap = high;
            ToModule(m => m.HostStrap(high));
        }

        private void ToModule(Action<CR870_SPISlave> a)
        {
            var m = J2Registry.Module(Link);
            if (m != null)
            {
                J2Registry.Post(machine, m.Machine, () => a(m), "j2-to-module");
            }
        }

        private readonly IMachine machine;
        private readonly AT32UC3_GPIO gpio;
        private readonly UC3Trace trace;
        private readonly Queue<byte> mirror = new Queue<byte>();
        private byte idle = 0xFF;
        private bool running,
            request,
            strap;
    }

    // ---- the test host (was J2TestHost.cs) ----
    // A test host on the module's J2 SPI link, in the module's own machine (zemu run --spi, module alone): rd,
    // rdv, wr, wrv, req, run in virtual time. It speaks the stock register protocol
    // (docs/protocol/spi-registers.md): F read `01 reg 00 00`, V read `03 reg 00 00`, write `00 reg b0 b1`, V write
    // `02 reg len16le data`; the response comes after a chip select cycle, 0x27 stuffing on data.
    //
    //   spis.testhost Run "rd 0x11 rdv 0x38"      (results appended to LogFile)
    //   spis.testhost Busy                         (true while a command list runs)

    public class J2TestHost : IJ2Host, IPeripheral
    {
        public J2TestHost(IMachine machine, CR870_SPISlave spis)
        {
            this.machine = machine;
            this.spis = spis;
            J2Registry.Add(spis.Link, this);
        }

        public IMachine Machine => machine;

        public string LogFile { get; set; }

        /// <summary>Virtual seconds to wait for the module's reply (default 0.5).</summary>
        public double ReplyTimeout { get; set; } = 0.5;

        /// <summary>ns per SPI byte on the wire (1 Mbit/s).</summary>
        public ulong ByteNs { get; set; } = 8000;

        public bool Busy => job != null;

        /// <summary>The output of the last command list.</summary>
        public string LastOutput => output.ToString();

        public void Reset() { }

        public void Run(string commands)
        {
            output.Clear();
            job = Commands(commands.Split(new[] { ' ' }, StringSplitOptions.RemoveEmptyEntries))
                .GetEnumerator();
            // start inside the time domain (the monitor thread must not add clock entries mid-emulation)
            machine.LocalTimeSource.ExecuteInNearestSyncedState(_ => Step());
        }

        // ---- IJ2Host (from the module) ----
        public void Tx(byte b) => mirror.Enqueue(b);

        public void Idle(byte b) => idle = b;

        public void Request(bool level) => request = level;

        public void Audio(int[] lr) { }

        public void ModuleReset(byte idleByte, bool level)
        {
            mirror.Clear();
            idle = idleByte;
            request = level;
        }

        // ---- the coroutine: yields the virtual ns to wait before the next step ----
        private void Step()
        {
            if (job == null)
            {
                return;
            }
            if (!job.MoveNext())
            {
                job = null;
                return;
            }
            machine.ScheduleAction(
                TimeInterval.FromNanoseconds(Math.Max(1ul, job.Current)),
                _ => Step(),
                "j2-testhost"
            );
        }

        private ulong Now => machine.ElapsedVirtualTime.TimeElapsed.Ticks;

        private byte lastReply;

        // one byte exchange: the reply is what the module has in its TX FIFO now (else its idle byte)
        private IEnumerable<ulong> X(byte b)
        {
            var credit = false;
            if (mirror.Count > 0)
            {
                lastReply = mirror.Dequeue();
                credit = true;
            }
            else
            {
                lastReply = idle;
            }
            var c = credit;
            J2Registry.Post(
                machine,
                spis.Machine,
                () =>
                {
                    spis.HostByte(b);
                    if (c)
                    {
                        spis.HostCredit(1);
                    }
                },
                "j2-testhost-byte"
            );
            yield return ByteNs;
        }

        private IEnumerable<ulong> Put(byte b)
        {
            if (b == 0x27)
            {
                foreach (var t in X(0x27))
                {
                    yield return t;
                }
                foreach (var t in X(0x27))
                {
                    yield return t;
                }
            }
            else if (b == 0xFF)
            {
                foreach (var t in X(0x27))
                {
                    yield return t;
                }
                foreach (var t in X(0x01))
                {
                    yield return t;
                }
            }
            else
            {
                foreach (var t in X(b))
                {
                    yield return t;
                }
            }
        }

        private int got;

        // get(): the next data byte (unstuffed), or -1 after the reply timeout
        private IEnumerable<ulong> Get()
        {
            var end = Now + (ulong)(ReplyTimeout * 1e9);
            got = -1;
            while (Now < end)
            {
                foreach (var t in X(0xFF))
                {
                    yield return t;
                }
                if (lastReply == 0xFF)
                {
                    continue;
                }
                if (lastReply != 0x27)
                {
                    got = lastReply;
                    yield break;
                }
                foreach (var t in X(0xFF))
                {
                    yield return t;
                }
                got = lastReply == 0x01 ? 0xFF : lastReply;
                yield break;
            }
        }

        private bool found;

        private IEnumerable<ulong> SkipTo(byte want)
        {
            var end = Now + (ulong)(ReplyTimeout * 1e9);
            found = false;
            while (Now < end)
            {
                foreach (var t in X(0xFF))
                {
                    yield return t;
                }
                if (lastReply == want)
                {
                    found = true;
                    yield break;
                }
            }
        }

        private IEnumerable<ulong> RequestFrame(byte typ, byte reg)
        {
            foreach (var b in new byte[] { typ, reg, 0, 0 })
            {
                foreach (var t in X(b))
                {
                    yield return t;
                }
            }
            yield return 5000000; // CS off, 5 ms, CS on (the module answers on the next CS)
        }

        private void Print(string s)
        {
            output.Append(s).Append('\n');
            if (!string.IsNullOrEmpty(LogFile))
            {
                File.AppendAllText(LogFile, s + "\n");
            }
        }

        private static int Num(string s) =>
            s.StartsWith("0x", StringComparison.OrdinalIgnoreCase)
                ? Convert.ToInt32(s.Substring(2), 16)
                : int.Parse(s);

        private IEnumerable<ulong> Commands(string[] a)
        {
            var i = 0;
            while (i < a.Length)
            {
                var c = a[i++];
                if (c == "rd")
                {
                    var reg = (byte)Num(a[i++]);
                    foreach (var t in RequestFrame(0x01, reg))
                    {
                        yield return t;
                    }
                    foreach (var t in SkipTo(0x01))
                    {
                        yield return t;
                    }
                    var ok = found;
                    if (ok)
                    {
                        foreach (var t in SkipTo(reg))
                        {
                            yield return t;
                        }
                        ok = found;
                    }
                    if (!ok)
                    {
                        Print(string.Format("rd 0x{0:x2} -> timeout", reg));
                        continue;
                    }
                    foreach (var t in Get())
                    {
                        yield return t;
                    }
                    var v0 = got;
                    foreach (var t in Get())
                    {
                        yield return t;
                    }
                    var v1 = got;
                    Print(string.Format("rd 0x{0:x2} -> {1:x2} {2:x2}", reg, v0 & 0xFF, v1 & 0xFF));
                }
                else if (c == "rdv")
                {
                    var reg = (byte)Num(a[i++]);
                    foreach (var t in RequestFrame(0x03, reg))
                    {
                        yield return t;
                    }
                    foreach (var t in SkipTo(0x03))
                    {
                        yield return t;
                    }
                    var ok = found;
                    if (ok)
                    {
                        foreach (var t in SkipTo(reg))
                        {
                            yield return t;
                        }
                        ok = found;
                    }
                    if (!ok)
                    {
                        Print(string.Format("rdv 0x{0:x2} -> timeout", reg));
                        continue;
                    }
                    foreach (var t in Get())
                    {
                        yield return t;
                    }
                    var lo = got;
                    foreach (var t in Get())
                    {
                        yield return t;
                    }
                    var n = (lo & 0xFF) | (got & 0xFF) << 8;
                    var d = new List<byte>();
                    for (var k = 0; k < Math.Min(n, 256); k++)
                    {
                        foreach (var t in Get())
                        {
                            yield return t;
                        }
                        d.Add((byte)got);
                    }
                    var hex = string.Join(" ", d.Take(64).Select(x => x.ToString("x2")));
                    var txt = new string(
                        d.Take(64).Select(x => x >= 0x20 && x < 0x7F ? (char)x : '�').ToArray()
                    );
                    Print(string.Format("rdv 0x{0:x2} -> {1} bytes {2} |{3}|", reg, n, hex, txt));
                }
                else if (c == "wr")
                {
                    var reg = (byte)Num(a[i++]);
                    var b0 = (byte)Num(a[i++]);
                    var b1 = (byte)Num(a[i++]);
                    foreach (var t in X(0))
                    {
                        yield return t;
                    }
                    foreach (var t in X(reg))
                    {
                        yield return t;
                    }
                    foreach (var t in Put(b0))
                    {
                        yield return t;
                    }
                    foreach (var t in Put(b1))
                    {
                        yield return t;
                    }
                    Print(string.Format("wr 0x{0:x2} <- {1:x2} {2:x2}", reg, b0, b1));
                }
                else if (c == "wrv")
                {
                    var reg = (byte)Num(a[i++]);
                    var hex = a[i++];
                    var d = Enumerable
                        .Range(0, hex.Length / 2)
                        .Select(k => Convert.ToByte(hex.Substring(2 * k, 2), 16))
                        .ToArray();
                    foreach (var t in X(0x02))
                    {
                        yield return t;
                    }
                    foreach (var t in X(reg))
                    {
                        yield return t;
                    }
                    foreach (var t in Put((byte)(d.Length & 0xFF)))
                    {
                        yield return t;
                    }
                    foreach (var t in Put((byte)(d.Length >> 8)))
                    {
                        yield return t;
                    }
                    foreach (var b in d)
                    {
                        foreach (var t in Put(b))
                        {
                            yield return t;
                        }
                    }
                    Print(
                        string.Format(
                            "wrv 0x{0:x2} <- {1}",
                            reg,
                            string.Join(" ", d.Select(x => x.ToString("x2")))
                        )
                    );
                }
                else if (c == "req")
                {
                    Print("request line " + (request ? 1 : 0));
                }
            }
        }

        private readonly IMachine machine;
        private readonly CR870_SPISlave spis;
        private readonly Queue<byte> mirror = new Queue<byte>();
        private readonly StringBuilder output = new StringBuilder();
        private IEnumerator<ulong> job;
        private byte idle = 0xFF;
        private bool request;
    }
}
