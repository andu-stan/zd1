//! Codecs: the server's codec header says which and in what format; chunks decode to interleaved i32 frames with
//! the sample left-justified (S32: what ALSA and the AV port take). PCM and FLAC; Opus/Vorbis aren't supported
//! (snapserver's default codec is FLAC). FLAC: our decoder (module/codecs/flac), on Warp0 (module/warp0, the FLAC
//! image) while `use_warp` says so and it is free, else its ARM build (zd1-flac, with FLAC's own limits: up to 8
//! channels, 65535-sample blocks); results come out in the chunks' order either way.

use std::collections::VecDeque;
use zd1_warp0::flac::{FlacStream, Output};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Format {
    pub rate: u32,
    pub channels: u32,
    pub bits: u32,
}

#[derive(Clone, Copy, PartialEq)]
enum Codec {
    Pcm,
    Flac,
}

/// A chunk's samples (interleaved, left-justified), or why it didn't decode.
pub type Decoded = Result<Vec<i32>, String>;

pub struct Decoder {
    codec: Codec,
    format: Format,
    streaminfo: Vec<u8>,            // FLAC: the 34 bytes Warp0 is set up with
    arm: Option<zd1_flac::Decoder>, // FLAC on the ARM: module/codecs/flac, the C Warp0 runs
    warp: Option<FlacStream>,
    ready: VecDeque<(u64, Decoded)>, // decoded, in order (before anything still on Warp0)
}

impl Decoder {
    pub fn new(codec: &str, header: &[u8]) -> Result<Decoder, String> {
        let (codec, format, streaminfo, arm) = match codec {
            "pcm" => (Codec::Pcm, wav_format(header).ok_or("pcm: bad RIFF header")?, Vec::new(), None),
            "flac" => {
                let si = streaminfo(header).ok_or("flac: no STREAMINFO")?;
                let d = zd1_flac::Decoder::new(si).map_err(|e| format!("flac: {}", zd1_flac::error_name(e)))?;
                let zd1_flac::Info { rate, channels, bits, .. } = d.info();
                (Codec::Flac, Format { rate, channels, bits }, si.to_vec(), Some(d))
            }
            c => return Err(format!("codec {c} not supported (use flac or pcm on the server)")),
        };
        let Format { rate, channels, bits } = format;
        let pcm_bits = codec == Codec::Flac || matches!(bits, 16 | 24 | 32);
        if !(RATES.contains(&rate) && (1..=8).contains(&channels) && pcm_bits) {
            return Err(format!("a stream of {rate} Hz, {channels} channels, {bits} bits isn't played"));
        }
        Ok(Decoder { codec, format, streaminfo, arm, warp: None, ready: VecDeque::new() })
    }
    pub fn format(&self) -> Format {
        self.format
    }
    pub fn is_flac(&self) -> bool {
        self.codec == Codec::Flac
    }
    pub fn on_warp(&self) -> bool {
        self.warp.as_ref().is_some_and(|w| w.on_warp())
    }
    /// Chunks on Warp0 whose results aren't out yet.
    pub fn busy(&self) -> bool {
        self.warp.as_ref().is_some_and(|w| w.in_flight() > 0)
    }

    /// FLAC on Warp0 from now on (true: taken if it is free; false: what it has in flight finished, then let go).
    /// Whether Warp0 decodes the stream now.
    pub fn use_warp(&mut self, on: bool) -> bool {
        if self.codec != Codec::Flac {
            return false;
        }
        if on && self.warp.is_none() {
            let s = FlacStream::new(&self.streaminfo, true);
            if s.on_warp() {
                self.warp = Some(s);
            }
        } else if !on && let Some(mut s) = self.warp.take() {
            s.finish();
            while let Some((tag, out)) = s.poll() {
                let r = self.finish_warp(out);
                self.ready.push_back((tag, r));
            }
        }
        self.on_warp()
    }

    /// A chunk; its result comes out of `take` in order. While a Warp0 stream exists every chunk goes through it,
    /// also after a hang took Warp0 away (it returns them for the ARM in their place, behind what it still holds).
    pub fn push(&mut self, tag: u64, data: Vec<u8>) {
        if let Some(w) = self.warp.as_mut() {
            w.submit(tag, data);
            return;
        }
        let mut out = Vec::new();
        let r = self.decode_arm(&data, &mut out).map(|_| out);
        self.ready.push_back((tag, r));
    }

    /// The oldest chunk's result, if it is in.
    pub fn take(&mut self) -> Option<(u64, Decoded)> {
        if let Some(r) = self.ready.pop_front() {
            return Some(r);
        }
        let (tag, out) = self.warp.as_mut()?.poll()?;
        Some((tag, self.finish_warp(out)))
    }

    fn finish_warp(&mut self, out: Output) -> Decoded {
        match out {
            Output::Pcm(v) => Ok(v),
            Output::Arm(data) => {
                let mut out = Vec::new();
                self.decode_arm(&data, &mut out).map(|_| out)
            }
        }
    }

    /// The chunk's frames appended to `out` (interleaved, `format().channels` per frame), on the ARM.
    fn decode_arm(&mut self, data: &[u8], out: &mut Vec<i32>) -> Result<(), String> {
        match self.codec {
            Codec::Pcm => {
                let f = self.format;
                let bytes = (f.bits as usize).div_ceil(8);
                let shift = 32 - f.bits;
                for s in data.chunks_exact(bytes) {
                    let v = match bytes {
                        2 => i16::from_le_bytes([s[0], s[1]]) as i32,
                        3 => (i32::from_le_bytes([0, s[0], s[1], s[2]])) >> 8,
                        4 => i32::from_le_bytes([s[0], s[1], s[2], s[3]]),
                        _ => 0,
                    };
                    out.push(if bytes == 4 { v } else { v << shift });
                }
                Ok(())
            }
            Codec::Flac => {
                let Some(d) = self.arm.as_mut() else { return Err("flac: no decoder".into()) };
                d.decode(data, out).map_err(|e| format!("flac: {}", zd1_flac::error_name(e)))
            }
        }
    }
}

/// The rates a stream may have (it is resampled to OUT_RATE); beyond these the header is broken.
const RATES: std::ops::RangeInclusive<u32> = 8000..=192_000;

fn wav_format(h: &[u8]) -> Option<Format> {
    if h.len() < 36 || &h[..4] != b"RIFF" || &h[8..12] != b"WAVE" {
        return None;
    }
    let mut at = 12;
    while at + 8 <= h.len() {
        let n = u32::from_le_bytes(h[at + 4..at + 8].try_into().ok()?) as usize;
        if &h[at..at + 4] == b"fmt " && at + 8 + 16 <= h.len() {
            let f = &h[at + 8..];
            return Some(Format {
                channels: u16::from_le_bytes([f[2], f[3]]) as u32,
                rate: u32::from_le_bytes(f[4..8].try_into().ok()?),
                bits: u16::from_le_bytes([f[14], f[15]]) as u32,
            });
        }
        at += 8 + n;
    }
    None
}

/// The STREAMINFO body in the codec header: "fLaC", then the first metadata block, which is STREAMINFO (the
/// decoder reads it).
fn streaminfo(h: &[u8]) -> Option<&[u8]> {
    (h.len() >= 8 + 34 && &h[..4] == b"fLaC" && h[4] & 0x7F == 0).then(|| &h[8..8 + 34])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pcm_16_and_24() {
        let mut hdr =
            b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0\x01\0\x02\0\x44\xac\0\0\0\0\0\0\x04\0\x10\0data\0\0\0\0".to_vec();
        let mut d = Decoder::new("pcm", &hdr).unwrap();
        assert_eq!(d.format(), Format { rate: 44100, channels: 2, bits: 16 });
        d.push(1, vec![0x01, 0x00, 0xFF, 0xFF]);
        assert_eq!(d.take().unwrap(), (1, Ok(vec![1 << 16, -1 << 16])));
        hdr[34] = 24;
        let mut d = Decoder::new("pcm", &hdr).unwrap();
        d.push(2, vec![0x01, 0x00, 0x00, 0xFF, 0xFF, 0xFF]);
        assert_eq!(d.take().unwrap(), (2, Ok(vec![1 << 8, -1 << 8])));
    }

    #[test]
    fn flac_header() {
        // "fLaC", STREAMINFO (last block, 34 bytes): blocks 4096, 48 kHz, 2 channels, 24 bits
        let mut h = b"fLaC\x80\0\0\x22".to_vec();
        h.extend([0x10, 0x00, 0x10, 0x00, 0, 0, 0, 0, 0, 0, 0x0B, 0xB8, 0x03, 0x70]);
        h.resize(8 + 34, 0);
        assert_eq!(Decoder::new("flac", &h).unwrap().format(), Format { rate: 48000, channels: 2, bits: 24 });
        h[4] = 0x84; // another block first
        assert!(Decoder::new("flac", &h).is_err());
        assert!(Decoder::new("flac", &h[..30]).is_err());
    }

    #[test]
    fn broken_headers_are_refused() {
        let hdr = b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0\x01\0\x02\0\x44\xac\0\0\0\0\0\0\x04\0\x10\0data\0\0\0\0".to_vec();
        for (at, v) in [(22, 0), (22, 9), (24, 0), (34, 8), (34, 20)] {
            let mut h = hdr.clone();
            h[at] = v;
            if at == 24 {
                h[25] = 0;
            }
            assert!(Decoder::new("pcm", &h).is_err(), "byte {at} = {v}");
        }
    }
}
