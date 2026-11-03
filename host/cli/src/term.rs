//! The terminal's side of the flows: the MCU's log, progress on one line, prompts, Ctrl-C. Unix only (termios,
//! SIGINT): zd1 runs on macOS and Linux; Windows can't open the speaker yet (no WinUSB descriptors in the firmware).

use std::cell::{Cell, RefCell};
use std::io::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use zd1_host::{Error, Events, Result};

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigint(_: libc::c_int) {
    if STOP.swap(true, Ordering::SeqCst) {
        std::process::exit(130);
    } // a second Ctrl-C: now
}

/// Ctrl-C asks the flow to stop where it safely can (an update: ABORT) instead of killing it mid-transfer.
pub fn catch_ctrl_c() {
    unsafe { libc::signal(libc::SIGINT, on_sigint as *const () as libc::sighandler_t) };
}

#[derive(Default)]
pub struct Term {
    log: Cell<bool>,
    line: Cell<bool>,
    rate: RefCell<Option<(String, f64)>>,
}

impl Term {
    /// Print the MCU's log as it comes (else it is dropped).
    pub fn show_log(&self, on: bool) {
        self.log.set(on)
    }

    fn end_line(&self) {
        if self.line.replace(false) {
            eprintln!();
        }
    }
}

impl Events for Term {
    fn phase(&self, n: usize, of: usize, what: &str) {
        self.end_line();
        eprintln!("[{n}/{of}] {what}");
    }
    fn text(&self, b: &[u8]) {
        if self.log.get() {
            self.end_line();
            print!("{}", String::from_utf8_lossy(b));
            let _ = std::io::stdout().flush();
        }
    }
    fn prompt(&self, s: &str) {
        self.end_line();
        eprintln!("{s}");
    }
    fn step(&self, s: &str) {
        self.end_line();
        println!("{s}");
    }
    fn progress(&self, what: &str, done: u64, total: u64) {
        let mut r = self.rate.borrow_mut();
        if r.as_ref().is_none_or(|(w, _)| w != what) {
            *r = Some((what.into(), zd1_host::now_ms()));
        }
        let dt = ((zd1_host::now_ms() - r.as_ref().unwrap().1) / 1000.0).max(1e-3);
        eprint!(
            "\r  {what} {:5.1} %  {:7} KB  {:6.1} KB/s ",
            100.0 * done as f64 / total.max(1) as f64,
            done / 1024,
            done as f64 / 1024.0 / dt
        );
        self.line.set(done < total);
        if done >= total {
            eprintln!();
            *r = None;
        }
    }
    fn wait(&self, what: &str, secs: u64) {
        self.end_line();
        eprint!("\r  {what}: {secs} s ");
        self.line.set(true);
    }
    fn cancelled(&self) -> bool {
        STOP.load(Ordering::SeqCst)
    }
    fn wants_device(&self, vid: u16, pid: u16, what: &str) {
        self.end_line();
        eprintln!("waiting for {what} ({vid:04X}:{pid:04X})");
    }
}

/// A passphrase from the terminal, not echoed.
pub fn passphrase(prompt: &str) -> Result<String> {
    eprint!("{prompt}");
    let fd = libc::STDIN_FILENO;
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    let tty = unsafe { libc::tcgetattr(fd, &mut t) } == 0;
    let old = t;
    if tty {
        t.c_lflag &= !libc::ECHO;
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &t) };
    }
    let mut s = String::new();
    let r = std::io::stdin().read_line(&mut s);
    if tty {
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &old) };
    }
    eprintln!();
    r.map_err(|e| Error::Failed(format!("passphrase: {e}")))?;
    Ok(s.trim_end_matches(['\r', '\n']).to_string())
}

/// The terminal raw (every key as it is typed, Ctrl-C included) until dropped.
struct Raw(Option<libc::termios>);

impl Raw {
    fn new() -> Raw {
        let mut t: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(libc::STDIN_FILENO, &mut t) } != 0 {
            return Raw(None); // not a terminal
        }
        let old = t;
        unsafe {
            libc::cfmakeraw(&mut t);
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &t);
        }
        Raw(Some(old))
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        if let Some(t) = self.0 {
            unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSADRAIN, &t) };
        }
    }
}

/// A terminal on the module's console (Ctrl-] leaves): the keys to it as typed, what it says to stdout.
pub async fn console<U: zd1_host::usb::Usb>(m: &mut zd1_host::Mcu<U>) -> Result<()> {
    use std::io::Read as _;
    use std::sync::mpsc;
    m.console_open(true).await?;
    eprintln!("the module's console over the host link (Ctrl-] leaves)");
    let raw = Raw::new();
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut b = [0u8; 1024];
        while let Ok(n @ 1..) = std::io::stdin().read(&mut b) {
            if tx.send(b[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let r = async {
        loop {
            let n = m.poll().await?;
            let out = m.console_take();
            if !out.is_empty() {
                let mut o = std::io::stdout();
                let _ = o.write_all(&out).and_then(|_| o.flush());
            }
            match rx.try_recv() {
                Ok(b) => match b.iter().position(|&c| c == 0x1d) {
                    Some(i) => return m.console_write(&b[..i]).await,
                    None => m.console_write(&b).await?,
                },
                Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
                Err(mpsc::TryRecvError::Empty) if n == 0 => m.usb.sleep(10).await,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
    }
    .await;
    drop(raw);
    eprintln!("\r\n[console] closed");
    m.console_open(false).await?;
    r
}
