//! The module's Vorbis decoder (`module/codecs/vorbis`, fixed point, C) for Rust: raw packets in (the Ogg layer is the
//! caller's), interleaved 16-bit samples out. The same C runs on Warp0 (`module/warp0`, the Vorbis image); this is
//! the ARM's copy, for streams Warp0 doesn't take and for the rest of a stream after a Warp0 failure.

use zd1_dsp as _; // the IMDCT the C calls

unsafe extern "C" {
    fn vorbis_state_size() -> u32;
    fn vorbis_init(
        d: *mut u8,
        ident: *const u8,
        ilen: u32,
        setup: *const u8,
        slen: u32,
        fast: *mut u8,
        flen: u32,
        slow: *mut u8,
        slen2: u32,
    ) -> i32;
    fn vorbis_decode(d: *mut u8, pkt: *const u8, len: u32, pcm: *mut i16) -> i32;
    fn vorbis_restart(d: *mut u8);
    fn vorbis_stream_info(d: *const u8) -> *const RawInfo;
}

/// vorbisdec.h `struct vorbis_info`
#[repr(C)]
#[derive(Clone, Copy)]
struct RawInfo {
    rate: u32,
    channels: u8,
    mode_bits: u8,
    modes: u8,
    pad: u8,
    blocksize: [u16; 2],
    bitrate_nominal: i32,
}

/// Samples per channel one packet can return at most.
pub const MAX_OUT: usize = 1024;
/// Channels the decoder takes at most.
pub const MAX_CHANNELS: usize = 2;

/// Error codes (vorbisdec.h)
pub const EBITS: i32 = -1;
pub const EUNSUP: i32 = -2;
pub const EDATA: i32 = -3;
pub const EMEM: i32 = -4;
pub const ENOTAUDIO: i32 = -5;

/// What the decoder's code means.
pub fn error_name(code: i32) -> &'static str {
    match code {
        EBITS => "a header ended early",
        EUNSUP => "a Vorbis stream this decoder doesn't do (floor 0, block sizes other than 256/2048, > 2 channels)",
        EDATA => "malformed",
        EMEM => "the setup didn't fit its memory",
        ENOTAUDIO => "not an audio packet",
        _ => "?",
    }
}

/// The stream's format, as the decoder read it from the identification header.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ident {
    pub rate: u32,
    pub channels: u8,
    pub blocksize: [u16; 2],
    pub bitrate_nominal: i32,
}

/// One stream's decoder.
pub struct Decoder {
    state: Vec<u64>,  // struct vorbis_dec, 8-byte aligned
    _arena: Vec<u64>, // the setup (codebooks, floors, residues), which the state points into
    ident: Ident,
}

// The state is plain memory, used by one thread at a time.
unsafe impl Send for Decoder {}

impl Decoder {
    /// From the identification and setup headers (packets 1 and 3 of the stream; the comment header is not
    /// needed). `Err` with the decoder's code (`error_name`): EUNSUP for a stream it doesn't take, here or on Warp0
    /// (block sizes other than 256 / 2048, more than two channels).
    pub fn new(ident: &[u8], setup: &[u8]) -> Result<Self, i32> {
        let words = (unsafe { vorbis_state_size() } as usize).div_ceil(8);
        let mut state = vec![0u64; words];
        // libvorbis's streams need 70-110 KB of setup; more for unusual ones
        for kb in [192usize, 1024] {
            let mut arena = vec![0u64; kb * 128];
            let bytes = (arena.len() * 8) as u32;
            let r = unsafe {
                vorbis_init(
                    state.as_mut_ptr() as *mut u8,
                    ident.as_ptr(),
                    ident.len() as u32,
                    setup.as_ptr(),
                    setup.len() as u32,
                    arena.as_mut_ptr() as *mut u8,
                    bytes,
                    arena.as_mut_ptr() as *mut u8,
                    bytes,
                )
            };
            match r {
                0 => {
                    let i = unsafe { *vorbis_stream_info(state.as_ptr() as *const u8) };
                    let ident = Ident {
                        rate: i.rate,
                        channels: i.channels,
                        blocksize: i.blocksize,
                        bitrate_nominal: i.bitrate_nominal,
                    };
                    return Ok(Decoder { state, _arena: arena, ident });
                }
                EMEM => continue,
                e => return Err(e),
            }
        }
        Err(EMEM)
    }

    pub fn ident(&self) -> &Ident {
        &self.ident
    }

    /// Decode one audio packet into `out` (replaced: interleaved samples); returns the samples per channel (0 for
    /// the first packet, which only primes the overlap), or `Err` with the decoder's code for a packet it drops.
    pub fn decode(&mut self, pkt: &[u8], out: &mut Vec<i16>) -> Result<usize, i32> {
        let ch = self.ident.channels as usize;
        out.resize(MAX_OUT * ch, 0);
        let n = unsafe {
            vorbis_decode(self.state.as_mut_ptr() as *mut u8, pkt.as_ptr(), pkt.len() as u32, out.as_mut_ptr())
        };
        if n < 0 {
            out.clear();
            return Err(n);
        }
        out.truncate(n as usize * ch);
        Ok(n as usize)
    }

    /// Forget the overlap (after a seek): the next packet primes it again.
    pub fn restart(&mut self) {
        unsafe { vorbis_restart(self.state.as_mut_ptr() as *mut u8) };
    }
}
