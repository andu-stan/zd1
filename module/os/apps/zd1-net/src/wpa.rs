//! The Wi-Fi supplicant's control socket (wpa_supplicant's `ctrl_interface`, what `wpa_cli` speaks): a datagram
//! socket of our own, a request one datagram, its reply one back; after `ATTACH` the supplicant's events come on the
//! same socket, each prefixed with its level, `<N>`. [`Watch`] turns what the supplicant says into the State
//! record's Wi-Fi fields (`docs/module/settings.md`), and answers a scan.
//! Under Wireless Extensions the supplicant's `SIGNAL_POLL` says -80 dBm whatever the level: the signal is the BSS
//! table's (`BSS BSSID`, its `level`), which the scan before the association filled in [HW 2026-10-24].

use std::io::{self, ErrorKind};
use std::os::unix::net::UnixDatagram;
use std::time::Duration;
pub use zd1_hostlink::{WifiSecurity, WifiState};

/// Where the supplicant's sockets are (`ctrl_interface` in its configuration: zd1-settings writes it).
pub const CTRL_DIR: &str = "/var/run/wpa_supplicant";
/// How long a request may take; the supplicant answers from its event loop, usually within a millisecond.
const REPLY: Duration = Duration::from_millis(300);

pub struct Wpa {
    sock: UnixDatagram,
    ours: String,
    events: Vec<String>,
}

impl Wpa {
    /// The supplicant on `iface`, attached for its events.
    pub fn open(iface: &str) -> io::Result<Wpa> {
        let ours = format!("/run/zd1/wpa-{iface}.{}", std::process::id());
        let _ = std::fs::create_dir_all("/run/zd1");
        let _ = std::fs::remove_file(&ours);
        let sock = UnixDatagram::bind(&ours)?;
        sock.connect(format!("{CTRL_DIR}/{iface}"))?;
        sock.set_read_timeout(Some(REPLY))?;
        let mut w = Wpa { sock, ours, events: Vec::new() };
        if w.request("ATTACH")? != "OK" {
            return Err(io::Error::other("ATTACH refused"));
        }
        Ok(w)
    }

    /// A request and its reply (its trailing newline gone); events that arrive meanwhile are kept for [`Wpa::events`].
    pub fn request(&mut self, cmd: &str) -> io::Result<String> {
        self.sock.send(cmd.as_bytes())?;
        let mut buf = vec![0u8; 4096];
        loop {
            let n = self.sock.recv(&mut buf)?;
            let s = String::from_utf8_lossy(&buf[..n]).into_owned();
            if let Some(e) = event(&s) {
                self.events.push(e);
                continue;
            }
            return Ok(s.trim_end_matches('\n').to_string());
        }
    }

    /// The events since the last call, oldest first, their level prefix gone.
    pub fn events(&mut self) -> Vec<String> {
        let mut buf = vec![0u8; 4096];
        let _ = self.sock.set_nonblocking(true);
        loop {
            match self.sock.recv(&mut buf) {
                Ok(n) => {
                    if let Some(e) = event(&String::from_utf8_lossy(&buf[..n])) {
                        self.events.push(e);
                    }
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted => break,
                Err(_) => break,
            }
        }
        let _ = self.sock.set_nonblocking(false);
        std::mem::take(&mut self.events)
    }

    /// `STATUS`: the supplicant's state (`wpa_state`) and the access point (`bssid`) when associated.
    pub fn status(&mut self) -> io::Result<Status> {
        Ok(parse_status(&self.request("STATUS")?))
    }

    /// The BSS table's signal for an access point, dBm (`BSS BSSID`: `level=-57`).
    pub fn level(&mut self, bssid: &str) -> io::Result<Option<i32>> {
        Ok(self.request(&format!("BSS {bssid}"))?.lines().find_map(|l| l.strip_prefix("level=")?.trim().parse().ok()))
    }

    /// The last scan's networks (`SCAN_RESULTS`), as the supplicant lists them.
    pub fn scan_results(&mut self) -> io::Result<Vec<Network>> {
        Ok(parse_scan_results(&self.request("SCAN_RESULTS")?))
    }

    /// Start a scan (`SCAN`; one already running: `FAIL-BUSY`, which is fine).
    pub fn scan(&mut self) -> io::Result<()> {
        self.request("SCAN").map(|_| ())
    }
}

impl Drop for Wpa {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.ours);
    }
}

/// An unsolicited message (`<N>text`): its text.
fn event(s: &str) -> Option<String> {
    let rest = s.strip_prefix('<')?;
    let (_, text) = rest.split_once('>')?;
    Some(text.trim_end_matches('\n').to_string())
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub wpa_state: String,
    pub bssid: String,
    pub ssid: Vec<u8>,
    pub key_mgmt: String,
}

pub fn parse_status(s: &str) -> Status {
    let mut st = Status::default();
    for l in s.lines() {
        match l.split_once('=') {
            Some(("wpa_state", v)) => st.wpa_state = v.trim().to_string(),
            Some(("bssid", v)) => st.bssid = v.trim().to_string(),
            Some(("ssid", v)) => st.ssid = printf_decode(v.trim_end()),
            Some(("key_mgmt", v)) => st.key_mgmt = v.trim().to_string(),
            _ => {}
        }
    }
    st
}

/// A network from `SCAN_RESULTS`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Network {
    pub bssid: String,
    pub freq: u32,
    pub level: i32,
    pub flags: String,
    pub ssid: Vec<u8>,
}

impl Network {
    /// Whether this speaker can join it, from the beacon's flags (`[WPA2-PSK-CCMP][ESS]`).
    pub fn security(&self) -> WifiSecurity {
        security(&self.flags)
    }
}

/// `bssid / frequency / signal level / flags / ssid` after a header line, tab-separated, the SSID printf-escaped.
pub fn parse_scan_results(s: &str) -> Vec<Network> {
    s.lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            if f.len() < 5 {
                return None;
            }
            Some(Network {
                bssid: f[0].to_string(),
                freq: f[1].parse().ok()?,
                level: f[2].parse().ok()?,
                flags: f[3].to_string(),
                ssid: printf_decode(f[4]),
            })
        })
        .collect()
}

pub fn security(flags: &str) -> WifiSecurity {
    let keyed = flags.split(']').any(|f| f.contains("PSK"));
    if keyed {
        WifiSecurity::Psk
    } else if flags.contains("SAE") {
        WifiSecurity::Sae
    } else if flags.contains("WEP") || flags.contains("EAP") || flags.contains("WPA") {
        WifiSecurity::Other
    } else {
        WifiSecurity::Open
    }
}

/// The supplicant's escaping of an SSID (`printf_encode`: `\xNN` for anything outside printable ASCII, `\\`, `\"`,
/// `\e`, `\n`, `\r`, `\t`), undone.
pub fn printf_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' || i + 1 >= b.len() {
            out.push(b[i]);
            i += 1;
            continue;
        }
        match b[i + 1] {
            b'x' if i + 3 < b.len() => match hex_byte(b[i + 2], b[i + 3]) {
                Some(v) => {
                    out.push(v);
                    i += 4;
                }
                None => {
                    out.push(b'\\');
                    i += 1;
                }
            },
            b'n' => {
                out.push(b'\n');
                i += 2;
            }
            b'r' => {
                out.push(b'\r');
                i += 2;
            }
            b't' => {
                out.push(b'\t');
                i += 2;
            }
            b'e' => {
                out.push(0x1b);
                i += 2;
            }
            c @ (b'\\' | b'"') => {
                out.push(c);
                i += 2;
            }
            _ => {
                out.push(b'\\');
                i += 1;
            }
        }
    }
    out
}

/// Two hex digits as a byte (bytes, not a `&str` slice: what follows `\x` may be any UTF-8).
fn hex_byte(hi: u8, lo: u8) -> Option<u8> {
    let d = |c: u8| (c as char).to_digit(16);
    Some((d(hi)? * 16 + d(lo)?) as u8)
}

/// The State record's Wi-Fi fields from what the supplicant says: polled once a second by zd1-linkd. Without a
/// supplicant to talk to (no radio, the Wi-Fi off, the service between restarts) the state is OFF; with one and no
/// network configured, IDLE.
pub struct Watch {
    iface: String,
    wpa: Option<Wpa>,
    /// A failure the events reported, shown until the next association (a wrong key) or the network is seen.
    reason: Option<WifiState>,
    pub state: WifiState,
    /// The network being joined or joined (associating on), empty otherwise.
    pub ssid: String,
    /// That network is joined with a key (the supplicant's `key_mgmt`: WPA-PSK, WPA2-PSK).
    pub keyed: bool,
    pub bssid: String,
    /// -dBm (0: unknown).
    pub signal: u8,
}

impl Watch {
    pub fn new(iface: &str) -> Watch {
        Watch {
            iface: iface.into(),
            wpa: None,
            reason: None,
            state: WifiState::Off,
            ssid: String::new(),
            keyed: false,
            bssid: String::new(),
            signal: 0,
        }
    }

    /// Whether the supplicant answers (a scan can be asked).
    pub fn up(&self) -> bool {
        self.wpa.is_some()
    }

    /// Once a second: reach the supplicant if it is there, take its events, read its status.
    pub fn poll(&mut self) {
        if self.wpa.is_none() {
            if !crate::Iface::exists(&self.iface)
                || !std::path::Path::new(&format!("{CTRL_DIR}/{}", self.iface)).exists()
            {
                self.off();
                return;
            }
            match Wpa::open(&self.iface) {
                Ok(w) => {
                    self.wpa = Some(w);
                    self.reason = None;
                }
                Err(_) => {
                    self.off();
                    return;
                }
            }
        }
        let w = self.wpa.as_mut().unwrap();
        for e in w.events() {
            self.reason = reason(&e, self.reason);
        }
        let st = match w.status() {
            Ok(st) => st,
            Err(_) => {
                self.wpa = None;
                self.off();
                return;
            } // gone (restarted): again next time
        };
        let (state, reason) = state_of(&st.wpa_state, self.reason);
        self.reason = reason;
        self.state = state;
        let joining = matches!(state, WifiState::Connected | WifiState::Connecting | WifiState::WrongKey);
        self.ssid = if joining { String::from_utf8_lossy(&st.ssid).into_owned() } else { String::new() };
        self.keyed = joining && st.key_mgmt.contains("PSK");
        self.bssid = if state == WifiState::Connected { st.bssid } else { String::new() };
        self.signal = if state == WifiState::Connected {
            w.level(&self.bssid).ok().flatten().map(|l| (-l).clamp(0, 255) as u8).unwrap_or(0)
        } else {
            0
        };
    }

    fn off(&mut self) {
        self.state = WifiState::Off;
        self.ssid.clear();
        self.keyed = false;
        self.bssid.clear();
        self.signal = 0;
        self.reason = None;
    }

    /// The networks around, from the supplicant's last scan; None without a supplicant.
    pub fn networks(&mut self) -> Option<Vec<Network>> {
        self.wpa.as_mut()?.scan_results().ok()
    }

    /// Ask the supplicant for a new scan.
    pub fn scan(&mut self) {
        if let Some(w) = self.wpa.as_mut() {
            let _ = w.scan();
        }
    }
}

/// What an event means for the state, given what was known: a failure to show, or none.
pub fn reason(event: &str, before: Option<WifiState>) -> Option<WifiState> {
    if event.starts_with("CTRL-EVENT-SSID-TEMP-DISABLED") {
        return Some(if event.contains("reason=WRONG_KEY") { WifiState::WrongKey } else { WifiState::Failed });
    }
    if event.contains("4-Way Handshake failed") {
        return Some(WifiState::WrongKey);
    }
    if event.starts_with("CTRL-EVENT-NETWORK-NOT-FOUND") {
        return Some(WifiState::NotFound);
    }
    if event.starts_with("CTRL-EVENT-ASSOC-REJECT") || event.starts_with("CTRL-EVENT-AUTH-REJECT") {
        return Some(WifiState::Failed);
    }
    if event.starts_with("CTRL-EVENT-CONNECTED") {
        return None;
    }
    before
}

/// The supplicant's `wpa_state` with a failure its events reported: the state to show, and the failure kept. A
/// wrong key stays shown through the retries (the supplicant tries the network again after a pause) until a
/// connection completes; "not found" and other failures go once the network is being joined.
pub fn state_of(wpa_state: &str, reason: Option<WifiState>) -> (WifiState, Option<WifiState>) {
    match wpa_state {
        "COMPLETED" => (WifiState::Connected, None),
        "ASSOCIATING" | "ASSOCIATED" | "AUTHENTICATING" | "4WAY_HANDSHAKE" | "GROUP_HANDSHAKE" => match reason {
            Some(WifiState::WrongKey) => (WifiState::WrongKey, reason),
            _ => (WifiState::Connecting, None),
        },
        "INACTIVE" => (reason.unwrap_or(WifiState::Idle), reason), // no network configured
        "INTERFACE_DISABLED" => (reason.unwrap_or(WifiState::Off), reason),
        _ => (reason.unwrap_or(WifiState::Scanning), reason), // SCANNING, DISCONNECTED
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_and_bss() {
        let st = parse_status(
            "bssid=02:00:00:00:00:0a\nfreq=2462\nssid=Caf\\xc3\\xa9\nid=0\nkey_mgmt=WPA2-PSK\nwpa_state=COMPLETED\naddress=ee:66:d1:00:00:42\n",
        );
        assert_eq!(
            st,
            Status {
                wpa_state: "COMPLETED".into(),
                bssid: "02:00:00:00:00:0a".into(),
                ssid: "Café".as_bytes().into(),
                key_mgmt: "WPA2-PSK".into()
            }
        );
        assert_eq!(parse_status("wpa_state=SCANNING\n").bssid, "");
        assert_eq!(
            event("<3>CTRL-EVENT-CONNECTED - Connection to 02:00:00:00:00:0a completed\n").unwrap(),
            "CTRL-EVENT-CONNECTED - Connection to 02:00:00:00:00:0a completed"
        );
        assert_eq!(event("OK\n"), None);
    }

    #[test]
    fn scan_results() {
        let s = "bssid / frequency / signal level / flags / ssid\n\
                 02:00:00:00:00:0a\t2462\t-57\t[WPA2-PSK-CCMP][ESS]\tHome\n\
                 02:00:00:00:00:0b\t2412\t-71\t[WPA2-PSK+SAE-CCMP][ESS]\tCaf\\xc3\\xa9 \"W\"\n\
                 02:00:00:00:00:0c\t2437\t-80\t[WPA2-SAE-CCMP][ESS]\tNew\n\
                 02:00:00:00:00:0d\t2437\t-82\t[WPA2-EAP-CCMP][ESS]\tOffice\n\
                 02:00:00:00:00:0e\t2437\t-85\t[ESS]\t\n\
                 02:00:00:00:00:0f\t2437\t-88\t[WEP][ESS]\tOld\n\
                 bad line\n";
        let n = parse_scan_results(s);
        assert_eq!(n.len(), 6);
        assert_eq!(
            (n[0].bssid.as_str(), n[0].freq, n[0].level, n[0].security()),
            ("02:00:00:00:00:0a", 2462, -57, WifiSecurity::Psk)
        );
        assert_eq!(String::from_utf8(n[1].ssid.clone()).unwrap(), "Café \"W\"");
        assert_eq!(n[1].security(), WifiSecurity::Psk, "a WPA3 transition network joins");
        assert_eq!(n[2].security(), WifiSecurity::Sae);
        assert_eq!(n[3].security(), WifiSecurity::Other);
        assert_eq!((n[4].ssid.len(), n[4].security()), (0, WifiSecurity::Open));
        assert_eq!(n[5].security(), WifiSecurity::Other);
        assert_eq!(printf_decode("a\\\\b\\n\\x4"), b"a\\b\n\\x4");
        assert_eq!(printf_decode("\\x1é"), "\\x1é".as_bytes(), "not hex: kept, no panic in the middle of a character");
    }

    #[test]
    fn states() {
        use WifiState::*;
        assert_eq!(state_of("COMPLETED", Some(WrongKey)), (Connected, None));
        assert_eq!(state_of("SCANNING", None), (Scanning, None));
        assert_eq!(state_of("4WAY_HANDSHAKE", None), (Connecting, None));
        assert_eq!(state_of("INACTIVE", None), (Idle, None));
        assert_eq!(state_of("INTERFACE_DISABLED", None), (Off, None));
        let r = reason(
            "CTRL-EVENT-SSID-TEMP-DISABLED id=0 ssid=\"Home\" auth_failures=1 duration=10 reason=WRONG_KEY",
            None,
        );
        assert_eq!(r, Some(WrongKey));
        assert_eq!(state_of("SCANNING", r), (WrongKey, r));
        assert_eq!(state_of("ASSOCIATING", r), (WrongKey, r), "the retry: still the wrong key");
        let r = reason("CTRL-EVENT-NETWORK-NOT-FOUND", None);
        assert_eq!(state_of("SCANNING", r), (NotFound, r));
        assert_eq!(state_of("ASSOCIATING", r), (Connecting, None), "seen now");
        assert_eq!(reason("CTRL-EVENT-SCAN-RESULTS", r), r, "an event that says nothing keeps what was known");
        assert_eq!(reason("CTRL-EVENT-CONNECTED - Connection to 00:11:22:33:44:55 completed", r), None);
        assert_eq!(
            reason(
                "CTRL-EVENT-SSID-TEMP-DISABLED id=0 ssid=\"x\" auth_failures=3 duration=30 reason=CONN_FAILED",
                None
            ),
            Some(Failed)
        );
    }
}
