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
///
/// Its callers: the camera following the player, a scripted camera pan
/// (`tickCameraMovement` in the dw_decomp decompilation: back to the player
/// after training, after "Agumon joins the city!") and a camera jump. The
/// pan's call is the first one after the function in the executable.
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

/// The same function as the Europe and Japan executables have it (`0x800E16F0`
/// on SLES-02914, `0x800E2EF4` on SLPS-01797 rev 1), masked like
/// `DW1_SCROLL_LIMITS`. Those two were compiled with less optimisation: the
/// flags' addresses are kept in `s1` and `s0`, and every value is loaded
/// again each time it is used, so the function is 85 words instead of 53.
/// What it does, its callers and their order are the same.
const DW1_SCROLL_LIMITS_EU_JP: [(u32, u32); 85] = {
    const F: u32 = 0xFFFF_FFFF;
    const A: u32 = 0xFFFF_0000;
    [
        (0x27BDFFF8, F), (0xAFB10004, F), (0xAFB00000, F), (0x00808821, F), (0x00A08021, F), (0x24020001, F), (0xAE020000, F), (0xAE220000, F),
        (0x3C010000, A), (0x84220000, A), (0x00000000, F), (0x04410009, F), (0x00000000, F), (0x3C010000, A), (0xA4200000, A), (0x87820000, A),
        (0x00000000, F), (0xAF820000, A), (0xAE200000, F), (0x10000018, F), (0x00000000, F), (0x3C010000, A), (0x84230000, A), (0x3C010000, A),
        (0x80220000, A), (0x00000000, F), (0x000211C0, F), (0x2042FEC0, F), (0x0043082A, F), (0x1020000E, F), (0x00000000, F), (0x3C010000, A),
        (0x80220000, A), (0x00000000, F), (0x000211C0, F), (0x2042FEC0, F), (0x00021400, F), (0x00021403, F), (0x3C010000, A), (0xA4220000, A),
        (0x87820000, A), (0x00000000, F), (0xAF820000, A), (0xAE200000, F), (0x3C010000, A), (0x84220000, A), (0x00000000, F), (0x04410009, F),
        (0x00000000, F), (0x3C010000, A), (0xA4200000, A), (0x87820000, A), (0x00000000, F), (0xAF820000, A), (0xAE000000, F), (0x10000018, F),
        (0x00000000, F), (0x3C010000, A), (0x84230000, A), (0x3C010000, A), (0x80220000, A), (0x00000000, F), (0x000211C0, F), (0x2042FF10, F),
        (0x0043082A, F), (0x1020000E, F), (0x00000000, F), (0x3C010000, A), (0x80220000, A), (0x00000000, F), (0x000211C0, F), (0x2042FF10, F),
        (0x00021400, F), (0x00021403, F), (0x3C010000, A), (0xA4220000, A), (0x87820000, A), (0x00000000, F), (0xAF820000, A), (0xAE000000, F),
        (0x8FB10004, F), (0x8FB00000, F), (0x27BD0008, F), (0x03E00008, F), (0x00000000, F),
    ]
};

/// One way the executables have the scroll limits: the function's words
/// and, among them, where the horizontal axis's addresses are read from. The
/// vertical axis's are the same, `y` words further on.
struct LimitsShape {
    words: &'static [(u32, u32)],
    /// The `lui at` of the scroll's first load, and of the map size's.
    scroll: usize,
    size: usize,
    /// The companion word's store, and the loads of its bases at the low and
    /// the high end (all off `gp`).
    other: usize,
    low_base: usize,
    high_base: usize,
    y: usize,
}

const DW1_LIMITS_SHAPES: [LimitsShape; 2] = [
    LimitsShape { words: &DW1_SCROLL_LIMITS, scroll: 3, size: 14, other: 11, low_base: 8, high_base: 22, y: 24 },
    LimitsShape { words: &DW1_SCROLL_LIMITS_EU_JP, scroll: 8, size: 23, other: 17, low_base: 15, high_base: 40, y: 36 },
];

/// An executable's code as instruction words.
fn code_words(exe: &PsxExe) -> Vec<u32> {
    exe.text.chunks_exact(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect()
}

/// Where `pattern` (`(word, mask)` pairs) is first found in `words`.
fn find_words(words: &[u32], pattern: &[(u32, u32)]) -> Option<usize> {
    words.windows(pattern.len()).position(|w| w.iter().zip(pattern).all(|(&x, &(want, mask))| x & mask == want))
}

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
    ///
    /// `pan`: the call is a camera pan's. A pan ends once each axis has
    /// either reached its target or been stopped here, and the game keeps
    /// its targets inside its own limits, so there a pan that has run out of
    /// steps is on its target. With a margin the target can lie past the
    /// limit: the pan then stands on the limit, never past it and never on
    /// its target, and would go on for ever (the camera stuck, a script
    /// waiting for it hung). So for a pan, standing on a limit moved in by
    /// the margin counts as stopped by it.
    fn limit(&self, bus: &mut dwrc_psx::Bus, gp: u32, margin: i32, pan: bool) -> bool {
        let scroll = bus.peek16(self.scroll) as i16 as i32;
        let max = bus.peek8(self.size) as i8 as i32 * 128 - self.screen;
        // A map narrower than the wide picture: the scroll stays in its middle.
        let margin = margin.min(max.max(0) / 2);
        let (lo, hi) = (margin, max - margin);
        let on = pan && margin > 0;
        let gpw = |off: i16| gp.wrapping_add(off as i32 as u32);
        let base = |bus: &dwrc_psx::Bus, off: i16| bus.peek16(gpw(off)) as i16 as i32;
        let (at, other) = if scroll < lo || (on && scroll == lo) {
            (lo, base(bus, self.low_base) - lo)
        } else if scroll > hi || (on && scroll == hi) {
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
/// function's address, its two axes, and where the camera pan's call of it
/// returns to (see `DW1_SCROLL_LIMITS`), if the calls are laid out as known.
fn find_scroll_limits(exe: &PsxExe) -> Option<(u32, [ScrollAxis; 2], Option<u32>)> {
    let words = code_words(exe);
    let (shape, at) = DW1_LIMITS_SHAPES.iter().find_map(|s| Some((s, find_words(&words, s.words)?)))?;
    let w = &words[at..];
    let imm = |i: usize| w[i] as u16 as i16;
    // `lui at, hi` then a load off `at`.
    let abs = |i: usize| ((w[i] & 0xFFFF) << 16).wrapping_add(imm(i + 1) as i32 as u32);
    let axis = |base: usize, screen: i32| ScrollAxis { scroll: abs(base + shape.scroll), size: abs(base + shape.size), screen, other: imm(base + shape.other), low_base: imm(base + shape.low_base), high_base: imm(base + shape.high_base) };
    let addr = exe.load_addr + 4 * at as u32;
    // Its callers, in the executable's order: following the player (before
    // the function), the pan, the jump.
    let jal = 0x0C00_0000 | (addr >> 2 & 0x03FF_FFFF);
    let calls: Vec<usize> = words.iter().enumerate().filter(|&(_, &x)| x == jal).map(|(i, _)| i).collect();
    let pan = match calls[..] {
        [follow, pan, jump] if follow < at && pan > at && jump > pan => Some(exe.load_addr + 4 * (pan as u32 + 2)),
        _ => None,
    };
    Some((addr, [axis(0, 320), axis(shape.y, 240)], pan))
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
    let Some((addr, [x, y], pan)) = find_scroll_limits(exe) else {
        log::warn!("widescreen: Digimon World 1's field scroll limits not found; the field may scroll past the map's ends");
        return false;
    };
    if pan.is_none() {
        log::warn!("widescreen: Digimon World 1's camera pan not found among the scroll limits' callers; every caller is taken for one");
    }
    psx.bus.add_hook(
        addr,
        Box::new(move |cpu, bus| {
            let Some((num, den)) = bus.gpu.widescreen() else { return HookAction::Continue };
            let margin = dwrc_psx::gpu::widescreen_margin(320, num, den) as i32;
            let (gp, flag_x, flag_y, ra) = (cpu.regs[28], cpu.regs[4], cpu.regs[5], cpu.regs[31]);
            let panning = pan.is_none_or(|p| p == ra);
            let mx = x.limit(bus, gp, margin, panning);
            let my = y.limit(bus, gp, 0, panning);
            bus.poke32(flag_x, mx as u32);
            bus.poke32(flag_y, my as u32);
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

/// The end of the same loop as the Europe and Japan executables have it
/// (in `0x800DE090..0x800DE3A0` on SLES-02914, `0x800DFA00..0x800DFD10` on
/// SLPS-01797 rev 1), from where it works out the tile's table entry for the
/// call that sets its quad (`0x800DE2FC`, `0x800DFC6C`). Compiled with less
/// optimisation, the loop keeps only the packet (`s0`), the column (`s1`) and
/// the row (`s2`) in registers; the window's first tile, the map's width and
/// the window's columns are halfwords on the stack, and the tile's number is
/// worked out from them each time it is used.
const DW1_FIELD_TILES_EU_JP: [(u32, u32); 41] = {
    const F: u32 = 0xFFFF_FFFF;
    const A: u32 = 0xFFFF_0000;
    const J: u32 = 0xFC00_0000;
    [
        (0x87A20032, F), (0x00000000, F), (0x00520018, F), (0x00001812, F), (0x87A20030, F), (0x00000000, F), (0x00431020, F), (0x02221820, F),
        (0x00031080, F), (0x00431020, F), (0x00021880, F), (0x3C020000, A), (0x24420000, A), (0x00432021, F), (0x3C010000, A), (0x84250000, A),
        (0x3C010000, A), (0x84260000, A), (0x02003821, F), (0x0C000000, J), (0x00000000, F), (0x8FA2002C, F), (0x00000000, F), (0x24443FFC, F),
        (0x02002821, F), (0x0C000000, J), (0x00000000, F), (0x26100028, F), (0x02002021, F), (0x0C000000, J), (0x00000000, F), (0x22310001, F),
        (0x87A20034, F), (0x00000000, F), (0x0222082A, F), (0x1420FF44, F), (0x00000000, F), (0x22520001, F), (0x2A410003, F), (0x1420FF3D, F),
        (0x00000000, F),
    ]
};

/// Tiles in Digimon World 1's tile table: the biggest map is 7 × 5.
const DW1_MAP_TILES: u32 = 35;

/// Where Digimon World 1's field background loop keeps what the hook needs
/// when it gets there: the packet is in `s0` either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TileWindow {
    /// `s2` the tile, `s3` its column in the window, `s5` the window's columns (USA).
    Registers,
    /// `s1` the column and `s2` the row; halfwords at these offsets off `sp`
    /// hold the window's first tile, the map's width and the window's
    /// columns (Europe, Japan).
    Stack { first: i16, width: i16, columns: i16 },
}

impl TileWindow {
    /// The tile being drawn, its column in the window and the window's columns.
    fn read(self, cpu: &dwrc_psx::Cpu, bus: &dwrc_psx::Bus) -> (u32, u32, u32) {
        match self {
            TileWindow::Registers => (cpu.regs[18], cpu.regs[19], cpu.regs[21]),
            TileWindow::Stack { first, width, columns } => {
                let half = |off: i16| bus.peek16(cpu.regs[29].wrapping_add(off as i32 as u32)) as i16 as i32;
                let (col, row) = (cpu.regs[17] as i32, cpu.regs[18] as i32);
                (half(first).wrapping_add(half(width).wrapping_mul(row)).wrapping_add(col) as u32, col as u32, half(columns) as u32)
            }
        }
    }
}

/// Digimon World 1's field background loop in an executable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TileLoop {
    /// The hook address: after a quad is set, before it is added to the drawing list.
    hook: u32,
    /// The tile table and the table of which tiles have a picture.
    table: u32,
    has: u32,
    window: TileWindow,
}

/// Finds Digimon World 1's field background loop (see `DW1_FIELD_TILES`).
fn find_field_tiles(exe: &PsxExe) -> Option<TileLoop> {
    let words = code_words(exe);
    let abs = |hi: u32, lo: u32| ((hi & 0xFFFF) << 16).wrapping_add(lo as u16 as i16 as i32 as u32);
    // The picture check, earlier in the loop: `lui v0, hi; addiu v0, v0, lo; addu v0, v0, <tile>; lb v0, 0(v0)`.
    let has = |p: usize, add: u32| (words[p] & 0xFFFF_0000 == 0x3C02_0000 && words[p + 1] & 0xFFFF_0000 == 0x2442_0000 && words[p + 2] == add && words[p + 3] == 0x8042_0000).then(|| abs(words[p], words[p + 1]));
    let hook = |i: usize| exe.load_addr + 4 * i as u32;
    let found = if let Some(at) = find_words(&words, &DW1_FIELD_TILES) {
        TileLoop { hook: hook(at + 19), table: abs(words[at + 13], words[at + 15]), has: has(at.checked_sub(66)?, 0x0052_1021)?, window: TileWindow::Registers }
    } else {
        let at = find_words(&words, &DW1_FIELD_TILES_EU_JP)?;
        let imm = |i: usize| words[at + i] as u16 as i16;
        TileLoop { hook: hook(at + 21), table: abs(words[at + 11], words[at + 12]), has: has(at.checked_sub(138)?, 0x0043_1021)?, window: TileWindow::Stack { first: imm(4), width: imm(0), columns: imm(32) } }
    };
    // The two tables are apart (which comes first differs between releases).
    let apart = |a: u32, b: u32, len: u32| a.checked_add(len).is_some_and(|end| end <= b);
    (apart(found.has, found.table, DW1_MAP_TILES) || apart(found.table, found.has, 20 * DW1_MAP_TILES)).then_some(found)
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
    let Some(TileLoop { hook, table, has, window }) = find_field_tiles(exe) else {
        log::warn!("widescreen: Digimon World 1's field tiles not found; the field's sides may be black in places");
        return false;
    };
    let mut map = FieldTiles { table, has, tiles: DW1_MAP_TILES, copy: Vec::new() };
    psx.bus.add_hook(
        hook,
        Box::new(move |cpu, bus| {
            if bus.gpu.widescreen().is_none() {
                return HookAction::Continue;
            }
            let packet = cpu.regs[16];
            let (tile, col, cols) = window.read(cpu, bus);
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

    /// A camera pan standing on a limit the margin moved in counts as
    /// stopped there (its target may lie past it); anything else on a limit
    /// may move, as in the game's own function.
    #[test]
    fn a_pan_on_a_moved_limit_is_stopped() {
        const GP: u32 = 0x8001_0000;
        let mut psx = Psx::new(&vec![0u8; 512 * 1024], dwrc_psx::VideoStandard::Ntsc);
        let bus = &mut psx.bus;
        let axis = ScrollAxis { scroll: 0x8000_2000, size: 0x8000_2004, screen: 320, other: 0x10, low_base: 0x20, high_base: 0x24 };
        let (other, low, high) = (GP + 0x10, GP + 0x20, GP + 0x24);
        // A map of 5 tiles: the game's scroll runs 0..=320, at 16:9 53..=267.
        bus.poke8(axis.size, 5);
        bus.poke16(low, 320);
        bus.poke16(high, 0);
        let at = |bus: &mut dwrc_psx::Bus, scroll: i32, margin: i32, pan: bool| {
            bus.poke16(axis.scroll, scroll as u16);
            bus.poke32(other, 0x7777);
            let moves = axis.limit(bus, GP, margin, pan);
            (moves, bus.peek16(axis.scroll) as i16 as i32, bus.peek32(other) as i32)
        };
        assert_eq!(at(bus, 100, 53, true), (true, 100, 0x7777));
        // Past a limit: put back on it, for every caller.
        assert_eq!(at(bus, 50, 53, false), (false, 53, 267));
        assert_eq!(at(bus, 300, 53, true), (false, 267, 53));
        // On it: only a pan is stopped.
        assert_eq!(at(bus, 53, 53, false), (true, 53, 0x7777));
        assert_eq!(at(bus, 267, 53, false), (true, 267, 0x7777));
        assert_eq!(at(bus, 53, 53, true), (false, 53, 267));
        assert_eq!(at(bus, 267, 53, true), (false, 267, 53));
        // The game's own limits (no margin: the other axis, widescreen's 4:3 part) are the game's.
        assert_eq!(at(bus, 0, 0, true), (true, 0, 0x7777));
        assert_eq!(at(bus, 320, 0, true), (true, 320, 0x7777));
        // A map narrower than the wide picture (3 tiles: 0..=64) stays in its middle.
        bus.poke8(axis.size, 3);
        bus.poke16(high, 256);
        assert_eq!(at(bus, 32, 53, true), (false, 32, 288));
        assert_eq!(at(bus, 32, 53, false), (true, 32, 0x7777));
    }

    /// An executable of `words` at 0x80010000.
    fn exe_of(words: &[u32]) -> PsxExe {
        let text: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        PsxExe { pc: 0, gp: 0, load_addr: 0x8001_0000, text_size: text.len() as u32, bss_addr: 0, bss_size: 0, sp_base: 0, sp_offset: 0, region_marker: String::new(), text }
    }

    /// A pattern's words, each masked field holding the word's place in it.
    fn filled(pattern: &[(u32, u32)]) -> Vec<u32> {
        pattern.iter().enumerate().map(|(i, &(word, mask))| word | (i as u32 & !mask)).collect()
    }

    /// Both ways the executables have the scroll limits are found, with
    /// each address read from its own instruction and the pan's call told
    /// from the other two.
    #[test]
    fn scroll_limits_in_both_shapes() {
        let axis = |scroll: u32, size: u32, screen, other, low_base, high_base| ScrollAxis { scroll: (scroll << 16) + scroll + 1, size: (size << 16) + size + 1, screen, other, low_base, high_base };
        let want = [
            (&DW1_SCROLL_LIMITS[..], [axis(3, 14, 320, 11, 8, 22), axis(27, 38, 240, 35, 32, 46)]),
            (&DW1_SCROLL_LIMITS_EU_JP[..], [axis(8, 23, 320, 17, 15, 40), axis(44, 59, 240, 53, 51, 76)]),
        ];
        for (pattern, axes) in want {
            let at = 8;
            let jal = 0x0C00_0000 | (0x8001_0000u32 + 4 * at as u32) >> 2 & 0x03FF_FFFF;
            let mut words = vec![0u32; at];
            words[2] = jal;
            words.extend(filled(pattern));
            words.extend([0, jal, 0, 0, jal, 0]);
            let pan = 0x8001_0000 + 4 * (at + pattern.len() + 1 + 2) as u32;
            assert_eq!(find_scroll_limits(&exe_of(&words)), Some((0x8001_0020, axes, Some(pan))));
            // Callers laid out another way: the function is still found.
            words[2] = 0;
            assert_eq!(find_scroll_limits(&exe_of(&words)), Some((0x8001_0020, axes, None)));
        }
        assert_eq!(find_scroll_limits(&exe_of(&[0; 200])), None);
    }

    /// Both ways the executables have the field background loop are found:
    /// the hook's place, the two tables, and where the loop keeps its count.
    #[test]
    fn field_tiles_in_both_shapes() {
        // The check of whether a tile has a picture, `back` words before the pattern.
        let exe = |pattern: &[(u32, u32)], back: usize, add: u32| {
            let mut words = vec![0u32; back];
            words[..4].copy_from_slice(&[0x3C02_8015, 0x2442_F060, add, 0x8042_0000]);
            words.extend(filled(pattern));
            exe_of(&words)
        };
        let has = 0x8014_F060;
        let us = find_field_tiles(&exe(&DW1_FIELD_TILES, 66, 0x0052_1021));
        assert_eq!(us, Some(TileLoop { hook: 0x8001_0000 + 4 * (66 + 19), table: (13 << 16) + 15, has, window: TileWindow::Registers }));
        let eu = find_field_tiles(&exe(&DW1_FIELD_TILES_EU_JP, 138, 0x0043_1021));
        assert_eq!(eu, Some(TileLoop { hook: 0x8001_0000 + 4 * (138 + 21), table: (11 << 16) + 12, has, window: TileWindow::Stack { first: 48, width: 50, columns: 52 } }));
        // Without the check where it belongs, the loop is not taken.
        assert_eq!(find_field_tiles(&exe(&DW1_FIELD_TILES_EU_JP, 138, 0)), None);
    }

    /// The Europe and Japan loop's tile is worked out from the stack:
    /// first tile + map width × row + column.
    #[test]
    fn a_tile_window_on_the_stack() {
        let mut psx = Psx::new(&vec![0u8; 512 * 1024], dwrc_psx::VideoStandard::Ntsc);
        let sp = 0x8000_3000u32;
        for (off, v) in [(48u32, 8u16), (50, 6), (52, 4)] {
            psx.bus.poke16(sp + off, v);
        }
        psx.cpu.regs[29] = sp;
        (psx.cpu.regs[17], psx.cpu.regs[18]) = (3, 2);
        assert_eq!(TileWindow::Stack { first: 48, width: 50, columns: 52 }.read(&psx.cpu, &psx.bus), (8 + 6 * 2 + 3, 3, 4));
        (psx.cpu.regs[18], psx.cpu.regs[19], psx.cpu.regs[21]) = (23, 3, 4);
        assert_eq!(TileWindow::Registers.read(&psx.cpu, &psx.bus), (23, 3, 4));
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
