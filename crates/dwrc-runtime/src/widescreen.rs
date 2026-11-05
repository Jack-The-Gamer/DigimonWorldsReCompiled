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
use dwrc_disc::{GameId, PsxExe};
use dwrc_psx::gpu::UiRules;
use dwrc_psx::{HookAction, Psx};

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
        GameId::Dw3 => Some(UiRules { pages: vec![(Some(5), 0, 511, 0, false), (Some(6), 0, 511, 0, false), (Some(8), 0, 511, 0, false)], untextured: true, flat_polygons: true, insets: true, stretch: true }),
        GameId::Dw2 => Some(UiRules {
            pages: vec![(Some(7), 0, 511, 48, true), (Some(15), 0, 511, 48, true), (Some(12), 248, 248, 0, false), (Some(12), 249, 253, 24, false), (Some(30), 0, 511, 0, true)],
            untextured: false,
            flat_polygons: false,
            insets: false,
            stretch: false,
        }),
        GameId::Dw1 => Some(UiRules {
            pages: vec![(Some(30), 491, 499, 0, false), (Some(5), 496, 511, 0, false), (Some(31), 496, 498, 0, false), (Some(27), 0, 511, 0, false)],
            untextured: true,
            flat_polygons: false,
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

/// Digimon World 1's field scroll limits (`0x800D7F68` on SLUS-01032), with
/// what to take from each word: the function's instructions, address
/// fields (`lui`, and loads/stores off `at` or `gp`) masked, as `(word, mask)`.
///
/// Every frame the field code calls it before it scrolls: it keeps the
/// scroll (a halfword per axis, in pixels) between 0 and the map's size
/// (a byte per axis, in 128-pixel tiles) less the screen (320×240), and
/// says per axis (flags at `a0`, `a1`) whether the scroll may move. When it
/// stops one, a companion word (gp-relative; the base it was set from at
/// the map's load, less the scroll) is reset to the base at that end.
const DW1_SCROLL_LIMITS: [(u32, u32); 53] = {
    const F: u32 = 0xFFFF_FFFF;
    const A: u32 = 0xFFFF_0000;
    [
        (0x24020001, F), (0xACA20000, F), (0xAC820000, F), (0x3C010000, A), (0x84220000, A), (0x00000000, F), (0x04410007, F), (0x00401821, F),
        (0x87820000, A), (0x3C010000, A), (0xA4200000, A), (0xAF820000, A), (0x1000000E, F), (0xAC800000, F), (0x3C010000, A), (0x80220000, A),
        (0x00000000, F), (0x000211C0, F), (0x2042FEC0, F), (0x0043082A, F), (0x10200006, F), (0x00403021, F), (0x87820000, A), (0x3C010000, A),
        (0xA4260000, A), (0xAF820000, A), (0xAC800000, F), (0x3C010000, A), (0x84220000, A), (0x00000000, F), (0x04410007, F), (0x00401821, F),
        (0x87820000, A), (0x3C010000, A), (0xA4200000, A), (0xAF820000, A), (0x1000000E, F), (0xACA00000, F), (0x3C010000, A), (0x80220000, A),
        (0x00000000, F), (0x000211C0, F), (0x2042FF10, F), (0x0043082A, F), (0x10200006, F), (0x00402021, F), (0x87820000, A), (0x3C010000, A),
        (0xA4240000, A), (0xAF820000, A), (0xACA00000, F), (0x03E00008, F), (0x00000000, F),
    ]
};

/// One axis of Digimon World 1's field scroll: where its values are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScrollAxis {
    /// The scroll (halfword) and the map's size in tiles (signed byte).
    scroll: u32,
    size: u32,
    /// The screen's size on this axis.
    screen: i32,
    /// The companion word and its bases at either end (gp-relative).
    other: i16,
    low_base: i16,
    high_base: i16,
}

impl ScrollAxis {
    /// Keeps the scroll `margin` pixels inside the map's ends (the game: 0),
    /// as the game does; returns whether it may move.
    fn limit(&self, bus: &mut dwrc_psx::Bus, gp: u32, margin: i32) -> bool {
        let scroll = bus.peek16(self.scroll) as i16 as i32;
        let max = bus.peek8(self.size) as i8 as i32 * 128 - self.screen;
        // A map narrower than the wide picture: the scroll stays in its middle.
        let margin = margin.min(max.max(0) / 2);
        let (lo, hi) = (margin, max - margin);
        let gpw = |off: i16| gp.wrapping_add(off as i32 as u32);
        let base = |bus: &dwrc_psx::Bus, off: i16| bus.peek16(gpw(off)) as i16 as i32;
        let (at, other) = if scroll < lo {
            (lo, base(bus, self.low_base) - lo)
        } else if scroll > hi {
            (hi, base(bus, self.high_base) + (max - hi))
        } else {
            return true;
        };
        bus.poke16(self.scroll, at as u16);
        bus.poke32(gpw(self.other), other as u32);
        false
    }
}

/// Finds Digimon World 1's field scroll limits in the executable: the
/// function's address and its two axes.
fn find_scroll_limits(exe: &PsxExe) -> Option<(u32, [ScrollAxis; 2])> {
    let words: Vec<u32> = exe.text.chunks_exact(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
    let at = words.windows(DW1_SCROLL_LIMITS.len()).position(|w| w.iter().zip(&DW1_SCROLL_LIMITS).all(|(&x, &(want, mask))| x & mask == want))?;
    let w = &words[at..];
    let imm = |i: usize| w[i] as u16 as i16;
    // `lui at, hi` then a load off `at`.
    let abs = |i: usize| ((w[i] & 0xFFFF) << 16).wrapping_add(imm(i + 1) as i32 as u32);
    let axis = |base: usize, screen: i32| ScrollAxis { scroll: abs(base + 3), size: abs(base + 14), screen, other: imm(base + 11), low_base: imm(base + 8), high_base: imm(base + 22) };
    Some((exe.load_addr + 4 * at as u32, [axis(0, 320), axis(24, 240)]))
}

/// Digimon World 1's field scrolls until the edge of its 4:3 screen meets
/// the end of the map, so a wide picture would show past the map's ends
/// (black). Hooks the game's scroll limits to stop the scroll the
/// widescreen margin earlier, while widescreen is on; with it off the game's
/// own code runs. Returns whether the executable has the function.
fn install_scroll_limits(psx: &mut Psx, game: GameId, exe: &PsxExe) -> bool {
    if game != GameId::Dw1 {
        return false;
    }
    let Some((addr, [x, y])) = find_scroll_limits(exe) else {
        log::warn!("widescreen: Digimon World 1's field scroll limits not found; the field may scroll past the map's ends");
        return false;
    };
    psx.bus.add_hook(
        addr,
        Box::new(move |cpu, bus| {
            let Some((num, den)) = bus.gpu.widescreen() else { return HookAction::Continue };
            let margin = dwrc_psx::gpu::widescreen_margin(320, num, den) as i32;
            let (gp, flag_x, flag_y) = (cpu.regs[28], cpu.regs[4], cpu.regs[5]);
            let mx = x.limit(bus, gp, margin);
            let my = y.limit(bus, gp, 0);
            bus.poke32(flag_x, mx as u32);
            bus.poke32(flag_y, my as u32);
            let ra = cpu.regs[31];
            cpu.set_pc(ra);
            HookAction::Redirect
        }),
    );
    true
}

/// Digimon World 1's field background loop (in `0x800D5920..0x800D5AEC` on
/// SLUS-01032), from where it has set a tile's quad (`0x800D5A64`), as
/// `(word, mask)` like `DW1_SCROLL_LIMITS`; `jal` targets masked too.
///
/// A field's map is a grid of 128×128 tiles, all in RAM (8 bits per pixel,
/// one palette), and a table holds a 20-byte entry per tile: pixels' address
/// (+0), map x and y (+6, +8), VRAM x and y (+10, +12: the tile's place in a
/// ring of 4 columns × 3 rows at VRAM x 768..1023), texture page (+14) and
/// CLUT (+16); a byte per tile before the table says whether it has a picture
/// (-1: drawn black). Every picture the loop draws a window of 4 columns × 3
/// rows of tiles from the ring, one quad each (`s0` the packet, `s2` the
/// tile, `s3` its column in the window of `s5`). Four columns are 512 pixels
/// and the screen 320, so a wide picture (426 at 16:9) often reaches past
/// the window's first or last column, where VRAM has no tile.
const DW1_FIELD_TILES: [(u32, u32); 34] = {
    const F: u32 = 0xFFFF_FFFF;
    const A: u32 = 0xFFFF_0000;
    const J: u32 = 0xFC00_0000;
    [
        (0x3C020000, A), (0x24420000, A), (0x00511021, F), (0x84420000, F), (0x3C010000, A), (0xA6020016, F), (0x3C020000, A), (0x24420000, A),
        (0x00511021, F), (0x84420000, F), (0x02003821, F), (0xA602000E, F), (0x84250000, A), (0x3C020000, A), (0x3C010000, A), (0x24420000, A),
        (0x84260000, A), (0x0C000000, J), (0x00512021, F), (0x02C02021, F), (0x0C000000, J), (0x02002821, F), (0x26100028, F), (0x0C000000, J),
        (0x02002021, F), (0x22730001, F), (0x22520001, F), (0x0275082A, F), (0x1420FF9C, F), (0x00000000, F), (0x22940001, F), (0x2A810003, F),
        (0x1420FF8E, F), (0x00000000, F),
    ]
};

/// Where Digimon World 1's field tiles are: the hook address (after a quad
/// is set, before it is added to the drawing list), the tile table and the
/// table of which tiles have a picture.
fn find_field_tiles(exe: &PsxExe) -> Option<(u32, u32, u32)> {
    let words: Vec<u32> = exe.text.chunks_exact(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
    let at = words.windows(DW1_FIELD_TILES.len()).position(|w| w.iter().zip(&DW1_FIELD_TILES).all(|(&x, &(want, mask))| x & mask == want))?;
    let abs = |hi: u32, lo: u32| ((hi & 0xFFFF) << 16).wrapping_add(lo as u16 as i16 as i32 as u32);
    let table = abs(words[at + 13], words[at + 15]);
    // The picture check, earlier in the loop: `lui v0, hi; addiu v0, v0, lo; addu v0, v0, s2; lb v0, 0(v0)`.
    let p = at.checked_sub(66)?;
    let ok = words[p] & 0xFFFF_0000 == 0x3C02_0000 && words[p + 1] & 0xFFFF_0000 == 0x2442_0000 && words[p + 2] == 0x0052_1021 && words[p + 3] == 0x8042_0000;
    let has = abs(words[p], words[p + 1]);
    (ok && has < table).then(|| (exe.load_addr + 4 * (at as u32 + 19), table, has))
}

/// Digimon World 1's field map, as `install_field_tiles` sees it.
struct FieldTiles {
    /// The tile table and the "has a picture" bytes (see `DW1_FIELD_TILES`).
    table: u32,
    has: u32,
    /// Tiles the table has room for.
    tiles: u32,
    /// The map's tiles as last seen whole in RAM (VRAM halfwords, 64×128 each).
    copy: Vec<Option<Vec<u16>>>,
}

impl FieldTiles {
    fn entry(&self, n: u32) -> u32 {
        self.table + 20 * n
    }

    /// Tile `n`'s pixels in RAM, while RAM still holds the map: a battle
    /// loads over it (the game goes on drawing from VRAM). Each tile's pixels
    /// come after its place on the map (`y << 16 | x / 2`), and the next
    /// tile's after them, so both places must be there.
    fn in_ram(&self, bus: &dwrc_psx::Bus, n: u32) -> Option<Vec<u16>> {
        if n >= self.tiles || bus.peek8(self.has + n) as i8 == -1 {
            return None;
        }
        let place = |e: u32| (bus.peek16(e + 8) as u32) << 16 | bus.peek16(e + 6) as u32 / 2;
        let at = (bus.peek32(self.entry(n)) & 0x1F_FFFF) as usize;
        if at < 4 || at + 128 * 128 + 4 > bus.ram.len() {
            return None;
        }
        let word = |a: usize| u32::from_le_bytes([bus.ram[a], bus.ram[a + 1], bus.ram[a + 2], bus.ram[a + 3]]);
        if word(at - 4) != place(self.entry(n)) {
            return None;
        }
        let next = self.entry(n + 1);
        if n + 1 < self.tiles && (bus.peek32(next) & 0x1F_FFFF) as usize == at + 128 * 128 + 4 && word(at + 128 * 128) != place(next) {
            return None;
        }
        Some(bus.ram[at..at + 128 * 128].chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect())
    }

    /// Copies every tile RAM holds.
    fn take_copy(&mut self, bus: &dwrc_psx::Bus) {
        self.copy = (0..self.tiles).map(|n| self.in_ram(bus, n)).collect();
    }
}

/// VRAM `rect` (x, y, width, height), row by row.
fn vram_rect(bus: &dwrc_psx::Bus, [x, y, w, h]: [u32; 4]) -> Vec<u16> {
    (y..y + h).flat_map(|r| bus.gpu.vram[(r * 1024 + x) as usize..][..w as usize].iter().copied()).collect()
}

/// Draws the tiles beside Digimon World 1's field window in the widescreen
/// margins (see `DW1_FIELD_TILES`): when the loop sets the quad of a tile in
/// the window's first (last) column, the GPU is given the tile to its left
/// (right) on the map, to draw beside that quad when it draws it. The tiles
/// come from RAM while it holds the map (the tile being drawn is the same
/// there as in VRAM), else from a copy of the map taken then, if the copy's
/// tile is the one in VRAM (in battle). Only the host's picture changes: the
/// console runs as without it.
fn install_field_tiles(psx: &mut Psx, exe: &PsxExe) -> bool {
    let Some((addr, table, has)) = find_field_tiles(exe) else {
        log::warn!("widescreen: Digimon World 1's field tiles not found; the field's sides may be black in places");
        return false;
    };
    let mut map = FieldTiles { table, has, tiles: table - has, copy: Vec::new() };
    psx.bus.add_hook(
        addr,
        Box::new(move |cpu, bus| {
            if bus.gpu.widescreen().is_none() {
                return HookAction::Continue;
            }
            let (packet, tile, col, cols) = (cpu.regs[16], cpu.regs[18], cpu.regs[19], cpu.regs[21]);
            let edges = [(-1i32, col == 0), (1, col + 1 == cols)];
            if tile >= map.tiles || !edges.iter().any(|e| e.1) {
                return HookAction::Continue;
            }
            let e = map.entry(tile);
            let (x, y) = (bus.peek16(e + 6) as i16, bus.peek16(e + 8) as i16);
            let rect = [bus.peek16(e + 10) as u32, bus.peek16(e + 12) as u32, 64, 128];
            if rect[0] + 64 > 1024 || rect[1] + 128 > 512 {
                return HookAction::Continue;
            }
            let shown = vram_rect(bus, rect);
            let ram_ok = map.in_ram(bus, tile).is_some_and(|t| t == shown);
            for (side, edge) in edges {
                let n = tile.wrapping_add(side as u32);
                if !edge || n >= map.tiles {
                    continue;
                }
                // The neighbour on the same row of the map.
                let (nx, ny) = (bus.peek16(map.entry(n) + 6) as i16, bus.peek16(map.entry(n) + 8) as i16);
                if ny != y || nx as i32 - x as i32 != 128 * side {
                    continue;
                }
                let copied = |m: &FieldTiles, i: u32, want: &Vec<u16>| m.copy.get(i as usize).and_then(|c| c.as_ref()) == Some(want);
                // RAM's tile (copying the map again when the copy is of another
                // map), else the copy's, when it is of this map.
                let texels = match ram_ok.then(|| map.in_ram(bus, n)).flatten() {
                    Some(t) => {
                        if !copied(&map, tile, &shown) || !copied(&map, n, &t) {
                            map.take_copy(bus);
                        }
                        t
                    }
                    None if copied(&map, tile, &shown) => match map.copy.get(n as usize) {
                        Some(Some(t)) => t.clone(),
                        _ => continue,
                    },
                    None => continue,
                };
                bus.gpu.add_wide_beside(packet, dwrc_psx::gpu::Beside { dx: 128 * side, rect, texels });
            }
            HookAction::Continue
        }),
    );
    true
}

/// Installs what widescreen needs in a game's own code: Digimon World 1's
/// field scroll limits and field tiles. Both do nothing while widescreen is off.
pub fn install_hooks(psx: &mut Psx, game: GameId, exe: &PsxExe) {
    if game == GameId::Dw1 {
        install_scroll_limits(psx, game, exe);
        install_field_tiles(psx, exe);
    }
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
