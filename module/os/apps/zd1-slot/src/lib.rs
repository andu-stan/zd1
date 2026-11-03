//! The system slots (docs/module/storage.md): two MTD partitions, `system-a` and `system-b`, each a whole root file
//! system (JFFS2, read-only once installed: busybox, zd1, /etc, /usr) that the boot stub mounts as the root. The MCU
//! decides which one starts (mcu/src/modboot.c) and the stub puts it on the kernel command line: `zd1.slot=a|b` (or
//! `zd1.rescue`), and `zd1.good=a|b`, the slot the MCU falls back to. A slot holds a system when it has /sbin/init and
//! its stamp, /.zd1-system: what was installed, written last (an interrupted install leaves none).
//! Writing one: [`install`] erases the partition as `flash_eraseall -j` does (JFFS2 cleanmarkers in the spare areas),
//! mounts it, unpacks a gzip'd newc cpio of the root into it, writes the stamp. Never the running slot, nor the good
//! one while another runs on trial (it is the way back).

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};
use std::os::unix::io::AsRawFd;
use std::path::Path;
use zd1_sys::{cstr, mknod, mtd as ioctl};

mod gunzip;

pub const STAMP: &str = ".zd1-system";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Slot {
    A,
    B,
}

impl Slot {
    pub fn parse(s: &str) -> Option<Slot> {
        match s {
            "a" | "A" => Some(Slot::A),
            "b" | "B" => Some(Slot::B),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Slot::A => "a",
            Slot::B => "b",
        }
    }
    pub fn partition(self) -> &'static str {
        match self {
            Slot::A => "system-a",
            Slot::B => "system-b",
        }
    }
    pub fn other(self) -> Slot {
        match self {
            Slot::A => Slot::B,
            Slot::B => Slot::A,
        }
    }
}

/// What the stub started, from the kernel command line.
pub struct Boot {
    pub running: Option<Slot>,
    pub good: Option<Slot>,
    pub rescue: bool,
}

impl Boot {
    /// Where an update goes: the slot that doesn't run (the rescue, or an initramfs: the good one).
    pub fn target(&self) -> Slot {
        match self.running {
            Some(s) => s.other(),
            None => self.good.unwrap_or(Slot::A),
        }
    }
}

/// The good slot is the MCU's latest word: /run/zd1/good (zd1-linkd, from reg 0xC4 at every ready: a trial the MCU
/// has confirmed since the boot), else the command line's.
pub fn boot() -> Boot {
    let c = fs::read_to_string("/proc/cmdline").unwrap_or_default();
    let get = |k: &str| c.split_whitespace().find_map(|w| w.strip_prefix(k)).and_then(Slot::parse);
    let good = fs::read_to_string("/run/zd1/good").ok().and_then(|g| Slot::parse(g.trim())).or(get("zd1.good="));
    Boot { running: get("zd1.slot="), good, rescue: c.split_whitespace().any(|w| w == "zd1.rescue") }
}

/// The MTD number of partition `name` (/proc/mtd), its device nodes made (mdev on 2.6.24 makes neither the character
/// nodes nor, before init runs it, the block ones).
pub fn mtd(name: &str) -> Option<u32> {
    let t = fs::read_to_string("/proc/mtd").ok()?;
    let quoted = format!("\"{name}\"");
    let n: u32 = t.lines().find_map(|l| {
        let (dev, rest) = l.split_once(':')?;
        if rest.trim().rsplit(' ').next()? != quoted {
            return None;
        }
        dev.strip_prefix("mtd")?.parse().ok()
    })?;
    for (path, kind, minor) in [
        (format!("/dev/mtd{n}"), libc::S_IFCHR, n * 2),
        (format!("/dev/mtd{n}ro"), libc::S_IFCHR, n * 2 + 1),
        (format!("/dev/mtdblock{n}"), libc::S_IFBLK, n),
    ] {
        let major = if kind == libc::S_IFCHR { 90 } else { 31 };
        if !Path::new(&path).exists() {
            let _ = mknod(&path, kind, 0o600, major, minor);
        }
    }
    Some(n)
}

fn mount(dev: &str, at: &str, ro: bool) -> Result<(), String> {
    zd1_sys::mount(dev, at, "jffs2", if ro { libc::MS_RDONLY } else { 0 }, "").map_err(|e| e.to_string())
}
use zd1_sys::umount;

/// The slots' lock, across processes: a stamp read holds it shared while its read-only mount lives, an install
/// exclusive from the erase to the unmount. A second mount of a mounted JFFS2 partition shares the first one's
/// superblock, read-only too: an install started while zd1-linkd read the other slot's stamp (its first seconds after
/// boot) failed at its first write with EROFS, after erasing the partition under the reader [HW 2026-11-02]. None if
/// /run can't hold the file (nothing to share it with then).
fn lock(exclusive: bool) -> Option<fs::File> {
    let f = OpenOptions::new().create(true).truncate(false).write(true).mode(0o600).open("/run/zd1-slot.lock").ok()?;
    let op = if exclusive { libc::LOCK_EX } else { libc::LOCK_SH };
    while unsafe { libc::flock(f.as_raw_fd(), op) } != 0 {
        if io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return None;
        }
    }
    Some(f)
}

fn holds_system(root: &str) -> Option<String> {
    let init = fs::metadata(format!("{root}/sbin/init")).ok()?;
    if init.permissions().mode() & 0o111 == 0 {
        return None;
    }
    let s = fs::read_to_string(format!("{root}/{STAMP}")).ok()?;
    let s = s.trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Where `install` records what a slot holds (its stamp, empty: no system), so that reading it needs no mount: a
/// mount scans the partition's JFFS2 in the kernel for ~12 s, and this kernel doesn't preempt that, so zd1-linkd
/// stopped serving the MCU meanwhile (the SPI slave's FIFO overran: an Update BEGIN right after boot lost bytes in
/// 4 of 6 tries, none once the read had ended [HW 2026-11-02]).
fn record(s: Slot) -> String {
    format!("/data/zd1/stamp.{}", s.name())
}

/// What slot `s` holds (its stamp), None: no system. The running slot is `/`; another one is what `install` recorded,
/// or without a record (a slot written before records, or by other means) read from the slot itself once and recorded.
/// That read mounts it read-only at a mount point of its own: two readers at once (zd1-linkd and `zd1-ctl status`)
/// would each unmount the other's mount and read one slot as empty [HW 2026-11-01].
pub fn stamp(s: Slot) -> Option<String> {
    if boot().running == Some(s) {
        return holds_system("");
    }
    if let Ok(r) = fs::read_to_string(record(s)) {
        let r = r.trim();
        return (!r.is_empty()).then(|| r.to_string());
    }
    let r = stamp_from_slot(s);
    let _ = zd1_sys::write_private(&record(s), &format!("{}\n", r.as_deref().unwrap_or("")));
    r
}

fn stamp_from_slot(s: Slot) -> Option<String> {
    let n = mtd(s.partition())?;
    let _lock = lock(false);
    static READS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0); // threads share the pid
    let k = READS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let at = format!("/tmp/zd1-slot.{}.r{}.{k}", s.name(), std::process::id());
    fs::create_dir_all(&at).ok()?;
    let r = mount(&format!("/dev/mtdblock{n}"), &at, true).ok().and_then(|_| {
        let r = holds_system(&at);
        umount(&at);
        r
    });
    let _ = fs::remove_dir(&at);
    r
}

// ---- erasing: flash_eraseall -j (busybox miscutils/flash_eraseall.c) ----

/// Erase MTD partition `n` for JFFS2: every good block erased, a cleanmarker in its first page's spare area. Bad blocks
/// are skipped (JFFS2 skips them too). The number of bad blocks.
pub fn erase(n: u32) -> Result<u32, String> {
    let dev = format!("/dev/mtd{n}");
    let f = OpenOptions::new().read(true).write(true).open(&dev).map_err(|e| format!("{dev}: {e}"))?;
    let fd = f.as_raw_fd();
    let s = |e: io::Error| format!("{dev}: {e}");
    let info = ioctl::info(fd).map_err(s)?;
    if info.kind != ioctl::NANDFLASH {
        return Err(format!("{dev}: not NAND (type {})", info.kind));
    }
    let (pos, len) = ioctl::oob_free(fd, &info).map_err(s)?;
    if len == 0 {
        return Err("no free spare bytes for the cleanmarker".into());
    }
    let mut marker = [0u8; 12]; // jffs2_unknown_node: magic, nodetype, totlen, hdr_crc (JFFS2's CRC-32: from 0)
    marker[0..2].copy_from_slice(&0x1985u16.to_le_bytes());
    marker[2..4].copy_from_slice(&0x2003u16.to_le_bytes()); // CLEANMARKER (RWCOMPAT_DELETE | ACCURATE | 3)
    marker[4..8].copy_from_slice(&8u32.to_le_bytes());
    let crc = zd1_sys::crc32_update(0, &marker[..8]);
    marker[8..12].copy_from_slice(&crc.to_le_bytes());
    let mut bad = 0;
    let mut at = 0u32;
    while at < info.size {
        if ioctl::is_bad(fd, at).map_err(s)? {
            bad += 1;
        } else {
            ioctl::erase(fd, at, info.erasesize).map_err(s)?;
            ioctl::write_oob(fd, at + pos, &marker[..len as usize]).map_err(s)?;
        }
        at += info.erasesize;
    }
    Ok(bad)
}

// ---- unpacking: a newc cpio (module/os/mkcpio.py writes them) ----
fn hex(h: &[u8], field: usize) -> Result<u32, String> {
    let s = std::str::from_utf8(&h[6 + 8 * field..14 + 8 * field]).map_err(|_| "cpio: bad header")?;
    u32::from_str_radix(s, 16).map_err(|_| "cpio: bad header".into())
}
fn skip_pad(r: &mut impl Read, n: usize) -> io::Result<()> {
    let mut p = [0u8; 3];
    r.read_exact(&mut p[..(4 - n % 4) % 4])
}

/// The longest name or link target an entry may have (Linux's PATH_MAX): the sizes come from the image.
const PATH_MAX: usize = 4096;

/// A newc cpio from `r` into `dir`: directories, files, symlinks, device nodes, with their modes (owner: us). Entries
/// are written as they come; the bytes unpacked. Everything lands inside `dir`: no `..` or absolute names, nothing
/// under a symlink the archive made (it could point anywhere), files created new and never through a link.
pub fn unpack(r: &mut impl Read, dir: &str) -> Result<u64, String> {
    let mut total = 0u64;
    let mut buf = vec![0u8; 64 * 1024];
    let mut links: Vec<String> = Vec::new();
    loop {
        let mut h = [0u8; 110];
        r.read_exact(&mut h).map_err(|e| format!("cpio: {e}"))?;
        if &h[..6] != b"070701" {
            return Err("cpio: not newc".into());
        }
        let (mode, size, namesize) = (hex(&h, 1)?, hex(&h, 6)? as u64, hex(&h, 11)? as usize);
        let (rmaj, rmin) = (hex(&h, 9)?, hex(&h, 10)?);
        if namesize > PATH_MAX {
            return Err(format!("cpio: a name of {namesize} bytes"));
        }
        let mut name = vec![0u8; namesize];
        r.read_exact(&mut name).map_err(|e| format!("cpio: {e}"))?;
        skip_pad(r, 110 + namesize).map_err(|e| format!("cpio: {e}"))?;
        let name = String::from_utf8_lossy(&name[..namesize.saturating_sub(1)]).to_string();
        if name == "TRAILER!!!" {
            return Ok(total);
        }
        if name.split('/').any(|c| c == "..") || name.starts_with('/') {
            return Err(format!("cpio: {name}: outside"));
        }
        if name.match_indices('/').any(|(i, _)| links.iter().any(|l| *l == name[..i])) {
            return Err(format!("cpio: {name}: under a symlink"));
        }
        let path = format!("{dir}/{name}");
        let perm = mode & 0o7777;
        match mode & 0o170000 {
            0o040000 => {
                if !fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir()) {
                    fs::create_dir(&path).map_err(|e| format!("{name}: {e}"))?;
                }
                fs::set_permissions(&path, fs::Permissions::from_mode(perm)).map_err(|e| format!("{name}: {e}"))?;
            }
            0o100000 => {
                let mut f = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&path)
                    .map_err(|e| format!("{name}: {e}"))?;
                let mut left = size;
                while left > 0 {
                    let k = left.min(buf.len() as u64) as usize;
                    r.read_exact(&mut buf[..k]).map_err(|e| format!("cpio: {name}: {e}"))?;
                    f.write_all(&buf[..k]).map_err(|e| format!("{name}: {e}"))?;
                    left -= k as u64;
                }
                f.set_permissions(fs::Permissions::from_mode(perm)).map_err(|e| format!("{name}: {e}"))?;
                total += size;
            }
            0o120000 => {
                if size as usize > PATH_MAX {
                    return Err(format!("cpio: {name}: a link of {size} bytes"));
                }
                let mut t = vec![0u8; size as usize];
                r.read_exact(&mut t).map_err(|e| format!("cpio: {name}: {e}"))?;
                symlink(std::ffi::OsStr::from_bytes(&t), &path).map_err(|e| format!("{name}: {e}"))?;
                links.push(name.clone());
            }
            k @ (0o020000 | 0o060000) => mknod(&path, k, perm, rmaj, rmin).map_err(|e| e.to_string())?,
            _ => return Err(format!("cpio: {name}: unsupported type {mode:o}")),
        }
        if mode & 0o170000 != 0o120000 && mode & 0o170000 != 0o100000 && size > 0 {
            io::copy(&mut r.by_ref().take(size), &mut io::sink()).map_err(|e| format!("cpio: {e}"))?;
        }
        skip_pad(r, size as usize).map_err(|e| format!("cpio: {e}"))?;
    }
}

/// Install a system (a gzip'd newc cpio of the root, from `image`) into slot `s`, stamped `id`. The slot holds no
/// system from the erase until the stamp is written: after the whole gzip stream (its CRC checked) and `check` (the
/// caller's: the image's hash, what follows the gzip stream). A message for a person.
pub fn install<R: Read>(
    s: Slot,
    id: &str,
    image: &mut R,
    check: &mut dyn FnMut(&mut R) -> Result<(), String>,
) -> Result<String, String> {
    let b = boot();
    if b.running == Some(s) {
        return Err(format!("slot {} is running", s.name()));
    }
    if let (Some(run), Some(good)) = (b.running, b.good)
        && run != good
        && s == good
    {
        return Err(format!("slot {} is the good one and slot {} runs on trial: the way back", s.name(), run.name()));
    }
    if id.is_empty() || id.contains('\n') {
        return Err("a one-line ID".into());
    }
    let n = mtd(s.partition()).ok_or(format!("no partition {}", s.partition()))?;
    let at = format!("/tmp/zd1-slot.{}", s.name());
    fs::create_dir_all(&at).map_err(|e| format!("{at}: {e}"))?;
    let _lock = lock(true); // a stamp read of this slot ends first (seconds: its mount scans the JFFS2)
    umount(&at);
    let _ = zd1_sys::write_private(&record(s), "\n"); // no system from here until the stamp
    let bad = erase(n)?;
    mount(&format!("/dev/mtdblock{n}"), &at, false)?;
    let r = (|| -> Result<u64, String> {
        let mut gz = gunzip::Gunzip::new(&mut *image);
        let bytes = unpack(&mut gz, &at)?;
        io::copy(&mut gz, &mut io::sink()).map_err(|e| format!("the image: {e}"))?; // to its end: the CRC
        drop(gz);
        check(image)?;
        if holds_system_but_stamp(&at).is_none() {
            return Err("the image has no executable /sbin/init".into());
        }
        fs::write(format!("{at}/{STAMP}"), format!("{id}\n")).map_err(|e| format!("the stamp: {e}"))?;
        let _ = zd1_sys::write_private(&record(s), &format!("{id}\n"));
        unsafe {
            libc::sync();
        }
        Ok(bytes)
    })();
    let used = statfs_used_kb(&at);
    umount(&at);
    let bytes = r?;
    Ok(format!(
        "slot {}: {id} installed ({} KB of files, {} KB of the partition used{})",
        s.name(),
        bytes / 1024,
        used,
        if bad > 0 { format!(", {bad} bad blocks skipped") } else { String::new() }
    ))
}

fn holds_system_but_stamp(root: &str) -> Option<()> {
    (fs::metadata(format!("{root}/sbin/init")).ok()?.permissions().mode() & 0o111 != 0).then_some(())
}

fn statfs_used_kb(at: &str) -> u64 {
    let mut s: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(cstr(at).as_ptr(), &mut s) } != 0 {
        return 0;
    }
    (s.f_blocks - s.f_bfree) as u64 * s.f_bsize as u64 / 1024
}

// the program (zd1-slot), in the multicall (module/os/apps/zd1)
mod cli;
pub use cli::main;
