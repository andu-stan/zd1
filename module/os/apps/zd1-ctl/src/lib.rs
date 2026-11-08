//! zd1-ctl: the running system from a shell.
//!   zd1-ctl [status]                      init's services (their state, process, starts) and the system slots (what
//!                                         runs, the good one, what each holds: the other slot is mounted to read it)
//!   zd1-ctl start | stop | restart S      a service by name (init, module/os/apps/zd1-init)
//!   zd1-ctl debug on | off | default      the debug services (normally the MCU's word: `zd1 debug`)
//!   zd1-ctl changed KEY...                what zd1-settings tells init after a change (restarts what uses KEY)
//! To zd1-linkd's app socket (/run/zd1link; module/os/README.md has the messages):
//!   zd1-ctl state playing|paused|stopped [SOURCE]   play state to the MCU (playing switches the speaker to the
//!                                         module; SOURCE: who plays)
//!   zd1-ctl volume 0-100                  volume to the MCU
//!   zd1-ctl log TEXT                      a line in the log (the kernel's: the console, the MCU's log)
//!   zd1-ctl watch                         print zd1-linkd's events (key ..., audio on|off, volume N, net ...)
//!   zd1-ctl as SOURCE [STATE]             be a receiver with a sender connected (a test stand-in): `session on`,
//!                                         the state (default paused), then print what it is told until killed
//!   zd1-ctl serve PORT                    the status page over HTTP (init's `status` service runs it in stock-MCU
//!                                         mode, which has no USB host link): `/` status, the slots, the end of each
//!                                         service's log (read-only, never the settings file); `/wifi` a network to
//!                                         join, taken over Ethernet only
//! Which slot starts is the MCU's (`zd1 slot`); writing one: `zd1-slot install`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::time::Duration;
use zd1_slot::Slot;

const USAGE: &str = "usage: zd1-ctl [status] | start|stop|restart SERVICE | debug on|off|default | changed KEY... | \
                     state playing|paused|stopped [SOURCE] | volume N | log TEXT | watch | as SOURCE [STATE] | \
                     serve PORT";

pub fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        [] | ["status"] => {
            out(&init("status"));
            out(&slots());
        }
        ["start" | "stop" | "restart", _] | ["debug", "on" | "off" | "default"] | ["changed", _, ..] => {
            out(&init(&args.join(" ")))
        }
        ["state" | "volume" | "log" | "watch" | "as", ..] => linkd(&a),
        ["serve", port] => match port.parse::<u16>() {
            Ok(p) => serve(p),
            Err(_) => zd1_sys::die("zd1-ctl", "serve PORT"),
        },
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
}

/// Text to stdout; a reader that has gone (`| head`) ends us quietly (std's print! panics on EPIPE).
fn out(s: &str) {
    if std::io::stdout().write_all(s.as_bytes()).is_err() {
        std::process::exit(0);
    }
}

/// A request to init, its answer.
fn init(line: &str) -> String {
    zd1_svc::request(line).unwrap_or_else(|e| zd1_sys::die("zd1-ctl", &e))
}

/// What the stub started, the good slot, what each slot holds.
fn slots() -> String {
    let b = zd1_slot::boot();
    let name = |s: Option<Slot>| s.map(|s| s.name()).unwrap_or("?");
    let running = if b.rescue {
        "the rescue"
    } else if b.running.is_some() {
        name(b.running)
    } else {
        "no slot (an initramfs)"
    };
    let trial = if b.running.is_some() && b.running != b.good { " (this one runs on trial)" } else { "" };
    let mut o = format!("\nrunning: {running}\ngood:    {}{trial}\n", name(b.good));
    for s in [Slot::A, Slot::B] {
        o += &format!("slot {}:  {}\n", s.name(), zd1_slot::stamp(s).unwrap_or_else(|| "no system".into()));
    }
    o
}

/// The status page, one connection at a time, built at each request: `/` the status (read-only), `/wifi` B&W's
/// setup page's one job, a Wi-Fi network to join (SSID and passphrase: WPA2, or open without one), taken only over
/// Ethernet (the connection's local address is eth0's), as `wifi.add` (the passphrase never stored or shown: its
/// key, as host/core and the stock restore make it).
fn serve(port: u16) {
    let l = TcpListener::bind(("0.0.0.0", port)).unwrap_or_else(|e| zd1_sys::die("zd1-ctl", &format!("port {port}: {e}")));
    for c in l.incoming() {
        let Ok(mut c) = c else { continue };
        let _ = c.set_read_timeout(Some(Duration::from_secs(3)));
        let _ = c.set_write_timeout(Some(Duration::from_secs(5)));
        let Some(r) = request(&mut c) else { continue };
        let local = c.local_addr().ok().map(|a| a.ip());
        let (code, kind, body) = respond(&r, local);
        let head = format!(
            "HTTP/1.0 {code}\r\nContent-Type: {kind}; charset=utf-8\r\nCache-Control: no-store\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = c.write_all(head.as_bytes()).and_then(|_| c.write_all(body.as_bytes()));
    }
}

struct Request {
    method: String,
    path: String,
    body: Vec<u8>,
}

/// The request line, the headers (8 KB at most) and a body of up to 4 KB (Content-Length).
fn request(c: &mut TcpStream) -> Option<Request> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    let end = loop {
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p + 4;
        }
        if buf.len() > 8192 {
            return None;
        }
        let n = c.read(&mut tmp).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..end]).to_string();
    let mut words = head.lines().next()?.split(' ');
    let (method, path) = (words.next()?.to_string(), words.next()?.to_string());
    let len = head
        .lines()
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok())
        .unwrap_or(0)
        .min(4096);
    let mut body = buf[end..].to_vec();
    while body.len() < len {
        let n = c.read(&mut tmp).ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    body.truncate(len);
    Some(Request { method, path, body })
}

fn respond(r: &Request, local: Option<IpAddr>) -> (&'static str, &'static str, String) {
    let path = r.path.split('?').next().unwrap_or("");
    match (r.method.as_str(), path) {
        ("GET", "/wifi") => ("200 OK", "text/html", wifi_page("")),
        ("POST", "/wifi") => {
            let eth = zd1_net::Iface::open("eth0").ok().and_then(|i| i.ipv4()).map(IpAddr::V4);
            if eth.is_none() || local != eth {
                return ("403 Forbidden", "text/plain", "Wi-Fi setup is taken over Ethernet only.\n".into());
            }
            let f = form(&r.body);
            let field = |k: &str| f.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str()).unwrap_or("");
            match add_wifi(field("ssid"), field("pass")) {
                Ok(ssid) => ("200 OK", "text/html", wifi_page(&format!("Saved: the speaker joins {} now.", esc(&ssid)))),
                Err(e) => ("400 Bad Request", "text/html", wifi_page(&esc(&e))),
            }
        }
        _ => ("200 OK", "text/plain", page()),
    }
}

/// A network to join: `wpa2 KEY SSID` (KEY: PBKDF2-SHA1 of the passphrase, 4096 rounds, salted with the SSID) or
/// `open SSID`, then init told (it restarts what uses it).
fn add_wifi(ssid: &str, pass: &str) -> Result<String, String> {
    let ssid = ssid.trim_end_matches(['\r', '\n']);
    if ssid.is_empty() || ssid.len() > 32 || ssid.chars().any(char::is_control) {
        return Err("The network name must be 1 to 32 bytes.".into());
    }
    let v = if pass.is_empty() {
        format!("open {ssid}")
    } else {
        if !(8..=63).contains(&pass.len()) || !pass.bytes().all(|b| (0x20..0x7f).contains(&b)) {
            return Err("A WPA2 passphrase is 8 to 63 printable ASCII characters.".into());
        }
        let mut k = [0u8; 32];
        pbkdf2::pbkdf2_hmac::<sha1::Sha1>(pass.as_bytes(), ssid.as_bytes(), 4096, &mut k);
        format!("wpa2 {} {ssid}", zd1_sys::hex(&k))
    };
    zd1_settings::act("wifi.add", &v)?;
    zd1_settings::apply("wifi.add");
    Ok(ssid.to_string())
}

fn wifi_page(note: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\">\
         <title>Wi-Fi</title></head><body style=\"font-family:sans-serif;max-width:28em;margin:2em auto;padding:0 1em\">\
         <h1>Wi-Fi</h1><p>{note}</p><form method=\"post\" action=\"/wifi\">\
         <p><label>Network name<br><input name=\"ssid\" maxlength=\"32\" required></label></p>\
         <p><label>Passphrase (empty for an open network)<br><input name=\"pass\" type=\"password\" maxlength=\"63\"></label></p>\
         <p><button>Join</button></p></form><p><a href=\"/\">Status</a></p></body></html>\n"
    )
}

/// application/x-www-form-urlencoded: `+` a space, `%XX` a byte.
fn form(body: &[u8]) -> Vec<(String, String)> {
    let dec = |s: &str| {
        let b = s.as_bytes();
        let mut o = Vec::with_capacity(b.len());
        let mut i = 0;
        while i < b.len() {
            match b[i] {
                b'+' => o.push(b' '),
                b'%' if i + 2 < b.len() => {
                    match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("x"), 16) {
                        Ok(v) => {
                            o.push(v);
                            i += 2;
                        }
                        Err(_) => o.push(b'%'),
                    }
                }
                x => o.push(x),
            }
            i += 1;
        }
        String::from_utf8_lossy(&o).to_string()
    };
    String::from_utf8_lossy(body)
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (dec(k), dec(v)))
        .collect()
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The page: what `zd1-ctl status` prints, then the end of each service's log (/var/log/NAME.log).
fn page() -> String {
    let read = |p: &str| std::fs::read_to_string(p).unwrap_or_default();
    let release = read("/run/os-release");
    let uptime = read("/proc/uptime");
    let pretty = release.lines().find(|l| l.starts_with("PRETTY_NAME=")).unwrap_or("PRETTY_NAME=?");
    let mut o = format!("{pretty}\nuptime: {} s\n\n", uptime.split(' ').next().unwrap_or("?"));
    o += &zd1_svc::request("status").unwrap_or_else(|e| format!("init: {e}\n"));
    o += &slots();
    let mut logs: Vec<_> = std::fs::read_dir("/var/log")
        .map(|d| d.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "log")).collect())
        .unwrap_or_default();
    logs.sort();
    for p in logs {
        let t = std::fs::read_to_string(&p).unwrap_or_default();
        let lines: Vec<&str> = t.lines().collect();
        o += &format!("\n== {} (last {} of {} lines)\n", p.display(), lines.len().min(60), lines.len());
        for l in &lines[lines.len().saturating_sub(60)..] {
            o.push_str(l);
            o.push('\n');
        }
    }
    o
}

/// A line to zd1-linkd; `watch` and `as` stay and print what it says.
fn linkd(a: &[&str]) {
    let mut s =
        UnixStream::connect("/run/zd1link").unwrap_or_else(|e| zd1_sys::die("zd1-ctl", &format!("/run/zd1link: {e}")));
    let line = match a {
        ["watch"] => String::new(),
        ["as", source, rest @ ..] => {
            let state = rest.first().copied().unwrap_or("paused");
            format!("session on {source}\nstate {state} {source}\n")
        }
        ["as"] => zd1_sys::die("zd1-ctl", "as SOURCE [STATE]"),
        _ => format!("{}\n", a.join(" ")),
    };
    if let Err(e) = s.write_all(line.as_bytes()) {
        zd1_sys::die("zd1-ctl", &e.to_string());
    }
    if matches!(a[0], "watch" | "as") {
        for l in BufReader::new(s).lines().map_while(Result::ok) {
            out(&format!("zd1-ctl: {l}\n"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_form_is_decoded() {
        let f = form(b"ssid=My+Net%21&pass=a%25b+c%2");
        assert_eq!(f, vec![("ssid".into(), "My Net!".into()), ("pass".into(), "a%b c%2".into())]);
    }

    #[test]
    fn markup_is_escaped() {
        assert_eq!(esc("<a href=\"x\">&"), "&lt;a href=&quot;x&quot;&gt;&amp;");
    }

    #[test]
    fn bad_networks_are_refused_before_anything_is_written() {
        assert!(add_wifi("", "password").is_err());
        assert!(add_wifi(&"x".repeat(33), "password").is_err());
        assert!(add_wifi("net", "short").is_err());
        assert!(add_wifi("net", &"p".repeat(64)).is_err());
        assert!(add_wifi("net", "pass\u{7f}word").is_err());
    }
}
