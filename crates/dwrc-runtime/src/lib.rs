//! Application runtime shared by the launcher and the tools: where data
//! lives, the game library (import + verification), BIOS handling,
//! settings, mod selection and play sessions.

pub mod bios;
pub mod config;
pub mod library;
pub mod paths;
pub mod session;

pub use config::Config;
pub use library::{DiscStatus, Library};
pub use paths::Paths;
pub use session::Session;

use dwrc_disc::GameId;
use dwrc_mods::{EnabledMod, ModInfo, ModSource, Skip};

/// Every mod the program can see: the player's installed mods first, then
/// the bundled ones (an installed copy of a bundled id overrides it).
pub fn installed_mods(paths: &Paths) -> Vec<ModInfo> {
    dwrc_mods::discover_all(&paths.mod_roots())
}

/// Mods that run on a game.
pub fn available_mods(paths: &Paths, game: GameId) -> Vec<ModInfo> {
    installed_mods(paths).into_iter().filter(|m| m.supports(game)).collect()
}

/// Which of the switched-on mods load for `game` (in load order) and which
/// are skipped, and why. Empty in safe mode.
pub fn mod_plan<'a>(cfg: &Config, all: &'a [ModInfo], game: GameId, serial: Option<&str>) -> (Vec<&'a ModInfo>, Vec<(&'a ModInfo, Skip)>) {
    if cfg.mods.safe_mode {
        return (Vec::new(), Vec::new());
    }
    let order = cfg.mods.load_order(all.iter().map(|m| m.id()));
    let wanted: Vec<&ModInfo> = order
        .iter()
        .filter(|id| cfg.mods.is_enabled(game, id))
        .filter_map(|id| all.iter().find(|m| m.id() == id))
        .collect();
    dwrc_mods::plan(&wanted, game, serial)
}

/// The mods that load for a game, in load order, with their options.
pub fn enabled_mods(paths: &Paths, cfg: &Config, game: GameId, serial: &str) -> Vec<EnabledMod> {
    let all = installed_mods(paths);
    let (load, skipped) = mod_plan(cfg, &all, game, Some(serial));
    for (m, why) in skipped {
        log::warn!("mod {} not loaded: {why}", m.id());
    }
    load.into_iter()
        .map(|m| EnabledMod {
            info: m.clone(),
            options: dwrc_mods::resolve_options(m, cfg.mods.options.get(m.id())),
            data_dir: paths.mod_data_dir(game, m.id()),
        })
        .collect()
}

/// Whether a mod lives in the player's folder (and can be removed/updated).
pub fn is_user_mod(m: &ModInfo) -> bool {
    m.source == ModSource::User
}
