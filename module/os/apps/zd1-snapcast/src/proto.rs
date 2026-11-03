//! The Snapcast binary protocol (doc/binary_protocol.md upstream): a 26-byte little-endian header, then the typed
//! message. Times are (sec, usec) pairs: ours are the client's monotonic clock, the server's its own.

use std::io::{self, Read, Write};

pub const CODEC_HEADER: u16 = 1;
pub const WIRE_CHUNK: u16 = 2;
pub const SERVER_SETTINGS: u16 = 3;
pub const TIME: u16 = 4;
pub const HELLO: u16 = 5;
pub const CLIENT_INFO: u16 = 7;
pub const ERROR: u16 = 8;

pub fn tv(us: i64) -> [u8; 8] {
    let (s, u) = (us.div_euclid(1_000_000) as i32, us.rem_euclid(1_000_000) as i32);
    let mut b = [0u8; 8];
    b[..4].copy_from_slice(&s.to_le_bytes());
    b[4..].copy_from_slice(&u.to_le_bytes());
    b
}
pub fn tv_us(b: &[u8]) -> i64 {
    i32::from_le_bytes(b[..4].try_into().unwrap()) as i64 * 1_000_000
        + i32::from_le_bytes(b[4..8].try_into().unwrap()) as i64
}
fn u32le(b: &[u8]) -> u32 {
    u32::from_le_bytes(b[..4].try_into().unwrap())
}

pub struct Header {
    pub typ: u16,
    pub sent_us: i64,
    pub received_us: i64,
}

pub enum Msg {
    CodecHeader { codec: String, data: Vec<u8> },
    WireChunk { ts_us: i64, data: Vec<u8> },
    ServerSettings(String),
    Time { latency_us: i64 },
    Error(String),
    Other,
}

/// One message; `now` stamps its arrival (the header's received field).
pub fn read(r: &mut impl Read, now: impl Fn() -> i64) -> io::Result<(Header, Msg)> {
    let mut h = [0u8; 26];
    r.read_exact(&mut h)?;
    let received_us = now();
    let size = u32le(&h[22..]) as usize;
    if size > 4 << 20 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "message too big"));
    }
    let mut p = vec![0u8; size];
    r.read_exact(&mut p)?;
    let hdr = Header { typ: u16::from_le_bytes([h[0], h[1]]), sent_us: tv_us(&h[6..14]), received_us };
    let bad = || io::Error::new(io::ErrorKind::InvalidData, "short message");
    let lp = |b: &[u8], at: usize| -> io::Result<(Vec<u8>, usize)> {
        // length-prefixed field
        if b.len() < at + 4 {
            return Err(bad());
        }
        let n = u32le(&b[at..]) as usize;
        if b.len() < at + 4 + n {
            return Err(bad());
        }
        Ok((b[at + 4..at + 4 + n].to_vec(), at + 4 + n))
    };
    let msg = match hdr.typ {
        CODEC_HEADER => {
            let (c, at) = lp(&p, 0)?;
            let (d, _) = lp(&p, at)?;
            Msg::CodecHeader { codec: String::from_utf8_lossy(&c).into_owned(), data: d }
        }
        WIRE_CHUNK => {
            if p.len() < 8 {
                return Err(bad());
            }
            let (d, _) = lp(&p, 8)?;
            Msg::WireChunk { ts_us: tv_us(&p[..8]), data: d }
        }
        SERVER_SETTINGS => Msg::ServerSettings(String::from_utf8_lossy(&lp(&p, 0)?.0).into_owned()),
        TIME => {
            if p.len() < 8 {
                return Err(bad());
            }
            Msg::Time { latency_us: tv_us(&p[..8]) }
        }
        ERROR => {
            let (m, at) = lp(&p, 4).unwrap_or_default();
            let (d, _) = lp(&p, at).unwrap_or_default();
            Msg::Error(format!("{} {}", String::from_utf8_lossy(&m), String::from_utf8_lossy(&d)))
        }
        _ => Msg::Other,
    };
    Ok((hdr, msg))
}

pub fn write(w: &mut impl Write, typ: u16, id: u16, sent_us: i64, payload: &[u8]) -> io::Result<()> {
    let mut b = Vec::with_capacity(26 + payload.len());
    b.extend_from_slice(&typ.to_le_bytes());
    b.extend_from_slice(&id.to_le_bytes());
    b.extend_from_slice(&0u16.to_le_bytes());
    b.extend_from_slice(&tv(sent_us));
    b.extend_from_slice(&tv(0));
    b.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    b.extend_from_slice(payload);
    w.write_all(&b)
}

pub fn json_payload(json: &str) -> Vec<u8> {
    let mut p = (json.len() as u32).to_le_bytes().to_vec();
    p.extend_from_slice(json.as_bytes());
    p
}

/// `s` as the inside of a JSON string.
pub fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 => out += &format!("\\u{:04x}", c as u32),
            c => out.push(c),
        }
    }
    out
}

/// A number or boolean field of a flat JSON object (the server's settings): enough here, no JSON library.
pub fn json_field<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let k = format!("\"{key}\"");
    let at = json.find(&k)? + k.len();
    let rest = json[at..].trim_start().strip_prefix(':')?.trim_start();
    let end = rest.find([',', '}']).unwrap_or(rest.len());
    Some(rest[..end].trim())
}
