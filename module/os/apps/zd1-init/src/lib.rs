//! zd1-init: PID 1 of the module OS (a system slot, the emulator's dev boot) and of the rescue. One program, two
//! profiles, told apart by the root it finds itself on (the rescue's /etc/os-release says `VARIANT_ID=rescue`).
//!
//! 1. The early start (start.rs): file systems, /dev, the MTD nodes, the identification, the data partition, the
//!    kernel's modules, the board (module/dm870: the pads, the PHY, the clocks).
//! 2. The services (services.rs), started in order from one table, each with how it starts (a function that reads
//!    the settings and the command line and says what to run, or why not, or when to try again), how it ends and
//!    restarts, where its output goes, and the settings that restart it.
//! 3. The supervisor (supervisor.rs): one loop that reaps every child (PID 1 inherits the orphans), restarts what
//!    stopped, answers its control socket (/run/zd1/init: zd1-ctl, zd1-settings, zd1-linkd), and on `reboot` / `poweroff` / `halt` (busybox's, which signal PID 1)
//!    stops everything and restarts or halts the chip.
//!
//! `[os] up` on the kernel log once every service has been started is what the emulator waits for; the rescue's
//! "waiting for a system" too. A broken init in a new slot fails its trial boot, and the MCU goes back to the good
//! one (docs/module/storage.md).

mod services;
mod start;
mod supervisor;
#[cfg(feature = "full")]
mod system;

/// Which root we run.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Profile {
    /// A system slot (or the dev boot's initramfs): everything.
    System,
    /// The app partition's initramfs: the MCU link and a shell, waiting for `zd1 update`.
    Rescue,
}

impl Profile {
    fn here() -> Profile {
        // the core build is the rescue's (the system runs the full one): there is no system in it to run
        if !cfg!(feature = "full") {
            return Profile::Rescue;
        }
        let release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
        if release.lines().any(|l| l == "VARIANT_ID=rescue") { Profile::Rescue } else { Profile::System }
    }

    /// How our lines start on the kernel log.
    pub fn tag(self) -> &'static str {
        if self == Profile::Rescue { "[rescue]" } else { "[os]" }
    }
}

static TAG: std::sync::OnceLock<&str> = std::sync::OnceLock::new();

/// A line on the kernel log, our profile's tag first.
pub fn say(s: &str) {
    zd1_sys::kmsg(TAG.get().copied().unwrap_or("[os]"), s);
}

pub fn main() {
    if std::process::id() != 1 {
        eprintln!("zd1-init runs as PID 1; the services it runs: zd1-ctl status");
        std::process::exit(2);
    }
    let profile = Profile::here();
    let _ = TAG.set(profile.tag());
    let boot = start::early(profile);
    supervisor::run(profile, boot)
}
