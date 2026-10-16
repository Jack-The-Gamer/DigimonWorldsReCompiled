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
    /// Which games the mod is for: `game = "dw1"`, `game = "any"`, or a
    /// list, `games = ["dw1", "dw2"]`. One install serves every game it names.
    #[serde(alias = "games", with = "game_list")]
    pub game: Vec<GameSel>,
    /// Restrict to specific releases (boot serials such as `SLUS-01032`).
    #[serde(default)]
    pub serials: Vec<String>,
    /// Lua entry script, relative to the mod folder.
    #[serde(default)]
    pub script: Option<String>,
    /// Folder (relative) whose contents replace files on the disc by ISO path.
    #[serde(default = "default_files_dir")]
    pub files_dir: String,
    /// GitHub repository (`owner/repo` or a github.com URL) whose Releases
    /// publish this mod as a `.zip`. Turns on update checks and the
    /// version picker in the launcher.
    #[serde(default)]
    pub github: Option<String>,
    /// Web page for the mod (shown as a link).
    #[serde(default)]
    pub homepage: Option<String>,
    /// One to four of [`CATEGORIES`].
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub license: Option<String>,
    /// Re:Compiled versions the mod works with, as a semver range
    /// (for example `">=0.2"`).
    #[serde(default)]
    pub engine: Option<String>,
    /// Mods this one needs: `"id"` or `"id@<range>"` (e.g. `"core-lib@>=1.2"`).
    #[serde(default)]
    pub dependencies: Vec<String>,
    /// Mods this one cannot be combined with (`"id"` or `"id@<range>"`).
    #[serde(default)]
    pub conflicts: Vec<String>,
    /// Marked unfinished by its author.
    #[serde(default)]
    pub experimental: bool,
}

/// The closed category vocabulary shared by manifests and mod indexes.
pub const CATEGORIES: &[&str] = &[
    "GAMEPLAY",
    "CONTENT",
    "BALANCE",
    "ART",
    "AUDIO",
    "UI",
    "QOL",
    "TRANSLATION",
    "TOTAL_CONVERSION",
    "LIBRARY",
    "TOOL",
    "OTHER",
];

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

    pub fn parse(s: &str) -> Option<GameSel> {
        if s.eq_ignore_ascii_case("any") || s.eq_ignore_ascii_case("all") {
            return Some(GameSel::Any);
        }
        GameId::from_slug(&s.to_ascii_lowercase()).map(GameSel::Game)
    }
}

impl ModSection {
    /// Whether this mod runs on `g`.
    pub fn supports(&self, g: GameId) -> bool {
        self.game.iter().any(|s| s.matches(g))
    }

    /// Every game this mod runs on, in library order.
    pub fn games(&self) -> Vec<GameId> {
        GameId::ALL.into_iter().filter(|g| self.supports(*g)).collect()
    }

    /// The `owner/repo` this mod updates from, if `github` is valid.
    pub fn github_repo(&self) -> Option<String> {
        self.github.as_deref().and_then(parse_github_repo)
    }
}

/// Normalises the ways a GitHub repository is written (`owner/repo`,
/// `https://github.com/owner/repo`, with or without `.git` or a trailing
/// slash) to `owner/repo`.
pub fn parse_github_repo(s: &str) -> Option<String> {
    let t = s.trim().trim_end_matches('/');
    let t = t
        .strip_prefix("https://github.com/")
        .or_else(|| t.strip_prefix("http://github.com/"))
        .or_else(|| t.strip_prefix("https://www.github.com/"))
        .or_else(|| t.strip_prefix("github.com/"))
        .unwrap_or(t);
    let t = t.strip_suffix(".git").unwrap_or(t);
    let mut parts = t.split('/');
    let (owner, repo) = (parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let ok = |p: &str| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if ok(owner) && ok(repo) && !owner.starts_with('.') && !repo.starts_with('.') {
        Some(format!("{owner}/{repo}"))
    } else {
        None
    }
}

/// A dependency or conflict entry: `"id"` or `"id@<semver range>"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModRef {
    pub id: String,
    pub range: Option<String>,
}

impl ModRef {
    pub fn parse(s: &str) -> Option<ModRef> {
        let s = s.trim();
        let (id, range) = match s.split_once('@') {
            Some((i, r)) => (i.trim(), Some(r.trim().to_string()).filter(|r| !r.is_empty())),
            None => (s, None),
        };
        valid_id(id).then(|| ModRef { id: id.to_string(), range })
    }

    /// Whether `version` satisfies the range (no range: any version).
    pub fn accepts(&self, version: &str) -> bool {
        self.range.as_deref().is_none_or(|r| crate::version::satisfies(version, r))
    }
}

impl std::fmt::Display for ModRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.range {
            Some(r) => write!(f, "{} {}", self.id, r),
            None => f.write_str(&self.id),
        }
    }
}

/// Mod ids: letters, digits, '-' and '_' (they name the install folder).
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

mod game_list {
    use super::GameSel;
    use serde::de::{self, SeqAccess, Visitor};
    use serde::ser::SerializeSeq;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(g: &[GameSel], s: S) -> Result<S::Ok, S::Error> {
        let name = |g: &GameSel| match g {
            GameSel::Any => "any",
            GameSel::Game(g) => g.slug(),
        };
        if g.len() == 1 {
            return s.serialize_str(name(&g[0]));
        }
        let mut seq = s.serialize_seq(Some(g.len()))?;
        for x in g {
            seq.serialize_element(name(x))?;
        }
        seq.end()
    }

    struct V;

    fn one<E: de::Error>(s: &str) -> Result<GameSel, E> {
        GameSel::parse(s).ok_or_else(|| E::custom(format!("unknown game '{s}' (use dw1, dw2, dw3 or any)")))
    }

    impl<'de> Visitor<'de> for V {
        type Value = Vec<GameSel>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a game (dw1, dw2, dw3, any) or a list of games")
        }
        fn visit_str<E: de::Error>(self, s: &str) -> Result<Self::Value, E> {
            Ok(vec![one(s)?])
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Self::Value, A::Error> {
            let mut out = Vec::new();
            while let Some(s) = a.next_element::<String>()? {
                out.push(one(&s)?);
            }
            if out.is_empty() {
                return Err(de::Error::custom("games must name at least one game"));
            }
            Ok(out)
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<GameSel>, D::Error> {
        d.deserialize_any(V)
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

    pub fn parse(text: &str) -> Result<Self> {
        Ok(toml::from_str(text)?)
    }

    /// Non-fatal remarks (shown to authors by `dwrc-cli mod validate`).
    pub fn notes(&self) -> Vec<String> {
        let mut n = Vec::new();
        for c in &self.info.categories {
            if !CATEGORIES.contains(&c.to_ascii_uppercase().as_str()) {
                n.push(format!("unknown category '{c}' (known: {})", CATEGORIES.join(", ")));
            }
        }
        if self.info.categories.len() > 4 {
            n.push("use at most four categories".into());
        }
        if self.info.github.is_none() {
            n.push("no github repository set: players will not be offered updates".into());
        }
        n
    }

    /// Checks references inside the mod folder.
    pub fn validate(&self, root: &Path) -> Vec<String> {
        let mut p = Vec::new();
        if !valid_id(&self.info.id) {
            p.push("mod id must be 1-64 letters, digits, '-' or '_'".into());
        }
        if crate::version::parse(&self.info.version).is_none() {
            p.push(format!("version '{}' is not a version number like 1.2.0", self.info.version));
        }
        if let Some(g) = &self.info.github {
            if parse_github_repo(g).is_none() {
                p.push(format!("github '{g}' must be owner/repo or a github.com URL"));
            }
        }
        if let Some(r) = &self.info.engine {
            if !crate::version::valid_range(r) {
                p.push(format!("engine '{r}' is not a version range like \">=0.2\""));
            }
        }
        for d in self.info.dependencies.iter().chain(&self.info.conflicts) {
            match ModRef::parse(d) {
                Some(r) if r.range.as_deref().is_none_or(crate::version::valid_range) => {}
                _ => p.push(format!("'{d}' must be a mod id, optionally with @<version range>")),
            }
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
