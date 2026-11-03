//! A module image through BL2's XModem receiver over SPI, driven by our MCU (`XmFrame`): the way to install one with
//! our MCU firmware running, without the bootloader's relay.

use crate::spec::ModOp;
use crate::usb::Usb;
use crate::{Error, Mcu, Result, msg};

fn crc16_xmodem(d: &[u8]) -> u16 {
    let mut c = 0u16;
    for b in d {
        c ^= (*b as u16) << 8;
        for _ in 0..8 {
            c = if c & 0x8000 != 0 { c << 1 ^ 0x1021 } else { c << 1 };
        }
    }
    c
}

/// BL2's answer to a frame: the MCU waits for it up to `XM_WAIT_MS` itself (its watchdog allows no longer), then it
/// is asked for again (an empty `XmFrame`) until `XM_ANSWER_MS` have passed since `sent` (when the frame went out).
/// `wait_us` then counts from there.
const XM_WAIT_MS: u16 = 500;
const XM_ANSWER_MS: f64 = 10_000.0;
async fn xm_answer<U: Usb>(m: &mut Mcu<U>, mut x: msg::XmReply, sent: f64) -> Result<msg::XmReply> {
    if !x.answers.is_empty() {
        return Ok(x);
    }
    while x.answers.is_empty() && crate::now_ms() - sent < XM_ANSWER_MS {
        m.usb.sleep(20).await;
        x.answers = m.call(&msg::XmFrame::default()).await?.answers;
    }
    if !x.answers.is_empty() {
        x.wait_us = ((crate::now_ms() - sent) * 1000.0) as u32;
        x.read_us = 0;
    }
    Ok(x)
}

/// Install a module image through BL2's XModem over SPI (registers 0x2D / 0x2C), fed through `XmFrame`: each
/// 1029-byte frame goes to the MCU in pieces, then to BL2 in one write; the MCU times the write, BL2's answer and the
/// read-out. `Module BL2` first unless `in_bl2`. BL2 erases the app partition as the download starts: stopping
/// midway leaves it invalid. `limit`: a rehearsal of that many blocks, no EOT. Lines for a person.
pub async fn xm_install<U: Usb>(
    m: &mut Mcu<U>,
    data: &[u8],
    in_bl2: bool,
    limit: Option<usize>,
) -> Result<Vec<String>> {
    let ev = m.events().clone();
    let total = data.len().div_ceil(1024);
    let n = limit.map_or(total, |l| l.min(total));
    if !in_bl2 {
        let r = m.call(&msg::Module { op: ModOp::Bl2, timeout_s: 60 }).await?;
        if !r.ok {
            return Err(Error::Failed(format!("the module did not reach BL2 (status 0x{:04x})", r.status)));
        }
    }
    // HCR 0x2220: BL2 starts its XModem download
    m.call(&msg::ModWrite { reg: 0x28, v: false, data: vec![0x22, 0x20] }).await?;
    ev.step("waiting for BL2's XModem start (C on 0x2C)");
    let t0 = crate::now_ms();
    loop {
        let a = m.call(&msg::XmFrame::default()).await?;
        if a.answers.contains(&b'C') {
            break;
        }
        if crate::now_ms() - t0 > 90_000.0 {
            return Err(Error::Timeout("no C from BL2 within 90 s: is it waiting for an XModem download?".into()));
        }
        m.usb.sleep(200).await;
    }
    ev.step(&format!("BL2 asks (C) after {:.1} s; {n} blocks", (crate::now_ms() - t0) / 1000.0));
    const PIECE: usize = 240;
    let (mut stats, mut retries, ts) = (Vec::new(), 0, crate::now_ms());
    for (i, blk) in data.chunks(1024).take(n).enumerate() {
        let k = (i + 1) as u8;
        let mut b = blk.to_vec();
        b.resize(1024, 0x1A);
        let frame = [&[0x02, k, 0xFF - k][..], &b, &crc16_xmodem(&b).to_be_bytes()].concat();
        let th = crate::now_ms();
        let last = (frame.len() - 1) / PIECE * PIECE;
        for off in (0..last).step_by(PIECE) {
            m.call(&msg::XmFrame { offset: off as u16, data: frame[off..off + PIECE].to_vec(), ..Default::default() })
                .await?;
        }
        let host = crate::now_ms() - th;
        let mut r = None;
        for attempt in 0..10 {
            let sent = crate::now_ms();
            let x = if attempt == 0 {
                // the last piece, then the whole frame goes out
                m.call(&msg::XmFrame {
                    offset: last as u16,
                    data: frame[last..].to_vec(),
                    send: true,
                    wait_ms: XM_WAIT_MS,
                })
                .await?
            } else {
                // the buffer still holds the frame: send it again
                m.call(&msg::XmFrame { offset: frame.len() as u16, data: vec![], send: true, wait_ms: XM_WAIT_MS })
                    .await?
            };
            let x = xm_answer(m, x, sent).await?;
            match x.answers.last() {
                Some(0x06) => {
                    r = Some(x);
                    break;
                }
                Some(0x18) => {
                    return Err(Error::Failed(format!("block {}: BL2 cancelled ({})", i + 1, crate::hex(&x.answers))));
                }
                _ => {
                    retries += 1;
                    ev.step(&format!("block {}: answer {}, sending again", i + 1, crate::hex(&x.answers)));
                }
            }
        }
        let r = r.ok_or_else(|| Error::Failed(format!("block {}: no ACK after 10 tries", i + 1)))?;
        stats.push([r.wire_us as f64 / 1000.0, r.wait_us as f64 / 1000.0, r.read_us as f64 / 1000.0, host]);
        ev.progress("xmodem", i as u64 + 1, n as u64);
    }
    let el = (crate::now_ms() - ts) / 1000.0;
    let mut out = Vec::new();
    let eot = if limit.is_some() {
        out.push(format!("rehearsal: {n} of {total} blocks, no EOT (the app partition is left invalid)"));
        None
    } else {
        let sent = crate::now_ms();
        let x = m.call(&msg::XmFrame { offset: 0, data: vec![0x04], send: true, wait_ms: XM_WAIT_MS }).await?;
        Some(xm_answer(m, x, sent).await?)
    };
    let eot_ans = eot.map(|r| r.answers);
    out.push(format!(
        "{n} blocks in {el:.1} s: {:.1} KB/s, {retries} retries; EOT answer {}; the whole image at this rate: {:.0} s",
        n as f64 / el,
        eot_ans.as_ref().map_or("-".into(), |a| if a.is_empty() { "none".into() } else { crate::hex(a) }),
        total as f64 * el / n as f64
    ));
    for (name, k) in [
        ("SPI write of a frame", 0),
        ("BL2 until it answers", 1),
        ("reading the answer (ours)", 2),
        ("host link, the frame to the MCU", 3),
    ] {
        let mut v: Vec<f64> = stats.iter().map(|s| s[k]).collect();
        v.sort_by(|a, b| a.total_cmp(b));
        if let (Some(med), Some(max)) = (v.get(v.len() / 2), v.last()) {
            out.push(format!("  {name:<32} median {med:6.1} ms, 95% {:6.1}, max {max:6.1}", v[v.len() * 95 / 100]));
        }
    }
    if eot_ans.is_some_and(|a| a.last() != Some(&0x06)) {
        return Err(Error::Failed(out.join("\n")));
    }
    Ok(out)
}
