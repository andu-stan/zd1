//! One-shot mDNS lookups for the module OS apps: [`browse_all`] finds the instances of a service type (zd1-snapcast
//! and zd1-linkd: `_snapcast._tcp.local`, what snapserver announces through Avahi / Bonjour). It asks once with a
//! "legacy unicast" query (RFC 6762 6.7) from an ephemeral port, so responders answer us directly and no multicast
//! group or port 5353 is needed on our side. PTR names the instance, its SRV the port and host, an A record the
//! address (the sender's address when a responder leaves the A record out).

use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::time::{Duration, Instant};

#[cfg(test)]
const SERVICE: &str = "_snapcast._tcp.local";
const GROUP: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(224, 0, 0, 251), 5353);
const A: u16 = 1;

#[derive(Debug, PartialEq)]
pub struct Found {
    pub name: String,
    pub addr: Ipv4Addr,
    pub port: u16,
}

/// Every instance of `service` that answers within `wait`, asked once from each of `local` (interface addresses): a
/// query from an unbound socket goes out on one interface only (the multicast route's), so a server reachable on
/// another one (the wired link next to Wi-Fi) would never be heard. Duplicates (same address and port) dropped.
pub fn browse_all(service: &str, local: &[Ipv4Addr], wait: Duration) -> Vec<Found> {
    let q = query(service);
    let socks: Vec<UdpSocket> = local
        .iter()
        .filter_map(|&a| {
            let s = UdpSocket::bind(SocketAddrV4::new(a, 0)).ok()?;
            s.set_multicast_ttl_v4(255).ok();
            let ifaddr = libc::in_addr { s_addr: u32::from_ne_bytes(a.octets()) };
            use std::os::unix::io::AsRawFd;
            unsafe {
                libc::setsockopt(
                    s.as_raw_fd(),
                    libc::IPPROTO_IP,
                    libc::IP_MULTICAST_IF,
                    &ifaddr as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::in_addr>() as libc::socklen_t,
                );
            }
            s.send_to(&q, GROUP).ok()?;
            s.set_nonblocking(true).ok()?;
            Some(s)
        })
        .collect();
    let end = Instant::now() + wait;
    let mut found: Vec<Found> = Vec::new();
    let mut buf = [0u8; 1500];
    while Instant::now() < end && !socks.is_empty() {
        let mut got = false;
        for s in &socks {
            while let Ok((n, from)) = s.recv_from(&mut buf) {
                got = true;
                let std::net::IpAddr::V4(from) = from.ip() else { continue };
                if let Some(f) = parse(&buf[..n], service, from)
                    && !found.iter().any(|g| g.addr == f.addr && g.port == f.port)
                {
                    found.push(f);
                }
            }
        }
        if !got {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    found
}

/// One question, the PTR records of `name`, id 0, no flags (RFC 6762 18).
pub fn query(name: &str) -> Vec<u8> {
    let mut q = vec![0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    put_name(&mut q, name);
    q.extend_from_slice(&[0, PTR as u8, 0, 1]);
    q
}

const PTR: u16 = 12;
const SRV: u16 = 33;

fn put_name(out: &mut Vec<u8>, name: &str) {
    for label in name.split('.').filter(|l| !l.is_empty()) {
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
}

/// A name at `pos` (labels and compression pointers); the name and the position after it in the record.
fn name(m: &[u8], mut pos: usize) -> Option<(String, usize)> {
    let mut out = String::new();
    let mut after = None;
    for _ in 0..64 {
        // pointer loops end here
        let len = *m.get(pos)? as usize;
        if len == 0 {
            return Some((out, after.unwrap_or(pos + 1)));
        }
        if len & 0xC0 == 0xC0 {
            let p = (len & 0x3F) << 8 | *m.get(pos + 1)? as usize;
            after.get_or_insert(pos + 2);
            pos = p;
            continue;
        }
        if !out.is_empty() {
            out.push('.');
        }
        out.push_str(&String::from_utf8_lossy(m.get(pos + 1..pos + 1 + len)?));
        pos += 1 + len;
    }
    None
}

fn be16(m: &[u8], p: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*m.get(p)?, *m.get(p + 1)?]))
}

type Rec = (String, u16, usize, usize); // name, type, rdata offset, rdata length

/// The records of a response (answers, authority, additionals).
fn records(m: &[u8]) -> Option<Vec<Rec>> {
    if m.len() < 12 || m[2] & 0x80 == 0 {
        return None;
    } // responses only
    let qd = be16(m, 4)?;
    let rr = be16(m, 6)? as usize + be16(m, 8)? as usize + be16(m, 10)? as usize;
    let mut pos = 12;
    for _ in 0..qd {
        pos = name(m, pos)?.1 + 4;
    }
    let mut recs = Vec::new(); // (name, type, rdata offset, rdata length)
    for _ in 0..rr {
        let (n, p) = name(m, pos)?;
        let (typ, len) = (be16(m, p)?, be16(m, p + 8)? as usize);
        if p + 10 + len > m.len() {
            return None;
        }
        recs.push((n, typ, p + 10, len));
        pos = p + 10 + len;
    }
    Some(recs)
}

fn eq(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// The server from a response: the PTR for `service`, the SRV of that instance, the A of its target (or `from`).
pub fn parse(m: &[u8], service: &str, from: Ipv4Addr) -> Option<Found> {
    let recs = records(m)?;
    let inst = recs.iter().find(|r| r.1 == PTR && eq(&r.0, service)).and_then(|r| name(m, r.2))?.0;
    let mut f = instance(m, &recs, &inst, from)?;
    let label = inst.strip_suffix(service).map(|s| s.trim_end_matches('.')).unwrap_or(&inst);
    f.name = label.to_string();
    Some(f)
}

fn instance(m: &[u8], recs: &[Rec], inst: &str, from: Ipv4Addr) -> Option<Found> {
    let srv = recs.iter().find(|r| r.1 == SRV && eq(&r.0, inst) && r.3 >= 7)?;
    let port = be16(m, srv.2 + 4)?;
    let target = name(m, srv.2 + 6)?.0;
    let addr = recs
        .iter()
        .find(|r| r.1 == A && r.3 == 4 && eq(&r.0, &target))
        .map(|r| Ipv4Addr::new(m[r.2], m[r.2 + 1], m[r.2 + 2], m[r.2 + 3]))
        .unwrap_or(from);
    Some(Found { name: inst.to_string(), addr, port })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What Avahi sends for `snapserver` on host "kitchen": PTR in answers, SRV/TXT/A in additionals, compressed.
    fn avahi_answer(with_a: bool) -> Vec<u8> {
        let mut m = vec![0, 0, 0x84, 0, 0, 0, 0, 1, 0, 0, 0, if with_a { 2 } else { 1 }];
        put_name(&mut m, SERVICE); // @12
        m.extend_from_slice(&[0, 12, 0, 1, 0, 0, 0x11, 0x94]);
        let inst = m.len() + 2; // rdata: "Snapcast" + pointer to the service name
        let rdata = [&[8u8][..], b"Snapcast", &[0xC0, 12]].concat();
        m.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
        m.extend_from_slice(&rdata);
        m.extend_from_slice(&[0xC0, inst as u8, 0, 33, 0x80, 1, 0, 0, 0, 120]);
        let target = [&[7u8][..], b"kitchen", &[5], b"local", &[0]].concat();
        let srv = [&[0u8, 0, 0, 0, 0x06, 0xA8][..], &target].concat(); // priority, weight, port 1704
        m.extend_from_slice(&(srv.len() as u16).to_be_bytes());
        let target_at = m.len() + 6;
        m.extend_from_slice(&srv);
        if with_a {
            m.extend_from_slice(&[0xC0, target_at as u8, 0, 1, 0x80, 1, 0, 0, 0, 120, 0, 4, 192, 0, 2, 20]);
        }
        m
    }

    #[test]
    fn the_query_asks_for_the_snapcast_ptr() {
        let q = query(SERVICE);
        assert_eq!(&q[..12], &[0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
        assert_eq!(name(&q, 12).unwrap(), (SERVICE.to_string(), q.len() - 4));
        assert_eq!(&q[q.len() - 4..], &[0, 12, 0, 1]);
    }

    #[test]
    fn an_avahi_answer_gives_name_address_and_port() {
        let f = parse(&avahi_answer(true), SERVICE, Ipv4Addr::new(10, 0, 0, 9)).unwrap();
        assert_eq!(f, Found { name: "Snapcast".into(), addr: Ipv4Addr::new(192, 0, 2, 20), port: 1704 });
    }

    #[test]
    fn without_an_a_record_the_sender_is_the_server() {
        let f = parse(&avahi_answer(false), SERVICE, Ipv4Addr::new(10, 0, 0, 9)).unwrap();
        assert_eq!(f.addr, Ipv4Addr::new(10, 0, 0, 9));
    }

    #[test]
    fn queries_truncated_packets_and_pointer_loops_are_ignored() {
        let from = Ipv4Addr::LOCALHOST;
        assert_eq!(parse(&query(SERVICE), SERVICE, from), None);
        let a = avahi_answer(true);
        for n in 0..a.len() {
            let _ = parse(&a[..n], SERVICE, from);
        }
        let mut lp = vec![0, 0, 0x84, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0xC0, 12];
        lp.extend_from_slice(&[0; 10]);
        assert_eq!(parse(&lp, SERVICE, from), None);
    }
}
