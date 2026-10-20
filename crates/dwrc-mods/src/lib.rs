//! The Re:Compiled mod platform.
//!
//! A mod is a folder with a `mod.toml` manifest, installed once into
//! `mods/<id>/` and declaring which games it runs on. Mods can:
//!
//! * **replace files on the disc** – `files/<ISO path>` for games with a
//!   real filesystem (Digimon World), or raw sector ranges (`[[lba]]`) for
//!   games that address data by LBA (Digimon World 2 / 2003). The player's
//!   image is never modified; replacements are served by a virtual disc.
//! * **patch memory** – `[[patch]]` entries, optionally conditional on the
//!   bytes at an address (to target a specific overlay).
//! * **run Lua scripts** (sandboxed: no file, OS or network access) with
//!   per-frame callbacks and execution hooks at any code address, full CPU
//!   register and memory access, per-mod options and per-mod save data.
//!
//! Mods are installed from `.zip` files ([`package`]), and can name a GitHub
//! repository whose Releases carry updates. See `docs/modding.md`.

pub mod disc;
pub mod manifest;
pub mod package;
pub mod patches;
pub mod script;
pub mod version;

pub use manifest::{parse_github_repo, valid_id, GameSel, ModManifest, ModRef, OptionDef, OptionKind, CATEGORIES};

use dwrc_disc::GameId;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Where a discovered mod lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModSource {
    /// Installed by the player (their data folder): can be updated and removed.
    User,
    /// Shipped with the program.
    Bundled,
}

/// A discovered mod.
#[derive(Debug, Clone)]
pub struct ModInfo {
    pub root: PathBuf,
    pub manifest: ModManifest,
    pub problems: Vec<String>,
    pub source: ModSource,
}

impl ModInfo {
    pub fn id(&self) -> &str {
        &self.manifest.info.id
    }
    pub fn name(&self) -> &str {
        &self.manifest.info.name
    }
    pub fn version(&self) -> &str {
        &self.manifest.info.version
    }
    pub fn is_usable(&self) -> bool {
        self.problems.is_empty()
    }
    pub fn supports(&self, g: GameId) -> bool {
        self.manifest.info.supports(g)
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

/// Folders holding one mod each, directly under `root`, plus the older
/// per-game layout (`root/dw1/<mod>`, `root/any/<mod>`) which is still read.
fn mod_folders(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut scan = |dir: &Path| {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut v: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')))
            .filter(|p| p.join("mod.toml").is_file())
            .collect();
        v.sort();
        out.extend(v);
    };
    scan(root);
    for sub in ["dw1", "dw2", "dw3", "any"] {
        let d = root.join(sub);
        if !d.join("mod.toml").is_file() {
            scan(&d);
        }
    }
    out
}

/// Scans the mod roots in priority order (the player's folder first, then
/// the bundled mods). The first copy of an id wins, so installing a mod
/// with the same id as a bundled one overrides it.
pub fn discover_all(roots: &[(PathBuf, ModSource)]) -> Vec<ModInfo> {
    let mut out: Vec<ModInfo> = Vec::new();
    for (root, source) in roots {
        for p in mod_folders(root) {
            match ModManifest::load(&p.join("mod.toml")) {
                Ok(m) => {
                    if out.iter().any(|o| o.manifest.info.id == m.info.id) {
                        continue;
                    }
                    let problems = m.validate(&p);
                    out.push(ModInfo { root: p, manifest: m, problems, source: *source });
                }
                Err(e) => log::warn!("{}: {e:#}", p.display()),
            }
        }
    }
    out
}

/// Folders whose `mod.toml` could not be read, with the reason (so the
/// manager can show broken installs instead of hiding them).
pub fn broken_mods(root: &Path) -> Vec<(PathBuf, String)> {
    mod_folders(root)
        .into_iter()
        .filter_map(|p| ModManifest::load(&p.join("mod.toml")).err().map(|e| (p, format!("{e:#}"))))
        .collect()
}

/// Mods that run on `game`.
pub fn discover(roots: &[(PathBuf, ModSource)], game: GameId) -> Vec<ModInfo> {
    discover_all(roots).into_iter().filter(|m| m.supports(game)).collect()
}

/// Why an enabled mod will not load for a game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skip {
    Broken,
    WrongGame,
    WrongRelease,
    Engine(String),
    Needs(String),
    Conflicts(String),
}

impl std::fmt::Display for Skip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Skip::Broken => f.write_str("has errors"),
            Skip::WrongGame => f.write_str("not made for this game"),
            Skip::WrongRelease => f.write_str("made for a different release of this game"),
            Skip::Engine(r) => write!(f, "needs Re:Compiled {r} (this is {})", version::RECOMPILED_VERSION),
            Skip::Needs(d) => write!(f, "needs {d}, which is not installed and switched on"),
            Skip::Conflicts(o) => write!(f, "cannot be combined with {o}"),
        }
    }
}

/// Works out which of the switched-on mods load for a game, and in what
/// order. `wanted` is the player's switched-on mods in load order. Mods
/// whose dependencies are missing, or that conflict with a mod loading
/// earlier, are skipped (with the reason); a mod is moved after any
/// dependency that sits later in the order.
pub fn plan<'a>(wanted: &[&'a ModInfo], game: GameId, serial: Option<&str>) -> (Vec<&'a ModInfo>, Vec<(&'a ModInfo, Skip)>) {
    let mut skipped: Vec<(&ModInfo, Skip)> = Vec::new();
    let mut set: Vec<&ModInfo> = Vec::new();
    for m in wanted {
        let i = &m.manifest.info;
        let skip = if !m.is_usable() {
            Some(Skip::Broken)
        } else if !i.supports(game) {
            Some(Skip::WrongGame)
        } else if serial.is_some_and(|s| !i.serials.is_empty() && !i.serials.iter().any(|x| x == s)) {
            Some(Skip::WrongRelease)
        } else {
            i.engine.as_ref().filter(|r| !version::satisfies(version::RECOMPILED_VERSION, r)).map(|r| Skip::Engine(r.clone()))
        };
        match skip {
            Some(s) => skipped.push((m, s)),
            None => set.push(m),
        }
    }
    // Drop mods with missing dependencies or conflicts until stable.
    loop {
        let mut drop: Option<(usize, Skip)> = None;
        'outer: for (k, m) in set.iter().enumerate() {
            for d in m.manifest.info.dependencies.iter().filter_map(|d| ModRef::parse(d)) {
                if !set.iter().any(|o| o.id() == d.id && d.accepts(o.version())) {
                    drop = Some((k, Skip::Needs(d.to_string())));
                    break 'outer;
                }
            }
            for c in m.manifest.info.conflicts.iter().filter_map(|c| ModRef::parse(c)) {
                // The mod earlier in the load order wins.
                if let Some(o) = set[..k].iter().find(|o| o.id() == c.id && c.accepts(o.version())) {
                    drop = Some((k, Skip::Conflicts(o.name().to_string())));
                    break 'outer;
                }
                if let Some(o) = set[k + 1..].iter().find(|o| o.id() == c.id && c.accepts(o.version())) {
                    let j = set.iter().position(|x| x.id() == o.id()).unwrap();
                    drop = Some((j, Skip::Conflicts(m.name().to_string())));
                    break 'outer;
                }
            }
        }
        match drop {
            Some((k, why)) => skipped.push((set.remove(k), why)),
            None => break,
        }
    }
    // Load dependencies first (stable otherwise).
    let mut ordered: Vec<&ModInfo> = Vec::new();
    let mut pending = set;
    while !pending.is_empty() {
        let before = pending.len();
        let mut k = 0;
        while k < pending.len() {
            let ready = pending[k]
                .manifest
                .info
                .dependencies
                .iter()
                .filter_map(|d| ModRef::parse(d))
                .all(|d| ordered.iter().any(|o| o.id() == d.id) || !pending.iter().any(|p| p.id() == d.id));
            if ready {
                ordered.push(pending.remove(k));
                // Restart so the original order is kept as far as possible.
                k = 0;
            } else {
                k += 1;
            }
        }
        if pending.len() == before {
            // A dependency cycle: keep the player's order for the rest.
            ordered.append(&mut pending);
        }
    }
    (ordered, skipped)
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
    pub fn build(game: GameId, serial: &str, mods: &[EnabledMod], disc_ctx: &mut disc::DiscContext) -> Self {
        let mut rt = ModRuntime { patches: Vec::new(), scripts: Vec::new(), log: Vec::new() };
        for m in mods.iter() {
            let id = m.info.id().to_string();
            match disc_ctx.apply_mod(&m.info) {
                Ok(n) if n > 0 => rt.log.push(format!("[{id}] replaced {n} disc file(s)/range(s)")),
                Ok(_) => {}
                Err(e) => rt.log.push(format!("[{id}] disc overrides failed: {e:#}")),
            }
            for p in &m.info.manifest.patches {
                match patches::MemoryPatch::from_def(p) {
                    Ok(mp) => rt.patches.push(mp),
                    Err(e) => rt.log.push(format!("[{id}] bad patch: {e}")),
                }
            }
            if let Some(s) = &m.info.manifest.info.script {
                let path = m.info.root.join(s);
                match script::ScriptHost::load(&id, game, serial, &path, m.options.clone(), m.data_dir.clone()) {
                    Ok(h) => {
                        rt.log.push(format!("[{id}] script loaded"));
                        rt.scripts.push(h);
                    }
                    Err(e) => rt.log.push(format!("[{id}] script error: {e}")),
                }
            }
        }
        match disc_ctx.finish() {
            Ok(n) if n > 0 => rt.log.push(format!("patched {n} file-table entr(y/ies) in the game executable")),
            Ok(_) => {}
            Err(e) => rt.log.push(format!("file-table patching failed: {e:#}")),
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

    /// The console speed the scripts ask for (the highest) and the sound
    /// chip voices they want kept at real time while sped up.
    pub fn speed(&self) -> (u32, u32) {
        self.scripts.iter().map(|s| s.speed()).fold((1, 0), |(sp, m), (s, v)| (sp.max(s), m | v))
    }

    /// Hands the scripts' speed requests to the console's sound chip.
    pub fn apply_speed(&self, psx: &mut dwrc_psx::Psx) {
        let (speed, voices) = self.speed();
        psx.bus.spu.set_speed(speed);
        psx.bus.spu.realtime_voices = voices;
    }

    /// Hands a key press on the keyboard to the scripts.
    pub fn on_key(&mut self, psx: &mut dwrc_psx::Psx, key: &str, shift: bool, ctrl: bool, alt: bool) {
        let frame = psx.frame_count;
        for s in &mut self.scripts {
            if let Err(e) = s.on_key(&mut psx.cpu, &mut psx.bus, frame, key, shift, ctrl, alt) {
                let msg = format!("[{}] on_key: {e}", s.mod_id());
                log::warn!("{msg}");
                self.log.push(msg);
            }
        }
    }

    /// Save state requests from the scripts since the last call (mod id,
    /// request), in load order.
    pub fn take_state_requests(&mut self) -> Vec<(String, script::StateRequest)> {
        self.scripts.iter_mut().flat_map(|s| s.take_requests().into_iter().map(|r| (s.mod_id().to_string(), r)).collect::<Vec<_>>()).collect()
    }

    /// On-screen messages from the scripts since the last call.
    pub fn take_notices(&mut self) -> Vec<String> {
        self.scripts.iter_mut().flat_map(|s| s.take_notices()).collect()
    }

    /// Lets scripts transform the controller state before each frame.
    pub fn filter_input(&mut self, buttons: u16) -> u16 {
        let mut b = buttons;
        for s in &mut self.scripts {
            match s.filter_input(b) {
                Ok(v) => b = v,
                Err(e) => log::warn!("[{}] on_input: {e}", s.mod_id()),
            }
        }
        b
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

#[cfg(test)]
mod tests {
    use super::*;

    fn m(id: &str, extra: &str) -> ModInfo {
        let text = format!("[mod]\nid = \"{id}\"\nname = \"{id}\"\nversion = \"1.0.0\"\ngames = [\"dw1\", \"dw2\"]\n{extra}");
        ModInfo { root: PathBuf::new(), manifest: ModManifest::parse(&text).unwrap(), problems: vec![], source: ModSource::User }
    }

    #[test]
    fn plan_orders_and_skips() {
        let lib = m("lib", "");
        let app = m("app", "dependencies = [\"lib@>=1\"]");
        let needs_new = m("needs-new", "dependencies = [\"lib@>=2\"]");
        let rival = m("rival", "conflicts = [\"app\"]");
        let future = m("future", "engine = \">=99\"");
        let wanted = [&app, &lib, &needs_new, &rival, &future];
        let (load, skipped) = plan(&wanted, GameId::Dw1, None);
        let ids: Vec<&str> = load.iter().map(|x| x.id()).collect();
        assert_eq!(ids, vec!["lib", "app"], "dependency moved first, failures dropped");
        let why: Vec<(&str, String)> = skipped.iter().map(|(x, s)| (x.id(), s.to_string())).collect();
        assert!(why.contains(&("future", format!("needs Re:Compiled >=99 (this is {})", version::RECOMPILED_VERSION))));
        assert!(why.iter().any(|(i, s)| *i == "needs-new" && s.contains("lib >=2")));
        assert!(why.iter().any(|(i, s)| *i == "rival" && s.contains("app")));
        // Not for this game.
        let (load, skipped) = plan(&[&lib], GameId::Dw3, None);
        assert!(load.is_empty() && skipped[0].1 == Skip::WrongGame);
        // Dependency missing entirely.
        let (load, _) = plan(&[&app], GameId::Dw2, None);
        assert!(load.is_empty());
    }

    #[test]
    fn manifest_game_forms() {
        let one = m("a", "");
        assert_eq!(one.manifest.info.games(), vec![GameId::Dw1, GameId::Dw2]);
        let any = ModManifest::parse("[mod]\nid = \"x\"\nname = \"x\"\ngame = \"any\"").unwrap();
        assert_eq!(any.info.games().len(), 3);
        assert!(ModManifest::parse("[mod]\nid = \"x\"\nname = \"x\"\ngame = \"dw9\"").is_err());
        assert_eq!(parse_github_repo("https://github.com/Some-One/my.mod.git/").as_deref(), Some("Some-One/my.mod"));
        assert_eq!(parse_github_repo("owner/repo").as_deref(), Some("owner/repo"));
        assert!(parse_github_repo("https://gitlab.com/a/b").is_none() && parse_github_repo("a/b/c").is_none());
        // The manifest serialises back with the same games.
        let back: ModManifest = toml::from_str(&toml::to_string(&one.manifest).unwrap()).unwrap();
        assert_eq!(back.info.games(), one.manifest.info.games());
    }
}
