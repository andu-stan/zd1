//! zd1-entropy: credits the kernel's entropy pool from CPU timing jitter, once at boot (init's first service).
//!
//! On the module the pool reads 0 for good: Linux 2.6.24 credits entropy only from a few interrupt sources and
//! none of this board's drivers is one, and there is no getrandom(2). Whatever waits for /dev/random to become
//! readable (the `getrandom` crate behind `rand::OsRng`, libsodium) then waits forever; /dev/urandom works but is
//! seeded with next to nothing.
//!
//! Source: the time a memory-bound loop takes, measured with CLOCK_MONOTONIC, many times (as jitterentropy and
//! haveged do): SDRAM refresh, the DMA engines and the other bus masters (Ethernet, Wi-Fi, the Warp cores) make it
//! vary. Each delta's contribution is bounded by a min-entropy estimate over the whole run (-log2 of the most common
//! delta's frequency, after folding out the counter's step); the deltas are not independent, so each is credited at
//! a quarter of that and never more than half a bit. The raw deltas go into the pool. On the unit: ~6.6 bits per
//! delta estimated, so 4096 deltas (0.2 s) credit 2048 bits. No estimate (a coarse clock, identical deltas): nothing is
//! credited, and it says so.
//!
//!   zd1-entropy [--samples N] [-v]

use std::collections::HashMap;
use std::time::Instant;

const RNDADDENTROPY: libc::c_ulong = 0x4008_5203; // _IOW('R', 0x03, int[2])
const RNDGETENTCNT: libc::c_ulong = 0x8004_5200; // _IOR('R', 0x00, int)

fn now_ns() -> u64 {
    let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) };
    t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64
}

fn entropy_count(fd: i32) -> i32 {
    let mut n: libc::c_int = 0;
    unsafe { libc::ioctl(fd, RNDGETENTCNT as _, &mut n) };
    n
}

pub fn main() {
    let args: Vec<String> = std::env::args().collect();
    let verbose = args.iter().any(|a| a == "-v");
    let samples: usize = args
        .iter()
        .position(|a| a == "--samples")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(4096);

    // memory-bound work: strided writes over 64 KB (four times the D-cache), each pass a little different
    let mut mem = vec![0u8; 64 * 1024];
    let mut deltas = Vec::with_capacity(samples);
    let t0 = Instant::now();
    let mut x: usize = 0x9E37_79B9;
    for i in 0..samples {
        let a = now_ns();
        for k in 0..64 {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
            let j = (x >> 7) % mem.len();
            mem[j] = mem[j].wrapping_add(k as u8 ^ i as u8);
        }
        deltas.push(now_ns() - a);
    }
    std::hint::black_box(&mem);

    // the counter's step (a coarse clock moves in ticks): the gcd of the deltas
    let step = deltas.iter().fold(0u64, |g, &d| gcd(g, d)).max(1);
    let mut hist: HashMap<u64, usize> = HashMap::new();
    for d in &deltas {
        *hist.entry(d / step).or_default() += 1;
    }
    let most = *hist.values().max().unwrap_or(&samples) as f64;
    let h_min = -(most / samples as f64).log2(); // bits per delta
    let credit = (((h_min / 4.0).min(0.5) * samples as f64) as i32).clamp(0, 4096);
    if verbose {
        let mut v: Vec<_> = hist.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1));
        eprintln!(
            "zd1-entropy: {samples} deltas in {:?}, step {step} ns, {} distinct, top {:?}, min-entropy {h_min:.2} bits/delta",
            t0.elapsed(),
            hist.len(),
            &v[..v.len().min(6)]
        );
    }

    let fd = unsafe { libc::open(c"/dev/random".as_ptr(), libc::O_WRONLY) };
    if fd < 0 {
        eprintln!("zd1-entropy: /dev/random: {}", std::io::Error::last_os_error());
        std::process::exit(1);
    }
    let before = entropy_count(fd);
    // struct rand_pool_info { int entropy_count; int buf_size; __u32 buf[]; }
    let bytes: Vec<u8> = deltas.iter().flat_map(|d| (*d as u32).to_le_bytes()).collect();
    let mut req = Vec::with_capacity(8 + bytes.len());
    req.extend_from_slice(&credit.to_ne_bytes());
    req.extend_from_slice(&(bytes.len() as i32).to_ne_bytes());
    req.extend_from_slice(&bytes);
    let r = unsafe { libc::ioctl(fd, RNDADDENTROPY as _, req.as_ptr()) };
    if r != 0 {
        eprintln!("zd1-entropy: RNDADDENTROPY: {}", std::io::Error::last_os_error());
        std::process::exit(1);
    }
    let after = entropy_count(fd);
    println!("zd1-entropy: credited {credit} bits ({h_min:.2} bits/delta estimated), pool {before} -> {after}");
    if credit == 0 {
        eprintln!("zd1-entropy: no timing jitter found: nothing credited (readers of /dev/random will block)");
        std::process::exit(2);
    }
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}
