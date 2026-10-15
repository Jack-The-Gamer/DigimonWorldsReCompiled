//! Player settings (`settings.toml`).

use crate::paths::Paths;
use serde::{Deserialize, Serialize};
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
            ("R1", "W"),
            ("L2", "A"),
            ("R2", "D"),
        ];
        Self { keyboard: pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect() }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GameModConfig {
    /// Enabled mod ids in load order.
    pub enabled: Vec<String>,
    /// Per-mod option values.
    pub options: BTreeMap<String, BTreeMap<String, toml::Value>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub video: VideoConfig,
    pub audio: AudioConfig,
    pub emulation: EmulationConfig,
    pub input: InputConfig,
    pub mods: BTreeMap<String, GameModConfig>,
}

impl Config {
    pub fn load(paths: &Paths) -> Self {
        std::fs::read_to_string(paths.config_file())
            .ok()
            .and_then(|t| toml::from_str(&t).map_err(|e| log::warn!("settings.toml: {e}")).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, paths: &Paths) -> anyhow::Result<()> {
        paths.ensure()?;
        std::fs::write(paths.config_file(), toml::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn game_mods(&self, g: dwrc_disc::GameId) -> GameModConfig {
        self.mods.get(g.slug()).cloned().unwrap_or_default()
    }

    pub fn game_mods_mut(&mut self, g: dwrc_disc::GameId) -> &mut GameModConfig {
        self.mods.entry(g.slug().to_string()).or_default()
    }
}
