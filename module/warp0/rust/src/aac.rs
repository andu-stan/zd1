//! AAC on Warp0 (the AAC image): access units sent ahead of playout; after a hang or a job error the queue is off
//! and the rest goes to the ARM's copy of the decoder (module/codecs/aac, [`crate::OnWarp`]).

use crate::Jobs;
use crate::abi::*;
use crate::queue::{Failed, HANG, Pipeline};
use crate::warp::{Image, Job, Warp0};
use shairplay::decoder::Decoded;

const SLOTS: usize = 8;

/// One AAC stream decoded on Warp0, frames submitted ahead of their time by the playout.
pub struct AacWarp {
    jobs: Pipeline,
}

impl AacWarp {
    pub fn new(sample_rate: u32, channels: u8) -> Result<Self, String> {
        let mut warp = Warp0::open(Image::Aac)?;
        let init = Job { op: W0_OP_AAC_INIT, len: sample_rate, ctr: channels as u32, ..Job::default() };
        let r = warp.run(&init, HANG)?;
        if r.result != W0_OK {
            return Err(format!("AAC_INIT: result {}", r.result));
        }
        let jobs = Pipeline::new(warp, "AAC", SLOTS, W0_AAC_MAX_AU, W0_AAC_OUT)?;
        Ok(AacWarp { jobs })
    }
}

impl Jobs for AacWarp {
    fn submit(&mut self, au: &[u8]) -> bool {
        if au.is_empty() {
            return false;
        }
        self.jobs.submit(0, au, Job { op: W0_OP_AAC_DECODE, ..Job::default() }, |_, _| {})
    }

    fn collect(&mut self, done: &mut dyn FnMut(Decoded)) {
        let mut error = None;
        self.jobs.collect(&mut |_, r| match r {
            Ok(pcm) => done(Decoded::S16(pcm.to_vec())),
            Err(Failed::Result { result, .. }) => {
                done(Decoded::Failed); // a frame that doesn't decode: dropped, as on the ARM
                if result != W0_EDECODE {
                    error.get_or_insert(result);
                }
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
