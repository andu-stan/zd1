//! The playback device (snd-sulu, AV port 2 -> the speaker's DSP): ALSA S32 stereo (the sample in bits 31:8) in
//! 256-frame periods, the driver's only size. `Device` counts the frames it was given, its stream's clock, and
//! measures when they play (`Playout`); the output (zd1-airplay, zd1-spotify) and zd1-snapcast's player write through it.

use alsa::pcm::{Access, Format, HwParams, PCM, State};
use alsa::{Direction, ValueOr};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

pub const PERIOD: usize = 256;

/// CLOCK_MONOTONIC in ns: shairplay's clock for play times (`util::now_ns`), which NTP's step at boot doesn't move.
pub fn mono_ns() -> u64 {
    let t = nix::time::clock_gettime(nix::time::ClockId::CLOCK_MONOTONIC).expect("CLOCK_MONOTONIC");
    t.tv_sec() as u64 * 1_000_000_000 + t.tv_nsec() as u64
}

/// `n` frames at `rate`, in ns (exactly: a stream's frame count times a rounded frame length would creep)
pub fn frames_ns(n: u64, rate: u32) -> i64 {
    let r = rate.max(1) as u64;
    ((n / r) * 1_000_000_000 + (n % r) * 1_000_000_000 / r) as i64
}

/// How long `Playout` keeps measurements.
const PLAYOUT_WINDOW_NS: u64 = 1_500_000_000;

/// When frame 0 of the device's stream played (monotonic ns), from each write's measurement while the device runs:
/// now + ALSA's delay - the frames written so far (not while it is prepared but not yet started: what it holds then
/// isn't playing). Every measurement overstates it: ALSA's delay moves only when the driver's pointer does (once per
/// period, so up to 5.8 ms late) and a measurement the scheduler held back is late by that much. The device's rate
/// is steady (its clock and the monotonic clock share the module's crystal), so the least of the last 1.5 s is the
/// time, to a fraction of a millisecond. Measurements while the buffer fills (after a start or an xrun) read up to
/// 5.4 ms early, so they count only until there are full-buffer ones (docs/streaming.md, Sync).
#[derive(Default)]
struct Playout {
    full: VecDeque<(u64, i64)>, // (when, the measurement)
    filling: VecDeque<(u64, i64)>,
}

impl Playout {
    /// This write's measurement (now, ALSA's delay in frames, the frames written so far, whether the buffer was
    /// full): when frame 0 played.
    fn measure(&mut self, now: u64, delay: i64, written: u64, rate: u32, full: bool) -> i64 {
        let w = if full { &mut self.full } else { &mut self.filling };
        w.push_back((now, now as i64 + frames_ns(delay.max(0) as u64, rate) - frames_ns(written, rate)));
        while w.front().is_some_and(|m| now - m.0 > PLAYOUT_WINDOW_NS) {
            w.pop_front();
        }
        let w = if self.full.is_empty() { &self.filling } else { &self.full };
        w.iter().map(|m| m.1).min().unwrap()
    }

    /// The device's stream broke (an xrun): its frames no longer play on the old line.
    fn reset(&mut self) {
        self.full.clear();
        self.filling.clear();
    }
}

pub struct Device {
    pub pcm: PCM,
    pub rate: u32,
    /// Every frame the device was given, silence included: its stream's clock.
    pub frames: u64,
    playout: Playout,
    full: i64, // a delay this long: the buffer is full (writes block)
}

impl Device {
    /// `name` at `rate` with a buffer of `periods` periods. `late_underrun`: an underrun is the hardware's pointer
    /// two periods past ours, not an empty buffer (ALSA's default): the output's faded flush (`reset`, then the fade
    /// written) was an xrun every time with the default, and with one frame past it still when the driver's period
    /// interrupt fell between the two.
    pub fn open(name: &str, rate: u32, periods: usize, late_underrun: bool) -> Result<Device, alsa::Error> {
        let pcm = PCM::new(name, Direction::Playback, false)?;
        let buffer = {
            let hw = HwParams::any(&pcm)?;
            hw.set_access(Access::RWInterleaved)?;
            hw.set_format(Format::S32LE)?;
            hw.set_channels(2)?;
            hw.set_rate(rate, ValueOr::Nearest)?;
            hw.set_period_size_near(PERIOD as alsa::pcm::Frames, ValueOr::Nearest)?;
            hw.set_buffer_size_near((PERIOD * periods) as alsa::pcm::Frames)?;
            pcm.hw_params(&hw)?;
            let got = hw.get_rate()?;
            tracing::debug!(
                buffer = hw.get_buffer_size().unwrap_or(0),
                period = hw.get_period_size().unwrap_or(0),
                "ALSA"
            );
            if got != rate {
                tracing::warn!(want = rate, got, "the device runs at another rate: the stream will play off pitch");
            }
            let buffer = hw.get_buffer_size()?;
            if late_underrun {
                let sw = pcm.sw_params_current()?;
                sw.set_stop_threshold(buffer + 2 * PERIOD as alsa::pcm::Frames)?;
                pcm.sw_params(&sw)?;
            }
            buffer
        };
        pcm.prepare()?;
        pcm.io_i32()?; // the format the writes take
        Ok(Device { pcm, rate, frames: 0, playout: Playout::default(), full: buffer as i64 - 2 * PERIOD as i64 })
    }

    /// As `open`, retried for up to 3 s: the source letting go of the device may still hold it.
    pub fn open_retry(name: &str, rate: u32, periods: usize, late_underrun: bool) -> Option<Device> {
        let t = Instant::now();
        loop {
            match Device::open(name, rate, periods, late_underrun) {
                Ok(d) => return Some(d),
                Err(e) if t.elapsed() > Duration::from_secs(3) => {
                    tracing::warn!(name, %e, "can't open the output");
                    return None;
                }
                Err(_) => std::thread::sleep(Duration::from_millis(100)),
            }
        }
    }

    /// ALSA's delay in frames while the device runs (None while it is prepared, not started).
    pub fn delay(&self) -> Option<i64> {
        (self.pcm.state() == State::Running).then(|| self.pcm.delay().ok()).flatten().map(i64::from)
    }

    /// When frame 0 of the device's stream played (monotonic ns), with this write's measurement (`delay` read at
    /// `now`) taken in; None without one.
    pub fn start_ns(&mut self, now: u64, delay: Option<i64>) -> Option<i64> {
        delay.map(|d| self.playout.measure(now, d, self.frames, self.rate, d >= self.full))
    }

    /// Interleaved stereo frames to the device (blocks while its buffer is full): Some(frames written), or None for
    /// an xrun it recovered from (the stream's timing starts over); Err: it didn't recover (open it again).
    pub fn write(&mut self, buf: &[i32]) -> Result<Option<usize>, alsa::Error> {
        match self.pcm.io_i32()?.writei(buf) {
            Ok(n) => {
                self.frames += n as u64;
                Ok(Some(n))
            }
            Err(e) => {
                tracing::debug!(%e, "ALSA xrun");
                self.playout.reset();
                self.pcm.try_recover(e, true)?;
                Ok(None)
            }
        }
    }
}
