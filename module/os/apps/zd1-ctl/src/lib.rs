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
//! Which slot starts is the MCU's (`zd1 slot`); writing one: `zd1-slot install`.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use zd1_slot::Slot;

const USAGE: &str = "usage: zd1-ctl [status] | start|stop|restart SERVICE | debug on|off|default | changed KEY... | \
                     state playing|paused|stopped [SOURCE] | volume N | log TEXT | watch | as SOURCE [STATE]";

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
