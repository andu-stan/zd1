//! Stock-MCU mode: the Zeppelin Air with Lightning, B&W's MCU firmware kept (docs/stock-mcu.md). Its MCU bootloader
//! takes no image over USB, so the image (os-zd1l.bcd, through BL2's web updater) brings the system along: the
//! initramfs is the rescue plus /system.cpio.gz and its stamp /system.id (module/os/assemble.py, lightning). The stock
//! MCU doesn't answer the stub's slot question (reg 0xC4), so every boot starts here, in the rescue: install the
//! system into slot A when slot A holds another one, then switch the root to slot A, its init as PID 1.
//! The rescue says nothing on the MCU link meanwhile (the stub's BOOTING stays): the stock MCU starts its setup task
//! once on READY and is stuck when the module goes silent after it (module/boot linuxboot.rs). Only the system's
//! zd1-linkd says READY.

use super::say;
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::Path;
use zd1_slot::Slot;

const SYSTEM: &str = "/system.cpio.gz";
const STAMP: &str = "/system.id";
const NEWROOT: &str = "/newroot";

/// Whether this boot is stock-MCU mode (`zd1.mcu=stock` on the command line: the Lightning's image).
pub fn mode(cmdline: &str) -> bool {
    cmdline.split_whitespace().any(|w| w == "zd1.mcu=stock")
}

/// Install the system if slot A doesn't hold it, then switch to slot A. Returns only when that failed: the rescue's
/// services run then (its console), and the stock MCU, never told READY, eventually reports its system error.
pub fn take_over() {
    if !Path::new(SYSTEM).exists() {
        say("stock-MCU mode: no system in this image");
        return;
    }
    let id = fs::read_to_string(STAMP).unwrap_or_default().trim().to_string();
    let have = zd1_slot::stamp(Slot::A);
    if have.as_deref() != Some(id.as_str()) {
        say(&format!(
            "stock-MCU mode: installing system {id} into slot A (it holds {})",
            have.as_deref().unwrap_or("none")
        ));
        let r = fs::File::open(SYSTEM)
            .map_err(|e| format!("{SYSTEM}: {e}"))
            .and_then(|mut f| zd1_slot::install(Slot::A, &id, &mut f, &mut |_: &mut fs::File| Ok(())));
        match r {
            Ok(m) => say(&m),
            Err(e) => {
                say(&format!("stock-MCU mode: the install failed: {e}"));
                return;
            }
        }
    }
    let _ = fs::remove_file(SYSTEM); // its RAM back before the system starts
    let Some(n) = zd1_slot::mtd(Slot::A.partition()) else {
        say("stock-MCU mode: no slot A partition");
        return;
    };
    if let Err(e) = zd1_sys::mount(&format!("/dev/mtdblock{n}"), NEWROOT, "jffs2", libc::MS_RDONLY, "") {
        say(&format!("stock-MCU mode: slot A: {e}"));
        return;
    }
    // the system's init mounts these itself: the data partition (JFFS2) must not be mounted twice
    for m in ["/data", "/dev/pts"] {
        zd1_sys::umount(m);
    }
    say("stock-MCU mode: switching to slot A");
    let err = std::process::Command::new("/bin/switch_root").args([NEWROOT, "/sbin/init"]).exec();
    say(&format!("stock-MCU mode: switch_root: {err}"));
    zd1_sys::umount(NEWROOT);
}
