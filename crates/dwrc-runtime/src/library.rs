//! The player's game library: imported discs and BIOS images.
//!
//! Discs are never copied or modified. Re:Compiled remembers where they
//! are, verifies them once (full SHA-1) at import, and afterwards only does
//! a quick size/mtime check before each launch.

use crate::bios::{identify_bios, BiosInfo};
use crate::paths::Paths;
use anyhow::{anyhow, bail, Context, Result};
use dwrc_disc::ident::{sha1_file, verified_dump, Region};
use dwrc_disc::{Disc, GameId, Identification};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameEntry {
    /// The `.cue` (or single `.bin`) the player picked.
    pub disc_path: PathBuf,
    pub ident: Identification,
    pub sha1: String,
    pub size: u64,
    pub mtime: u64,
    /// The full image matches a dump verified with Re:Compiled.
    pub verified: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BiosEntry {
    pub path: PathBuf,
    pub info: BiosInfo,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Library {
    pub games: BTreeMap<GameId, GameEntry>,
    pub bios: Vec<BiosEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscStatus {
    NotImported,
    Ready,
    Missing,
    Changed,
}

/// Result of scanning a folder.
#[derive(Debug, Default, Clone)]
pub struct ScanReport {
    pub games: Vec<(GameId, String, bool)>,
    pub bios: Vec<String>,
    pub skipped: Vec<(PathBuf, String)>,
}

fn file_stamp(p: &Path) -> Option<(u64, u64)> {
    let m = std::fs::metadata(p).ok()?;
    let mtime = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    Some((m.len(), mtime))
}

/// Resolves the image file(s) for a disc path: a `.cue` is used as-is; for
/// a `.bin`, a sibling `.cue` with the same name is preferred.
pub fn resolve_disc_path(p: &Path) -> PathBuf {
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if ext == "bin" {
        let cue = p.with_extension("cue");
        if cue.exists() {
            return cue;
        }
        let cue_upper = p.with_extension("CUE");
        if cue_upper.exists() {
            return cue_upper;
        }
    }
    p.to_path_buf()
}

impl Library {
    pub fn load(paths: &Paths) -> Self {
        std::fs::read_to_string(paths.library_file())
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, paths: &Paths) -> Result<()> {
        paths.ensure()?;
        let t = serde_json::to_string_pretty(self)?;
        std::fs::write(paths.library_file(), t)?;
        Ok(())
    }

    pub fn status(&self, g: GameId) -> DiscStatus {
        let Some(e) = self.games.get(&g) else { return DiscStatus::NotImported };
        let Ok(disc) = Disc::open(&e.disc_path) else { return DiscStatus::Missing };
        let files = disc.file_paths();
        let Some(first) = files.first() else { return DiscStatus::Missing };
        match file_stamp(first) {
            None => DiscStatus::Missing,
            Some((size, mtime)) if size == e.size && mtime == e.mtime => DiscStatus::Ready,
            Some(_) => DiscStatus::Changed,
        }
    }

    /// Identifies and verifies a disc image, adding it to the library.
    /// `progress` receives a 0..1 hashing fraction.
    pub fn import_disc(&mut self, path: &Path, progress: &mut dyn FnMut(f32)) -> Result<GameId> {
        let disc_path = resolve_disc_path(path);
        let mut disc = Disc::open(&disc_path).with_context(|| format!("could not open {}", disc_path.display()))?;
        let ident = dwrc_disc::identify(&mut disc)
            .map_err(|e| anyhow!("{e}"))?
            .ok_or_else(|| anyhow!("{} is not a supported Digimon World disc", disc_path.display()))?;
        let files = disc.file_paths();
        if files.len() != 1 {
            bail!("multi-file images are not supported (found {} files)", files.len());
        }
        let bin = &files[0];
        let (size, mtime) = file_stamp(bin).ok_or_else(|| anyhow!("cannot stat {}", bin.display()))?;
        let sha1 = sha1_file(bin, |d, t| progress(d as f32 / t.max(1) as f32))?;
        let verified = verified_dump(&sha1).map(|d| d.serial == ident.serial).unwrap_or(false);
        let game = ident.game;
        self.games.insert(game, GameEntry { disc_path, ident, sha1, size, mtime, verified });
        Ok(game)
    }

    pub fn import_bios(&mut self, path: &Path) -> Result<BiosInfo> {
        let data = std::fs::read(path)?;
        let info = identify_bios(&data).ok_or_else(|| anyhow!("{} is not a PlayStation BIOS image", path.display()))?;
        self.bios.retain(|b| b.info.sha1 != info.sha1);
        self.bios.push(BiosEntry { path: path.to_path_buf(), info: info.clone() });
        Ok(info)
    }

    /// Picks the best BIOS for a disc region: a verified one of the same
    /// region first, then any of the same region.
    pub fn bios_for(&self, region: Region) -> Option<&BiosEntry> {
        let mut c: Vec<&BiosEntry> = self.bios.iter().filter(|b| b.info.region == region && b.path.exists()).collect();
        c.sort_by_key(|b| b.info.known_name.is_none());
        c.first().copied()
    }

    /// Finds discs and BIOS images in a folder (recursively, a few levels).
    pub fn scan_folder(&mut self, dir: &Path, progress: &mut dyn FnMut(&str, f32)) -> ScanReport {
        let mut report = ScanReport::default();
        let mut candidates = Vec::new();
        collect_files(dir, 0, &mut candidates);
        // Prefer .cue files; only consider .bin files without a cue.
        let cues: Vec<PathBuf> = candidates
            .iter()
            .filter(|p| p.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("cue")).unwrap_or(false))
            .cloned()
            .collect();
        let mut seen_bins = std::collections::HashSet::new();
        for c in &cues {
            if let Ok(d) = Disc::open(c) {
                for f in d.file_paths() {
                    seen_bins.insert(f);
                }
            }
        }
        for p in &candidates {
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
            let len = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
            if (ext == "bin" || ext == "rom") && len == crate::bios::BIOS_SIZE as u64 {
                match self.import_bios(p) {
                    Ok(info) => report.bios.push(format!(
                        "{} ({})",
                        info.known_name.clone().unwrap_or(info.version.clone()),
                        crate::bios::region_label(info.region)
                    )),
                    Err(e) => report.skipped.push((p.clone(), e.to_string())),
                }
            }
        }
        let mut discs: Vec<PathBuf> = cues.clone();
        for p in &candidates {
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
            let len = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
            if ext == "bin" && len > 50 << 20 && !seen_bins.contains(p) {
                discs.push(p.clone());
            }
        }
        for p in discs {
            let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let mut cb = |f: f32| progress(&name, f);
            match self.import_disc(&p, &mut cb) {
                Ok(g) => {
                    let e = &self.games[&g];
                    report.games.push((g, e.ident.title.clone(), e.verified));
                }
                Err(e) => report.skipped.push((p, e.to_string())),
            }
        }
        report
    }
}

fn collect_files(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    if depth > 3 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_files(&p, depth + 1, out);
        } else {
            out.push(p);
        }
    }
}
