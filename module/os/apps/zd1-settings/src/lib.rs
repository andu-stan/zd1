//! The module OS's settings: what the speaker's owner chose (its name, the Wi-Fi networks, which receivers run), on
//! the data partition (JFFS2, docs/module/settings.md): the settings one `KEY=VALUE` line each in /data/zd1/settings,
//! the known Wi-Fi networks in /data/zd1/networks. Set from the host through the MCU (`zd1 settings`: the
//! configuration channel, zd1-linkd) or with `zd1-settings set` on the module; the init scripts and zd1-linkd read
//! them. A key that isn't set has its default. Nothing on the network can change them. The keys, their types, formats
//! and defaults are the State record's writable fields in the protocol spec (protocol/host/src/lib.rs), the actions
//! (a write with no state of its own: a network added, forgotten or put first, AirPlay's pairings forgotten, back to
//! the defaults) its `Actions` message's: one declaration for the module, the host and the page.
//!
//! Downgrades: an older OS meets a file a newer one wrote. The file stays `KEY=VALUE` lines for good; a newer OS adds
//! keys and values, and never changes what an existing key or value means (a different meaning gets a new key). So
//! this version reads the keys it knows with values it accepts and takes the default for anything else (a key it
//! doesn't know, a value it can't check), and a change rewrites only the changed key's line: the rest stays as it was,
//! for the newer OS to find again after an upgrade.

use std::collections::BTreeMap;
use std::path::Path;
pub use zd1_hostlink::codec::Val;
pub use zd1_hostlink::{Field, NetMode, Ty};
use zd1_sys::{hex, unhex};

pub const DIR: &str = "/data/zd1";
pub const FILE: &str = "/data/zd1/settings";
/// The known Wi-Fi networks, the preferred first: a line each, `KEY SSIDHEX` (KEY: 64 hex digits, or `-` for an open
/// network; the SSID in hex: any bytes).
pub const NETWORKS: &str = "/data/zd1/networks";
/// zd1-airplay's pairing store (its `peers`: `ID HEXKEY admin|user` a line, the oldest first; its `identity`).
pub const AIRPLAY: &str = "/data/zd1/airplay";

/// The settings: the State record's writable fields (protocol/host/src/lib.rs, `State`), in the spec's order.
pub fn fields() -> impl Iterator<Item = &'static Field> {
    zd1_hostlink::settings()
}

/// A setting by its key (`wifi.mode`: the field's name with dots).
pub fn field(key: &str) -> Option<&'static Field> {
    zd1_hostlink::setting(key)
}

/// An action by its key (`wifi.add`).
pub fn action(key: &str) -> Option<&'static Field> {
    zd1_hostlink::action(key)
}

/// Never shown: read back as "(set)", or with the key left out.
pub fn secret(f: &Field) -> bool {
    f.format == "psk" || f.format == "network"
}

fn ip(s: &str) -> bool {
    s.parse::<std::net::Ipv4Addr>().is_ok()
}

fn hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Whether `v` (in its string form) is a valid value for `f` (empty always is: back to the default): its type, its
/// maximum, its format.
pub fn check(f: &Field, v: &str) -> Result<(), String> {
    if v.is_empty() {
        return Ok(());
    }
    if v.chars().any(|c| c.is_control()) {
        return Err("control characters".into());
    }
    match f.ty {
        Ty::Str(max) if v.len() > max as usize => return Err(format!("at most {max} bytes")),
        Ty::Bool if v != "on" && v != "off" => return Err("on or off".into()),
        Ty::Enum(e) if zd1_hostlink::codec::enum_value(e, v).is_none() => {
            let names: Vec<String> = zd1_hostlink::enum_(e)
                .map(|e| e.values.iter().map(|v| v.0.to_lowercase()).collect())
                .unwrap_or_default();
            return Err(format!("one of {}", names.join(", ")));
        }
        Ty::U8 if v.parse::<u8>().is_err() => return Err("a number, 0 to 255".into()),
        Ty::U16 if v.parse::<u16>().is_err() => return Err("a number, 0 to 65535".into()),
        Ty::U32 if v.parse::<u32>().is_err() => return Err("a number".into()),
        _ => {}
    }
    match f.format {
        "psk" if !hex64(v) => Err("64 hex digits (the host derives them from the passphrase)".into()),
        "ip" if !ip(v) => Err("an IPv4 address".into()),
        "cidr" if !matches!(v.split_once('/'), Some((a, p)) if ip(a) && p.parse::<u8>().is_ok_and(|p| (1..=32).contains(&p))) => {
            Err("ADDRESS/PREFIX".into())
        }
        "host"
            if v.contains(' ')
                || v.rsplit_once(':').is_some_and(|(h, p)| h.is_empty() || p.parse::<u16>().is_err()) =>
        {
            Err("HOST or HOST:PORT".into())
        }
        "pin" if !(v.len() == 4 && v.bytes().all(|b| b.is_ascii_digit())) => Err("4 digits".into()),
        "bitrate" if !matches!(v, "96" | "160" | "320") => Err("96, 160 or 320".into()),
        "network" if parse_network(v).is_none() => Err("`wpa2 KEY SSID` (KEY 64 hex digits) or `open SSID`".into()),
        "reset" if !matches!(v, "settings" | "all") => Err("settings or all".into()),
        _ => Ok(()),
    }
}

/// What is stored and this version understands (not the defaults): known keys with values `check` accepts.
pub fn load_from(path: &str) -> BTreeMap<String, String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('='))
        .filter(|(k, v)| field(k).is_some_and(|f| !v.is_empty() && check(f, v).is_ok()))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

pub fn load() -> BTreeMap<String, String> {
    load_from(FILE)
}

/// Store `v` for `k` (empty: forget it), the file replaced in one rename (JFFS2 makes that atomic: a power cut leaves
/// the old file or the new one). Only `k`'s line changes; every other line is kept as it is, a newer OS's included.
pub fn set_in(path: &str, name: &str, v: &str) -> Result<(), String> {
    let f = field(name).ok_or_else(|| format!("no setting {name}"))?;
    check(f, v).map_err(|e| format!("{name}: {e}"))?;
    put_lines(path, &[(name, v)])
}

/// The lines for these keys replaced (an empty value: the line gone), the others kept.
fn put_lines(path: &str, set: &[(&str, &str)]) -> Result<(), String> {
    let old = std::fs::read_to_string(path).unwrap_or_default();
    let mut lines: Vec<String> = old
        .lines()
        .filter(|l| l.split_once('=').is_none_or(|(n, _)| !set.iter().any(|(k, _)| *k == n)))
        .map(str::to_string)
        .collect();
    for (k, v) in set {
        if !v.is_empty() {
            lines.push(format!("{k}={v}"));
        }
    }
    write_private(path, &lines.iter().map(|l| format!("{l}\n")).collect::<String>())
}

/// A file of ours written whole (zd1_sys::write_private: a power cut leaves the old one or the new one, only root
/// reads it).
fn write_private(path: &str, text: &str) -> Result<(), String> {
    zd1_sys::write_private(path, text).map_err(|e| e.to_string())
}

pub fn set(name: &str, v: &str) -> Result<(), String> {
    set_in(FILE, name, v)
}

/// Tell init a setting changed or an action was done: it restarts the services that use it (module/os/apps/zd1-init,
/// services.rs) once the burst has settled; `reset` restarts everything that uses a setting. It only schedules the
/// restarts, so the caller (zd1-linkd) doesn't wait for them.
pub fn apply(name: &str) {
    let keys = match name {
        "reset" => fields().map(|f| f.key()).chain(actions_keys()).collect::<Vec<_>>().join(" "),
        _ if field(name).is_some() || action(name).is_some() => name.to_string(),
        _ => return,
    };
    if let Err(e) = zd1_svc::changed(&keys) {
        eprintln!("zd1-settings: {name}: {e}");
    }
}

fn actions_keys() -> impl Iterator<Item = String> {
    zd1_hostlink::actions().map(|f| f.key())
}

/// The name before anyone chose one: "Zeppelin Air XXXXXX" from the unit's MAC.
pub fn default_name() -> String {
    zd1_sys::default_name(zd1_persparam::unit_mac().ok())
}

/// The value in effect: the stored one, else the default.
pub fn get_from(all: &BTreeMap<String, String>, name: &str) -> Option<String> {
    let f = field(name)?;
    Some(match all.get(name) {
        Some(v) => v.clone(),
        None if name == "name" => default_name(),
        None => f.default.to_string(),
    })
}

pub fn get(name: &str) -> Option<String> {
    get_from(&load(), name)
}

/// How a value is shown: secrets as "(set)"; a network added with its key left out (`wpa2 (key) SSID`).
pub fn shown(name: &str, v: &str) -> String {
    let Some(f) = field(name).or_else(|| action(name)) else { return v.to_string() };
    match (f.format, parse_network(v)) {
        ("network", Some(n)) => format!("{} {}", if n.psk.is_some() { "wpa2 (key)" } else { "open" }, n.ssid),
        _ if secret(f) && !v.is_empty() => "(set)".into(),
        _ => v.to_string(),
    }
}

/// The settings as the State record carries them: (field name, value) for each, the stored value or the default,
/// a secret as "(set)" or empty; for the module's `codec::encode("State", ...)`.
pub fn record_values(all: &BTreeMap<String, String>) -> Vec<(&'static str, Val)> {
    fields()
        .map(|f| {
            let key = f.key();
            let v = get_from(all, &key).unwrap_or_default();
            let val = match f.ty {
                Ty::Bool => Val::B(v == "on"),
                Ty::Enum(e) => Val::N(zd1_hostlink::codec::enum_value(e, &v).unwrap_or(0) as u32),
                Ty::U8 | Ty::U16 | Ty::U32 => Val::N(v.parse().unwrap_or(0)),
                _ => Val::from(shown(&key, &v)),
            };
            (f.name, val)
        })
        .collect()
}

/// An interface's address settings (`eth` or `wifi`): the mode, and with static the address, gateway and name server.
pub struct Address {
    pub mode: NetMode,
    pub address: String,
    pub gateway: String,
    pub dns: String,
}

pub fn address(all: &BTreeMap<String, String>, iface: &str) -> Address {
    let g = |k: &str| get_from(all, &format!("{iface}.{k}")).unwrap_or_default();
    let mode = zd1_hostlink::codec::enum_value("NetMode", &g("mode")).map_or(NetMode::Dhcp, NetMode::from);
    Address { mode, address: g("address"), gateway: g("gateway"), dns: g("dns") }
}

// ---- the known Wi-Fi networks ----

/// A Wi-Fi network the speaker knows: its SSID, its WPA2 key (64 hex digits) or none (open).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Network {
    pub ssid: String,
    pub psk: Option<String>,
}

/// `wpa2 KEY SSID` or `open SSID` (the `network` format: `wifi.add`).
pub fn parse_network(v: &str) -> Option<Network> {
    let (kind, rest) = v.split_once(' ')?;
    let (psk, ssid) = match kind {
        "wpa2" => {
            let (k, s) = rest.split_once(' ')?;
            (Some(k), s)
        }
        "open" => (None, rest),
        _ => return None,
    };
    if ssid.is_empty() || ssid.len() > 32 || psk.is_some_and(|k| !hex64(k)) {
        return None;
    }
    Some(Network { ssid: ssid.to_string(), psk: psk.map(|k| k.to_ascii_lowercase()) })
}

/// The known networks, the preferred first, from `dir`'s networks file.
pub fn networks_in(dir: &Path) -> Vec<Network> {
    std::fs::read_to_string(dir.join("networks"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (k, s) = l.split_once(' ')?;
            let ssid = String::from_utf8(unhex(s)?).ok().filter(|s| !s.is_empty())?;
            Some(Network { ssid, psk: (k != "-" && hex64(k)).then(|| k.to_string()) })
        })
        .collect()
}

pub fn networks() -> Vec<Network> {
    networks_in(Path::new(DIR))
}

fn save_networks(dir: &Path, nets: &[Network]) -> Result<(), String> {
    let text: String =
        nets.iter().map(|n| format!("{} {}\n", n.psk.as_deref().unwrap_or("-"), hex(n.ssid.as_bytes()))).collect();
    write_private(dir.join("networks").to_str().unwrap(), &text)
}

/// wpa_supplicant's configuration: its control socket (zd1-net's `wpa`, for the status and scans), and the known
/// networks with their priority by order (the supplicant joins the preferred one it sees; the SSID in hex: any bytes).
/// Without one the supplicant idles and scans when asked.
pub fn wpa_conf(nets: &[Network]) -> String {
    let mut c = "ctrl_interface=/var/run/wpa_supplicant\n".to_string();
    for (i, n) in nets.iter().enumerate() {
        let auth = match &n.psk {
            Some(p) => format!("\tpsk={p}\n\tkey_mgmt=WPA-PSK\n\tproto=RSN\n\tpairwise=CCMP\n"),
            None => "\tkey_mgmt=NONE\n".into(),
        };
        c += &format!(
            "network={{\n\tssid={}\n{auth}\tscan_ssid=1\n\tpriority={}\n}}\n",
            hex(n.ssid.as_bytes()),
            nets.len() - i
        );
    }
    c
}

// ---- AirPlay's pairings ----

/// The controllers paired with the AirPlay receiver, the oldest first: (identifier, admin), from its store in `dir`.
pub fn pairings_in(dir: &Path) -> Vec<(String, bool)> {
    std::fs::read_to_string(dir.join("peers"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut f = l.split(' ');
            let (id, _key, role) = (f.next()?, f.next()?, f.next());
            Some((id.to_string(), role != Some("user")))
        })
        .collect()
}

pub fn pairings() -> Vec<(String, bool)> {
    pairings_in(Path::new(AIRPLAY))
}

// ---- the actions ----

/// Do an action (`Actions` in the spec) on the module's state in `dir` (/data/zd1): checked like a setting, done, or
/// Err with why. `apply` then restarts what uses it.
pub fn act_in(dir: &Path, key: &str, v: &str) -> Result<(), String> {
    let f = action(key).ok_or_else(|| format!("no action {key}"))?;
    if v.is_empty() {
        return Err(format!("{key}: a value"));
    }
    check(f, v).map_err(|e| format!("{key}: {e}"))?;
    let mut nets = networks_in(dir);
    let known =
        |nets: &[Network]| nets.iter().position(|n| n.ssid == v).ok_or_else(|| format!("{key}: no known network {v}"));
    match key {
        "wifi.add" => {
            let n = parse_network(v).unwrap();
            nets.retain(|o| o.ssid != n.ssid);
            nets.insert(0, n);
            save_networks(dir, &nets)
        }
        "wifi.forget" => {
            let i = known(&nets)?;
            nets.remove(i);
            save_networks(dir, &nets)
        }
        "wifi.first" => {
            let i = known(&nets)?;
            let n = nets.remove(i);
            nets.insert(0, n);
            save_networks(dir, &nets)
        }
        "airplay.forget" => forget_pairings(dir),
        "reset" => {
            if v == "all" {
                forget_pairings(dir)?;
                remove(&dir.join("networks"))?;
            }
            remove(&dir.join("settings"))
        }
        _ => Err(format!("{key}: not done by this version")),
    }
}

pub fn act(key: &str, v: &str) -> Result<(), String> {
    act_in(Path::new(DIR), key, v)
}

fn forget_pairings(dir: &Path) -> Result<(), String> {
    remove(&dir.join("airplay").join("peers"))
}

fn remove(p: &Path) -> Result<(), String> {
    match std::fs::remove_file(p) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("{}: {e}", p.display())),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("zd1-settings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn values() {
        assert!(check(field("wifi.address").unwrap(), "192.0.2.9/24").is_ok());
        assert!(check(field("wifi.address").unwrap(), "192.0.2.9").is_err());
        assert!(check(field("eth.mode").unwrap(), "static").is_ok());
        assert!(check(field("eth.mode").unwrap(), "auto").is_err());
        assert!(check(field("airplay").unwrap(), "yes").is_err());
        assert!(check(field("name").unwrap(), &"x".repeat(64)).is_err());
        assert!(check(field("name").unwrap(), "Küche").is_ok());
        assert!(check(field("name").unwrap(), "a\tb").is_err());
        assert!(check(field("airplay.pin").unwrap(), "3939").is_ok());
        assert!(check(field("airplay.pin").unwrap(), "39a9").is_err());
        assert!(check(field("spotify.bitrate").unwrap(), "320").is_ok());
        assert!(check(field("spotify.bitrate").unwrap(), "256").is_err());
        assert!(check(field("snapcast.latency").unwrap(), "70000").is_err());
        let psk = "0123456789abcdef".repeat(4);
        assert!(check(action("wifi.add").unwrap(), &format!("wpa2 {psk} My Home")).is_ok());
        assert!(check(action("wifi.add").unwrap(), "wpa2 hunter22 My Home").is_err());
        assert!(check(action("wifi.add").unwrap(), "open Cafe").is_ok());
        assert!(check(action("reset").unwrap(), "everything").is_err());
        assert_eq!(shown("wifi.add", &format!("wpa2 {psk} My Home")), "wpa2 (key) My Home", "the key never shown");
        assert!(field("wifi.ssid").is_none(), "the networks are actions now, not settings");
    }

    #[test]
    fn networks_and_wpa() {
        let d = tmp("nets");
        let s = d.join("settings");
        let s = s.to_str().unwrap();
        let k1 = "ab".repeat(32);
        std::fs::write(s, "name=Kitchen\n").unwrap();
        assert!(networks_in(&d).is_empty());
        act_in(&d, "wifi.add", &format!("wpa2 {k1} Home")).unwrap();
        act_in(&d, "wifi.add", "open Café").unwrap();
        act_in(&d, "wifi.add", &format!("wpa2 {} Office", "cd".repeat(32))).unwrap();
        let n: Vec<String> = networks_in(&d).into_iter().map(|n| n.ssid).collect();
        assert_eq!(n, ["Office", "Café", "Home"]);
        act_in(&d, "wifi.first", "Home").unwrap();
        assert_eq!(networks_in(&d)[0], Network { ssid: "Home".into(), psk: Some(k1.clone()) });
        act_in(&d, "wifi.forget", "Café").unwrap();
        assert!(act_in(&d, "wifi.forget", "Nowhere").is_err());
        assert!(act_in(&d, "wifi.add", "").is_err());
        let c = wpa_conf(&networks_in(&d));
        assert!(c.contains("ssid=486f6d65") && c.contains(&format!("psk={k1}")) && c.contains("priority=2"));
        assert_eq!(c.matches("network={").count(), 2);
        act_in(&d, "wifi.forget", "Home").unwrap();
        act_in(&d, "wifi.forget", "Office").unwrap();
        assert!(networks_in(&d).is_empty());
        assert_eq!(wpa_conf(&[]), "ctrl_interface=/var/run/wpa_supplicant\n", "no network: the control socket alone");
        assert_eq!(get_from(&load_from(s), "name").unwrap(), "Kitchen", "the other settings kept");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn pairings_and_reset() {
        let d = tmp("reset");
        let s = d.join("settings");
        let s = s.to_str().unwrap();
        std::fs::create_dir_all(d.join("airplay")).unwrap();
        std::fs::write(
            d.join("airplay/peers"),
            format!("A1B2 {} admin\nC3D4 {} user\n", "00".repeat(32), "11".repeat(32)),
        )
        .unwrap();
        std::fs::write(d.join("airplay/identity"), [7u8; 32]).unwrap();
        assert_eq!(pairings_in(&d.join("airplay")), [("A1B2".to_string(), true), ("C3D4".to_string(), false)]);
        set_in(s, "name", "Kitchen").unwrap();
        set_in(s, "airplay", "off").unwrap();
        act_in(&d, "wifi.add", "open Home").unwrap();
        act_in(&d, "reset", "settings").unwrap();
        assert!(load_from(s).is_empty(), "every setting back to its default");
        assert_eq!(networks_in(&d).len(), 1, "the networks stay");
        assert_eq!(pairings_in(&d.join("airplay")).len(), 2, "the pairings stay");
        act_in(&d, "airplay.forget", "yes").unwrap();
        assert!(pairings_in(&d.join("airplay")).is_empty());
        assert!(d.join("airplay/identity").exists(), "the accessory stays the same");
        set_in(s, "name", "Kitchen").unwrap();
        act_in(&d, "reset", "all").unwrap();
        assert!(networks_in(&d).is_empty() && load_from(s).is_empty());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_newer_os_wrote_it() {
        let d = tmp("newer");
        let p = d.join("settings");
        let p = p.to_str().unwrap();
        std::fs::write(p, "airplay=auto\nname=Kitchen\nlights.color=warm\nwifi.address=fe80::1/64\nspotify=off\n")
            .unwrap();
        let all = load_from(p);
        assert_eq!(get_from(&all, "name").unwrap(), "Kitchen");
        assert_eq!(get_from(&all, "airplay").unwrap(), "on", "a value this version can't check: the default");
        assert_eq!(get_from(&all, "wifi.address").unwrap(), "", "a value this version can't check: the default");
        assert_eq!(get_from(&all, "wifi.mode").unwrap(), "dhcp");
        assert_eq!(get_from(&all, "spotify").unwrap(), "off");
        assert!(!all.contains_key("lights.color"));
        set_in(p, "name", "Bathroom").unwrap();
        set_in(p, "spotify", "").unwrap();
        assert_eq!(
            std::fs::read_to_string(p).unwrap(),
            "airplay=auto\nlights.color=warm\nwifi.address=fe80::1/64\nname=Bathroom\n",
            "the rest kept for the newer OS"
        );
        set_in(p, "airplay", "off").unwrap();
        assert_eq!(get_from(&load_from(p), "airplay").unwrap(), "off");
        assert!(!std::fs::read_to_string(p).unwrap().contains("airplay=auto"));
        std::fs::remove_dir_all(&d).unwrap();
    }
}

// the program (zd1-settings), in the multicall (module/os/apps/zd1)
mod cli;
pub use cli::main;
