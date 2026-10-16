//! A small on-disk cache: one JSON file per key, written atomically, so
//! listings and release lists survive restarts and going offline.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::path::{Path, PathBuf};

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cached<T> {
    /// Format version; entries from another version are ignored.
    pub v: u32,
    pub checked_at: u64,
    pub data: T,
}

impl<T> Cached<T> {
    pub fn age(&self) -> u64 {
        now().saturating_sub(self.checked_at)
    }
}

const FORMAT: u32 = 1;

#[derive(Debug, Clone)]
pub struct Cache {
    dir: PathBuf,
}

impl Cache {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn path(&self, kind: &str, key: &str, ext: &str) -> PathBuf {
        let h = hex::encode(Sha1::digest(key.as_bytes()));
        self.dir.join(format!("{kind}-{}.{ext}", &h[..20]))
    }

    pub fn get<T: DeserializeOwned>(&self, kind: &str, key: &str) -> Option<Cached<T>> {
        let text = std::fs::read_to_string(self.path(kind, key, "json")).ok()?;
        serde_json::from_str::<Cached<T>>(&text).ok().filter(|c| c.v == FORMAT)
    }

    pub fn put<T: Serialize>(&self, kind: &str, key: &str, data: &T) {
        let entry = Cached { v: FORMAT, checked_at: now(), data };
        let path = self.path(kind, key, "json");
        let r = (|| -> std::io::Result<()> {
            std::fs::create_dir_all(&self.dir)?;
            let tmp = path.with_extension(format!("tmp{}", std::process::id()));
            std::fs::write(&tmp, serde_json::to_vec(&entry)?)?;
            std::fs::rename(&tmp, &path)
        })();
        if let Err(e) = r {
            log::warn!("mod cache: could not write {}: {e}", path.display());
        }
    }

    pub fn remove(&self, kind: &str, key: &str) {
        let _ = std::fs::remove_file(self.path(kind, key, "json"));
    }
}
