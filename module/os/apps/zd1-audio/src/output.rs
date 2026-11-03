//! The output: the device (`device`) at the stream's rate, fed from a queue of the samples the sources push
//! (16-bit, or 32-bit for a deeper source: `queue`). AirPlay 1 and AirPlay 2 realtime streams are pushed as they
//! arrive (the sender paces them), AirPlay 2 buffered ones as they fall due, Spotify's as its player decodes them,
//! held back while half a second is queued (`push_paced`); either way the blocking ALSA write paces this thread. It
//! starts writing once PREBUFFER is queued (again after running dry: deeper each time a stream that has been playing
//! runs dry, up to MAX_PREBUFFER, for the rest of that stream), keeps the queue under MAX_QUEUE (oldest dropped:
//! bounded memory and latency), and closes the device IDLE after the last sample so other sources (zd1-snapcast) can
//! have it. The speaker is told "playing" before the device opens, so another source lets go of it first
//! (zd1-linkd `yield`).
//!
//! Timed streams (AirPlay 2 buffered audio on a followed PTP clock: `push` with a play time) carry the local time
//! each sample should leave the speaker. Before each write the thread predicts when the next sample will (the
//! device's measurement of when its stream started, plus the frames written since, plus the output's fixed offset
//! `OUTPUT_OFFSET_US`, calibrated acoustically), and steers the error to zero (`Steer`): on the first measurement
//! (after a jump, after a flush: pause, seek, next track) it jumps there (dropped frames or silence); after that a
//! jump only once the smoothed error passes JUMP_NS, else it slews: one frame more or fewer per write, stretched over
//! the write by interpolation (a 0.1 % rate change for 23 ms, inaudible; a spliced frame clicks), from STEER_NS until
//! within SETTLE_NS (that also follows the crystals' drift, ~22 ppm here: about a frame a second).
//! Audio fades over FADE frames where the queue runs dry (a flush, the network) and where it starts again.
//! docs/streaming.md (Synchronised playback, Playout) has the measurements behind the numbers.

use super::device::{Device, PERIOD, frames_ns, mono_ns};
use super::queue::{Queue, Sample};
use super::speaker::Speaker;
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

// ALSA's buffer: 186 ms at 44.1 kHz, so the thread may be held up for a while (output on the module's serial
// console held it 117 ms once)
const PERIODS: usize = 32;
const CHUNK: usize = PERIOD * 4; // frames per write: four periods (fewer system calls)
const PREBUFFER_MS: usize = 250;
// over Wi-Fi a realtime stream's packets stop for ~300 ms at times (nothing lost: shairplay's realtime gap_max_ms),
// past what 250 ms of prebuffer leaves queued beyond ALSA's
const PREBUFFER_STEP_MS: usize = 250;
const MAX_PREBUFFER_MS: usize = 1000;
const MAX_QUEUE_MS: usize = 3000; // timed realtime streams arrive ~2 s ahead of their play time
const IDLE: Duration = Duration::from_secs(3);
const JUMP_NS: i64 = 10_000_000;
const STEER_NS: i64 = 500_000;
const SETTLE_NS: i64 = 100_000;
const FADE: usize = 256; // frames (5.8 ms): audio fades out where the queue runs dry and in where it starts again
const GAP_SLACK: i64 = 32; // frames (0.7 ms): how close a gap in a timed stream is to whole missing frames
const RATES: std::ops::RangeInclusive<u32> = 8000..=192_000; // a stream's rate beyond these is a broken header

/// The output's fixed offset, added to the predicted time a sample leaves the speaker: the delay from where the
/// driver counts a frame played to the air (the module's AV port, the DSP, the codec), less whatever the driver
/// counts early. Calibrated acoustically against a Mac's own speakers in an AirPlay group (docs/streaming.md,
/// Calibration). ZD1_OUTPUT_DELAY_US overrides.
const OUTPUT_OFFSET_US: i64 = -3600;

/// A stream's number (`start_stream`), which its pushes and flushes name: a session another one replaced (a second
/// sender took over) then pushes into nothing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stream(u64);

#[derive(Clone)]
pub struct Output {
    inner: Arc<(Mutex<State>, Condvar)>,
}

struct State {
    queue: Queue, // interleaved, `channels` per frame
    rate: u32,
    channels: u8,
    active: bool, // a stream runs (start_stream .. stop)
    last_push: Instant,
    dropped: u64, // frames dropped because the queue overflowed
    tap: Tap,
    // timed streams: frames pushed and taken since the stream started, and (frame, local ns it should play) marks
    pushed: u64,
    taken: u64,
    marks: VecDeque<(u64, u64)>,
    flushes: u64, // flush() calls: running dry after one (pause, seek) is not a network stall
    stream: u64,  // the current stream's number
}

impl State {
    /// When frame `f` of the stream should play (local monotonic ns), from the last mark at or before it.
    fn target(&self, f: u64) -> Option<u64> {
        let &(at, t) = self.marks.iter().rev().find(|m| m.0 <= f)?;
        Some(t + (f - at) * 1_000_000_000 / self.rate as u64)
    }

    /// Frames queued.
    fn frames(&self) -> usize {
        self.queue.len() / self.channels as usize
    }

    /// `n` frames gone from the front of the queue (played or dropped): the count moves on, with the marks.
    fn advance(&mut self, n: usize) {
        self.taken += n as u64;
        // keep one mark at or before the front
        while self.marks.len() > 1 && self.marks[1].0 <= self.taken {
            self.marks.pop_front();
        }
    }

    /// `n` frames off the front of the queue (dropped or played).
    fn take(&mut self, n: usize) {
        let n = n.min(self.frames());
        self.queue.drain(n * self.channels as usize);
        self.advance(n);
    }

    /// A timed push's samples, where frames are missing before them (a seek's first frames come with sequence gaps,
    /// a realtime stream loses packets): up to 25 ms the audio before carried through (`Queue::conceal`), longer
    /// silence between fades; then the new samples fade in. Played as they came, each gap was a step in the timing. Such a gap is
    /// a whole number of frames (pushes of the same length) to within GAP_SLACK; anything else off the queue's end is
    /// the clock's mapping moving (a Wi-Fi stall), which the output steers through (docs/streaming.md, Playout).
    fn fill_gap<'a, T: Sample>(&mut self, samples: &'a [T], play_at: u64, faded: &'a mut Vec<T>) -> &'a [T] {
        let ch = self.channels as usize;
        let len = (samples.len() / ch) as i64;
        let gap = self.marks.back().map_or(0, |&(f, t)| {
            let end = t as i64 + frames_ns(self.pushed - f, self.rate);
            (play_at as i64 - end) * self.rate as i64 / 1_000_000_000
        });
        let missing =
            gap > 0 && len > 2 * GAP_SLACK && gap >= len - GAP_SLACK && (gap + GAP_SLACK) % len <= 2 * GAP_SLACK;
        if !missing || gap > self.rate as i64 {
            return samples;
        }
        // a short hole (a few lost packets) is carried through by what came before it; a longer one is silence
        if gap <= self.rate as i64 / 40 {
            tracing::debug!(frames = gap, "timed stream: frames missing, the audio before carried through");
            self.queue.conceal(gap as usize, ch);
        } else {
            tracing::debug!(frames = gap, "timed stream: frames missing, silence in their place");
            let n = self.frames().min(FADE);
            self.queue.fade_tail(n * ch);
            self.queue.extend_silence(gap as usize * ch);
        }
        self.pushed += gap as u64;
        faded.extend_from_slice(samples);
        let n = (faded.len() / ch).min(FADE) * ch;
        for (i, v) in faded[..n].iter_mut().enumerate() {
            *v = v.scale((i + 1) as i64, n as i64);
        }
        faded
    }

    /// Samples onto the queue; past MAX_QUEUE the oldest go.
    fn append<T: Sample>(&mut self, samples: &[T]) {
        let ch = self.channels as usize;
        T::extend(&mut self.queue, samples);
        self.pushed += (samples.len() / ch) as u64;
        self.tap.add(samples);
        let max = MAX_QUEUE_MS * self.rate as usize / 1000 * ch;
        if self.queue.len() > max {
            let n = (self.queue.len() - max) / ch;
            self.take(n);
            self.dropped += n as u64;
        }
        self.last_push = Instant::now();
    }
}

/// The test hooks: ZD1_PCM_HASH[=N] logs an FNV-1a hash over the (first N) samples of each stream, ZD1_PCM_DUMP=FILE
/// writes them, raw little-endian; either works alone.
struct Tap {
    hash: Option<u64>,
    dump: Option<std::fs::File>,
    n: u64,     // samples taken in
    limit: u64, // ZD1_PCM_HASH=N, else all
}

const FNV_BASIS: u64 = 0xcbf29ce484222325;

impl Tap {
    fn from_env() -> Tap {
        let hash = std::env::var("ZD1_PCM_HASH").ok();
        Tap {
            limit: hash.as_deref().and_then(|v| v.parse().ok()).unwrap_or(u64::MAX),
            hash: hash.map(|_| FNV_BASIS),
            dump: std::env::var_os("ZD1_PCM_DUMP").and_then(|p| std::fs::File::create(p).ok()),
            n: 0,
        }
    }

    fn add<T: Sample>(&mut self, samples: &[T]) {
        if self.hash.is_none() && self.dump.is_none() {
            return;
        }
        let samples = &samples[..samples.len().min(self.limit.saturating_sub(self.n) as usize)];
        if let Some(h) = self.hash.as_mut() {
            for &v in samples {
                *h = (*h ^ v.bits()).wrapping_mul(0x100000001b3);
            }
        }
        if let Some(f) = self.dump.as_mut() {
            use std::io::Write;
            let mut bytes = Vec::new();
            samples.iter().for_each(|v| v.le(&mut bytes));
            let _ = f.write_all(&bytes);
        }
        self.n += samples.len() as u64;
    }

    /// A new stream: the hash starts again (the dump goes on).
    fn restart(&mut self) {
        if let Some(h) = self.hash.as_mut() {
            *h = FNV_BASIS;
            self.n = 0;
        }
    }

    fn report(&self) {
        if let Some(h) = self.hash {
            tracing::info!(samples = self.n, hash = format!("{h:016x}"), "stream PCM");
        }
    }
}

impl Output {
    pub fn start(device: String, speaker: Speaker) -> Self {
        let out = Output {
            inner: Arc::new((
                Mutex::new(State {
                    queue: Queue::new(16),
                    rate: 44100,
                    channels: 2,
                    active: false,
                    last_push: Instant::now(),
                    dropped: 0,
                    tap: Tap::from_env(),
                    pushed: 0,
                    taken: 0,
                    marks: VecDeque::new(),
                    flushes: 0,
                    stream: 0,
                }),
                Condvar::new(),
            )),
        };
        let o2 = out.clone();
        std::thread::Builder::new()
            .name("alsa".into())
            .stack_size(64 * 1024)
            .spawn(move || o2.run(device, speaker))
            .expect("output thread");
        out
    }

    /// A stream (re)starts: its rate, channel count and sample width (16, or 32 for a deeper source: what the queue
    /// holds); whatever was queued is dropped (with its memory). A stream the output can't play (a rate outside
    /// RATES, more than two channels) is refused: its samples go nowhere.
    pub fn start_stream(&self, rate: u32, channels: u8, bits: u8) -> Stream {
        let (lock, cvar) = &*self.inner;
        let mut s = lock.lock().unwrap();
        s.stream += 1;
        s.queue = Queue::new(bits);
        s.tap.restart();
        let ok = RATES.contains(&rate) && (1..=2).contains(&channels);
        if !ok {
            tracing::warn!(rate, channels, "a stream the output can't play: refused");
        }
        s.rate = if ok { rate } else { 44100 };
        s.channels = if ok { channels } else { 2 };
        s.active = ok;
        (s.pushed, s.taken) = (0, 0);
        s.marks.clear();
        s.last_push = Instant::now();
        cvar.notify_all();
        Stream(s.stream)
    }

    /// Samples of `stream` (16-bit, or 32-bit with the sample in the top bits); with `play_at`, the local monotonic
    /// ns its first frame should leave the speaker (a timed stream).
    pub fn push<T: Sample>(&self, stream: Stream, samples: &[T], play_at: Option<u64>) {
        let (lock, cvar) = &*self.inner;
        let mut s = lock.lock().unwrap();
        if !s.active || s.stream != stream.0 {
            return;
        }
        let mut faded = Vec::new();
        let samples = match play_at {
            None => samples,
            Some(t) => {
                let samples = s.fill_gap(samples, t, &mut faded);
                if samples.is_empty() {
                    return;
                }
                let f = s.pushed;
                s.marks.push_back((f, t));
                samples
            }
        };
        s.append(samples);
        cvar.notify_all();
    }

    /// Samples from a producer that runs ahead of real time (a decoder pulling from a file: zd1-spotify): waits
    /// while more than `ahead_ms` is queued, then pushes. Doesn't wait while another source has the speaker (the
    /// samples are dropped: the producer must stay free to hear its sender pause). false: the stream stopped or
    /// another source has the speaker.
    pub fn push_paced(&self, stream: Stream, samples: &[i16], ahead_ms: usize, speaker: &Speaker) -> bool {
        let (lock, cvar) = &*self.inner;
        let mut s = lock.lock().unwrap();
        loop {
            if !s.active || s.stream != stream.0 || speaker.yielded() {
                return false;
            }
            let max = ahead_ms * s.rate as usize / 1000 * s.channels as usize;
            if s.queue.len() + samples.len() <= max.max(samples.len()) {
                break;
            }
            s = cvar.wait_timeout(s, Duration::from_millis(50)).unwrap().0;
        }
        s.append(samples);
        cvar.notify_all();
        true
    }

    /// What is queued of `stream` goes (a pause, a seek).
    pub fn flush(&self, stream: Stream) {
        let mut s = self.inner.0.lock().unwrap();
        if s.stream == stream.0 {
            s.queue.clear();
            s.taken = s.pushed;
            s.marks.clear();
            s.flushes += 1;
        }
    }

    /// Wait (up to `timeout`) until what is queued has gone to the device.
    pub fn drain(&self, timeout: Duration) {
        let (lock, cvar) = &*self.inner;
        let t = Instant::now();
        let mut s = lock.lock().unwrap();
        while s.active && !s.queue.is_empty() && t.elapsed() < timeout {
            s = cvar.wait_timeout(s, Duration::from_millis(20)).unwrap().0;
        }
    }

    /// The stream ends (its sender is gone).
    pub fn stop(&self) {
        let (lock, cvar) = &*self.inner;
        let mut s = lock.lock().unwrap();
        s.tap.report();
        s.active = false;
        s.queue.clear();
        cvar.notify_all();
    }

    fn run(&self, device: String, speaker: Speaker) {
        loop {
            let (rate, channels, flushes) = self.wait_prebuffer(&speaker);
            speaker.state("playing");
            if device == "null" {
                self.run_null(rate, channels, &speaker);
            } else if let Some(mut p) = Playback::open(&device, rate, channels, flushes) {
                while p.step(self, &speaker) {}
                p.close(self);
            } else {
                self.inner.0.lock().unwrap().queue.clear();
            }
            speaker.state("stopped");
        }
    }

    /// A stream with a prebuffer's worth of audio, while no other source has the speaker: its rate, channels and
    /// flush count.
    fn wait_prebuffer(&self, speaker: &Speaker) -> (u32, u8, u64) {
        let (lock, cvar) = &*self.inner;
        let mut s = lock.lock().unwrap();
        loop {
            let need = PREBUFFER_MS * s.rate as usize / 1000 * s.channels as usize;
            if s.active && s.queue.len() >= need && !speaker.yielded() {
                return (s.rate, s.channels, s.flushes);
            }
            s = cvar.wait_timeout(s, Duration::from_millis(200)).unwrap().0;
        }
    }

    /// The "null" device (measurements): takes the frames due by the clock, every 20 ms, and drops them.
    fn run_null(&self, rate: u32, channels: u8, speaker: &Speaker) {
        let (lock, _) = &*self.inner;
        let t0 = Instant::now();
        let mut taken = 0u64;
        loop {
            std::thread::sleep(Duration::from_millis(20));
            let mut s = lock.lock().unwrap();
            if !s.active || s.last_push.elapsed() > IDLE || speaker.yielded() {
                break;
            }
            let due = (t0.elapsed().as_secs_f64() * rate as f64) as u64;
            let n = ((due - taken) as usize * channels as usize).min(s.queue.len());
            s.queue.drain(n);
            taken = due;
        }
        tracing::info!(seconds = taken as f64 / rate as f64, "null output closed");
    }
}

/// A timed stream's steering towards its play times.
struct Steer {
    ema: Option<i64>, // the smoothed error (ns); None: aligning
    slewing: bool,
    frame_ns: i64,
    offset_ns: i64, // OUTPUT_OFFSET_US
}

/// A timed stream's figures over the last 10 s: the error (sum, count, min, max), frames dropped and repeated
/// (slewing), jumps.
struct Timing {
    sum: i64,
    n: i64,
    min: i64,
    max: i64,
    dropped: u32,
    repeated: u32,
    jumps: u32,
}

impl Default for Timing {
    fn default() -> Self {
        Timing { sum: 0, n: 0, min: i64::MAX, max: i64::MIN, dropped: 0, repeated: 0, jumps: 0 }
    }
}

/// What went wrong over the last 10 s, so dropouts can be told apart (silence = ran dry, dropped = overflow, xruns =
/// ALSA), and the queue's low water mark (the network's jitter eats into it).
struct Interval {
    since: Instant,
    xruns: u32,
    silence: u64,
    low: usize, // frames
}

/// One open of the device while a stream plays: what the output thread keeps from write to write.
struct Playback {
    dev: Device,
    channels: u8,
    buf: Vec<i32>,
    opened: Instant,
    prebuffer_ms: usize,
    flushes: u64,      // the flushes when the queue last ran dry: running dry after one is not the network's
    flushes_seen: u64, // the flushes the steering has seen: one starts it over
    timed: bool,       // the stream has play times (from its first one on)
    steer: Steer,
    fade_in: bool, // the next audio written follows silence (or a cut: a flush)
    audible: bool, // audio went to the device since the last flush's fade
    // what the device was given last, as much as its buffer holds: to fade out what a flush drops
    recent: VecDeque<i32>,
    xruns: u32,
    written: u64, // frames of the stream played
    silence: u64, // frames of silence written where the queue ran dry
    peak: i32,
    interval: Interval,
    timing: Timing,
}

impl Playback {
    fn open(device: &str, rate: u32, channels: u8, flushes: u64) -> Option<Playback> {
        let dev = Device::open_retry(device, rate, PERIODS, true)?;
        tracing::info!(device, rate, channels, "output open");
        let offset_us =
            std::env::var("ZD1_OUTPUT_DELAY_US").ok().and_then(|v| v.parse().ok()).unwrap_or(OUTPUT_OFFSET_US);
        Some(Playback {
            dev,
            channels,
            buf: vec![0i32; CHUNK * 2],
            opened: Instant::now(),
            prebuffer_ms: PREBUFFER_MS,
            flushes,
            flushes_seen: flushes,
            timed: false,
            steer: Steer {
                ema: None,
                slewing: false,
                frame_ns: 1_000_000_000 / rate as i64,
                offset_ns: offset_us * 1000,
            },
            fade_in: true,
            audible: false,
            recent: VecDeque::with_capacity(PERIOD * PERIODS * 2),
            xruns: 0,
            written: 0,
            silence: 0,
            peak: 0,
            interval: Interval { since: Instant::now(), xruns: 0, silence: 0, low: usize::MAX },
            timing: Timing::default(),
        })
    }

    /// One write. false: the stream stopped, another source has the speaker, or the device must be opened again.
    fn step(&mut self, out: &Output, speaker: &Speaker) -> bool {
        let (lock, cvar) = &*out.inner;
        let (mut delay, now) = (self.dev.delay(), mono_ns());
        let (frames, starved) = {
            let mut s = lock.lock().unwrap();
            let idle = s.last_push.elapsed() > IDLE;
            if !s.active || idle || speaker.yielded() || s.rate != self.dev.rate || s.channels != self.channels {
                return false;
            }
            if s.flushes != self.flushes_seen {
                // the new frames' times are new: align on the first measurement again instead of steering towards
                // them for seconds
                self.flushes_seen = s.flushes;
                self.steer.ema = None;
                self.fade_in = true;
                if delay.is_some() && std::mem::take(&mut self.audible) && self.fade_out_queued() {
                    delay = None; // read before the device's queue changed
                }
            }
            let (lead_silence, slip) = self.steer(&mut s, now, delay);
            self.interval.low = self.interval.low.min(s.frames());
            let r = self.fill(&mut s, lead_silence, slip);
            cvar.notify_all(); // room for a paced producer (push_paced)
            r
        };
        if self.interval.since.elapsed() >= Duration::from_secs(10) {
            self.report(lock.lock().unwrap().dropped);
        }
        match self.dev.write(&self.buf[..frames * 2]) {
            Ok(Some(n)) => {
                self.recent.extend(&self.buf[..n * 2]);
                let over = self.recent.len().saturating_sub(PERIOD * PERIODS * 2);
                self.recent.drain(..over);
            }
            Ok(None) => self.xruns += 1,
            Err(e) => {
                self.xruns += 1;
                tracing::warn!(%e, "ALSA: reopening");
                return false;
            }
        }
        if starved && !self.timed {
            self.deeper_prebuffer(out);
        }
        true
    }

    /// After a flush: the old audio the device still holds goes (a seek is heard at once), but not as a cut (that
    /// thumps): ALSA's queue past the hardware's pointer is dropped (`reset`, the device keeps running) and written
    /// again up to a fade. snd-sulu reports period P as the pointer and copies P into its SRAM packet at the same
    /// interrupt (docs/module/linux.md), so P plays as it was whatever is written over it: the fade goes into P + 1
    /// (written into P it never played, and the old audio stopped dead at P's end). Whether it did.
    fn fade_out_queued(&mut self) -> bool {
        let pcm = &self.dev.pcm;
        let Ok(d) = pcm.delay() else { return false };
        let d = d.max(0) as usize;
        if d < PERIOD + FADE || d > self.recent.len() / 2 || pcm.reset().is_err() {
            return false;
        }
        // what ALSA's queue held: its delay less what it still counts after the reset
        let after = pcm.delay().unwrap_or(0).max(0) as usize;
        let d = d - after.min(d);
        let from = self.recent.len() / 2 - d; // the frame at the hardware's pointer: P's first
        let n = (PERIOD + FADE).min(d);
        let mut tail: Vec<i32> = self.recent.range(from * 2..(from + n) * 2).copied().collect();
        if n > PERIOD {
            ramp(&mut tail, PERIOD, n - PERIOD, false);
        }
        tail.resize(CHUNK * 2, 0); // (a whole write: a shorter tail ran the driver dry)
        self.recent.truncate(from * 2);
        // the device plays on, so its measurement holds: the stream goes on from the pointer
        self.dev.frames -= d as u64;
        tracing::debug!(dropped = d, after, "flush: the device's queue faded out");
        if let Ok(Some(n)) = self.dev.write(&tail) {
            self.recent.extend(&tail[..n * 2]);
        }
        true
    }

    /// A timed stream: where the next frame lands against where it belongs, and what to do about it: frames of
    /// silence first (early, or not measured yet), or a frame more or fewer in this write (`slip`); a late stream
    /// is skipped ahead here.
    fn steer(&mut self, s: &mut State, now: u64, delay: Option<i64>) -> (usize, i64) {
        let (mut lead_silence, mut slip) = (0, 0i64);
        let target = s.target(s.taken);
        self.timed |= target.is_some();
        let start = self.dev.start_ns(now, delay);
        let st = &mut self.steer;
        if let (Some(target), Some(start)) = (target, start) {
            let e = start + frames_ns(self.dev.frames, self.dev.rate) + st.offset_ns - target as i64;
            let t = &mut self.timing;
            (t.sum, t.n, t.min, t.max) = (t.sum + e, t.n + 1, t.min.min(e), t.max.max(e));
            let m = match st.ema {
                None => e, // aligning: take this one as it is
                Some(m) => m + (e - m) / 16,
            };
            if (st.ema.is_none() && e.abs() > STEER_NS) || m.abs() > JUMP_NS {
                // by the error itself: the smoothed one lags (skipping by it caught up in dozens of small cuts)
                if e > 0 {
                    // late: skip ahead; if that much isn't queued yet, drop what is and keep aligning (silence
                    // meanwhile); either way the audio starts again faded in
                    s.take((e / st.frame_ns) as usize);
                } else {
                    // early: silence first (one write's worth at a time)
                    lead_silence = ((-e / st.frame_ns) as usize).min(CHUNK);
                }
                // aligning until a measurement after it is within STEER_NS: what's left then goes at once
                st.ema = None;
                st.slewing = false;
                self.fade_in = true;
                t.jumps += 1;
            } else {
                // slew from STEER_NS off until within SETTLE_NS: one frame per write, the next one's worth of frames
                // played as one more (late) or one fewer (early)
                st.slewing = if st.slewing { m.abs() > SETTLE_NS } else { m.abs() > STEER_NS };
                if st.slewing && s.frames() >= CHUNK + 2 {
                    slip = if m > 0 { 1 } else { -1 };
                    if slip > 0 { t.dropped += 1 } else { t.repeated += 1 }
                }
                st.ema = Some(m - slip * st.frame_ns);
            }
        }
        if start.is_none() && target.is_some() {
            // no measurement yet (the device just opened, restarted after an xrun, or still fills after a flush):
            // silence until there is one, so the audio goes out aligned
            lead_silence = CHUNK;
            self.fade_in = true;
        }
        (lead_silence, slip)
    }

    /// This write's frames into `buf`: `lead_silence` frames of silence, or a write's worth from the queue (`slip`
    /// one more or fewer of the queue's in it), faded where the queue starts or runs dry. The frames to write, and
    /// whether the queue ran dry.
    fn fill(&mut self, s: &mut State, lead_silence: usize, slip: i64) -> (usize, bool) {
        if lead_silence > 0 {
            self.buf[..lead_silence * 2].fill(0);
            self.fade_in = true;
            return (lead_silence, false);
        }
        let buf = &mut self.buf;
        let frames = if slip != 0 {
            s.queue.fill_slip(buf, CHUNK, slip, self.channels)
        } else {
            let frames = s.frames().min(CHUNK);
            s.queue.fill(buf, frames, self.channels);
            if self.fade_in && frames > 0 {
                ramp(buf, 0, frames.min(FADE), true);
                self.fade_in = false;
            }
            if frames < CHUNK {
                // ran dry (a flush, the network): down to the silence that follows, not a cut
                ramp(buf, frames - frames.min(FADE), frames.min(FADE), false);
                self.fade_in = true;
            }
            frames
        };
        self.audible |= frames > 0;
        s.advance(frames);
        self.written += frames as u64;
        self.peak = buf.iter().fold(self.peak, |p, &v| p.max((v >> 16).abs()));
        if slip == 0 {
            self.silence += (CHUNK - frames) as u64;
        }
        (CHUNK, slip == 0 && frames < CHUNK)
    }

    /// The queue ran dry: wait for a prebuffer again (silence went out meanwhile), a deeper one if the stream was
    /// under way (not its first seconds, when the sender is still filling). Not a timed stream: it writes silence on
    /// (the device keeps running, so its audio starts again measured and on time, with no xrun), and its frames come
    /// a set time ahead of when they play, so a deeper prebuffer couldn't fill before they are due.
    fn deeper_prebuffer(&mut self, out: &Output) {
        let (lock, cvar) = &*out.inner;
        let mut s = lock.lock().unwrap();
        let flushed = s.flushes != self.flushes;
        self.flushes = s.flushes;
        if !flushed && self.opened.elapsed() > Duration::from_secs(2) && self.prebuffer_ms < MAX_PREBUFFER_MS {
            self.prebuffer_ms += PREBUFFER_STEP_MS;
            tracing::info!(prebuffer_ms = self.prebuffer_ms, "output ran dry: deeper prebuffer");
        }
        let need = self.prebuffer_ms * self.dev.rate as usize / 1000 * self.channels as usize;
        let t = Instant::now();
        while s.active && s.queue.len() < need && t.elapsed() < IDLE {
            s = cvar.wait_timeout(s, Duration::from_millis(50)).unwrap().0;
        }
    }

    /// The last 10 s in the log.
    fn report(&mut self, dropped: u64) {
        let i = &self.interval;
        tracing::info!(
            xruns = self.xruns - i.xruns,
            silence_frames = self.silence - i.silence,
            dropped,
            low_ms = i.low as u64 * 1000 / self.dev.rate as u64,
            "output 10 s"
        );
        let t = &self.timing;
        if t.n > 0 {
            tracing::info!(
                err_avg_us = t.sum / t.n / 1000,
                err_min_us = t.min / 1000,
                err_max_us = t.max / 1000,
                dropped = t.dropped,
                repeated = t.repeated,
                jumps = t.jumps,
                "timing 10 s"
            );
        }
        self.timing = Timing::default();
        self.interval = Interval { since: Instant::now(), xruns: self.xruns, silence: self.silence, low: usize::MAX };
    }

    fn close(self, out: &Output) {
        let dropped = std::mem::take(&mut out.inner.0.lock().unwrap().dropped);
        tracing::info!(
            xruns = self.xruns,
            dropped,
            seconds = self.written as f64 / self.dev.rate as f64,
            silence_frames = self.silence,
            peak = self.peak,
            "output closed"
        );
    }
}

/// Frames `from`..`from + n` of `buf` (stereo) faded in (`up`) or out, linearly.
fn ramp(buf: &mut [i32], from: usize, n: usize, up: bool) {
    for i in 0..n {
        let g = if up { i + 1 } else { n - i } as i64;
        for v in &mut buf[(from + i) * 2..(from + i) * 2 + 2] {
            *v = (*v as i64 * g / n as i64) as i32;
        }
    }
}
