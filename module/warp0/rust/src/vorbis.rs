//! Vorbis on Warp0 (the Vorbis image), for zd1-spotify: packets submitted ahead (`submit`), results taken in order
//! (`poll`). The ARM's copy of the decoder (module/codecs/vorbis) is the fallback: for a stream Warp0 doesn't take, and for
//! the rest of a stream after a hang or a job error. The hand-over is seamless: the ARM decoder is primed with the
//! packet before the first one Warp0 didn't finish, then decodes those again, so no packet's samples are lost.
//!
//! On a hang the stream's headers and its last packets go to `REPLAY` (module/warp0/tools/emu.py vorbis FILE
//! replays them on the host, where an access that would hang the bus is caught with its PC).

use crate::abi::*;
use crate::queue::{Failed, HANG, Pipeline};
use crate::warp::{Image, Job, Warp0};
use std::collections::VecDeque;
use std::time::Duration;
use zd1_vorbis::{Decoder, Ident};

const SLOTS: usize = 8;
/// Where a hang's replay goes (headers + the last packets, vorbischeck's packets format).
pub const REPLAY: &str = "/tmp/zd1-warp0-hang.pkts";

/// A packet's result: interleaved samples (empty for the first packet), or the decoder's code for one it dropped.
pub type Decoded = Result<Vec<i16>, i32>;

pub struct VorbisStream {
    warp: Option<Pipeline>,
    arm: Decoder,
    ident: Vec<u8>,
    setup: Vec<u8>,
    tags: VecDeque<u64>, // tags in flight on Warp0, in order (their inputs: the pipeline's recent())
    ready: VecDeque<(u64, Decoded)>, // finished, in order
}

impl VorbisStream {
    /// A stream from its identification and setup headers; on Warp0 if `warp` and it is free and takes the stream,
    /// else on the ARM. Err only if the ARM can't decode it either.
    pub fn new(ident: &[u8], setup: &[u8], warp: bool) -> Result<Self, String> {
        let arm = Decoder::new(ident, setup).map_err(|e| zd1_vorbis::error_name(e).to_string())?;
        let mut s = VorbisStream {
            warp: None,
            arm,
            ident: ident.to_vec(),
            setup: setup.to_vec(),
            tags: VecDeque::new(),
            ready: VecDeque::new(),
        };
        if warp {
            match s.boot() {
                Ok(p) => {
                    tracing::info!("Warp0 decodes this Vorbis stream");
                    s.warp = Some(p);
                }
                Err(e) => tracing::info!(%e, "Warp0 not used for this Vorbis stream"),
            }
        }
        Ok(s)
    }

    fn boot(&self) -> Result<Pipeline, String> {
        // the ARM's decoder took the stream (new), and the firmware's is the same C with the same limits
        if self.ident.len() > 256 || self.setup.len() > W0_VORBIS_MAX_HEADER as usize {
            return Err("headers too long".into());
        }
        let mut warp = Warp0::open(Image::Vorbis)?;
        let (bi, bs) = (warp.alloc(256).ok_or("window full")?, warp.alloc(W0_VORBIS_MAX_HEADER).ok_or("window full")?);
        warp.put(bi, &self.ident);
        warp.put(bs, &self.setup);
        let t = std::time::Instant::now();
        let r = warp.run(
            &Job {
                op: W0_OP_VORBIS_INIT,
                src: bi,
                len: self.ident.len() as u32,
                aad: bs,
                aadlen: self.setup.len() as u32,
                ..Job::default()
            },
            Duration::from_secs(2),
        )?;
        if r.result != W0_OK {
            return Err(format!("VORBIS_INIT: result {}, {}", r.result, zd1_vorbis::error_name(r.ctr as i32)));
        }
        tracing::debug!(ms = t.elapsed().as_millis() as u64, arena = r.outlen, "Warp0 Vorbis setup");
        let mut p = Pipeline::new(warp, "Vorbis", SLOTS, W0_VORBIS_MAX_PACKET, W0_VORBIS_OUT)?;
        p.keep_recent(SLOTS + 1);
        Ok(p)
    }

    pub fn ident(&self) -> &Ident {
        self.arm.ident()
    }

    pub fn on_warp(&self) -> bool {
        self.warp.is_some()
    }

    pub fn in_flight(&self) -> usize {
        self.tags.len()
    }

    /// Room for another packet? (Always on the ARM: it decodes at once.)
    pub fn can_submit(&self) -> bool {
        self.warp.as_ref().is_none_or(|p| !p.full())
    }

    /// A packet, tagged; its result comes out of `poll` in order. false: Warp0 is full (poll, then try again).
    pub fn submit(&mut self, tag: u64, pkt: &[u8]) -> bool {
        if let Some(p) = &mut self.warp {
            if p.full() {
                return false;
            }
            if pkt.len() <= W0_VORBIS_MAX_PACKET as usize
                && p.submit(tag, pkt, Job { op: W0_OP_VORBIS_DECODE, ..Job::default() }, |_, _| {})
            {
                self.tags.push_back(tag);
                return true;
            }
            // too long for the firmware's buffers (no real stream has such packets): the ARM from here on
            self.hand_over(&format!("a {}-byte packet", pkt.len()), &[]);
        }
        let r = self.arm_decode(pkt);
        self.ready.push_back((tag, r));
        true
    }

    fn arm_decode(&mut self, pkt: &[u8]) -> Decoded {
        let mut out = Vec::new();
        self.arm.decode(pkt, &mut out).map(|_| out)
    }

    /// The oldest finished packet, if it is done.
    pub fn poll(&mut self) -> Option<(u64, Decoded)> {
        if self.ready.is_empty() {
            self.collect();
        }
        self.ready.pop_front()
    }

    fn collect(&mut self) {
        let Some(p) = &mut self.warp else { return };
        let mut failed: Vec<u64> = Vec::new();
        let mut error = None;
        let (tags, ready) = (&mut self.tags, &mut self.ready);
        p.collect(&mut |tag, r| {
            let front = tags.pop_front();
            debug_assert_eq!(front, Some(tag));
            match r {
                Ok(pcm) if failed.is_empty() => ready.push_back((tag, Ok(pcm.to_vec()))),
                Err(Failed::Result { result, ctr }) if result == W0_EDECODE && failed.is_empty() => {
                    ready.push_back((tag, Err(ctr as i32))) // a packet that doesn't decode: dropped, as on the ARM
                }
                Err(Failed::Result { result, .. }) => {
                    error.get_or_insert(format!("job result {result}"));
                    failed.push(tag);
                }
                Err(Failed::Hang) | Ok(_) => failed.push(tag),
            }
        });
        let why = match (p.dead(), error) {
            (Some(d), _) => format!("hang: {}", d.text),
            (None, Some(e)) => e,
            (None, None) => return,
        };
        if p.dead().is_some() {
            self.save_replay();
        }
        self.hand_over(&why, &failed);
    }

    /// Warp0 is out for the rest of the stream: the ARM decodes the packets it didn't finish (`failed`, and those
    /// still in flight: the last ones submitted) after one before them to prime its overlap, then everything after.
    fn hand_over(&mut self, why: &str, failed: &[u64]) {
        let Some(mut p) = self.warp.take() else { return };
        let mut failed = failed.to_vec();
        p.turn_off(why); // (a hang said so already)
        p.drain(&mut |tag, _| failed.push(tag));
        tracing::info!(redo = failed.len(), "the ARM decodes again the packets Warp0 didn't finish");
        let recent: Vec<Vec<u8>> = p.recent().map(|v| v.to_vec()).collect();
        drop(p);
        let k = failed.len().min(recent.len());
        self.arm.restart();
        if recent.len() > k {
            let _ = self.arm_decode(&recent[recent.len() - k - 1]); // primes the overlap; its samples were out
        }
        for (i, &tag) in failed.iter().enumerate().take(k) {
            let r = self.arm_decode(&recent[recent.len() - k + i]);
            self.ready.push_back((tag, r));
        }
        self.tags.clear();
    }

    fn save_replay(&self) {
        let Some(p) = &self.warp else { return };
        let mut f = Vec::new();
        for pkt in [&self.ident[..], &[3u8, b'v', b'o', b'r', b'b', b'i', b's'][..], &self.setup[..]]
            .into_iter()
            .chain(p.recent())
        {
            f.extend_from_slice(&(pkt.len() as u32).to_le_bytes());
            f.extend_from_slice(pkt);
        }
        match std::fs::write(REPLAY, &f) {
            Ok(()) => tracing::error!(
                path = REPLAY,
                "the stream's headers and last packets, for a replay on the host \
                                                     (module/warp0/tools/emu.py vorbis)"
            ),
            Err(e) => tracing::warn!(%e, "replay file"),
        }
    }

    /// A seek: everything in flight and finished is dropped, the overlap forgotten (the next packet primes it).
    pub fn restart(&mut self) {
        self.ready.clear();
        self.arm.restart();
        if let Some(p) = &mut self.warp {
            p.drain(&mut |_, _| {});
            self.tags.clear();
            if p.dead().is_some() {
                self.hand_over("hang while draining", &[]);
                return;
            }
            let r = p.warp().run(&Job { op: W0_OP_VORBIS_RESTART, ..Job::default() }, HANG);
            if let Err(e) = r {
                self.hand_over(&format!("restart: {e}"), &[]);
            }
        }
    }
}
