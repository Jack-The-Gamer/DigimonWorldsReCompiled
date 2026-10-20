//! Where Re:Compiled keeps its data.
//!
//! By default everything lives in the OS per-user data folder. Placing a
//! file named `portable.txt` next to the executable switches to portable
//! mode: data is kept in a `data` folder beside the executable instead.

use dwrc_disc::GameId;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Paths {
    pub root: PathBuf,
    pub exe_dir: PathBuf,
    pub portable: bool,
}

impl Paths {
    pub fn detect() -> Self {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."));
        if exe_dir.join("portable.txt").exists() {
            return Self { root: exe_dir.join("data"), exe_dir, portable: true };
        }
        let root = directories::ProjectDirs::from("", "", "DigimonWorldsReCompiled")
            .map(|d| d.data_dir().to_path_buf())
            .unwrap_or_else(|| exe_dir.join("data"));
        Self { root, exe_dir, portable: false }
    }

    /// Uses an explicit root (tests, command-line override).
    pub fn at(root: impl Into<PathBuf>) -> Self {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."));
        Self { root: root.into(), exe_dir, portable: true }
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        std::fs::create_dir_all(self.user_mods_root())?;
        std::fs::create_dir_all(self.screenshots_dir())
    }

    pub fn library_file(&self) -> PathBuf {
        self.root.join("library.json")
    }

    pub fn config_file(&self) -> PathBuf {
        self.root.join("settings.toml")
    }

    pub fn saves_dir(&self, g: GameId) -> PathBuf {
        self.root.join("saves").join(g.slug())
    }

    /// Generated cache (analysis results, thumbnails) for a game.
    pub fn cache_dir(&self, g: GameId) -> PathBuf {
        self.root.join("cache").join(g.slug())
    }

    /// Save states of one release (by serial).
    pub fn states_dir(&self, serial: &str) -> PathBuf {
        self.root.join("states").join(serial)
    }

    pub fn screenshots_dir(&self) -> PathBuf {
        self.root.join("screenshots")
    }

    /// Mods installed by the player.
    pub fn user_mods_root(&self) -> PathBuf {
        self.root.join("mods")
    }

    /// Mods shipped with the application (next to the executable), and the
    /// source tree's `mods/` folder when running from a checkout.
    pub fn bundled_mods_roots(&self) -> Vec<PathBuf> {
        let mut v = vec![self.exe_dir.join("mods")];
        // target/{debug,release}/ -> repository root
        if let Some(repo) = self.exe_dir.parent().and_then(Path::parent) {
            v.push(repo.join("mods"));
        }
        v
    }

    /// Where mods are looked for, in priority order.
    pub fn mod_roots(&self) -> Vec<(PathBuf, dwrc_mods::ModSource)> {
        let mut v = vec![(self.user_mods_root(), dwrc_mods::ModSource::User)];
        for r in self.bundled_mods_roots() {
            if r != self.user_mods_root() && !v.iter().any(|(p, _)| *p == r) {
                v.push((r, dwrc_mods::ModSource::Bundled));
            }
        }
        v
    }

    /// Cached mod indexes, GitHub release lists and thumbnails.
    pub fn modhub_cache_dir(&self) -> PathBuf {
        self.root.join("cache").join("modhub")
    }

    /// Mod downloads in progress.
    pub fn downloads_dir(&self) -> PathBuf {
        self.root.join("cache").join("downloads")
    }

    pub fn mod_data_dir(&self, g: GameId, mod_id: &str) -> PathBuf {
        self.root.join("mod-data").join(g.slug()).join(mod_id)
    }
}
