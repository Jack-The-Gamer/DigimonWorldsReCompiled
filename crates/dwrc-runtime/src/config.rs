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
    /// A CRT television: scanlines, phosphor mask, optional curvature.
    Crt,
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

/// Widescreen: the 3D scenes of the games that support it are drawn with a
/// wider view and shown at this screen shape (see `widescreen`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Widescreen {
    /// The picture as the console drew it.
    #[default]
    #[serde(rename = "off")]
    Off,
    #[serde(rename = "16:9")]
    W16x9,
    #[serde(rename = "16:10")]
    W16x10,
    #[serde(rename = "21:9")]
    W21x9,
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
    /// A wider view in the games that support it.
    pub widescreen: Widescreen,
    /// With widescreen: menus and panels near the screen's edges move to the
    /// edges of the wider picture (games with known UI only).
    pub widescreen_ui: bool,
    pub show_fps: bool,
    pub start_fullscreen: bool,
    /// Settings for the CRT filter.
    pub crt: CrtConfig,
    /// Run Digimon World 2003 (Europe) in its own 60 Hz mode, at the speed
    /// of the USA release; off plays it at the original 50 Hz.
    pub pal_60hz: bool,
}

impl Default for VideoConfig {
    fn default() -> Self {
        Self { filter: Filter::Nearest, aspect: Aspect::Tv4x3, widescreen: Widescreen::Off, widescreen_ui: false, show_fps: false, start_fullscreen: false, crt: CrtConfig::default(), pal_60hz: true }
    }
}

/// The CRT filter's strengths (0..1).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CrtConfig {
    /// How visible the gaps between scanlines are.
    pub scanlines: f32,
    /// How strong the red/green/blue phosphor stripes are.
    pub mask: f32,
    /// How curved the screen is (0 = flat).
    pub curvature: f32,
}

impl Default for CrtConfig {
    fn default() -> Self {
        Self { scanlines: 0.75, mask: 0.4, curvature: 0.5 }
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

/// Which BIOS a game starts with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum BiosMode {
    /// The player's own BIOS for the game's region when one has been added
    /// (most exact), otherwise the built-in one.
    #[default]
    Auto,
    /// Always the built-in BIOS.
    BuiltIn,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EmulationConfig {
    /// Skip the BIOS boot animation (the kernel still initialises normally).
    /// Only applies to a console BIOS; the built-in one boots directly.
    pub fast_boot: bool,
    pub bios: BiosMode,
    pub engine: EngineKind,
    /// Insert a second memory card in slot 2.
    pub memory_card_2: bool,
}

impl Default for EmulationConfig {
    fn default() -> Self {
        Self { fast_boot: true, bios: BiosMode::Auto, engine: EngineKind::Recompiled, memory_card_2: true }
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
            ("L1", "A"),
            ("L2", "Q"),
            ("R1", "D"),
            ("R2", "E"),
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

    /// Drops `id` into gap `gap` of `visible` (0 is above the first row,
    /// `visible.len()` below the last), as when a row is dragged there.
    /// Mods that aren't visible keep their places.
    pub fn move_to_gap(&mut self, installed: &[&str], visible: &[String], id: &str, gap: usize) -> bool {
        let before = self.load_order(installed.iter().copied());
        let mut full = before.clone();
        let Some(from) = full.iter().position(|x| x == id) else { return false };
        // The visible row it lands above, or else the one it lands below.
        let above = visible.get(gap..).unwrap_or_default().iter().find(|v| *v != id).cloned();
        let below = visible[..gap.min(visible.len())].iter().rev().find(|v| *v != id).cloned();
        full.remove(from);
        let at = match (above, below) {
            (Some(a), _) => full.iter().position(|x| *x == a),
            (None, Some(b)) => full.iter().position(|x| *x == b).map(|p| p + 1),
            (None, None) => None,
        };
        let Some(at) = at else { return false };
        full.insert(at, id.to_string());
        let changed = full != before;
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

/// Re:Compiled's own updates.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdatesConfig {
    /// Look for a newer release of Re:Compiled when the launcher starts.
    pub check_on_start: bool,
}

impl Default for UpdatesConfig {
    fn default() -> Self {
        Self { check_on_start: true }
    }
}

/// Settings file format version. 2: default R1 moved from W to E; mods
/// switched to a shared load order with per-game answers. 3: default
/// shoulder buttons moved to L1 A, L2 Q, R1 D, R2 E (were Q, A, E, D).
pub const SETTINGS_VERSION: u32 = 3;

/// The shoulder-button keys before settings version 3, and since.
const OLD_TRIGGERS: [(&str, &str); 4] = [("L1", "Q"), ("L2", "A"), ("R1", "E"), ("R2", "D")];
const NEW_TRIGGERS: [(&str, &str); 4] = [("L1", "A"), ("L2", "Q"), ("R1", "D"), ("R2", "E")];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub settings_version: u32,
    pub video: VideoConfig,
    pub audio: AudioConfig,
    pub emulation: EmulationConfig,
    pub input: InputConfig,
    pub mods: ModsConfig,
    pub updates: UpdatesConfig,
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
            updates: UpdatesConfig::default(),
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
        // Players still on the old shoulder-button defaults get the new
        // ones; any shoulder button they rebound keeps all four as they are.
        let k = &mut cfg.input.keyboard;
        if version < 3 && OLD_TRIGGERS.iter().all(|(b, key)| k.get(*b).is_none_or(|v| v == key)) {
            for (b, key) in NEW_TRIGGERS {
                k.insert(b.into(), key.into());
            }
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
    fn default_triggers() {
        let k = Config::default().input.keyboard;
        let got: Vec<&str> = ["L1", "L2", "R1", "R2"].iter().map(|b| k[*b].as_str()).collect();
        assert_eq!(got, vec!["A", "Q", "D", "E"]);
    }

    #[test]
    fn migrates_old_trigger_defaults() {
        let keys = |t: &str| {
            let k = Config::from_toml(t).unwrap().input.keyboard;
            ["L1", "L2", "R1", "R2"].iter().map(|b| k.get(*b).cloned().unwrap_or_default()).collect::<Vec<_>>().join(" ")
        };
        // The old defaults (version 2) move to the new ones.
        assert_eq!(keys("settings_version = 2\n[input.keyboard]\nL1 = \"Q\"\nL2 = \"A\"\nR1 = \"E\"\nR2 = \"D\"\nCross = \"X\"\n"), "A Q D E");
        // A version 1 file with R1 on W gets there too.
        assert_eq!(keys("[input.keyboard]\nL1 = \"Q\"\nL2 = \"A\"\nR1 = \"W\"\nR2 = \"D\"\n"), "A Q D E");
        // A player who rebound any shoulder button keeps all four.
        assert_eq!(keys("settings_version = 2\n[input.keyboard]\nL1 = \"Q\"\nL2 = \"A\"\nR1 = \"E\"\nR2 = \"F\"\n"), "Q A E F");
        // Already version 3: left alone, even if it matches the old defaults.
        assert_eq!(keys("settings_version = 3\n[input.keyboard]\nL1 = \"Q\"\nL2 = \"A\"\nR1 = \"E\"\nR2 = \"D\"\n"), "Q A E D");
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
        // R1 on W (the version 1 default) and L1 on Q: old defaults, so
        // they move to the current ones.
        assert_eq!(c.input.keyboard["R1"], "D");
        assert_eq!(c.input.keyboard["L1"], "A");
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
        // Dragging: visible rows are b, c, d (a stays hidden in place).
        assert_eq!(vis(&m), vec!["b", "c", "d"]);
        assert!(m.move_to_gap(&installed, &vis(&m), "d", 0));
        assert_eq!(m.load_order(installed), vec!["d", "b", "a", "c"]);
        assert!(m.move_to_gap(&installed, &vis(&m), "d", 3));
        assert_eq!(m.load_order(installed), vec!["b", "a", "c", "d"]);
        assert!(m.move_to_gap(&installed, &vis(&m), "b", 2));
        assert_eq!(m.load_order(installed), vec!["a", "c", "b", "d"]);
        // Dropped where it already is: nothing changes.
        assert!(!m.move_to_gap(&installed, &vis(&m), "b", 1));
        assert!(!m.move_to_gap(&installed, &vis(&m), "b", 2));
        assert!(!m.move_to_gap(&installed, &vis(&m), "zz", 0));
    }
}
