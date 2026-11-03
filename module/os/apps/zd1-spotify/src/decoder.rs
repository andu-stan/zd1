//! Spotify's Ogg Vorbis files decoded by our decoder (module/codecs/vorbis), on Warp0 when it is free (module/warp0: the
//! Vorbis image), else on the ARM: a librespot `DecoderPlugin` (our librespot fork, module/os/forks.md).
//!
//! The Ogg layer is the `ogg` crate's (pages, packets, seeking by granule position); packets go to the decoder
//! ahead of the player (up to the Warp0 pipeline's depth), results come back in order. Positions are counted in
//! samples, set exactly at every page's last packet from the page's granule position; the stream's last packet is
//! cut to its granule position (the encoder's padding). Mono streams come out as stereo: the player and the output
//! take two channels.

use librespot_metadata::audio::{AudioFileFormat, AudioFiles};
use librespot_playback::decoder::{
    AudioDecoder, AudioPacket, AudioPacketPosition, DecoderError, DecoderPlugin, DecoderResult, ReadSeek,
};
use std::collections::VecDeque;
use std::io::{Read, Seek};
use std::time::Duration;
use zd1_warp0::VorbisStream;

pub struct VorbisPlugin {
    pub warp0: bool,
}

impl DecoderPlugin for VorbisPlugin {
    fn takes(&self, format: AudioFileFormat) -> bool {
        AudioFiles::is_ogg_vorbis(format)
    }

    fn open(&self, file: Box<dyn ReadSeek>, _format: AudioFileFormat) -> DecoderResult<Box<dyn AudioDecoder + Send>> {
        Ok(Box::new(OggVorbis::open(file, self.warp0)?))
    }
}

fn err(e: impl std::fmt::Display) -> DecoderError {
    DecoderError::Plugin(e.to_string())
}

/// What the Ogg layer said about a packet in flight.
struct Meta {
    absgp: Option<u64>, // the page's granule position, if the packet is the page's last
    last: bool,         // the stream's last packet
}

pub struct OggVorbis {
    rdr: ogg::PacketReader<Box<dyn ReadSeek>>,
    stream: VorbisStream,
    rate: u64,
    mono: bool,
    meta: VecDeque<(u64, Meta)>,
    next_tag: u64,
    pos: u64, // samples per channel before the next output (exact after a page's last packet)
    eof: bool,
    read_any: bool,
}

impl OggVorbis {
    fn open(mut file: Box<dyn ReadSeek>, warp0: bool) -> DecoderResult<Self> {
        // librespot goes on without the key when Spotify refuses it: then this is still ciphertext
        let mut magic = [0u8; 4];
        let start = file.stream_position().map_err(err)?;
        file.read_exact(&mut magic).map_err(err)?;
        file.seek(std::io::SeekFrom::Start(start)).map_err(err)?;
        if &magic != b"OggS" {
            return Err(err("not an Ogg stream: still encrypted (no audio key from Spotify)?"));
        }
        let mut rdr = ogg::PacketReader::new(file);
        let mut headers = Vec::new();
        while headers.len() < 3 {
            let p = rdr.read_packet().map_err(err)?.ok_or_else(|| err("the headers are missing"))?;
            if p.data.first().is_some_and(|b| b & 1 == 1) {
                headers.push(p.data);
            }
        }
        let stream = VorbisStream::new(&headers[0], &headers[2], warp0).map_err(err)?;
        let id = *stream.ident();
        tracing::info!(
            rate = id.rate,
            channels = id.channels,
            kbps = id.bitrate_nominal / 1000,
            warp0 = stream.on_warp(),
            "Vorbis stream"
        );
        Ok(OggVorbis {
            rdr,
            rate: id.rate as u64,
            mono: id.channels == 1,
            stream,
            meta: VecDeque::new(),
            next_tag: 0,
            pos: 0,
            eof: false,
            read_any: false,
        })
    }

    /// Read and submit packets while the decoder has room.
    fn feed(&mut self) -> DecoderResult<()> {
        while !self.eof && self.stream.can_submit() && self.meta.len() < 16 {
            let Some(p) = self.rdr.read_packet().map_err(err)? else {
                self.eof = true;
                break;
            };
            self.read_any = true;
            if p.data.first().is_none_or(|b| b & 1 == 1) {
                continue; // a header (read again after a seek to the start) or nothing
            }
            let tag = self.next_tag;
            self.next_tag += 1;
            let meta = Meta { absgp: p.last_in_page().then(|| p.absgp_page()), last: p.last_in_stream() };
            if self.stream.submit(tag, &p.data) {
                self.meta.push_back((tag, meta));
            }
            if p.last_in_stream() {
                self.eof = true;
            }
        }
        Ok(())
    }
}

impl AudioDecoder for OggVorbis {
    fn seek(&mut self, position_ms: u32) -> Result<u32, DecoderError> {
        let target = position_ms as u64 * self.rate / 1000;
        if target == 0 && !self.read_any {
            return Ok(0); // the start, and nothing read past the headers yet
        }
        self.rdr.seek_absgp(None, target).map_err(err)?;
        self.stream.restart();
        self.meta.clear();
        self.pos = target; // until the next page's granule position says exactly
        self.eof = false;
        Ok(position_ms)
    }

    fn next_packet(&mut self) -> DecoderResult<Option<(AudioPacketPosition, AudioPacket)>> {
        loop {
            self.feed()?;
            if let Some((tag, r)) = self.stream.poll() {
                let Some((mtag, meta)) = self.meta.pop_front() else { continue };
                debug_assert_eq!(tag, mtag);
                let Ok(mut pcm) = r else {
                    continue; // a packet the decoder drops (corrupt): its time is lost, as libvorbis does
                };
                let ch = if self.mono { 1 } else { 2 };
                let mut n = (pcm.len() / ch) as u64;
                // a page's last packet ends at the page's granule position, except the stream's last: that one
                // decodes past it (the encoder's padding), so it starts where the count is and is cut there
                let start = match meta.absgp {
                    Some(g) if g >= n && !meta.last => g - n,
                    _ => self.pos,
                };
                if let (true, Some(g)) = (meta.last, meta.absgp)
                    && start + n > g
                {
                    n = g.saturating_sub(start);
                    pcm.truncate(n as usize * ch);
                }
                self.pos = start + n;
                if n == 0 {
                    continue;
                }
                if self.mono {
                    pcm = pcm.iter().flat_map(|&v| [v, v]).collect();
                }
                let position_ms = (start * 1000 / self.rate) as u32;
                return Ok(Some((AudioPacketPosition { position_ms, skipped: false }, AudioPacket::S16(pcm))));
            }
            if self.eof && self.meta.is_empty() {
                return Ok(None);
            }
            // Warp0 is decoding what is in flight: a tick of the kernel's clock (10 ms) and look again
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
