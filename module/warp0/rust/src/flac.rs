//! FLAC on Warp0 (the FLAC image), for zd1-snapcast: chunks of whole frames submitted ahead (`submit`), results
//! taken in order (`poll`). FLAC keeps no state from frame to frame, so the fallback needs no hand-over: a chunk
//! Warp0 doesn't decode (it is taken, refuses the stream or the chunk, or hangs) comes back as `Output::Arm` with the
//! chunk, for the caller's own decoder, in its place in the order. After a hang Warp0 is off for the rest of the
//! stream (the hang itself is diagnosed, counted and logged by `queue`).

use crate::abi::*;
use crate::queue::{Failed, HANG, Pipeline};
use crate::warp::{Image, Job, Warp0};
use std::collections::VecDeque;

const SLOTS: usize = 4;

/// A chunk's result, in order.
pub enum Output {
    /// Interleaved samples, each left-justified in 32 bits.
    Pcm(Vec<i32>),
    /// Not decoded on Warp0: the chunk, for the ARM.
    Arm(Vec<u8>),
}

struct Entry {
    tag: u64,
    chunk: Vec<u8>,      // kept until its result is in (an Arm result needs it)
    out: Option<Output>, // None while on Warp0
}

pub struct FlacStream {
    warp: Option<Pipeline>,
    order: VecDeque<Entry>,
}

impl FlacStream {
    /// A stream from its STREAMINFO body (34 bytes); on Warp0 if `warp` and it is free and takes the stream, else
    /// every chunk comes back for the ARM.
    pub fn new(streaminfo: &[u8], warp: bool) -> Self {
        let mut s = FlacStream { warp: None, order: VecDeque::new() };
        if warp {
            match Self::boot(streaminfo) {
                Ok(p) => {
                    tracing::debug!("Warp0 decodes this FLAC stream");
                    s.warp = Some(p);
                }
                Err(e) => tracing::info!(%e, "Warp0 not used for this FLAC stream"),
            }
        }
        s
    }

    fn boot(streaminfo: &[u8]) -> Result<Pipeline, String> {
        if streaminfo.len() != 34 {
            return Err("STREAMINFO".into());
        }
        let mut warp = Warp0::open(Image::Flac)?;
        let b = warp.alloc(64).ok_or("window full")?;
        warp.put(b, streaminfo);
        let r = warp.run(&Job { op: W0_OP_FLAC_INIT, src: b, len: 34, ..Job::default() }, HANG)?;
        if r.result != W0_OK {
            return Err(format!("FLAC_INIT: result {}, decoder {}", r.result, r.ctr as i32));
        }
        Pipeline::new(warp, "FLAC", SLOTS, W0_FLAC_MAX_IN, W0_FLAC_OUT)
    }

    pub fn on_warp(&self) -> bool {
        self.warp.is_some()
    }

    /// Chunks whose results aren't out yet.
    pub fn in_flight(&self) -> usize {
        self.order.iter().filter(|e| e.out.is_none()).count()
    }

    /// A chunk, tagged; its result comes out of `poll` in order. Waits (yielding) for Warp0's oldest job if all its
    /// slots are busy: a job takes a few ms.
    pub fn submit(&mut self, tag: u64, chunk: Vec<u8>) {
        while self.warp.as_ref().is_some_and(|p| p.full()) {
            self.collect();
            std::thread::yield_now();
        }
        let on_warp = match &mut self.warp {
            Some(p) if chunk.len() <= W0_FLAC_MAX_IN as usize => {
                p.submit(tag, &chunk, Job { op: W0_OP_FLAC_DECODE, ..Job::default() }, |_, _| {})
            }
            _ => false, // no Warp0, or a chunk longer than its buffers (no server sends one): the ARM
        };
        self.order.push_back(if on_warp {
            Entry { tag, chunk, out: None }
        } else {
            Entry { tag, chunk: Vec::new(), out: Some(Output::Arm(chunk)) }
        });
    }

    /// The oldest chunk's result, if it is in.
    pub fn poll(&mut self) -> Option<(u64, Output)> {
        if self.order.front().is_some_and(|e| e.out.is_none()) {
            self.collect();
        }
        self.order.front()?.out.as_ref()?;
        let e = self.order.pop_front()?;
        Some((e.tag, e.out?))
    }

    fn collect(&mut self) {
        let Some(p) = &mut self.warp else { return };
        let order = &mut self.order;
        let mut next = 0; // results come in submission order: the first entries still out
        let alive = p.collect_with(&mut |tag, r| {
            while order[next].out.is_some() {
                next += 1;
            }
            let e = &mut order[next];
            debug_assert_eq!(e.tag, tag);
            e.out = Some(match r {
                Ok((bytes, d)) if d.tag as usize == e.chunk.len() && bytes.len() % 4 == 0 => {
                    Output::Pcm(bytes.as_chunks::<4>().0.iter().map(|b| i32::from_le_bytes(*b)).collect())
                }
                // not all of it (the output full: no real chunk is that long), a frame that doesn't decode, a hang
                Ok(_) | Err(Failed::Result { .. } | Failed::Hang) => Output::Arm(std::mem::take(&mut e.chunk)),
            });
            e.chunk = Vec::new();
        });
        if !alive {
            self.warp = None; // (the pipeline said why)
            for e in self.order.iter_mut().filter(|e| e.out.is_none()) {
                e.out = Some(Output::Arm(std::mem::take(&mut e.chunk)));
            }
        }
    }

    /// Wait for every chunk in flight (yielding), e.g. before letting Warp0 go.
    pub fn finish(&mut self) {
        while self.in_flight() > 0 {
            self.collect();
            if self.in_flight() > 0 {
                std::thread::yield_now();
            }
        }
    }
}
