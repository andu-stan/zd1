//! The receiver's pairing state on the data partition (`/data/zd1/airplay`, docs/module/storage.md): the accessory's
//! identity (the Ed25519 seed behind the `pk` it advertises) and the controllers paired with a PIN or added by an
//! admin (HAP's `/pairings`). Without it every start was a new accessory under the same device ID (a new `pk` in the
//! TXT record after each restart). At most [`MAX_CONTROLLERS`]: a new one pushes out the oldest that isn't an admin;
//! with only admins left, the store is full. No expiry: a pairing lasts until it is removed (`/pairings`, or the data
//! partition goes). Files are small and change rarely (the identity once, a line per pairing change): written whole
//! to a temporary file, synced, renamed over the old.

use shairplay::PairingStore;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Controllers kept at most.
pub const MAX_CONTROLLERS: usize = 16;

/// A paired controller: identifier, Ed25519 public key, admin.
type Controller = (String, [u8; 32], bool);

pub struct DataStore {
    dir: PathBuf,
    peers: Mutex<Vec<Controller>>, // oldest first
}

impl DataStore {
    /// The store in `dir` (created if missing). A store that can't be read starts empty: pairing still works, it
    /// just isn't remembered.
    pub fn open(dir: &Path) -> Self {
        if let Err(e) = std::fs::create_dir_all(dir) {
            tracing::warn!(dir = %dir.display(), %e, "pairing store: can't create");
        }
        // a line per controller, oldest first: `ID HEXKEY admin|user`
        let peers = std::fs::read_to_string(dir.join("peers"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let mut f = l.split(' ');
                let (id, pk, role) = (f.next()?, unhex32(f.next()?)?, f.next());
                Some((id.to_string(), pk, role != Some("user")))
            })
            .collect::<Vec<_>>();
        tracing::info!(dir = %dir.display(), peers = peers.len(), "pairing store");
        DataStore { dir: dir.to_path_buf(), peers: Mutex::new(peers) }
    }

    fn write(&self, name: &str, data: &[u8]) {
        let tmp = self.dir.join(format!(".{name}.new"));
        let r = std::fs::File::create(&tmp)
            .and_then(|mut f| f.write_all(data).and_then(|_| f.sync_all()))
            .and_then(|_| std::fs::rename(&tmp, self.dir.join(name)));
        if let Err(e) = r {
            tracing::warn!(name, %e, "pairing store: not saved");
        }
    }

    fn save_peers(&self, peers: &[Controller]) {
        let text: String = peers
            .iter()
            .map(|(id, pk, admin)| format!("{id} {} {}\n", hex(pk), if *admin { "admin" } else { "user" }))
            .collect();
        self.write("peers", text.as_bytes());
    }
}

impl PairingStore for DataStore {
    fn get(&self, device_id: &str) -> Option<[u8; 32]> {
        self.peers.lock().unwrap().iter().find(|c| c.0 == device_id).map(|c| c.1)
    }

    fn put(&self, device_id: &str, public_key: [u8; 32], admin: bool) -> bool {
        // a line per controller: ids with spaces or line breaks can't be told apart in the file
        if device_id.is_empty() || device_id.contains(char::is_whitespace) {
            tracing::warn!(device_id, "pairing store: controller id not stored");
            return false;
        }
        let mut peers = self.peers.lock().unwrap();
        if let Some(c) = peers.iter_mut().find(|c| c.0 == device_id) {
            if (c.1, c.2) != (public_key, admin) {
                (c.1, c.2) = (public_key, admin);
                self.save_peers(&peers);
            }
            return true;
        }
        if peers.len() >= MAX_CONTROLLERS {
            let Some(i) = peers.iter().position(|c| !c.2) else {
                tracing::warn!(device_id, "pairing store full of admins: controller not stored");
                return false;
            };
            let old = peers.remove(i);
            tracing::info!(dropped = old.0, device_id, "pairing store full: the oldest non-admin goes");
        }
        peers.push((device_id.to_string(), public_key, admin));
        self.save_peers(&peers);
        true
    }

    fn remove(&self, device_id: &str) {
        let mut peers = self.peers.lock().unwrap();
        let n = peers.len();
        peers.retain(|c| c.0 != device_id);
        if peers.len() != n {
            self.save_peers(&peers);
        }
    }

    fn has_any_pairing(&self) -> bool {
        !self.peers.lock().unwrap().is_empty()
    }

    fn is_admin(&self, device_id: &str) -> bool {
        self.peers.lock().unwrap().iter().any(|c| c.0 == device_id && c.2)
    }

    fn list(&self) -> Vec<(String, [u8; 32], bool)> {
        self.peers.lock().unwrap().clone()
    }

    fn load_identity(&self) -> Option<[u8; 32]> {
        std::fs::read(self.dir.join("identity")).ok()?.try_into().ok()
    }

    fn save_identity(&self, seed: [u8; 32]) {
        self.write("identity", &seed);
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|v| format!("{v:02x}")).collect()
}

fn unhex32(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_and_peers_survive_a_restart() {
        let dir = std::env::temp_dir().join(format!("zd1-airplay-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let s = DataStore::open(&dir);
        assert_eq!(s.load_identity(), None);
        s.save_identity([7; 32]);
        assert!(s.put("AA:BB", [1; 32], true));
        assert!(s.put("phone", [3; 32], false));
        assert!(!s.put("with space", [2; 32], true));
        let s = DataStore::open(&dir);
        assert_eq!(s.load_identity(), Some([7; 32]));
        assert_eq!(s.get("AA:BB"), Some([1; 32]));
        assert!(s.is_admin("AA:BB") && !s.is_admin("phone"));
        assert_eq!(s.get("with space"), None);
        s.remove("AA:BB");
        s.remove("phone");
        assert!(!DataStore::open(&dir).has_any_pairing());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_full_store_drops_the_oldest_non_admin() {
        let dir = std::env::temp_dir().join(format!("zd1-airplay-full-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let s = DataStore::open(&dir);
        assert!(s.put("admin", [0; 32], true));
        for k in 1..MAX_CONTROLLERS as u8 {
            assert!(s.put(&format!("c{k}"), [k; 32], false));
        }
        assert!(s.put("new", [99; 32], false));
        assert_eq!(s.get("c1"), None, "the oldest non-admin went");
        assert!(s.get("admin").is_some() && s.get("new").is_some());
        assert_eq!(s.list().len(), MAX_CONTROLLERS);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
