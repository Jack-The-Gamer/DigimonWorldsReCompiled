//! The Re:Compiled mod platform.
//!
//! A mod is a folder with a `mod.toml` manifest. Mods can:
//!
//! * **replace files on the disc** – `files/<ISO path>` for games with a
//!   real filesystem (Digimon World), or raw sector ranges (`[[lba]]`) for
//!   games that address data by LBA (Digimon World 2 / 2003). The player's
//!   image is never modified; replacements are served by a virtual disc.
//! * **patch memory** – `[[patch]]` entries, optionally conditional on the
//!   bytes at an address (to target a specific overlay).
//! * **run Lua scripts** with per-frame callbacks and execution hooks at
//!   any code address, full CPU register and memory access, per-mod
//!   options and per-mod save data.
//!
//! See `docs/modding.md` for the full guide.

pub mod disc;
pub mod manifest;
pub mod patches;
pub mod script;

pub use manifest::{ModManifest, OptionDef, OptionKind};

use dwrc_disc::GameId;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A discovered mod.
#[derive(Debug, Clone)]
pub struct ModInfo {
    pub root: PathBuf,
    pub manifest: ModManifest,
    pub problems: Vec<String>,
}

impl ModInfo {
    pub fn id(&self) -> &str {
        &self.manifest.info.id
    }
    pub fn is_usable(&self) -> bool {
        self.problems.is_empty()
    }
}

/// Option value as seen by scripts.
pub type OptionValues = BTreeMap<String, toml::Value>;

/// A mod enabled for a session, with its resolved option values.
#[derive(Debug, Clone)]
pub struct EnabledMod {
    pub info: ModInfo,
    pub options: OptionValues,
    pub data_dir: PathBuf,
}

/// Scans `roots` for mods targeting `game`. Mods live in
/// `<root>/<game-slug>/<mod-folder>/mod.toml` or `<root>/<mod-folder>/mod.toml`.
pub fn discover(roots: &[PathBuf], game: GameId) -> Vec<ModInfo> {
    let mut out: Vec<ModInfo> = Vec::new();
    for root in roots {
        for dir in [root.join(game.slug()), root.clone()] {
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).filter(|p| p.join("mod.toml").is_file()).collect();
            entries.sort();
            for p in entries {
                match ModManifest::load(&p.join("mod.toml")) {
                    Ok(m) => {
                        if m.info.game != game {
                            continue;
                        }
                        if out.iter().any(|o| o.manifest.info.id == m.info.id) {
                            continue; // first root wins (user mods override bundled)
                        }
                        let problems = m.validate(&p);
                        out.push(ModInfo { root: p, manifest: m, problems });
                    }
                    Err(e) => {
                        log::warn!("{}: {e}", p.display());
                    }
                }
            }
        }
    }
    out
}

/// Resolves option values for a mod: saved values over manifest defaults.
pub fn resolve_options(info: &ModInfo, saved: Option<&OptionValues>) -> OptionValues {
    let mut v = OptionValues::new();
    for o in &info.manifest.options {
        let val = saved.and_then(|s| s.get(&o.id)).cloned().unwrap_or_else(|| o.default.clone());
        v.insert(o.id.clone(), val);
    }
    v
}

/// Everything a session needs from the enabled mods.
pub struct ModRuntime {
    pub patches: Vec<patches::MemoryPatch>,
    pub scripts: Vec<script::ScriptHost>,
    pub log: Vec<String>,
}

impl ModRuntime {
    /// Applies disc overrides and loads patches/scripts for `mods` (in
    /// load order: later mods win on conflicts).
    pub fn build(mods: &[EnabledMod], disc_ctx: &mut disc::DiscContext) -> Self {
        let mut rt = ModRuntime { patches: Vec::new(), scripts: Vec::new(), log: Vec::new() };
        for m in mods {
            let id = m.info.id().to_string();
            match disc_ctx.apply_mod(&m.info) {
                Ok(n) if n > 0 => rt.log.push(format!("[{id}] replaced {n} disc file(s)/range(s)")),
                Ok(_) => {}
                Err(e) => rt.log.push(format!("[{id}] disc overrides failed: {e}")),
            }
            for p in &m.info.manifest.patches {
                match patches::MemoryPatch::from_def(p) {
                    Ok(mp) => rt.patches.push(mp),
                    Err(e) => rt.log.push(format!("[{id}] bad patch: {e}")),
                }
            }
            if let Some(s) = &m.info.manifest.info.script {
                let path = m.info.root.join(s);
                match script::ScriptHost::load(&id, &path, m.options.clone(), m.data_dir.clone()) {
                    Ok(h) => {
                        rt.log.push(format!("[{id}] script loaded"));
                        rt.scripts.push(h);
                    }
                    Err(e) => rt.log.push(format!("[{id}] script error: {e}")),
                }
            }
        }
        rt
    }

    /// Registers execution hooks with the console.
    pub fn install(&mut self, psx: &mut dwrc_psx::Psx) {
        for s in &mut self.scripts {
            s.install_hooks(&mut psx.bus);
        }
    }

    /// Called once per video frame.
    pub fn on_frame(&mut self, psx: &mut dwrc_psx::Psx, frame: u64) {
        for p in &mut self.patches {
            p.apply(&mut psx.bus);
        }
        for s in &mut self.scripts {
            if let Err(e) = s.on_frame(&mut psx.cpu, &mut psx.bus, frame) {
                let msg = format!("[{}] {e}", s.mod_id());
                log::warn!("{msg}");
                if self.log.last() != Some(&msg) {
                    self.log.push(msg);
                }
            }
        }
    }

    pub fn save_all(&mut self) {
        for s in &mut self.scripts {
            s.save_data();
        }
    }
}

pub fn mod_dir_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}
