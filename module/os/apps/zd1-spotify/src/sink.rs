//! librespot's player -> our output (zd1-audio): its sink, and its "mixer" (the volume, which goes to the speaker's
//! own level control: samples go out at full scale, as from AirPlay).

use librespot_core::Error;
use librespot_playback::audio_backend::{Sink, SinkResult};
use librespot_playback::convert::Converter;
use librespot_playback::decoder::AudioPacket;
use librespot_playback::mixer::{Mixer, MixerConfig};
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;
use zd1_audio::{Output, Speaker, Stream};

/// How far the player may run ahead of the speaker (the decoders are faster than real time): the queue's depth.
const AHEAD_MS: usize = 500;

pub struct OutputSink {
    out: Output,
    speaker: Speaker,
    stream: Option<Stream>, // from the player's start on
}

impl OutputSink {
    pub fn new(out: Output, speaker: Speaker) -> Self {
        OutputSink { out, speaker, stream: None }
    }
}

impl Sink for OutputSink {
    fn start(&mut self) -> SinkResult<()> {
        let s = self.out.start_stream(librespot_playback::SAMPLE_RATE, librespot_playback::NUM_CHANNELS, 16);
        self.stream = Some(s);
        Ok(())
    }

    /// Paused or stopped: what is queued plays out (up to AHEAD_MS); the device closes once idle, so another
    /// source can have it.
    fn stop(&mut self) -> SinkResult<()> {
        self.out.drain(Duration::from_millis(AHEAD_MS as u64 * 2));
        Ok(())
    }

    fn write(&mut self, packet: AudioPacket, _converter: &mut Converter) -> SinkResult<()> {
        // S16 only: the player has no decoder but ours (librespot-playback without symphonia)
        if let (AudioPacket::S16(s), Some(stream)) = (packet, self.stream) {
            self.out.push_paced(stream, &s, AHEAD_MS, &self.speaker);
        }
        Ok(())
    }
}

/// Spotify's volume (0..=65535) as the speaker's (0-100), through zd1-linkd.
pub struct SpeakerVolume {
    volume: AtomicU16,
    speaker: Speaker,
}

impl SpeakerVolume {
    pub fn new(speaker: Speaker, initial: u16) -> Self {
        SpeakerVolume { volume: AtomicU16::new(initial), speaker }
    }
}

impl Mixer for SpeakerVolume {
    fn open(_: MixerConfig) -> Result<Self, Error> {
        Err(Error::unimplemented("made with SpeakerVolume::new"))
    }

    fn volume(&self) -> u16 {
        self.volume.load(Ordering::Relaxed)
    }

    fn set_volume(&self, volume: u16) {
        self.volume.store(volume, Ordering::Relaxed);
        let v = (volume as u32 * 100 + 32767) / 65535;
        tracing::debug!(volume, speaker = v, "volume");
        self.speaker.send(&format!("volume {v}"));
    }
}
