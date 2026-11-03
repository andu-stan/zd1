//! One stream's jobs on Warp0, in order: a fixed number of slots, each with its own input and output buffer in
//! the window; results collected oldest first. The stream users (realtime and buffered ALAC, AAC, Vorbis, FLAC) are
//! this plus their own descriptor fields.
//!
//! A job the Warp hasn't finished after [`HANG`] is a hang: diagnosed (warp.rs), counted, the core parked, and every
//! job in flight comes back `Failed::Hang`; the pipeline is off from then on, and its owner carries on in software.
//! So is it after [`Pipeline::turn_off`], for a job error that isn't a frame the stream can drop.
//! The last inputs stay available (`recent`) so the owner can save what led up to it for a replay on the host
//! (module/warp0/tools/emu.py).

use crate::abi::W0_HANG_MS;
use crate::warp::{Diagnosis, Done, Job, Warp0};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Why a job has no output.
#[derive(Clone, Debug)]
pub enum Failed {
    /// The job ran and returned this (w0abi.h W0_E*), with D_CTR (a decoder's own code)
    Result { result: u32, ctr: u32 },
    /// The Warp stopped answering (this job or one before it); the pipeline is dead
    Hang,
}

/// A job's output as 16-bit samples (`collect`)
pub type Samples<'a> = Result<&'a [i16], Failed>;
/// A job's output as the bytes it wrote, with its `Done` (`collect_with`, `drain`)
pub type Written<'a> = Result<(&'a [u8], Done), Failed>;

struct Pending {
    tag: u64,
    slot: u32,
    buf: usize,
    at: Instant,
}

pub struct Pipeline {
    warp: Warp0,
    what: &'static str,
    input: u32,
    output: u32,
    in_size: u32,
    out_size: u32,
    slots: usize,
    hang: Duration,
    jobs: VecDeque<Pending>,
    next_buf: usize,
    out: Vec<u32>, // one job's output (words: aligned for 16- and 32-bit samples)
    recent: VecDeque<Vec<u8>>,
    keep: usize,
    dead: Option<Diagnosis>,
    off: bool,
}

/// How long a stream job may take before it is a hang (w0abi.h W0_HANG_MS).
pub const HANG: Duration = Duration::from_millis(W0_HANG_MS as u64);

/// [`HANG`], or ZD1_WARP0_HANG_MS (tests: a few ms makes a healthy Warp0 "hang", to exercise the diagnosis, the log
/// and the hand-over to the ARM without a real one).
fn hang_timeout() -> Duration {
    std::env::var("ZD1_WARP0_HANG_MS").ok().and_then(|v| v.parse().ok()).map_or(HANG, Duration::from_millis)
}

impl Pipeline {
    /// `slots` jobs in flight at most, each with `in_size` bytes of input and `out_size` of output; `what` names
    /// the stream in logs.
    pub fn new(mut warp: Warp0, what: &'static str, slots: usize, in_size: u32, out_size: u32) -> Result<Self, String> {
        let input = warp.alloc(in_size * slots as u32).ok_or("window full")?;
        let output = warp.alloc(out_size * slots as u32).ok_or("window full")?;
        let hang = hang_timeout();
        Ok(Pipeline {
            warp,
            what,
            input,
            output,
            in_size,
            out_size,
            slots,
            hang,
            jobs: VecDeque::with_capacity(slots),
            next_buf: 0,
            out: vec![0; out_size.div_ceil(4) as usize],
            recent: VecDeque::new(),
            keep: 0,
            dead: None,
            off: false,
        })
    }

    /// Keep the last `n` inputs (for a hang's replay file).
    pub fn keep_recent(&mut self, n: usize) {
        self.keep = n;
    }

    /// The last inputs submitted, oldest first.
    pub fn recent(&self) -> impl Iterator<Item = &[u8]> {
        self.recent.iter().map(|v| v.as_slice())
    }

    pub fn warp(&mut self) -> &mut Warp0 {
        &mut self.warp
    }

    pub fn in_flight(&self) -> usize {
        self.jobs.len()
    }

    pub fn full(&self) -> bool {
        self.jobs.len() >= self.slots
    }

    /// The hang that killed the pipeline, if one did.
    pub fn dead(&self) -> Option<&Diagnosis> {
        self.dead.as_ref()
    }

    /// Off Warp0 for the rest of the stream (a hang, or [`turn_off`](Self::turn_off)): `submit` refuses.
    pub fn is_off(&self) -> bool {
        self.off
    }

    /// Off Warp0 for the rest of the stream, said once: its owner decodes on the ARM from here on.
    pub fn turn_off(&mut self, why: &str) {
        if !self.off {
            tracing::warn!(why, "Warp0 {} off for this stream: decoding on the ARM", self.what);
            self.off = true;
        }
    }

    /// The next free buffer pair's index (stable until the submit that uses it).
    pub fn next_buf(&self) -> usize {
        self.next_buf
    }

    /// Submit `input` (into the next buffer; src, len and dst filled in) with the rest of `job`, which `fill` may
    /// adjust knowing the buffer index. false: full, off, too big, or the ring is full.
    pub fn submit(&mut self, tag: u64, input: &[u8], mut job: Job, fill: impl FnOnce(&mut Job, usize)) -> bool {
        if self.off || self.full() || input.len() > self.in_size as usize {
            return false;
        }
        let b = self.next_buf;
        job.src = self.input + self.in_size * b as u32;
        job.dst = self.output + self.out_size * b as u32;
        job.len = input.len() as u32;
        fill(&mut job, b);
        self.warp.put(job.src, input);
        let Some(slot) = self.warp.submit(&job) else {
            return false;
        };
        if self.keep > 0 {
            if self.recent.len() >= self.keep {
                self.recent.pop_front();
            }
            self.recent.push_back(input.to_vec());
        }
        self.jobs.push_back(Pending { tag, slot, buf: b, at: Instant::now() });
        self.next_buf = (b + 1) % self.slots;
        true
    }

    /// The finished jobs, oldest first, until one isn't: `done(tag, Ok(samples) | Err(why))`, the output as 16-bit
    /// samples (the ALAC, AAC and Vorbis users). Returns false if the pipeline is (now) dead.
    pub fn collect(&mut self, done: &mut dyn FnMut(u64, Samples<'_>)) -> bool {
        self.collect_with(&mut |tag, r| match r {
            Ok((bytes, _)) if bytes.len() % 2 == 0 => {
                let s = unsafe { std::slice::from_raw_parts(bytes.as_ptr() as *const i16, bytes.len() / 2) };
                done(tag, Ok(s))
            }
            Ok((_, d)) => done(tag, Err(Failed::Result { result: d.result, ctr: d.ctr })),
            Err(e) => done(tag, Err(e)),
        })
    }

    /// As `collect`, the output as the bytes the job wrote (4-byte aligned) with its `Done` (D_TAG, D_CTR).
    pub fn collect_with(&mut self, done: &mut dyn FnMut(u64, Written<'_>)) -> bool {
        while let Some(&Pending { tag, slot, buf, at }) = self.jobs.front() {
            let Some(Done { result, outlen, ctr, tag: jtag }) = self.warp.poll(slot) else {
                if at.elapsed() > self.hang {
                    let d = self.warp.hang(&format!(
                        "{}: a job took over {} ms ({} in flight)",
                        self.what,
                        self.hang.as_millis(),
                        self.jobs.len()
                    ));
                    self.turn_off(&format!("hang: {}", d.text));
                    self.dead = Some(d);
                    while let Some(j) = self.jobs.pop_front() {
                        done(j.tag, Err(Failed::Hang));
                    }
                    return false;
                }
                return true;
            };
            self.jobs.pop_front();
            let d = Done { result, outlen, ctr, tag: jtag };
            if result == 0 && outlen <= self.out_size {
                let bytes =
                    unsafe { std::slice::from_raw_parts_mut(self.out.as_mut_ptr() as *mut u8, outlen as usize) };
                self.warp.get(self.output + self.out_size * buf as u32, bytes);
                done(tag, Ok((bytes, d)));
            } else {
                done(tag, Err(Failed::Result { result, ctr }));
            }
        }
        self.dead.is_none()
    }

    /// Wait (sleeping 1 ms at a time) until nothing is in flight; everything still out is given to `done`, as
    /// `collect_with` gives it.
    pub fn drain(&mut self, done: &mut dyn FnMut(u64, Written<'_>)) {
        while !self.jobs.is_empty() {
            if !self.collect_with(done) {
                return;
            }
            if !self.jobs.is_empty() {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
}
