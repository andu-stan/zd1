//! The early start, before any service: what the kernel leaves to PID 1.

use super::{Profile, say};
use dm870::DevMem;
use std::time::Instant;
use zd1_sys::{OsRelease, mknod, mount, umount};

/// What the early start found, for the services.
#[cfg_attr(not(feature = "full"), allow(dead_code))] // the command line: the system's services read it
pub struct Boot {
    /// The kernel's command line (switches: zd1.debug, zd1.noairplay, ...).
    pub cmdline: String,
    /// When the board came up: the Ethernet PHY needs a moment out of reset.
    pub board_up: Instant,
}

#[cfg_attr(not(feature = "full"), allow(dead_code))]
impl Boot {
    /// Whether the command line carries a switch (`zd1.nowifi`).
    pub fn switch(&self, name: &str) -> bool {
        self.cmdline.split_whitespace().any(|w| w == name)
    }

    /// A `key=value` on the command line.
    pub fn arg(&self, key: &str) -> Option<&str> {
        self.cmdline.split_whitespace().find_map(|w| w.strip_prefix(key)?.strip_prefix('='))
    }
}

/// Device nodes: /dev is a tmpfs (2.6.24 has no devtmpfs, and busybox's `mdev -s` wants /sys/dev, which it lacks
/// too). Hotplug still works: mdev makes the sound card's nodes when snd-sulu loads. The MTD nodes come from /proc/mtd.
const NODES: &[(&str, u32, u32, u32)] = &[
    ("console", 5, 1, 0o666),
    ("null", 1, 3, 0o666),
    ("zero", 1, 5, 0o666),
    ("tty", 5, 0, 0o666),
    ("ttyS0", 4, 64, 0o666),
    ("ttyS1", 4, 65, 0o666),
    ("random", 1, 8, 0o666),
    ("urandom", 1, 9, 0o666),
    ("mem", 1, 1, 0o600),
    ("kmsg", 1, 11, 0o666),
    ("ptmx", 5, 2, 0o666),
];

/// Everything before the services. Failures are said and the start goes on: a speaker with half a system still
/// talks to the MCU, which can put a working one back.
pub fn early(profile: Profile) -> Boot {
    let _ = mount("proc", "/proc", "proc", 0, "");
    let _ = mount("sys", "/sys", "sysfs", 0, "");
    // the kernel's log on the console too (it starts at level 1): the UART our MCU logs
    let _ = std::fs::write("/proc/sys/kernel/printk", "7");
    // RAM, and no swap: each tmpfs gets a ceiling (only what is written takes memory), so a runaway writer gets ENOSPC
    // rather than the receivers' memory. /var: the services' logs (init keeps each under 64 KB); /tmp: a program
    // copied over for a test, with its log
    let tmpfs = [("/dev", "755", "64k"), ("/run", "755", "512k"), ("/tmp", "1777", "8m"), ("/var", "755", "1m")];
    for (dir, mode, size) in tmpfs {
        let options = format!("mode={mode},size={size}");
        if let Err(e) = mount(dir.trim_start_matches('/'), dir, "tmpfs", 0, &options) {
            say(&e.to_string());
        }
    }
    for &(name, major, minor, mode) in NODES {
        let _ = mknod(&format!("/dev/{name}"), libc::S_IFCHR, mode, major, minor);
    }
    for d in ["/dev/pts", "/var/run", "/var/log", "/run/zd1", "/data"] {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = mount("devpts", "/dev/pts", "devpts", 0, "");
    let _ = std::fs::write("/proc/sys/kernel/hotplug", "/sbin/mdev");
    let mtd = std::fs::read_to_string("/proc/mtd").unwrap_or_default();
    for name in mtd.lines().filter_map(|l| l.split('"').nth(1)) {
        let _ = zd1_slot::mtd(name);
    }

    let cmdline = std::fs::read_to_string("/proc/cmdline").unwrap_or_default().trim().to_string();
    let boot = Boot { cmdline, board_up: Instant::now() };
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default().trim().to_string();
    match profile {
        #[cfg(feature = "full")]
        Profile::System => super::system::early(&boot, &kernel),
        #[cfg(not(feature = "full"))]
        Profile::System => unreachable!("the rescue's build has no system"),
        Profile::Rescue => {
            let release = OsRelease::read("/etc/os-release");
            say(&format!("zd1 rescue {}, kernel {kernel}", release.get("VERSION_ID")));
            data(false);
        }
    }
    board();
    Boot { board_up: Instant::now(), ..boot }
}

/// /data, the settings (docs/module/settings.md). A partition that isn't ours yet (no /data/.zd1-data: new, or the
/// stock firmware's leftovers) is formatted once by the system, never again for a new image (every version reads
/// what another wrote); the rescue only mounts ours. None, or no luck: RAM until the next boot.
pub(super) fn data(may_format: bool) {
    const MARK: &str = "/data/.zd1-data";
    const DATA_IN_RAM: &str = "size=1m"; // the settings and the AirPlay pairing: a few KB
    let Some(n) = zd1_slot::mtd("data") else {
        say("data: no partition (no mtdparts on the kernel command line): settings in RAM until the next boot");
        let _ = mount("data", "/data", "tmpfs", 0, DATA_IN_RAM);
        return;
    };
    let block = format!("/dev/mtdblock{n}");
    let ours = mount(&block, "/data", "jffs2", 0, "").is_ok() && std::path::Path::new(MARK).exists();
    if !ours {
        umount("/data");
        let formatted = may_format && {
            say("data: formatting the data partition");
            match zd1_slot::erase(n) {
                Ok(_) => mount(&block, "/data", "jffs2", 0, "").is_ok() && std::fs::write(MARK, "").is_ok(),
                Err(e) => {
                    say(&format!("data: {e}"));
                    false
                }
            }
        };
        if !formatted {
            if may_format {
                say("data: FAILED: settings in RAM until the next boot");
            }
            umount("/data");
            let _ = mount("data", "/data", "tmpfs", 0, DATA_IN_RAM);
        }
        unsafe {
            libc::sync();
        }
    }
    let _ = std::fs::create_dir_all("/data/zd1");
}

/// The board (module/os/apps/zd1-board): after the modules, so what the drivers own stays as they left it.
fn board() {
    match DevMem::open() {
        Ok(mut mem) => {
            zd1_board::up(&mut mem);
            say("board: pads routed, Ethernet PHY out of reset with its clock, clock gates on");
        }
        Err(e) => say(&format!("board: /dev/mem: {e}: the pads stay unrouted, the MCU won't hear us")),
    }
}
