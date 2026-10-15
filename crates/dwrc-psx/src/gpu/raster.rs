//! Software rasterizer reproducing the PlayStation GPU's drawing rules.
//!
//! Polygon edge stepping and attribute interpolation follow the fixed-point
//! scheme Mednafen derived from hardware tests (32.32 edge coordinates with
//! a sub-pixel bias, 12+12 bit attribute gradients), so coverage, gouraud
//! shading and texture coordinates match the console.

use super::{rgb24_to_15, sign_extend_11, Gpu, DITHER, VRAM_W};

const COORD_FBS: u32 = 12;
const COORD_POST_PADDING: u32 = 12;

#[derive(Clone, Copy, Default, Debug)]
struct Vertex {
    x: i32,
    y: i32,
    r: i32,
    g: i32,
    b: i32,
    u: i32,
    v: i32,
}

#[derive(Clone, Copy, Default)]
struct Deltas {
    du_dx: u32,
    dv_dx: u32,
    dr_dx: u32,
    dg_dx: u32,
    db_dx: u32,
    du_dy: u32,
    dv_dy: u32,
    dr_dy: u32,
    dg_dy: u32,
    db_dy: u32,
}

#[derive(Clone, Copy, Default)]
struct Group {
    u: u32,
    v: u32,
    r: u32,
    g: u32,
    b: u32,
}

impl Group {
    #[inline(always)]
    fn add_dx(&mut self, d: &Deltas, n: i32) {
        let n = n as u32;
        self.u = self.u.wrapping_add(d.du_dx.wrapping_mul(n));
        self.v = self.v.wrapping_add(d.dv_dx.wrapping_mul(n));
        self.r = self.r.wrapping_add(d.dr_dx.wrapping_mul(n));
        self.g = self.g.wrapping_add(d.dg_dx.wrapping_mul(n));
        self.b = self.b.wrapping_add(d.db_dx.wrapping_mul(n));
    }
    #[inline(always)]
    fn add_dy(&mut self, d: &Deltas, n: i32) {
        let n = n as u32;
        self.u = self.u.wrapping_add(d.du_dy.wrapping_mul(n));
        self.v = self.v.wrapping_add(d.dv_dy.wrapping_mul(n));
        self.r = self.r.wrapping_add(d.dr_dy.wrapping_mul(n));
        self.g = self.g.wrapping_add(d.dg_dy.wrapping_mul(n));
        self.b = self.b.wrapping_add(d.db_dy.wrapping_mul(n));
    }
}

#[inline(always)]
fn make_poly_xfp(x: i32) -> i64 {
    ((x as i64) << 32) + ((1i64 << 32) - (1 << 11))
}

#[inline(always)]
fn make_poly_xfp_step(dx: i32, dy: i32) -> i64 {
    let mut dx_ex = (dx as i64) << 32;
    if dx_ex < 0 {
        dx_ex -= dy as i64 - 1;
    }
    if dx_ex > 0 {
        dx_ex += dy as i64 - 1;
    }
    dx_ex / dy as i64
}

#[inline(always)]
fn xfp_int(x: i64) -> i32 {
    (x >> 32) as i32
}

/// Per-primitive drawing parameters.
#[derive(Clone, Copy)]
struct Prim {
    shaded: bool,
    textured: bool,
    semi: bool,
    raw: bool,
    dither: bool,
    /// Semi-transparency mode 0..3.
    blend: u32,
    /// Texture page base in VRAM.
    tp_x: u32,
    tp_y: u32,
    /// 0 = 4bpp, 1 = 8bpp, 2/3 = 15bpp
    depth: u32,
    clut_x: u32,
    clut_y: u32,
}

impl Gpu {
    fn decode_vertex(&self, w: u32) -> (i32, i32) {
        (sign_extend_11(w & 0x7FF), sign_extend_11((w >> 16) & 0x7FF))
    }

    fn prim_from_texpage(&self, tp: u32, clut: u32, shaded: bool, textured: bool, semi: bool, raw: bool) -> Prim {
        let tex_disabled = self.texpage & (1 << 11) != 0;
        let textured = textured && !tex_disabled;
        let dither_en = self.texpage & (1 << 9) != 0;
        Prim {
            shaded,
            textured,
            semi,
            raw,
            dither: dither_en && (shaded || (textured && !raw)),
            blend: (tp >> 5) & 3,
            tp_x: (tp & 0xF) * 64,
            tp_y: ((tp >> 4) & 1) * 256,
            depth: (tp >> 7) & 3,
            clut_x: (clut & 0x3F) * 16,
            clut_y: (clut >> 6) & 0x1FF,
        }
    }

    // ------------------------------------------------------------------
    // Polygons
    // ------------------------------------------------------------------

    pub(super) fn draw_polygon_cmd(&mut self) {
        let op = self.cmd[0] >> 24;
        let shaded = op & 0x10 != 0;
        let quad = op & 0x08 != 0;
        let textured = op & 0x04 != 0;
        let semi = op & 0x02 != 0;
        let raw = op & 0x01 != 0;
        let n = if quad { 4 } else { 3 };
        let mut verts = [Vertex::default(); 4];
        let mut idx = 0usize;
        let mut clut = 0u32;
        let mut tp = self.texpage;
        for (i, vtx) in verts.iter_mut().enumerate().take(n) {
            let color = if i == 0 || !shaded {
                if i == 0 {
                    idx += 1;
                }
                self.cmd[0]
            } else {
                let c = self.cmd[idx];
                idx += 1;
                c
            };
            let (x, y) = self.decode_vertex(self.cmd[idx]);
            idx += 1;
            vtx.x = x + self.off_x;
            vtx.y = y + self.off_y;
            vtx.r = (color & 0xFF) as i32;
            vtx.g = ((color >> 8) & 0xFF) as i32;
            vtx.b = ((color >> 16) & 0xFF) as i32;
            if textured {
                let uv = self.cmd[idx];
                idx += 1;
                vtx.u = (uv & 0xFF) as i32;
                vtx.v = ((uv >> 8) & 0xFF) as i32;
                if i == 0 {
                    clut = uv >> 16;
                } else if i == 1 {
                    tp = uv >> 16;
                }
            }
        }
        if textured {
            // Textured polygons update the texture page in GPUSTAT.
            self.texpage = (self.texpage & !0x9FF) | (tp & 0x9FF);
            if !self.tex_disable_allowed {
                self.texpage &= !(1 << 11);
            }
        }
        let prim = self.prim_from_texpage(self.texpage, clut, shaded, textured, semi, raw);
        let flat = if shaded { None } else { Some((verts[0].r, verts[0].g, verts[0].b)) };
        let _ = flat;
        self.draw_triangle([verts[0], verts[1], verts[2]], &prim);
        if quad {
            self.draw_triangle([verts[1], verts[2], verts[3]], &prim);
        }
    }

    fn calc_deltas(a: &Vertex, b: &Vertex, c: &Vertex, prim: &Prim) -> Option<Deltas> {
        let denom = (b.x - a.x) as i64 * (c.y - b.y) as i64 - (c.x - b.x) as i64 * (b.y - a.y) as i64;
        if denom == 0 {
            return None;
        }
        let mut d = Deltas::default();
        // CALCIS(attr, y) and CALCIS(x, attr)
        let dx = |fa: i32, fb: i32, fc: i32| -> u32 {
            let n = (fb - fa) as i64 * (c.y - b.y) as i64 - (fc - fb) as i64 * (b.y - a.y) as i64;
            (((n << COORD_FBS) / denom) << COORD_POST_PADDING) as u32
        };
        let dy = |fa: i32, fb: i32, fc: i32| -> u32 {
            let n = (b.x - a.x) as i64 * (fc - fb) as i64 - (c.x - b.x) as i64 * (fb - fa) as i64;
            (((n << COORD_FBS) / denom) << COORD_POST_PADDING) as u32
        };
        if prim.shaded {
            d.dr_dx = dx(a.r, b.r, c.r);
            d.dg_dx = dx(a.g, b.g, c.g);
            d.db_dx = dx(a.b, b.b, c.b);
            d.dr_dy = dy(a.r, b.r, c.r);
            d.dg_dy = dy(a.g, b.g, c.g);
            d.db_dy = dy(a.b, b.b, c.b);
        }
        if prim.textured {
            d.du_dx = dx(a.u, b.u, c.u);
            d.dv_dx = dx(a.v, b.v, c.v);
            d.du_dy = dy(a.u, b.u, c.u);
            d.dv_dy = dy(a.v, b.v, c.v);
        }
        Some(d)
    }

    fn draw_triangle(&mut self, mut v: [Vertex; 3], prim: &Prim) {
        // Determine the "core" vertex (left-most) before sorting by Y, as
        // the hardware does; attribute evaluation is anchored there.
        let mut cv: u32 = if v[1].x <= v[0].x {
            if v[2].x <= v[1].x {
                1 << 2
            } else {
                1 << 1
            }
        } else if v[2].x < v[0].x {
            1 << 2
        } else {
            1 << 0
        };
        if v[2].y < v[1].y {
            v.swap(2, 1);
            cv = ((cv >> 1) & 0x2) | ((cv << 1) & 0x4) | (cv & 0x1);
        }
        if v[1].y < v[0].y {
            v.swap(1, 0);
            cv = ((cv >> 1) & 0x1) | ((cv << 1) & 0x2) | (cv & 0x4);
        }
        if v[2].y < v[1].y {
            v.swap(2, 1);
            cv = ((cv >> 1) & 0x2) | ((cv << 1) & 0x4) | (cv & 0x1);
        }
        let core = (cv >> 1) as usize;

        if v[0].y == v[2].y {
            return;
        }
        if v[2].y - v[0].y >= 512 {
            return;
        }
        if (v[2].x - v[0].x).abs() >= 1024 || (v[2].x - v[1].x).abs() >= 1024 || (v[1].x - v[0].x).abs() >= 1024 {
            return;
        }
        let Some(d) = Self::calc_deltas(&v[0], &v[1], &v[2], prim) else { return };

        let half = 1u32 << (COORD_FBS - 1);
        let mut ig = Group::default();
        let c = &v[core];
        if prim.textured {
            ig.u = (((c.u as u32) << COORD_FBS) + half) << COORD_POST_PADDING;
            ig.v = (((c.v as u32) << COORD_FBS) + half) << COORD_POST_PADDING;
        }
        ig.r = (((c.r as u32) << COORD_FBS) + half) << COORD_POST_PADDING;
        ig.g = (((c.g as u32) << COORD_FBS) + half) << COORD_POST_PADDING;
        ig.b = (((c.b as u32) << COORD_FBS) + half) << COORD_POST_PADDING;
        ig.add_dx(&d, -c.x);
        ig.add_dy(&d, -c.y);

        let base_step = make_poly_xfp_step(v[2].x - v[0].x, v[2].y - v[0].y);
        let (bound_upper_step, right_facing) = if v[1].y == v[0].y {
            (0i64, v[1].x > v[0].x)
        } else {
            let s = make_poly_xfp_step(v[1].x - v[0].x, v[1].y - v[0].y);
            (s, s > base_step)
        };
        let bound_lower_step = if v[2].y == v[1].y { 0 } else { make_poly_xfp_step(v[2].x - v[1].x, v[2].y - v[1].y) };

        let mut base = make_poly_xfp(v[0].x);
        let mut bound = make_poly_xfp(v[0].x);
        for y in v[0].y..v[1].y {
            let (l, r) = if right_facing { (base, bound) } else { (bound, base) };
            self.draw_span(y, xfp_int(l), xfp_int(r), &ig, &d, prim);
            base += base_step;
            bound += bound_upper_step;
        }
        let mut bound = make_poly_xfp(v[1].x);
        for y in v[1].y..v[2].y {
            let (l, r) = if right_facing { (base, bound) } else { (bound, base) };
            self.draw_span(y, xfp_int(l), xfp_int(r), &ig, &d, prim);
            base += base_step;
            bound += bound_lower_step;
        }
    }

    #[inline]
    fn draw_span(&mut self, y: i32, x_start: i32, x_bound: i32, ig0: &Group, d: &Deltas, prim: &Prim) {
        if y < self.draw_y1 || y > self.draw_y2 {
            return;
        }
        if self.skip_line(y) {
            return;
        }
        let x0 = x_start.max(self.draw_x1);
        let x1 = x_bound.min(self.draw_x2 + 1);
        if x0 >= x1 {
            return;
        }
        let mut ig = *ig0;
        ig.add_dx(d, x0);
        ig.add_dy(d, y);
        let yy = (y & 0x1FF) as usize;
        let shift = COORD_FBS + COORD_POST_PADDING;
        for x in x0..x1 {
            let r = (ig.r >> shift) as i32;
            let g = (ig.g >> shift) as i32;
            let b = (ig.b >> shift) as i32;
            let u = ig.u >> shift;
            let v = ig.v >> shift;
            self.shade_pixel(x, yy, r, g, b, u, v, prim);
            ig.add_dx(d, 1);
        }
    }

    // ------------------------------------------------------------------
    // Pixel pipeline
    // ------------------------------------------------------------------

    #[inline(always)]
    fn dither_c(c: i32, x: i32, y: usize, dither: bool) -> u16 {
        let c = if dither { c + DITHER[y & 3][(x & 3) as usize] } else { c };
        (c.clamp(0, 255) >> 3) as u16
    }

    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    fn shade_pixel(&mut self, x: i32, y: usize, r: i32, g: i32, b: i32, u: u32, v: u32, prim: &Prim) {
        let pix = if prim.textured {
            let t = self.texel(u, v, prim);
            if t == 0 {
                return;
            }
            if prim.raw {
                t
            } else {
                let tr = (t & 0x1F) as i32;
                let tg = ((t >> 5) & 0x1F) as i32;
                let tb = ((t >> 10) & 0x1F) as i32;
                // (texel5 * colour8) / 16 in 8-bit space, then dither/quantise.
                let mr = Self::dither_c((tr * r) >> 4, x, y, prim.dither);
                let mg = Self::dither_c((tg * g) >> 4, x, y, prim.dither);
                let mb = Self::dither_c((tb * b) >> 4, x, y, prim.dither);
                (t & 0x8000) | mr | (mg << 5) | (mb << 10)
            }
        } else {
            let pr = Self::dither_c(r, x, y, prim.dither);
            let pg = Self::dither_c(g, x, y, prim.dither);
            let pb = Self::dither_c(b, x, y, prim.dither);
            pr | (pg << 5) | (pb << 10)
        };
        let semi = prim.semi && (!prim.textured || pix & 0x8000 != 0);
        self.plot(x as usize & (VRAM_W - 1), y, pix, semi, prim.blend);
    }

    #[inline(always)]
    fn plot(&mut self, x: usize, y: usize, mut pix: u16, semi: bool, blend: u32) {
        let idx = y * VRAM_W + x;
        let bg = self.vram[idx];
        if self.mask_check && bg & 0x8000 != 0 {
            return;
        }
        if semi {
            pix = (pix & 0x8000) | blend_pixels(bg, pix, blend);
        }
        self.vram[idx] = pix | ((self.mask_set as u16) << 15);
    }

    #[inline(always)]
    fn texel(&self, u: u32, v: u32, prim: &Prim) -> u16 {
        let tw = self.tex_window;
        let mask_x = (tw & 0x1F) * 8;
        let mask_y = ((tw >> 5) & 0x1F) * 8;
        let off_x = ((tw >> 10) & 0x1F) * 8;
        let off_y = ((tw >> 15) & 0x1F) * 8;
        let u = ((u & !mask_x) | (off_x & mask_x)) & 0xFF;
        let v = ((v & !mask_y) | (off_y & mask_y)) & 0xFF;
        let ty = ((prim.tp_y + v) & 0x1FF) as usize;
        match prim.depth {
            0 => {
                let w = self.vram[ty * VRAM_W + ((prim.tp_x + u / 4) & 0x3FF) as usize];
                let i = (w >> ((u & 3) * 4)) & 0xF;
                self.vram[prim.clut_y as usize * VRAM_W + ((prim.clut_x + i as u32) & 0x3FF) as usize]
            }
            1 => {
                let w = self.vram[ty * VRAM_W + ((prim.tp_x + u / 2) & 0x3FF) as usize];
                let i = (w >> ((u & 1) * 8)) & 0xFF;
                self.vram[prim.clut_y as usize * VRAM_W + ((prim.clut_x + i as u32) & 0x3FF) as usize]
            }
            _ => self.vram[ty * VRAM_W + ((prim.tp_x + u) & 0x3FF) as usize],
        }
    }

    // ------------------------------------------------------------------
    // Rectangles
    // ------------------------------------------------------------------

    pub(super) fn draw_rect_cmd(&mut self) {
        let op = self.cmd[0] >> 24;
        let textured = op & 0x04 != 0;
        let semi = op & 0x02 != 0;
        let raw = op & 0x01 != 0;
        let color = self.cmd[0] & 0xFF_FFFF;
        let (x, y) = self.decode_vertex(self.cmd[1]);
        let mut idx = 2;
        let (mut u0, mut v0, mut clut) = (0u32, 0u32, 0u32);
        if textured {
            let uv = self.cmd[idx];
            idx += 1;
            u0 = uv & 0xFF;
            v0 = (uv >> 8) & 0xFF;
            clut = uv >> 16;
        }
        let (w, h) = match (op >> 3) & 3 {
            0 => {
                let s = self.cmd[idx];
                ((s & 0x3FF) as i32, ((s >> 16) & 0x1FF) as i32)
            }
            1 => (1, 1),
            2 => (8, 8),
            _ => (16, 16),
        };
        let mut prim = self.prim_from_texpage(self.texpage, clut, false, textured, semi, raw);
        prim.dither = false; // rectangles are never dithered
        let r = (color & 0xFF) as i32;
        let g = ((color >> 8) & 0xFF) as i32;
        let b = ((color >> 16) & 0xFF) as i32;
        let x0 = x + self.off_x;
        let y0 = y + self.off_y;
        let ys = y0.max(self.draw_y1);
        let ye = (y0 + h).min(self.draw_y2 + 1);
        let xs = x0.max(self.draw_x1);
        let xe = (x0 + w).min(self.draw_x2 + 1);
        for py in ys..ye {
            if self.skip_line(py) {
                continue;
            }
            let dy = (py - y0) as u32;
            let v = if self.rect_flip_y { v0.wrapping_sub(dy) } else { v0.wrapping_add(dy) } & 0xFF;
            for px in xs..xe {
                let dx = (px - x0) as u32;
                let u = if self.rect_flip_x { u0.wrapping_sub(dx) } else { u0.wrapping_add(dx) } & 0xFF;
                self.shade_pixel(px, (py & 0x1FF) as usize, r, g, b, u, v, &prim);
            }
        }
    }

    // ------------------------------------------------------------------
    // Lines
    // ------------------------------------------------------------------

    pub(super) fn draw_line_cmd(&mut self) {
        let op = self.cmd[0] >> 24;
        let shaded = op & 0x10 != 0;
        let semi = op & 0x02 != 0;
        let c0 = self.cmd[0] & 0xFF_FFFF;
        if shaded {
            let (p0, c1, p1) = (self.cmd[1], self.cmd[2] & 0xFF_FFFF, self.cmd[3]);
            self.draw_line(p0, p1, c0, c1, true, semi);
        } else {
            let (p0, p1) = (self.cmd[1], self.cmd[2]);
            self.draw_line(p0, p1, c0, c0, false, semi);
        }
    }

    pub(super) fn draw_line(&mut self, w0: u32, w1: u32, c0: u32, c1: u32, shaded: bool, semi: bool) {
        let (x0, y0) = self.decode_vertex(w0);
        let (x1, y1) = self.decode_vertex(w1);
        let (x0, y0, x1, y1) = (x0 + self.off_x, y0 + self.off_y, x1 + self.off_x, y1 + self.off_y);
        let dx = (x1 - x0).abs();
        let dy = (y1 - y0).abs();
        if dx >= 1024 || dy >= 512 {
            return;
        }
        let dither = shaded && self.texpage & (1 << 9) != 0;
        let blend = (self.texpage >> 5) & 3;
        let col = |c: u32| ((c & 0xFF) as i32, ((c >> 8) & 0xFF) as i32, ((c >> 16) & 0xFF) as i32);
        // Draw from the left-most point (hardware swaps endpoints).
        let (mut ax, mut ay, mut bx, mut by, mut ca, mut cb) = (x0, y0, x1, y1, col(c0), col(c1));
        if ax > bx {
            std::mem::swap(&mut ax, &mut bx);
            std::mem::swap(&mut ay, &mut by);
            std::mem::swap(&mut ca, &mut cb);
        }
        let k = dx.max(dy);
        let div = |delta: i64| -> i64 {
            if k == 0 {
                return 0;
            }
            let mut d = delta << 32;
            if d < 0 {
                d -= k as i64 - 1;
            }
            if d > 0 {
                d += k as i64 - 1;
            }
            d / k as i64
        };
        let step_x = div((bx - ax) as i64);
        let step_y = div((by - ay) as i64);
        let mut cx = ((ax as i64) << 32) | (1i64 << 31);
        let mut cy = ((ay as i64) << 32) | (1i64 << 31);
        cx -= 1024;
        if step_y < 0 {
            cy -= 1024;
        }
        let cstep = |a: i32, b: i32| -> i32 { if k == 0 { 0 } else { ((b - a) << 12) / k } };
        let (sr, sg, sb) = (cstep(ca.0, cb.0), cstep(ca.1, cb.1), cstep(ca.2, cb.2));
        let (mut r, mut g, mut b) = ((ca.0 << 12) | (1 << 11), (ca.1 << 12) | (1 << 11), (ca.2 << 12) | (1 << 11));
        for _ in 0..=k {
            let px = sign_extend_11(((cx >> 32) as u32) & 0x7FF);
            let py = sign_extend_11(((cy >> 32) as u32) & 0x7FF);
            if px >= self.draw_x1 && px <= self.draw_x2 && py >= self.draw_y1 && py <= self.draw_y2 && !self.skip_line(py) {
                let yy = (py & 0x1FF) as usize;
                let pr = Self::dither_c(r >> 12, px, yy, dither);
                let pg = Self::dither_c(g >> 12, px, yy, dither);
                let pb = Self::dither_c(b >> 12, px, yy, dither);
                let pix = pr | (pg << 5) | (pb << 10);
                self.plot(px as usize & (VRAM_W - 1), yy, pix, semi, blend);
            }
            cx += step_x;
            cy += step_y;
            if shaded {
                r += sr;
                g += sg;
                b += sb;
            }
        }
        let _ = rgb24_to_15;
    }
}

/// Semi-transparency on 15-bit pixels.
#[inline(always)]
pub fn blend_pixels(bg: u16, fg: u16, mode: u32) -> u16 {
    let (br, bg_, bb) = ((bg & 0x1F) as i32, ((bg >> 5) & 0x1F) as i32, ((bg >> 10) & 0x1F) as i32);
    let (fr, fg_, fb) = ((fg & 0x1F) as i32, ((fg >> 5) & 0x1F) as i32, ((fg >> 10) & 0x1F) as i32);
    let f = |b: i32, f: i32| -> i32 {
        match mode {
            0 => (b + f) >> 1,
            1 => (b + f).min(31),
            2 => (b - f).max(0),
            _ => (b + (f >> 2)).min(31),
        }
    };
    (f(br, fr) | (f(bg_, fg_) << 5) | (f(bb, fb) << 10)) as u16
}
