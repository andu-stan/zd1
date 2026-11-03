//! zd1-update: a system image from the MCU into the slot that doesn't run (docs/module/updater.md). zd1-linkd starts
//! it when the MCU writes reg 0xC5 (the host's `Update` BEGIN):
//!   zd1-update SIZE BAUD SHA256 ID...
//! The image comes on the module console's UART (ttyS1, the MCU's USART1): for the time it runs the line is ours,
//! raw at BAUD; the console's other readers (the shell) are stopped and the kernel's console messages muted. J2 has
//! no RTS/CTS and Linux's tty buffer is 4 KB, so the MCU sends only what we let it: frames 7E SEQ LEN(2, LE)
//! DATA(<= 512) CRC32(4, LE; over SEQ, LEN and DATA), four in flight. We answer a good one with 0E SEQ as we read it;
//! a bad one, a missing one or 500 ms of nothing with 15 SEQ (the frame we want next) and skip to that frame's start.
//! The module loses bytes at 1 Mbaud while it writes the NAND [HW]: the frames take that.
//!
//! Our lines to the MCU are 0x02 TEXT \n:
//!   zd1-update ready SLOT          at BAUD, before the first frame
//!   zd1-update ok SLOT ID          installed and stamped (the MCU then starts it with `Boot` TRY)
//!   zd1-update error WHY           the slot holds no system
//! then the line goes back to 115200 and the console to the shell. 10 s without data ends it (an error).

use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use zd1_slot::{boot, install};
use zd1_sys::{crc32_update, hex, unhex};

const TTY: &str = "/dev/ttyS1";
const IDLE_MS: i32 = 10_000;
const LINE: u8 = 0x02;
const SOF: u8 = 0x7E;
const FACK: u8 = 0x0E;
const FNAK: u8 = 0x15;
const FRAME: usize = 512;
const RESEND_MS: i32 = 500;

/// A line on the kernel log and in our own (/var/log/zd1-update.log: our stderr).
fn say(s: &str) {
    zd1_sys::kmsg("[zd1-update]", s);
    eprintln!("[zd1-update] {s}");
}

/// What we took of the console while we have the line, written down as it is taken (CONSOLE_TAKEN): who else reads
/// it (stopped, then continued), the kernel's console level, the line's settings. [`restore`] gives it back, from our
/// Drop or, when we die without one (killed, a panic: abort), from zd1-linkd once it has reaped us.
const CONSOLE_TAKEN: &str = "/run/zd1/update-console";

struct Console;

impl Console {
    fn take() -> Console {
        let me = std::process::id() as i32;
        let mut stopped = Vec::new();
        for e in fs::read_dir("/proc").into_iter().flatten().flatten() {
            let Ok(pid) = e.file_name().to_string_lossy().parse::<i32>() else { continue };
            if pid == 1 || pid == me {
                continue;
            }
            let reads_console = fs::read_link(format!("/proc/{pid}/fd/0"))
                .map(|p| {
                    let p = p.to_string_lossy().to_string();
                    p == "/dev/console" || p == TTY
                })
                .unwrap_or(false);
            if reads_console && unsafe { libc::kill(pid, libc::SIGSTOP) } == 0 {
                stopped.push(pid);
            }
        }
        let printk = fs::read_to_string("/proc/sys/kernel/printk").unwrap_or_default();
        let level = printk.split_whitespace().next().unwrap_or("7");
        let pids: Vec<String> = stopped.iter().map(i32::to_string).collect();
        let _ = fs::create_dir_all("/run/zd1");
        let _ = fs::write(CONSOLE_TAKEN, format!("printk {level}\nstopped {}\n", pids.join(" ")));
        let _ = fs::write("/proc/sys/kernel/printk", "1"); // dmesg -n 1: only emergencies on the line
        Console
    }

    /// The line's settings before we changed them, for [`restore`].
    fn line_was(&self, t: &libc::termios) {
        let bytes =
            unsafe { std::slice::from_raw_parts(t as *const libc::termios as *const u8, size_of::<libc::termios>()) };
        if let Ok(mut f) = OpenOptions::new().append(true).open(CONSOLE_TAKEN) {
            let _ = writeln!(f, "termios {}", hex(bytes));
        }
    }
}

impl Drop for Console {
    fn drop(&mut self) {
        restore();
    }
}

/// Give the console back if an update took it (CONSOLE_TAKEN): the line's settings, the kernel's console level, the
/// stopped readers continued. Nothing to do when none did.
pub fn restore() {
    let Ok(taken) = fs::read_to_string(CONSOLE_TAKEN) else { return };
    for l in taken.lines() {
        match l.split_once(' ') {
            Some(("termios", h)) => {
                let b = unhex(h).unwrap_or_default();
                if b.len() == size_of::<libc::termios>() {
                    let t: libc::termios = unsafe { std::ptr::read_unaligned(b.as_ptr() as *const libc::termios) };
                    if let Ok(f) = OpenOptions::new().read(true).write(true).custom_flags(libc::O_NOCTTY).open(TTY) {
                        unsafe {
                            libc::tcsetattr(f.as_raw_fd(), libc::TCSADRAIN, &t);
                        }
                    }
                }
            }
            Some(("printk", level)) => {
                let _ = fs::write("/proc/sys/kernel/printk", level);
            }
            Some(("stopped", pids)) => {
                for pid in pids.split_whitespace().filter_map(|p| p.parse::<i32>().ok()) {
                    unsafe {
                        libc::kill(pid, libc::SIGCONT);
                    }
                }
            }
            _ => {}
        }
    }
    let _ = fs::remove_file(CONSOLE_TAKEN);
}

/// ttyS1 raw at a speed (the console's settings come back with the console: [`restore`]).
struct Line {
    f: File,
}

fn speed(baud: u32) -> Option<libc::speed_t> {
    Some(match baud {
        115200 => libc::B115200,
        230400 => libc::B230400,
        460800 => libc::B460800,
        921600 => libc::B921600,
        1000000 => libc::B1000000,
        2000000 => libc::B2000000,
        _ => return None,
    })
}

impl Line {
    fn open(baud: u32, console: &Console) -> Result<Line, String> {
        let sp = speed(baud).ok_or(format!("baud {baud}: not one of ours"))?;
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY)
            .open(TTY)
            .map_err(|e| format!("{TTY}: {e}"))?;
        let fd = f.as_raw_fd();
        let mut old: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(fd, &mut old) } != 0 {
            return Err(format!("{TTY}: tcgetattr"));
        }
        console.line_was(&old);
        let mut t = old;
        unsafe {
            libc::cfmakeraw(&mut t);
            libc::cfsetispeed(&mut t, sp);
            libc::cfsetospeed(&mut t, sp);
        }
        t.c_cflag |= libc::CLOCAL | libc::CREAD;
        t.c_cflag &= !libc::CRTSCTS;
        if unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &t) } != 0 {
            return Err(format!("{TTY}: tcsetattr {baud}"));
        }
        low_latency(fd);
        Ok(Line { f })
    }
    fn line(&mut self, s: &str) {
        let mut b = vec![LINE];
        b.extend_from_slice(s.as_bytes());
        b.push(b'\n');
        let _ = self.f.write_all(&b);
    }
}

impl Drop for Line {
    fn drop(&mut self) {
        unsafe {
            libc::tcdrain(self.f.as_raw_fd()); // our last line out at our speed
        }
    }
}

/// The tty's low-latency flag: received bytes reach the reader from the interrupt, not a tick later (HZ = 100: 10 ms
/// a read, a window's worth of the line at 1 Mbaud). Best effort.
fn low_latency(fd: i32) {
    const TIOCGSERIAL: libc::c_ulong = 0x541E;
    const TIOCSSERIAL: libc::c_ulong = 0x541F;
    const ASYNC_LOW_LATENCY: i32 = 1 << 13;
    let mut ss = [0i32; 32]; // struct serial_struct (60 bytes on ARM): flags at 16
    unsafe {
        if libc::ioctl(fd, TIOCGSERIAL as _, ss.as_mut_ptr()) == 0 {
            ss[4] |= ASYNC_LOW_LATENCY;
            libc::ioctl(fd, TIOCSSERIAL as _, ss.as_ptr());
        }
    }
}

/// The image as it comes, in frames (each answered): `left` bytes of it, hashed.
struct Stream<'a> {
    line: &'a mut Line,
    left: u64,
    hash: Sha256,
    rx: Vec<u8>,
    at: usize,
    data: Vec<u8>,
    took: usize,
    expect: u32,
    recovering: bool,
    naks: u32,
}

impl Stream<'_> {
    /// The next byte from the line, None after `ms` of nothing.
    fn byte(&mut self, ms: i32) -> io::Result<Option<u8>> {
        if self.at == self.rx.len() {
            let fd = self.line.f.as_raw_fd();
            let mut p = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
            if unsafe { libc::poll(&mut p, 1, ms) } <= 0 {
                return Ok(None);
            }
            self.rx.resize(4096, 0);
            let n = self.line.f.read(&mut self.rx)?;
            if n == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the line closed"));
            }
            self.rx.truncate(n);
            self.at = 0;
        }
        self.at += 1;
        Ok(Some(self.rx[self.at - 1]))
    }

    fn idle(&self) -> io::Error {
        io::Error::new(io::ErrorKind::TimedOut, format!("no data for 10 s, {} bytes short", self.left))
    }

    fn nak(&mut self) -> io::Result<()> {
        self.naks += 1;
        self.recovering = true;
        self.line.f.write_all(&[FNAK, self.expect as u8])
    }

    /// The frame we expect next, into `data`: whatever comes before its start skipped, anything wrong asked again.
    fn frame(&mut self) -> io::Result<()> {
        let want = (self.left as usize).min(FRAME);
        let mut last = std::time::Instant::now();
        'hunt: loop {
            if last.elapsed().as_millis() as i32 > IDLE_MS {
                return Err(self.idle());
            }
            let mut h = [0u8; 4];
            for (i, b) in h.iter_mut().enumerate() {
                match self.byte(RESEND_MS)? {
                    None => {
                        self.nak()?;
                        continue 'hunt;
                    }
                    Some(c) => *b = c,
                }
                last = std::time::Instant::now();
                if i == 0 && *b != SOF {
                    continue 'hunt;
                }
                if i == 1 && *b != self.expect as u8 {
                    if !self.recovering {
                        self.nak()?;
                    }
                    continue 'hunt;
                }
            }
            if u16::from_le_bytes([h[2], h[3]]) as usize != want {
                self.nak()?;
                continue;
            }
            self.data.clear();
            for _ in 0..want + 4 {
                match self.byte(RESEND_MS)? {
                    None => {
                        self.nak()?;
                        continue 'hunt;
                    }
                    Some(c) => self.data.push(c),
                }
            }
            let crc = u32::from_le_bytes(self.data[want..].try_into().unwrap());
            self.data.truncate(want);
            if crc32_update(crc32_update(!0, &h[1..]), &self.data) != !crc {
                self.nak()?;
                continue;
            }
            self.line.f.write_all(&[FACK, self.expect as u8])?;
            self.expect += 1;
            self.recovering = false;
            self.took = 0;
            return Ok(());
        }
    }
}

impl Read for Stream<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.left == 0 {
            return Ok(0);
        }
        if self.took == self.data.len() {
            self.frame()?;
        }
        let n = buf.len().min(self.left as usize).min(self.data.len() - self.took);
        buf[..n].copy_from_slice(&self.data[self.took..self.took + n]);
        self.took += n;
        self.hash.update(&buf[..n]);
        self.left -= n as u64;
        Ok(n)
    }
}

fn run(size: u64, baud: u32, sha: [u8; 32], id: &str) -> Result<String, String> {
    let b = boot();
    let slot = b.target();
    let console = Console::take();
    let mut line = Line::open(baud, &console)?;
    line.line(&format!("zd1-update ready {}", slot.name()));
    let mut s = Stream {
        line: &mut line,
        left: size,
        hash: Sha256::new(),
        rx: Vec::new(),
        at: 0,
        data: Vec::new(),
        took: 0,
        expect: 0,
        recovering: false,
        naks: 0,
    };
    // the first frame before the slot is touched: an MCU that missed our echo of its BEGIN gave up and sends nothing,
    // and the slot that doesn't run (the way back) must not be erased for that [HW 2026-11-02]
    if size > 0 {
        s.frame().map_err(|e| format!("no data: {e}"))?;
    }
    let r = install(slot, id, &mut s, &mut |s: &mut Stream| {
        let mut rest = [0u8; 512];
        while s.left > 0 {
            s.read(&mut rest).map_err(|e| format!("the image: {e}"))?;
        } // past the gzip stream
        let got: [u8; 32] = std::mem::take(&mut s.hash).finalize().into();
        if got != sha {
            return Err(format!("SHA-256 {}, expected {}", hex(&got), hex(&sha)));
        }
        Ok(())
    });
    say(&format!("{} frames asked again", s.naks));
    match r {
        Ok(msg) => {
            line.line(&format!("zd1-update ok {} {id}", slot.name()));
            Ok(msg)
        }
        Err(e) => {
            line.line(&format!("zd1-update error {e}"));
            Err(e)
        }
    }
}

pub fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let parsed = (|| {
        let size: u64 = args.first()?.parse().ok()?;
        let baud: u32 = args.get(1)?.parse().ok()?;
        let sha: [u8; 32] = unhex(args.get(2)?)?.try_into().ok()?;
        let id = args[3..].join(" ");
        (!id.is_empty()).then_some((size, baud, sha, id))
    })();
    let Some((size, baud, sha, id)) = parsed else {
        eprintln!("usage: zd1-update SIZE BAUD SHA256 ID...  (zd1-linkd starts it for the MCU)");
        std::process::exit(2);
    };
    say(&format!("{size} bytes at {baud} baud into the slot that doesn't run, stamp \"{id}\""));
    match run(size, baud, sha, &id) {
        Ok(m) => say(&m),
        Err(e) => {
            say(&format!("FAILED: {e}"));
            std::process::exit(1);
        }
    }
}
