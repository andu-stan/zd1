//! The module's system over USB (docs/module/updater.md): the image into the slot that doesn't run, then that slot
//! started once; ready makes it the good one, else the MCU goes back to the old one.
//!
//! `Update` BEGIN (the MCU starts zd1-update over SPI and takes the console UART at the baud), the image to the bulk
//! endpoint (on the emulator's EP0-only bridge: `PipeData`), the MCU passing it on in frames as the module answers;
//! the module checks the SHA-256 and stamps the slot ("VERSION SHA256[:12] LENGTH"). Then `Boot` TRY with restart,
//! and the verdict.

use crate::image::system_stamp;
use crate::spec::{BootOp, Err, Slot, UpdateOp, UpdateState};
use crate::usb::Usb;
use crate::{Error, Mcu, Result, msg};

pub struct Options {
    /// The console UART's speed while it runs.
    pub baud: u32,
    /// Start the new slot once (else: installed only; `Boot` TRY later).
    pub start: bool,
    /// A test: ABORT this long into the stream (the MCU must answer, the slot holds nothing).
    pub abort_after_ms: Option<u32>,
}

impl Default for Options {
    fn default() -> Options {
        Options { baud: 1_000_000, start: true, abort_after_ms: None }
    }
}

/// How it ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// In `slot`, not started (`Options::start` off).
    Installed(Slot),
    /// Started, and it got ready: the good slot now.
    Running(Slot),
    /// Started, but it didn't get ready: the module is back on `now`.
    FellBack { slot: Slot, now: Slot },
}

/// Update the module's system with `image` (a gzip'd cpio of a slot's root).
pub async fn system<U: Usb>(m: &mut Mcu<U>, image: &[u8], o: &Options) -> Result<Outcome> {
    let e = m.events().clone();
    let i = m.call(&msg::Info).await?;
    if !i.mod_os.starts_with("zd1-os ") {
        return Err(Error::Failed(format!(
            "the module doesn't run our OS ({:?}): an update needs a slot or the rescue up",
            i.mod_os
        )));
    }
    let id = system_stamp(image)?;
    e.step(&format!("module: {}; {} bytes, stamp \"{id}\"", i.mod_os, image.len()));
    m.call(&msg::Update {
        op: UpdateOp::Begin,
        size: image.len() as u32,
        sha256: crate::sha256(image).to_vec(),
        id: id.clone(),
        baud: o.baud,
    })
    .await?;
    let t0 = crate::now_ms();
    let r = loop {
        let r = m.call(&msg::UpdateStatus).await?;
        if r.state != UpdateState::Starting {
            break r;
        }
        if crate::now_ms() - t0 > 20_000.0 * m.usb.slow() as f64 {
            return Err(Error::Timeout(format!("the MCU still waits for the module's updater: {}", r.text)));
        }
        m.usb.sleep(200).await;
    };
    if r.state != UpdateState::Streaming {
        return Err(Error::Failed(format!("update: {}", r.text)));
    }
    e.step(&format!("into slot {}", r.slot));
    let (t0, mut sent, mut aborted) = (crate::now_ms(), 0usize, false);
    let streaming = |r: &msg::UpdateReply| r.state == UpdateState::Streaming;
    if m.usb.ep0_only() {
        // the emulator: EP0 only
        while sent < image.len() {
            let n = 240.min(image.len() - sent);
            match m.call(&msg::PipeData { data: image[sent..sent + n].to_vec() }).await {
                Ok(_) => sent += n,
                Err(Error::Mcu(Err::Busy, _)) => {
                    if !streaming(&m.call(&msg::UpdateStatus).await?) {
                        break;
                    }
                    m.usb.sleep(10).await;
                }
                Err(x) => return Err(x),
            }
            if sent % (240 * 64) < 240 {
                e.progress("update", sent as u64, image.len() as u64);
            }
        }
    } else {
        let abort_at = o.abort_after_ms.map(|a| t0 + a as f64);
        sent = m
            .bulk("update", image, async |m, _| {
                if e.cancelled() || abort_at.is_some_and(|a| crate::now_ms() > a) {
                    // the MCU holds a full pipe and a packet in the endpoint's bank here: its abort must answer
                    let r = m.call(&msg::Update { op: UpdateOp::Abort, ..Default::default() }).await?;
                    e.step(&format!("aborted: {}", r.text));
                    aborted = true;
                    return Ok(false);
                }
                // it ended early: the rest would go to DISCARD
                Ok(streaming(&m.call(&msg::UpdateStatus).await?))
            })
            .await?;
    }
    e.progress("update", sent as u64, image.len() as u64);
    let t1 = crate::now_ms();
    let r = loop {
        e.wait("the module writes and checks the system", ((crate::now_ms() - t1) / 1000.0) as u64);
        let r = m.call(&msg::UpdateStatus).await?;
        if matches!(r.state, UpdateState::Done | UpdateState::Failed)
            || crate::now_ms() - t1 > 90_000.0 * m.usb.slow() as f64
        {
            break r;
        }
        m.usb.sleep(500).await;
    };
    e.step(&format!("module: {} ({:.0} s)", r.text, (crate::now_ms() - t0) / 1000.0));
    if aborted {
        return Err(Error::Cancelled);
    }
    if r.state != UpdateState::Done {
        return Err(Error::Failed(format!("update failed: {}", r.text)));
    }
    if !o.start {
        return Ok(Outcome::Installed(r.slot));
    }
    start_once(m, r.slot).await
}

/// `Boot` TRY the slot with restart, and wait for the verdict: the try used up (the stub asked), then the boot it
/// ended on ready: the new slot (now the good one), or another (the MCU went back: a ready clears the failures).
pub async fn start_once<U: Usb>(m: &mut Mcu<U>, slot: Slot) -> Result<Outcome> {
    let e = m.events().clone();
    e.step(&format!("starting slot {slot} once"));
    m.call(&msg::Boot { op: BootOp::Try, slot, restart: true }).await?;
    let t0 = crate::now_ms();
    while crate::now_ms() - t0 < 300_000.0 * m.usb.slow() as f64 {
        m.usb.sleep(1000).await;
        e.wait(&format!("slot {slot} starting"), ((crate::now_ms() - t0) / 1000.0) as u64);
        let Ok(b) = m.call(&msg::BootState).await else { continue }; // the MCU busy with the module's power: again
        if b.next != Slot::None || b.trial || b.last == Slot::None {
            continue;
        }
        let secs = (crate::now_ms() - t0) / 1000.0;
        if b.last == slot && b.good == slot {
            e.step(&format!("slot {slot} is up and the good slot now ({secs:.0} s)"));
            return Ok(Outcome::Running(slot));
        }
        e.step(&format!("slot {slot} didn't get ready: the module is back on slot {} ({secs:.0} s)", b.last));
        return Ok(Outcome::FellBack { slot, now: b.last });
    }
    Err(Error::Timeout(format!("slot {slot}: no verdict in 300 s (`zd1 slot`)")))
}

/// The emulator without the MCU's part: zd1-update started from the module console's shell and fed over the console
/// socket (`ZD1_MODCON_EMU`) as the MCU does it (mcu/src/update.c): frames of 512 bytes, 7E SEQ LEN(2) DATA CRC32,
/// four in flight, sent again from the one the module asks for. The slot it went into.
#[cfg(not(target_arch = "wasm32"))]
pub fn via_console<S: std::io::Read + std::io::Write>(
    c: &mut S,
    image: &[u8],
    baud: u32,
    ev: &dyn crate::Events,
) -> Result<Slot> {
    use std::io::ErrorKind::{TimedOut, WouldBlock};
    const FRAME: usize = 512;
    const FRAMES: usize = 4;
    let io = |x: std::io::Error| Error::Device(format!("the module console: {x}"));
    let read = |c: &mut S, buf: &mut Vec<u8>| -> Result<()> {
        let mut b = [0u8; 4096];
        match c.read(&mut b) {
            Ok(n) => {
                buf.extend(&b[..n]);
                Ok(())
            }
            Err(x) if x.kind() == WouldBlock || x.kind() == TimedOut => Ok(()),
            Err(x) => Err(io(x)),
        }
    };
    let id = system_stamp(image)?;
    c.write_all(b"\r").map_err(io)?;
    c.write_all(format!("zd1-update {} {baud} {} {id}\r", image.len(), crate::hex(&crate::sha256(image))).as_bytes())
        .map_err(io)?;
    let (mut buf, t0) = (Vec::new(), crate::now_ms());
    let tag = b"\x02zd1-update ready ";
    let slot = loop {
        read(c, &mut buf)?;
        if let Some(k) = buf.windows(tag.len()).position(|w| w == tag)
            && let Some(nl) = buf[k..].iter().position(|b| *b == b'\n')
        {
            let slot = match buf[k + tag.len()] {
                b'a' => Slot::A,
                b'b' => Slot::B,
                x => Slot::Unlisted(x),
            };
            buf.drain(..k + nl + 1);
            break slot;
        }
        if crate::now_ms() - t0 > 30_000.0 {
            return Err(Error::Timeout(format!(
                "no ready from zd1-update: {:?}",
                String::from_utf8_lossy(&buf[buf.len().saturating_sub(200)..])
            )));
        }
    };
    ev.step(&format!("into slot {slot}"));
    let frames = image.len().div_ceil(FRAME);
    let frame = |i: usize| {
        let d = &image[i * FRAME..((i + 1) * FRAME).min(image.len())];
        let h = [i as u8, d.len() as u8, (d.len() >> 8) as u8];
        let mut crc = crc32fast::Hasher::new();
        crc.update(&h);
        crc.update(d);
        [&[0x7E][..], &h, d, &crc.finalize().to_le_bytes()].concat()
    };
    // the module's answers: 0E SEQ (that one arrived), 15 SEQ (it wants that one next), its lines 02 TEXT \n
    let (mut acked, mut next, mut resends, t0) = (0usize, 0usize, 0u32, crate::now_ms());
    let result = loop {
        let (mut i, mut result) = (0, None);
        while i < buf.len() {
            match buf[i] {
                t @ (0x0E | 0x15) if i + 1 < buf.len() => {
                    let d = buf[i + 1].wrapping_sub(acked as u8) as usize;
                    if t == 0x0E && d < next - acked {
                        acked += d + 1;
                    } else if t == 0x15 && d <= next - acked {
                        acked += d;
                        resends += (next > acked) as u32;
                        next = acked;
                    }
                    i += 2;
                }
                0x0E | 0x15 => break,
                0x02 => match buf[i..].iter().position(|b| *b == b'\n') {
                    Some(j) => {
                        result = Some(String::from_utf8_lossy(&buf[i + 1..i + j]).into_owned());
                        i += j + 1;
                    }
                    None => break,
                },
                _ => i += 1,
            }
        }
        buf.drain(..i);
        if let Some(r) = result {
            break r;
        }
        while next < frames && next < acked + FRAMES {
            c.write_all(&frame(next)).map_err(io)?;
            next += 1;
        }
        ev.progress("update", (acked * FRAME).min(image.len()) as u64, image.len() as u64);
        read(c, &mut buf)?;
        if crate::now_ms() - t0 > 900_000.0 {
            return Err(Error::Timeout("no result from zd1-update in 900 s".into()));
        }
    };
    ev.step(&format!("module: {result} ({:.0} s, {resends} resends)", (crate::now_ms() - t0) / 1000.0));
    if !result.starts_with("zd1-update ok") {
        return Err(Error::Failed(result));
    }
    Ok(slot)
}
