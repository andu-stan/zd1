//! zd1: the module OS's Rust programs in one executable, busybox style: one copy of std and the libraries for all of
//! them. Each program is its crate's `main` (module/os/apps/NAME, a library). The name it is started as picks the
//! program (the images have a link per program, so `ps` and `killall` know them by name), or else its first
//! argument: `zd1 PROGRAM ARGS...`, with or without the `zd1-` (`zd1 airplay --name x`: a copy run from /tmp while
//! working on it); it then starts itself again under the program's name.
//! Two builds (docs/module/storage.md): the core, in the rescue initramfs (/bin/zd1: init's rescue profile, the MCU
//! link, settings, the slot writer), built `--no-default-features`; and the full one in the system slots
//! (/usr/bin/zd1: everything). The core holds only what the rescue runs, so the rescue changes only when that does.
//! The system's debug build (`debug`) adds the tools a running speaker shouldn't carry: the self test, the memory
//! shell (raw memory for anyone on the network), PersParam's raw reads, the MCU link's request trace.

use std::os::unix::process::CommandExt;

/// The programs: name, entry point.
const PROGRAMS: &[(&str, fn())] = &[
    ("init", zd1_init::main), // /sbin/init, the rescue's /init: PID 1
    ("zd1-linkd", zd1_linkd::main),
    ("zd1-ctl", zd1_ctl::main),
    ("zd1-board", zd1_board::main),
    ("zd1-persparam", zd1_persparam::main),
    ("zd1-settings", zd1_settings::main),
    ("zd1-slot", zd1_slot::main),
    ("zd1-update", zd1_update::main),
    #[cfg(feature = "full")]
    ("zd1-dhcp", zd1_net::main),
    #[cfg(feature = "full")]
    ("zd1-entropy", zd1_entropy::main),
    #[cfg(feature = "full")]
    ("zd1-snapcast", zd1_snapcast::main),
    #[cfg(feature = "full")]
    ("zd1-airplay", zd1_airplay::main),
    #[cfg(feature = "full")]
    ("zd1-spotify", zd1_spotify::main),
    #[cfg(feature = "debug")]
    ("zd1-selftest", zd1_selftest::main),
    #[cfg(feature = "debug")]
    ("zd1-memsh", zd1_memsh::main),
];

fn find(name: &str) -> Option<&'static (&'static str, fn())> {
    PROGRAMS.iter().find(|(n, _)| *n == name || n.strip_prefix("zd1-") == Some(name))
}

fn main() {
    zd1_compat::init();
    let mut args = std::env::args();
    let argv0 = args.next().unwrap_or_default();
    if let Some((_, run)) = find(argv0.rsplit('/').next().unwrap_or("")) {
        return run();
    }
    if let Some(&(name, _)) = args.next().as_deref().and_then(find) {
        let e = std::process::Command::new("/proc/self/exe").arg0(name).args(args).exec();
        eprintln!("zd1: {name}: {e}");
        std::process::exit(1);
    }
    let names: Vec<&str> = PROGRAMS.iter().map(|(n, _)| *n).collect();
    eprintln!("usage: zd1 PROGRAM [ARGS...], or a link named PROGRAM; the programs: {}", names.join(" "));
    std::process::exit(2);
}
