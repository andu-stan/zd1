//! The module's FLAC decoder (`module/codecs/flac`, fixed point, C) for Rust: whole frames in (a Snapcast server's
//! chunks), interleaved samples out, each left-justified in 32 bits (as S32 audio). The same C runs on Warp0
//! (`module/warp0`, the FLAC image, with its smaller limits); this is the ARM's copy, built with FLAC's own limits.

unsafe extern "C" {
    fn flac_state_size() -> u32;
    fn flac_init(d: *mut u8, streaminfo: *const u8, len: u32) -> i32;
    fn flac_stream_info(d: *const u8) -> *const Info;
    fn flac_decode_from(
        d: *mut u8,
        input: *const u8,
        len: u32,
        start: u32,
        out: *mut i32,
        cap: u32,
        used: *mut u32,
        samples: *mut u32,
    ) -> i32;
}

/// Channels and block length this build takes at most.
pub const MAX_CHANNELS: usize = 8;
pub const MAX_BLOCK: usize = 65535;

/// Error codes (flacdec.h)
pub const EINFO: i32 = -1;
pub const EUNSUP: i32 = -2;
pub const ESYNC: i32 = -3;
pub const EHEADER: i32 = -4;
pub const ECRC8: i32 = -5;
pub const ESUBFRAME: i32 = -6;
pub const ERESIDUAL: i32 = -7;
pub const EOVERRUN: i32 = -8;
pub const ECRC16: i32 = -9;
pub const EFORMAT: i32 = -10;
pub const EOUTPUT: i32 = -11;

/// What the decoder's code means.
pub fn error_name(code: i32) -> &'static str {
    match code {
        EINFO => "STREAMINFO short or invalid",
        EUNSUP => "more than 8 channels or 24 bits",
        ESYNC => "no frame sync where a frame should start",
        EHEADER => "a reserved value in a frame header",
        ECRC8 => "a frame header's CRC-8",
        ESUBFRAME => "a reserved subframe type or LPC parameter",
        ERESIDUAL => "a reserved residual coding or partition order",
        EOVERRUN => "a frame runs past the chunk",
        ECRC16 => "a frame's CRC-16",
        EFORMAT => "a frame's channels or bits differ from STREAMINFO's",
        EOUTPUT => "no room for the next frame",
        _ => "?",
    }
}

/// The stream's format, as the decoder read it from STREAMINFO (flacdec.h `struct flac_info`).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Info {
    pub rate: u32,
    pub channels: u32,
    pub bits: u32,
    pub min_block: u32,
    pub max_block: u32,
}

pub struct Decoder {
    state: Vec<u64>, // struct flac_dec (8-byte aligned)
    info: Info,
    input: Vec<u32>, // the chunk, word aligned (the decoder reads whole words)
}

impl Decoder {
    /// From the 34-byte STREAMINFO body.
    pub fn new(streaminfo: &[u8]) -> Result<Decoder, i32> {
        if streaminfo.len() < 34 {
            return Err(EINFO);
        }
        let mut state = vec![0u64; (unsafe { flac_state_size() } as usize).div_ceil(8)];
        let r = unsafe { flac_init(state.as_mut_ptr() as *mut u8, streaminfo.as_ptr(), 34) };
        if r != 0 {
            return Err(r);
        }
        let info = unsafe { *flac_stream_info(state.as_ptr() as *const u8) };
        Ok(Decoder { state, info, input: Vec::new() })
    }

    pub fn info(&self) -> Info {
        self.info
    }

    /// A chunk's frames appended to `out` (interleaved, `info().channels` per frame). The frames before a bad one
    /// stay in `out`.
    pub fn decode(&mut self, chunk: &[u8], out: &mut Vec<i32>) -> Result<(), i32> {
        self.input.clear();
        self.input.resize(chunk.len().div_ceil(4), 0);
        unsafe { std::ptr::copy_nonoverlapping(chunk.as_ptr(), self.input.as_mut_ptr() as *mut u8, chunk.len()) };
        let len = chunk.len() as u32;
        // room for a frame of the stream's largest block at least; more frames: another call from where it stopped
        let room = (self.info.max_block.max(4608) * self.info.channels) as usize;
        let mut at = 0u32;
        while at < len {
            let base = out.len();
            out.resize(base + room, 0);
            let (mut used, mut samples) = (0u32, 0u32);
            let r = unsafe {
                flac_decode_from(
                    self.state.as_mut_ptr() as *mut u8,
                    self.input.as_ptr() as *const u8,
                    len,
                    at,
                    out.as_mut_ptr().add(base),
                    room as u32,
                    &mut used,
                    &mut samples,
                )
            };
            out.truncate(base + samples as usize);
            if r != 0 {
                return Err(r);
            }
            if used <= at {
                break; // nothing taken: the rest is shorter than a frame header (trailing bytes)
            }
            at = used;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaminfo_is_read_and_checked() {
        // 44.1 kHz, 2 channels, 16 bits, blocks 4096
        let si = [
            0x10, 0x00, 0x10, 0x00, 0, 0, 0, 0, 0, 0, 0x0A, 0xC4, 0x42, 0xF0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0,
        ];
        let d = Decoder::new(&si).unwrap();
        assert_eq!(d.info(), Info { rate: 44100, channels: 2, bits: 16, min_block: 4096, max_block: 4096 });
        assert_eq!(Decoder::new(&si[..20]).err(), Some(EINFO));
    }

    #[test]
    fn a_stream_decodes_like_libflac() {
        // tests/data: 0.25 s of 44.1 kHz stereo 16-bit music at flac -8 (LPC, mid/side) and what `flac -d` makes of it
        let file = include_bytes!("../tests/data/music.flac");
        let want = include_bytes!("../tests/data/music.raw");
        let mut at = 4;
        let mut si = None;
        loop {
            let (last, kind) = (file[at] >> 7, file[at] & 0x7F);
            let n = (file[at + 1] as usize) << 16 | (file[at + 2] as usize) << 8 | file[at + 3] as usize;
            if kind == 0 {
                si = Some(&file[at + 4..at + 4 + 34]);
            }
            at += 4 + n;
            if last == 1 {
                break;
            }
        }
        let mut d = Decoder::new(si.unwrap()).unwrap();
        let mut out = Vec::new();
        d.decode(&file[at..], &mut out).unwrap();
        let got: Vec<u8> = out.iter().flat_map(|&v| ((v >> 16) as i16).to_le_bytes()).collect();
        assert_eq!(got.len(), want.len());
        assert!(got == want[..]);
    }
}
