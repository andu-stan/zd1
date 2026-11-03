//! What a person asks of the speaker and reads about it, once for both front-ends: the play choices as `Select`
//! requests, and the state in words (the Wi-Fi, the amp, the gate).

use crate::msg;
use crate::spec::{Gate, NetMode, Player, Source, WifiState};

/// The play choices: a source, a receiver of the module OS (its sender resumes), or standby.
pub const PLAY: &[&str] = &["usb", "aux", "wifi", "ipod", "airplay", "spotify", "snapcast", "standby"];

/// The `Select` that plays `choice` (one of [`PLAY`]).
pub fn select(choice: &str) -> Option<msg::Select> {
    let (source, player) = match choice {
        "usb" => (Source::Usb, Player::None),
        "aux" => (Source::Aux, Player::None),
        "wifi" => (Source::Wifi, Player::None),
        "ipod" => (Source::Ipod, Player::None),
        "airplay" => (Source::Wifi, Player::Airplay),
        "spotify" => (Source::Wifi, Player::Spotify),
        "snapcast" => (Source::Wifi, Player::Snapcast),
        "standby" => (Source::None, Player::None),
        _ => return None,
    };
    Some(msg::Select { source, player })
}

/// How a sentence reads: as it is, good news, or something to act on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Plain,
    Ok,
    Warn,
}

impl Tone {
    pub fn name(self) -> &'static str {
        match self {
            Tone::Plain => "",
            Tone::Ok => "ok",
            Tone::Warn => "warn",
        }
    }
}

/// The Wi-Fi as a sentence, from the State record: joined or not, and the plain reason when not. `known`: the
/// networks the speaker knows (`settings::networks`).
pub fn wifi(st: &msg::State, known: &[msg::KnownNetwork]) -> (String, Tone) {
    if st.wifi_mode == NetMode::Off {
        return ("Off (wifi.mode).".into(), Tone::Plain);
    }
    let ssid = |or: &str| if st.wifi_ssid.is_empty() { or.to_string() } else { st.wifi_ssid.clone() };
    let looking = known.iter().map(|n| n.ssid.as_str()).collect::<Vec<_>>().join(", ");
    match st.wifi {
        WifiState::Connected => (
            format!(
                "On {}{} (access point {}), {}.",
                st.wifi_ssid,
                if st.signal != 0 { format!(" at -{} dBm", st.signal) } else { String::new() },
                st.bssid,
                if st.wifi_ip.is_empty() { "waiting for an address".into() } else { format!("address {}", st.wifi_ip) }
            ),
            Tone::Ok,
        ),
        WifiState::Connecting => (format!("Joining {}…", ssid("a network")), Tone::Plain),
        WifiState::Scanning => {
            (format!("Looking for {}…", if looking.is_empty() { "a known network" } else { &looking }), Tone::Plain)
        }
        WifiState::Idle => ("No network known yet: the radio idles (it scans when asked).".into(), Tone::Plain),
        WifiState::WrongKey => {
            (format!("{}: the passphrase is wrong; join it again with the right one.", ssid("The network")), Tone::Warn)
        }
        WifiState::NotFound => (
            format!(
                "None of the known networks is in range{}. The radio is 2.4 GHz only.",
                if looking.is_empty() { String::new() } else { format!(" ({looking})") }
            ),
            Tone::Warn,
        ),
        WifiState::Failed => {
            (format!("Joining {} failed: the access point refused the speaker.", ssid("the network")), Tone::Warn)
        }
        WifiState::Off => ("The radio is off (no Wi-Fi hardware answered).".into(), Tone::Warn),
        WifiState::Unlisted(v) => (format!("State {v}."), Tone::Plain),
    }
}

/// The amp now, and its lock: "on", "asleep: the source is silent", "locked until the speaker restarts", ...
pub fn amp(i: &msg::InfoReply) -> String {
    if !i.amp_allowed {
        return if i.amp_auto { "locked until the speaker restarts" } else { "locked, at every start too" }.into();
    }
    let now = if i.amp_asleep {
        "asleep: the source is silent"
    } else if i.amp_on {
        "on"
    } else {
        "off"
    };
    if i.amp_auto { now.into() } else { format!("{now} (allowed until the speaker restarts, locked at every start)") }
}

/// The gate in front of the requests that change the speaker.
pub fn gate(i: &msg::InfoReply) -> String {
    match i.gate {
        Gate::Locked => "locked: each change asks for a press of ON/OFF on the speaker".into(),
        Gate::Unlocked => format!("unlocked on the speaker for {} s more", i.gate_s),
        Gate::Open => "open for good: no press needed (kept in the EEPROM)".into(),
        Gate::Unlisted(v) => format!("state {v}"),
    }
}
