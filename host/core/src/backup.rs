//! The MCU's backup before a flash, as one file for the command line and the page alike: a tar of `app_flash.bin`
//! (0x80004000..0x8003FFFF, the app region), `user_page.bin` (0x80800000, 512 bytes), `bootloader.bin`
//! (0x80000000..0x80003FFF: BOOTPROT keeps DFU from writing it, not from reading it; B&W's other models may have
//! another, and only JTAG puts one back) and `manifest.json` (when it was read, the firmware it holds, each file's
//! size and SHA-256). Backups from before the bootloader was kept have no `bootloader.bin`. Taken back as MCU firmware
//! it is its app region (`McuImage::parse`); `tar x` gives the files.

use crate::image::{BOOTLOADER_END, FLASH_SIZE, USER_PAGE_SIZE, ident};
use crate::{Error, Result};

/// What a backup holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Backup {
    pub app: Vec<u8>,
    pub user: Option<Vec<u8>>,
    /// The bootloader region, read only (nothing here writes it back).
    pub bootloader: Option<Vec<u8>>,
    /// When it was read, UTC (`2026-11-01T19:30:00Z`).
    pub when: String,
}

const APP: &str = "app_flash.bin";
const USER: &str = "user_page.bin";
const BOOT: &str = "bootloader.bin";
const MANIFEST: &str = "manifest.json";

impl Backup {
    /// A backup read now.
    pub fn new(app: Vec<u8>, user: Option<Vec<u8>>, bootloader: Option<Vec<u8>>) -> Backup {
        Backup { app, user, bootloader, when: crate::utc_now() }
    }

    /// The firmware in it: our identification's version, or "unknown" (B&W's firmware, an empty app region).
    pub fn holds(&self) -> String {
        ident(&self.app).map_or("unknown".into(), |i| i.version)
    }

    /// Its file name: `mcu-backup-YYYYMMDD-HHMMSS-VERSION.tar` (when, what it holds).
    pub fn file_name(&self) -> String {
        let t: String = self.when.chars().filter(|c| c.is_ascii_digit()).collect();
        format!("mcu-backup-{}-{}-{}.tar", &t[..8.min(t.len())], &t[8.min(t.len())..], self.holds())
    }

    fn manifest(&self) -> String {
        let mut files = serde_json::Map::new();
        for (name, d) in self.files() {
            files.insert(name.into(), serde_json::json!({ "bytes": d.len(), "sha256": crate::hex(&crate::sha256(d)) }));
        }
        let m = serde_json::json!({
            "when": self.when,
            "holds": self.holds(),
            "files": files,
            "notes": format!("{APP} = 0x80004000..0x8003FFFF (the app region); {USER} = 0x80800000, 512 bytes; {BOOT} = \
                      0x80000000..0x80003FFF, the bootloader: BOOTPROT write-protected, read for the record (only JTAG \
                      writes it). Our tools never write fuses."),
        });
        serde_json::to_string_pretty(&m).unwrap_or_default() + "\n"
    }

    fn files(&self) -> impl Iterator<Item = (&'static str, &[u8])> {
        [(APP, Some(self.app.as_slice())), (USER, self.user.as_deref()), (BOOT, self.bootloader.as_deref())]
            .into_iter()
            .filter_map(|(n, d)| Some((n, d?)))
    }

    /// The tar.
    pub fn tar(&self) -> Vec<u8> {
        let manifest = self.manifest();
        let mut out = Vec::new();
        for (name, d) in self.files().chain([(MANIFEST, manifest.as_bytes())]) {
            let mut h = [0u8; 512];
            let mut put = |off: usize, s: &str| h[off..off + s.len()].copy_from_slice(s.as_bytes());
            put(0, name);
            put(100, "0000644\0");
            put(108, "0000000\0");
            put(116, "0000000\0");
            put(124, &format!("{:011o}\0", d.len()));
            put(136, &format!("{:011o}\0", 0));
            put(148, "        ");
            put(156, "0");
            put(257, "ustar\0");
            put(263, "00");
            let sum: u32 = h.iter().map(|b| *b as u32).sum();
            h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
            out.extend(h);
            out.extend(d);
            out.resize(out.len().next_multiple_of(512), 0);
        }
        out.extend([0; 1024]);
        out
    }

    /// A tar this program wrote (or one with the same files): `app_flash.bin` the size of the app region,
    /// `user_page.bin` 512 bytes and `bootloader.bin` the bootloader region's size if they are there, each as the
    /// manifest says when it says.
    pub fn from_tar(data: &[u8]) -> Result<Backup> {
        let bad = |why: String| Error::Failed(format!("not a backup: {why}"));
        let files = untar(data).map_err(|e| bad(e.into()))?;
        let get = |n: &str| files.iter().find(|(f, _)| f.rsplit('/').next() == Some(n)).map(|(_, d)| d.to_vec());
        let app = get(APP).ok_or_else(|| bad(format!("no {APP}")))?;
        if app.len() != (FLASH_SIZE - BOOTLOADER_END) as usize {
            return Err(bad(format!(
                "{APP} has {} bytes, not the app region's {}",
                app.len(),
                FLASH_SIZE - BOOTLOADER_END
            )));
        }
        let user = get(USER);
        if user.as_ref().is_some_and(|u| u.len() != USER_PAGE_SIZE as usize) {
            return Err(bad(format!("{USER} isn't 512 bytes")));
        }
        let bootloader = get(BOOT);
        if bootloader.as_ref().is_some_and(|b| b.len() != BOOTLOADER_END as usize) {
            return Err(bad(format!(
                "{BOOT} has {} bytes, not the bootloader region's {BOOTLOADER_END}",
                bootloader.as_ref().map_or(0, Vec::len)
            )));
        }
        let manifest: serde_json::Value =
            get(MANIFEST).and_then(|m| serde_json::from_slice(&m).ok()).unwrap_or(serde_json::Value::Null);
        for (name, d) in [(APP, Some(&app)), (USER, user.as_ref()), (BOOT, bootloader.as_ref())] {
            let (Some(d), Some(want)) = (d, manifest["files"][name]["sha256"].as_str()) else { continue };
            if crate::hex(&crate::sha256(d)) != want {
                return Err(bad(format!("{name}'s SHA-256 isn't the manifest's")));
            }
        }
        let when = manifest["when"].as_str().unwrap_or("").to_string();
        Ok(Backup { app, user, bootloader, when })
    }
}

/// A ustar archive: the regular files, (name, bytes).
fn untar(data: &[u8]) -> std::result::Result<Vec<(String, &[u8])>, &'static str> {
    let (mut files, mut off) = (Vec::new(), 0);
    while off + 512 <= data.len() {
        let h = &data[off..off + 512];
        if h.iter().all(|b| *b == 0) {
            return Ok(files);
        }
        if &h[257..262] != b"ustar" {
            return Err("not a tar");
        }
        let text = |r: std::ops::Range<usize>| {
            let f = &h[r];
            String::from_utf8_lossy(&f[..f.iter().position(|b| *b == 0).unwrap_or(f.len())]).into_owned()
        };
        let size = usize::from_str_radix(text(124..136).trim(), 8).map_err(|_| "a bad size")?;
        let body = data.get(off + 512..off + 512 + size).ok_or("cut short")?;
        if matches!(h[156], b'0' | 0) {
            files.push((text(0..100), body));
        }
        off += 512 + size.next_multiple_of(512);
    }
    if files.is_empty() { Err("not a tar") } else { Ok(files) }
}

/// The bytes are a tar (its ustar mark).
pub fn is_tar(data: &[u8]) -> bool {
    data.get(257..262) == Some(b"ustar")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let mut app = vec![0xFF; (FLASH_SIZE - BOOTLOADER_END) as usize];
        app[..4].copy_from_slice(&[1, 2, 3, 4]);
        let b = Backup {
            app,
            user: Some(vec![7; 512]),
            bootloader: Some(vec![9; BOOTLOADER_END as usize]),
            when: "2026-11-01T19:30:05Z".into(),
        };
        let t = b.tar();
        assert!(is_tar(&t) && t.len() % 512 == 0);
        assert_eq!(Backup::from_tar(&t).unwrap(), b);
        assert_eq!(b.file_name(), "mcu-backup-20261101-193005-unknown.tar");
        let mut bad = t.clone();
        bad[512 + 1] ^= 1; // app_flash.bin's second byte
        assert!(Backup::from_tar(&bad).is_err());
        assert!(Backup::from_tar(b"no tar at all").is_err());
        // a backup from before the bootloader was kept still reads
        let old = Backup { bootloader: None, ..b.clone() };
        assert_eq!(Backup::from_tar(&old.tar()).unwrap(), old);
        // a bootloader.bin of the wrong size is refused
        let short = Backup { bootloader: Some(vec![9; 100]), ..b };
        assert!(Backup::from_tar(&short.tar()).is_err());
    }
}
