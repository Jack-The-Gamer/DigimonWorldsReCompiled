//! The player's other imported discs, which a mod naming them in
//! `uses_games` can read while it loads (`dwrc.discs`). They are opened only
//! when a script first reads one, read-only: a mod can build data from
//! another game the player owns without shipping any of it.

use anyhow::Result;
use dwrc_disc::{Disc, GameId};

type Opener = Box<dyn Fn() -> Result<Disc>>;

struct Entry {
    game: GameId,
    serial: String,
    title: String,
    open: Opener,
    disc: Option<Disc>,
}

/// One disc per game: the one the player chose to play it with.
#[derive(Default)]
pub struct OtherDiscs {
    entries: Vec<Entry>,
}

impl OtherDiscs {
    /// Adds `game`'s disc; `open` is called the first time a script reads it.
    /// A second disc for the same game replaces the first.
    pub fn add(&mut self, game: GameId, serial: &str, title: &str, open: impl Fn() -> Result<Disc> + 'static) {
        self.entries.retain(|e| e.game != game);
        self.entries.push(Entry { game, serial: serial.to_string(), title: title.to_string(), open: Box::new(open), disc: None });
    }

    /// The serial and title of `game`'s disc, if the player has one.
    pub fn info(&self, game: GameId) -> Option<(&str, &str)> {
        self.entries.iter().find(|e| e.game == game).map(|e| (e.serial.as_str(), e.title.as_str()))
    }

    pub fn games(&self) -> Vec<GameId> {
        GameId::ALL.into_iter().filter(|g| self.info(*g).is_some()).collect()
    }

    /// `game`'s disc, opened on first use.
    pub fn disc(&mut self, game: GameId) -> Result<&mut Disc> {
        let e = self.entries.iter_mut().find(|e| e.game == game).ok_or_else(|| anyhow::anyhow!("no {} disc in the library", game.slug()))?;
        if e.disc.is_none() {
            e.disc = Some((e.open)()?);
        }
        Ok(e.disc.as_mut().unwrap())
    }
}
