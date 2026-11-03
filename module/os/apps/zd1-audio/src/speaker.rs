//! The speaker, through zd1-linkd's socket /run/zd1link (module/os/README.md): a source's play state, whether a sender
//! is connected (`session`) and the sender's volume go to the MCU; the speaker's keys come back to the source
//! (`Event::Key`; a key meant for another source is dropped), its volume (`level`, and `Event::Volume` when the knob
//! or the remote changed it), `Event::Release` when the speaker leaves the module's input (`audio off`: pause the
//! sender), `select` (chosen on the speaker) as a play key, and
//! `yield`/`resume` say whether another source has the speaker (`yielded`, and `Event::Yield` so the source can pause
//! its sender). A source still playing when it gets the speaker back (`select`, `resume`) says so again: zd1-linkd
//! then makes it the one playing. Reconnects when zd1-linkd isn't there (yet). ZD1_LINK=PATH: another socket (lab
//! runs kept away from the MCU).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// What the speaker tells a source.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Event<'a> {
    /// A key: "play", "pause", "playpause", "next", "prev"; `playing`: whether we last reported playing
    Key { key: &'a str, playing: bool },
    /// Another source has the speaker now: pause the sender; `playing`: whether we last reported playing
    Yield { playing: bool },
    /// The speaker's volume changed on the speaker (knob, keys, remote: not by our `volume`), 0-100 % of the range
    /// a module may use
    Volume(u32),
    /// The speaker left the module's input (another source, standby): pause the sender if it plays
    Release { playing: bool },
}

#[derive(Clone)]
pub struct Speaker {
    link: Arc<Mutex<Link>>,
    source: &'static str,
    yielded: Arc<AtomicBool>,
}

struct Link {
    stream: Option<UnixStream>,
    state: &'static str, // what we last reported (sent again on reconnect)
    session: bool,
    volume: Option<String>,
    level: Option<u32>,  // the speaker's volume: its last report or our last `volume`
    audio: Option<bool>, // the module is the speaker's input (`audio on|off`)
}

impl Speaker {
    /// The link for `source` (our name to zd1-linkd: `state playing <source>`, `yield <other>`); `events` gets the
    /// speaker's keys and yields, on the link's thread.
    pub fn start(source: &'static str, events: impl Fn(Event) + Send + 'static) -> Self {
        let sp = Speaker {
            link: Arc::new(Mutex::new(Link {
                stream: None,
                state: "stopped",
                session: false,
                volume: None,
                level: None,
                audio: None,
            })),
            source,
            yielded: Arc::new(AtomicBool::new(false)),
        };
        let s2 = sp.clone();
        std::thread::spawn(move || s2.run(events));
        sp
    }

    /// Another source has the speaker: no output until it lets go.
    pub fn yielded(&self) -> bool {
        self.yielded.load(Ordering::Relaxed)
    }

    /// The speaker has the module as its input (`audio on`, or not told otherwise yet).
    pub fn audio_on(&self) -> bool {
        self.link.lock().unwrap().audio != Some(false)
    }

    /// The speaker's volume (0-100) as far as we know it: what zd1-linkd last said, or our last `volume`.
    pub fn level(&self) -> Option<u32> {
        self.link.lock().unwrap().level
    }

    /// A line to zd1-linkd (dropped while it isn't connected; the state and volume are resent on connect).
    pub fn send(&self, line: &str) {
        let mut l = self.link.lock().unwrap();
        if let Some(v) = line.strip_prefix("volume ") {
            l.volume = Some(v.to_string());
            l.level = v.parse().ok().or(l.level);
        }
        write_line(&mut l.stream, line);
    }

    pub fn state(&self, state: &'static str) {
        let mut l = self.link.lock().unwrap();
        if l.state != state {
            l.state = state;
            tracing::info!(state, "speaker");
            write_line(&mut l.stream, &format!("state {state} {}", self.source));
        }
    }

    /// A sender is connected (an AirPlay client, a Spotify Connect session) or gone: the speaker lists us as a source.
    pub fn session(&self, on: bool) {
        let mut l = self.link.lock().unwrap();
        if l.session != on {
            l.session = on;
            tracing::info!(on, "session");
            write_line(&mut l.stream, &format!("session {} {}", if on { "on" } else { "off" }, self.source));
        }
    }

    /// A person picked this receiver (on the phone, in the app): the speaker switches to it and says so.
    pub fn intent(&self) {
        tracing::info!("picked: telling the speaker");
        write_line(&mut self.link.lock().unwrap().stream, &format!("intent {}", self.source));
    }

    fn run(&self, events: impl Fn(Event)) {
        // ZD1_LINK: another socket (lab runs that must not reach the MCU: no pick, no amp)
        let path = std::env::var("ZD1_LINK").unwrap_or_else(|_| "/run/zd1link".into());
        loop {
            let Ok(s) = UnixStream::connect(&path) else {
                std::thread::sleep(Duration::from_secs(2));
                continue;
            };
            {
                let mut l = self.link.lock().unwrap();
                l.stream = s.try_clone().ok();
                let (state, session, vol) = (l.state, l.session, l.volume.clone());
                write_line(&mut l.stream, &format!("state {state} {}", self.source));
                if session {
                    write_line(&mut l.stream, &format!("session on {}", self.source));
                }
                if let Some(v) = vol {
                    write_line(&mut l.stream, &format!("volume {v}"));
                }
            }
            for line in BufReader::new(s).lines() {
                let Ok(line) = line else { break };
                let (cmd, arg) = line.split_once(' ').unwrap_or((line.as_str(), ""));
                match (cmd, arg) {
                    ("key", k) => {
                        // `key KEY [SOURCE]`: for the source in front, or everyone
                        let (key, to) = k.split_once(' ').unwrap_or((k, self.source));
                        if to != self.source {
                            continue;
                        }
                        let playing = self.link.lock().unwrap().state == "playing";
                        events(Event::Key { key, playing });
                    }
                    ("select", src) if src == self.source => {
                        // chosen on the speaker: resume the sender
                        tracing::info!("chosen on the speaker: resuming the sender");
                        self.yielded.store(false, Ordering::Relaxed);
                        let playing = self.reclaim();
                        events(Event::Key { key: "play", playing });
                    }
                    ("yield", src) if src != self.source => {
                        tracing::info!(src, "another source has the speaker: pausing the sender");
                        self.yielded.store(true, Ordering::Relaxed);
                        let playing = self.link.lock().unwrap().state == "playing";
                        events(Event::Yield { playing });
                    }
                    ("resume", src) if src != self.source => {
                        if self.yielded.swap(false, Ordering::Relaxed) {
                            tracing::info!(src, "the other source stopped: the speaker is ours again");
                            self.reclaim();
                        }
                    }
                    ("audio", on) => {
                        // on a connect, then when the speaker enters / leaves us
                        let was = self.link.lock().unwrap().audio.replace(on == "on");
                        if was == Some(true) && on == "off" {
                            let playing = self.link.lock().unwrap().state == "playing";
                            events(Event::Release { playing });
                        }
                    }
                    ("volume", v) => {
                        if let Ok(v) = v.parse::<u32>() {
                            // on a connect, then the speaker's changes
                            let was = self.link.lock().unwrap().level.replace(v);
                            if was.is_some() && was != Some(v) {
                                events(Event::Volume(v));
                            }
                        }
                    }
                    _ => {}
                }
            }
            self.link.lock().unwrap().stream = None;
            std::thread::sleep(Duration::from_secs(1));
        }
    }
}

impl Speaker {
    /// The speaker is ours again: if we still play, say so (zd1-linkd makes the newest `playing` the one playing).
    /// Whether we play.
    fn reclaim(&self) -> bool {
        let mut l = self.link.lock().unwrap();
        let playing = l.state == "playing";
        if playing {
            write_line(&mut l.stream, &format!("state playing {}", self.source));
        }
        playing
    }
}

fn write_line(stream: &mut Option<UnixStream>, line: &str) {
    if let Some(s) = stream
        && s.write_all(format!("{line}\n").as_bytes()).is_err()
    {
        *stream = None;
    }
}
