//! Buffered AirPlay's ALAC frames on Warp0 (the crypto image's `W0_OP_RTP_AUDIO`, plain: the frames come
//! decrypted), 16- and 24-bit (an Apple TV's lossless ALAC/48000/S24), sent ahead of playout. A frame not done in
//! time (a hang), or a job error, turns the queue off: the rest of the stream goes to shairplay's own decoder on the
//! ARM ([`crate::OnWarp`]).

use crate::Jobs;
use crate::abi::*;
use crate::queue::{Failed, Pipeline};
use crate::warp::{Image, Job, Warp0};
use shairplay::decoder::{AlacParams, Decoded};

const SLOTS: usize = 8;

/// A stream's ALAC parameters as the firmware takes them (fw/alac.h `struct alac_config`), if it decodes the stream
/// at the given bit depths.
pub(crate) struct Config([u32; 6]);

impl Config {
    pub(crate) fn new(p: &AlacParams, depths: &[u8]) -> Result<Self, String> {
        if !depths.contains(&p.bit_depth)
            || !(1..=2).contains(&p.channels)
            || p.frame_length == 0
            || p.frame_length > W0_ALAC_MAX_FRAME
        {
            return Err(format!("ALAC {p:?} is not decoded by the firmware"));
        }
        let c = [p.frame_length, p.bit_depth as u32, p.pb as u32, p.mb as u32, p.kb as u32, p.channels as u32];
        Ok(Config(c))
    }

    /// Into the window, for D_CONF.
    pub(crate) fn put(&self, warp: &mut Warp0) -> Result<u32, String> {
        let at = warp.alloc(64).ok_or("window full")?;
        let bytes: Vec<u8> = self.0.iter().flat_map(|w| w.to_le_bytes()).collect();
        warp.put(at, &bytes);
        Ok(at)
    }
}

/// One buffered ALAC stream on Warp0.
pub struct AlacWarp {
    jobs: Pipeline,
    conf: u32,
    wide: bool,
}

impl AlacWarp {
    pub fn new(p: &AlacParams) -> Result<Self, String> {
        let config = Config::new(p, &[16, 24])?;
        let mut warp = Warp0::open(Image::Crypto)?;
        let conf = config.put(&mut warp)?;
        let jobs = Pipeline::new(warp, "buffered ALAC", SLOTS, W0_RTP_MAX, W0_RTP_OUT)?;
        Ok(AlacWarp { jobs, conf, wide: p.bit_depth == 24 })
    }
}

impl Jobs for AlacWarp {
    fn submit(&mut self, frame: &[u8]) -> bool {
        if frame.is_empty() || frame.len() > W0_RTP_MAX as usize {
            return false;
        }
        let job = Job { op: W0_OP_RTP_AUDIO, ctr: W0_RTP_PLAIN | W0_RTP_ALAC, conf: self.conf, ..Job::default() };
        self.jobs.submit(0, frame, job, |_, _| {})
    }

    fn collect(&mut self, done: &mut dyn FnMut(Decoded)) {
        let (wide, mut error) = (self.wide, None);
        self.jobs.collect_with(&mut |_, r| match r {
            // (the pipeline's buffer is words: aligned for either width)
            Ok((b, _)) if wide && !b.is_empty() && b.len() % 4 == 0 => {
                let s = unsafe { std::slice::from_raw_parts(b.as_ptr() as *const i32, b.len() / 4) };
                done(Decoded::S24(s.to_vec()))
            }
            Ok((b, _)) if !wide && !b.is_empty() && b.len() % 2 == 0 => {
                let s = unsafe { std::slice::from_raw_parts(b.as_ptr() as *const i16, b.len() / 2) };
                done(Decoded::S16(s.to_vec()))
            }
            Ok(_) => done(Decoded::Failed), // a frame that doesn't decode: dropped, as on the ARM
            Err(Failed::Result { result, .. }) => {
                done(Decoded::Failed);
                error.get_or_insert(result);
            }
            Err(Failed::Hang) => done(Decoded::Failed),
        });
        if let Some(result) = error {
            self.jobs.turn_off(&format!("job result {result}"));
        }
    }

    fn live(&self) -> bool {
        !self.jobs.is_off()
    }

    fn full(&self) -> bool {
        self.jobs.full()
    }
}
