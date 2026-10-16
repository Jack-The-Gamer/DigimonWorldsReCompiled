//! Player settings (`settings.toml`).

use crate::paths::Paths;
use serde::{Deserialize, Serialize};
use dwrc_disc::GameId;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Filter {
    /// Sharp pixels, as the console output them.
    #[default]
    Nearest,
    /// Smoothed scaling.
    Linear,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Aspect {
    /// 4:3, like a CRT television.
    #[default]
    Tv4x3,
    /// Largest integer multiple of the native resolution.
    IntegerScale,
    /// Fill the window.
    Stretch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum EngineKind {
    /// Statically recompiled native code (falls back to the interpreter
    /// for anything it cannot translate).
    #[default]
    Recompiled,
    /// The reference interpreter.
    Interpreter,
    /// Recompiled code checked against the interpreter block by block
    /// (slow; for development).
    Validate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VideoConfig {
    pub filter: Filter,
    pub aspect: Aspect,
    pub show_fps: bool,
    pub start_fullscreen: bool,
}

impl Default for VideoConfig {
    fn default() -> Self {
        Self { filter: Filter::Nearest, aspect: Aspect::Tv4x3, show_fps: false, start_fullscreen: false }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioConfig {
    pub volume: f32,
    pub muted: bool,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self { volume: 0.8, muted: false }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EmulationConfig {
    /// Skip the BIOS boot animation (the kernel still initialises normally).
    pub fast_boot: bool,
    pub engine: EngineKind,
    /// Insert a second memory card in slot 2.
    pub memory_card_2: bool,
}

impl Default for EmulationConfig {
    fn default() -> Self {
        Self { fast_boot: true, engine: EngineKind::Recompiled, memory_card_2: true }
    }
}

/// Keyboard bindings: PlayStation button name -> key name (egui key names).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct InputConfig {
    pub keyboard: BTreeMap<String, String>,
}

impl Default for InputConfig {
    fn default() -> Self {
        let pairs = [
            ("Up", "ArrowUp"),
            ("Down", "ArrowDown"),
            ("Left", "ArrowLeft"),
            ("Right", "ArrowRight"),
            ("Cross", "X"),
            ("Circle", "C"),
            ("Square", "Z"),
            ("Triangle", "S"),
            ("Start", "Enter"),
            ("Select", "Backspace"),
            ("L1", "Q"),
            ("R1", "E"),
            ("L2", "A"),
            ("R2", "D"),
        ];
        Self { keyboard: pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect() }
    }
}

/// Mod setup, modelled on Gen1Recomp: one load order shared by every game,
/// a per-game on/off answer for each mod, per-mod option values, and the
/// mod indexes the player added.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ModsConfig {
    /// Load order shared by every game: first loads first, later mods win
    /// where two change the same thing. Mods missing from the list load
    /// after the listed ones, sorted by id.
    pub order: Vec<String>,
    /// Per game (`dw1`, `dw2`, `dw3`): the mods switched on for it.
    pub enabled: BTreeMap<String, Vec<String>>,
    /// Per mod id: option values chosen in the manager.
    pub options: BTreeMap<String, BTreeMap<String, toml::Value>>,
    /// Mod indexes the player added (`owner/repo`, a GitHub URL or a feed
    /// URL). The built-in index is always present and is not stored here.
    pub indexes: Vec<String>,
    /// Check installed mods for updates when the Mods page opens.
    pub check_updates: bool,
    /// Start games with no mods at all (troubleshooting), without losing
    /// which mods are switched on.
    pub safe_mode: bool,
}

impl Default for ModsConfig {
    fn default() -> Self {
        Self {
            order: Vec::new(),
            enabled: BTreeMap::new(),
            options: BTreeMap::new(),
            indexes: Vec::new(),
            check_updates: true,
            safe_mode: false,
        }
    }
}

impl ModsConfig {
    pub fn is_enabled(&self, g: GameId, id: &str) -> bool {
        self.enabled.get(g.slug()).is_some_and(|v| v.iter().any(|e| e == id))
    }

    pub fn set_enabled(&mut self, g: GameId, id: &str, on: bool) {
        let list = self.enabled.entry(g.slug().to_string()).or_default();
        list.retain(|e| e != id);
        if on {
            list.push(id.to_string());
        }
        self.ensure_ordered(id);
    }

    /// Makes sure `id` has a place in the load order (appended last).
    pub fn ensure_ordered(&mut self, id: &str) {
        if !self.order.iter().any(|e| e == id) {
            self.order.push(id.to_string());
        }
    }

    /// The full load order for the installed mods: the saved order (minus
    /// mods that are no longer installed), then any others sorted by id.
    pub fn load_order<'a>(&self, installed: impl IntoIterator<Item = &'a str>) -> Vec<String> {
        let mut rest: Vec<&str> = installed.into_iter().collect();
        rest.sort_unstable();
        rest.dedup();
        let mut out: Vec<String> = self.order.iter().filter(|id| rest.contains(&id.as_str())).cloned().collect();
        for id in rest {
            if !out.iter().any(|o| o == id) {
                out.push(id.to_string());
            }
        }
        out
    }

    /// Moves `id` one place up (`delta < 0`) or down among `visible` (the
    /// rows the player can see), swapping it with its visible neighbour.
    pub fn move_within(&mut self, installed: &[&str], visible: &[String], id: &str, delta: i32) -> bool {
        let mut full = self.load_order(installed.iter().copied());
        let Some(vi) = visible.iter().position(|v| v == id) else { return false };
        let ni = vi as i64 + delta.signum() as i64;
        if ni < 0 || ni >= visible.len() as i64 {
            return false;
        }
        let other = &visible[ni as usize];
        let (Some(a), Some(b)) = (full.iter().position(|x| x == id), full.iter().position(|x| x == other)) else {
            return false;
        };
        full.swap(a, b);
        self.order = full;
        true
    }

    /// Moves `id` before the first (`top`) or after the last visible row.
    pub fn move_to_edge(&mut self, installed: &[&str], visible: &[String], id: &str, top: bool) -> bool {
        let mut full = self.load_order(installed.iter().copied());
        let Some(from) = full.iter().position(|x| x == id) else { return false };
        let anchor = if top { visible.iter().find(|v| *v != id) } else { visible.iter().rev().find(|v| *v != id) };
        let Some(anchor) = anchor else { return false };
        full.remove(from);
        let Some(at) = full.iter().position(|x| x == anchor) else { return false };
        full.insert(if top { at } else { at + 1 }, id.to_string());
        let changed = full != self.order;
        self.order = full;
        changed
    }

    /// Records a fresh install: the mod is switched on for every game it
    /// supports (installing a mod is asking to use it) and loads last.
    pub fn note_installed(&mut self, id: &str, games: &[GameId]) {
        for g in games {
            if !self.is_enabled(*g, id) {
                self.set_enabled(*g, id, true);
            }
        }
        self.ensure_ordered(id);
    }

    /// Forgets an uninstalled mod's on/off answers and load-order slot.
    /// Option values are kept, so reinstalling restores them.
    pub fn forget(&mut self, id: &str) {
        for list in self.enabled.values_mut() {
            list.retain(|e| e != id);
        }
        self.order.retain(|e| e != id);
    }

    /// Number of mods switched on for a game (whether or not installed).
    pub fn enabled_count(&self, g: GameId) -> usize {
        self.enabled.get(g.slug()).map_or(0, Vec::len)
    }
}

/// Settings file format version. 2: default R1 moved from W to E; mods
/// switched to a shared load order with per-game answers.
pub const SETTINGS_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub settings_version: u32,
    pub video: VideoConfig,
    pub audio: AudioConfig,
    pub emulation: EmulationConfig,
    pub input: InputConfig,
    pub mods: ModsConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            settings_version: SETTINGS_VERSION,
            video: VideoConfig::default(),
            audio: AudioConfig::default(),
            emulation: EmulationConfig::default(),
            input: InputConfig::default(),
            mods: ModsConfig::default(),
        }
    }
}

impl Config {
    pub fn load(paths: &Paths) -> Self {
        match std::fs::read_to_string(paths.config_file()) {
            Ok(t) => Self::from_toml(&t).unwrap_or_else(|e| {
                log::warn!("settings.toml: {e}");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    /// Parses a settings file, upgrading older formats.
    pub fn from_toml(text: &str) -> anyhow::Result<Self> {
        let mut doc: toml::Table = toml::from_str(text)?;
        let version = doc.get("settings_version").and_then(|v| v.as_integer()).unwrap_or(0) as u32;
        // Version 1 kept mods per game: [mods.dw1] enabled = [...], options = {...}.
        let mut legacy: Vec<(String, toml::Table)> = Vec::new();
        if let Some(toml::Value::Table(mods)) = doc.get_mut("mods") {
            for g in GameId::ALL {
                if let Some(toml::Value::Table(t)) = mods.get(g.slug()) {
                    legacy.push((g.slug().to_string(), t.clone()));
                    mods.remove(g.slug());
                }
            }
        }
        let mut cfg: Config = toml::Value::Table(doc).try_into()?;
        for (slug, t) in legacy {
            let Some(g) = GameId::from_slug(&slug) else { continue };
            if let Some(toml::Value::Array(ids)) = t.get("enabled") {
                for id in ids.iter().filter_map(|v| v.as_str()) {
                    cfg.mods.set_enabled(g, id, true);
                }
            }
            if let Some(toml::Value::Table(opts)) = t.get("options") {
                for (id, vals) in opts {
                    if let toml::Value::Table(vals) = vals {
                        let slot = cfg.mods.options.entry(id.clone()).or_default();
                        for (k, v) in vals {
                            slot.entry(k.clone()).or_insert_with(|| v.clone());
                        }
                    }
                }
            }
        }
        if version < 2 && cfg.input.keyboard.get("R1").map(String::as_str) == Some("W") {
            // W was the default for R1 before version 2.
            cfg.input.keyboard.insert("R1".into(), "E".into());
        }
        cfg.settings_version = SETTINGS_VERSION;
        Ok(cfg)
    }

    pub fn save(&self, paths: &Paths) -> anyhow::Result<()> {
        paths.ensure()?;
        let tmp = paths.config_file().with_extension("toml.tmp");
        std::fs::write(&tmp, toml::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, paths.config_file())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_r1_is_e() {
        assert_eq!(Config::default().input.keyboard.get("R1").map(String::as_str), Some("E"));
    }

    #[test]
    fn migrates_version_1_settings() {
        let old = r#"
[input.keyboard]
R1 = "W"
L1 = "Q"

[mods.dw1]
enabled = ["b", "a"]
[mods.dw1.options.a]
speed = 3

[mods.dw3]
enabled = ["a"]
"#;
        let c = Config::from_toml(old).unwrap();
        assert_eq!(c.input.keyboard["R1"], "E");
        assert_eq!(c.input.keyboard["L1"], "Q");
        assert!(c.mods.is_enabled(GameId::Dw1, "a") && c.mods.is_enabled(GameId::Dw1, "b"));
        assert!(c.mods.is_enabled(GameId::Dw3, "a") && !c.mods.is_enabled(GameId::Dw3, "b"));
        assert!(!c.mods.is_enabled(GameId::Dw2, "a"));
        assert_eq!(c.mods.order, vec!["b", "a"]);
        assert_eq!(c.mods.options["a"]["speed"].as_integer(), Some(3));
        assert!(c.mods.check_updates);
        // A rebinding the player chose is kept.
        let custom = "settings_version = 2\n[input.keyboard]\nR1 = \"W\"\n";
        assert_eq!(Config::from_toml(custom).unwrap().input.keyboard["R1"], "W");
        // Round trip.
        let again = Config::from_toml(&toml::to_string_pretty(&c).unwrap()).unwrap();
        assert_eq!(again.mods.order, c.mods.order);
        assert_eq!(again.mods.enabled, c.mods.enabled);
    }

    #[test]
    fn load_order_moves() {
        let mut m = ModsConfig { order: vec!["c".into(), "a".into()], ..Default::default() };
        let installed = ["a", "b", "c", "d"];
        assert_eq!(m.load_order(installed), vec!["c", "a", "b", "d"]);
        // The visible rows (a is filtered out), in load order.
        let vis = |m: &ModsConfig| -> Vec<String> { m.load_order(installed).into_iter().filter(|x| x != "a").collect() };
        assert!(m.move_within(&installed, &vis(&m), "b", -1));
        assert_eq!(m.load_order(installed), vec!["b", "a", "c", "d"]);
        assert!(m.move_to_edge(&installed, &vis(&m), "d", true));
        assert_eq!(m.load_order(installed), vec!["d", "b", "a", "c"]);
        assert!(m.move_to_edge(&installed, &vis(&m), "d", false));
        assert_eq!(m.load_order(installed), vec!["b", "a", "c", "d"]);
        assert!(!m.move_within(&installed, &vis(&m), "d", 1));
    }
}
