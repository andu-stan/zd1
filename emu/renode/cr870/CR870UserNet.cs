// A user-mode network for the DM870's MACB, like QEMU's `-netdev user,net=192.168.50.0/24,host=192.168.50.1,
// dhcpstart=192.168.50.101`: the gateway 192.168.50.1 answers ARP, ping and DHCP (always .101), refuses DNS, and
// NATs TCP to its own address onto the host's loopback (`connect 192.168.50.1:5000` = host 127.0.0.1:5000).
// Forward "8080:80,2323:23" listens on Bind:8080 and opens guest port 80 per connection (QEMU hostfwd). Mdns = port
// sends the guest's mDNS queries to a responder on the host.
//
// Everything runs in the module machine's virtual time: guest frames arrive on the CPU thread (MACB TX), replies go
// out after LatencyUs at 100 Mbit/s wire rate, and the host sockets are polled every PollUs (non-blocking) from a
// clock entry. The TCP side is minimal but correct on a lossless link with a finite RX ring: in-order receive,
// go-back-N retransmit after RtoMs, no window scaling or SACK offered, MSS 1460.
using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Net;
using System.Net.Sockets;
using System.Text;
using Antmicro.Renode.Core;
using Antmicro.Renode.Network;
using Antmicro.Renode.Peripherals.Bus;
using Antmicro.Renode.Time;
using Antmicro.Renode.Utilities;

namespace Antmicro.Renode.Peripherals.CR870
{
    public class CR870_UserNet : IDoubleWordPeripheral, IKnownSize
    {
        public CR870_UserNet(IMachine machine, CR870_MACB macb)
        {
            this.machine = machine;
            this.macb = macb;
            macb.FrameReady += f => FromGuest(f.Bytes);
        }

        public long Size => 0x10;

        public bool Enabled { get; set; } = true;

        public string GuestIp
        {
            get => Ip(guestIp);
            set => guestIp = ParseIp(value);
        }

        /// <summary>Host UDP port of an mDNS responder: the guest's queries to 224.0.0.251:5353 go there, its replies
        /// come back as unicast from the gateway (what one-shot queries from an ephemeral port get on a real net).
        /// 0 = no mDNS.</summary>
        public int Mdns { get; set; }

        public string Bind { get; set; } = "127.0.0.1";

        public ulong LatencyUs { get; set; } = 20;

        public ulong PollUs { get; set; } = 500;

        public ulong RtoMs { get; set; } = 200;

        /// <summary>"hostport:guestport,..." (TCP), "udp:hostport:guestport" (UDP), like QEMU hostfwd.</summary>
        public string Forward
        {
            get => forward;
            set
            {
                forward = value;
                foreach (var l in listeners)
                {
                    l.Item1.Stop();
                }
                listeners.Clear();
                foreach (var u in udpListeners)
                {
                    u.Item1.Dispose();
                }
                udpListeners.Clear();
                foreach (
                    var spec in (value ?? "").Split(
                        new[] { ',' },
                        StringSplitOptions.RemoveEmptyEntries
                    )
                )
                {
                    var p = spec.Split(':');
                    if (p[0] == "udp")
                    {
                        var u = new Socket(
                            AddressFamily.InterNetwork,
                            SocketType.Dgram,
                            ProtocolType.Udp
                        )
                        {
                            Blocking = false,
                        };
                        u.Bind(new IPEndPoint(IPAddress.Parse(Bind), int.Parse(p[1])));
                        udpListeners.Add(Tuple.Create(u, int.Parse(p[2])));
                        Log("forward udp {0}:{1} -> guest :{2}", Bind, p[1], p[2]);
                        continue;
                    }
                    var l = new TcpListener(IPAddress.Parse(Bind), int.Parse(p[0]));
                    l.Server.SetSocketOption(
                        SocketOptionLevel.Socket,
                        SocketOptionName.ReuseAddress,
                        true
                    );
                    l.Start();
                    listeners.Add(Tuple.Create(l, int.Parse(p[1])));
                    Log("forward {0}:{1} -> guest :{2}", Bind, p[0], p[1]);
                }
            }
        }

        public string LogFile
        {
            get => logFile;
            set
            {
                logFile = value;
                log?.Dispose();
                log = string.IsNullOrEmpty(value)
                    ? null
                    : new StreamWriter(value, true, Encoding.ASCII) { AutoFlush = true };
            }
        }

        public ulong FramesIn { get; private set; }

        public ulong FramesOut { get; private set; }

        public uint ReadDoubleWord(long offset) => 0;

        public void WriteDoubleWord(long offset, uint value) { }

        public void Reset()
        {
            // a chip reset loses the guest's TCP state: drop ours too (the host peers see a reset)
            foreach (var c in conns.Values.ToList())
            {
                Close(c);
            }
        }

        // ---- from the guest (CPU thread) ----
        private void FromGuest(byte[] f)
        {
            if (!Enabled || f.Length < 14)
            {
                return;
            }
            FramesIn++;
            StartPoll();
            Array.Copy(f, 6, guestMac, 0, 6);
            var type = f[12] << 8 | f[13];
            if (type == 0x0806 && f.Length >= 42)
            {
                Arp(f);
            }
            else if (type == 0x0800 && f.Length >= 34 && (f[14] >> 4) == 4)
            {
                var ihl = (f[14] & 15) * 4;
                var total = Math.Min(f[16] << 8 | f[17], f.Length - 14);
                if (total < ihl)
                {
                    return;
                }
                var proto = f[23];
                var src = Be32(f, 26);
                var dst = Be32(f, 30);
                var p = new ArraySegment<byte>(f, 14 + ihl, total - ihl);
                if (proto == 17)
                {
                    Udp(src, dst, p);
                }
                else if (proto == 6)
                {
                    Tcp(src, dst, p);
                }
                else if (proto == 1 && dst == gateway)
                {
                    Icmp(src, p);
                }
            }
        }

        private void Arp(byte[] f)
        {
            var op = f[20] << 8 | f[21];
            var spa = Be32(f, 28);
            var tpa = Be32(f, 38);
            if (op != 1 || tpa == spa || (tpa & mask) != (gateway & mask) || tpa == guestIp)
            {
                return; // only requests for "other" hosts on the net: all of them are us
            }
            var r = new byte[28];
            r[1] = 1;
            r[2] = 8;
            r[4] = 6;
            r[5] = 4;
            r[7] = 2;
            Array.Copy(gwMac, 0, r, 8, 6);
            Put32(r, 14, tpa);
            Array.Copy(f, 22, r, 18, 6);
            Put32(r, 24, spa);
            Send(Eth(guestMac, 0x0806, r));
        }

        private void Icmp(uint src, ArraySegment<byte> p)
        {
            if (p.Count < 8 || p.Array[p.Offset] != 8)
            {
                return;
            }
            var r = p.ToArray();
            r[0] = 0;
            r[2] = r[3] = 0;
            var c = Checksum(r, 0, r.Length, 0);
            r[2] = (byte)(c >> 8);
            r[3] = (byte)c;
            Send(Eth(guestMac, 0x0800, Ip4(gateway, src, 1, r)));
        }

        // ---- UDP: DHCP and DNS ----
        private void Udp(uint src, uint dst, ArraySegment<byte> p)
        {
            if (p.Count < 8)
            {
                return;
            }
            var a = p.Array;
            var o = p.Offset;
            var sport = a[o] << 8 | a[o + 1];
            var dport = a[o + 2] << 8 | a[o + 3];
            var data = new ArraySegment<byte>(
                a,
                o + 8,
                Math.Min(p.Count, a[o + 4] << 8 | a[o + 5]) - 8
            );
            if (dport == 67)
            {
                Dhcp(data);
            }
            else if (dport == 53 && dst == gateway && data.Count >= 12)
            {
                var r = data.ToArray(); // REFUSED: the guest gives up at once
                r[2] = (byte)(0x80 | (r[2] & 0x79));
                r[3] = 0x05;
                Send(Eth(guestMac, 0x0800, Ip4(gateway, src, 17, UdpSeg(53, sport, r))));
            }
            else if (dst == gateway)
            {
                UdpOut(src, sport, dport, data);
            }
            else if (dst == 0xE00000FB && dport == 5353 && Mdns > 0)
            {
                UdpOut(src, sport, Mdns, data); // mDNS query -> a responder on host Bind:Mdns; it answers unicast
            }
        }

        private void Dhcp(ArraySegment<byte> d)
        {
            var b = d.ToArray();
            if (b.Length < 240 || b[0] != 1 || Be32(b, 236) != 0x63825363)
            {
                return;
            }
            var type = 0;
            for (var i = 240; i + 1 < b.Length && b[i] != 255; )
            {
                if (b[i] == 0)
                {
                    i++;
                    continue;
                }
                if (b[i] == 53 && i + 2 < b.Length)
                {
                    type = b[i + 2];
                }
                i += 2 + b[i + 1];
            }
            if (type != 1 && type != 3)
            {
                return;
            }
            var r = new byte[300];
            r[0] = 2;
            r[1] = 1;
            r[2] = 6;
            Array.Copy(b, 4, r, 4, 4); // xid
            Array.Copy(b, 10, r, 10, 2); // flags
            Put32(r, 16, guestIp); // yiaddr
            Put32(r, 20, gateway); // siaddr
            Array.Copy(b, 28, r, 28, 16); // chaddr
            Put32(r, 236, 0x63825363);
            var opts = new List<byte> { 53, 1, (byte)(type == 1 ? 2 : 5), 54, 4 };
            opts.AddRange(Bytes(gateway));
            opts.AddRange(new byte[] { 51, 4, 0, 1, 0x51, 0x80, 1, 4 });
            opts.AddRange(Bytes(mask));
            opts.AddRange(new byte[] { 3, 4 });
            opts.AddRange(Bytes(gateway));
            opts.AddRange(new byte[] { 6, 4 });
            opts.AddRange(Bytes(gateway));
            opts.Add(255);
            opts.CopyTo(r, 240);
            Log(
                "dhcp {0} -> {1} {2}",
                type == 1 ? "DISCOVER" : "REQUEST",
                type == 1 ? "OFFER" : "ACK",
                Ip(guestIp)
            );
            Send(Eth(Broadcast, 0x0800, Ip4(gateway, 0xFFFFFFFF, 17, UdpSeg(67, 68, r))));
        }

        // an RTSP SETUP reply from the guest names its RTP ports: forward them (UDP) from the host, like a NAT's ALG
        private void RtspAlg(ArraySegment<byte> data)
        {
            var t = Encoding.ASCII.GetString(data.Array, data.Offset, data.Count);
            if (t.IndexOf("Transport:", StringComparison.OrdinalIgnoreCase) < 0)
            {
                return;
            }
            foreach (
                System.Text.RegularExpressions.Match m in System.Text.RegularExpressions.Regex.Matches(
                    t,
                    @"(server_port|control_port|timing_port)=(\d+)"
                )
            )
            {
                var port = int.Parse(m.Groups[2].Value);
                if (udpListeners.Any(u => u.Item2 == port))
                {
                    continue;
                }
                try
                {
                    var u = new Socket(
                        AddressFamily.InterNetwork,
                        SocketType.Dgram,
                        ProtocolType.Udp
                    )
                    {
                        Blocking = false,
                    };
                    u.Bind(new IPEndPoint(IPAddress.Parse(Bind), port));
                    udpListeners.Add(Tuple.Create(u, port));
                    Log("rtsp: {0} -> forward udp {1}:{2}", m.Groups[1].Value, Bind, port);
                }
                catch (SocketException e)
                {
                    Log("rtsp: udp {0}: {1}", port, e.SocketErrorCode);
                }
            }
        }

        // guest -> gateway:port = host Bind:port, from one host socket per (guest port, port): replies come back
        private void UdpOut(uint src, int sport, int dport, ArraySegment<byte> data)
        {
            var key = Key(src, sport, dport);
            if (!udpFlows.TryGetValue(key, out var f))
            {
                var sock = new Socket(
                    AddressFamily.InterNetwork,
                    SocketType.Dgram,
                    ProtocolType.Udp
                )
                {
                    Blocking = false,
                };
                sock.Bind(new IPEndPoint(IPAddress.Parse(Bind), 0));
                f = Tuple.Create(sock, src, sport, dport);
                udpFlows[key] = f;
                Log("udp {0}:{1} -> host :{2}", Ip(src), sport, dport);
            }
            try
            {
                f.Item1.SendTo(
                    data.Array,
                    data.Offset,
                    data.Count,
                    SocketFlags.None,
                    new IPEndPoint(IPAddress.Parse(Bind), dport)
                );
            }
            catch (SocketException) { }
        }

        private void UdpPoll()
        {
            var buf = new byte[2048];
            EndPoint from = new IPEndPoint(IPAddress.Any, 0);
            foreach (var f in udpFlows.Values)
            {
                while (f.Item1.Available > 0)
                {
                    int n;
                    try
                    {
                        n = f.Item1.ReceiveFrom(buf, ref from);
                    }
                    catch (SocketException)
                    {
                        break;
                    }
                    Send(
                        Eth(
                            guestMac,
                            0x0800,
                            Ip4(
                                gateway,
                                f.Item2,
                                17,
                                UdpSeg(f.Item4, f.Item3, buf.Take(n).ToArray())
                            )
                        )
                    );
                }
            }
            foreach (var u in udpListeners)
            {
                while (u.Item1.Available > 0)
                {
                    int n;
                    try
                    {
                        n = u.Item1.ReceiveFrom(buf, ref from);
                    }
                    catch (SocketException)
                    {
                        break;
                    }
                    var sport = ((IPEndPoint)from).Port; // the guest sees gateway:<sender's port>: replies NAT back
                    Send(
                        Eth(
                            guestMac,
                            0x0800,
                            Ip4(gateway, guestIp, 17, UdpSeg(sport, u.Item2, buf.Take(n).ToArray()))
                        )
                    );
                }
            }
        }

        // ---- TCP ----
        private class Conn
        {
            public uint GuestIp;
            public int GuestPort,
                Port; // Port: ours (the gateway's)
            public Socket Sock;
            public bool Inbound,
                Established,
                HostEof,
                FinSent,
                GuestFin,
                Closed;
            public uint Iss,
                SndUna,
                SndNxt,
                RcvNxt,
                FinSeq;
            public int Wnd = 1460;
            public readonly List<byte> Buf = new List<byte>(); // host bytes from SndUna on
            public ulong LastSend;
            public string Name;
        }

        private void Tcp(uint src, uint dst, ArraySegment<byte> p)
        {
            if (p.Count < 20)
            {
                return;
            }
            var a = p.Array;
            var o = p.Offset;
            var sport = a[o] << 8 | a[o + 1];
            var dport = a[o + 2] << 8 | a[o + 3];
            var seq = Be32(a, o + 4);
            var ack = Be32(a, o + 8);
            var off = (a[o + 12] >> 4) * 4;
            var fl = a[o + 13];
            var wnd = a[o + 14] << 8 | a[o + 15];
            var data = new ArraySegment<byte>(a, o + off, Math.Max(0, p.Count - off));
            var key = Key(src, sport, dport);
            conns.TryGetValue(key, out var c);
            if (c == null)
            {
                if ((fl & Rst) != 0)
                {
                    return;
                }
                if ((fl & (Syn | Ack)) != Syn || dst != gateway)
                {
                    SendRst(
                        src,
                        sport,
                        dst,
                        dport,
                        (fl & Ack) != 0 ? ack : 0,
                        seq + (uint)data.Count + ((fl & Syn) != 0 ? 1u : 0),
                        (fl & Ack) == 0
                    );
                    return;
                }
                var s = new Socket(AddressFamily.InterNetwork, SocketType.Stream, ProtocolType.Tcp)
                {
                    NoDelay = true,
                };
                try
                {
                    s.Connect(IPAddress.Parse(Bind), dport);
                }
                catch (SocketException e)
                {
                    Log("tcp {0}:{1} -> host {2}: {3}", Ip(src), sport, dport, e.SocketErrorCode);
                    s.Dispose();
                    SendRst(src, sport, dst, dport, 0, seq + 1, true);
                    return;
                }
                s.Blocking = false;
                c = new Conn
                {
                    GuestIp = src,
                    GuestPort = sport,
                    Port = dport,
                    Sock = s,
                    Iss = NextIss(),
                    Name = string.Format(":{0} -> host :{1}", sport, dport),
                };
                c.SndUna = c.Iss;
                c.SndNxt = c.Iss + 1;
                c.RcvNxt = seq + 1;
                c.Wnd = wnd;
                conns[key] = c;
                Log("tcp open {0}", c.Name);
                Seg(c, c.Iss, Syn | Ack, null, 0, 0, true);
                c.LastSend = NowUs;
                return;
            }
            if ((fl & Rst) != 0)
            {
                Log("tcp {0}: reset by guest", c.Name);
                Close(c);
                return;
            }
            if (c.Inbound && !c.Established)
            {
                if ((fl & (Syn | Ack)) == (Syn | Ack) && ack == c.Iss + 1)
                {
                    c.RcvNxt = seq + 1;
                    c.SndUna = c.SndNxt = c.Iss + 1;
                    c.Established = true;
                    c.Wnd = wnd;
                    SendAck(c);
                }
                return;
            }
            if ((fl & Syn) != 0)
            {
                SendAck(c); // a retransmitted SYN: our SYN-ACK got lost
                if (!c.Established)
                {
                    Seg(c, c.Iss, Syn | Ack, null, 0, 0, true);
                }
                return;
            }
            if ((fl & Ack) != 0)
            {
                if (!c.Established && ack == c.Iss + 1)
                {
                    c.Established = true;
                    c.SndUna = c.Iss + 1;
                }
                var acked = (int)(ack - c.SndUna);
                var inflight = (int)(c.SndNxt - c.SndUna);
                if (acked > 0 && acked <= inflight)
                {
                    var bytes = Math.Min(acked, c.Buf.Count);
                    c.Buf.RemoveRange(0, bytes);
                    c.SndUna = ack;
                    c.LastSend = NowUs;
                }
                c.Wnd = wnd;
            }
            var need = false;
            if (data.Count > 0)
            {
                if (seq == c.RcvNxt && !c.GuestFin)
                {
                    if (c.Inbound)
                    {
                        RtspAlg(data);
                    }
                    try
                    {
                        c.Sock.Blocking = true;
                        c.Sock.Send(data.Array, data.Offset, data.Count, SocketFlags.None);
                        c.Sock.Blocking = false;
                        c.RcvNxt += (uint)data.Count;
                    }
                    catch (SocketException e)
                    {
                        Log("tcp {0}: host send {1}", c.Name, e.SocketErrorCode);
                        SendRst(c.GuestIp, c.GuestPort, gateway, c.Port, c.SndNxt, 0, false);
                        Close(c);
                        return;
                    }
                }
                need = true;
            }
            if ((fl & Fin) != 0 && seq + (uint)data.Count == c.RcvNxt && !c.GuestFin)
            {
                c.GuestFin = true;
                c.RcvNxt++;
                try
                {
                    c.Sock.Shutdown(SocketShutdown.Send);
                }
                catch (SocketException) { }
                need = true;
            }
            if (need)
            {
                SendAck(c);
            }
            Pump(c);
            Done(c);
        }

        private void Pump(Conn c)
        {
            if (!c.Established || c.Closed)
            {
                return;
            }
            var now = NowUs;
            if (c.SndNxt != c.SndUna && now - c.LastSend > RtoMs * 1000)
            {
                c.SndNxt = c.SndUna; // go back N
                if (c.FinSent && c.SndUna <= c.FinSeq)
                {
                    c.FinSent = false;
                }
                c.LastSend = now;
            }
            var segs = 0;
            while (segs < 24)
            {
                var sent = (int)(c.SndNxt - c.SndUna);
                var room = Math.Min(c.Buf.Count - sent, c.Wnd - sent);
                if (room <= 0)
                {
                    break;
                }
                var n = Math.Min(room, Mss);
                Seg(c, c.SndNxt, Ack | Psh, c.Buf, sent, n, false);
                c.SndNxt += (uint)n;
                c.LastSend = now;
                segs++;
            }
            if (c.HostEof && !c.FinSent && (int)(c.SndNxt - c.SndUna) == c.Buf.Count)
            {
                c.FinSeq = c.SndNxt;
                Seg(c, c.SndNxt, Fin | Ack, null, 0, 0, false);
                c.SndNxt++;
                c.FinSent = true;
                c.LastSend = now;
            }
        }

        private void Done(Conn c)
        {
            if (!c.Closed && c.GuestFin && c.FinSent && c.SndUna == c.SndNxt)
            {
                Log("tcp closed {0}", c.Name);
                Close(c);
            }
        }

        private void Close(Conn c)
        {
            c.Closed = true;
            try
            {
                c.Sock.Dispose();
            }
            catch (Exception) { }
            conns.Remove(Key(c.GuestIp, c.GuestPort, c.Port));
        }

        // ---- host side, polled in virtual time ----
        private void StartPoll()
        {
            if (polling)
            {
                return;
            }
            polling = true;
            machine.ScheduleAction(
                TimeInterval.FromMicroseconds(PollUs),
                _ => Poll(),
                "usernet-poll"
            );
        }

        private void Poll()
        {
            foreach (var l in listeners)
            {
                while (l.Item1.Pending())
                {
                    var s = l.Item1.AcceptSocket();
                    s.NoDelay = true;
                    s.Blocking = false;
                    var port = nextPort++;
                    var c = new Conn
                    {
                        GuestIp = guestIp,
                        GuestPort = l.Item2,
                        Port = port,
                        Sock = s,
                        Inbound = true,
                        Iss = NextIss(),
                        Name = string.Format(
                            "host :{0} -> :{1}",
                            ((IPEndPoint)s.LocalEndPoint).Port,
                            l.Item2
                        ),
                    };
                    c.SndUna = c.Iss;
                    c.SndNxt = c.Iss + 1;
                    conns[Key(guestIp, l.Item2, port)] = c;
                    Log("tcp open {0}", c.Name);
                    Seg(c, c.Iss, Syn, null, 0, 0, true);
                    c.LastSend = NowUs;
                }
            }
            UdpPoll();
            var buf = new byte[16384];
            foreach (var c in conns.Values.ToList())
            {
                if (!c.Established)
                {
                    if (NowUs - c.LastSend > RtoMs * 1000)
                    {
                        Seg(c, c.Iss, c.Inbound ? Syn : (byte)(Syn | Ack), null, 0, 0, true);
                        c.LastSend = NowUs;
                    }
                    continue;
                }
                while (!c.HostEof && c.Buf.Count < 262144)
                {
                    int n;
                    try
                    {
                        if (c.Sock.Available == 0 && !c.Sock.Poll(0, SelectMode.SelectRead))
                        {
                            break;
                        }
                        n = c.Sock.Receive(buf);
                    }
                    catch (SocketException e) when (e.SocketErrorCode == SocketError.WouldBlock)
                    {
                        break;
                    }
                    catch (SocketException)
                    {
                        n = 0;
                    }
                    if (n == 0)
                    {
                        c.HostEof = true;
                        break;
                    }
                    c.Buf.AddRange(buf.Take(n));
                }
                Pump(c);
                Done(c);
            }
            machine.ScheduleAction(
                TimeInterval.FromMicroseconds(PollUs),
                _ => Poll(),
                "usernet-poll"
            );
        }

        // ---- frame building ----
        private void SendAck(Conn c) => Seg(c, c.SndNxt, Ack, null, 0, 0, false);

        private void Seg(Conn c, uint seq, byte flags, List<byte> src, int at, int n, bool mss)
        {
            var hl = mss ? 24 : 20;
            var t = new byte[hl + n];
            t[0] = (byte)(c.Port >> 8);
            t[1] = (byte)c.Port;
            t[2] = (byte)(c.GuestPort >> 8);
            t[3] = (byte)c.GuestPort;
            Put32(t, 4, seq);
            Put32(t, 8, (flags & Ack) != 0 ? c.RcvNxt : 0);
            t[12] = (byte)(hl / 4 << 4);
            t[13] = flags;
            t[14] = 0xFF;
            t[15] = 0xFF; // 64 KiB window (the host side buffers)
            if (mss)
            {
                t[20] = 2;
                t[21] = 4;
                t[22] = Mss >> 8;
                t[23] = Mss & 0xFF;
            }
            if (n > 0)
            {
                src.CopyTo(at, t, hl, n);
            }
            TcpChecksum(t, gateway, c.GuestIp);
            Send(Eth(guestMac, 0x0800, Ip4(gateway, c.GuestIp, 6, t)));
        }

        private void SendRst(
            uint guest,
            int gport,
            uint ours,
            int port,
            uint seq,
            uint ack,
            bool withAck
        )
        {
            var t = new byte[20];
            t[0] = (byte)(port >> 8);
            t[1] = (byte)port;
            t[2] = (byte)(gport >> 8);
            t[3] = (byte)gport;
            Put32(t, 4, seq);
            Put32(t, 8, ack);
            t[12] = 5 << 4;
            t[13] = (byte)(Rst | (withAck ? Ack : 0));
            TcpChecksum(t, ours, guest);
            Send(Eth(guestMac, 0x0800, Ip4(ours, guest, 6, t)));
        }

        private static void TcpChecksum(byte[] t, uint src, uint dst)
        {
            t[16] = t[17] = 0;
            var c = Checksum(t, 0, t.Length, Pseudo(src, dst, 6, t.Length));
            t[16] = (byte)(c >> 8);
            t[17] = (byte)c;
        }

        private static byte[] UdpSeg(int sport, int dport, byte[] data)
        {
            var u = new byte[8 + data.Length];
            u[0] = (byte)(sport >> 8);
            u[1] = (byte)sport;
            u[2] = (byte)(dport >> 8);
            u[3] = (byte)dport;
            u[4] = (byte)(u.Length >> 8);
            u[5] = (byte)u.Length;
            data.CopyTo(u, 8);
            return u; // checksum 0 = none
        }

        private byte[] Ip4(uint src, uint dst, byte proto, byte[] payload)
        {
            var p = new byte[20 + payload.Length];
            p[0] = 0x45;
            p[2] = (byte)(p.Length >> 8);
            p[3] = (byte)p.Length;
            p[4] = (byte)(ipId >> 8);
            p[5] = (byte)ipId;
            ipId++;
            p[6] = 0x40; // DF
            p[8] = 64;
            p[9] = proto;
            Put32(p, 12, src);
            Put32(p, 16, dst);
            var c = Checksum(p, 0, 20, 0);
            p[10] = (byte)(c >> 8);
            p[11] = (byte)c;
            payload.CopyTo(p, 20);
            return p;
        }

        private byte[] Eth(byte[] dst, int type, byte[] payload)
        {
            var e = new byte[14 + payload.Length];
            Array.Copy(dst, 0, e, 0, 6);
            Array.Copy(gwMac, 0, e, 6, 6);
            e[12] = (byte)(type >> 8);
            e[13] = (byte)type;
            payload.CopyTo(e, 14);
            return e;
        }

        private void Send(byte[] e)
        {
            if (!Misc.TryCreateFrameOrLogWarning(macb, e, out var f, addCrc: false))
            {
                return;
            }
            FramesOut++;
            var now = NowNs;
            var at =
                Math.Max(now + LatencyUs * 1000, lastWire)
                + (ulong)(Math.Max(e.Length, 60) + 24) * 80;
            lastWire = at;
            machine.ScheduleAction(
                TimeInterval.FromNanoseconds(at - now),
                _ => macb.ReceiveFrame(f),
                "usernet-rx"
            );
        }

        // ---- helpers ----
        private static uint Pseudo(uint src, uint dst, int proto, int len) =>
            (src >> 16) + (src & 0xFFFF) + (dst >> 16) + (dst & 0xFFFF) + (uint)proto + (uint)len;

        private static ushort Checksum(byte[] b, int o, int n, uint sum)
        {
            for (var i = 0; i + 1 < n; i += 2)
            {
                sum += (uint)(b[o + i] << 8 | b[o + i + 1]);
            }
            if ((n & 1) != 0)
            {
                sum += (uint)b[o + n - 1] << 8;
            }
            while ((sum >> 16) != 0)
            {
                sum = (sum & 0xFFFF) + (sum >> 16);
            }
            return (ushort)~sum;
        }

        private static uint Be32(byte[] b, int o) =>
            (uint)(b[o] << 24 | b[o + 1] << 16 | b[o + 2] << 8 | b[o + 3]);

        private static void Put32(byte[] b, int o, uint v)
        {
            b[o] = (byte)(v >> 24);
            b[o + 1] = (byte)(v >> 16);
            b[o + 2] = (byte)(v >> 8);
            b[o + 3] = (byte)v;
        }

        private static byte[] Bytes(uint v) =>
            new[] { (byte)(v >> 24), (byte)(v >> 16), (byte)(v >> 8), (byte)v };

        private static uint ParseIp(string s) => Be32(IPAddress.Parse(s).GetAddressBytes(), 0);

        private static string Ip(uint v) => string.Join(".", Bytes(v));

        private static ulong Key(uint ip, int gport, int port) =>
            (ulong)ip << 32 | (ulong)gport << 16 | (uint)port;

        private uint NextIss() => issBase += 0x01000000;

        private ulong NowNs => machine.ElapsedVirtualTime.TimeElapsed.Ticks;

        private ulong NowUs => NowNs / 1000;

        private void Log(string format, params object[] args)
        {
            log?.Write(
                string.Format("{0,10:F3} net: {1}\n", NowNs / 1e6, string.Format(format, args))
            );
        }

        private const byte Fin = 1,
            Syn = 2,
            Rst = 4,
            Psh = 8,
            Ack = 16;
        private const int Mss = 1460;
        private static readonly byte[] Broadcast = { 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF };

        private readonly IMachine machine;
        private readonly CR870_MACB macb;
        private readonly byte[] gwMac = { 0x52, 0x55, 0xC0, 0xA8, 0x32, 0x01 };
        private readonly byte[] guestMac = { 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF };
        private readonly uint gateway = 0xC0A83201,
            mask = 0xFFFFFF00;
        private uint guestIp = 0xC0A83265;
        private readonly Dictionary<ulong, Conn> conns = new Dictionary<ulong, Conn>();
        private readonly List<Tuple<TcpListener, int>> listeners =
            new List<Tuple<TcpListener, int>>();
        private readonly List<Tuple<Socket, int>> udpListeners = new List<Tuple<Socket, int>>();
        private readonly Dictionary<ulong, Tuple<Socket, uint, int, int>> udpFlows =
            new Dictionary<ulong, Tuple<Socket, uint, int, int>>();
        private string forward,
            logFile;
        private StreamWriter log;
        private bool polling;
        private ushort ipId;
        private int nextPort = 40000;
        private uint issBase = 0x10000000;
        private ulong lastWire;
    }
}
