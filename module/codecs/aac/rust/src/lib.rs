//! The module's AAC-LC decoder (`module/codecs/aac`, fixed point, C) for Rust, and [`decoder`], the same as a
//! shairplay-rust `Decoder` for a buffered AAC stream (zd1-warp0's ARM side: `RaopServerBuilder::decoders`).

use zd1_dsp as _; // the IMDCT the C calls

unsafe extern "C" {
    fn aac_state_size() -> u32;
    fn aac_init(d: *mut u8, rate: u32, channels: u32) -> i32;
    fn aac_decode(d: *mut u8, input: *const u8, len: u32, pcm: *mut i16) -> i32;
}

/// One stream's decoder: raw AAC-LC access units in, 1024 interleaved 16-bit samples per channel out.
pub struct Decoder {
    state: Vec<u64>, // struct aac_dec, 8-byte aligned
    channels: usize,
}

impl Decoder {
    /// `None` for a rate or channel count the decoder doesn't take (rates 7.35-96 kHz, one or two channels).
    pub fn new(sample_rate: u32, channels: u8) -> Option<Self> {
        let words = (unsafe { aac_state_size() } as usize).div_ceil(8);
        let mut state = vec![0u64; words];
        if unsafe { aac_init(state.as_mut_ptr() as *mut u8, sample_rate, channels as u32) } != 0 {
            return None;
        }
        Some(Decoder { state, channels: channels as usize })
    }

    /// Decode one access unit into `out` (replaced); `Err` with the decoder's code for a frame it drops.
    pub fn decode(&mut self, au: &[u8], out: &mut Vec<i16>) -> Result<(), i32> {
        out.resize(1024 * self.channels, 0);
        let n =
            unsafe { aac_decode(self.state.as_mut_ptr() as *mut u8, au.as_ptr(), au.len() as u32, out.as_mut_ptr()) };
        if n < 0 {
            return Err(n);
        }
        out.truncate(n as usize);
        Ok(())
    }
}

/// A shairplay-rust decoder: each access unit decoded as it is sent.
struct Plugin {
    decoder: Decoder,
    done: std::collections::VecDeque<shairplay::decoder::Decoded>,
}

impl shairplay::decoder::Decoder for Plugin {
    fn send(&mut self, au: &[u8]) {
        use shairplay::decoder::Decoded;
        let mut out = Vec::new();
        self.done.push_back(match self.decoder.decode(au, &mut out) {
            Ok(_) => Decoded::S16(out),
            Err(_) => Decoded::Failed,
        });
    }

    fn receive(&mut self) -> Option<shairplay::decoder::Decoded> {
        self.done.pop_front()
    }
}

/// This decoder as shairplay-rust's for an AAC stream of `rate` and `channels`, if it takes the format.
pub fn decoder(rate: u32, channels: u8) -> Option<Box<dyn shairplay::decoder::Decoder>> {
    let decoder = Decoder::new(rate, channels)?;
    Some(Box::new(Plugin { decoder, done: Default::default() }))
}
