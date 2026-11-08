//! Stock-MCU mode (docs/stock-mcu.md): our OS on a Zeppelin Air with Lightning whose MCU runs B&W's firmware. The MCU
//! never greets us (0xC3), so the base layer's stock-safe queueing holds for good (`App::notify`), and this is the
//! dialect B&W's MCU app expects from its network module (docs/protocol/spi-registers.md §4.2):
//!   to the MCU:   the stock versions (0x25 with "9112" where its gate looks, 0x26, 0x27), the play state as DRC XML
//!                 (0x47: `pl` switches the speaker to Wi-Fi), the volume as an attenuation (0xE4 `03 03 01 vv`), the
//!                 acks of its DRC commands (0xE3), network status and address (0x32, 0x38: as in our dialect)
//!   from the MCU: DRC commands (0x48: next / prev / play-pause, pause / resume / release / enable of the audio path,
//!                 JBConnect), its volume (0x40), the configuration channel (0x45: `pssCne/CneId`, its HostVersion, and
//!                 the restore of the network settings it backed up from the stock app: the Wi-Fi it knew)
//! Nothing here has run against a unit yet: the emulator first (`zemu run` with the Lightning's MCU app).

use zd1_link::spi::codec::{play, volume};
use zd1_link::spi::spec::{Drc, player};

/// The stock app's versions, as the stock MCU reads them in its setup task (spi-registers.md §2.3): VersionImage must
/// have "9112" in characters 13..16 once its spaces are gone, or the MCU raises system error 7.
pub const VERSION_IMAGE: &[u8] = b"I2014092908219112\0";
pub const VERSION_BOOT: (u8, &[u8]) = (0x26, b"B201009271404 081008");
pub const VERSION_CNE: (u8, &[u8]) = (0x27, b"C 2014092908219112\0");

/// What `pssCne/CneId` answers (the MCU stores it as version information; at most 10 bytes).
pub const CNE_ID: &[u8] = b"zd1";

/// Whether this boot is stock-MCU mode (`zd1.mcu=stock` on the kernel command line: the Lightning's image).
pub fn mode() -> bool {
    std::fs::read_to_string("/proc/cmdline").is_ok_and(|c| c.split_whitespace().any(|w| w == "zd1.mcu=stock"))
}

/// The play icon for the receiver in front's state (0xC6 [1] in our dialect).
pub fn play_of(state: u8) -> play::Play {
    match state {
        player::PLAYING => play::Play::Playing,
        player::PAUSED => play::Play::Paused,
        _ => play::Play::Empty,
    }
}

/// The DRC XML the stock app sends on 0x47.
pub fn play_xml(p: play::Play) -> Vec<u8> {
    let mut v = Vec::new();
    play::xml(p, &mut |b| v.extend_from_slice(b));
    v
}

/// The volume message for 0..100 % (0xE4).
pub fn volume_message(percent: u32) -> [u8; 4] {
    volume::encode(volume::att_from_percent(percent.min(100) as u8))
}

/// 0..100 % from the MCU's volume message (0x40), if it is one.
pub fn percent_of(p: &[u8]) -> Option<u32> {
    volume::decode(p).map(|att| volume::percent_from_att(att) as u32)
}

/// What a DRC command means for us.
pub enum Command {
    /// A key for the receiver in front (`key KEY SOURCE`): "next", "prev", "playpause".
    Key(&'static str),
    /// The speaker's audio path is ours (true) or not (false): `audio on|off` to the receivers.
    Audio(bool),
    /// The MCU wants the wireless settings applied again (after it wrote `Application/NetIF/mode`).
    WirelessApply,
    /// JBConnect, the stock setup network: not ours (Wi-Fi comes from the MCU's restore or our settings).
    Ignored(&'static str),
}

/// A DRC command (0x48 b0): what it means, and the ack it wants on 0xE3 (b0), if any.
pub fn drc(code: u8) -> Option<(Command, Option<u8>)> {
    let d = Drc::from_code(code)?;
    let c = match d {
        Drc::Next => Command::Key("next"),
        Drc::Prev => Command::Key("prev"),
        Drc::PlayPause => Command::Key("playpause"),
        Drc::Pause | Drc::Release => Command::Audio(false),
        Drc::Play | Drc::Enable => Command::Audio(true),
        Drc::WirelessApply => Command::WirelessApply,
        Drc::JbcStart | Drc::JbcStop => Command::Ignored(d.name()),
    };
    Some((c, d.ack()))
}

/// The Wi-Fi the stock MCU restores through the configuration channel (W `Networking/…`, spi-registers.md §3.7): its
/// backup of the stock app's network settings. Profile 1's SSID and WPA passphrase become a known network.
#[derive(Default)]
pub struct Restore {
    ssid: Option<String>,
    passphrase: Option<String>,
}

impl Restore {
    /// Take a configuration write; true if it was one of ours (the value never goes to the log).
    pub fn take(&mut self, name: &str, value: &str) -> bool {
        if name.ends_with("/Profile1/SSID") {
            self.ssid = Some(value.to_string());
        } else if name.ends_with("/Profile1/WPAPSK/Passphrase") {
            self.passphrase = Some(value.to_string());
        } else {
            return false;
        }
        true
    }

    /// The network as zd1-settings' `wifi.add` takes it (`wpa2 KEY SSID`, the key derived here as wpa_passphrase
    /// does), once both are known; then forgotten.
    pub fn network(&mut self) -> Option<String> {
        let (ssid, pass) = (self.ssid.as_deref()?, self.passphrase.as_deref()?);
        if ssid.is_empty() || ssid.len() > 32 || !(8..=63).contains(&pass.len()) {
            return None;
        }
        let mut k = [0u8; 32];
        pbkdf2::pbkdf2_hmac::<sha1::Sha1>(pass.as_bytes(), ssid.as_bytes(), 4096, &mut k);
        let n = format!("wpa2 {} {ssid}", zd1_sys::hex(&k));
        *self = Restore::default();
        Some(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_passes_the_stock_gate() {
        // spaces removed, characters 13..16 must be "9112" (spi-registers.md §2.3)
        let s: Vec<u8> = VERSION_IMAGE.iter().copied().filter(|&c| c != b' ' && c != 0).collect();
        assert_eq!(&s[13..17], b"9112");
    }

    #[test]
    fn the_play_state_is_the_stock_xml() {
        assert_eq!(play_xml(play_of(player::PLAYING)), b"<update id=\"status\"><icon id=\"play\">pl</icon></update>");
        assert_eq!(play_xml(play_of(player::STOPPED)), b"<update id=\"status\"><icon id=\"play\">em</icon></update>");
    }

    #[test]
    fn drc_acks_are_the_stock_ones() {
        let ack = |c| drc(c).unwrap().1;
        assert_eq!((ack(0xDE), ack(0xDF), ack(0xE1), ack(0xE2)), (Some(1), Some(0), Some(4), Some(5)));
        assert_eq!(ack(0xB0), None);
    }

    #[test]
    fn the_volume_goes_both_ways() {
        assert_eq!(volume_message(100), [3, 3, 1, 0]);
        assert_eq!(percent_of(&volume_message(100)), Some(100));
        assert_eq!(percent_of(&[3, 3, 1, 0xFF]), Some(0));
    }

    #[test]
    fn a_restored_network_becomes_a_known_one() {
        // the IEEE 802.11i test vector: "password" / "IEEE"
        let mut r = Restore::default();
        assert!(r.take("Networking/WlanCfg/Profile1/SSID", "IEEE"));
        assert!(r.take("Networking/WlanCfg/Profile1/WPAPSK/Passphrase", "password"));
        assert!(!r.take("Networking/IpCfg/DeviceName", "x"));
        assert_eq!(
            r.network().unwrap(),
            "wpa2 f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e IEEE"
        );
        assert!(r.network().is_none(), "forgotten once handed over");
    }
}
