//! Finding, installing and updating mods over the network, after
//! Gen1Recomp's design:
//!
//! * **Indexes** ([`index`]): GitHub repositories that publish a JSON feed of
//!   mods. The Re:Compiled index is built in; players can add others.
//! * **Updates** ([`github`]): a mod that names its GitHub repository is
//!   checked against that repository's Releases; any release can be
//!   installed (update or roll back).
//! * **Downloads** go through the same `.zip` installer as a manual import.
//!
//! Everything here is blocking; the launcher calls it from worker threads.
//! Feeds are cached for a day and release lists for six hours, and a failed
//! fetch falls back to the cached copy, so going offline degrades the
//! listing instead of emptying it.

pub mod cache;
pub mod github;
pub mod http;
pub mod index;
pub mod text;

pub use cache::Cache;
pub use github::Release;
pub use http::{Http, Progress};
pub use index::{Index, IndexEntry, IndexSource};

use anyhow::Result;
use std::path::{Path, PathBuf};

/// How long a fetched index is trusted before it is fetched again.
pub const INDEX_TTL: u64 = 24 * 60 * 60;
/// How long a mod's release list is trusted.
pub const RELEASES_TTL: u64 = 6 * 60 * 60;

/// The outcome of reading one index.
#[derive(Debug, Clone)]
pub struct IndexFetch {
    pub index: Option<Index>,
    /// Set when the live fetch failed (the cached copy, if any, is shown).
    pub error: Option<String>,
    pub from_cache: bool,
    pub checked_at: u64,
    /// The base URL relative paths resolve against (the mirror's when the
    /// feed came from the fallback).
    pub base: String,
}

/// The outcome of reading a mod's releases.
#[derive(Debug, Clone)]
pub struct ReleasesFetch {
    pub releases: Vec<Release>,
    pub error: Option<String>,
    pub from_cache: bool,
    pub checked_at: u64,
}

#[derive(Clone)]
pub struct Hub {
    pub http: Http,
    pub cache: Cache,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CachedIndex {
    index: Index,
    base: String,
}

impl Hub {
    pub fn new(cache_dir: impl Into<PathBuf>) -> Self {
        Self { http: Http::new(), cache: Cache::new(cache_dir) }
    }

    /// Reads an index: the cached copy while it is fresh (unless `force`),
    /// else the live feed (then its mirror), falling back to a stale cache.
    pub fn fetch_index(&self, src: &IndexSource, force: bool) -> IndexFetch {
        let cached = self.cache.get::<CachedIndex>("index", &src.feed);
        if let Some(c) = &cached {
            if !force && c.age() < INDEX_TTL {
                return IndexFetch { index: Some(c.data.index.clone()), error: None, from_cache: true, checked_at: c.checked_at, base: c.data.base.clone() };
            }
        }
        let attempt = |url: &str, base: &str| -> Result<(Index, String), String> {
            let body = self.http.get_text(url, Some("application/json")).map_err(|e| format!("{e:#}"))?;
            index::parse(&body).map(|ix| (ix, base.to_string()))
        };
        let mut result = attempt(&src.feed, &src.base);
        if let (Err(first), Some((url, base))) = (&result, &src.fallback) {
            match attempt(url, base) {
                Ok(ok) => result = Ok(ok),
                Err(_) => result = Err(first.clone()),
            }
        }
        match result {
            Ok((ix, base)) => {
                self.cache.put("index", &src.feed, &CachedIndex { index: ix.clone(), base: base.clone() });
                IndexFetch { index: Some(ix), error: None, from_cache: false, checked_at: cache::now(), base }
            }
            Err(e) => match cached {
                Some(c) => IndexFetch { index: Some(c.data.index), error: Some(e), from_cache: true, checked_at: c.checked_at, base: c.data.base },
                None => IndexFetch { index: None, error: Some(e), from_cache: false, checked_at: 0, base: src.base.clone() },
            },
        }
    }

    /// Reads the installable releases of mod `id` in GitHub repository
    /// `repo` (cached for six hours unless `force`).
    pub fn fetch_releases(&self, repo: &str, id: &str, force: bool) -> ReleasesFetch {
        let key = format!("{repo}#{id}");
        let cached = self.cache.get::<Vec<Release>>("releases", &key);
        if let Some(c) = &cached {
            if !force && c.age() < RELEASES_TTL {
                return ReleasesFetch { releases: c.data.clone(), error: None, from_cache: true, checked_at: c.checked_at };
            }
        }
        let live = self.http.github(&github::releases_path(repo)).and_then(|j| github::parse_releases(&j, id));
        match live {
            Ok(r) => {
                self.cache.put("releases", &key, &r);
                ReleasesFetch { releases: r, error: None, from_cache: false, checked_at: cache::now() }
            }
            Err(e) => {
                let msg = format!("{e:#}");
                let msg = if msg.starts_with("not found") { format!("{repo} was not found on GitHub") } else { msg };
                match cached {
                    Some(c) => ReleasesFetch { releases: c.data, error: Some(msg), from_cache: true, checked_at: c.checked_at },
                    None => ReleasesFetch { releases: Vec::new(), error: Some(msg), from_cache: false, checked_at: 0 },
                }
            }
        }
    }

    /// A text file (an index entry's description), cached alongside feeds.
    pub fn fetch_text(&self, url: &str) -> Result<String> {
        if let Some(c) = self.cache.get::<String>("text", url) {
            if c.age() < INDEX_TTL {
                return Ok(c.data);
            }
        }
        let t = self.http.get_text(url, None)?;
        self.cache.put("text", url, &t);
        Ok(t)
    }

    /// A small image (≤ 2 MB), cached on disk.
    pub fn fetch_image(&self, url: &str) -> Result<Vec<u8>> {
        let path = self.cache.path("image", url, "bin");
        if let Ok(meta) = std::fs::metadata(&path) {
            let fresh = meta.modified().ok().and_then(|m| m.elapsed().ok()).is_some_and(|age| age.as_secs() < 7 * INDEX_TTL);
            if fresh {
                if let Ok(b) = std::fs::read(&path) {
                    return Ok(b);
                }
            }
        }
        let b = self.http.get_bytes(url, 2 << 20)?;
        let _ = std::fs::create_dir_all(self.cache.dir());
        let _ = std::fs::write(&path, &b);
        Ok(b)
    }

    /// Downloads a mod `.zip` into `dir` and returns its path.
    pub fn download_zip(&self, url: &str, dir: &Path, progress: &Progress) -> Result<PathBuf> {
        let name = format!("download-{}.zip", &hex_hash(url)[..16]);
        let dest = dir.join(name);
        self.http.download(url, &dest, progress)?;
        Ok(dest)
    }
}

fn hex_hash(s: &str) -> String {
    use sha1::{Digest, Sha1};
    hex::encode(Sha1::digest(s.as_bytes()))
}

/// An available update for an installed mod.
#[derive(Debug, Clone)]
pub struct UpdateOffer {
    pub version: String,
    pub url: String,
    /// Release notes, when the offer comes from a GitHub release.
    pub notes: Option<String>,
    /// Where it came from: `GitHub` or an index label.
    pub from: String,
}

/// The best update for a mod at `installed`, from its GitHub releases
/// (preferred) or, for mods without a repository, from index listings.
pub fn update_offer(installed: &str, releases: Option<&[Release]>, listings: &[(&IndexSource, &IndexEntry)]) -> Option<UpdateOffer> {
    if let Some(rs) = releases.filter(|r| !r.is_empty()) {
        let best = github::best(rs)?;
        return dwrc_mods::version::is_newer(&best.version, installed).then(|| UpdateOffer {
            version: best.version.clone(),
            url: best.zip.url.clone(),
            notes: Some(best.body.clone()).filter(|b| !b.trim().is_empty()),
            from: "GitHub".into(),
        });
    }
    listings
        .iter()
        .filter_map(|(src, e)| {
            let v = e.display_version();
            let url = e.install_url().ok()?;
            dwrc_mods::version::is_newer(&v, installed).then(|| UpdateOffer { version: v, url, notes: None, from: src.label.clone() })
        })
        .max_by(|a, b| dwrc_mods::version::compare(&a.version, &b.version))
}
