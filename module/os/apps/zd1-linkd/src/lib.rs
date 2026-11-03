//! zd1-linkd: the MCU link once Linux runs on the module. It reaches the SPI slave, the GPIO and timer block and the
//! BL2 mailbox through /dev/mem (module/dm870) and serves the register protocol (module/link) to our MCU firmware in its
//! native dialect (zd1-spi spec::native; the only one our MCU speaks).
//! Until our MCU has greeted us (0xC3) only what a stock host knows is queued: the MCU bootloader's relay to BL2
//! reads nothing else. Our link starting (0x11 notified once) makes the MCU greet us, at boot or after a restart.
//!   to the MCU:  status ready (0x11), our identification (0x25 VersionImage: "zd1-os VERSION KIND", how our MCU tells
//!                our OS from the stock app), our dialect (0xD5), network (0x32 "21" = connected / "00"), IP (0x38),
//!                device name (0x58), our receivers (0xC6: the one in front and its state is the play state), picks
//!                (0xC0), volume requests (0xD4, a step), the State record (0xD6: the host link's `State` message,
//!                after the hello and whenever it changes, once a second at most; docs/module/settings.md). Our log is the kernel's, which goes to the console UART: the
//!                MCU logs that (init hands our stdout to /dev/kmsg)
//!   from the MCU: the speaker (0xD2: its volume, the cap, whether we are its input), key presses (0xD3), a receiver
//!                chosen on the speaker (0xC2), the configuration channel (0x45; our settings as "zd1/KEY": zd1-settings,
//!                our records as "zd1/@N"),
//!                shutdown (0xE2: acked before it cuts power), HCR 0x21 -> reboot into BL2, the debug services'
//!                mode (0xC1: only the MCU switches them, nothing on the network can), its version (0xC3), the good
//!                system slot (0xC4 b1, at ready: /run/zd1/good), an update (0xC5: zd1-update)
//! Apps talk to it over /run/zd1link (zd1-ctl): one text line per message, see `Cmd` and `App::event`.
//! The SPI slave's pads and clock are init's to set up (zd1-board): we only check them.
//! Polling: the SPI slave isn't bound to any kernel driver (no IRQ 20). It spins while a request or response is
//! mid-transfer and for 3 ms after the MCU's last byte (it keeps clocking idle bytes until our response is out: they
//! must not be left filling the RX FIFO, or the next request, e.g. a key press, is dropped), else sleeps one tick
//! (10 ms at HZ=100; the MCU waits up to 500 ms for a reply). Spinning whenever the MCU had talked recently (200 ms)
//! took 90 % of the CPU: this kernel's sched_yield() hardly yields.

use dm870::DevMem;
use dm870::pads::{self, pads};
use dm870::timers::timers;
use std::fs::OpenOptions;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::{Duration, Instant};
use zd1_link::Link;

const SOCKET: &str = "/run/zd1link";
use std::sync::{Arc, Mutex};
use zd1_hostlink::codec::{self, Val};
use zd1_hostlink::{Record, Slot, WifiSecurity};
use zd1_link::reg;
use zd1_link::spi::codec::{config, net};
use zd1_link::spi::spec::{STOCK_F, STOCK_V, boot, native, player};
use zd1_net::Iface;
use zd1_net::wpa::{Network, Watch, WifiState};
use zd1_sys::{OsRelease, hex};

#[derive(Clone, Default, PartialEq)]
struct Net {
    eth: String,
    wifi: String,
    eth_link: bool,
}

impl Net {
    /// The address the MCU is told (0x38) and the receivers' `net up`, empty for no network: the wired one with a cable
    /// in and a lease, else the Wi-Fi's, else the wired link-local one (a computer on the cable, no DHCP server). eth0
    /// keeps its fallback 169.254.1.1 without a cable too: that alone is no network.
    fn ip(&self) -> &str {
        let wired = if self.eth_link { self.eth.as_str() } else { "" };
        if !wired.is_empty() && !wired.starts_with("169.254.") {
            wired
        } else if !self.wifi.is_empty() {
            &self.wifi
        } else {
            wired
        }
    }
}

/// Each interface's IPv4 address (eth0, wlan0), and whether the wired link is up.
fn net_state() -> Net {
    let iface = |name| Iface::open(name).ok();
    let ip = |name| iface(name).and_then(|i| i.ipv4()).map(|a| a.to_string()).unwrap_or_default();
    Net { eth: ip("eth0"), wifi: ip("wlan0"), eth_link: iface("eth0").is_some_and(|i| i.carrier()) }
}

/// "zd1-os VERSION KIND" from /etc/os-release (the commit; release, debug or rescue).
fn os_ident() -> String {
    let r = OsRelease::read("/etc/os-release");
    format!("zd1-os {} {}", r.get("VERSION_ID"), r.get("VARIANT_ID"))
}

fn mtime(p: &str) -> Option<std::time::SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

fn cstr(s: &str) -> Vec<u8> {
    let mut v = s.as_bytes().to_vec();
    v.push(0);
    v
}

/// At most `max` bytes of `s`, cut at a character boundary (a field's maximum).
fn trim(s: &str, max: usize) -> &str {
    let mut n = s.len().min(max);
    while !s.is_char_boundary(n) {
        n -= 1;
    }
    &s[..n]
}

/// A list record's entries, as many as fit one notification (its length prefixes counted).
fn fit(entries: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    let mut size = 0;
    entries
        .into_iter()
        .take_while(|e| {
            size += 2 + e.len();
            size <= zd1_link::NOTE_MAX
        })
        .collect()
}

/// A network as a `ScanEntry` (the SSID cut to the field's 32 bytes; the signal as -dBm).
fn scan_entry(n: &Network) -> Result<Vec<u8>, codec::Error> {
    let ssid = String::from_utf8_lossy(&n.ssid);
    codec::encode(
        "ScanEntry",
        &[
            ("ssid", Val::from(trim(&ssid, 32))),
            ("signal", Val::N((-n.level).clamp(0, 255) as u32)),
            ("security", Val::N(n.security().value() as u32)),
        ],
    )
}

struct Client {
    s: UnixStream,
    buf: Vec<u8>,
    id: u64,
}

/// The speaker as the MCU tells it (0xD2).
#[derive(Clone, Copy, PartialEq)]
struct Speaker {
    step: u8,
    cap: u8,
    input: bool,
}

struct App {
    link: Link<DevMem>,
    setting_err: Option<(String, String)>, // a refused setting: its key and why, for the MCU's read-back
    name: String,                          // the device name we told the MCU (zd1-settings `name`)
    clients: Vec<Client>,
    net: Net,
    volume: Option<u32>,      // the module's volume as apps see it: % of the speaker's 0..cap
    speaker: Option<Speaker>, // the MCU's last 0xD2
    audio_on: bool,
    owner: Option<String>,    // the source playing now (`state playing SOURCE`)
    front: (u8, u8),          // the receiver in front and its state (0xC6 [0], [1])
    sessions: Vec<(u64, u8)>, // receivers with a sender connected: (client id, receiver)
    shown: [u8; 3],           // what the MCU was told on 0xC6
    next_id: u64,
    intents: u8,                         // counts the picks (0xC0 b1)
    greeted: bool,                       // our MCU firmware greeted us (0xC3): out of the stock-safe mode
    ident: String,                       // "zd1-os VERSION KIND"
    slot: Option<zd1_slot::Slot>,        // the slot we run from (none: the rescue, an initramfs)
    boot: String,                        // the boot image's identity (zd1.boot=; empty: an older one)
    good: Option<zd1_slot::Slot>,        // the good slot, as the MCU said at ready (0xC4 b1)
    mcu: String,                         // the MCU firmware's version (0xC3)
    state_shown: Vec<u8>,                // the State record as the MCU last got it (0xD6)
    wifi: Arc<Mutex<Wifi>>,              // the supplicant's word: the Wi-Fi's state, access point, signal
    servers: Arc<Mutex<Servers>>,        // the snapservers found over mDNS, and whether a look runs
    stamps: Arc<Mutex<[String; 2]>>,     // what slots A and B hold (zd1-slot's stamps; read in a thread)
    update: Option<std::process::Child>, // zd1-update while it runs
}

/// Both slots' stamps, read in a thread (the slot that doesn't run is mounted to read it: seconds of JFFS2 scan).
fn read_stamps(into: &Arc<Mutex<[String; 2]>>) {
    let into = into.clone();
    std::thread::spawn(move || {
        let s = [zd1_slot::Slot::A, zd1_slot::Slot::B].map(|s| zd1_slot::stamp(s).unwrap_or_default());
        *into.lock().unwrap() = s;
    });
}

/// The Wi-Fi as the supplicant last said, with the networks its last scan found (zd1-net's `wpa::Watch`, polled once
/// a second in a thread). A request to the supplicant waits up to 300 ms for its answer, longer while it is busy
/// joining a network, and the link must not wait: the SPI slave's 64-byte RX FIFO overruns within milliseconds of a
/// long MCU write (the likely cause of an Update BEGIN half received ~20 s after boot [HW 2026-11-02]).
struct Wifi {
    state: WifiState,
    ssid: String,
    keyed: bool,
    bssid: String,
    signal: u8,
    nets: Option<Vec<Network>>, // None: no supplicant
    scan: bool,                 // a read of the Scan record wants a new scan
}

fn watch_wifi() -> Arc<Mutex<Wifi>> {
    let shared = Arc::new(Mutex::new(Wifi {
        state: WifiState::Off,
        ssid: String::new(),
        keyed: false,
        bssid: String::new(),
        signal: 0,
        nets: None,
        scan: false,
    }));
    let wifi = shared.clone();
    std::thread::spawn(move || {
        let mut w = Watch::new("wlan0");
        loop {
            w.poll();
            let nets = w.networks();
            if std::mem::take(&mut wifi.lock().unwrap().scan) {
                w.scan();
            }
            let mut s = wifi.lock().unwrap();
            (s.state, s.keyed, s.signal, s.nets) = (w.state, w.keyed, w.signal, nets);
            s.ssid.clone_from(&w.ssid);
            s.bssid.clone_from(&w.bssid);
            drop(s);
            std::thread::sleep(Duration::from_secs(1));
        }
    });
    shared
}

/// The snapservers the last look over mDNS found (the `Servers` record), and whether one runs now (in a thread: a
/// look takes 1.5 s, and the link must not wait).
#[derive(Default)]
struct Servers {
    found: Vec<zd1_mdns::Found>,
    looking: bool,
}

const SNAPSERVER: &str = "_snapcast._tcp.local";

impl App {
    /// A line for the log: stdout, which init hands to the kernel log (and so the console the MCU logs).
    fn say(&mut self, s: &str) {
        print!("{s}");
        let _ = std::io::stdout().flush();
    }

    /// To every connected app; a client that can't take it is dropped.
    fn event(&mut self, line: &str) {
        self.event_except(0, line);
    }

    /// To every connected app but `id` (0: none left out).
    fn event_except(&mut self, id: u64, line: &str) {
        let msg = format!("{line}\n");
        self.clients.retain_mut(|c| c.id == id || c.s.write_all(msg.as_bytes()).is_ok());
    }

    /// The speaker from the MCU (0xD2): our input or not (`audio on|off`), and its volume as apps see it (`volume N`,
    /// unless it is what they asked for).
    fn speaker(&mut self, p: &[u8]) {
        if p.len() < 4 || p[0] < native::PROTO {
            return;
        }
        let sp = Speaker { step: p[1], cap: p[2], input: p[3] & native::INPUT != 0 };
        let was = self.speaker.replace(sp);
        if was.map(|w| w.input) != Some(sp.input) {
            self.audio_on = sp.input;
            self.event(if sp.input { "audio on" } else { "audio off" });
        }
        let asked = self.volume.is_some_and(|v| native::step_from_percent(v as u8, sp.cap) == sp.step);
        if !asked {
            let v = native::percent_from_step(sp.step, sp.cap) as u32;
            self.volume = Some(v);
            self.event(&format!("volume {v}"));
        }
    }

    /// An app's volume (0-100 % of the speaker's 0..cap) to the MCU (0xD4) and the other apps.
    fn set_volume(&mut self, id: u64, p: u32) {
        let p = p.min(100);
        let Some(sp) = self.speaker else { return self.say("[zd1-linkd] volume before the MCU's hello: dropped\n") };
        self.volume = Some(p);
        self.notify(reg::VOLUME_SET, &[native::step_from_percent(p as u8, sp.cap), 0]);
        self.event_except(id, &format!("volume {p}"));
    }

    /// The stock-safe mode: until our MCU firmware has greeted us (0xC3), only registers the stock tables know are
    /// queued; ours (0xC0, 0xC6, 0xD4) only update their values. A stock host skips a register it doesn't know (the
    /// MCU bootloader's relay, our way back to BL2, reads 0x0C again and again, it never sends HCR 0x21,
    /// docs/mcu/bootloader-dfu.md), and the queue stays stuck on it. That host never greets.
    fn notify(&mut self, reg: u8, data: &[u8]) {
        if !self.greeted && !STOCK_F.contains(&reg) && !STOCK_V.contains(&reg) {
            self.link.set_value(reg, data);
            return;
        }
        if !self.link.notify(reg, data) {
            self.say(&format!("[zd1-linkd] notification 0x{reg:02x} dropped (queue full)\n"));
        }
    }

    /// The State record (the host link's `State` message, encoded with its codec; docs/module/settings.md): the
    /// settings as stored (zd1-settings) and the status as this daemon knows it, the Wi-Fi's (its state, the access
    /// point, the signal) from the supplicant (`Wifi`).
    fn state_record(&self) -> Vec<u8> {
        let slot = |s: Option<zd1_slot::Slot>| {
            Val::N(
                match s {
                    Some(zd1_slot::Slot::A) => Slot::A,
                    Some(zd1_slot::Slot::B) => Slot::B,
                    None => Slot::None,
                }
                .value() as u32,
            )
        };
        let mut v = zd1_settings::record_values(&zd1_settings::load());
        let wifi = self.wifi.lock().unwrap();
        let stamps = self.stamps.lock().unwrap().clone();
        v.extend([("stamp_a", Val::from(trim(&stamps[0], 64))), ("stamp_b", Val::from(trim(&stamps[1], 64)))]);
        v.push(("boot", Val::from(trim(&self.boot, 12))));
        let (st, fst) = self.front;
        let senders = self.sessions.iter().fold(0u8, |m, &(_, p)| m | 1 << p);
        v.extend([
            ("os", Val::from(self.ident.as_str())),
            ("slot", slot(self.slot)),
            ("good", slot(self.good)),
            ("mcu", Val::from(self.mcu.as_str())),
            ("eth_link", Val::B(self.net.eth_link)),
            ("eth_ip", Val::from(self.net.eth.as_str())),
            ("wifi_ip", Val::from(self.net.wifi.as_str())),
            ("wifi", Val::N(wifi.state.value() as u32)),
            ("bssid", Val::from(wifi.bssid.as_str())),
            ("signal", Val::N(wifi.signal as u32)),
            ("wifi_ssid", Val::from(wifi.ssid.as_str())),
            ("wifi_psk", Val::from(if wifi.keyed { "(set)" } else { "" })),
            ("input", Val::B(self.audio_on)),
            ("volume", Val::N(self.volume.unwrap_or(0))),
            ("front", Val::N(st as u32)),
            ("front_state", Val::N(fst as u32)),
            ("senders", Val::N(senders as u32)),
        ]);
        codec::encode("State", &v).unwrap_or_default()
    }

    /// The Scan record (docs/module/settings.md): the networks the supplicant's last scan found, one entry per SSID
    /// (the strongest access point's), the strongest first, hidden ones left out, as many as fit a notification; and
    /// a new scan started, for the next read. Without a supplicant (the radio off, no wlan0): `!` and why.
    fn scan_record(&mut self) -> Vec<u8> {
        let mut nets = {
            let mut w = self.wifi.lock().unwrap();
            w.scan = true;
            let Some(n) = w.nets.clone() else { return b"!no Wi-Fi: the radio is off".to_vec() };
            n
        };
        nets.sort_by_key(|n| -n.level);
        let mut seen: Vec<&[u8]> = Vec::new();
        let entries = nets
            .iter()
            .filter(|n| {
                !n.ssid.is_empty() && !seen.contains(&n.ssid.as_slice()) && {
                    seen.push(&n.ssid);
                    true
                }
            })
            .filter_map(|n| scan_entry(n).ok())
            .collect();
        codec::encode_list(&fit(entries))
    }

    /// The Networks record: the known Wi-Fi networks, the preferred first (their keys stay here).
    fn networks_record(&self) -> Vec<u8> {
        let e: Vec<Vec<u8>> = zd1_settings::networks()
            .iter()
            .filter_map(|n| {
                let sec = if n.psk.is_some() { WifiSecurity::Psk } else { WifiSecurity::Open };
                codec::encode(
                    "KnownNetwork",
                    &[("ssid", Val::from(n.ssid.as_str())), ("security", Val::N(sec.value() as u32))],
                )
                .ok()
            })
            .collect();
        codec::encode_list(&fit(e))
    }

    /// The Servers record: the snapservers the last look found; and a new look started in a thread (on each
    /// interface with an address), for the next read.
    fn servers_record(&mut self) -> Vec<u8> {
        let local: Vec<std::net::Ipv4Addr> =
            [&self.net.eth, &self.net.wifi].iter().filter_map(|a| a.parse().ok()).collect();
        let mut s = self.servers.lock().unwrap();
        let e: Vec<Vec<u8>> = s
            .found
            .iter()
            .filter_map(|f| {
                codec::encode(
                    "Snapserver",
                    &[
                        ("name", Val::from(trim(&f.name, 63))),
                        ("host", Val::from(f.addr.to_string().as_str())),
                        ("port", Val::N(f.port as u32)),
                    ],
                )
                .ok()
            })
            .collect();
        if local.is_empty() {
            return b"!no network: no address on either interface".to_vec();
        }
        if !s.looking {
            s.looking = true;
            let shared = self.servers.clone();
            std::thread::spawn(move || {
                let found = zd1_mdns::browse_all(SNAPSERVER, &local, Duration::from_millis(1500));
                *shared.lock().unwrap() = Servers { found, looking: false };
            });
        }
        codec::encode_list(&fit(e))
    }

    /// The Pairings record: the controllers paired with AirPlay, the oldest first.
    fn pairings_record(&self) -> Vec<u8> {
        let e: Vec<Vec<u8>> = zd1_settings::pairings()
            .iter()
            .filter_map(|(id, admin)| {
                codec::encode("Pairing", &[("id", Val::from(trim(id, 63))), ("admin", Val::B(*admin))]).ok()
            })
            .collect();
        codec::encode_list(&fit(e))
    }

    /// The State record to the MCU when it changed: once a second (the rate limit), at the hello, and after a write
    /// from the host.
    fn push_state(&mut self) {
        if !self.greeted {
            return;
        }
        let r = self.state_record();
        if r != self.state_shown {
            self.notify(reg::STATE, &r);
            self.state_shown = r;
        }
    }

    /// The device name (0x58) from the settings, when it changed.
    fn publish_name(&mut self) {
        let n = zd1_settings::get("name").unwrap_or_default();
        if n != self.name {
            self.notify(reg::DEVICE_NAME, n.as_bytes());
            self.name = n;
        }
    }

    /// The MCU starts an update (reg 0xC5: size, baud, sha256, the stamp): zd1-update takes the console UART. One at
    /// a time: a second would read the same line.
    fn update(&mut self, p: &[u8]) {
        if self.update.is_some() {
            self.say("[zd1-linkd] update: one runs already: refused\n");
            return;
        }
        if p.len() < 41 {
            self.say("[zd1-linkd] update: a short request\n");
            return;
        }
        let size = u32::from_le_bytes(p[0..4].try_into().unwrap());
        let baud = match u32::from_le_bytes(p[4..8].try_into().unwrap()) {
            0 => 1_000_000,
            b => b,
        };
        let sha = hex(&p[8..40]);
        let id = String::from_utf8_lossy(&p[40..]).trim_end_matches('\0').to_string();
        self.say(&format!("[zd1-linkd] update: {size} bytes at {baud} baud, stamp \"{id}\"\n"));
        let log = OpenOptions::new().create(true).write(true).truncate(true).open("/var/log/zd1-update.log");
        let child = std::process::Command::new("zd1-update")
            .args([size.to_string(), baud.to_string(), sha, id])
            .stdin(std::process::Stdio::null())
            .stdout(log.as_ref().ok().and_then(|f| f.try_clone().ok()).map_or(std::process::Stdio::null(), Into::into))
            .stderr(log.ok().map_or(std::process::Stdio::null(), Into::into))
            .spawn();
        match child {
            Ok(c) => self.update = Some(c),
            Err(e) => self.say(&format!("[zd1-linkd] update: zd1-update: {e}\n")),
        }
    }

    /// Once zd1-update has ended: the console back if it couldn't give it back itself (killed, or a panic), and the
    /// slots' stamps again (a slot holds another system now).
    fn reap_update(&mut self) {
        let Some(c) = &mut self.update else { return };
        if !matches!(c.try_wait(), Ok(Some(_)) | Err(_)) {
            return;
        }
        self.update = None;
        zd1_update::restore();
        read_stamps(&self.stamps);
    }

    /// The debug services' mode from the MCU (0xC1 b0: 0 the image's default, 1 on, 2 off), handed to init, which
    /// decides with the image's default.
    fn debug_mode(&mut self, mode: u8) {
        let what = match mode {
            1 => "on",
            2 => "off",
            _ => "default",
        };
        if let Err(e) = zd1_svc::request(&format!("debug {what}")) {
            self.say(&format!("[zd1-linkd] debug {what}: {e}\n"));
        }
    }

    /// The receiver in front, its state and the receivers with a sender, to the MCU (0xC6), when any changed.
    fn publish_player(&mut self) {
        self.sessions.retain(|&(id, _)| self.clients.iter().any(|c| c.id == id)); // its app went away
        let on = self.sessions.iter().fold(0u8, |m, &(_, p)| m | 1 << p);
        if self.front.1 != player::PLAYING && self.front.0 != player::NONE && on & 1 << self.front.0 == 0 {
            self.front = (player::NONE, player::STOPPED); // paused in front, its sender (or its app) gone
        }
        let p = [self.front.0, self.front.1, on];
        if p != self.shown {
            self.shown = p;
            self.notify(reg::PLAYER, &p);
        }
    }
    fn set_front(&mut self, p: (u8, u8)) {
        self.front = p;
        self.publish_player();
    }

    /// A key from the speaker, for the receiver in front (else the only one with a sender): `key KEY SOURCE`. A key
    /// to everyone would reach every connected sender (next on Spotify skipping a paused AirPlay sender too).
    fn key(&mut self, key: &str) {
        let to = if self.front.0 != player::NONE {
            Some(self.front.0)
        } else if self.sessions.len() == 1 {
            Some(self.sessions[0].1)
        } else {
            None
        };
        match to.and_then(player::source_name) {
            Some(name) => self.event(&format!("key {key} {name}")),
            None => self.say(&format!("[zd1-linkd] key {key}: no receiver in front\n")),
        }
    }

    /// A receiver picked on a phone or in an app (`intent SOURCE`): in front, the others pause, and the MCU is told
    /// (0xC0) so it switches to the module at once and says so.
    fn intent(&mut self, src: &str) {
        let who = player::from_source(src);
        self.say(&format!("[zd1-linkd] {src} picked\n"));
        if self.owner.as_deref().is_some_and(|o| o != src) {
            self.owner = None;
        }
        self.event(&format!("yield {src}"));
        if self.front != (who, player::PLAYING) {
            self.set_front((who, player::PAUSED));
        }
        self.intents = self.intents.wrapping_add(1);
        self.notify(reg::INTENT, &[who, self.intents]);
    }

    /// A receiver chosen on the speaker (0xC2): the others pause their senders, that one resumes.
    fn select(&mut self, who: u8) {
        let Some(name) = player::source_name(who) else { return };
        self.say(&format!("[zd1-linkd] {name} chosen on the speaker\n"));
        if self.owner.as_deref().is_some_and(|o| o != name) {
            self.owner = None;
        } // being paused
        self.event(&format!("yield {name}"));
        self.event(&format!("select {name}"));
        if self.front != (who, player::PLAYING) {
            self.set_front((who, player::PAUSED));
        }
    }

    fn publish_net(&mut self, n: Net) {
        let up = !n.ip().is_empty();
        self.notify(reg::NET_STATUS, if up { &net::CONNECTED } else { &net::DOWN });
        if up {
            self.notify(reg::IP, &cstr(n.ip()));
            self.event(&format!("net up {}", n.ip()));
        } else {
            self.event("net down");
        }
        self.net = n;
    }

    /// A line from an app (client `id`).
    fn command(&mut self, id: u64, line: &str) {
        let (cmd, arg) = line.split_once(' ').unwrap_or((line, ""));
        match cmd {
            "intent" => self.intent(arg.trim()),
            "session" => {
                // `session on|off SOURCE`: a sender connected / gone
                let (on, src) = arg.split_once(' ').unwrap_or((arg, "app"));
                let who = player::from_source(src.trim());
                self.sessions.retain(|&(i, p)| !(i == id && p == who));
                if on == "on" {
                    self.sessions.push((id, who));
                }
                self.publish_player();
            }
            "state" => {
                // one speaker, several sources: the newest `playing` owns it; the others are told to let go of the
                // audio device (`yield`) and get it back when the owner stops (`resume`). An app that names no
                // source is "app". Only the owner's stop / pause reaches the MCU while one plays.
                // The receiver in front (0xC6): the owner while one plays, else the last one paused or chosen until
                // it stops; only its pause / stop reaches the MCU (one waiting to resume after a choice on the
                // speaker isn't overruled by the one it replaced stopping).
                let (st, src) = arg.split_once(' ').unwrap_or((arg, "app"));
                let src = src.trim();
                let who = player::from_source(src);
                match st {
                    "playing" => {
                        if self.owner.as_deref() != Some(src) {
                            self.event(&format!("yield {src}"));
                            self.owner = Some(src.to_string());
                        }
                        self.set_front((who, player::PLAYING));
                    }
                    "paused" | "stopped" => {
                        // stopped with its sender still connected (an AirPlay stream torn down on pause, the session
                        // kept): paused, as a person sees it
                        let st =
                            if st == "stopped" && self.sessions.iter().any(|&(_, p)| p == who) { "paused" } else { st };
                        match self.owner.as_deref() {
                            Some(o) if o != src => return, // not the one playing: keep the speaker's state
                            Some(_) => {
                                self.owner = None;
                                self.event(&format!("resume {src}"));
                            }
                            None => {}
                        }
                        if self.front.0 != player::NONE && self.front.0 != who {
                            return;
                        }
                        self.set_front(if st == "paused" {
                            (who, player::PAUSED)
                        } else {
                            (player::NONE, player::STOPPED)
                        });
                    }
                    _ => self.say(&format!("[zd1-linkd] unknown command: {line}\n")),
                }
            }
            "volume" => {
                if let Ok(p) = arg.parse::<u32>() {
                    self.set_volume(id, p)
                }
            }
            "log" => {
                let s = format!("{arg}\n");
                self.say(&s);
            }
            _ => self.say(&format!("[zd1-linkd] unknown command: {line}\n")),
        }
    }

    /// A write from the MCU.
    fn mcu(&mut self, e: zd1_link::Event) {
        match (e.reg, e.v) {
            (reg::SPEAKER, true) => self.speaker(e.payload()),
            (reg::KEY, false) => match native::key_name((e.value >> 8) as u8) {
                Some(k) => self.key(k),
                None => self.say(&format!("[zd1-linkd] key 0x{:04x}?\n", e.value)),
            },
            (reg::CONFIG, true) => self.config_request(e.payload()),
            (reg::SELECT, false) => self.select((e.value >> 8) as u8),
            (reg::DEBUG, false) => self.debug_mode((e.value >> 8) as u8),
            (reg::UPDATE, true) => self.update(e.payload()),
            (reg::BOOT, false) => {
                // at ready: the good slot now (zd1-slot)
                let good = match e.value as u8 {
                    boot::SLOT_A => zd1_slot::Slot::A,
                    boot::SLOT_B => zd1_slot::Slot::B,
                    _ => return,
                };
                let _ = std::fs::create_dir_all("/run/zd1");
                let _ = std::fs::write("/run/zd1/good", format!("{}\n", good.name()));
                self.good = Some(good);
            }
            (reg::MCU_VERSION, true) => {
                let v = String::from_utf8_lossy(e.payload()).trim_end_matches('\0').to_string();
                let _ = std::fs::create_dir_all("/run/zd1");
                let _ = std::fs::write("/run/zd1/mcu-version", format!("{v}\n"));
                self.say(&format!("[zd1-linkd] MCU firmware {v}\n"));
                // every hello gets our state (the speaker's comes after it, 0xD2), not only the first
                self.greeted = true;
                self.mcu = v;
                let p = self.shown;
                self.notify(reg::PLAYER, &p);
                self.state_shown.clear();
                self.push_state();
            }
            (reg::SHUTDOWN, _) => {
                // the MCU is about to cut our power
                self.say("[zd1-linkd] shutdown requested by the MCU\n");
                let _ = std::process::Command::new("sync").status();
                self.notify(reg::SHUTDOWN, &[0x01, 0x00]);
            }
            _ => {}
        }
    }

    /// The configuration channel (0x45, zd1-spi codec::config): reads reply by notification on 0x45. Names
    /// "zd1/KEY" are our settings (zd1-settings; docs/module/settings.md): a write stores and applies it, a read
    /// answers the value in effect (secrets as "(set)"), or "!why" once after a write was refused; "zd1/?" the keys.
    fn config_request(&mut self, p: &[u8]) {
        let Some(req) = config::parse(p) else { return };
        let name = String::from_utf8_lossy(req.name).into_owned();
        let value = String::from_utf8_lossy(req.value).into_owned();
        if let Some(n) = name.strip_prefix("zd1/@") {
            // a record by number (docs/module/settings.md)
            if !req.op.is_read() {
                return;
            }
            let r = match n.parse::<u8>().map(Record::from) {
                Ok(Record::State) => self.state_record(),
                Ok(Record::Scan) => self.scan_record(),
                Ok(Record::Networks) => self.networks_record(),
                Ok(Record::Servers) => self.servers_record(),
                Ok(Record::Pairings) => self.pairings_record(),
                _ => format!("!no record {n}").into_bytes(),
            };
            self.notify(reg::CONFIG, &r);
            return;
        }
        if let Some(key) = name.strip_prefix("zd1/") {
            if req.op.is_read() {
                let refused = self.setting_err.as_ref().is_some_and(|(k, _)| k == key);
                let v = if refused {
                    format!("!{}", self.setting_err.take().unwrap().1)
                } else if key == "?" {
                    zd1_settings::fields().chain(zd1_hostlink::actions()).map(|f| f.key()).collect::<Vec<_>>().join(" ")
                } else if zd1_settings::action(key).is_some() {
                    String::new() // an action: no state of its own
                } else {
                    zd1_settings::get(key)
                        .map(|v| zd1_settings::shown(key, &v))
                        .unwrap_or_else(|| format!("!no setting {key}"))
                };
                self.notify(reg::CONFIG, v.as_bytes());
            } else {
                let action = zd1_settings::action(key).is_some();
                let r = if action { zd1_settings::act(key, &value) } else { zd1_settings::set(key, &value) };
                match r {
                    Ok(()) => {
                        self.say(&format!(
                            "[zd1-linkd] {} {key} = {}\n",
                            if action { "action" } else { "setting" },
                            zd1_settings::shown(key, &value)
                        ));
                        zd1_settings::apply(key);
                        if key == "name" || key == "reset" {
                            self.publish_name();
                        }
                        // the record now, not within the second: queued before the read-back's answer, so a host
                        // that reads it after its write sees the write
                        self.push_state();
                    }
                    Err(e) => {
                        self.say(&format!("[zd1-linkd] setting refused: {e}\n"));
                        self.setting_err = Some((key.to_string(), e));
                    }
                }
            }
            return;
        }
        // not ours (the stock MCU firmware's Cne paths): its value never in the log, a read gets "!"
        self.say(&format!("[zd1-linkd] config {name}: not a zd1/ setting\n"));
        if req.op.is_read() {
            self.notify(reg::CONFIG, format!("!not a zd1/ setting: {name}").as_bytes());
        }
    }

    fn accept(&mut self, l: &UnixListener) {
        while let Ok((s, _)) = l.accept() {
            let _ = s.set_nonblocking(true);
            self.next_id += 1;
            let mut c = Client { s, buf: Vec::new(), id: self.next_id };
            let mut hello = format!("audio {}\n", if self.audio_on { "on" } else { "off" });
            if let Some(v) = self.volume {
                hello += &format!("volume {v}\n");
            }
            hello +=
                &if self.net.ip().is_empty() { "net down\n".into() } else { format!("net up {}\n", self.net.ip()) };
            let _ = c.s.write_all(hello.as_bytes()); // a one-shot `zd1-ctl CMD` may be gone already: read it anyway
            self.clients.push(c);
        }
    }

    fn read_clients(&mut self) {
        let mut lines = Vec::new();
        let mut buf = [0u8; 512];
        let before = self.clients.len();
        self.clients.retain_mut(|c| {
            loop {
                match c.s.read(&mut buf) {
                    Ok(0) => return false,
                    Ok(n) => {
                        c.buf.extend_from_slice(&buf[..n]);
                        while let Some(i) = c.buf.iter().position(|&b| b == b'\n') {
                            let l: Vec<u8> = c.buf.drain(..=i).collect();
                            lines.push((c.id, String::from_utf8_lossy(&l).trim().to_string()));
                        }
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => return true,
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(_) => return false,
                }
            }
        });
        for (id, l) in lines {
            if !l.is_empty() {
                self.command(id, &l);
            }
        }
        if self.clients.len() != before {
            self.publish_player();
        } // an app went: its sessions with it
    }
}

pub fn main() {
    let mut mem = match DevMem::open() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("zd1-linkd: /dev/mem: {e}");
            std::process::exit(1);
        }
    };
    let _ = std::fs::create_dir_all("/run");
    let _ = std::fs::remove_file(SOCKET);
    let listener = UnixListener::bind(SOCKET).and_then(|l| {
        l.set_nonblocking(true)?;
        Ok(l)
    });
    if let Err(e) = &listener {
        eprintln!("zd1-linkd: {SOCKET}: {e}");
    }
    if !pads(&mut mem).holds(&pads::BL2) {
        eprintln!("zd1-linkd: the pads aren't routed (init's zd1-board up didn't run): the MCU won't hear us");
    }
    let mut app = App {
        link: Link::new(mem),
        setting_err: None,
        name: String::new(),
        clients: Vec::new(),
        net: Net::default(),
        volume: None,
        speaker: None,
        audio_on: false,
        owner: None,
        front: (player::NONE, player::STOPPED),
        sessions: Vec::new(),
        shown: [0; 3],
        next_id: 0,
        intents: 0,
        greeted: false,
        ident: os_ident(),
        slot: zd1_slot::boot().running,
        boot: zd1_slot::boot().image,
        good: None,
        mcu: String::new(),
        state_shown: Vec::new(),
        wifi: watch_wifi(),
        servers: Arc::default(),
        stamps: Arc::default(),
        update: None,
    };
    read_stamps(&app.stamps);
    app.say("[zd1-linkd] MCU link up (SPI slave via /dev/mem)\n");
    let ident = app.ident.clone();
    app.link.set_value(reg::VERSION_IMAGE, &cstr(&ident));
    app.link.set_reg(reg::LINK_PROTO, native::PROTO as u16);
    app.say(&format!("[zd1-linkd] {ident}\n"));
    app.publish_name();
    let mut settings_mtime = mtime(zd1_settings::FILE);
    app.notify(reg::PLAYER, &[player::NONE, player::STOPPED, 0]);
    let n = net_state();
    app.publish_net(n);
    let mut last_activity = timers(app.link.mmio()).now();
    let mut last_net = Instant::now();
    #[cfg(feature = "debug")]
    let mut trace = std::env::var_os("ZD1LINKD_TRACE").map(|_| 0usize); // every MCU request on the console
    loop {
        let before = app.link.activity();
        app.link.poll();
        if app.link.bl2_requested() {
            app.say("[zd1-linkd] HCR 0x21: rebooting into BL2\n");
            app.link.reboot_to_bl2();
        }
        while let Some(e) = app.link.next_event() {
            app.mcu(e);
        }
        #[cfg(feature = "debug")]
        if let Some(traced) = &mut trace {
            let (n, log) = app.link.requests();
            for k in (*traced).max(n.saturating_sub(32))..n {
                let (t, r, a) = log[k % 32];
                let what = ["write F", "read F", "write V", "read V"].get(t as usize).copied().unwrap_or("?");
                println!("[zd1-linkd] MCU {what} 0x{r:02x} {a:04x}");
            }
            *traced = n;
        }
        if let Ok(l) = &listener {
            app.accept(l);
        }
        app.read_clients();
        if last_net.elapsed() >= Duration::from_secs(1) {
            last_net = Instant::now();
            let n = net_state();
            if n != app.net {
                app.publish_net(n);
            }
            let m = mtime(zd1_settings::FILE); // set from a shell (zd1-settings): the name again
            if m != settings_mtime {
                settings_mtime = m;
                app.publish_name();
            }
            app.reap_update();
            app.push_state();
        }
        if app.link.activity() != before {
            last_activity = timers(app.link.mmio()).now();
        }
        let quiet = timers(app.link.mmio()).us_since(last_activity);
        if app.link.rx_busy() && quiet > 100_000 {
            // bytes lost (the RX FIFO overran while we were away): the MCU misses the write's echo and sends it again
            app.link.drop_partial();
            app.say("[zd1-linkd] a request half received, then nothing for 100 ms: dropped (bytes lost?)\n");
        }
        if (app.link.busy() && quiet < 20_000) || quiet < 3_000 {
            unsafe {
                libc::sched_yield();
            } // mid-transfer: keep the bytes flowing
        } else {
            std::thread::sleep(Duration::from_millis(1)); // one tick
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Net;

    fn net(eth: &str, wifi: &str, eth_link: bool) -> Net {
        Net { eth: eth.into(), wifi: wifi.into(), eth_link }
    }

    #[test]
    fn the_address_is_the_network_the_speaker_is_on() {
        // no cable: eth0's fallback is no network
        assert_eq!(net("169.254.1.1", "192.0.2.40", false).ip(), "192.0.2.40");
        assert_eq!(net("169.254.1.1", "", false).ip(), "");
        // a cable with a lease wins
        assert_eq!(net("192.0.2.7", "192.0.2.40", true).ip(), "192.0.2.7");
        // a cable without a DHCP server: the Wi-Fi's if there is one, else the link-local one
        assert_eq!(net("169.254.1.1", "192.0.2.40", true).ip(), "192.0.2.40");
        assert_eq!(net("169.254.1.1", "", true).ip(), "169.254.1.1");
    }
}
