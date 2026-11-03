//! The services: what runs once the early start is done, in this order, and how each behaves. A service's `start`
//! looks at the settings and the command line when it is (re)started and says what to run, or why not, or when to try
//! again; the supervisor does the rest.
// The rescue's build uses only part of this vocabulary (the system's table, system.rs, the rest).
#![cfg_attr(not(feature = "full"), allow(dead_code))]

use super::start::Boot;
use super::{Profile, say};
use std::collections::BTreeMap;
use std::time::Duration;

pub struct Service {
    pub name: &'static str,
    pub about: &'static str,
    pub start: fn(&Ctx) -> Start,
    /// After its processes have ended (a stop or a restart): what it leaves behind, cleared.
    pub stop: Option<fn()>,
    pub lifetime: Lifetime,
    pub output: Output,
    /// The settings it uses: a change restarts it. "wifi." = every key that starts so.
    pub settings: &'static [&'static str],
}

pub enum Lifetime {
    /// Runs until stopped; started again `again_after` it ends (longer while it keeps ending soon after starting).
    Daemon { again_after: Duration },
    /// Runs to its end. `wait`: the services after it start once it has ended; `retry`: a failure is tried again.
    Once { wait: bool, retry: Option<Backoff> },
}

/// Retries: `first` apart `times` times, then `then` apart, for good.
pub struct Backoff {
    pub first: Duration,
    pub times: u32,
    pub then: Duration,
}

impl Backoff {
    pub fn after(&self, attempt: u32) -> Duration {
        if attempt < self.times { self.first } else { self.then }
    }
}

/// Where a service's output goes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Output {
    /// /var/log/NAME.log, through init; the older part in NAME.log.1: a log at 32 KB, and a daemon's when it ends
    /// (with how), become that.
    Log,
    /// The log and, line by line, the kernel's (the console: the MCU logs it).
    LogAndKernel,
    /// The console, as it is (the self test's report).
    Console,
    /// The console as a login shell's terminal: a session of its own, the console its controlling tty.
    Shell,
}

/// What a service's `start` gets: the boot, the settings as they are now, the debug services' mode from the MCU,
/// and how many times in a row it said [`Start::Later`].
pub struct Ctx<'a> {
    pub boot: &'a Boot,
    pub settings: BTreeMap<String, String>,
    pub debug: Option<bool>,
    pub attempt: u32,
}

impl Ctx<'_> {
    pub fn setting(&self, key: &str) -> String {
        zd1_settings::get_from(&self.settings, key).unwrap_or_default()
    }
}

pub enum Start {
    Run(Vec<Command>),
    Off(String),
    Later(Duration, String),
}

/// A program and its arguments; `arg0` if it must see another name (a login shell's "-sh").
pub struct Command {
    pub argv: Vec<String>,
    pub arg0: Option<&'static str>,
}

pub(super) fn run(argv: &[&str]) -> Start {
    Start::Run(vec![command(argv)])
}

pub(super) fn command(argv: &[&str]) -> Command {
    Command { argv: argv.iter().map(|s| s.to_string()).collect(), arg0: None }
}

pub(super) const fn daemon(again_after_ms: u64) -> Lifetime {
    Lifetime::Daemon { again_after: Duration::from_millis(again_after_ms) }
}

/// The system's table is in `system` (the full build only): the rescue's build, which runs only RESCUE, doesn't
/// change when a system service does.
pub fn table(profile: Profile) -> &'static [Service] {
    match profile {
        #[cfg(feature = "full")]
        Profile::System => super::system::SERVICES,
        #[cfg(not(feature = "full"))]
        Profile::System => RESCUE,
        Profile::Rescue => RESCUE,
    }
}

const RESCUE: &[Service] = &[
    Service {
        name: "zd1-linkd",
        about: "the MCU link: \"zd1-os VERSION rescue\", and zd1-update when it asks",
        start: |_| run(&["zd1-linkd"]),
        stop: None,
        lifetime: daemon(1000),
        output: Output::LogAndKernel,
        settings: &[],
    },
    Service {
        name: "console",
        about: "a shell on the console",
        start: shell,
        stop: None,
        lifetime: daemon(1000),
        output: Output::Shell,
        settings: &[],
    },
];

/// Once every service has been started: what the emulator and the docs wait for. The slots' stamps aren't read here:
/// reading one mounts its slot, which PID 1 doesn't do (`zd1-ctl status`, and the State record, have them).
pub fn up(profile: Profile) {
    match profile {
        Profile::System => say("up"),
        Profile::Rescue => {
            let b = zd1_slot::boot();
            let name = |s: Option<zd1_slot::Slot>| s.map(|s| s.name()).unwrap_or("?");
            say(&format!("good slot: {}", name(b.good)));
            say("waiting for a system (zd1 update); a shell on the console");
        }
    }
}

// ---- the rescue's starts (the system's: system.rs) ----

pub(super) fn shell(_: &Ctx) -> Start {
    Start::Run(vec![Command { argv: vec!["/bin/sh".into()], arg0: Some("-sh") }])
}
