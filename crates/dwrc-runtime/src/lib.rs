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
use dwrc_mods::{EnabledMod, ModInfo};

/// Mods available for a game (user mods first, then bundled).
pub fn available_mods(paths: &Paths, game: GameId) -> Vec<ModInfo> {
    let mut roots = vec![paths.user_mods_root()];
    roots.extend(paths.bundled_mods_roots());
    dwrc_mods::discover(&roots, game)
}

/// The mods the player enabled for a game, in load order, with options.
pub fn enabled_mods(paths: &Paths, cfg: &Config, game: GameId, serial: &str) -> Vec<EnabledMod> {
    let gm = cfg.game_mods(game);
    let all = available_mods(paths, game);
    gm.enabled
        .iter()
        .filter_map(|id| all.iter().find(|m| m.id() == id))
        .filter(|m| m.is_usable())
        .filter(|m| m.manifest.info.serials.is_empty() || m.manifest.info.serials.iter().any(|s| s == serial))
        .map(|m| EnabledMod {
            info: m.clone(),
            options: dwrc_mods::resolve_options(m, gm.options.get(m.id())),
            data_dir: paths.mod_data_dir(game, m.id()),
        })
        .collect()
}
