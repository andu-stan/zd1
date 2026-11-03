//! The output's queue: interleaved samples, `channels` per frame, as the stream delivers them: 16-bit, or 32-bit
//! with the sample in the top bits for a deeper source (24-bit ALAC), so a 16-bit stream takes no more memory than
//! before. Samples of the other width are converted on the way in (a 16-bit sample into bits 31:16; a 32-bit one
//! rounded to 16 bits).

use std::collections::VecDeque;

/// A queue sample: i16, or i32 with the sample in the top bits.
pub trait Sample: Copy + Default {
    /// The sample in bits 31:.. of an i32 (what ALSA's S32 takes).
    fn s32(self) -> i32;
    /// The sample scaled by `num / den` (fades).
    fn scale(self, num: i64, den: i64) -> Self;
    /// Appended to `q`, converted if its width is the other one.
    fn extend(q: &mut Queue, samples: &[Self]);
    /// For ZD1_PCM_HASH (FNV-1a over the samples' bits) and ZD1_PCM_DUMP (little-endian bytes).
    fn bits(self) -> u64;
    fn le(self, out: &mut Vec<u8>);
}

impl Sample for i16 {
    fn s32(self) -> i32 {
        (self as i32) << 16
    }
    fn scale(self, num: i64, den: i64) -> Self {
        (self as i64 * num / den) as i16
    }
    fn extend(q: &mut Queue, samples: &[Self]) {
        match q {
            Queue::S16(d) => d.extend(samples),
            Queue::S32(d) => d.extend(samples.iter().map(|v| v.s32())),
        }
    }
    fn bits(self) -> u64 {
        self as u16 as u64
    }
    fn le(self, out: &mut Vec<u8>) {
        out.extend(self.to_le_bytes());
    }
}

impl Sample for i32 {
    fn s32(self) -> i32 {
        self
    }
    fn scale(self, num: i64, den: i64) -> Self {
        (self as i64 * num / den) as i32
    }
    fn extend(q: &mut Queue, samples: &[Self]) {
        match q {
            Queue::S32(d) => d.extend(samples),
            Queue::S16(d) => d.extend(samples.iter().map(|&v| ((v as i64 + 0x8000) >> 16).min(i16::MAX as i64) as i16)),
        }
    }
    fn bits(self) -> u64 {
        self as u32 as u64
    }
    fn le(self, out: &mut Vec<u8>) {
        out.extend(self.to_le_bytes());
    }
}

pub enum Queue {
    S16(VecDeque<i16>),
    S32(VecDeque<i32>),
}

/// `$body` with `$q` the queue's deque, whichever its width.
macro_rules! each {
    ($queue:expr, $q:ident => $body:expr) => {
        match $queue {
            Queue::S16($q) => $body,
            Queue::S32($q) => $body,
        }
    };
}

impl Queue {
    /// An empty queue for a stream of `bits` (16, or 32 for a deeper source).
    pub fn new(bits: u8) -> Self {
        if bits > 16 { Queue::S32(VecDeque::new()) } else { Queue::S16(VecDeque::new()) }
    }

    /// Samples queued (not frames).
    pub fn len(&self) -> usize {
        each!(self, q => q.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&mut self) {
        each!(self, q => q.clear())
    }

    /// `n` samples off the front.
    pub fn drain(&mut self, n: usize) {
        each!(self, q => { q.drain(..n.min(q.len())); })
    }

    /// `n` samples of silence at the end.
    pub fn extend_silence(&mut self, n: usize) {
        each!(self, q => q.resize(q.len() + n, Default::default()))
    }

    /// The last `n` samples faded out, linearly.
    pub fn fade_tail(&mut self, n: usize) {
        each!(self, q => {
            let end = q.len();
            for i in 0..n {
                let v = &mut q[end - n + i];
                *v = v.scale((n - i) as i64, n as i64);
            }
        })
    }

    /// `frames` frames of `channels` after the end that carry it through a short hole: its last frames mirrored
    /// (backwards from the end, so the join has no step), fading out to silence by the hole's end; silence past what
    /// is queued.
    pub fn conceal(&mut self, frames: usize, channels: usize) {
        each!(self, q => {
            let have = q.len() / channels.max(1);
            for i in 0..frames {
                for c in 0..channels {
                    let v = if i < have { q[(have - 1 - i) * channels + c] } else { Default::default() };
                    q.push_back(v.scale((frames - i) as i64, frames as i64 + 1));
                }
            }
        })
    }

    /// `frames` frames into the S32 stereo buffer (mono doubled), silence after them.
    pub fn fill(&mut self, buf: &mut [i32], frames: usize, channels: u8) {
        each!(self, q => fill(buf, q, frames, channels))
    }

    /// CHUNK frames for the device from `chunk + slip` of the queue's (`fill_slip`). Returns the frames taken.
    pub fn fill_slip(&mut self, buf: &mut [i32], chunk: usize, slip: i64, channels: u8) -> usize {
        each!(self, q => fill_slip(buf, q, chunk, slip, channels))
    }
}

fn fill<T: Sample>(buf: &mut [i32], queue: &mut VecDeque<T>, frames: usize, channels: u8) {
    let n = frames * channels as usize;
    let mut out = 0;
    {
        let (a, b) = queue.as_slices();
        for &v in a.iter().chain(b.iter()).take(n) {
            if channels == 2 {
                buf[out] = v.s32();
                out += 1;
            } else {
                buf[out] = v.s32();
                buf[out + 1] = v.s32();
                out += 2;
            }
        }
    }
    queue.drain(..n);
    buf[out..].fill(0);
}

/// `chunk` frames for the device from `chunk + slip` of the queue's (`slip` ±1: one frame more or fewer played) by
/// linear interpolation, so the rate is 1/chunk off for one write instead of a frame cut out or doubled (a click).
/// The queue holds at least `chunk` + 2 frames. Returns the frames taken from it.
fn fill_slip<T: Sample>(buf: &mut [i32], queue: &mut VecDeque<T>, chunk: usize, slip: i64, channels: u8) -> usize {
    let ch = channels as usize;
    let used = (chunk as i64 + slip) as usize;
    for i in 0..chunk {
        let p = i as u64 * ((used as u64) << 16) / chunk as u64; // where output frame i falls in the queue, Q16
        let (j, f) = ((p >> 16) as usize, (p & 0xffff) as i64);
        for c in 0..2 {
            let a = queue[j * ch + c.min(ch - 1)].s32() as i64;
            let b = queue[(j + 1) * ch + c.min(ch - 1)].s32() as i64;
            buf[i * 2 + c] = (a + (((b - a) * f) >> 16)) as i32;
        }
    }
    queue.drain(..used * ch);
    used
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_convert_on_the_way_in() {
        let mut q = Queue::new(32);
        i16::extend(&mut q, &[1, -1]);
        i32::extend(&mut q, &[0x7FFF_FF00]);
        let mut buf = [0i32; 4];
        q.fill(&mut buf, 1, 2);
        assert_eq!(buf, [0x1_0000, -0x1_0000, 0, 0]);
        let mut q = Queue::new(16);
        i32::extend(&mut q, &[0x1_8000, 0x7FFF_FF00, -0x1_8001]);
        q.fill(&mut buf, 1, 2);
        assert_eq!(&buf[..2], [0x2_0000, 0x7FFF_0000]);
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn a_24_bit_sample_keeps_its_low_bits() {
        let mut q = Queue::new(32);
        i32::extend(&mut q, &[0x1234_5600, -0x100]);
        let mut buf = [0i32; 2];
        q.fill(&mut buf, 1, 2);
        assert_eq!(buf, [0x1234_5600, -0x100]);
    }

    #[test]
    fn slip_interpolates_in_either_width() {
        // a ramp: one frame fewer played over 4 output frames lands between the input frames, the same in both
        let ramp16: Vec<i16> = (0..8).flat_map(|k| [k * 100, -k * 100]).collect();
        let ramp32: Vec<i32> = ramp16.iter().map(|&v| (v as i32) << 16).collect();
        let (mut a, mut b) = ([0i32; 8], [0i32; 8]);
        let (mut q16, mut q32) = (Queue::new(16), Queue::new(32));
        i16::extend(&mut q16, &ramp16);
        i32::extend(&mut q32, &ramp32);
        assert_eq!(q16.fill_slip(&mut a, 4, 1, 2), 5);
        assert_eq!(q32.fill_slip(&mut b, 4, 1, 2), 5);
        assert_eq!(a, b);
        assert_eq!(a[2], 125 << 16);
    }

    #[test]
    fn a_short_hole_carries_the_end_on_mirrored_and_fading() {
        let mut q = Queue::new(16);
        i16::extend(&mut q, &[10, -10, 20, -20, 30, -30]); // three stereo frames
        q.conceal(4, 2);
        let Queue::S16(d) = &q else { unreachable!() };
        // the last frame first (4/5), then the two before it (3/5, 2/5), then silence past what was queued
        assert_eq!(d.iter().skip(6).copied().collect::<Vec<_>>(), [24, -24, 12, -12, 4, -4, 0, 0]);
    }

    #[test]
    fn fade_tail_in_either_width() {
        let mut q = Queue::new(32);
        i32::extend(&mut q, &[1 << 20; 4]);
        q.fade_tail(2);
        let mut buf = [0i32; 4];
        q.fill(&mut buf, 2, 2);
        assert_eq!(buf, [1 << 20, 1 << 20, 1 << 20, 1 << 19]);
    }
}
