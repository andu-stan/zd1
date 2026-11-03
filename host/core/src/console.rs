//! The network module's console over the host link (docs/protocol/host.md: `Console`, `ConsoleWrite` and the CONSOLE
//! records): our OS's shell driven from here, a command at a time to its end. The terminal on it is the front-end's
//! (`zd1 console`, the page's pane); this is what both front-ends share. BL1's escape into BL2 is the MCU's
//! (`Module` ESC: it knows when the reset is released).

use crate::usb::Usb;
use crate::{Error, Mcu, Result};

/// The console open for this host, for commands on our OS's shell; [`Shell::close`] closes it.
pub struct Shell<'a, U: Usb> {
    pub m: &'a mut Mcu<U>,
    /// A fresh line has been made (whatever was half typed dropped).
    ready: bool,
}

impl<'a, U: Usb> Shell<'a, U> {
    /// Open the console (afresh).
    pub async fn open(m: &'a mut Mcu<U>) -> Result<Self> {
        m.console_open(true).await?;
        Ok(Shell { m, ready: false })
    }

    /// Close the console: the module's bytes lost while it was open (the host didn't poll fast enough), if any is
    /// an error.
    pub async fn close(self) -> Result<()> {
        match self.m.console_open(false).await?.lost {
            0 => Ok(()),
            n => Err(Error::Failed(format!("the console lost {n} of the module's bytes: POLL was too slow"))),
        }
    }

    /// What the console says within `ms` of the device's time.
    pub async fn read_for(&mut self, ms: u32) -> Result<Vec<u8>> {
        let t0 = crate::now_ms();
        let mut out = Vec::new();
        while crate::now_ms() - t0 < (ms * self.m.usb.slow()) as f64 {
            if self.m.poll().await? == 0 {
                self.m.usb.sleep(10).await;
            }
            out.extend(self.m.console_take());
        }
        Ok(out)
    }

    /// A command on the shell, to its end: its output and exit status. A marker echoed after it ends the wait, so
    /// nothing depends on how long the command takes (`timeout_ms` at most, in the device's time).
    pub async fn run(&mut self, cmd: &str, timeout_ms: u32) -> Result<(String, i32)> {
        if !self.ready {
            // a fresh line: whatever was half typed is dropped
            self.m.console_write(b"\x03\n").await?;
            self.read_for(500).await?;
            self.ready = true;
        }
        let tag = format!("__zd1_{:06}__", crate::seed() % 1_000_000);
        self.m.console_write(format!("{cmd}; echo {tag}$?{tag}\n").as_bytes()).await?;
        let (t0, mut buf) = (crate::now_ms(), Vec::new());
        while crate::now_ms() - t0 < (timeout_ms * self.m.usb.slow()) as f64 {
            if self.m.poll().await? == 0 {
                self.m.usb.sleep(10).await;
            }
            buf.extend(self.m.console_take());
            if let Some((at, status)) = find_status(&buf, tag.as_bytes()) {
                let text = String::from_utf8_lossy(&strip_ansi(&buf[..at])).replace('\r', "");
                let lines: Vec<&str> = text.split('\n').collect();
                // the command's own echo
                let echo = format!("{tag}$?");
                let k = lines.iter().position(|l| l.contains(&echo)).map_or(0, |i| i + 1);
                return Ok((lines[k..].join("\n").trim_end_matches('\n').to_string(), status));
            }
        }
        Err(Error::Timeout(format!("{cmd:?} did not finish within {} s", timeout_ms / 1000)))
    }
}

/// `TAG<digits>TAG` in `buf`: where it starts and the number.
fn find_status(buf: &[u8], tag: &[u8]) -> Option<(usize, i32)> {
    let mut from = 0;
    while let Some(i) = find(&buf[from..], tag).map(|i| i + from) {
        let rest = &buf[i + tag.len()..];
        let digits = rest.iter().take_while(|c| c.is_ascii_digit()).count();
        if digits > 0 && rest[digits..].starts_with(tag) {
            let n = std::str::from_utf8(&rest[..digits]).ok()?.parse().ok()?;
            return Some((i, n));
        }
        from = i + 1;
    }
    None
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

/// Terminal escapes (`ESC [ ... letter`) out: the shell's line editing.
pub fn strip_ansi(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == 0x1b && b.get(i + 1) == Some(&b'[') {
            let mut j = i + 2;
            while j < b.len() && (b[j].is_ascii_digit() || b[j] == b';' || b[j] == b'?') {
                j += 1;
            }
            if j < b.len() && b[j].is_ascii_alphabetic() {
                i = j + 1;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_status_after_the_echo() {
        let tag = b"__zd1_123456__";
        let buf = b"ls; echo __zd1_123456__$?__zd1_123456__\r\nbin\r\n__zd1_123456__0__zd1_123456__\r\n# ";
        let (at, n) = find_status(buf, tag).unwrap();
        assert_eq!((n, &buf[at..at + 6]), (0, &b"__zd1_"[..]));
        assert_eq!(find_status(b"echo __zd1_123456__$?__zd1_123456__", tag), None);
        assert_eq!(find_status(b"__zd1_123456__127__zd1_123456__", tag).map(|r| r.1), Some(127));
    }

    #[test]
    fn escapes_out() {
        assert_eq!(strip_ansi(b"\x1b[1;32mok\x1b[0m \x1b[?25h!"), b"ok !");
        assert_eq!(strip_ansi(b"a\x1b"), b"a\x1b");
    }
}
