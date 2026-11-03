//! zd1-snapcast: a Snapcast client for the module OS. Connects to a snapserver (TCP 1704), keeps the server's clock,
//! decodes the stream (FLAC, PCM) and plays it in sync through ALSA (zd1-audio's device: snd-sulu, AV port 2 -> the
//! speaker's DSP); the speaker drives it through zd1-linkd (zd1-audio's speaker link, `on_speaker`).
//!
//!   zd1-snapcast [--host HOST] [--port 1704] [--rpc-port PORT+1] [--device hw:0,0] [--latency MS] [--name NAME]
//!                [--fixed-delay] [--no-warp0] [-v]
//!
//! Without --host it looks for a server on the network (mDNS `_snapcast._tcp`, zd1-mdns, asked on every interface),
//! again after each session; one on the wired link's subnet comes first.
//!
//! Playout delay: the server's buffer (minus this client's latency) is the floor; on top of it the client keeps a
//! margin of its own for what its network needs (`Margin`: none on the wired link, WIFI_BASE_US on Wi-Fi to start
//! with). When the queue runs dry in the middle of a stream and the missing audio then arrives (a network stall),
//! the margin grows by the stall and SAFETY_US and the stream goes on where it stopped: no audio is lost, there is
//! one gap, and the next stall of that size is absorbed. Not every late chunk is the network's: after a break in its
//! input snapserver stamps the first chunk as if there had been none, then jumps its clock to now (`resync`); that
//! chunk is late by the break, the next one jumps ahead. So a chunk counts as late only if the next one continues
//! the timeline, and a stall grows the margin only then; the stale chunk is dropped. Once the stream is idle the
//! margin is set again from the latest arrival (relative to the floor) of the last HISTORY_S, so it comes down only
//! while nothing plays. While the margin is above zero the speaker plays that much later than other clients of the
//! server; --fixed-delay keeps it at zero (multi-room).
//!
//! Threads: this one reads the server; a decoder (`decoding`) turns its chunks into queued frames; player.rs writes
//! ALSA; the speaker link's thread brings the speaker's keys and volume; a timer sends the time requests and the
//! speaker's volume to the server.
//!
//! FLAC decodes on Warp0 (module/warp0, the FLAC image) while a stream has sound, the same decoder's ARM build
//! (zd1-flac) otherwise and whenever Warp0 is taken (AirPlay's and Spotify's decoders use it too): it is taken at the
//! first sound and let go after IDLE_US without. --no-warp0 keeps FLAC on the ARM (for comparisons).

mod decode;
mod player;
mod proto;
mod rpc;
mod timesync;

use std::collections::VecDeque;
use std::io::BufReader;
use std::net::{Ipv4Addr, TcpStream};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use zd1_audio::{Args, Event, Speaker};

pub const OUT_RATE: u32 = 44100; // AV port 2: I2S at 512 x 44.1 kHz into the speaker's DSP
const IDLE_US: i64 = 3_000_000; // this long without sound = stopped
const SAFETY_US: i64 = 150_000; // margin beyond the latest arrival seen
const MAX_MARGIN_US: i64 = 5_000_000;
const WIFI_BASE_US: i64 = 300_000; // the margin to start with on Wi-Fi (the wired link: 0)
const HISTORY_S: i64 = 600; // arrivals remembered for setting the margin
const MAX_BUFFER_MS: i64 = 5000; // the most of the server's bufferMs taken (the queue holds the delay and 3 s more)
pub const CONTINUATION_US: i64 = 50_000; // a chunk this close to where the last one ended continues the stream
pub const SOURCE: &str = "snapcast"; // our name to zd1-linkd (`state playing snapcast`, `yield <other>`)

const USAGE: &str = "zd1-snapcast [--host HOST] [--port 1704] [--rpc-port N] [--device hw:0,0] [--latency MS] \
                     [--name NAME] [--fixed-delay] [--no-warp0] [-v]";

/// The client's clock: CLOCK_MONOTONIC in us, the device's (zd1-audio's `mono_ns`).
pub fn now_us() -> i64 {
    (zd1_audio::device::mono_ns() / 1000) as i64
}

pub struct Chunk {
    pub ts_us: i64,
    pub frames: Vec<i32>,
    pub arrived_us: i64,
}

impl Chunk {
    pub fn end_us(&self) -> i64 {
        self.ts_us + (self.frames.len() / 2) as i64 * 1_000_000 / OUT_RATE as i64
    }
}

/// The queue ran dry in the middle of a stream: `at` = the server time of the next frame due, `since` = when (client
/// time) it should have been heard.
#[derive(Clone, Copy)]
pub struct Stall {
    pub at: i64,
    pub since: i64,
}

/// The playout delay's margin over the server's buffer, for what this client's network needs.
pub struct Margin {
    pub adaptive: bool, // it follows the network (not --fixed-delay)
    pub us: i64,        // played this much later than the server's buffer says
    pub base_us: i64,   // the least on this session's link
    pub stalls: u64,
    lateness: VecDeque<(i64, i64)>, // (second, the latest arrival in it, relative to the buffer's floor)
}

impl Margin {
    pub fn new(adaptive: bool) -> Margin {
        Margin { adaptive, us: 0, base_us: 0, stalls: 0, lateness: VecDeque::new() }
    }

    /// A session starts, on the wired link or not: the link's base, the arrivals forgotten.
    pub fn start(&mut self, wired: bool) {
        self.base_us = if wired || !self.adaptive { 0 } else { WIFI_BASE_US };
        self.us = self.base_us;
        self.lateness.clear();
    }

    /// How long after its timestamp a frame is heard (us), with the server's buffer and this client's latency.
    pub fn delay_us(&self, buffer_ms: i64, latency_ms: i64) -> i64 {
        (buffer_ms - latency_ms) * 1000 + self.us
    }

    /// A chunk arrived `late_us` after the time it was due at the floor (negative: early).
    pub fn note_arrival(&mut self, now: i64, late_us: i64) {
        let sec = now / 1_000_000;
        match self.lateness.back_mut() {
            Some(b) if b.0 == sec => b.1 = b.1.max(late_us),
            _ => self.lateness.push_back((sec, late_us)),
        }
        while self.lateness.front().is_some_and(|f| sec - f.0 > HISTORY_S) {
            self.lateness.pop_front();
        }
    }

    /// A stall of `stalled_us`: the margin grows by it (and SAFETY_US), up to MAX_MARGIN_US.
    pub fn grow(&mut self, stalled_us: i64) {
        self.stalls += 1;
        let m = (self.us + stalled_us + SAFETY_US).min(MAX_MARGIN_US);
        tracing::info!(
            stall_ms = stalled_us / 1000,
            later_ms = (m - self.us) / 1000,
            margin_ms = m / 1000,
            stalls = self.stalls,
            "the stream stalled: playing later from now on"
        );
        self.us = m;
    }

    /// While nothing plays: the margin for the latest arrival of the last HISTORY_S, at least the link's base.
    pub fn settle(&mut self) {
        if !self.adaptive {
            return;
        }
        let latest = self.lateness.iter().map(|l| l.1).max().unwrap_or(i64::MIN);
        let m = latest.saturating_add(SAFETY_US).clamp(self.base_us, MAX_MARGIN_US);
        if m != self.us {
            tracing::info!(
                from_ms = self.us / 1000,
                to_ms = m / 1000,
                latest_ms = latest.max(0) / 1000,
                minutes = HISTORY_S / 60,
                "margin set from the latest arrival past the buffer"
            );
            self.us = m;
        }
    }
}

/// State shared by the threads.
pub struct Shared {
    pub queue: VecDeque<Chunk>, // decoded, at OUT_RATE, stereo; oldest first
    pub pos: usize,             // read position in the front chunk (samples)
    pub diff: Option<i64>,      // server - client clock
    pub buffer_ms: i64,
    pub latency_ms: i64,
    pub muted: bool,
    pub volume: Option<u32>, // the server's and the speaker's, last agreed
    pub paused: bool,        // the speaker's play/pause key
    pub out_audible: bool,   // the last period carried sound (player)
    pub corrections: u64,    // frames dropped or repeated for sync (player)
    pub realigns: u64,
    pub last_sound_us: i64, // client time of the last sound that arrived
    pub state: &'static str,
    pub server: Option<(String, u16)>, // the snapserver host and control port of the session (keys -> rpc.rs)
    pub speaker: Speaker,
    pub margin: Margin,
    pub stall: Option<Stall>, // the queue ran dry mid-stream (player)
    pub wired: bool,          // the session's server is on the wired link
}

impl Shared {
    pub fn new(speaker: Speaker, latency_ms: i64, adaptive: bool) -> Shared {
        Shared {
            queue: VecDeque::new(),
            pos: 0,
            diff: None,
            buffer_ms: 1000,
            latency_ms,
            muted: false,
            volume: None,
            paused: false,
            out_audible: false,
            corrections: 0,
            realigns: 0,
            last_sound_us: -IDLE_US * 2,
            state: "stopped",
            server: None,
            speaker,
            margin: Margin::new(adaptive),
            stall: None,
            wired: false,
        }
    }

    pub fn audible(&self) -> bool {
        self.speaker.audio_on() && !self.paused && !self.muted && !self.speaker.yielded()
    }
    /// Sample `k` from the read position on (0 when the queue is empty).
    pub fn peek(&self, k: usize) -> i32 {
        self.queue.front().and_then(|c| c.frames.get(self.pos + k)).copied().unwrap_or(0)
    }
    /// How long after its timestamp a frame is heard (us).
    pub fn delay_us(&self) -> i64 {
        self.margin.delay_us(self.buffer_ms, self.latency_ms)
    }

    /// Play state to the speaker (only changes are sent).
    pub fn report(&mut self, state: &'static str) {
        if self.state != state {
            self.state = state;
            self.speaker.state(state);
        }
    }
}

struct Opts {
    host: String,
    port: u16,
    rpc_port: Option<u16>,
    latency_ms: i64,
    name: String,
    fixed_delay: bool,
    warp0: bool,
}

/// This client's id at the server (its MAC, as snapclient's).
pub fn mac() -> String {
    zd1_sys::format_mac(&zd1_audio::app::mac())
}

/// Linear interpolation to OUT_RATE, stereo out (mono doubled, extra channels dropped); state carries across
/// chunks. Integer only: the ARM926 has no FPU. Configure the server's stream at 44100 Hz to skip it.
struct Resampler {
    step: u64,
    pos: u64,
    prev: [i32; 2],
}

impl Resampler {
    /// None for rate 0 (a step of 0 would never get through a chunk).
    fn new(rate: u32) -> Option<Resampler> {
        (rate > 0).then(|| Resampler { step: ((rate as u64) << 32) / OUT_RATE as u64, pos: 0, prev: [0; 2] })
    }
    fn run(&mut self, input: &[i32], ch: usize, out: &mut Vec<i32>) {
        let frame = |i: usize| -> [i32; 2] {
            let f = &input[i * ch..];
            [f[0], if ch > 1 { f[1] } else { f[0] }]
        };
        let n = input.len() / ch;
        if self.step == 1 << 32 {
            for i in 0..n {
                out.extend_from_slice(&frame(i));
            }
            return;
        }
        // pos: position in input frames (32.32), relative to the frame before input[0] (= prev)
        while (self.pos >> 32) < n as u64 {
            let i = (self.pos >> 32) as usize;
            let t = (self.pos & 0xFFFF_FFFF) as i64;
            let a = if i == 0 { self.prev } else { frame(i - 1) };
            let b = frame(i);
            for c in 0..2 {
                out.push((a[c] as i64 + (((b[c] as i64 - a[c] as i64) * t) >> 32)) as i32);
            }
            self.pos += self.step;
        }
        if n > 0 {
            self.prev = frame(n - 1);
        }
        self.pos -= (n as u64) << 32;
    }
}

const WIRED: &str = "eth0";

/// The interfaces with an IPv4 address: (name, address, netmask, carrier).
fn links() -> Vec<(&'static str, Ipv4Addr, Ipv4Addr, bool)> {
    [WIRED, "wlan0"]
        .into_iter()
        .filter_map(|n| {
            let i = zd1_net::Iface::open(n).ok()?;
            Some((n, i.ipv4()?, i.ipv4_mask()?, i.carrier()))
        })
        .collect()
}

/// Whether `host` is on the wired link's subnet (with carrier).
fn on_wired(host: &str) -> bool {
    let Ok(h) = host.parse::<Ipv4Addr>() else { return false };
    links()
        .iter()
        .any(|&(n, a, m, c)| n == WIRED && c && (u32::from(a) & u32::from(m)) == (u32::from(h) & u32::from(m)))
}

/// The server to use: --host, or the ones that answer on mDNS (asked on every interface every 5 s until one does):
/// one on the wired link's subnet first.
fn server(o: &Opts) -> (String, u16) {
    if !o.host.is_empty() {
        return (o.host.clone(), o.port);
    }
    tracing::info!("looking for a snapserver (mDNS _snapcast._tcp)");
    loop {
        let local: Vec<Ipv4Addr> = links().iter().filter(|l| l.3).map(|l| l.1).collect();
        let found = zd1_mdns::browse_all("_snapcast._tcp.local", &local, Duration::from_secs(2));
        if let Some(f) = found.iter().find(|f| on_wired(&f.addr.to_string())).or(found.first()) {
            let wired = on_wired(&f.addr.to_string());
            tracing::info!(name = f.name, addr = %f.addr, port = f.port, wired, "found a snapserver");
            return (f.addr.to_string(), f.port);
        }
        std::thread::sleep(Duration::from_secs(3));
    }
}

/// The speaker's keys and volume: the keys to the stream when the server can control it (rpc.rs), else play / pause
/// mute us here (only ours while we play: not while another source has the speaker, nor without a server); the
/// volume to the server (`volume`, which the session sends).
fn on_speaker(shared: &Arc<Mutex<Shared>>, volume: &mpsc::Sender<u32>, ev: Event) {
    let mut st = shared.lock().unwrap();
    match ev {
        Event::Key { key, .. } if st.speaker.yielded() || st.server.is_none() => {
            tracing::debug!(key, "key: not ours now")
        }
        Event::Key { key, .. } => {
            let cmd = match key {
                "play" => "play",
                "pause" => "pause",
                "playpause" => "playPause",
                "next" => "next",
                "prev" => "previous",
                _ => return,
            };
            let Some((host, rpc_port)) = st.server.clone() else { return };
            let (shared, key) = (shared.clone(), key.to_string());
            std::thread::spawn(move || match rpc::control(&host, rpc_port, &mac(), cmd) {
                Ok(()) => tracing::info!(key, cmd, "key -> the server"),
                Err(e) => {
                    tracing::info!(key, %e, "key: the server can't: here");
                    local_key(&mut shared.lock().unwrap(), &key);
                }
            });
        }
        // the speaker left our input: quiet (audible()), and say so
        Event::Release { .. } if st.state == "playing" => st.report("paused"),
        Event::Volume(v) if st.volume != Some(v) => {
            st.volume = Some(v);
            let _ = volume.send(v);
        }
        // another source has the speaker: the player lets go of the device (Speaker::yielded)
        _ => {}
    }
}

/// play / pause without the server: mute or unmute our output (and say so to the speaker).
fn local_key(st: &mut Shared, k: &str) {
    match k {
        "pause" => {
            st.paused = true;
            st.report("paused");
        }
        "play" => st.paused = false,
        "playpause" => {
            st.paused = !st.paused;
            let p = if st.paused { "paused" } else { "playing" };
            st.report(p);
        }
        _ => {}
    }
}

fn session(
    o: &Opts,
    host: &str,
    port: u16,
    shared: &Arc<Mutex<Shared>>,
    volume: &mpsc::Receiver<u32>,
) -> std::io::Result<()> {
    let s = TcpStream::connect((host, port))?;
    s.set_nodelay(true)?;
    // a small receive window: the server can't send more than a few frames back to back (sulu_eth's SRAM RX ring holds
    // about two full frames; bursts of a whole default window get dropped and retransmitted)
    let rcvbuf: libc::c_int = 64 << 10;
    unsafe {
        use std::os::unix::io::AsRawFd;
        libc::setsockopt(
            s.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            &rcvbuf as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
    }
    let mut w = s.try_clone()?;
    let mut r = BufReader::with_capacity(64 << 10, s);
    let wired = on_wired(host);
    let (vol_tx, vol_rx) = mpsc::channel::<String>();
    let client_info = |v: u32| format!(r#"{{"volume":{v},"muted":false}}"#);
    {
        let mut st = shared.lock().unwrap();
        st.wired = wired;
        st.margin.start(wired);
        st.stall = None;
        tracing::info!(
            host,
            port,
            wired,
            margin_ms = st.margin.us / 1000,
            fixed = !st.margin.adaptive,
            "connected to the snapserver"
        );
        st.server = Some((host.to_string(), o.rpc_port.unwrap_or(port + 1)));
        st.speaker.session(true); // the speaker lists Snapcast as a source
        // the speaker's volume, if known, is the one to keep: the server may remember another
        if let Some(v) = st.speaker.level() {
            st.volume = Some(v);
            let _ = vol_tx.send(client_info(v));
        }
    }
    let mac = mac();
    let hello = format!(
        r#"{{"Arch":"armv5te","ClientName":"Snapclient","HostName":"{}","ID":"{}","Instance":1,"MAC":"{}","OS":"zd1 module OS","SnapStreamProtocolVersion":2,"Version":"0.35.0"}}"#,
        proto::json_escape(&o.name),
        mac,
        mac
    );
    proto::write(&mut w, proto::HELLO, 0, now_us(), &proto::json_payload(&hello))?;
    // time requests (a burst first, then one a second) and the speaker's volume, from their own thread
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (stop2, mut w2) = (stop.clone(), w.try_clone()?);
    let timer = std::thread::spawn(move || {
        let mut id = 1u16;
        while !stop2.load(std::sync::atomic::Ordering::Relaxed) {
            if proto::write(&mut w2, proto::TIME, id, now_us(), &proto::tv(0)).is_err() {
                break;
            }
            for j in vol_rx.try_iter() {
                if proto::write(&mut w2, proto::CLIENT_INFO, 0, now_us(), &proto::json_payload(&j)).is_err() {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(if id < 20 { 50 } else { 1000 }));
            id = id.wrapping_add(1);
        }
    });
    let mut sync = timesync::TimeSync::new();
    let (to_dec, rx) = mpsc::channel::<ToDecoder>();
    let decoder = {
        let (s, warp0) = (shared.clone(), o.warp0);
        std::thread::spawn(move || decoding(rx, s, warp0))
    };
    let res = loop {
        for v in volume.try_iter() {
            let _ = vol_tx.send(client_info(v));
        }
        let (h, msg) = match proto::read(&mut r, now_us) {
            Ok(x) => x,
            Err(e) => break Err(e),
        };
        match msg {
            proto::Msg::ServerSettings(j) => {
                let num = |k| proto::json_field(&j, k).and_then(|v| v.parse::<i64>().ok());
                let mut st = shared.lock().unwrap();
                st.buffer_ms = num("bufferMs").unwrap_or(1000).clamp(0, MAX_BUFFER_MS);
                st.latency_ms = (num("latency").unwrap_or(0) + o.latency_ms).clamp(0, st.buffer_ms);
                st.muted = proto::json_field(&j, "muted") == Some("true");
                let v = num("volume").unwrap_or(100).clamp(0, 100) as u32;
                if st.volume != Some(v) {
                    // the server's volume -> the speaker
                    st.volume = Some(v);
                    st.speaker.send(&format!("volume {v}"));
                }
                tracing::info!(
                    buffer_ms = st.buffer_ms,
                    latency_ms = st.latency_ms,
                    volume = v,
                    muted = st.muted,
                    "server settings"
                );
            }
            proto::Msg::CodecHeader { codec, data } => {
                let _ = to_dec.send(ToDecoder::Codec(codec, data));
            }
            proto::Msg::WireChunk { ts_us, data } => {
                let _ = to_dec.send(ToDecoder::Chunk { ts_us, arrived_us: now_us(), data });
            }
            proto::Msg::Time { latency_us } => {
                sync.add(latency_us, h.sent_us, h.received_us);
                if sync.count() >= 5 {
                    shared.lock().unwrap().diff = sync.diff();
                }
            }
            proto::Msg::Error(e) => tracing::warn!(e, "server error"),
            proto::Msg::Other => {}
        }
        let mut st = shared.lock().unwrap();
        if st.state == "playing" && now_us() - st.last_sound_us > IDLE_US {
            tracing::info!(
                corrections = st.corrections,
                stalls = st.margin.stalls,
                "stream idle (sync corrections: frames dropped or repeated)"
            );
            st.report("stopped");
            st.margin.settle(); // nothing plays: the margin may move now
        }
    };
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = w.shutdown(std::net::Shutdown::Both);
    let _ = timer.join();
    drop(to_dec);
    let _ = decoder.join();
    res
}

enum ToDecoder {
    Codec(String, Vec<u8>),
    Chunk { ts_us: i64, arrived_us: i64, data: Vec<u8> },
}

const WARP_RETRY_US: i64 = 5_000_000; // Warp0 taken by another receiver: asked again this much later

/// The decoder thread of a session: codec headers and chunks from the reader, in order; each chunk decoded (FLAC on
/// Warp0 while there is sound, unless `warp0` is off), resampled and queued for the player with its arrival time.
fn decoding(rx: mpsc::Receiver<ToDecoder>, shared: Arc<Mutex<Shared>>, warp0: bool) {
    let mut dec: Option<decode::Decoder> = None;
    let mut rs: Option<Resampler> = None;
    let mut meta: VecDeque<(u64, i64, i64)> = VecDeque::new(); // tag, timestamp, arrival of the chunks out
    let mut tag = 0u64;
    let mut pending: Option<(i64, i64)> = None; // the last chunk's (end, lateness): counted once the next continues it
    let mut warp_at = 0i64; // when to ask for Warp0 next
    loop {
        // results from Warp0 come in while waiting for the next chunk: a chunk takes it ~8 ms; one kernel tick (HZ = 100)
        // is the shortest wait that sleeps (a shorter timeout returns at once: a loop of system calls)
        let wait = if dec.as_ref().is_some_and(|d| d.busy()) { 10 } else { 500 };
        match rx.recv_timeout(Duration::from_millis(wait)) {
            Ok(ToDecoder::Codec(codec, data)) => {
                if let Some(d) = dec.as_mut() {
                    d.use_warp(false);
                }
                match decode::Decoder::new(&codec, &data) {
                    Ok(d) => {
                        let f = d.format();
                        tracing::info!(codec, rate = f.rate, bits = f.bits, channels = f.channels, "stream format");
                        if codec == "pcm" && !shared.lock().unwrap().wired {
                            tracing::info!(
                                "PCM over Wi-Fi: `codec = flac` in the server's [stream] section (or the stream's \
                                 URI) sends about half the bytes, losslessly"
                            );
                        }
                        rs = Resampler::new(f.rate);
                        dec = rs.is_some().then_some(d);
                    }
                    Err(e) => {
                        tracing::warn!(e, "stream not played");
                        dec = None;
                    }
                }
                meta.clear();
                pending = None;
                let mut st = shared.lock().unwrap();
                st.queue.clear();
                st.pos = 0;
            }
            Ok(ToDecoder::Chunk { ts_us, arrived_us, data }) => {
                if let Some(d) = dec.as_mut() {
                    tag += 1;
                    meta.push_back((tag, ts_us, arrived_us));
                    d.push(tag, data);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        let Some(d) = dec.as_mut() else { continue };
        if d.is_flac() {
            // Warp0 while the stream has sound and the speaker is ours: taken at the first sound, let go after IDLE_US
            let now = now_us();
            let want = warp0 && {
                let st = shared.lock().unwrap();
                now - st.last_sound_us < IDLE_US && !st.speaker.yielded()
            };
            if want && !d.on_warp() && now >= warp_at {
                if d.use_warp(true) {
                    tracing::info!("Warp0 decodes the FLAC stream");
                } else {
                    warp_at = now + WARP_RETRY_US;
                }
            } else if !want && d.on_warp() {
                d.use_warp(false);
                tracing::info!("Warp0 let go (no sound)");
            }
        }
        while let Some((t, r)) = d.take() {
            let Some(at) = meta.iter().position(|m| m.0 == t) else { continue };
            let (_, ts_us, arrived_us) = meta.remove(at).unwrap();
            let pcm = match r {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(e, "chunk not decoded");
                    continue;
                }
            };
            let Some(rsm) = rs.as_mut() else { continue };
            let mut frames = Vec::with_capacity(pcm.len() * 2);
            rsm.run(&pcm, d.format().channels as usize, &mut frames);
            deliver(&shared, ts_us, arrived_us, frames, &mut pending);
        }
    }
    if let Some(d) = dec.as_mut() {
        d.use_warp(false);
    }
}

/// A decoded chunk (stereo frames at OUT_RATE) into the player's queue: its lateness noted, a stream's start told
/// to the speaker.
fn deliver(shared: &Arc<Mutex<Shared>>, ts_us: i64, now: i64, frames: Vec<i32>, pending: &mut Option<(i64, i64)>) {
    let mut st = shared.lock().unwrap();
    if let Some(diff) = st.diff {
        // how late against the floor (the margin left out)
        if let Some((end, late)) = pending.take()
            && (ts_us - end).abs() < CONTINUATION_US
        {
            st.margin.note_arrival(now, late);
        } // else: a stale chunk
        let floor = (st.buffer_ms - st.latency_ms) * 1000;
        *pending = Some((ts_us + (frames.len() / 2) as i64 * 1_000_000 / OUT_RATE as i64, now + diff - ts_us - floor));
    }
    if frames.iter().any(|&v| v.unsigned_abs() > 1 << 16) {
        // sound (not just dither / silence)
        if now - st.last_sound_us > IDLE_US && !st.paused {
            st.report("playing"); // a stream starts: the speaker switches to us
        }
        st.last_sound_us = now;
    }
    // bound the queue: a few seconds beyond the playout delay is plenty
    let limit = (st.delay_us() / 1000 + 3000).max(0) as usize * OUT_RATE as usize / 1000 * 2;
    let mut total: usize = st.queue.iter().map(|c| c.frames.len()).sum();
    while total > limit {
        if let Some(c) = st.queue.pop_front() {
            total -= c.frames.len();
            st.pos = 0;
        } else {
            break;
        }
    }
    st.queue.push_back(Chunk { ts_us, frames, arrived_us: now });
}

pub fn main() {
    zd1_compat::init();
    let options = ["--host", "--port", "--rpc-port", "--device", "--latency", "--name"];
    let args = Args::parse(USAGE, &options, &["--fixed-delay", "--no-warp0", "-v"]);
    args.log();
    let o = Opts {
        host: args.get("--host").unwrap_or("").to_string(),
        port: args.get_or("--port", 1704),
        rpc_port: args.get("--rpc-port").and_then(|v| v.parse().ok()),
        latency_ms: args.get_or("--latency", 0),
        name: args.name(),
        fixed_delay: args.flag("--fixed-delay"),
        warp0: !args.flag("--no-warp0"),
    };
    let (vol_tx, vol_rx) = mpsc::channel();
    let shared: Arc<Mutex<Shared>> = Arc::new_cyclic(|weak: &std::sync::Weak<Mutex<Shared>>| {
        let weak = weak.clone();
        let speaker = Speaker::start(SOURCE, move |ev| {
            if let Some(shared) = weak.upgrade() {
                on_speaker(&shared, &vol_tx, ev);
            }
        });
        Mutex::new(Shared::new(speaker, o.latency_ms, !o.fixed_delay))
    });
    {
        let s = shared.clone();
        let d = args.device();
        std::thread::spawn(move || player::run(d, s));
    }
    loop {
        let (host, port) = server(&o);
        match session(&o, &host, port, &shared, &vol_rx) {
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                tracing::info!(host, port, "the server closed the connection")
            }
            Err(e) => tracing::warn!(host, port, %e, "session ended"),
            Ok(()) => {}
        }
        {
            let mut st = shared.lock().unwrap();
            st.queue.clear();
            st.pos = 0;
            st.diff = None;
            st.server = None;
            st.report("stopped");
            st.speaker.session(false);
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wifi() -> Margin {
        let mut m = Margin::new(true);
        m.start(false);
        m
    }

    #[test]
    fn margin_follows_the_latest_arrival_and_comes_back_down() {
        let mut m = wifi();
        m.settle();
        assert_eq!(m.us, WIFI_BASE_US); // nothing seen: the link's base
        m.note_arrival(10_000_000, -900_000); // early, as usual
        m.note_arrival(11_000_000, 1_200_000); // a chunk 1.2 s past the floor (a stall)
        m.settle();
        assert_eq!(m.us, 1_200_000 + SAFETY_US);
        m.note_arrival(11_000_000 + (HISTORY_S + 1) * 1_000_000, -900_000); // HISTORY_S later, all early again
        m.settle();
        assert_eq!(m.us, WIFI_BASE_US);
    }

    #[test]
    fn a_stall_grows_the_margin_up_to_the_cap_and_fixed_delay_never_moves() {
        let mut m = wifi();
        m.grow(750_000);
        assert_eq!(m.us, WIFI_BASE_US + 750_000 + SAFETY_US);
        m.grow(10_000_000);
        assert_eq!(m.us, MAX_MARGIN_US);
        let mut f = Margin::new(false);
        f.start(false);
        f.note_arrival(1_000_000, 2_000_000);
        f.settle();
        assert_eq!(f.us, 0);
    }

    #[test]
    fn the_delay_is_the_buffer_less_latency_plus_the_margin() {
        let mut m = Margin::new(true);
        m.us = 300_000;
        assert_eq!(m.delay_us(500, 40), 760_000);
    }
}
