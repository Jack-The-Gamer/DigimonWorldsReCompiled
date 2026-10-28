//! Pictures mods show over the game (`dwrc.show_image`).
//!
//! A mod names a PNG file from its own folder and a corner of the screen.
//! Positions and sizes are in pixels of a 320×240 grid laid over the game's
//! picture, whatever resolution the game draws at, so an image keeps its
//! place and size when the game changes video mode or the window is resized.
//! The launcher draws them on top of the picture; `dwrc-cli` paints them into
//! its screenshots.

use anyhow::{bail, Context, Result};
use std::path::Path;
use std::sync::Arc;

/// The grid positions and sizes are given on.
pub const GRID_W: f32 = 320.0;
pub const GRID_H: f32 = 240.0;
/// Largest image a mod may show (pixels per side), and its largest file.
pub const MAX_SIDE: u32 = 256;
pub const MAX_FILE: u64 = 1 << 20;
/// Images one mod may show at once.
pub const MAX_PER_MOD: usize = 16;

/// A decoded image: `width * height` RGBA pixels, not premultiplied.
#[derive(Debug, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Corner {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "top_left" => Corner::TopLeft,
            "top_right" => Corner::TopRight,
            "bottom_left" => Corner::BottomLeft,
            "bottom_right" => Corner::BottomRight,
            _ => return None,
        })
    }
}

/// One image on screen.
#[derive(Debug, Clone)]
pub struct Overlay {
    /// `<mod id>/<name>`: unique among all mods' images.
    pub key: String,
    pub image: Arc<Image>,
    pub corner: Corner,
    /// Distance from the corner's edges, in grid pixels.
    pub x: f32,
    pub y: f32,
    /// Grid pixels per image pixel.
    pub scale: f32,
}

impl Overlay {
    /// Where the image goes on the 320×240 grid: `[left, top, width, height]`.
    pub fn rect(&self) -> [f32; 4] {
        let (w, h) = (self.image.width as f32 * self.scale, self.image.height as f32 * self.scale);
        let left = match self.corner {
            Corner::TopLeft | Corner::BottomLeft => self.x,
            Corner::TopRight | Corner::BottomRight => GRID_W - self.x - w,
        };
        let top = match self.corner {
            Corner::TopLeft | Corner::TopRight => self.y,
            Corner::BottomLeft | Corner::BottomRight => GRID_H - self.y - h,
        };
        [left, top, w, h]
    }

    /// Which edge of the grid the image sits against, for widescreen with
    /// Widescreen UI (where it follows that edge out, like the game's UI):
    /// -1 left, 1 right, 0 neither (or both).
    pub fn edge(&self) -> i32 {
        const EDGE: f32 = 24.0;
        let [l, _, w, _] = self.rect();
        match (l <= EDGE, l + w >= GRID_W - EDGE) {
            (true, false) => -1,
            (false, true) => 1,
            _ => 0,
        }
    }

    /// Paints the image (nearest neighbour, alpha blended) onto a picture of
    /// `0xAARRGGBB` pixels (the alpha byte is kept).
    pub fn paint(&self, pixels: &mut [u32], width: u32, height: u32) {
        self.paint_within(pixels, width, height, 0, width);
    }

    /// Paints the image on the grid laid over columns `left..left + inner`
    /// of the frame (the console's picture inside a widescreen frame).
    pub fn paint_within(&self, pixels: &mut [u32], width: u32, height: u32, left: u32, inner: u32) {
        let [l, t, w, h] = self.rect();
        let (sx, sy) = (inner as f32 / GRID_W, height as f32 / GRID_H);
        let (x0, y0) = ((l * sx).round() as i64 + left as i64, (t * sy).round() as i64);
        let (x1, y1) = (((l + w) * sx).round() as i64 + left as i64, ((t + h) * sy).round() as i64);
        let img = &self.image;
        for y in y0.max(0)..y1.min(height as i64) {
            let iy = (((y - y0) as f32 + 0.5) / (y1 - y0) as f32 * img.height as f32) as u32;
            for x in x0.max(0)..x1.min(width as i64) {
                let ix = (((x - x0) as f32 + 0.5) / (x1 - x0) as f32 * img.width as f32) as u32;
                let s = &img.rgba[((iy.min(img.height - 1) * img.width + ix.min(img.width - 1)) * 4) as usize..][..4];
                let a = s[3] as u32;
                let d = &mut pixels[(y as u32 * width + x as u32) as usize];
                let mix = |c: u32, sh: u32| ((c * a + ((*d >> sh) & 0xFF) * (255 - a) + 127) / 255) << sh;
                *d = (*d & 0xFF00_0000) | mix(s[0] as u32, 16) | mix(s[1] as u32, 8) | mix(s[2] as u32, 0);
            }
        }
    }
}

/// Reads a PNG (any colour type and depth) as RGBA.
pub fn load_png(path: &Path) -> Result<Image> {
    let len = std::fs::metadata(path).with_context(|| format!("{}", path.display()))?.len();
    if len > MAX_FILE {
        bail!("{} is larger than {} KiB", path.display(), MAX_FILE >> 10);
    }
    let mut dec = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path)?));
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let info = dec.read_header_info().with_context(|| format!("{} is not a PNG image", path.display()))?;
    let (width, height) = (info.width, info.height);
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        bail!("{} is {width}×{height}; images may be at most {MAX_SIDE}×{MAX_SIDE}", path.display());
    }
    let mut reader = dec.read_info()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let frame = reader.next_frame(&mut buf)?;
    let px = &buf[..frame.buffer_size()];
    let rgba = match frame.color_type {
        png::ColorType::Rgba => px.to_vec(),
        png::ColorType::Rgb => px.chunks(3).flat_map(|c| [c[0], c[1], c[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => px.chunks(2).flat_map(|c| [c[0], c[0], c[0], c[1]]).collect(),
        png::ColorType::Grayscale => px.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => bail!("{}: indexed colour was not expanded", path.display()),
    };
    Ok(Image { width, height, rgba })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_and_painting() {
        // A 2×2 image: opaque red, transparent, half-transparent white, opaque blue.
        let image = Arc::new(Image { width: 2, height: 2, rgba: vec![255, 0, 0, 255, 0, 0, 0, 0, 255, 255, 255, 128, 0, 0, 255, 255] });
        let o = Overlay { key: "m/x".into(), image, corner: Corner::TopRight, x: 4.0, y: 2.0, scale: 3.0 };
        assert_eq!(o.rect(), [310.0, 2.0, 6.0, 6.0]);
        let bottom = Overlay { corner: Corner::BottomLeft, ..o.clone() };
        assert_eq!(bottom.rect(), [4.0, 232.0, 6.0, 6.0]);
        // On a 640×480 picture every grid pixel is 2×2.
        let mut px = vec![0u32; 640 * 480];
        o.paint(&mut px, 640, 480);
        let at = |x: usize, y: usize| px[y * 640 + x];
        assert_eq!(at(620, 4), 0xFF0000);
        assert_eq!(at(631, 4), 0);
        assert_eq!(at(620, 15), 0x808080);
        assert_eq!(at(631, 15), 0x0000FF);
        assert_eq!(at(619, 4), 0);
        assert_eq!(at(632, 16), 0);
        assert_eq!(px.iter().filter(|&&p| p != 0).count(), 6 * 6 * 4 * 3 / 4);
    }

    #[test]
    fn reads_pngs() {
        let d = std::env::temp_dir().join(format!("dwrc-overlay-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let write = |name: &str, w: u32, h: u32, color: png::ColorType, data: &[u8]| {
            let f = std::fs::File::create(d.join(name)).unwrap();
            let mut e = png::Encoder::new(f, w, h);
            e.set_color(color);
            e.set_depth(png::BitDepth::Eight);
            e.write_header().unwrap().write_image_data(data).unwrap();
        };
        write("rgb.png", 2, 1, png::ColorType::Rgb, &[1, 2, 3, 4, 5, 6]);
        write("ga.png", 1, 1, png::ColorType::GrayscaleAlpha, &[9, 99]);
        write("big.png", 257, 1, png::ColorType::Grayscale, &[0; 257]);
        assert_eq!(load_png(&d.join("rgb.png")).unwrap(), Image { width: 2, height: 1, rgba: vec![1, 2, 3, 255, 4, 5, 6, 255] });
        assert_eq!(load_png(&d.join("ga.png")).unwrap().rgba, vec![9, 9, 9, 99]);
        assert!(load_png(&d.join("big.png")).is_err());
        std::fs::write(d.join("text.png"), "not a picture").unwrap();
        assert!(load_png(&d.join("text.png")).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
