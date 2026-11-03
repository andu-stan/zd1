//! The output: zd1-audio's device at the port's 44.1 kHz. Each frame is played when the server's clock says
//! `chunk timestamp + buffer` (buffer = the server's bufferMs - this client's latency): the frame written now is heard
//! when the device's measurement says (`Device::start_ns`: the least of the recent now + ALSA's delay readings, which
//! a late wake-up only ever makes later), so the server time due then is that + diff - buffer. Once aligned it reads
//! straight on; the error is averaged (~0.4 s) and corrected beyond 2 ms by dropping or repeating one frame per
//! period; a big error (> 30 ms) re-aligns.
//! During a session it always writes (silence when idle), so the port and ALSA's delay stay steady, like KnOS's
//! always-on stream. Without a server (no clock) for RELEASE_US it closes the device: other apps (aplay) can use it
//! and the audio DMA stops; a session opens it (at start-up too).
//! The playout delay is `Shared::delay_us` (the server's buffer and this client's margin, lib.rs). When the queue runs
//! dry in the middle of a stream the place is kept (`Stall`): if the next chunk is the continuation, the margin grows
//! by the time it was late and play goes on from there (adaptive), else it aligns afresh, dropping what came too late.

use super::{CONTINUATION_US, IDLE_US, OUT_RATE, Shared, Stall, now_us};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use zd1_audio::device::{Device, PERIOD, frames_ns, mono_ns};

const PERIODS: usize = 16; // ALSA's buffer: 93 ms
const REALIGN_US: i64 = 30_000;
const RELEASE_US: i64 = 5_000_000;

/// Where the next frame taken from the queue sits on the server's timeline: a chunk timestamp plus whole frames,
/// so the time is exact (adding a period's 5804.988 us as integer microseconds lost 170 ppm, which the drift
/// correction then made up by dropping frames).
#[derive(Clone, Copy)]
struct Cursor {
    base: i64,
    frames: u64,
}

impl Cursor {
    fn at(&self) -> i64 {
        self.base + (self.frames * 1_000_000 / OUT_RATE as u64) as i64
    }
}
const DEADBAND_US: f64 = 2_000.0;
const STALE_WAIT_US: i64 = 100_000; // how long a lone continuing chunk waits for the next to tell a stall

pub fn run(device: String, shared: Arc<Mutex<Shared>>) {
    let mut buf = vec![0i32; PERIOD * 2];
    loop {
        // open the device only for a server (its clock is known once a session runs), and not while another source
        // has the speaker
        while {
            let s = shared.lock().unwrap();
            s.diff.is_none() || s.speaker.yielded()
        } {
            std::thread::sleep(Duration::from_millis(100));
        }
        let mut dev = match Device::open(&device, OUT_RATE, PERIODS, false) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(device, %e, "can't open the output");
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
        };
        tracing::info!(device, rate = OUT_RATE, "output open");
        let mut cursor: Option<Cursor> = None; // the next frame we take (None = not aligned)
        let mut err = 0.0f64; // averaged cursor - due (us)
        let mut serverless_since = None;
        loop {
            let heard_at = heard_at(&mut dev);
            {
                let mut s = shared.lock().unwrap();
                fill(&mut s, &mut buf, heard_at, &mut cursor, &mut err);
                if s.speaker.yielded() {
                    tracing::info!("output closed for the other source");
                    break;
                }
                if s.diff.is_some() {
                    serverless_since = None;
                } else if now_us() - *serverless_since.get_or_insert(now_us()) > RELEASE_US {
                    tracing::info!("no server: output closed");
                    break;
                }
            }
            match dev.write(&buf) {
                Ok(Some(_)) => {}
                Ok(None) => cursor = None, // an xrun: align again
                Err(e) => {
                    tracing::warn!(%e, "ALSA: reopening");
                    break;
                }
            }
        }
    }
}

/// When the next frame written will be heard (client us): from the device's measurement while it runs, else (before
/// its first write starts it) now + what it holds.
fn heard_at(dev: &mut Device) -> i64 {
    let (now, delay) = (mono_ns(), dev.delay());
    let ns = match dev.start_ns(now, delay) {
        Some(start) => start + frames_ns(dev.frames, OUT_RATE),
        None => now as i64 + frames_ns(dev.pcm.delay().unwrap_or(0).max(0) as u64, OUT_RATE),
    };
    ns / 1000
}

/// One period: the frames due at `heard_at` (client time), or silence.
fn fill(s: &mut Shared, buf: &mut [i32], heard_at: i64, cursor: &mut Option<Cursor>, err: &mut f64) {
    buf.fill(0);
    let frame_us = 1_000_000.0 / OUT_RATE as f64;
    let Some(diff) = s.diff else { return };
    let mut due = heard_at + diff - s.delay_us(); // server time that should be heard
    if let Some(st) = s.stall
        && let Some(c) = s.queue.front()
    {
        if s.margin.adaptive && (c.ts_us - st.at).abs() < CONTINUATION_US {
            // the stream continues where it ran dry: the network's (the next chunk continues too: grow and go on
            // from there) or snapserver's stale chunk after a break in its input (the next jumps ahead: drop it)
            let (end, arrived) = (c.end_us(), c.arrived_us);
            match s.queue.get(1).map(|n| (n.ts_us - end).abs() < CONTINUATION_US) {
                Some(true) => {
                    s.margin.grow(heard_at - st.since);
                    due = heard_at + diff - s.delay_us();
                }
                Some(false) => {
                    s.queue.pop_front();
                    s.pos = 0;
                }
                None if now_us() - arrived < STALE_WAIT_US => return, // silence until the next chunk tells
                None => {}
            }
        }
        s.stall = None;
    }
    // nothing older than what is due now is ever needed
    while let Some(c) = s.queue.front() {
        if c.ts_us + (c.frames.len() / 2) as i64 * 1_000_000 / OUT_RATE as i64 <= due - REALIGN_US {
            s.queue.pop_front();
            s.pos = 0;
        } else {
            break;
        }
    }
    if let Some(c) = *cursor
        && (c.at() - due).abs() > REALIGN_US
    {
        s.realigns += 1;
        tracing::info!(by_ms = (due - c.at()) / 1000, realigns = s.realigns, "realigned");
        *cursor = None;
    }
    if cursor.is_none() {
        let Some(c) = s.queue.front() else { return };
        if c.ts_us > due {
            // the stream starts later: silence until then
            let gap = ((c.ts_us - due) as f64 / frame_us) as usize;
            if gap >= buf.len() / 2 {
                return;
            }
            s.pos = 0;
            *cursor = Some(Cursor { base: c.ts_us, frames: 0 }); // the chunk's first frame, after `gap` silence
            take(s, &mut buf[gap * 2..], cursor, heard_at + (gap as f64 * frame_us) as i64);
            s.out_audible |= buf.iter().any(|&v| v != 0);
            if !s.audible() {
                buf.fill(0);
            }
            return;
        }
        let skip = ((due - c.ts_us) as f64 / frame_us) as usize;
        s.pos = (skip * 2).min(c.frames.len());
        *cursor = Some(Cursor { base: c.ts_us, frames: (s.pos / 2) as u64 });
        *err = (cursor.unwrap().at() - due) as f64;
    }
    // drift: one frame per period towards `due`, when the averaged error says so
    *err += ((cursor.unwrap().at() - due) as f64 - *err) / 64.0;
    if *err > DEADBAND_US {
        // we would play early: repeat the first frame
        *err -= frame_us;
        s.corrections += 1;
        buf[0] = s.peek(0);
        buf[1] = s.peek(1);
        take(s, &mut buf[2..], cursor, heard_at);
    } else if *err < -DEADBAND_US {
        // late: drop one frame
        *err += frame_us;
        s.corrections += 1;
        let mut junk = [0i32; 2];
        take(s, &mut junk, cursor, heard_at);
        take(s, buf, cursor, heard_at);
    } else {
        take(s, buf, cursor, heard_at);
    }
    s.out_audible |= buf.iter().any(|&v| v != 0);
    if !s.audible() {
        buf.fill(0);
    }
}

/// Copy frames from the queue into `out` (heard from `heard_at` on), advancing the cursor; silence where the queue
/// runs dry. Dry in the middle of a stream (sound came within IDLE_US): the place is kept as a `Stall`.
fn take(s: &mut Shared, out: &mut [i32], cursor: &mut Option<Cursor>, heard_at: i64) {
    let mut i = 0;
    while i < out.len() {
        let Some(c) = s.queue.front() else { break };
        let n = (c.frames.len() - s.pos).min(out.len() - i);
        out[i..i + n].copy_from_slice(&c.frames[s.pos..s.pos + n]);
        i += n;
        s.pos += n;
        if s.pos >= c.frames.len() {
            s.queue.pop_front();
            s.pos = 0;
        }
    }
    if let Some(c) = cursor.as_mut() {
        c.frames += (i / 2) as u64;
    }
    if i < out.len() {
        // ran dry: align again on the next chunk
        if let Some(c) = *cursor
            && s.stall.is_none()
            && now_us() - s.last_sound_us < IDLE_US
        {
            s.stall = Some(Stall { at: c.at(), since: heard_at + (i / 2) as i64 * 1_000_000 / OUT_RATE as i64 });
        }
        *cursor = None;
    }
}
