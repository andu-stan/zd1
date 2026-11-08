//! Stock-MCU mode's fixed output rate (docs/stock-mcu.md): B&W's MCU app runs the module's audio path at 44.1 kHz
//! only, so there a stream at another rate (an Apple TV's 48 kHz, a Mac's AAC-LC/48000) is converted on its way into
//! the queue, and everything downstream (the queue, the timing marks, the device) runs at 44.1 kHz. Integer linear
//! interpolation, like Snapcast's (no FPU here): cheap, and its roll-off near 20 kHz is inaudible at these rates.
//! Elsewhere (our MCU firmware follows the stream's rate) nothing changes.

use super::queue::Sample;
use std::sync::OnceLock;

/// The rate the output runs at whatever the stream's, if any: 44.1 kHz in stock-MCU mode (`zd1.mcu=stock`).
pub fn fixed_rate() -> Option<u32> {
    static FIXED: OnceLock<Option<u32>> = OnceLock::new();
    *FIXED.get_or_init(|| {
        let stock = std::fs::read_to_string("/proc/cmdline")
            .is_ok_and(|c| c.split_whitespace().any(|w| w == "zd1.mcu=stock"));
        stock.then_some(44100)
    })
}

/// A stream's samples from `from` Hz to `to` Hz, push by push: the position between pushes carries over, so the
/// output is one continuous signal. Samples come out as i32 with the sample in the top bits (the queue converts).
pub struct Resampler {
    ch: usize,
    step: u64,     // source frames per output frame, Q32
    pos: u64,      // the next output frame's position, Q32 source frames after `prev`
    prev: Vec<i32>, // the last source frame of the previous push
    primed: bool,
}

impl Resampler {
    pub fn new(from: u32, to: u32, channels: u8) -> Resampler {
        let ch = channels.max(1) as usize;
        Resampler { ch, step: ((from as u64) << 32) / to.max(1) as u64, pos: 0, prev: vec![0; ch], primed: false }
    }

    /// After a flush (a pause, a seek): the next push starts afresh.
    pub fn reset(&mut self) {
        self.primed = false;
        self.pos = 0;
    }

    /// One push's samples (interleaved), converted.
    pub fn run<T: Sample>(&mut self, inp: &[T]) -> Vec<i32> {
        let (ch, n) = (self.ch, inp.len() / self.ch);
        if n == 0 {
            return Vec::new();
        }
        if !self.primed {
            for c in 0..ch {
                self.prev[c] = inp[c].s32();
            }
            self.primed = true;
        }
        // positions count from `prev` (index 0); the push's frames are 1..=n
        let frame = |i: u64, c: usize| if i == 0 { self.prev[c] } else { inp[(i as usize - 1) * ch + c].s32() };
        let mut out = Vec::with_capacity((n as u64 * (1 << 32) / self.step + 2) as usize * ch);
        while (self.pos >> 32) < n as u64 {
            let i = self.pos >> 32;
            let frac = ((self.pos >> 16) & 0xFFFF) as i64; // Q16: (b - a) * frac stays in an i64
            for c in 0..ch {
                let (a, b) = (frame(i, c) as i64, frame(i + 1, c) as i64);
                out.push((a + ((b - a) * frac >> 16)) as i32);
            }
            self.pos += self.step;
        }
        for c in 0..ch {
            self.prev[c] = inp[(n - 1) * ch + c].s32();
        }
        self.pos -= (n as u64) << 32;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_at_48k_becomes_a_second_at_44k1() {
        let mut r = Resampler::new(48000, 44100, 2);
        let mut frames = 0;
        for _ in 0..100 {
            frames += r.run(&[0i16; 480 * 2]).len() / 2; // 100 pushes of 10 ms
        }
        assert!((44099..=44101).contains(&frames), "{frames}");
    }

    #[test]
    fn a_steady_level_stays_where_it_is() {
        let mut r = Resampler::new(48000, 44100, 1);
        let out = r.run(&[1000i16; 4800]);
        assert!(out.iter().all(|&v| v == 1000 << 16));
    }

    #[test]
    fn a_ramp_stays_a_ramp_across_pushes() {
        // a linear ramp is reproduced exactly by linear interpolation, also across push boundaries
        let mut r = Resampler::new(48000, 44100, 1);
        let ramp: Vec<i16> = (0..4800).map(|i| i as i16).collect();
        let mut out = r.run(&ramp[..2400]);
        out.extend(r.run(&ramp[2400..]));
        for w in out[2..].windows(2) {
            // (the first frame comes twice: the start)
            let step = (w[1] - w[0]) >> 16;
            assert!((1..=2).contains(&step), "{step}");
        }
    }

    #[test]
    fn the_same_rate_passes_through() {
        let mut r = Resampler::new(44100, 44100, 2);
        let inp: Vec<i16> = (0..200).map(|i| (i * 7) as i16).collect();
        let out = r.run(&inp);
        // one frame of delay (the previous frame comes first), then the input
        assert_eq!(out.len(), inp.len());
        assert_eq!(out[2..].iter().map(|&v| (v >> 16) as i16).collect::<Vec<_>>(), inp[..inp.len() - 2]);
    }
}
