//! Widescreen: picture beside the console's own.
//!
//! The games draw 3D scenes with polygons that often reach past the edges of
//! the screen; the GPU clips them at the drawing area. With widescreen on,
//! primitives are drawn a second time, through the same pixel pipeline
//! (textures, shading, dithering, semi-transparency, mask bit), into a
//! host-side canvas that extends their drawing area by a margin on each side.
//!
//! Two ways of using it:
//!
//! * Strips only (the default): only primitives that cross the left or right
//!   edge go to the canvas, and only their parts beside the area. The picture
//!   between the strips always comes from VRAM, pixel for pixel.
//! * Widescreen UI ([`UiRules`]): every primitive goes to the canvas, which
//!   then holds the whole wide picture (uploads, copies and fills into the
//!   area are mirrored into it). Pieces of the game's UI (recognised by the
//!   texture pages the game keeps its UI pictures on) are drawn moved by the
//!   margin when they sit near the left or right edge, so they stay at the
//!   edges of the wider picture (when clearly on that side). Pieces that
//!   touch move together when drawn one after another (unless one lies
//!   inside the other) or when they overlap by no more than a sliver. A
//!   group inside another one's bounds and drawn after it (text on its
//!   panel) moves with the smallest such holder; HUD that a window is drawn
//!   over keeps its own place. A group near no edge right beside a moved
//!   one follows it. A group held at one edge that reaches well
//!   past the middle (a banner across the screen) is stretched instead: the
//!   opaque pieces of its left half move left, those of its right half
//!   right, and its middle, drawn in place, fills the gap between them; its
//!   letters move with the group's edge. The groups come from the pieces drawn in the
//!   last few pictures. A piece no group holds yet goes with the group it
//!   lies on or touches, or a moved piece beside it in the same picture, or
//!   where it went the last time it was seen (`Remembered`).
//!
//! None of this is part of the console: VRAM, the GPU's registers and the
//! game see exactly what a PlayStation does, and save states leave it out.
//! What the margins show is what the game sent past the edges. Things the
//! game skipped because they were off screen are missing there.
//!
//! An untextured rectangle, or a screen-aligned quad untextured or with a
//! texture of one colour, across the whole width of the area is a screen
//! layer (a backdrop colour, a fade) and is drawn across the margins too (in
//! `raster`).
//!
//! The margins are cleared when the game starts on a new picture in that
//! drawing area: when the display moves to another buffer (double
//! buffering), when a fill covers the whole width of the area, and next to
//! any upload or copy of at least 8×8 pixels that writes the area's edge
//! column (a movie frame, a pre-drawn background).

use super::{Gpu, VRAM_H, VRAM_W};

/// Drawing areas smaller than this are render targets, not screens.
const MIN_W: i32 = 256;
const MIN_H: i32 = 192;
/// Canvases kept at once (least recently drawn goes first).
const MAX_CANVASES: usize = 4;
/// UI within this many pixels of an edge stays at that edge.
const EDGE: i32 = 24;
/// A group this close to a moved one (and near no edge) moves with it...
const BESIDE: i32 = 8;
/// ...unless it is centred on the screen to within half this many pixels.
const CENTRED: i32 = 4;
/// UI pieces drawn in this many recent pictures make up the groups (a
/// picture: the frames in which something was drawn there; a game may
/// draw a new picture in a buffer only every few frames).
const WINDOW: u32 = 8;
/// Touching pieces this close in drawing order belong together...
const RUN: u64 = 4;
/// ...and so do ones drawn further apart that overlap by at most this many
/// pixels (a border's pieces; a wider overlap is one element over another),
/// unless one is small (a letter, at most 12×16: text sitting on another
/// element's edge belongs to what holds it, found by bounds).
const SLIVER: i64 = 16;

/// Pieces of UI seen before and which way their group moved (-1, 0, 1),
/// by the size of their drawing area and their box in it; kept across
/// pictures without them and save state loads (canvases are not).
type Remembered = std::collections::HashMap<(i32, i32, [i32; 4]), i8>;
/// Pieces remembered at most (then it starts over).
const REMEMBER: usize = 8192;

/// How a game draws its UI, for Widescreen UI.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UiRules {
    /// Where the UI's pictures and text are: (texture page, or `None` for
    /// any; first and last CLUT row, the VRAM line of the palette; the
    /// largest piece, width and height, or 0 for any size: a page a game
    /// shares between small UI pieces and big scenery; whether only
    /// unshaded pieces count: a page shared with Gouraud-shaded 3D models).
    /// Pages are GP0 texpage bits 0-4: x in units of 64 halfwords, bit 4 =
    /// the lower half of VRAM.
    pub pages: Vec<(Option<u8>, u16, u16, u16, bool)>,
    /// Untextured shapes (bar fills, panels) are UI, except screen layers.
    pub untextured: bool,
    /// Untextured flat-shaded polygons count among them (Digimon World 1
    /// draws its windows with rectangles and shaded quads, and the black
    /// edge of the "BATTLE" banner flying over the scene with flat quads).
    pub flat_polygons: bool,
    /// Insets are UI: drawing into a smaller area inside the picture (a
    /// view drawn in a window, like Digimon World 2003's partner camera), and
    /// copies into the picture from elsewhere in VRAM.
    pub insets: bool,
    /// Stretch a group held at one edge that reaches well past the middle
    /// (Digimon World 2003's Start menu banner) instead of moving it.
    pub stretch: bool,
}

pub(super) struct Canvas {
    x1: i32,
    y1: i32,
    x2: i32,
    y2: i32,
    /// Columns added on each side.
    margin: i32,
    /// (width + 2 * margin) x height pixels. Strips only: the middle is unused.
    px: Vec<u16>,
    used: u64,
    /// Widescreen UI: the middle is kept too, and the whole row is shown.
    full: bool,
    /// Widescreen UI may stretch groups (`UiRules::stretch`).
    stretch: bool,
    /// UI pieces drawn in recent pictures (area coordinates x0, x1, y0, y1),
    /// with the pictures since each was last drawn and when in drawing order.
    boxes: Vec<([i32; 4], u32, u64)>,
    /// UI pieces drawn so far (drawing order).
    seq: u64,
    /// Groups of touching UI pieces, how far each moves, and where the
    /// stretched group it belongs to splits, if any (the first column of
    /// its right half: a piece starting there or later goes right).
    groups: Vec<([i32; 4], i32, Option<i32>)>,
    /// UI pieces drawn since the groups were last made, and how far each moved.
    drawn: Vec<([i32; 4], i32)>,
}

impl Canvas {
    fn width(&self) -> i32 {
        self.x2 - self.x1 + 1
    }

    fn stride(&self) -> usize {
        (self.width() + 2 * self.margin) as usize
    }

    /// Clears the margins of rows `y0..=y1` (screen coordinates) to `color`.
    fn clear_rows(&mut self, y0: i32, y1: i32, left: bool, right: bool, color: u16) {
        let (stride, m, w) = (self.stride(), self.margin as usize, self.width() as usize);
        for y in y0.max(self.y1)..=y1.min(self.y2) {
            let row = &mut self.px[(y - self.y1) as usize * stride..][..stride];
            if left {
                row[..m].fill(color);
            }
            if right {
                row[m + w..].fill(color);
            }
        }
    }

    /// Copies VRAM `x, y, w, h` into the canvas, moved right by `dx`.
    /// Only VRAM columns of the area itself are taken: a margin's columns in
    /// VRAM belong to whatever lies beside the area (Digimon World 2's
    /// battle draws two buffers side by side).
    fn mirror(&mut self, vram: &[u16], x: i32, y: i32, w: i32, h: i32, dx: i32) {
        let stride = self.stride();
        for row in y.max(self.y1)..(y + h).min(self.y2 + 1) {
            let line = &vram[(row as usize & (VRAM_H - 1)) * VRAM_W..][..VRAM_W];
            let out = &mut self.px[(row - self.y1) as usize * stride..][..stride];
            for col in x.max(self.x1)..(x + w).min(self.x2 + 1) {
                let cx = col + dx - (self.x1 - self.margin);
                if cx >= 0 && (cx as usize) < stride {
                    out[cx as usize] = line[col as usize & (VRAM_W - 1)];
                }
            }
        }
    }

    /// Where to draw a UI piece (`b` in area coordinates), and notes it for
    /// the groups: moves, each with a column range (area coordinates) to clip
    /// to. A piece of a stretched group is drawn twice: moved to its half's
    /// edge, and in place clipped to the gap that opens in the middle. A
    /// letter or a semi-transparent piece is drawn once.
    fn ui_shift(&mut self, b: [i32; 4], semi: bool, seen: &Remembered) -> Draws {
        self.seq += 1;
        let seq = self.seq;
        match self.boxes.iter_mut().rev().find(|(x, _, _)| *x == b) {
            Some((_, age, s)) => (*age, *s) = (0, seq),
            None => self.boxes.push((b, 0, seq)),
        }
        // The innermost group holding the whole piece (its own group does).
        let m = self.margin;
        let (dx, split) = self
            .groups
            .iter()
            .find(|(g, _, _)| b[0] >= g[0] && b[1] <= g[1] && b[2] >= g[2] && b[3] <= g[3])
            .map(|(_, dx, s)| (*dx, *s))
            // A piece seen before, in pictures since forgotten (the field HUD
            // back after a battle or a talk, or after a save state load).
            .or_else(|| seen.get(&(self.width(), self.y2 - self.y1 + 1, b)).map(|&d| (d as i32 * m, None)))
            // A piece no group holds yet (an element drawn bigger than in the
            // last pictures, growing or sliding in): with the group it lies
            // on or touches most, if any.
            .or_else(|| {
                let touch = |g: &[i32; 4]| b[0] <= g[1] + 1 && g[0] <= b[1] + 1 && b[2] <= g[3] + 1 && g[2] <= b[3] + 1;
                let common = |g: &[i32; 4]| (b[1].min(g[1]) - b[0].max(g[0]) + 1).max(0) as i64 * (b[3].min(g[3]) - b[2].max(g[2]) + 1).max(0) as i64;
                self.groups.iter().filter(|(g, _, _)| touch(g)).max_by_key(|(g, _, _)| common(g)).map(|(_, dx, s)| (*dx, *s))
            })
            .unwrap_or_else(|| {
                let (dx, stretch) = anchor(b, self.width(), m);
                if dx != 0 {
                    return (dx, (stretch && self.stretch).then_some((b[0] + b[1]) / 2));
                }
                // A new element (no groups for it yet): beside a piece moved
                // earlier in this picture, with the biggest one (a bar's icon
                // drawn the first time with it), unless they move both ways.
                let near = |g: &[i32; 4]| b[0] <= g[1] + BESIDE && g[0] <= b[1] + BESIDE && b[2] <= g[3] + BESIDE && g[2] <= b[3] + BESIDE;
                let moved: Vec<&([i32; 4], i32)> = self.drawn.iter().filter(|(g, d)| *d != 0 && near(g)).collect();
                let both_ways = moved.iter().any(|m| m.1 < 0) && moved.iter().any(|m| m.1 > 0);
                let area = |g: &[i32; 4]| (g[1] - g[0] + 1) as i64 * (g[3] - g[2] + 1) as i64;
                let dx = if both_ways { 0 } else { moved.iter().max_by_key(|m| area(&m.0)).map_or(0, |m| m.1) };
                (dx, None)
            });
        self.drawn.push((b, dx));
        match split {
            Some(at) if !semi && !small(b) => {
                let side = if b[0] < at { -m } else { m };
                [Some((side, None)), Some((0, Some((at - m, at + m - 1))))]
            }
            _ => [Some((dx, None)), None],
        }
    }

    /// Vblank: the groups are made again from the recent pieces.
    fn regroup(&mut self, seen: &mut Remembered) {
        if !self.boxes.iter().any(|(_, age, _)| *age == 0) {
            return;
        }
        // Remember how the pieces of the last picture moved (unless stretched).
        if seen.len() > REMEMBER {
            seen.clear();
        }
        let size = (self.width(), self.y2 - self.y1 + 1);
        for (b, age, _) in &self.boxes {
            if *age == 0 {
                if let Some((_, dx, None)) = self.groups.iter().find(|(g, _, _)| b[0] >= g[0] && b[1] <= g[1] && b[2] >= g[2] && b[3] <= g[3]) {
                    seen.insert((size.0, size.1, *b), dx.signum() as i8);
                }
            }
        }
        self.drawn.clear();
        for (_, age, _) in &mut self.boxes {
            *age += 1;
        }
        self.boxes.retain(|(_, age, _)| *age <= WINDOW);
        let boxes: Vec<([i32; 4], u64)> = self.boxes.iter().map(|(b, _, s)| (*b, *s)).collect();
        let groups = anchored_groups(&group_boxes(&boxes), self.width(), self.margin);
        // A stretched group splits at the first of its pieces that starts at
        // or after its middle, so the gap between its halves is exactly the
        // width the in-place copy fills.
        let split = |span: [i32; 2]| -> i32 {
            let mid = (span[0] + span[1]) / 2;
            let g = groups.iter().find(|(g, _, _)| [g[0], g[1]] == span).map(|(g, _, _)| *g).unwrap_or([span[0], span[1], i32::MIN, i32::MAX]);
            boxes
                .iter()
                .map(|(b, _)| *b)
                .filter(|b| !small(*b) && b[0] >= g[0] && b[1] <= g[1] && b[2] >= g[2] && b[3] <= g[3] && b[0] >= mid)
                .map(|b| b[0])
                .min()
                .unwrap_or(mid)
        };
        let ok = self.stretch;
        self.groups = groups.iter().map(|&(g, dx, span)| (g, dx, span.filter(|_| ok).map(split))).collect();
    }
}

/// Whether box `p` lies mostly on box `q`, which is bigger: three quarters
/// of its area (a text strip drawn on a panel and reaching past its end,
/// where the strip is transparent: Digimon World 1's save slots).
fn on(p: [i32; 4], q: [i32; 4]) -> bool {
    let area = |b: [i32; 4]| (b[1] - b[0] + 1) as i64 * (b[3] - b[2] + 1) as i64;
    let common = (p[1].min(q[1]) - p[0].max(q[0]) + 1).max(0) as i64 * (p[3].min(q[3]) - p[2].max(q[2]) + 1).max(0) as i64;
    area(q) > area(p) && 4 * common >= 3 * area(p)
}

/// A letter or an icon: at most 12×16.
fn small(q: [i32; 4]) -> bool {
    q[1] - q[0] < 12 && q[3] - q[2] < 16
}

/// How far UI with bounds `b` moves: to the left edge's side when it is near
/// the left edge, to the right's when near the right, else not at all; and
/// whether it is stretched (held at one edge, reaching well past the middle).
fn anchor(b: [i32; 4], width: i32, margin: i32) -> (i32, bool) {
    // Clearly on one side: near that edge, and its gap there at least EDGE
    // smaller than on the other side (a nearly centred box stays).
    let (gl, gr) = (b[0], width - 1 - b[1]);
    let (left, right) = (b[0] <= EDGE && gr - gl >= EDGE, b[1] >= width - 1 - EDGE && gl - gr >= EDGE);
    match (left, right) {
        (true, false) => (-margin, b[1] > width / 2 + EDGE),
        (false, true) => (margin, b[0] < width / 2 - EDGE),
        _ => (0, false),
    }
}

/// Groups (bounds, first drawn) with their moves: a group inside another
/// one's bounds moves with it; the others by their own edges. A window
/// across the screen holds only what is drawn after it (on top of it), so
/// HUD it is drawn over (Digimon World 1's battle command window) keeps its
/// own place.
fn anchored_groups(items: &[([i32; 4], u64)], width: i32, margin: i32) -> Vec<([i32; 4], i32, Option<[i32; 2]>)> {
    let groups: Vec<[i32; 4]> = items.iter().map(|(g, _)| *g).collect();
    let first: std::collections::HashMap<[i32; 4], u64> = items.iter().copied().collect();
    let mut order: Vec<usize> = (0..groups.len()).collect();
    let size = |g: &[i32; 4]| (g[1] - g[0] + 1) as i64 * (g[3] - g[2] + 1) as i64;
    // A window across the screen (near both edges) only holds what is drawn
    // on top of it; a smaller panel holds its contents whatever the order
    // (Digimon World 2 draws some text before the panel behind it).
    let spans = |g: &[i32; 4]| g[0] <= EDGE && g[1] >= width - 1 - EDGE;
    order.sort_by_key(|&i| std::cmp::Reverse(size(&groups[i])));
    let mut out: Vec<([i32; 4], i32, Option<[i32; 2]>)> = Vec::with_capacity(groups.len());
    // For each placed group, the index of the group holding it.
    let mut held: Vec<Option<usize>> = Vec::with_capacity(groups.len());
    // Inside another group (2 pixels' leeway), or mostly (see `on`).
    let holds = |h: &[i32; 4], g: &[i32; 4]| (h[0] - 2 <= g[0] && g[1] <= h[1] + 2 && h[2] - 2 <= g[2] && g[3] <= h[3] + 2) || on(*g, *h);
    for i in order {
        let g = groups[i];
        // The smallest group already placed that holds this one.
        let holder = out
            .iter()
            .enumerate()
            .filter(|(_, (h, _, _))| holds(h, &g) && (!spans(h) || first[h] <= first[&g]))
            .min_by_key(|(_, (h, _, _))| size(h))
            .map(|(j, _)| j);
        let (dx, span) = match holder {
            Some(j) => (out[j].1, out[j].2),
            None => {
                let (dx, stretch) = anchor(g, width, margin);
                (dx, stretch.then_some([g[0], g[1]]))
            }
        };
        out.push((g, dx, span));
        held.push(holder);
    }
    // A group near neither edge that overlaps or sits beside a group being
    // moved (an icon sticking out of its bar, the HP box beside a name, a
    // count box beside its list) goes with the biggest such group, unless
    // neighbours move both ways (the middle one of a row of panels stays).
    let near_edge = |g: &[i32; 4]| g[0] <= EDGE || g[1] >= width - 1 - EDGE;
    let near = |a: &[i32; 4], b: &[i32; 4]| a[0] <= b[1] + BESIDE && b[0] <= a[1] + BESIDE && a[2] <= b[3] + BESIDE && b[2] <= a[3] + BESIDE;
    for i in 0..out.len() {
        let (g, dx, _) = out[i];
        // Contents follow their holder (below), not its neighbours; a group
        // centred on the screen keeps its place (the middle slot of a row,
        // like Digimon World 2's middle enemy or party panel).
        let centred = (g[0] + g[1] - (width - 1)).abs() <= CENTRED;
        if dx != 0 || near_edge(&g) || held[i].is_some() || centred {
            continue;
        }
        let near_moved: Vec<(i64, i32, Option<[i32; 2]>)> = out.iter().enumerate().filter(|&(j, (h, hdx, _))| j != i && *hdx != 0 && near(h, &g)).map(|(_, &(h, hdx, hs))| (size(&h), hdx, hs)).collect();
        let both_ways = near_moved.iter().any(|m| m.1 < 0) && near_moved.iter().any(|m| m.1 > 0);
        let follow = if both_ways { None } else { near_moved.iter().max_by_key(|m| m.0).map(|m| (m.1, m.2)) };
        if let Some((hdx, hs)) = follow {
            out[i].1 = hdx;
            out[i].2 = hs;
        }
    }
    // Contents take their holder's final move (holders come first: biggest first).
    for i in 0..out.len() {
        if let Some(j) = held[i] {
            out[i].1 = out[j].1;
            out[i].2 = out[j].2;
        }
    }
    // Smallest first, so a piece finds the innermost group holding it.
    out.reverse();
    out
}

/// Bounds of the groups of boxes that belong together (box, drawing order;
/// x0, x1, y0, y1 inclusive): touching, and drawn close together or
/// overlapping by no more than a sliver.
fn group_boxes(items: &[([i32; 4], u64)]) -> Vec<([i32; 4], u64)> {
    let boxes: Vec<[i32; 4]> = items.iter().map(|(b, _)| *b).collect();
    let n = boxes.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn root(p: &mut [usize], mut i: usize) -> usize {
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    for i in 0..n {
        for j in i + 1..n {
            let (a, b) = (boxes[i], boxes[j]);
            // Touching (overlapping or side by side).
            if !(a[0] <= b[1] + 1 && b[0] <= a[1] + 1 && a[2] <= b[3] + 1 && b[2] <= a[3] + 1) {
                continue;
            }
            let overlap = (a[1].min(b[1]) - a[0].max(b[0]) + 1).max(0) as i64 * (a[3].min(b[3]) - a[2].max(b[2]) + 1).max(0) as i64;
            // One inside the other is left to the holder rule (a window
            // drawn right after the HUD it covers is not part of it).
            let inside = |p: [i32; 4], q: [i32; 4]| (p[0] >= q[0] && p[1] <= q[1] && p[2] >= q[2] && p[3] <= q[3]) || on(p, q);
            let nested = inside(a, b) || inside(b, a);
            if (items[i].1.abs_diff(items[j].1) <= RUN && !nested) || (overlap <= SLIVER && !small(a) && !small(b)) {
                let (ri, rj) = (root(&mut parent, i), root(&mut parent, j));
                parent[ri] = rj;
            }
        }
    }
    let mut out: Vec<(usize, [i32; 4], u64)> = Vec::new();
    for (i, b) in boxes.iter().enumerate() {
        let r = root(&mut parent, i);
        match out.iter_mut().find(|(k, _, _)| *k == r) {
            Some((_, g, f)) => {
                *g = [g[0].min(b[0]), g[1].max(b[1]), g[2].min(b[2]), g[3].max(b[3])];
                *f = (*f).min(items[i].1);
            }
            None => out.push((r, *b, items[i].1)),
        }
    }
    out.into_iter().map(|(_, g, f)| (g, f)).collect()
}

/// Where the second pass of a primitive goes.
pub(super) struct Side {
    canvas: Canvas,
    /// Columns (VRAM coordinates) of the canvas `lo..=hi` and of the area `x1..=x2`.
    pub(super) lo: i32,
    pub(super) hi: i32,
    pub(super) x1: i32,
    pub(super) x2: i32,
    /// Draw across the whole canvas (Widescreen UI), not only the strips.
    pub(super) full: bool,
    /// `lo` and `hi` without a draw's own clip.
    lo0: i32,
    hi0: i32,
}

#[derive(Default)]
pub(super) struct Wide {
    /// The picture's width over a 4:3 picture's: num / den (4/3 for 16:9).
    num: u32,
    den: u32,
    /// Widescreen UI: how this game draws its UI.
    ui: Option<UiRules>,
    canvases: Vec<Canvas>,
    clock: u64,
    seen: Remembered,
}

/// How to draw a piece in the second pass: up to two (move, clip) pairs;
/// the clip is a column range, in area coordinates or (from `side_begin`)
/// VRAM coordinates.
pub(super) type Draws = [Option<(i32, Option<(i32, i32)>)>; 2];

/// Columns added on each side of a picture `width` pixels wide.
pub fn margin(width: u32, num: u32, den: u32) -> u32 {
    if num <= den {
        return 0;
    }
    (width * (num - den) + den) / (2 * den)
}

impl Gpu {
    /// Switches the side strips on with the picture `num / den` times as
    /// wide as 4:3 (4/3 for 16:9), or off with `None`.
    pub fn set_widescreen(&mut self, ratio: Option<(u32, u32)>) {
        let ui = self.widescreen_ui();
        self.wide = match ratio {
            Some((num, den)) if num > den => Some(Box::new(Wide { num, den, ui, ..Default::default() })),
            _ => None,
        };
    }

    /// How much wider than 4:3 the pictures are (`None`: off).
    pub fn widescreen(&self) -> Option<(u32, u32)> {
        self.wide.as_ref().map(|w| (w.num, w.den))
    }

    /// Widescreen UI on (with how the game draws its UI) or off. Takes
    /// effect while widescreen is on.
    pub fn set_widescreen_ui(&mut self, rules: Option<UiRules>) {
        if let Some(w) = &mut self.wide {
            if w.ui != rules {
                w.ui = rules;
                w.canvases.clear();
                w.seen.clear();
            }
        } else if let Some(r) = rules {
            // Kept for when widescreen comes on.
            self.wide = None;
            self.pending_ui = Some(r);
        } else {
            self.pending_ui = None;
        }
    }

    pub fn widescreen_ui(&self) -> Option<UiRules> {
        match &self.wide {
            Some(w) => w.ui.clone(),
            None => self.pending_ui.clone(),
        }
    }

    /// Forgets what the canvases hold (after a save state load or a reset).
    pub fn clear_widescreen(&mut self) {
        if let Some(w) = &mut self.wide {
            w.canvases.clear();
        }
    }

    /// Whether a primitive is part of the game's UI (Widescreen UI on).
    pub(super) fn is_ui(&self, textured: bool, shaded: bool, polygon: bool, layer: bool, clut: u32, size: (i32, i32)) -> bool {
        match self.wide.as_ref().and_then(|w| w.ui.as_ref()) {
            None => false,
            Some(_) if layer => false,
            Some(r) if textured => {
                let (page, row) = ((self.texpage & 0x1F) as u8, ((clut >> 6) & 0x1FF) as u16);
                let fits = |m: u16| m == 0 || (size.0 <= m as i32 && size.1 <= m as i32);
                r.pages.iter().any(|&(p, a, b, m, flat)| p.is_none_or(|p| p == page) && row >= a && row <= b && fits(m) && !(flat && shaded))
            }
            Some(r) => r.untextured && !layer && (r.flat_polygons || shaded || !polygon),
        }
    }

    /// Starts the second pass of a primitive spanning `xmin..=xmax`,
    /// `ymin..=ymax` (VRAM coordinates). Strips only: when it reaches past the
    /// drawing area's sides. Widescreen UI: always. Returns where to draw it
    /// (see `Draws`; set each one's clip with `side_clip`), or `None` when
    /// there is no second pass.
    pub(super) fn side_begin(&mut self, xmin: i32, xmax: i32, ymin: i32, ymax: i32, ui: bool, semi: bool) -> Option<Draws> {
        let (x1, y1, x2, y2) = (self.draw_x1, self.draw_y1, self.draw_x2, self.draw_y2);
        if y2 >= VRAM_H as i32 || x2 < x1 || y2 < y1 {
            return None;
        }
        let w = self.wide.as_mut()?;
        let full = w.ui.is_some();
        let stretch = w.ui.as_ref().is_some_and(|r| r.stretch);
        if x2 - x1 + 1 < MIN_W || y2 - y1 + 1 < MIN_H {
            // An inset: a smaller area inside a picture, drawn with it (moved as UI).
            if !w.ui.as_ref().is_some_and(|r| r.insets) {
                return None;
            }
            let i = w.canvases.iter().position(|c| c.full && x1 >= c.x1 && x2 <= c.x2 && y1 >= c.y1 && y2 <= c.y2)?;
            let mut canvas = w.canvases.swap_remove(i);
            let dx = canvas.ui_shift([x1 - canvas.x1, x2 - canvas.x1, y1 - canvas.y1, y2 - canvas.y1], true, &w.seen)[0].map_or(0, |d| d.0);
            self.side = Some(Side { canvas, lo: x1 + dx, hi: x2 + dx, x1: x1 + dx, x2: x2 + dx, full: true, lo0: x1 + dx, hi0: x2 + dx });
            if let Some(p) = self.trace.as_mut().and_then(|t| t.last_mut()) {
                p.shift = Some(dx);
            }
            return Some([Some((dx, None)), None]);
        }
        if !full && xmin >= x1 && xmax <= x2 {
            return None;
        }
        if xmax < x1 - (x2 - x1) || xmin > x2 + (x2 - x1) {
            return None;
        }
        w.clock += 1;
        let i = match w.canvases.iter().position(|c| (c.x1, c.y1, c.x2, c.y2) == (x1, y1, x2, y2)) {
            Some(i) => i,
            None => {
                if w.canvases.len() >= MAX_CANVASES {
                    let old = (0..w.canvases.len()).min_by_key(|&i| w.canvases[i].used).unwrap();
                    w.canvases.swap_remove(old);
                }
                let margin = margin((x2 - x1 + 1) as u32, w.num, w.den) as i32;
                let stride = (x2 - x1 + 1 + 2 * margin) as usize;
                let mut c = Canvas {
                    x1,
                    y1,
                    x2,
                    y2,
                    margin,
                    px: vec![0; stride * (y2 - y1 + 1) as usize],
                    used: 0,
                    full,
                    stretch,
                    boxes: Vec::new(),
                    seq: 0,
                    groups: Vec::new(),
                    drawn: Vec::new(),
                };
                if full {
                    // The picture drawn so far.
                    c.mirror(&self.vram, x1, y1, x2 - x1 + 1, y2 - y1 + 1, 0);
                }
                w.canvases.push(c);
                w.canvases.len() - 1
            }
        };
        let mut canvas = w.canvases.swap_remove(i);
        canvas.used = w.clock;
        let mut draws = if ui { canvas.ui_shift([xmin - x1, xmax - x1, ymin - y1, ymax - y1], semi, &w.seen) } else { [Some((0, None)), None] };
        for (_, clip) in draws.iter_mut().flatten() {
            if let Some((a, b)) = clip {
                *clip = Some((*a + x1, *b + x1));
            }
        }
        let m = canvas.margin;
        self.side = Some(Side { canvas, lo: x1 - m, hi: x2 + m, x1, x2, full, lo0: x1 - m, hi0: x2 + m });
        if let Some(p) = self.trace.as_mut().and_then(|t| t.last_mut()) {
            p.shift = draws[0].map(|d| d.0);
        }
        Some(draws)
    }

    /// Clips the second pass to columns `clip` (VRAM), or back to the canvas.
    pub(super) fn side_clip(&mut self, clip: Option<(i32, i32)>) {
        if let Some(s) = &mut self.side {
            (s.lo, s.hi) = match clip {
                Some((a, b)) => (a.max(s.lo0), b.min(s.hi0)),
                None => (s.lo0, s.hi0),
            };
        }
    }

    pub(super) fn side_end(&mut self) {
        if let (Some(s), Some(w)) = (self.side.take(), self.wide.as_mut()) {
            w.canvases.push(s.canvas);
        }
    }

    /// Writes a pixel of the second pass (`x` in the canvas, `y` in the area).
    #[inline]
    pub(super) fn plot_side(&mut self, x: i32, y: usize, mut pix: u16, semi: bool, blend: u32) {
        let (mask_check, mask_set) = (self.mask_check, self.mask_set);
        let Some(s) = self.side.as_mut() else { return };
        let c = &mut s.canvas;
        let idx = (y as i32 - c.y1) as usize * c.stride() + (x - (c.x1 - c.margin)) as usize;
        let Some(bg) = c.px.get(idx).copied() else { return };
        if mask_check && bg & 0x8000 != 0 {
            return;
        }
        if semi {
            pix = (pix & 0x8000) | super::raster::blend_pixels(bg, pix, blend);
        }
        c.px[idx] = pix | ((mask_set as u16) << 15);
    }

    /// The display moved to (x, y): the other buffers start a new picture.
    pub(super) fn wide_display_moved(&mut self, x: u32, y: u32) {
        let Some(w) = self.wide.as_mut() else { return };
        let (x, y) = (x as i32, y as i32);
        for c in &mut w.canvases {
            if !(x >= c.x1 && x <= c.x2 && y >= c.y1 && y <= c.y2) {
                let (y1, y2) = (c.y1, c.y2);
                c.clear_rows(y1, y2, true, true, 0);
            }
        }
    }

    /// Vblank: the UI groups are made again from the recent pieces.
    pub(super) fn wide_vblank(&mut self) {
        if let Some(w) = &mut self.wide {
            for c in w.canvases.iter_mut().filter(|c| c.full) {
                c.regroup(&mut w.seen);
            }
        }
    }

    /// A fill of VRAM (already done there): one covering an area's whole
    /// width fills its margins too; Widescreen UI copies it into the canvas.
    pub(super) fn wide_fill(&mut self, x: u32, y: u32, w: u32, h: u32, color: u16) {
        let Some(wd) = self.wide.as_mut() else { return };
        let (x0, x1, y0, y1) = (x as i32, (x + w) as i32 - 1, y as i32, (y + h) as i32 - 1);
        for c in &mut wd.canvases {
            if c.full {
                c.mirror(&self.vram, x0, y0, w as i32, h as i32, 0);
            }
            if x0 <= c.x1 && x1 >= c.x2 {
                c.clear_rows(y0, y1, true, true, color);
            }
        }
    }

    /// An upload or copy into VRAM is starting: the margin beside any edge
    /// column it writes is cleared. Small ones (under 8×8) are not a new picture.
    pub(super) fn wide_write(&mut self, x: u32, y: u32, w: u32, h: u32) {
        let Some(wd) = self.wide.as_mut() else { return };
        if w < 8 || h < 8 {
            return;
        }
        let (x0, x1, y0, y1) = (x as i32, (x + w) as i32 - 1, y as i32, (y + h) as i32 - 1);
        for c in &mut wd.canvases {
            let (left, right) = (x0 <= c.x1 && x1 >= c.x1, x0 <= c.x2 && x1 >= c.x2);
            if left || right {
                c.clear_rows(y0, y1, left, right, 0);
            }
        }
    }

    /// An upload or copy into VRAM is done. A copy from a picture (to
    /// another one: a game drawing in a back buffer and copying the result to
    /// the one it shows; or within one) copies the canvas too, strips
    /// included when it is the whole width, so moved UI stays moved. Otherwise Widescreen UI copies VRAM into the canvas; a
    /// copy from outside the area into it is UI when the game's rules say so.
    pub(super) fn wide_written(&mut self, x: u32, y: u32, w: u32, h: u32, from: Option<(u32, u32)>) {
        let Some(wd) = self.wide.as_mut() else { return };
        let copies_ui = wd.ui.as_ref().is_some_and(|r| r.insets);
        let (x, y, w, h) = (x as i32, y as i32, w as i32, h as i32);
        // Canvas to canvas.
        let mut done: Vec<usize> = Vec::new();
        if let Some((sx, sy)) = from {
            let (sx, sy) = (sx as i32, sy as i32);
            let src = wd.canvases.iter().position(|c| sx >= c.x1 && sx + w - 1 <= c.x2 && sy >= c.y1 && sy + h - 1 <= c.y2);
            if let Some(si) = src {
                let s = &wd.canvases[si];
                let whole = sx == s.x1 && w == s.width();
                let (stride, m) = (s.stride(), s.margin);
                let rows: Vec<Vec<u16>> = (0..h)
                    .map(|r| {
                        let line = &s.px[(sy + r - s.y1) as usize * stride..][..stride];
                        if whole { line.to_vec() } else { line[(sx - s.x1 + m) as usize..][..w as usize].to_vec() }
                    })
                    .collect();
                for (ci, c) in wd.canvases.iter_mut().enumerate() {
                    if x < c.x1 || x + w - 1 > c.x2 || y < c.y1 || y + h - 1 > c.y2 {
                        continue;
                    }
                    if whole && !(x == c.x1 && c.stride() == stride) {
                        continue;
                    }
                    let (cs, cm) = (c.stride(), c.margin);
                    for (r, row) in rows.iter().enumerate() {
                        let line = &mut c.px[(y + r as i32 - c.y1) as usize * cs..][..cs];
                        let at = if whole { 0 } else { (x - c.x1 + cm) as usize };
                        line[at..at + row.len()].copy_from_slice(row);
                    }
                    done.push(ci);
                }
            }
        }
        for (_, c) in wd.canvases.iter_mut().enumerate().filter(|(ci, c)| c.full && !done.contains(ci)) {
            if x > c.x2 || x + w <= c.x1 || y > c.y2 || y + h <= c.y1 {
                continue;
            }
            let outside = from.is_some_and(|(sx, sy)| {
                let (sx, sy) = (sx as i32, sy as i32);
                sx + w <= c.x1 || sx > c.x2 || sy + h <= c.y1 || sy > c.y2
            });
            let dx = if copies_ui && outside { c.ui_shift([x - c.x1, x + w - 1 - c.x1, y - c.y1, y + h - 1 - c.y1], true, &wd.seen)[0].map_or(0, |d| d.0) } else { 0 };
            c.mirror(&self.vram, x, y, w, h, dx);
        }
    }

    /// The canvas row beside (and, with Widescreen UI, under) the display at
    /// `x, y`, `width` wide with `margin` columns each side: (row pixels,
    /// whole row) per row, `None` without a matching canvas.
    pub(super) fn wide_row<'a>(&'a self, x: u32, y: u32, width: u32, margin: u32) -> Option<(bool, impl Fn(u32) -> &'a [u16] + 'a)> {
        let w = self.wide.as_ref()?;
        let c = w.canvases.iter().find(|c| c.x1 == x as i32 && c.width() == width as i32 && c.margin == margin as i32 && c.y1 <= y as i32 && y as i32 <= c.y2)?;
        let stride = c.stride();
        Some((c.full, move |row: u32| -> &'a [u16] {
            let r = (y + row) as i32 - c.y1;
            if r < 0 || r > c.y2 - c.y1 {
                return &[];
            }
            &c.px[r as usize * stride..][..stride]
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn margins() {
        assert_eq!(margin(320, 4, 3), 53);
        assert_eq!(margin(320, 6, 5), 32);
        assert_eq!(margin(320, 7, 4), 120);
        assert_eq!(margin(320, 1, 1), 0);
    }

    #[test]
    fn anchors() {
        // Near the left edge, near the right, across both, in the middle.
        assert_eq!(anchor([10, 100, 0, 20], 320, 53), (-53, false));
        assert_eq!(anchor([200, 310, 0, 20], 320, 53), (53, false));
        assert_eq!(anchor([0, 319, 200, 239], 320, 53), (0, false));
        // A nearly centred box reaching close to one edge stays.
        assert_eq!(anchor([37, 296, 160, 220], 320, 53), (0, false));
        assert_eq!(anchor([100, 200, 0, 20], 320, 53), (0, false));
        // A banner from the left half to the right edge: stretched.
        assert_eq!(anchor([34, 321, 13, 37], 320, 53), (53, true));
    }

    #[test]
    fn groups() {
        // A box of three touching pieces spanning the screen stays one group.
        let g = group_boxes(&[([0, 99, 0, 10], 1), ([100, 219, 0, 10], 2), ([220, 319, 0, 10], 3), ([10, 50, 100, 120], 4)]);
        assert_eq!(g.len(), 2);
        assert!(g.iter().any(|(b, _)| *b == [0, 319, 0, 10]));
        // Side by side but drawn far apart: still one (a border's pieces).
        assert_eq!(group_boxes(&[([0, 99, 0, 10], 1), ([100, 219, 0, 10], 50)]).len(), 1);
        // One laid widely over another, drawn far apart: two elements.
        assert_eq!(group_boxes(&[([66, 161, 13, 37], 1), ([76, 116, 17, 58], 30)]).len(), 2);
    }

    #[test]
    fn letters_on_an_edge_stay_with_their_panel() {
        // A banner (drawn first) and a row of letters sitting on its bottom edge, drawn much later.
        let g = group_boxes(&[([34, 321, 13, 37], 1), ([61, 68, 37, 44], 40)]);
        assert_eq!(g.len(), 2);
    }

    #[test]
    fn an_icon_sticking_out_of_a_bar_follows_it() {
        // A bar at the right edge and an icon (taller than the bar) overlapping its left end.
        let g = anchored_groups(&[([235, 310, 205, 213], 1), ([238, 249, 203, 226], 2)], 320, 53);
        assert!(g.iter().all(|(_, dx, _)| *dx == 53), "{g:?}");
    }

    #[test]
    fn a_box_beside_a_moved_one_follows_it() {
        // Name tags at the left edge, HP boxes 3 pixels to their right.
        let g = anchored_groups(&[([16, 100, 76, 92], 1), ([103, 197, 76, 108], 2)], 320, 53);
        assert!(g.iter().all(|(_, dx, _)| *dx == -53), "{g:?}");
    }

    #[test]
    fn a_window_drawn_right_after_the_hud_is_its_own_group() {
        let g = group_boxes(&[([17, 53, 20, 52], 1), ([12, 307, 19, 125], 2)]);
        assert_eq!(g.len(), 2);
    }

    #[test]
    fn a_centred_window_does_not_hold_the_hud_it_covers() {
        // A window across the screen, and a HUD icon at the left edge under it.
        // The HUD was drawn first, the window over it.
        let g = anchored_groups(&[([8, 311, 16, 125], 2), ([17, 53, 20, 52], 1)], 320, 53);
        let icon = g.iter().find(|(b, _, _)| b[0] == 17).unwrap();
        assert_eq!(icon.1, -53);
    }

    #[test]
    fn text_drawn_on_a_centred_box_stays_in_it() {
        // A message box across the screen, its text near the left edge, drawn after it.
        let g = anchored_groups(&[([12, 307, 190, 230], 1), ([20, 120, 196, 207], 2)], 320, 53);
        assert!(g.iter().all(|(_, dx, _)| *dx == 0), "{g:?}");
    }

    #[test]
    fn a_panel_holds_text_drawn_before_it() {
        let g = anchored_groups(&[([8, 115, 70, 130], 2), ([60, 110, 80, 92], 1)], 320, 53);
        assert!(g.iter().all(|(_, dx, _)| *dx == -53), "{g:?}");
    }

    #[test]
    fn the_middle_of_a_row_of_panels_stays() {
        let g = anchored_groups(&[([13, 109, 362, 460], 1), ([112, 208, 362, 460], 2), ([211, 307, 362, 460], 3)], 320, 53);
        let dx: Vec<i32> = [13, 112, 211].iter().map(|x| g.iter().find(|(b, _, _)| b[0] == *x).unwrap().1).collect();
        assert_eq!(dx, vec![-53, 0, 53]);
    }

    #[test]
    fn the_middle_panels_contents_stay_with_it() {
        let g = anchored_groups(
            &[([13, 109, 362, 460], 1), ([112, 208, 362, 460], 2), ([211, 307, 362, 460], 3), ([114, 146, 404, 426], 4), ([162, 205, 374, 388], 5)],
            320,
            53,
        );
        let dx = |x: i32| g.iter().find(|(b, _, _)| b[0] == x).unwrap().1;
        assert_eq!((dx(13), dx(112), dx(211), dx(114), dx(162)), (-53, 0, 53, 0, 0));
    }

    #[test]
    fn a_centred_panel_stays_beside_a_moved_one() {
        // Two enemy panels in the left and middle slots of a three-slot row.
        let g = anchored_groups(&[([13, 109, 24, 82], 1), ([112, 208, 24, 82], 2)], 320, 53);
        let dx = |x: i32| g.iter().find(|(b, _, _)| b[0] == x).unwrap().1;
        assert_eq!((dx(13), dx(112)), (-53, 0));
    }

    #[test]
    fn a_strip_mostly_on_its_panel_stays_with_it() {
        // Digimon World 1's save slot: a panel in the middle, and a text strip
        // drawn on it that reaches past its right end, close to the screen's edge.
        let items = [([51, 269, 44, 74], 1), ([88, 312, 60, 72], 2)];
        assert_eq!(group_boxes(&items).len(), 2);
        let g = anchored_groups(&items, 320, 53);
        assert!(g.iter().all(|(_, dx, _)| *dx == 0), "{g:?}");
    }

    #[test]
    fn text_moves_with_its_panel() {
        // A panel near the left edge, and its text (not near the edge) inside it.
        let g = anchored_groups(&[([8, 156, 15, 58], 1), ([60, 120, 20, 32], 2)], 320, 53);
        assert!(g.iter().all(|(_, dx, _)| *dx == -53));
    }
}
