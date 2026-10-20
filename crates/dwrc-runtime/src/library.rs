//! The player's game library: imported discs and BIOS images.
//!
//! Discs are never copied or modified. Re:Compiled remembers where they
//! are, verifies them once (full SHA-1) at import, and afterwards only does
//! a quick size/mtime check before each launch.
//!
//! A game can have several discs (other regions, other copies); one of them
//! is chosen on the Games page and is the one that plays.

use crate::bios::{identify_bios, BiosInfo};
use crate::paths::Paths;
use anyhow::{anyhow, bail, Context, Result};
use dwrc_disc::cue::CueSheet;
use dwrc_disc::ident::{sha1_file, verified_dump, Region};
use dwrc_disc::{Disc, GameId, Identification};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameEntry {
    /// The `.cue` (or single `.bin`) that is opened.
    pub disc_path: PathBuf,
    /// The image file itself (`disc_path` may be its `.cue`). Identifies
    /// the disc in the library.
    #[serde(default)]
    pub bin: PathBuf,
    pub ident: Identification,
    pub sha1: String,
    pub size: u64,
    pub mtime: u64,
    /// The full image matches a dump verified with Re:Compiled.
    pub verified: bool,
}

impl GameEntry {
    /// The image's file name, to tell copies of one release apart.
    pub fn file_name(&self) -> String {
        let p = if self.bin.as_os_str().is_empty() { &self.disc_path } else { &self.bin };
        p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
    }

    fn is_image(&self, bin: &Path) -> bool {
        same_file(&self.bin, bin)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BiosEntry {
    pub path: PathBuf,
    pub info: BiosInfo,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Library {
    /// The disc each game plays: the one chosen on the Games page.
    pub games: BTreeMap<GameId, GameEntry>,
    /// Every imported disc, the chosen ones included.
    #[serde(default)]
    pub discs: Vec<GameEntry>,
    pub bios: Vec<BiosEntry>,
}

/// Which BIOS a session uses.
#[derive(Debug, Clone, Copy)]
pub enum BiosChoice<'a> {
    /// A BIOS dumped from the player's console.
    Console(&'a BiosEntry),
    /// The built-in OpenBIOS.
    BuiltIn,
}

impl BiosChoice<'_> {
    pub fn label(&self) -> String {
        match self {
            BiosChoice::Console(b) => b.info.known_name.clone().unwrap_or_else(|| b.info.version.clone()),
            BiosChoice::BuiltIn => crate::bios::BUILTIN_NAME.to_string(),
        }
    }
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

/// Whether two paths name the same file (compared as given when either
/// cannot be resolved, e.g. because it is missing).
fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

fn has_ext(p: &Path, ext: &str) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

/// The files a `.cue` names, if it can be read.
fn cue_files(cue: &Path) -> Option<Vec<PathBuf>> {
    CueSheet::load(cue).ok().map(|s| s.files)
}

/// Resolves what to open for a disc image:
/// * a `.cue` is used when the files it names exist; a `.cue` whose `.bin`
///   was renamed falls back to the `.bin` with the cue's own name;
/// * a `.bin` is opened through the `.cue` beside it with the same name,
///   but only when that cue describes this `.bin` (two copies of a disc in
///   one folder can share a cue that names only one of them).
pub fn resolve_disc_path(p: &Path) -> PathBuf {
    if has_ext(p, "cue") {
        if cue_files(p).is_some_and(|f| !f.is_empty() && f.iter().all(|f| f.exists())) {
            return p.to_path_buf();
        }
        for ext in ["bin", "BIN"] {
            let bin = p.with_extension(ext);
            if bin.exists() {
                return bin;
            }
        }
    } else if has_ext(p, "bin") {
        for ext in ["cue", "CUE"] {
            let cue = p.with_extension(ext);
            if cue.exists() && cue_files(&cue).is_some_and(|f| f.len() == 1 && same_file(&f[0], p)) {
                return cue;
            }
        }
    }
    p.to_path_buf()
}

/// What to try opening for a disc path, best first: [`resolve_disc_path`],
/// then, when that is a `.cue`, the `.bin` it names and the `.bin` named like
/// it, and for a `.bin` the `.bin` on its own. A `.cue` that is there but
/// unusable (bad syntax, an odd track layout) so never stops its `.bin`.
fn open_candidates(p: &Path) -> Vec<PathBuf> {
    let first = resolve_disc_path(p);
    let mut v = vec![first.clone()];
    if has_ext(&first, "cue") {
        v.extend(cue_files(&first).into_iter().flatten().filter(|f| f.exists()));
        v.extend(["bin", "BIN"].iter().map(|e| first.with_extension(e)).filter(|b| b.exists()));
    }
    if has_ext(p, "bin") {
        v.push(p.to_path_buf());
    }
    let mut out: Vec<PathBuf> = Vec::new();
    for c in v {
        if !out.iter().any(|o| same_file(o, &c)) {
            out.push(c);
        }
    }
    out
}

/// The image file behind a disc path (the `.bin` a `.cue` names).
fn image_file(disc_path: &Path) -> Option<PathBuf> {
    Disc::open(disc_path).ok().and_then(|d| d.file_paths().into_iter().next())
}

/// Opens an imported disc: through its `.cue` when that still works,
/// otherwise its `.bin` on its own (a `.cue` deleted or changed since).
pub fn open_disc(e: &GameEntry) -> Result<Disc> {
    match Disc::open(&e.disc_path) {
        Ok(d) => Ok(d),
        Err(err) if !e.bin.as_os_str().is_empty() && e.bin.exists() && !same_file(&e.bin, &e.disc_path) => {
            log::info!("{} could not be used ({err}); opening {} on its own", e.disc_path.display(), e.bin.display());
            Disc::open(&e.bin).with_context(|| format!("could not open {}", e.bin.display()))
        }
        Err(err) => Err(anyhow!("could not open {}: {err}", e.disc_path.display())),
    }
}

/// Whether an imported disc is still there, unchanged since it was verified.
/// The image file is what counts; its `.cue` may have gone (see [`open_disc`]).
pub fn disc_status(e: &GameEntry) -> DiscStatus {
    let bin = if !e.bin.as_os_str().is_empty() && e.bin.exists() {
        e.bin.clone()
    } else {
        match image_file(&e.disc_path) {
            Some(b) => b,
            None => return DiscStatus::Missing,
        }
    };
    match file_stamp(&bin) {
        None => DiscStatus::Missing,
        Some((size, mtime)) if size == e.size && mtime == e.mtime => DiscStatus::Ready,
        Some(_) => DiscStatus::Changed,
    }
}

/// Order of discs in a game's list: by region, then title, then file name.
fn region_rank(r: Region) -> u8 {
    match r {
        Region::NtscU => 0,
        Region::Pal => 1,
        Region::NtscJ => 2,
    }
}

impl Library {
    pub fn load(paths: &Paths) -> Self {
        let mut lib: Self = std::fs::read_to_string(paths.library_file())
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        lib.normalize();
        lib
    }

    /// Brings a library written by an older version up to date: fills in
    /// each disc's image file and lists every chosen disc among `discs`.
    fn normalize(&mut self) {
        for e in self.games.values_mut().chain(self.discs.iter_mut()) {
            if e.bin.as_os_str().is_empty() {
                if let Some(b) = image_file(&e.disc_path) {
                    e.bin = b;
                }
            }
        }
        for e in self.games.values() {
            if !self.discs.iter().any(|d| d.is_image(&e.bin) && d.ident.game == e.ident.game) {
                self.discs.push(e.clone());
            }
        }
    }

    pub fn save(&self, paths: &Paths) -> Result<()> {
        paths.ensure()?;
        let t = serde_json::to_string_pretty(self)?;
        std::fs::write(paths.library_file(), t)?;
        Ok(())
    }

    pub fn status(&self, g: GameId) -> DiscStatus {
        match self.games.get(&g) {
            Some(e) => disc_status(e),
            None => DiscStatus::NotImported,
        }
    }

    /// Every disc imported for a game, in display order.
    pub fn discs_for(&self, g: GameId) -> Vec<&GameEntry> {
        let mut v: Vec<&GameEntry> = self.discs.iter().filter(|e| e.ident.game == g).collect();
        v.sort_by(|a, b| {
            (region_rank(a.ident.region), &a.ident.title, a.file_name().to_lowercase())
                .cmp(&(region_rank(b.ident.region), &b.ident.title, b.file_name().to_lowercase()))
        });
        v
    }

    /// Makes the disc with image file `bin` the one its game plays.
    pub fn select(&mut self, g: GameId, bin: &Path) -> bool {
        match self.discs.iter().find(|e| e.ident.game == g && e.is_image(bin)) {
            Some(e) => {
                self.games.insert(g, e.clone());
                true
            }
            None => false,
        }
    }

    /// Removes a disc from the list (the file itself is not touched). When
    /// it was the chosen one, another disc of the game takes its place.
    pub fn forget(&mut self, g: GameId, bin: &Path) {
        self.discs.retain(|e| !(e.ident.game == g && e.is_image(bin)));
        if self.games.get(&g).is_some_and(|e| e.is_image(bin)) {
            self.games.remove(&g);
            self.choose_if_unplayable(g);
        }
    }

    /// Picks a playable disc for a game whose chosen one is missing, changed
    /// or not set: verified dumps first, then in list order.
    fn choose_if_unplayable(&mut self, g: GameId) {
        if self.status(g) == DiscStatus::Ready {
            return;
        }
        let pick = {
            let ready: Vec<&GameEntry> = self.discs_for(g).into_iter().filter(|e| disc_status(e) == DiscStatus::Ready).collect();
            ready.iter().find(|e| e.verified).or(ready.first()).map(|e| (*e).clone())
        };
        if let Some(e) = pick {
            self.games.insert(g, e);
        }
    }

    /// Adds (or refreshes) a disc. The game plays it when `choose` is set or
    /// when the game has no disc yet; a refreshed copy of the chosen disc
    /// stays chosen.
    fn add(&mut self, e: GameEntry, choose: bool) {
        let g = e.ident.game;
        self.discs.retain(|d| !(d.ident.game == g && d.is_image(&e.bin)));
        let was_chosen = self.games.get(&g).is_some_and(|c| c.is_image(&e.bin));
        if choose || was_chosen || !self.games.contains_key(&g) {
            self.games.insert(g, e.clone());
        }
        self.discs.push(e);
    }

    /// Identifies and verifies a disc image, adding it to the library as the
    /// disc its game plays. `progress` receives a 0..1 hashing fraction.
    pub fn import_disc(&mut self, path: &Path, progress: &mut dyn FnMut(f32)) -> Result<GameId> {
        self.import(path, true, progress)
    }

    fn import(&mut self, path: &Path, choose: bool, progress: &mut dyn FnMut(f32)) -> Result<GameId> {
        // The first way of opening it that gives a supported disc wins; the
        // error reported is the first one.
        let mut first_err = None;
        let mut found = None;
        for p in open_candidates(path) {
            let r = Disc::open(&p).map_err(|e| anyhow!("could not open {}: {e}", p.display())).and_then(|mut d| {
                match dwrc_disc::identify(&mut d) {
                    Ok(Some(ident)) => Ok((d, ident)),
                    Ok(None) => Err(anyhow!("{} is not a supported Digimon World disc", p.display())),
                    Err(e) => Err(anyhow!("{}: {e}", p.display())),
                }
            });
            match r {
                Ok((d, ident)) => {
                    found = Some((p, d, ident));
                    break;
                }
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            }
        }
        let Some((disc_path, disc, ident)) = found else {
            return Err(first_err.unwrap_or_else(|| anyhow!("nothing to open at {}", path.display())));
        };
        let files = disc.file_paths();
        if files.len() != 1 {
            bail!("multi-file images are not supported (found {} files)", files.len());
        }
        let bin = files[0].clone();
        let (size, mtime) = file_stamp(&bin).ok_or_else(|| anyhow!("cannot stat {}", bin.display()))?;
        // An image already verified and unchanged since is not hashed again.
        let known = self.discs.iter().find(|e| e.is_image(&bin) && e.size == size && e.mtime == mtime).map(|e| e.sha1.clone());
        let sha1 = match known {
            Some(h) => {
                progress(1.0);
                h
            }
            None => sha1_file(&bin, |d, t| progress(d as f32 / t.max(1) as f32))?,
        };
        let verified = verified_dump(&sha1).map(|d| d.serial == ident.serial).unwrap_or(false);
        let game = ident.game;
        self.add(GameEntry { disc_path, bin, ident, sha1, size, mtime, verified }, choose);
        Ok(game)
    }

    pub fn import_bios(&mut self, path: &Path) -> Result<BiosInfo> {
        let data = std::fs::read(path)?;
        let info = identify_bios(&data).ok_or_else(|| anyhow!("{} is not a PlayStation BIOS image", path.display()))?;
        self.bios.retain(|b| b.info.sha1 != info.sha1);
        self.bios.push(BiosEntry { path: path.to_path_buf(), info: info.clone() });
        Ok(info)
    }

    /// The BIOS a game of `region` starts with under `mode`.
    pub fn bios_choice(&self, region: Region, mode: crate::config::BiosMode) -> BiosChoice<'_> {
        match (mode, self.bios_for(region)) {
            (crate::config::BiosMode::Auto, Some(b)) => BiosChoice::Console(b),
            _ => BiosChoice::BuiltIn,
        }
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
        // Each disc image once, however many .cue files point at it: the
        // .cue named like the image is preferred, then any .cue, then the
        // bare .bin.
        let mut discs: Vec<(PathBuf, PathBuf)> = Vec::new(); // (what to open, image file)
        for p in &candidates {
            let len = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
            if !(has_ext(p, "cue") || (has_ext(p, "bin") && len > 50 << 20)) {
                continue;
            }
            // A .cue that leads to nothing (its .bin gone, none named like it)
            // is ignored; a .bin is opened through its .cue only if that works.
            let Some((open, bin)) = open_candidates(p).into_iter().find_map(|o| image_file(&o).map(|b| (o, b))) else {
                log::info!("ignoring {}: no disc image found through it", p.display());
                continue;
            };
            let named_like = |o: &Path| has_ext(o, "cue") && o.file_stem() == bin.file_stem();
            match discs.iter_mut().find(|(_, b)| same_file(b, &bin)) {
                Some(d) => {
                    let better = (named_like(&open), has_ext(&open, "cue")) > (named_like(&d.0), has_ext(&d.0, "cue"));
                    if better {
                        d.0 = open;
                    }
                }
                None => discs.push((open, bin)),
            }
        }
        // Games that had no disc get the best one found (not simply the
        // first the folder listing gave).
        let had_none: Vec<GameId> = GameId::ALL.into_iter().filter(|g| !self.games.contains_key(g)).collect();
        let mut games = Vec::new();
        for (p, bin) in discs {
            let name = bin.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let mut cb = |f: f32| progress(&name, f);
            match self.import(&p, false, &mut cb) {
                Ok(g) => {
                    let e = self.discs.iter().find(|e| e.is_image(&bin)).expect("just imported");
                    report.games.push((g, format!("{}  ·  {name}", e.ident.title), e.verified));
                    games.push(g);
                }
                Err(e) => report.skipped.push((p, e.to_string())),
            }
        }
        for g in had_none {
            self.games.remove(&g);
        }
        for g in games {
            self.choose_if_unplayable(g);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let d = std::env::temp_dir().join(format!("dwrc-lib-{name}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn fake_bin(p: &Path) {
        std::fs::write(p, vec![0u8; 2352]).unwrap();
    }

    fn cue_for(bin_name: &str) -> String {
        format!("FILE \"{bin_name}\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n")
    }

    fn entry(game: GameId, serial: &str, region: Region, bin: &Path, verified: bool) -> GameEntry {
        let (size, mtime) = file_stamp(bin).unwrap();
        let ident = Identification {
            game,
            serial: serial.into(),
            title: format!("Title {serial}"),
            region,
            volume_id: String::new(),
            boot_file: String::new(),
            exe_sha1: String::new(),
            exe_load_addr: 0,
            exe_entry: 0,
        };
        GameEntry { disc_path: bin.to_path_buf(), bin: bin.to_path_buf(), ident, sha1: String::new(), size, mtime, verified }
    }

    #[test]
    fn cue_and_bin_resolution() {
        let d = tmp("resolve");
        // Two copies of one disc, each with a cue named like it, both cues
        // naming the first copy.
        fake_bin(&d.join("Game (Europe) (En,Fr).bin"));
        fake_bin(&d.join("Game (Europe).bin"));
        std::fs::write(d.join("Game (Europe) (En,Fr).cue"), cue_for("Game (Europe) (En,Fr).bin")).unwrap();
        std::fs::write(d.join("Game (Europe).cue"), cue_for("Game (Europe) (En,Fr).bin")).unwrap();
        // A cue whose .bin was renamed to the cue's own name.
        fake_bin(&d.join("Game (Japan).bin"));
        std::fs::write(d.join("Game (Japan).cue"), cue_for("Game (Japan) (Rev 1).bin")).unwrap();

        assert_eq!(resolve_disc_path(&d.join("Game (Europe) (En,Fr).bin")), d.join("Game (Europe) (En,Fr).cue"));
        // The second copy is not opened through a cue that names the first.
        assert_eq!(resolve_disc_path(&d.join("Game (Europe).bin")), d.join("Game (Europe).bin"));
        assert_eq!(resolve_disc_path(&d.join("Game (Europe).cue")), d.join("Game (Europe).cue"));
        assert_eq!(resolve_disc_path(&d.join("Game (Japan).cue")), d.join("Game (Japan).bin"));
        assert_eq!(resolve_disc_path(&d.join("Game (Japan).bin")), d.join("Game (Japan).bin"));
        let img = image_file(&resolve_disc_path(&d.join("Game (Japan).cue"))).unwrap();
        assert!(same_file(&img, &d.join("Game (Japan).bin")));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn cues_that_lead_nowhere_or_do_not_fit() {
        let d = tmp("orphans");
        // A .cue whose .bin is gone, with nothing named like it: ignored.
        std::fs::write(d.join("Gone.cue"), cue_for("Gone (Rev 1).bin")).unwrap();
        // A .bin whose .cue can't be read: the .bin is used on its own.
        fake_bin(&d.join("Game.bin"));
        std::fs::write(d.join("Game.cue"), "FILE \"Game.bin\" BINARY
  TRACK 01 NONSENSE
").unwrap();
        assert_eq!(resolve_disc_path(&d.join("Game.bin")), d.join("Game.bin"));
        assert!(open_candidates(&d.join("Gone.cue")).iter().all(|c| image_file(c).is_none()));
        let mut lib = Library::default();
        // Neither is a game (the fake image has no filesystem), and the
        // orphaned .cue is not even mentioned.
        let report = lib.scan_folder(&d, &mut |_, _| {});
        assert!(report.games.is_empty());
        assert!(report.skipped.iter().all(|(p, _)| p.file_name().unwrap() != "Gone.cue"), "{:?}", report.skipped);
        // Picked by hand, a .cue that leads nowhere still says why.
        assert!(lib.import_disc(&d.join("Gone.cue"), &mut |_| {}).is_err());

        // An imported disc whose .cue breaks or goes away still opens, and
        // still counts as ready.
        std::fs::write(d.join("Game.cue"), cue_for("Game.bin")).unwrap();
        let mut e = entry(GameId::Dw1, "SLUS-01032", Region::NtscU, &d.join("Game.bin"), true);
        e.disc_path = d.join("Game.cue");
        assert!(open_disc(&e).is_ok());
        std::fs::write(d.join("Game.cue"), "FILE \"Elsewhere.bin\" BINARY
  TRACK 01 MODE2/2352
    INDEX 01 00:00:00
").unwrap();
        assert!(open_disc(&e).is_ok());
        assert_eq!(disc_status(&e), DiscStatus::Ready);
        std::fs::remove_file(d.join("Game.cue")).unwrap();
        assert!(open_disc(&e).is_ok());
        assert_eq!(disc_status(&e), DiscStatus::Ready);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn several_discs_per_game() {
        let d = tmp("choose");
        let (us, eu, eu2) = (d.join("us.bin"), d.join("eu.bin"), d.join("eu2.bin"));
        for p in [&us, &eu, &eu2] {
            fake_bin(p);
        }
        let g = GameId::Dw3;
        let mut lib = Library::default();
        lib.add(entry(g, "SLES-03936", Region::Pal, &eu, true), false);
        assert_eq!(lib.games[&g].bin, eu, "a game's first disc is the one it plays");
        lib.add(entry(g, "SLES-03936", Region::Pal, &eu2, false), false);
        lib.add(entry(g, "SLUS-01436", Region::NtscU, &us, false), false);
        assert_eq!(lib.games[&g].bin, eu, "adding discs keeps the choice");
        let order: Vec<PathBuf> = lib.discs_for(g).iter().map(|e| e.bin.clone()).collect();
        assert_eq!(order, vec![us.clone(), eu.clone(), eu2.clone()]);
        // The same image again is refreshed, not listed twice.
        lib.add(entry(g, "SLES-03936", Region::Pal, &eu, true), false);
        assert_eq!(lib.discs.len(), 3);
        assert!(lib.select(g, &eu2));
        assert_eq!(lib.games[&g].bin, eu2);
        assert!(!lib.select(GameId::Dw1, &eu), "not a disc of that game");
        // Forgetting the chosen disc falls back to a playable one, verified first.
        lib.forget(g, &eu2);
        assert_eq!(lib.games[&g].bin, eu);
        assert_eq!(lib.discs.len(), 2);
        // A chosen disc that went missing gives way to one that is there.
        std::fs::remove_file(&eu).unwrap();
        assert_eq!(lib.status(g), DiscStatus::Missing);
        lib.choose_if_unplayable(g);
        assert_eq!(lib.games[&g].bin, us);
        // Importing a disc by hand makes it the one that plays.
        fake_bin(&eu2);
        lib.add(entry(g, "SLES-03936", Region::Pal, &eu2, false), true);
        assert_eq!(lib.games[&g].bin, eu2);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn older_library_files_still_load() {
        let d = tmp("old");
        let bin = d.join("dw1.bin");
        fake_bin(&bin);
        // Written before discs had a `bin` and the library a `discs` list.
        let mut e = serde_json::to_value(entry(GameId::Dw1, "SLUS-01032", Region::NtscU, &bin, true)).unwrap();
        e.as_object_mut().unwrap().remove("bin");
        let json = serde_json::json!({ "games": { "Dw1": e }, "bios": [] });
        let mut lib: Library = serde_json::from_value(json).unwrap();
        lib.normalize();
        assert_eq!(lib.discs.len(), 1);
        assert!(same_file(&lib.games[&GameId::Dw1].bin, &bin));
        assert_eq!(lib.status(GameId::Dw1), DiscStatus::Ready);
        let _ = std::fs::remove_dir_all(&d);
    }
}
