//! zd1-memsh: the stock KnOS shell's `wr ADDR VAL` / `rd ADDR N` over TCP, on `/dev/mem`, so the Mac-side Warp
//! and Sol probe tools (tools/zeppelin/warp/warprun.py and re/netmodule/sol-probes, through tools/zeppelin/knos)
//! run unchanged against our OS: same prompt (`sds://>`), same replies ("set to value", "ADDR: w w w w").
//! Physical addresses, 32-bit, through dm870's DevMem (O_SYNC mappings, uncached: what the Warp cores and Sol see).
//!
//!   zd1-memsh [PORT]        default 23; one client at a time (like the KnOS shell); started by init's `debug` service
//!                           when the debug services are on. Raw memory for anyone who connects: only in the system's
//!                           debug build.
use dm870::DevMem;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

const PROMPT: &[u8] = b"sds://>";

/// A word's address: 32-bit words only (dm870's DevMem maps each page once and keeps it).
fn aligned(addr: u32) -> Result<u32, String> {
    if addr & 3 != 0 {
        return Err(format!("unaligned address {addr:#010x}"));
    }
    Ok(addr)
}

fn rd(mem: &mut DevMem, addr: u32) -> Result<u32, String> {
    mem.read32(aligned(addr)?).map_err(|e| e.to_string())
}

fn wr(mem: &mut DevMem, addr: u32, v: u32) -> Result<(), String> {
    mem.write32(aligned(addr)?, v).map_err(|e| e.to_string())
}

fn num(s: &str) -> Option<u32> {
    let t = s.trim();
    if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u32::from_str_radix(h, 16).ok()
    } else if t.chars().all(|c| c.is_ascii_digit()) {
        t.parse::<u32>().ok()
    } else {
        u32::from_str_radix(t, 16).ok() // KnOS takes bare hex
    }
}

/// One command line -> the reply (without the prompt).
fn exec(mem: &mut DevMem, line: &str) -> String {
    let t: Vec<&str> = line.split_whitespace().collect();
    match t.as_slice() {
        [] => String::new(),
        ["wr", a, v] => match (num(a), num(v)) {
            (Some(a), Some(v)) => match wr(mem, a, v) {
                Ok(()) => format!("{:#010x} set to value {:#010x}\r\n", a, v),
                Err(e) => format!("error: {}\r\n", e),
            },
            _ => "usage: wr ADDR VALUE\r\n".into(),
        },
        ["rd", a] | ["rd", a, _] => {
            let n = t.get(2).and_then(|x| num(x)).unwrap_or(1).min(4096);
            match num(a) {
                Some(a) => {
                    let mut out = String::new();
                    let mut i = 0;
                    while i < n {
                        out += &format!("{:08X}:", a.wrapping_add(4 * i));
                        for _ in 0..4.min(n - i) {
                            match rd(mem, a.wrapping_add(4 * i)) {
                                Ok(v) => out += &format!(" {:08X}", v),
                                Err(e) => return format!("error: {}\r\n", e),
                            }
                            i += 1;
                        }
                        out += "\r\n";
                    }
                    out
                }
                None => "usage: rd ADDR [N]\r\n".into(),
            }
        }
        ["os", "timestamp"] => format!("{}\r\n", unsafe { libc::time(std::ptr::null_mut()) }),
        ["help"] => "wr ADDR VALUE | rd ADDR [N] (32-bit, physical)\r\n".into(),
        _ => "?\r\n".into(),
    }
}

fn session(mut s: TcpStream, mem: &mut DevMem) -> std::io::Result<()> {
    s.set_nodelay(true)?;
    s.write_all(PROMPT)?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = s.read(&mut chunk)?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        while let Some(i) = buf.iter().position(|&b| b == b'\n' || b == b'\r') {
            let crlf = buf[i] == b'\r' && buf.get(i + 1) == Some(&b'\n'); // CRLF = one line, one prompt
            let line: Vec<u8> =
                buf.drain(..i + if crlf { 2 } else { 1 }).filter(|&b| b != b'\r' && b != b'\n' && b != 0x18).collect();
            let line = String::from_utf8_lossy(&line).to_string();
            let mut reply = Vec::new();
            if !line.trim().is_empty() {
                reply.extend_from_slice(line.as_bytes()); // the echo the client slices off
                reply.extend_from_slice(b"\r\n");
                reply.extend_from_slice(exec(mem, &line).as_bytes());
            }
            reply.extend_from_slice(PROMPT);
            s.write_all(&reply)?;
        }
    }
}

pub fn main() {
    let port: u16 = std::env::args().nth(1).and_then(|p| p.parse().ok()).unwrap_or(23);
    let mut mem = match DevMem::open() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("zd1-memsh: /dev/mem: {}", e);
            std::process::exit(1);
        }
    };
    let l = match TcpListener::bind(("0.0.0.0", port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("zd1-memsh: bind {}: {}", port, e);
            std::process::exit(1);
        }
    };
    println!("zd1-memsh: listening on {}", port);
    for s in l.incoming().flatten() {
        if let Err(e) = session(s, &mut mem) {
            eprintln!("zd1-memsh: session: {}", e);
        }
    }
}
