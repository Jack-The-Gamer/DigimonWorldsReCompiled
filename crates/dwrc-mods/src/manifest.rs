//! `mod.toml` manifest format.

use anyhow::{Context, Result};
use dwrc_disc::GameId;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModManifest {
    #[serde(rename = "mod")]
    pub info: ModSection,
    #[serde(default, rename = "option")]
    pub options: Vec<OptionDef>,
    #[serde(default, rename = "patch")]
    pub patches: Vec<PatchDef>,
    #[serde(default, rename = "lba")]
    pub lba: Vec<LbaDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModSection {
    pub id: String,
    pub name: String,
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub description: String,
    /// `dw1`, `dw2`, `dw3`, or `any` for mods that work with every game.
    #[serde(with = "game_slug")]
    pub game: GameSel,
    /// Restrict to specific releases (boot serials such as `SLUS-01032`).
    #[serde(default)]
    pub serials: Vec<String>,
    /// Lua entry script, relative to the mod folder.
    #[serde(default)]
    pub script: Option<String>,
    /// Folder (relative) whose contents replace files on the disc by ISO path.
    #[serde(default = "default_files_dir")]
    pub files_dir: String,
}

fn default_version() -> String {
    "1.0.0".into()
}

fn default_files_dir() -> String {
    "files".into()
}

/// Which game(s) a mod targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameSel {
    Any,
    Game(GameId),
}

impl GameSel {
    pub fn matches(self, g: GameId) -> bool {
        match self {
            GameSel::Any => true,
            GameSel::Game(x) => x == g,
        }
    }
}

mod game_slug {
    use super::GameSel;
    use dwrc_disc::GameId;
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(g: &GameSel, s: S) -> Result<S::Ok, S::Error> {
        match g {
            GameSel::Any => s.serialize_str("any"),
            GameSel::Game(g) => s.serialize_str(g.slug()),
        }
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<GameSel, D::Error> {
        let s = String::deserialize(d)?;
        if s.eq_ignore_ascii_case("any") {
            return Ok(GameSel::Any);
        }
        GameId::from_slug(&s)
            .map(GameSel::Game)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown game '{s}' (use dw1, dw2, dw3 or any)")))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OptionKind {
    Bool,
    Int,
    Float,
    Choice,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptionDef {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: OptionKind,
    pub default: toml::Value,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub choices: Vec<String>,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchDef {
    pub address: u32,
    /// Hex bytes, e.g. "00 00 02 24".
    #[serde(default)]
    pub bytes: Option<String>,
    #[serde(default)]
    pub u8: Option<u8>,
    #[serde(default)]
    pub u16: Option<u16>,
    #[serde(default)]
    pub u32: Option<u32>,
    /// "always" (every frame, default) or "once".
    #[serde(default = "default_when")]
    pub when: String,
    /// Only apply while the bytes at `if_address` equal `if_bytes`.
    #[serde(default)]
    pub if_address: Option<u32>,
    #[serde(default)]
    pub if_bytes: Option<String>,
}

fn default_when() -> String {
    "always".into()
}

/// Raw sector-range replacement for games that address data by LBA.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LbaDef {
    /// First logical block (2048-byte data sector) to replace.
    pub lba: u32,
    /// File (relative to the mod folder) holding the replacement data.
    pub file: String,
    /// Maximum sectors the original data occupies (safety check).
    #[serde(default)]
    pub max_sectors: Option<u32>,
}

impl ModManifest {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    /// Checks references inside the mod folder.
    pub fn validate(&self, root: &Path) -> Vec<String> {
        let mut p = Vec::new();
        if self.info.id.is_empty() || !self.info.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            p.push("mod id must be non-empty and use only letters, digits, '-' and '_'".into());
        }
        if let Some(s) = &self.info.script {
            if !root.join(s).is_file() {
                p.push(format!("script '{s}' not found"));
            }
        }
        for l in &self.lba {
            if !root.join(&l.file).is_file() {
                p.push(format!("LBA replacement file '{}' not found", l.file));
            }
        }
        for o in &self.options {
            let ok = match o.kind {
                OptionKind::Bool => o.default.is_bool(),
                OptionKind::Int => o.default.is_integer(),
                OptionKind::Float => o.default.is_float() || o.default.is_integer(),
                OptionKind::Choice => o.default.as_str().map(|s| o.choices.iter().any(|c| c == s)).unwrap_or(false),
            };
            if !ok {
                p.push(format!("option '{}' has a default that does not match its type", o.id));
            }
        }
        p
    }
}
