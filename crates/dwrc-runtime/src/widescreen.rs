//! Widescreen: more of the picture at the sides, nothing stretched.
//!
//! The games draw 3D scenes with polygons that reach past the edges of the
//! 4:3 screen, and the console's GPU clips them there. With widescreen on,
//! the GPU model also draws those parts into strips beside the picture
//! (`dwrc_psx::gpu::wide`), so frames come out wider (426×240 instead of
//! 320×240 for 16:9) with square pixels as before. The middle of the
//! picture is the console's own, pixel for pixel, and the console runs
//! exactly as it does without widescreen: only the host draws more.
//!
//! What the strips show is what the game sent past the edges. Pictures the
//! games draw flat (menus, text, movies, pre-drawn screens) end at the 4:3
//! edges, so the strips stay black beside them.

use crate::config::Widescreen;
use dwrc_disc::GameId;
use dwrc_psx::gpu::UiRules;
use dwrc_psx::Psx;

/// Whether a game is shown in widescreen when it is switched on.
pub fn supported(game: GameId) -> bool {
    matches!(game, GameId::Dw1 | GameId::Dw2 | GameId::Dw3)
}

impl Widescreen {
    pub const ALL: [Widescreen; 4] = [Widescreen::Off, Widescreen::W16x9, Widescreen::W16x10, Widescreen::W21x9];

    pub fn label(self) -> &'static str {
        match self {
            Widescreen::Off => "Off",
            Widescreen::W16x9 => "16:9",
            Widescreen::W16x10 => "16:10",
            Widescreen::W21x9 => "21:9",
        }
    }

    /// The screen's width and height (None: off).
    pub fn ratio(self) -> Option<(u32, u32)> {
        match self {
            Widescreen::Off => None,
            Widescreen::W16x9 => Some((16, 9)),
            Widescreen::W16x10 => Some((16, 10)),
            Widescreen::W21x9 => Some((21, 9)),
        }
    }

    /// How much wider than 4:3 the picture is, as a fraction: 3w / 4h
    /// (16:9 gives 4/3).
    pub fn widening(self) -> Option<(u32, u32)> {
        let (w, h) = self.ratio()?;
        let (n, d) = (3 * w, 4 * h);
        let g = gcd(n, d);
        Some((n / g, d / g))
    }

    /// How much wider than 4:3 the picture is shown (1.0 when off).
    pub fn stretch(self) -> f32 {
        match self.widening() {
            Some((n, d)) => n as f32 / d as f32,
            None => 1.0,
        }
    }

    /// Reads "off", "16:9", "16:10" or "21:9".
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|w| w.label().eq_ignore_ascii_case(s))
    }
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// How a game draws its UI, for Widescreen UI.
///
/// Digimon World 2003/3 keeps its UI pictures and text on texture pages 5,
/// 6 and 8 (VRAM x 320, 384 and 512, top half): battle menu, health panels,
/// Start menu, text boxes. Its 3D scenes, field backgrounds and characters
/// use other pages. Its bar fills are untextured, and the partner camera
/// feed in battle is drawn in its own small area inside the picture.
///
/// Digimon World 2's dungeon HUD (name, HP/EP) is on pages 7 and 15 in
/// small flat pieces (battle draws its 3D walls and floor on page 15 in
/// shaded polygons), its
/// letters on page 12 with CLUT row 248. In battle its panels and windows
/// are flat pieces on page 30 (the partner models on the same page are
/// shaded) and dim letters on page 12 rows 249-253, which the
/// dungeon floor also uses, in pieces of 30 pixels and more (letters are 7×18). The talk
/// portraits (page 12, rows 240-242) stay with their frames. Its dungeon walls are
/// untextured, so untextured shapes are not UI there. The world map's labels
/// share page 5 and its CLUT rows with the dungeon walls, so they stay.
///
/// Digimon World 1 shares texture pages between its HUD and the map, so it
/// goes by page and CLUT row: the clock (page 30, rows 496-499), the field
/// status bars (page 5, rows 498-503), the battle command icon (page 31,
/// rows 496-497), the partner's HP/MP in battle (page 30, rows 491-492) and
/// battle text such as "Your Call" (page 27). The enemy's HP readout floats
/// over it in the scene (page 31 row 499, digits on page 22 row 496, outline
/// in lines), so it is left out. Map tiles are on rows 480-486, models on
/// 488-495. Its windows are untextured (a battle command window spans the
/// screen, so what it covers stays with it).
pub fn ui_rules(game: GameId) -> Option<UiRules> {
    match game {
        GameId::Dw3 => Some(UiRules { pages: vec![(Some(5), 0, 511, 0, false), (Some(6), 0, 511, 0, false), (Some(8), 0, 511, 0, false)], untextured: true, insets: true, stretch: true }),
        GameId::Dw2 => Some(UiRules {
            pages: vec![(Some(7), 0, 511, 48, true), (Some(15), 0, 511, 48, true), (Some(12), 248, 248, 0, false), (Some(12), 249, 253, 24, false), (Some(30), 0, 511, 0, true)],
            untextured: false,
            insets: false,
            stretch: false,
        }),
        GameId::Dw1 => Some(UiRules {
            pages: vec![(Some(30), 491, 499, 0, false), (Some(5), 496, 511, 0, false), (Some(31), 496, 498, 0, false), (Some(27), 0, 511, 0, false)],
            untextured: true,
            insets: false,
            stretch: false,
        }),
    }
}

/// Switches widescreen on or off, with Widescreen UI when `ui` is set and
/// the game's UI is known (it lasts across resets, and can change at any time).
pub fn apply(psx: &mut Psx, game: GameId, w: Widescreen, ui: bool) {
    psx.bus.gpu.set_widescreen_ui(if ui { ui_rules(game) } else { None });
    psx.bus.gpu.set_widescreen(w.widening());
}

/// The middle `keep` columns of a frame (a widescreen frame cut to 4:3).
pub fn crop_middle(pixels: &[u32], width: u32, height: u32, keep: u32) -> Vec<u32> {
    let keep = keep.min(width);
    let x0 = ((width - keep) / 2) as usize;
    pixels.chunks_exact(width as usize).take(height as usize).flat_map(|row| row[x0..x0 + keep as usize].iter().copied()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratios() {
        assert_eq!(Widescreen::Off.widening(), None);
        assert_eq!(Widescreen::W16x9.widening(), Some((4, 3)));
        assert_eq!(Widescreen::W16x10.widening(), Some((6, 5)));
        assert_eq!(Widescreen::W21x9.widening(), Some((7, 4)));
        assert_eq!(Widescreen::parse("16:9"), Some(Widescreen::W16x9));
        assert_eq!(Widescreen::parse("OFF"), Some(Widescreen::Off));
        assert!((Widescreen::W16x9.stretch() - 4.0 / 3.0).abs() < 1e-6);
        assert_eq!(crop_middle(&[1, 2, 3, 4, 5, 6, 7, 8], 4, 2, 2), vec![2, 3, 6, 7]);
    }

    #[test]
    fn settings_key() {
        let mut c = crate::Config::default();
        c.video.widescreen = Widescreen::W16x9;
        let t = toml::to_string_pretty(&c).unwrap();
        assert!(t.contains("widescreen = \"16:9\""), "{t}");
        assert_eq!(crate::Config::from_toml(&t).unwrap().video.widescreen, Widescreen::W16x9);
        assert_eq!(crate::Config::from_toml("").unwrap().video.widescreen, Widescreen::Off);
    }
}
