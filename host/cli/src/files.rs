//! The files zd1 reads and writes: this checkout's default images, the unit's MCU backups, and what an image file is
//! (`zd1 image`, the release manifest's lines).

use std::path::{Path, PathBuf};
use zd1_host::backup::Backup;
use zd1_host::image::{self, Kind};
use zd1_host::{Error, Events, Result};

/// The repository (default images, units/): ZD1_REPO, else the directory above with flake.nix and host/.
pub fn repo() -> PathBuf {
    if let Some(r) = std::env::var_os("ZD1_REPO") {
        return r.into();
    }
    let here = std::env::current_dir().unwrap_or_default();
    here.ancestors()
        .find(|d| d.join("flake.nix").exists() && d.join("host").is_dir())
        .map_or(here.clone(), Path::to_path_buf)
}

pub fn read(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|e| Error::Failed(format!("{}: {e}", path.display())))
}

/// A file as the release manifest describes it: `{file, kind, size, sha256, version, built|stamp}`; `kind` mcu
/// (Intel HEX or the app region's .bin), backup (an MCU backup), system (a gzip'd cpio) or bcd (the module's boot
/// image).
pub fn image_json(path: &Path) -> Result<String> {
    let data = read(path)?;
    let name = path.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
    let mut j =
        serde_json::json!({ "file": name, "size": data.len(), "sha256": zd1_host::hex(&zd1_host::sha256(&data)) });
    match image::identify(&name, &data)? {
        Kind::System { version, stamp } => {
            j["kind"] = "system".into();
            j["version"] = version.into();
            j["stamp"] = stamp.into();
        }
        Kind::Bcd { build, ours } => {
            j["kind"] = "bcd".into();
            j["build"] = build.into();
            j["ours"] = ours.into();
        }
        Kind::Backup(b) => {
            j["kind"] = "backup".into();
            j["version"] = b.holds().into();
            j["when"] = b.when.into();
        }
        Kind::Mcu(img) => {
            j["kind"] = "mcu".into();
            if let Some(i) = img.ident() {
                j["version"] = i.version.into();
                j["built"] = i.built.into();
            }
        }
    }
    Ok(j.to_string())
}

/// An MCU backup into `to`: that file, or a directory (the backup's own name in it); never over an existing file,
/// which is a way back too.
pub fn save_backup(to: &Path, b: &Backup, ev: &dyn Events) -> Result<()> {
    let path = if to.is_dir() || to.extension().is_none() { to.join(b.file_name()) } else { to.to_path_buf() };
    let fail = |e: std::io::Error| Error::Failed(format!("{}: {e}", path.display()));
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(fail)?;
    }
    let tar = b.tar();
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, &tar))
        .map_err(fail)?;
    ev.step(&format!(
        "backup of {} in {} (sha {})",
        b.holds(),
        path.display(),
        &zd1_host::hex(&zd1_host::sha256(&b.app))[..16]
    ));
    Ok(())
}

/// Where the unit's MCU backups go: units/<unit>/mcu/dfu-backups (the one unit in units/ unless said).
pub fn backup_dir(unit: Option<PathBuf>) -> Result<PathBuf> {
    let unit = match unit {
        Some(u) => u,
        None => {
            let units: Vec<PathBuf> = std::fs::read_dir(repo().join("units"))
                .map(|d| d.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect())
                .unwrap_or_default();
            match units.as_slice() {
                [u] => u.clone(),
                [] => return Err(Error::Failed("no unit in units/: say where the backup goes with --unit DIR".into())),
                _ => return Err(Error::Failed("several units in units/: say which with --unit DIR".into())),
            }
        }
    };
    Ok(unit.join("mcu/dfu-backups"))
}
