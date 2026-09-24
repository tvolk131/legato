//! On-disk state: this machine's identity key and the list of paired peers.
//!
//! The identity key is kept in a file readable only by the current user. It moves to the
//! OS keychain once builds are signed: on macOS, every unsigned rebuild would otherwise
//! trigger a keychain access prompt.

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use iroh::{EndpointId, SecretKey};
use legato_proto::Os;
use serde::{Deserialize, Serialize};

const IDENTITY_FILE: &str = "identity.key";
const PEERS_FILE: &str = "peers.json";

/// Environment variable overriding the state directory, e.g. to run two instances on one
/// machine.
pub const HOME_ENV: &str = "LEGATO_HOME";

/// The default state directory, honouring [`HOME_ENV`].
pub fn default_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os(HOME_ENV) {
        return Ok(PathBuf::from(dir));
    }
    let dirs = directories::ProjectDirs::from("io.github", "tvolk131", "Legato")
        .context("could not determine the user's data directory")?;
    Ok(dirs.data_dir().to_path_buf())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedPeer {
    #[serde(with = "endpoint_id_str")]
    pub id: EndpointId,
    pub name: String,
    pub os: Os,
    /// Seconds since the Unix epoch.
    pub paired_at: u64,
}

#[derive(Debug)]
pub struct Store {
    dir: PathBuf,
    peers: RwLock<Vec<PairedPeer>>,
}

impl Store {
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let peers_path = dir.join(PEERS_FILE);
        let peers = match fs::read(&peers_path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("reading {}", peers_path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", peers_path.display())),
        };
        Ok(Self {
            dir,
            peers: RwLock::new(peers),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Loads this machine's secret key, creating one on first run.
    pub fn identity(&self) -> Result<SecretKey> {
        let path = self.dir.join(IDENTITY_FILE);
        match fs::read(&path) {
            Ok(bytes) => {
                let Ok(bytes) = <[u8; 32]>::try_from(bytes.as_slice()) else {
                    bail!("{} is corrupt: expected 32 bytes", path.display());
                };
                Ok(SecretKey::from_bytes(&bytes))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let key = SecretKey::generate();
                write_private(&path, &key.to_bytes())?;
                Ok(key)
            }
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn peers(&self) -> Vec<PairedPeer> {
        self.peers.read().unwrap().clone()
    }

    pub fn peer(&self, id: &EndpointId) -> Option<PairedPeer> {
        self.peers
            .read()
            .unwrap()
            .iter()
            .find(|p| &p.id == id)
            .cloned()
    }

    pub fn peer_ids(&self) -> HashSet<EndpointId> {
        self.peers.read().unwrap().iter().map(|p| p.id).collect()
    }

    /// Adds or updates a paired peer and saves the list.
    pub fn add_peer(&self, id: EndpointId, name: String, os: Os) -> Result<PairedPeer> {
        let peer = PairedPeer {
            id,
            name,
            os,
            paired_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
        };
        let mut peers = self.peers.write().unwrap();
        peers.retain(|p| p.id != id);
        peers.push(peer.clone());
        self.save(&peers)?;
        Ok(peer)
    }

    /// Removes a paired peer. Returns whether it was present.
    pub fn remove_peer(&self, id: &EndpointId) -> Result<bool> {
        let mut peers = self.peers.write().unwrap();
        let before = peers.len();
        peers.retain(|p| &p.id != id);
        let removed = peers.len() != before;
        if removed {
            self.save(&peers)?;
        }
        Ok(removed)
    }

    fn save(&self, peers: &[PairedPeer]) -> Result<()> {
        let json = serde_json::to_vec_pretty(peers)?;
        write_private(&self.dir.join(PEERS_FILE), &json)
    }
}

/// Writes a file atomically, readable only by the current user.
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    // On Windows the per-user app data directory is already private to the user.
    let mut file = options
        .open(&tmp)
        .with_context(|| format!("writing {}", tmp.display()))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

mod endpoint_id_str {
    use iroh::EndpointId;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(id: &EndpointId, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(id)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<EndpointId, D::Error> {
        String::deserialize(d)?.parse().map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "legato-store-test-{}-{}",
            std::process::id(),
            SecretKey::generate().public().fmt_short()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn identity_is_created_once_and_reused() {
        let dir = temp_dir();
        let first = Store::open(&dir).unwrap().identity().unwrap();
        let second = Store::open(&dir).unwrap().identity().unwrap();
        assert_eq!(first.public(), second.public());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.join(IDENTITY_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn peers_persist() {
        let dir = temp_dir();
        let id = SecretKey::generate().public();
        {
            let store = Store::open(&dir).unwrap();
            store.add_peer(id, "MacBook".into(), Os::MacOs).unwrap();
            // Re-pairing replaces rather than duplicates.
            store
                .add_peer(id, "Tommy's MacBook".into(), Os::MacOs)
                .unwrap();
        }
        let store = Store::open(&dir).unwrap();
        let peers = store.peers();
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].name, "Tommy's MacBook");
        assert!(store.remove_peer(&id).unwrap());
        assert!(Store::open(&dir).unwrap().peers().is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn corrupt_identity_is_an_error_not_a_new_key() {
        let dir = temp_dir();
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(IDENTITY_FILE), b"short").unwrap();
        assert!(Store::open(&dir).unwrap().identity().is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
