//! A PTP master for the receiver under test: the Mac's CLOCK_UPTIME_RAW (mach_absolute_time, also CoreAudio's host
//! time, so microphone timestamps compare directly) served as IEEE 1588 two-step Sync /
//! Follow_Up at 8 Hz (Apple's aPTP rate) and Announce at 1 Hz, unicast to the receiver's 319 / 320. The same clock
//! stamps the stream's SETRATEANCHORTIME (`now_ns`), so the receiver can be checked against a clock we own.

use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::time::Duration;

pub const CLOCK_ID: u64 = 0x0211_22ff_fe33_4455;

/// The master's time: CLOCK_UPTIME_RAW, ns.
pub fn now_ns() -> u64 {
    let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: clock_gettime writes the timespec it is given
    unsafe { libc::clock_gettime(libc::CLOCK_UPTIME_RAW, &mut t) };
    t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64
}

fn header(buf: &mut [u8], msg_type: u8, len: usize, seq: u16, control: u8, log_interval: i8, two_step: bool) {
    buf[0] = 0x10 | msg_type; // transportSpecific 1 (802.1AS-style, as Apple sends)
    buf[1] = 2;
    buf[2..4].copy_from_slice(&(len as u16).to_be_bytes());
    if two_step {
        buf[6] = 0x02;
    }
    buf[20..28].copy_from_slice(&CLOCK_ID.to_be_bytes());
    buf[28..30].copy_from_slice(&1u16.to_be_bytes());
    buf[30..32].copy_from_slice(&seq.to_be_bytes());
    buf[32] = control;
    buf[33] = log_interval as u8;
}

fn timestamp(buf: &mut [u8], ns: u64) {
    let (s, n) = (ns / 1_000_000_000, (ns % 1_000_000_000) as u32);
    buf[0..2].copy_from_slice(&((s >> 32) as u16).to_be_bytes());
    buf[2..6].copy_from_slice(&(s as u32).to_be_bytes());
    buf[6..10].copy_from_slice(&n.to_be_bytes());
}

/// Serve the clock to `to` until `secs` have passed (forever if None). Blocks.
pub fn serve(to: IpAddr, secs: Option<f64>) {
    let sock = UdpSocket::bind(("0.0.0.0", 0)).expect("ptp socket");
    let (event, general) = (SocketAddr::new(to, 319), SocketAddr::new(to, 320));
    let start = now_ns();
    let mut seq: u16 = 0;
    loop {
        let t = now_ns();
        if secs.is_some_and(|s| (t - start) as f64 / 1e9 >= s) {
            return;
        }
        if seq % 8 == 0 {
            let mut a = [0u8; 64];
            header(&mut a, 0xb, 64, seq / 8, 5, 0, false);
            a[47] = 248; // priority1
            a[48] = 248; // clockClass
            a[49] = 0xfe; // clockAccuracy unknown
            a[50..52].copy_from_slice(&0xffffu16.to_be_bytes());
            a[52] = 248; // priority2
            a[53..61].copy_from_slice(&CLOCK_ID.to_be_bytes());
            a[63] = 0xa0; // timeSource: internal oscillator
            let _ = sock.send_to(&a, general);
        }
        let mut sync = [0u8; 44];
        header(&mut sync, 0x0, 44, seq, 0, -3, true);
        let t1 = now_ns();
        let _ = sock.send_to(&sync, event);
        let mut fu = [0u8; 76];
        header(&mut fu, 0x8, 76, seq, 2, -3, false);
        timestamp(&mut fu[34..44], t1);
        // 802.1AS Follow_Up information TLV: type 3, length 28, 00-80-C2 / subtype 1, the rest zero
        fu[44..46].copy_from_slice(&3u16.to_be_bytes());
        fu[46..48].copy_from_slice(&28u16.to_be_bytes());
        fu[48..51].copy_from_slice(&[0x00, 0x80, 0xc2]);
        fu[53] = 1;
        let _ = sock.send_to(&fu, general);
        seq = seq.wrapping_add(1);
        let next = start + seq as u64 * 125_000_000;
        let now = now_ns();
        if next > now {
            std::thread::sleep(Duration::from_nanos(next - now));
        }
    }
}
