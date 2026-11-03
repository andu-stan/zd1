// B&W Zeppelin Air main board ("ZD1") around the AT32UC3A1256: what the MCU firmware sees outside the chip.
//
//   I2C (TWI)  ADAU1445 DSP 0x38 (2-byte sub-address), AD193x codec 0x04, 24C02 EEPROM 0x50, Apple auth
//              coprocessor 0x10 (register stub); the bus is held while the DSP is unpowered (PB03 low)
//   CS2000     clock synthesiser on bit-banged I2C PB13 (SCL) / PB14 (SDA); its ratio sets the I2S rate
//   J2         the CR870 network module: SPI0/NPCS2, power PA04, reset PA17, request line PA20. Either
//              ZD1_ModuleStandIn (status 0x2000/0x2100, a few notifications) or the link to a CR870 machine
//   keys/IR    ON/OFF on PA08 (Key = held at power-up), RC6 receiver on PA03 (IR)
//   LED        RGB on PWM channels 1-3 ("LED rgb" trace lines)
//   ADC        ADC2 board variant, ADC4/ADC5 protections (Adc = "ch:val,...")
using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Text;
using Antmicro.Renode.Core;
using Antmicro.Renode.Logging;
using Antmicro.Renode.Peripherals.Bus;
using Antmicro.Renode.Peripherals.I2C;
using Antmicro.Renode.Peripherals.SPI;
using Antmicro.Renode.Peripherals.UC3;
using Antmicro.Renode.Time;

namespace Antmicro.Renode.Peripherals.ZD1
{
    /// <summary>The board glue: input scripts, keys, IR, LED, ADC levels, the I2C bus hold, the CS2000.
    /// Registered on the system bus at an unused address only so the monitor and scripts can reach it (reads 0).</summary>
    public class ZD1Board : IDoubleWordPeripheral, IKnownSize, IUC3I2SClock
    {
        public long Size => 0x10;

        public uint ReadDoubleWord(long offset) => 0;

        public void WriteDoubleWord(long offset, uint value) { }

        public ZD1Board(
            IMachine machine,
            AT32UC3_GPIO gpio,
            AT32UC3_TWI twi,
            AT32UC3_PWM pwm,
            AT32UC3_ADC adc
        )
        {
            this.machine = machine;
            this.gpio = gpio;
            trace = UC3Trace.For(machine);
            pwm.DutyWritten += OnDuty;
            twi.BusHeld = () => TwiStuckUnpowered && !gpio.OutHigh(1, 3);
            gpio.PortChanged += p =>
            {
                if (p == 1)
                {
                    CsPins();
                }
            };
            // healthy variant-1 unit (ADC2 >= 0x200 = variant 2; ADC5 < 0x1FF / ADC4 < 0x1BC = fault)
            adc.SetChannel(2, 0x100);
            adc.SetChannel(4, 0x300);
            adc.SetChannel(5, 0x300);
            adc.SetChannel(6, 0x300);
            this.adc = adc;
            cs.Scl = cs.Sda = true;
        }

        public void Reset()
        {
            // chip reset: the board stays powered (CS2000 keeps its registers); the bit-bang state restarts
            cs.Bits = cs.NBytes = 0;
            cs.Pull = cs.Addressed = false;
            cs.Scl = cs.Sda = true;
        }

        // ---- knobs (monitor: board ...; zemu run --renode "zd1:board ...") ----

        /// <summary>The bus hangs while the DSP is unpowered (ZD1_TWI_STUCK_UNPOWERED, default on).</summary>
        public bool TwiStuckUnpowered { get; set; } = true;

        /// <summary>ON/OFF held at power-up (released by a Pins event, e.g. "3000:PA08=1").</summary>
        public bool Key
        {
            get => key;
            set
            {
                key = value;
                gpio.OnGPIO(8, !value);
            }
        }

        /// <summary>Scripted input levels at virtual ms: "ms:PB00=0,ms:PA24=1,..." (ZD1_PINS).</summary>
        public string Pins
        {
            get => pins;
            set
            {
                pins = value;
                foreach (
                    var tok in (value ?? "").Split(
                        new[] { ',' },
                        StringSplitOptions.RemoveEmptyEntries
                    )
                )
                {
                    var m = System.Text.RegularExpressions.Regex.Match(
                        tok.Trim(),
                        @"^(\d+):P([A-D])(\d+)=(\d)$"
                    );
                    if (!m.Success)
                    {
                        continue;
                    }
                    var ms = ulong.Parse(m.Groups[1].Value);
                    var pin = (m.Groups[2].Value[0] - 'A') * 32 + int.Parse(m.Groups[3].Value);
                    var v = m.Groups[4].Value != "0";
                    At(
                        ms * 1000000,
                        () =>
                        {
                            gpio.OnGPIO(pin, v);
                            trace.Log(
                                string.Format(
                                    "*** INPUT P{0}{1:00} = {2} (script)",
                                    (char)('A' + pin / 32),
                                    pin % 32,
                                    v ? 1 : 0
                                )
                            );
                        }
                    );
                }
            }
        }

        /// <summary>RC6 frames on the IR receiver (PA03): "ms:cmd[:addr[:repeat]],..." (address default 0xD2).</summary>
        public string IR
        {
            get => ir;
            set
            {
                ir = value;
                IrParse(value ?? "");
            }
        }

        /// <summary>ADC inputs "ch:val,..." (10 bit).</summary>
        public string Adc
        {
            get => adcSpec;
            set
            {
                adcSpec = value;
                foreach (
                    var tok in (value ?? "").Split(
                        new[] { ',' },
                        StringSplitOptions.RemoveEmptyEntries
                    )
                )
                {
                    var p = tok.Split(':');
                    if (p.Length == 2)
                    {
                        adc.SetChannel(
                            int.Parse(p[0]),
                            (int)Convert.ToUInt32(p[1], p[1].StartsWith("0x") ? 16 : 10)
                        );
                    }
                }
            }
        }

        /// <summary>
        /// "addr:len,...": every virtual ms, log changes of these guest words (len in bytes, multiple of 4, max 64)
        /// as "*** WATCH addr = value (was old)" in the MCU trace (ZD1_WATCH). E.g. stock's UI/key word 0x7d18:4.
        /// </summary>
        public string Watch
        {
            get => watchSpec;
            set
            {
                watchSpec = value;
                watches.Clear();
                foreach (
                    var tok in (value ?? "").Split(
                        new[] { ',' },
                        StringSplitOptions.RemoveEmptyEntries
                    )
                )
                {
                    var p = tok.Split(':');
                    if (p.Length == 2 && watches.Count < 8)
                    {
                        var len = Math.Min(64, int.Parse(p[1])) & ~3;
                        watches.Add((Convert.ToUInt32(p[0], 16), new uint[len / 4], false));
                    }
                }
                if (watches.Count > 0)
                {
                    At(1000000, WatchTick);
                }
            }
        }

        /// <summary>
        /// "ms:addr:or,...": at virtual time ms, OR a value into the guest word at addr (ZD1_POKE). Raises stock
        /// firmware events, e.g. "8000:0x2fa4:0x1000" (USB audio activation).
        /// </summary>
        public string Poke
        {
            get => pokeSpec;
            set
            {
                pokeSpec = value;
                foreach (
                    var tok in (value ?? "").Split(
                        new[] { ',' },
                        StringSplitOptions.RemoveEmptyEntries
                    )
                )
                {
                    var p = tok.Split(':');
                    if (p.Length != 3)
                    {
                        continue;
                    }
                    var a = Convert.ToUInt32(p[1], 16);
                    var v = Convert.ToUInt32(p[2], p[2].StartsWith("0x") ? 16 : 10);
                    At(
                        ulong.Parse(p[0]) * 1000000,
                        () =>
                        {
                            var w = machine.SystemBus.ReadDoubleWord(a) | v; // the bus is big-endian like the CPU
                            machine.SystemBus.WriteDoubleWord(a, w);
                            trace.Log(
                                string.Format("*** POKE {0:x8} |= {1:x8} -> {2:x8}", a, v, w)
                            );
                        }
                    );
                }
            }
        }

        private void WatchTick()
        {
            for (var i = 0; i < watches.Count; i++)
            {
                var (addr, last, init) = watches[i];
                for (var o = 0; o < last.Length; o++)
                {
                    var v = machine.SystemBus.ReadDoubleWord(addr + (ulong)(4 * o));
                    if (!init || v != last[o])
                    {
                        trace.Log(
                            string.Format(
                                "*** WATCH {0:x8} = {1:x8} (was {2:x8})",
                                addr + 4 * o,
                                v,
                                last[o]
                            )
                        );
                    }
                    last[o] = v;
                }
                watches[i] = (addr, last, true);
            }
            machine.ScheduleAction(TimeInterval.FromMilliseconds(1), _ => WatchTick(), "zd1-watch");
        }

        /// <summary>Sample rate the CS2000 makes for the I2S/SSC: 12 MHz x ratio / 2^20 / 256.</summary>
        public double SscFs
        {
            get
            {
                var r =
                    (uint)cs.Regs[6] << 24
                    | (uint)cs.Regs[7] << 16
                    | (uint)cs.Regs[8] << 8
                    | cs.Regs[9];
                return 12e6 * r / 1048576.0 / 256.0;
            }
        }

        /// <summary>The CS2000 ratio is programmed: the codec PLL has its MCLK.</summary>
        public bool McklPresent => (cs.Regs[6] | cs.Regs[7] | cs.Regs[8] | cs.Regs[9]) != 0;

        public byte[] Cs2000Registers => cs.Regs;

        // ---- time helpers ----

        private void At(ulong ns, Action a)
        {
            void Schedule()
            {
                var now = (ulong)machine.ElapsedVirtualTime.TimeElapsed.Ticks;
                machine.ScheduleAction(
                    TimeInterval.FromNanoseconds(ns > now ? ns - now : 0),
                    _ => a(),
                    "zd1-input"
                );
            }
            if (machine.SystemBus.TryGetCurrentCPU(out var _))
            {
                Schedule(); // on the CPU thread
            }
            else
            {
                machine.LocalTimeSource.ExecuteInNearestSyncedState(_ => Schedule()); // from the monitor
            }
        }

        // ---- RC6 mode 0 transmitter: leader 6T mark + 2T space, start 1, mode 000, trailer (toggle) 2T, address,
        // command MSB first; T = 444 us; receiver output active low (mark = low) ----
        private void IrParse(string spec)
        {
            var edges = new List<(ulong at, bool level)>();
            var toggle = 0;
            foreach (var tok in spec.Split(new[] { ',' }, StringSplitOptions.RemoveEmptyEntries))
            {
                var p = tok.Split(':');
                if (p.Length < 2)
                {
                    continue;
                }
                var ms = ulong.Parse(p[0]);
                var cmd = (int)Convert.ToUInt32(p[1], p[1].StartsWith("0x") ? 16 : 10);
                var addr =
                    p.Length > 2
                        ? (int)Convert.ToUInt32(p[2], p[2].StartsWith("0x") ? 16 : 10)
                        : 0xD2;
                var rep = p.Length > 3 ? int.Parse(p[3]) : 1;
                var t = ms * 1000000;
                for (var r = 0; r < rep; r++, t += 107000000)
                {
                    var ft = t;
                    void Half(bool level, int halves)
                    {
                        if (edges.Count == 0 || edges[edges.Count - 1].level != level)
                        {
                            edges.Add((ft, level));
                        }
                        ft += (ulong)halves * 444000;
                    }
                    void Bit(int b, int halves)
                    {
                        Half(b == 0, halves);
                        Half(b != 0, halves);
                    }
                    Half(false, 6);
                    Half(true, 2);
                    Bit(1, 1);
                    for (var i = 0; i < 3; i++)
                    {
                        Bit(0, 1);
                    }
                    Bit(toggle, 2);
                    for (var i = 7; i >= 0; i--)
                    {
                        Bit((addr >> i) & 1, 1);
                    }
                    for (var i = 7; i >= 0; i--)
                    {
                        Bit((cmd >> i) & 1, 1);
                    }
                    Half(true, 1);
                }
                toggle ^= 1;
                trace.Log(
                    string.Format(
                        "*** IR RC6 scheduled @{0} ms: addr {1:x2} cmd {2:x2} x{3}",
                        ms,
                        addr,
                        cmd,
                        rep
                    )
                );
            }
            foreach (var e in edges)
            {
                var level = e.level;
                At(e.at, () => gpio.OnGPIO(3, level));
            }
        }

        // ---- RGB LED on PWM channels 1-3 ----
        private void OnDuty(int ch, uint v)
        {
            if (ch < 1 || ch > 3)
            {
                return;
            }
            if (led[ch - 1] != v || !ledLogged)
            {
                led[ch - 1] = v;
                ledLogged = true;
                trace.Log(string.Format("  LED rgb {0,3} {1,3} {2,3}", led[0], led[1], led[2]));
            }
        }

        // ---- CS2000-CP on bit-banged I2C: PB13 SCL (push-pull), PB14 SDA (open drain), 8-bit address 0x9C ----
        private sealed class Cs2000
        {
            public bool Scl,
                Sda,
                Pull,
                Addressed;
            public int Bits,
                NBytes;
            public byte Cur;
            public byte[] Buf = new byte[16];
            public byte[] Regs = new byte[0x80];
        }

        private void CsPins()
        {
            var scl = gpio.LevelOut(1, 13);
            var sda = gpio.LevelOut(1, 14);
            if (scl && cs.Scl && sda != cs.Sda)
            {
                if (!sda)
                {
                    cs.Bits = cs.NBytes = 0; // START
                    cs.Cur = 0;
                    cs.Addressed = false;
                    SetPull(false);
                }
                else
                {
                    CsEnd(); // STOP
                }
            }
            else if (scl && !cs.Scl)
            {
                if (cs.Bits < 8) // rising SCL: sample
                {
                    cs.Cur = (byte)(cs.Cur << 1 | (sda ? 1 : 0));
                    cs.Bits++;
                }
            }
            else if (!scl && cs.Scl)
            {
                if (cs.Bits == 8) // byte done: ACK during the 9th clock
                {
                    if (cs.NBytes == 0)
                    {
                        cs.Addressed = cs.Cur == 0x9C;
                    }
                    if (cs.Addressed && cs.NBytes < cs.Buf.Length)
                    {
                        cs.Buf[cs.NBytes++] = cs.Cur;
                    }
                    SetPull(cs.Addressed);
                    cs.Bits = 9;
                }
                else if (cs.Bits == 9) // end of the ACK clock: release
                {
                    SetPull(false);
                    cs.Bits = 0;
                    cs.Cur = 0;
                }
            }
            cs.Scl = scl;
            cs.Sda = sda;
        }

        private void SetPull(bool pull)
        {
            if (cs.Pull != pull)
            {
                cs.Pull = pull;
                gpio.OnGPIO(32 + 14, !pull);
            }
        }

        private void CsEnd()
        {
            if (cs.Addressed && cs.NBytes >= 2)
            {
                var reg = cs.Buf[1] & 0x7F; // bit 7 of the register byte = auto-increment
                var sb = new StringBuilder();
                for (var i = 2; i < cs.NBytes; i++)
                {
                    cs.Regs[(reg + i - 2) & 0x7F] = cs.Buf[i];
                    sb.AppendFormat(" {0:x2}", cs.Buf[i]);
                }
                trace.Log(string.Format("  CS2000 W reg {0:x2} <-{1}", reg, sb));
            }
            cs.Addressed = false;
            cs.NBytes = 0;
        }

        private readonly IMachine machine;
        private readonly AT32UC3_GPIO gpio;
        private readonly AT32UC3_ADC adc;
        private readonly UC3Trace trace;
        private readonly Cs2000 cs = new Cs2000();
        private readonly uint[] led = new uint[3];
        private bool ledLogged,
            key;
        private string pins,
            ir,
            adcSpec,
            watchSpec,
            pokeSpec;
        private readonly List<(uint addr, uint[] last, bool init)> watches =
            new List<(uint, uint[], bool)>();
    }

    /// <summary>
    /// ADAU1445 DSP at 0x38: the 2-byte sub-address space (64 KB, byte stream per write, read back), S/PDIF lock on
    /// the second byte at 0xE0C9 (stock FUN_8001f8e8), the input meter at param 0x031 (5.23), and the sink for the
    /// module's I2S (DspSink: routing E080/E002/param 0x14, master volume param 0x4B, codec DAC mute).
    /// </summary>
    public class ZD1_DSP : IUC3I2CDevice
    {
        public ZD1_DSP(IMachine machine)
        {
            trace = UC3Trace.For(machine);
        }

        public bool SpdifLock { get; set; }

        public byte[] Memory => mem;

        public uint Param(int n) => n >= 0 && n < 0x400 ? param[n] : 0;

        public bool ModuleRouted => e080 == 0x0002 && e002 == 0x0908 && param[0x14] == 0;

        /// <summary>The codec (its DAC mute gates the output); set in the platform.</summary>
        public ZD1_Codec Codec { get; set; }

        /// <summary>The MCU's GPIO, for the amp pins in the DSPOUT lines (PB09 power stage, PB16 unmute).</summary>
        public AT32UC3_GPIO Gpio { get; set; }

        /// <summary>Output WAV (24-bit stereo 44.1 kHz) of what the DSP plays from the module (ZD1_AUDIO_WAV).</summary>
        public string AudioWav
        {
            get => audioWav;
            set
            {
                audioWav = value;
                wav?.Dispose();
                wav = string.IsNullOrEmpty(value) ? null : new FileStream(value, FileMode.Create);
                wframes = 0;
                WavHeader();
            }
        }

        /// <summary>
        /// One I2S frame from the module (24-bit samples): the DSP applies what the MCU programmed (module input routed,
        /// master volume param 0x4B in 5.23, codec DAC mute), writes AudioWav, logs a DSPOUT line per second and keeps
        /// the input meter (param 0x031, peak over 100 ms) current. As the QEMU zd1 DSP sink.
        /// </summary>
        public void Frame(int l, int r)
        {
            var routed = ModuleRouted;
            var dac = Codec == null || !Codec.DacMuted;
            var g = routed && dac ? Coef(param[0x4B]) : 0.0;
            var o = new byte[6];
            var i = 0;
            foreach (var x in new[] { l, r })
            {
                var a = Math.Abs(x);
                ipeak = Math.Max(ipeak, a);
                if (routed)
                {
                    meter = Math.Max(meter, a);
                }
                var y = x * g;
                var v =
                    y > 8388607 ? 8388607
                    : y < -8388608 ? -8388608
                    : (int)Math.Round(y, MidpointRounding.ToEven);
                opeak = Math.Max(opeak, Math.Abs(v));
                if (v != 0)
                {
                    onz++;
                }
                o[i++] = (byte)v;
                o[i++] = (byte)(v >> 8);
                o[i++] = (byte)(v >> 16);
            }
            if (routed)
            {
                routedFrames++;
            }
            if (wav != null)
            {
                wav.Write(o, 0, 6);
                wframes++;
            }
            if (++frames % 4410 == 0)
            {
                SetMeter((uint)Math.Min(meter, 0x7fffff));
                meter = 0;
            }
            if (frames - sfr >= 44100)
            {
                var vol = Coef(param[0x4B]);
                trace.Log(
                    string.Format(
                        "DSPOUT {0} frames: in peak {1:F1} dBFS, out peak {2:F1} dBFS ({3} nonzero), module routed {4}, vol 0x4B {5:x8} ({6:F1} dB), codec DAC {7}, amp PB09 {8} PB16 {9}",
                        frames,
                        Db(ipeak),
                        Db(opeak),
                        onz,
                        routedFrames,
                        param[0x4B],
                        vol > 0 ? 20 * Math.Log10(vol) : -999.0,
                        dac ? "on" : "muted",
                        Gpio != null && Gpio.OutHigh(1, 9) ? 1 : 0,
                        Gpio != null && Gpio.OutHigh(1, 16) ? 1 : 0
                    ),
                    true
                );
                if (wav != null)
                {
                    WavHeader();
                    wav.Flush();
                }
                sfr = frames;
                ipeak = opeak = 0;
                onz = routedFrames = 0;
            }
        }

        private static double Coef(uint v) => (int)(v << 4) / 16.0 / 8388608.0;

        private static double Db(int peak) =>
            peak != 0 ? 20 * Math.Log10(peak / 8388608.0) : -999.0;

        private void WavHeader()
        {
            if (wav == null)
            {
                return;
            }
            var data = (uint)(wframes * 6);
            var h = new uint[]
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
            };
            var pos = wav.Position;
            wav.Position = 0;
            foreach (var w in h)
            {
                wav.Write(BitConverter.GetBytes(w), 0, 4);
            }
            wav.Position = Math.Max(pos, 44);
        }

        public void Reset()
        {
            readSub = 0;
            reads = 0;
        }

        public void ReadAddress(byte[] a)
        {
            readSub = a.Length >= 2 ? (a[0] << 8 | a[1]) : 0;
            reads = 0;
        }

        public byte[] Read(int count = 1)
        {
            var r = new byte[count];
            for (var i = 0; i < count; i++)
            {
                var off = reads++;
                if (readSub == 0xE0C9)
                {
                    r[i] = off == 1 ? (byte)(SpdifLock ? 1 : 0) : (byte)0;
                }
                else
                {
                    r[i] = mem[(readSub + off) & 0xFFFF];
                }
            }
            return r;
        }

        public void Write(byte[] b)
        {
            if (b.Length < 2)
            {
                return;
            }
            var sub = b[0] << 8 | b[1];
            var len = b.Length - 2;
            Array.Copy(b, 2, mem, sub, Math.Min(len, 0x10000 - sub));
            SinkWrite(sub, b, 2, len);
            var sb = new StringBuilder();
            for (var i = 0; i < len && i < 8; i++)
            {
                sb.AppendFormat(" {0:x2}", b[2 + i]);
            }
            trace.Log(
                string.Format(
                    "  I2C DSP  W {0:x4} len {1,5} crc {2:x8} :{3}",
                    sub,
                    len,
                    Crc32(b, 2, len),
                    sb
                )
            );
        }

        public void FinishTransmission() { }

        /// <summary>The peak input level over ~100 ms as a 5.23 word at param 0x031 (what `dsp rd 0x031 4` reads).</summary>
        public void SetMeter(uint v)
        {
            mem[0x031] = (byte)(v >> 24);
            mem[0x032] = (byte)(v >> 16);
            mem[0x033] = (byte)(v >> 8);
            mem[0x034] = (byte)v;
        }

        private void SinkWrite(int sub, byte[] b, int o, int len)
        {
            uint Be32(int i) =>
                (uint)(b[o + i] << 24 | b[o + i + 1] << 16 | b[o + i + 2] << 8 | b[o + i + 3]);
            if (len == 2 && sub == 0xE080)
            {
                e080 = (ushort)(b[o] << 8 | b[o + 1]);
            }
            else if (len == 2 && sub == 0xE002)
            {
                e002 = (ushort)(b[o] << 8 | b[o + 1]);
            }
            else if (len == 4 && sub == 0x0001)
            {
                slVal = Be32(0);
            }
            else if (len == 4 && sub == 0x0006)
            {
                slAddr = Be32(0);
            }
            else if (len == 4 && sub == 0x0007)
            {
                var p = slAddr + 1;
                if (p < 0x400)
                {
                    param[p] = slVal;
                }
            }
            else if (sub < 0x400 && len >= 4 && len % 4 == 0)
            {
                for (var i = 0; i < len / 4 && sub + i < 0x400; i++)
                {
                    param[sub + i] = Be32(4 * i);
                }
            }
            if (!ModuleRouted)
            {
                SetMeter(0); // the meter follows the selected input; only the module's is modelled (AUX: silent)
            }
        }

        public static uint Crc32(byte[] b, int o, int len)
        {
            var crc = 0xFFFFFFFFu;
            for (var i = 0; i < len; i++)
            {
                crc ^= b[o + i];
                for (var k = 0; k < 8; k++)
                {
                    crc = (crc >> 1) ^ (0xEDB88320u & (uint)-(int)(crc & 1));
                }
            }
            return ~crc;
        }

        private readonly UC3Trace trace;
        private readonly byte[] mem = new byte[0x10000];
        private readonly uint[] param = new uint[0x400];
        private uint slVal,
            slAddr;
        private ushort e080,
            e002;
        private int readSub,
            reads;
        private string audioWav;
        private FileStream wav;
        private ulong frames,
            sfr,
            wframes,
            onz,
            routedFrames;
        private int ipeak,
            opeak,
            meter;
    }

    /// <summary>AD193x codec at 0x04: 32 registers, 1-byte address; reg 1 bit 3 (PLL locked) once the CS2000 makes MCLK.</summary>
    public class ZD1_Codec : IUC3I2CDevice
    {
        public ZD1_Codec(IMachine machine, ZD1Board board)
        {
            trace = UC3Trace.For(machine);
            this.board = board;
        }

        public byte[] Registers => regs;

        public bool DacMuted => (regs[4] & 1) != 0;

        public void Reset() { }

        public void ReadAddress(byte[] a)
        {
            if (a.Length > 0)
            {
                ptr = a[0];
            }
            reads = 0;
        }

        public byte[] Read(int count = 1)
        {
            var r = new byte[count];
            for (var i = 0; i < count; i++)
            {
                var reg = (ptr + reads++) & 31;
                r[i] = reg == 1 ? (byte)(regs[1] | (board.McklPresent ? 0x08 : 0)) : regs[reg];
            }
            return r;
        }

        public void Write(byte[] b)
        {
            if (b.Length < 1)
            {
                return;
            }
            for (var i = 1; i < b.Length; i++)
            {
                regs[(b[0] + i - 1) & 31] = b[i];
            }
            if (b.Length > 1)
            {
                trace.Log(
                    string.Format(
                        "  I2C CODEC W reg {0:x2} <- {1:x2}{2}",
                        b[0],
                        b[1],
                        b.Length > 2 ? " (+more)" : ""
                    )
                );
            }
            ptr = b[0];
            reads = 0;
        }

        public void FinishTransmission() { }

        private readonly UC3Trace trace;
        private readonly ZD1Board board;
        private readonly byte[] regs = new byte[32];
        private int ptr,
            reads;
    }

    /// <summary>24C02 EEPROM at 0x50, 16-byte pages (the address wraps inside the page: the stock firmware writes
    /// 16-byte rows). File: persistent image (created on the first write); blank (0xFF) without one.</summary>
    public class ZD1_Eeprom : IUC3I2CDevice
    {
        public ZD1_Eeprom(IMachine machine)
        {
            trace = UC3Trace.For(machine);
            for (var i = 0; i < data.Length; i++)
            {
                data[i] = 0xFF;
            }
        }

        public string File
        {
            get => file;
            set
            {
                file = value;
                if (!string.IsNullOrEmpty(value) && System.IO.File.Exists(value))
                {
                    var b = System.IO.File.ReadAllBytes(value);
                    Array.Copy(b, data, Math.Min(b.Length, data.Length));
                }
            }
        }

        public byte[] Data => data;

        public void Reset() { }

        public void ReadAddress(byte[] a)
        {
            if (a.Length > 0)
            {
                ptr = a[0];
            }
            reads = 0;
        }

        public byte[] Read(int count = 1)
        {
            var r = new byte[count];
            for (var i = 0; i < count; i++)
            {
                r[i] = data[(byte)(ptr + reads++)];
            }
            return r;
        }

        public void Write(byte[] b)
        {
            if (b.Length < 1)
            {
                return;
            }
            var a = b[0];
            for (var i = 1; i < b.Length; i++)
            {
                data[(a & ~15) | ((a + i - 1) & 15)] = b[i];
            }
            if (b.Length > 1)
            {
                trace.Log(string.Format("  I2C EEPROM W @{0:x2} len {1}", a, b.Length - 1));
                if (!string.IsNullOrEmpty(file))
                {
                    System.IO.File.WriteAllBytes(file, data);
                }
            }
            ptr = a;
            reads = 0;
        }

        public void FinishTransmission() { }

        private readonly UC3Trace trace;
        private readonly byte[] data = new byte[256];
        private string file;
        private int ptr,
            reads;
    }

    /// <summary>Apple auth coprocessor at 0x10: register file only (CP 2.0B-style version regs, device ID 0x200).
    /// The stock selftest pings it ("System error 6" without it).</summary>
    public class ZD1_Auth : IUC3I2CDevice
    {
        public void Reset() { }

        public void ReadAddress(byte[] a)
        {
            if (a.Length > 0)
            {
                ptr = a[0];
            }
            reads = 0;
        }

        public byte[] Read(int count = 1)
        {
            var r = new byte[count];
            for (var i = 0; i < count; i++)
            {
                r[i] = Regs[(ptr + reads++) & 15];
            }
            return r;
        }

        public void Write(byte[] b)
        {
            if (b.Length > 0)
            {
                ptr = b[0];
                reads = 0;
            }
        }

        public void FinishTransmission() { }

        private static readonly byte[] Regs =
        {
            0x05,
            0x01,
            0x02,
            0x00,
            0x00,
            0x00,
            0x02,
            0x00,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        };
        private int ptr,
            reads;
    }

    /// <summary>
    /// Stand-in for the CR870 network module behind SPI0/NPCS2 (docs/protocol): powered when PA04 is on and PA17
    /// released; status 0 while in its bootloader (BootMs / 2), 0x2000 (up), 0x2100 (ready, at BootMs) with three
    /// notifications queued (NetStatus connected, IP, DeviceName); optionally an AirPlay "playing" at PlayMs after
    /// ready. Request line PA20 high = unread status change or queued notifications. Frames: type, reg, [len16le],
    /// payload; 0x27 0x27 = 0x27 and 0x27 0x01 = 0xFF stuffing; the reply goes out on the next chip select.
    /// </summary>
    public class ZD1_ModuleStandIn : ISPIPeripheral
    {
        public ZD1_ModuleStandIn(IMachine machine, AT32UC3_GPIO gpio, AT32UC3_SPI spi)
        {
            this.machine = machine;
            this.gpio = gpio;
            trace = UC3Trace.For(machine);
            gpio.PortChanged += p =>
            {
                if (p == 0)
                {
                    Pins();
                }
            };
            gpio.SetExternalInput(20, Pa20);
            spi.ChipSelect += (cs, on) =>
            {
                if (cs == 2)
                {
                    Cs(on);
                }
            };
        }

        public ulong BootMs { get; set; } = 3000;
        public ulong PlayMs { get; set; }

        public void Reset()
        {
            running = false;
            csActive = false;
        }

        public byte Transmit(byte b)
        {
            if (!running || !csActive)
            {
                return 0xFF;
            }
            var o = txPos < txLen ? tx[txPos++] : (byte)0xFF;
            if (rxN == 0 && b == 0xFF)
            {
                return o; // idle clocking
            }
            if (rxEsc)
            {
                b = b == 0x01 ? (byte)0xFF : b;
                rxEsc = false;
            }
            else if (b == 0x27 && rxN >= 1)
            {
                rxEsc = true;
                return o;
            }
            if (rxN < rx.Length)
            {
                rx[rxN++] = b;
            }
            return o;
        }

        public void FinishTransmission() { }

        private ulong NowNs => trace.NowNs;

        private void Enqueue(byte reg, byte[] data)
        {
            if (q.Count >= 8 || data.Length > 160)
            {
                return;
            }
            q.Add((reg, data));
        }

        private ushort Status()
        {
            if (!running)
            {
                return 0;
            }
            var el = NowNs - tRelease;
            var boot = BootMs * 1000000;
            if (el < boot / 2)
            {
                return 0; // still in its bootloader
            }
            if (el < boot)
            {
                return 0x2000;
            }
            if (!announced)
            {
                announced = true;
                Enqueue(0x32, Encoding.ASCII.GetBytes("21"));
                Enqueue(0x38, Encoding.ASCII.GetBytes("192.168.50.101\0\0"));
                Enqueue(0x58, Encoding.ASCII.GetBytes("Zeppelin Air"));
                trace.Log("*** DM870 ready: queued 3 notifications (0x32, 0x38, 0x58)");
            }
            if (PlayMs != 0 && !playSent && el >= boot + PlayMs * 1000000)
            {
                playSent = true;
                Enqueue(
                    0x47,
                    Encoding.ASCII.GetBytes(
                        "<update id=\"status\"><icon id=\"play\">pl</icon></update>"
                    )
                );
                trace.Log("*** DM870: queued reg 0x47 play=pl (AirPlay playing)");
            }
            return 0x2100;
        }

        private bool StatusUnread()
        {
            var st = Status();
            return st != 0 && st != statusSeen;
        }

        private bool Pa20()
        {
            if (!running)
            {
                return false;
            }
            return StatusUnread() || q.Count > 0;
        }

        private void Pins()
        {
            var pwr = gpio.OutHigh(0, 4);
            var rst = gpio.OutHigh(0, 17);
            var run = pwr && rst;
            if (run && !running)
            {
                running = true;
                tRelease = NowNs;
                statusSeen = 0;
                txLen = txPos = rxN = 0;
                announced = false;
                q.Clear();
                playSent = false;
                trace.Log(
                    string.Format(
                        "*** DM870 running (PA04 on, PA17 released); ready in {0} ms",
                        BootMs
                    )
                );
            }
            else if (!run && running)
            {
                running = false;
                trace.Log(
                    string.Format(
                        "*** DM870 stopped ({0})",
                        pwr ? "PA17 reset asserted" : "PA04 power off"
                    )
                );
            }
        }

        private void TxByte(byte b)
        {
            if (txLen > tx.Length - 2)
            {
                return;
            }
            if (b == 0x27)
            {
                tx[txLen++] = 0x27;
                tx[txLen++] = 0x27;
            }
            else if (b == 0xFF)
            {
                tx[txLen++] = 0x27;
                tx[txLen++] = 0x01;
            }
            else
            {
                tx[txLen++] = b;
            }
        }

        private static readonly string[] Versions =
        {
            "I2014092908219112",
            "B201009271404 081008",
            "C 2014092908219112",
        };

        private void Cs(bool active)
        {
            if (active == csActive)
            {
                return;
            }
            csActive = active;
            if (active)
            {
                rxN = 0;
                rxEsc = false;
                txPos = 0;
                return;
            }
            txLen = txPos = 0;
            if (!running || rxN < 2)
            {
                return;
            }
            var type = rx[0];
            var reg = rx[1];
            if ((type == 0x01 || type == 0x03) && rxN >= 4)
            {
                byte[] pl;
                int v;
                if (reg == 0x11)
                {
                    v = Status();
                    if (v == 0)
                    {
                        trace.Log("  DM870 read 0x11 while still booting: no answer");
                        return;
                    }
                    statusSeen = (ushort)v;
                    pl = new[] { (byte)(v >> 8), (byte)v };
                }
                else if (reg == 0x0C)
                {
                    v =
                        StatusUnread() ? 0x1100
                        : q.Count > 0 ? q[0].reg << 8
                        : 0;
                    pl = new[] { (byte)(v >> 8), (byte)0 };
                }
                else if (q.Count > 0 && q[0].reg == reg)
                {
                    pl = q[0].data;
                    if (type == 0x01)
                    {
                        pl = new[]
                        {
                            pl.Length > 0 ? pl[0] : (byte)0,
                            pl.Length > 1 ? pl[1] : (byte)0,
                        };
                    }
                    q.RemoveAt(0);
                }
                else if (type == 0x03 && reg >= 0x25 && reg <= 0x27)
                {
                    var t = Versions[reg - 0x25];
                    var bytes = Encoding.ASCII.GetBytes(t);
                    pl = reg == 0x26 ? bytes : bytes.Concat(new byte[] { 0 }).ToArray();
                }
                else
                {
                    v = regs[reg];
                    pl = new[] { (byte)(v >> 8), (byte)v };
                }
                tx[txLen++] = type; // header bytes are never stuffed
                tx[txLen++] = reg;
                if (type == 0x03)
                {
                    TxByte((byte)pl.Length);
                    TxByte((byte)(pl.Length >> 8));
                }
                foreach (var x in pl)
                {
                    TxByte(x);
                }
                trace.Log(
                    string.Format(
                        "  DM870 read{0} reg {1:x2} -> {2} byte(s) {3:x2} {4:x2}.. (reply on next CS)",
                        type == 0x03 ? "V" : "F",
                        reg,
                        pl.Length,
                        pl.Length > 0 ? pl[0] : 0,
                        pl.Length > 1 ? pl[1] : 0
                    )
                );
            }
            else if (type == 0x00 && rxN >= 4)
            {
                regs[reg] = (ushort)(rx[2] << 8 | rx[3]);
                trace.Log(
                    string.Format("  DM870 write reg {0:x2} <- {1:x2} {2:x2}", reg, rx[2], rx[3])
                );
                if (reg == 0x48) // DRC command: ack on 0xE3 like the stock module
                {
                    var c = rx[2];
                    var ack =
                        c == 0xDF ? 0
                        : c == 0xDE ? 1
                        : c == 0xE1 ? 4
                        : c == 0xE2 ? 5
                        : -1;
                    if (ack >= 0)
                    {
                        Enqueue(0xE3, new[] { (byte)ack, (byte)0 });
                        trace.Log(string.Format("  DM870 DRC {0:x2} -> ack 0xE3 = {1}", c, ack));
                    }
                }
            }
            else
            {
                trace.Log(
                    string.Format(
                        "  DM870 ignored request type {0:x2} reg {1:x2} ({2} bytes)",
                        type,
                        reg,
                        rxN
                    )
                );
            }
        }

        private readonly IMachine machine;
        private readonly AT32UC3_GPIO gpio;
        private readonly UC3Trace trace;
        private bool running,
            csActive,
            rxEsc,
            announced,
            playSent;
        private ulong tRelease;
        private ushort statusSeen;
        private int rxN,
            txLen,
            txPos;
        private readonly byte[] rx = new byte[16];
        private readonly byte[] tx = new byte[400];
        private readonly ushort[] regs = new ushort[256];
        private readonly List<(byte reg, byte[] data)> q = new List<(byte, byte[])>();
    }
}
