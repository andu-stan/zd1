//! The speaker's settings, kept by our module OS on its data partition (docs/module/settings.md), through our MCU
//! firmware (`GetSetting` / `SetSetting`, the configuration channel): nothing on the network changes them. The
//! module defines the keys (`?` lists them); a refused value comes back as `!` and why. The Wi-Fi passphrase never
//! leaves this computer: the WPA2 key is derived here. The keys are the State record's writable fields (the spec:
//! `zd1_hostlink::settings()`), and the record (`ModRecord`), kept by the MCU, is one read for all of them and the
//! module's status.

use crate::codec::{self, Msg, Typed, Val};
use crate::spec::{Field, NetMode, Record, Ty};
use crate::usb::Usb;
use crate::{Error, Mcu, Result, msg};

/// A setting's value (secrets read back as `(set)` or empty).
pub async fn get<U: Usb>(m: &mut Mcu<U>, key: &str) -> Result<String> {
    let v = m.call(&msg::GetSetting { key: key.into() }).await?.value;
    match v.strip_prefix('!') {
        Some(why) => Err(Error::Failed(format!("{key}: {why}"))),
        None => Ok(v),
    }
}

/// Change a setting ("" = back to its default), or do an action (`spec::actions()`: the value says what): the value as
/// the module has it now (an action's: empty).
pub async fn set<U: Usb>(m: &mut Mcu<U>, key: &str, value: &str) -> Result<String> {
    let v = m.call(&msg::SetSetting { key: key.into(), value: value.into() }).await?.value;
    match v.strip_prefix('!') {
        Some(why) => {
            Err(Error::Failed(format!("{key}: refused: {}", why.strip_prefix(&format!("{key}: ")).unwrap_or(why))))
        }
        None => Ok(v),
    }
}

/// A record of the module OS (`ModRecord`), its bytes: a message of the spec, read in pieces of 240 bytes. A record
/// the module hasn't comes back as `!` and why.
pub async fn record<U: Usb>(m: &mut Mcu<U>, r: Record) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    loop {
        let p = m.call(&msg::ModRecord { record: r, offset: buf.len() as u16 }).await?;
        let done = p.data.is_empty() || buf.len() + p.data.len() >= p.total as usize;
        buf.extend_from_slice(&p.data);
        if done {
            break;
        }
    }
    if buf.first() == Some(&b'!') {
        return Err(Error::Failed(format!("{r}: {}", String::from_utf8_lossy(&buf[1..]))));
    }
    Ok(buf)
}

/// The State record, decoded: the speaker as the module OS knows it (its settings and its status).
pub async fn state_msg<U: Usb>(m: &mut Mcu<U>) -> Result<Msg> {
    let st = codec::decode(&record(m, Record::State).await?)?;
    if st.name() != msg::State::NAME {
        return Err(Error::Failed(format!("the State record holds a {}", st.name())));
    }
    Ok(st)
}

/// The State record as its type.
pub async fn state<U: Usb>(m: &mut Mcu<U>) -> Result<msg::State> {
    Ok(msg::State::from_msg(&state_msg(m).await?))
}

/// A list record, its entries as their type.
pub async fn list_of<U: Usb, T: Typed>(m: &mut Mcu<U>, r: Record) -> Result<Vec<T>> {
    let l = codec::decode_list(&record(m, r).await?)?;
    if let Some(e) = l.iter().find(|e| e.name() != T::NAME) {
        return Err(Error::Failed(format!("{r}: a {} where a {} belongs", e.name(), T::NAME)));
    }
    Ok(l.iter().map(T::from_msg).collect())
}

/// The Wi-Fi networks around the speaker (the `Scan` record): the module's last scan, the strongest first, one entry
/// per SSID; the module starts another scan at each read, so the first read of a fresh radio may be empty and a read
/// a few seconds later has it. Err `Failed` without a radio (the Wi-Fi off).
pub async fn scan<U: Usb>(m: &mut Mcu<U>) -> Result<Vec<msg::ScanEntry>> {
    list_of(m, Record::Scan).await
}

/// The Wi-Fi networks the speaker knows, the one it prefers first (their keys stay on the speaker).
pub async fn networks<U: Usb>(m: &mut Mcu<U>) -> Result<Vec<msg::KnownNetwork>> {
    list_of(m, Record::Networks).await
}

/// The snapservers on the speaker's networks (mDNS): the module's last look, and it looks again at each read (a
/// first read may be empty: read again two seconds later).
pub async fn servers<U: Usb>(m: &mut Mcu<U>) -> Result<Vec<msg::Snapserver>> {
    list_of(m, Record::Servers).await
}

/// The controllers paired with AirPlay, the oldest first.
pub async fn pairings<U: Usb>(m: &mut Mcu<U>) -> Result<Vec<msg::Pairing>> {
    list_of(m, Record::Pairings).await
}

/// A field's value in its string form (what `SetSetting` takes, what a person reads): bools `on` / `off`, enums by
/// name, numbers in decimal, text as it is.
pub fn shown(f: &Field, v: &Val) -> String {
    match (f.ty, v) {
        (_, Val::B(b)) => {
            if *b {
                "on".into()
            } else {
                "off".into()
            }
        }
        (Ty::Enum(e), Val::N(n)) => codec::enum_name(e, *n).map(str::to_lowercase).unwrap_or_else(|| n.to_string()),
        (_, Val::N(n)) => n.to_string(),
        (_, Val::S(s)) => String::from_utf8_lossy(s).into_owned(),
    }
}

/// Every setting with its value, from the State record (one read).
pub async fn list<U: Usb>(m: &mut Mcu<U>) -> Result<Vec<(String, String)>> {
    let st = state_msg(m).await?;
    Ok(st.fields().filter(|(f, _)| f.set).map(|(f, v)| (f.key(), shown(f, v))).collect())
}

/// The network key wpa_supplicant derives (wpa_passphrase): PBKDF2-SHA1(passphrase, SSID, 4096), 32 bytes, as hex.
pub fn psk(ssid: &str, passphrase: &str) -> Result<String> {
    if !(8..=63).contains(&passphrase.len()) {
        return Err(Error::Failed("a WPA2 passphrase has 8 to 63 characters".into()));
    }
    let mut k = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<sha1::Sha1>(passphrase.as_bytes(), ssid.as_bytes(), 4096, &mut k);
    Ok(crate::hex(&k))
}

/// How the speaker joins a network.
pub enum Wifi<'a> {
    /// WPA2: the passphrase, turned into the key here.
    Wpa2(&'a str),
    Open,
}

/// Join `ssid`: added to the known networks (or its key changed) and put first, in one write (`wifi.add`); a Wi-Fi
/// switched off comes on, by DHCP.
pub async fn join<U: Usb>(m: &mut Mcu<U>, ssid: &str, how: Wifi<'_>) -> Result<()> {
    if ssid.is_empty() || ssid.len() > 32 {
        return Err(Error::Failed("an SSID has 1 to 32 bytes".into()));
    }
    let v = match how {
        Wifi::Wpa2(p) => format!("wpa2 {} {ssid}", psk(ssid, p)?),
        Wifi::Open => format!("open {ssid}"),
    };
    set(m, "wifi.add", &v).await?;
    if state(m).await?.wifi_mode == NetMode::Off {
        set(m, "wifi.mode", "dhcp").await?;
    }
    Ok(())
}

/// Forget a known network.
pub async fn forget<U: Usb>(m: &mut Mcu<U>, ssid: &str) -> Result<()> {
    set(m, "wifi.forget", ssid).await.map(drop)
}

/// Put a known network first: the speaker prefers it when it sees it.
pub async fn prefer<U: Usb>(m: &mut Mcu<U>, ssid: &str) -> Result<()> {
    set(m, "wifi.first", ssid).await.map(drop)
}

/// The module back to its defaults: every setting (`all`: the known networks and AirPlay's pairings too).
pub async fn reset<U: Usb>(m: &mut Mcu<U>, all: bool) -> Result<()> {
    set(m, "reset", if all { "all" } else { "settings" }).await.map(drop)
}

/// Every setting the speaker would take back (the secrets aren't readable, so none are among them): KEY=VALUE lines,
/// the defaults left out; `import` writes them again.
pub async fn export<U: Usb>(m: &mut Mcu<U>) -> Result<String> {
    let st = state_msg(m).await?;
    Ok(st
        .fields()
        .filter(|(f, _)| f.set && f.format != "psk")
        .map(|(f, v)| (f.key(), shown(f, v), f.default))
        .filter(|(k, v, d)| v != d && !v.is_empty() && !(k == "name" && generated_name(v)))
        .map(|(k, v, _)| format!("{k}={v}\n"))
        .collect())
}

/// The name a speaker has before anyone chose one: "Zeppelin Air" and the MAC's last three bytes (the module's
/// default; its own on each speaker, so an export leaves it out).
fn generated_name(v: &str) -> bool {
    v.strip_prefix("Zeppelin Air ").is_some_and(|m| m.len() == 6 && m.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Settings from `export`'s lines (`#` comments and blank lines skipped), each written; the keys this firmware's
/// module refuses are listed in the error, the others stay written.
pub async fn import<U: Usb>(m: &mut Mcu<U>, text: &str) -> Result<usize> {
    let mut n = 0;
    let mut refused = Vec::new();
    for l in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let Some((k, v)) = l.split_once('=') else {
            refused.push(format!("{l}: not KEY=VALUE"));
            continue;
        };
        if crate::spec::setting(k.trim()).is_none() {
            refused.push(format!("{k}: not a setting"));
            continue;
        }
        match set(m, k.trim(), v.trim()).await {
            Ok(_) => n += 1,
            Err(Error::Failed(e)) => refused.push(e),
            Err(e) => return Err(e),
        }
    }
    if refused.is_empty() { Ok(n) } else { Err(Error::Failed(format!("{n} written; refused: {}", refused.join("; ")))) }
}

/// The Wi-Fi off (the network stays stored: `join`, or `wifi.mode`, brings it back).
pub async fn wifi_off<U: Usb>(m: &mut Mcu<U>) -> Result<()> {
    set(m, "wifi.mode", "off").await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn psk_is_wpa_passphrase() {
        // IEEE 802.11i-2004, H.4.3: "password" / "IEEE"
        assert_eq!(
            super::psk("IEEE", "password").unwrap(),
            "f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e"
        );
        assert!(super::psk("x", "short").is_err());
    }
}
