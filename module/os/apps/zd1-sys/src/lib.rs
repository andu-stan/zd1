//! What the module OS's programs share below their own work: a line on the kernel log, the system calls std lacks
//! (mount, mknod), a private file written whole, the MTD ioctls, and the formats (CRC-32, hex, MACs,
//! /etc/os-release). The receivers' default name too: the speaker's, from a MAC.

use std::ffi::CString;
use std::io::{self, Write};
use std::path::Path;

pub mod mtd;

// ---- the kernel log ----

/// A line on the kernel log: the console (which the MCU logs: `zd1 log`), dmesg, JTAG; `tag` first ("[os]"). Without
/// /dev/kmsg (a host, a test): stderr.
pub fn kmsg(tag: &str, line: &str) {
    if !kmsg_raw(format!("{tag} {line}\n").as_bytes()) {
        eprintln!("{tag} {line}");
    }
}

/// Bytes onto the kernel log as they are (one line, its newline included); false if there is no kernel log.
pub fn kmsg_raw(line: &[u8]) -> bool {
    std::fs::OpenOptions::new().write(true).open("/dev/kmsg").is_ok_and(|mut k| k.write_all(line).is_ok())
}

/// The program's error and its end: "PROGRAM: why" on stderr, exit status 1.
pub fn die(program: &str, why: &str) -> ! {
    eprintln!("{program}: {why}");
    std::process::exit(1)
}

// ---- system calls ----

/// A path for a system call (one with a NUL in it becomes "", which the call refuses).
pub fn cstr(s: &str) -> CString {
    CString::new(s).unwrap_or_default()
}

#[cfg(target_os = "linux")]
fn check(r: libc::c_int, what: &str) -> io::Result<()> {
    if r != 0 {
        let e = io::Error::last_os_error();
        return Err(io::Error::new(e.kind(), format!("{what}: {e}")));
    }
    Ok(())
}

/// mount(2): `fs` from `source` at `at` (created if missing), with its options (`mode=755,size=64k`).
#[cfg(target_os = "linux")]
pub fn mount(source: &str, at: &str, fs: &str, flags: libc::c_ulong, options: &str) -> io::Result<()> {
    let _ = std::fs::create_dir_all(at);
    let opts = cstr(options);
    let data = if options.is_empty() { std::ptr::null() } else { opts.as_ptr() as *const libc::c_void };
    let r = unsafe { libc::mount(cstr(source).as_ptr(), cstr(at).as_ptr(), cstr(fs).as_ptr(), flags, data) };
    check(r, &format!("mount {fs} {source} on {at}"))
}

#[cfg(target_os = "linux")]
pub fn umount(at: &str) {
    unsafe {
        libc::umount(cstr(at).as_ptr());
    }
}

#[cfg(target_os = "linux")]
/// mknod(2): a device node, `kind` libc::S_IFCHR or S_IFBLK, with its permissions (umask doesn't apply).
pub fn mknod(path: &str, kind: libc::mode_t, perm: libc::mode_t, major: u32, minor: u32) -> io::Result<()> {
    let p = cstr(path);
    check(unsafe { libc::mknod(p.as_ptr(), kind | perm, libc::makedev(major, minor)) }, path)?;
    check(unsafe { libc::chmod(p.as_ptr(), perm) }, path)
}

// ---- files ----

/// A file written whole and only root reads it (it may hold Wi-Fi keys): a new file, synced, renamed over the old one
/// (a power cut leaves the old file or the new one), the directory synced; the directory made if missing.
pub fn write_private(path: &str, text: &str) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let named = |e: io::Error, p: &str| io::Error::new(e.kind(), format!("{p}: {e}"));
    let dir = Path::new(path).parent().unwrap_or(Path::new("/"));
    std::fs::create_dir_all(dir).map_err(|e| named(e, &dir.display().to_string()))?;
    let tmp = format!("{path}.new");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| named(e, &tmp))?;
    f.write_all(text.as_bytes()).and_then(|_| f.sync_all()).map_err(|e| named(e, &tmp))?;
    drop(f);
    std::fs::rename(&tmp, path).map_err(|e| named(e, path))?;
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

/// /etc/os-release and its kind: `KEY="value"` lines.
pub struct OsRelease(Vec<(String, String)>);

impl OsRelease {
    pub fn read(path: &str) -> OsRelease {
        OsRelease::parse(&std::fs::read_to_string(path).unwrap_or_default())
    }

    pub fn parse(text: &str) -> OsRelease {
        OsRelease(
            text.lines()
                .filter_map(|l| l.split_once('='))
                .map(|(k, v)| (k.to_string(), v.trim_matches('"').to_string()))
                .collect(),
        )
    }

    /// The value of `key`, "" if there is none.
    pub fn get(&self, key: &str) -> &str {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str()).unwrap_or("")
    }
}

// ---- formats ----

const CRC_TABLE: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

/// CRC-32 (reflected, 0xEDB88320) carried on from `crc` over `b`, with no inversion either side: zlib's and gzip's
/// is `!crc32_update(!0, b)`, JFFS2's `crc32_update(0, b)`.
pub fn crc32_update(mut crc: u32, b: &[u8]) -> u32 {
    for &x in b {
        crc = CRC_TABLE[((crc ^ x as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc
}

/// zlib's CRC-32 of `b`.
pub fn crc32(b: &[u8]) -> u32 {
    !crc32_update(!0, b)
}

/// Lower-case hex.
pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Hex (either case) back to bytes; None for an odd length or a non-hex digit.
pub fn unhex(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    if b.len() % 2 != 0 {
        return None;
    }
    let d = |c: u8| (c as char).to_digit(16);
    b.chunks(2).map(|p| Some((d(p[0])? * 16 + d(p[1])?) as u8)).collect()
}

/// A MAC address.
pub type Mac = [u8; 6];

/// `aa:bb:cc:dd:ee:ff` (either case).
pub fn parse_mac(s: &str) -> Option<Mac> {
    let v: Vec<u8> =
        s.trim().split(':').map(|h| unhex(h).filter(|b| b.len() == 1).map(|b| b[0])).collect::<Option<_>>()?;
    v.try_into().ok()
}

/// `aa:bb:cc:dd:ee:ff`.
pub fn format_mac(m: &Mac) -> String {
    m.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":")
}

/// eth0's MAC as the kernel has it (init sets the unit's at boot).
pub fn eth0_mac() -> Option<Mac> {
    parse_mac(&std::fs::read_to_string("/sys/class/net/eth0/address").ok()?)
}

/// The speaker's name before anyone chose one: "Zeppelin Air XXXXXX" from its MAC's last three bytes, like the stock
/// module.
pub fn default_name(mac: Option<Mac>) -> String {
    match mac {
        Some(m) => format!("Zeppelin Air {:02X}{:02X}{:02X}", m[3], m[4], m[5]),
        None => "Zeppelin Air".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926, "the check value");
        assert_eq!(!crc32_update(crc32_update(!0, b"1234"), b"56789"), 0xCBF4_3926, "in pieces");
        assert_eq!(crc32_update(0, &[]), 0);
    }

    #[test]
    fn hex_and_macs() {
        assert_eq!(hex(&[0, 0xAB, 0x7F]), "00ab7f");
        assert_eq!(unhex("00aB7f"), Some(vec![0, 0xAB, 0x7F]));
        assert_eq!(unhex("0"), None);
        assert_eq!(unhex("x1é"), None);
        let m = parse_mac("EC:66:d1:00:00:42\n").unwrap();
        assert_eq!(format_mac(&m), "ec:66:d1:00:00:42");
        assert_eq!(parse_mac("ec:66:d1:00:00"), None);
        assert_eq!(parse_mac("ec:66:d1:00:00:4"), None);
        assert_eq!(default_name(Some(m)), "Zeppelin Air 000042");
        assert_eq!(default_name(None), "Zeppelin Air");
    }

    #[test]
    fn os_release() {
        let r = OsRelease::parse("NAME=\"zd1 module OS\"\nVERSION_ID=\"abc\"\nVARIANT_ID=rescue\n");
        assert_eq!(
            (r.get("NAME"), r.get("VERSION_ID"), r.get("VARIANT_ID"), r.get("X")),
            ("zd1 module OS", "abc", "rescue", "")
        );
    }
}
