//! GPU: GP0/GP1 command processing, VRAM, video timing and display output.
//! Drawing primitives live in [`raster`].

mod raster;

use crate::irq::{Interrupt, Irq};
use crate::timers::TimerGpuInfo;
use crate::VideoStandard;

pub const VRAM_W: usize = 1024;
pub const VRAM_H: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gp0State {
    /// Waiting for / collecting a command.
    Command,
    /// Receiving pixel data for a CPU->VRAM transfer.
    Upload { x: u32, y: u32, w: u32, h: u32, pos: u32 },
    /// Collecting polyline vertices until the terminator.
    Polyline { shaded: bool },
}

#[derive(Debug, Clone, Copy)]
struct Download {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    pos: u32,
}

/// Information the frontend needs to present a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayInfo {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub bpp24: bool,
    pub interlaced: bool,
    pub enabled: bool,
    pub pal: bool,
}

pub struct Gpu {
    pub standard: VideoStandard,
    pub vram: Box<[u16]>,

    // GP0 state
    state: Gp0State,
    cmd: Vec<u32>,
    cmd_len: usize,
    download: Option<Download>,
    read_latch: u32,

    // Drawing environment
    pub texpage: u32,
    pub tex_window: u32,
    pub draw_x1: i32,
    pub draw_y1: i32,
    pub draw_x2: i32,
    pub draw_y2: i32,
    pub off_x: i32,
    pub off_y: i32,
    pub mask_set: bool,
    pub mask_check: bool,
    pub rect_flip_x: bool,
    pub rect_flip_y: bool,
    tex_disable_allowed: bool,
    // Raw E-register values for GP1(10h)
    e2: u32,
    e3: u32,
    e4: u32,
    e5: u32,

    // Display
    pub display_x: u32,
    pub display_y: u32,
    pub h_start: u32,
    pub h_end: u32,
    pub v_start: u32,
    pub v_end: u32,
    pub display_mode: u32,
    pub display_disabled: bool,
    dma_dir: u32,
    irq_flag: bool,

    // Timing
    video_clock: u64,
    line_len: u64,
    lines: u32,
    /// Position inside the current line in units of 1/CPU_CLOCK GPU cycles.
    pos: u64,
    last_sync: u64,
    pub line: u32,
    pub field: bool,
    in_vblank: bool,
    pub hblank_count: u64,
    pub frames: u64,
    /// Display parameters latched at the start of vblank (what was shown).
    pub latched: DisplayInfo,
}

const DITHER: [[i32; 4]; 4] = [[-4, 0, -3, 1], [2, -2, 3, -1], [-3, 1, -4, 0], [3, -1, 2, -2]];

impl Gpu {
    pub fn new(standard: VideoStandard) -> Self {
        let mut g = Self {
            standard,
            vram: vec![0u16; VRAM_W * VRAM_H].into_boxed_slice(),
            state: Gp0State::Command,
            cmd: Vec::with_capacity(16),
            cmd_len: 0,
            download: None,
            read_latch: 0,
            texpage: 0,
            tex_window: 0,
            draw_x1: 0,
            draw_y1: 0,
            draw_x2: 0,
            draw_y2: 0,
            off_x: 0,
            off_y: 0,
            mask_set: false,
            mask_check: false,
            rect_flip_x: false,
            rect_flip_y: false,
            tex_disable_allowed: false,
            e2: 0,
            e3: 0,
            e4: 0,
            e5: 0,
            display_x: 0,
            display_y: 0,
            h_start: 0x200,
            h_end: 0xC00,
            v_start: 0x10,
            v_end: 0x100,
            display_mode: 0,
            display_disabled: true,
            dma_dir: 0,
            irq_flag: false,
            video_clock: 0,
            line_len: 0,
            lines: 0,
            pos: 0,
            last_sync: 0,
            line: 0,
            field: false,
            in_vblank: false,
            hblank_count: 0,
            frames: 0,
            latched: DisplayInfo { x: 0, y: 0, width: 320, height: 240, bpp24: false, interlaced: false, enabled: false, pal: false },
        };
        g.set_standard(standard);
        g.soft_reset();
        g
    }

    fn set_standard(&mut self, standard: VideoStandard) {
        self.standard = standard;
        let (clock, len, lines) = match standard {
            VideoStandard::Ntsc => (53_693_175u64, 3413u64, 263u32),
            VideoStandard::Pal => (53_203_425u64, 3406u64, 314u32),
        };
        self.video_clock = clock;
        self.line_len = len * crate::CPU_CLOCK;
        self.lines = lines;
        if standard == VideoStandard::Pal {
            self.display_mode |= 1 << 3;
        }
    }

    fn soft_reset(&mut self) {
        self.state = Gp0State::Command;
        self.cmd.clear();
        self.download = None;
        self.irq_flag = false;
        self.display_disabled = true;
        self.dma_dir = 0;
        self.display_x = 0;
        self.display_y = 0;
        self.h_start = 0x200;
        self.h_end = 0xC00;
        self.v_start = 0x10;
        self.v_end = 0x100;
        // GP1(08h) = 0, but keep the console's video standard.
        self.display_mode = if self.standard == VideoStandard::Pal { 1 << 3 } else { 0 };
        if self.standard == VideoStandard::Pal {
            self.v_start = 0x23;
            self.v_end = 0x123;
        }
        for c in 0xE1..=0xE6u32 {
            self.gp0_env(c << 24);
        }
    }

    // ------------------------------------------------------------------
    // Timing
    // ------------------------------------------------------------------

    pub fn is_pal_mode(&self) -> bool {
        self.display_mode & (1 << 3) != 0
    }

    fn interlaced(&self) -> bool {
        self.display_mode & (1 << 5) != 0
    }

    fn vres480(&self) -> bool {
        self.display_mode & (1 << 2) != 0 && self.interlaced()
    }

    pub fn dot_divider(&self) -> u64 {
        if self.display_mode & (1 << 6) != 0 {
            7
        } else {
            match self.display_mode & 3 {
                0 => 10,
                1 => 8,
                2 => 5,
                _ => 4,
            }
        }
    }

    pub fn in_vblank(&self) -> bool {
        self.in_vblank
    }

    pub fn timer_info(&self) -> TimerGpuInfo {
        TimerGpuInfo { video_clock: self.video_clock, dot_divider: self.dot_divider(), in_vblank: self.in_vblank }
    }

    fn vblank_start_line(&self) -> u32 {
        self.v_end.min(self.lines).max(self.v_start.min(self.lines - 1) + 1)
    }

    /// Advances video timing to `now`. Returns true when a frame ended
    /// (vblank started).
    pub fn sync(&mut self, now: u64, irq: &mut Irq) -> bool {
        let elapsed = now.saturating_sub(self.last_sync);
        self.last_sync = now;
        self.pos += elapsed * self.video_clock;
        let mut frame_done = false;
        while self.pos >= self.line_len {
            self.pos -= self.line_len;
            self.line += 1;
            self.hblank_count += 1;
            if self.line >= self.lines {
                self.line = 0;
                if self.interlaced() {
                    self.field = !self.field;
                } else {
                    self.field = false;
                }
            }
            let vb_start = self.vblank_start_line();
            let vb_end = self.v_start.min(self.lines - 1);
            if self.line == vb_start {
                self.in_vblank = true;
                irq.raise(Interrupt::VBlank);
                self.latched = self.display_info();
                self.frames += 1;
                frame_done = true;
            } else if self.line == vb_end {
                self.in_vblank = false;
            }
        }
        frame_done
    }

    /// CPU cycle of the next line boundary.
    pub fn next_event_cycle(&self, now: u64) -> u64 {
        let remaining = self.line_len - self.pos;
        now + remaining.div_ceil(self.video_clock).max(1)
    }

    fn odd_line_bit(&self) -> bool {
        if self.in_vblank {
            return false;
        }
        if self.vres480() {
            self.field
        } else {
            self.line & 1 != 0
        }
    }

    /// Lines of the currently displayed field are not drawn in 480i
    /// unless "draw to display area" is enabled.
    #[inline]
    fn skip_line(&self, y: i32) -> bool {
        self.vres480() && self.texpage & (1 << 10) == 0 && ((y & 1) != 0) == self.odd_line_bit()
    }

    // ------------------------------------------------------------------
    // Registers
    // ------------------------------------------------------------------

    pub fn status(&self) -> u32 {
        let mut s = self.texpage & 0x7FF;
        s |= (self.mask_set as u32) << 11;
        s |= (self.mask_check as u32) << 12;
        let field = if self.interlaced() { self.field } else { true };
        s |= (field as u32) << 13;
        s |= ((self.display_mode >> 7) & 1) << 14;
        s |= ((self.texpage >> 11) & 1) << 15;
        s |= ((self.display_mode >> 6) & 1) << 16;
        s |= (self.display_mode & 3) << 17;
        s |= ((self.display_mode >> 2) & 1) << 19;
        s |= ((self.display_mode >> 3) & 1) << 20;
        s |= ((self.display_mode >> 4) & 1) << 21;
        s |= ((self.display_mode >> 5) & 1) << 22;
        s |= (self.display_disabled as u32) << 23;
        s |= (self.irq_flag as u32) << 24;
        let idle = matches!(self.state, Gp0State::Command) && self.cmd.is_empty();
        let ready_cmd = idle;
        let ready_read = self.download.is_some();
        let ready_dma = idle || matches!(self.state, Gp0State::Upload { .. });
        s |= (ready_cmd as u32) << 26;
        s |= (ready_read as u32) << 27;
        s |= (ready_dma as u32) << 28;
        s |= self.dma_dir << 29;
        let req = match self.dma_dir {
            0 => false,
            1 => true,
            2 => ready_dma,
            _ => ready_read,
        };
        s |= (req as u32) << 25;
        s |= (self.odd_line_bit() as u32) << 31;
        s
    }

    pub fn read_data(&mut self) -> u32 {
        if let Some(mut d) = self.download {
            let mut v = 0u32;
            for i in 0..2 {
                let px = d.pos % d.w;
                let py = d.pos / d.w;
                let x = (d.x + px) as usize & (VRAM_W - 1);
                let y = (d.y + py) as usize & (VRAM_H - 1);
                v |= (self.vram[y * VRAM_W + x] as u32) << (16 * i);
                d.pos += 1;
                if d.pos >= d.w * d.h {
                    break;
                }
            }
            if d.pos >= d.w * d.h {
                self.download = None;
            } else {
                self.download = Some(d);
            }
            self.read_latch = v;
        }
        self.read_latch
    }

    pub fn gp1(&mut self, v: u32, _irq: &mut Irq) {
        let op = v >> 24;
        match op {
            0x00 => self.soft_reset(),
            0x01 => {
                self.state = Gp0State::Command;
                self.cmd.clear();
            }
            0x02 => self.irq_flag = false,
            0x03 => self.display_disabled = v & 1 != 0,
            0x04 => self.dma_dir = v & 3,
            0x05 => {
                self.display_x = v & 0x3FE;
                self.display_y = (v >> 10) & 0x1FF;
            }
            0x06 => {
                self.h_start = v & 0xFFF;
                self.h_end = (v >> 12) & 0xFFF;
            }
            0x07 => {
                self.v_start = v & 0x3FF;
                self.v_end = (v >> 10) & 0x3FF;
            }
            0x08 => {
                self.display_mode = (v & 0x3F) | (((v >> 6) & 1) << 6) | (((v >> 7) & 1) << 7);
                let pal = self.display_mode & (1 << 3) != 0;
                let std = if pal { VideoStandard::Pal } else { VideoStandard::Ntsc };
                if std != self.standard {
                    let mode = self.display_mode;
                    self.set_standard(std);
                    self.display_mode = mode;
                    if self.line >= self.lines {
                        self.line = 0;
                    }
                }
            }
            0x09 => self.tex_disable_allowed = v & 1 != 0,
            0x10..=0x1F => {
                self.read_latch = match v & 7 {
                    2 => self.e2 & 0xFFFFF,
                    3 => self.e3 & 0x7FFFF,
                    4 => self.e4 & 0x7FFFF,
                    5 => self.e5 & 0x3FFFFF,
                    7 => 2,
                    _ => self.read_latch,
                };
            }
            _ => log::debug!("unhandled GP1 {v:08x}"),
        }
    }

    // ------------------------------------------------------------------
    // GP0
    // ------------------------------------------------------------------

    fn command_length(op: u32) -> usize {
        match op {
            0x02 => 3,
            0x20..=0x3F => {
                let quad = op & 0x08 != 0;
                let shaded = op & 0x10 != 0;
                let textured = op & 0x04 != 0;
                let n = if quad { 4 } else { 3 };
                let per = 1 + textured as usize + shaded as usize;
                1 + n * per - shaded as usize
            }
            0x40..=0x5F => {
                if op & 0x08 != 0 {
                    // Polyline: first segment, rest streamed.
                    if op & 0x10 != 0 {
                        4
                    } else {
                        3
                    }
                } else if op & 0x10 != 0 {
                    4
                } else {
                    3
                }
            }
            0x60..=0x7F => {
                let textured = op & 0x04 != 0;
                let variable = (op >> 3) & 3 == 0;
                2 + textured as usize + variable as usize
            }
            0x80..=0x9F => 4,
            0xA0..=0xBF => 3,
            0xC0..=0xDF => 3,
            _ => 1,
        }
    }

    pub fn gp0(&mut self, v: u32) {
        match self.state {
            Gp0State::Upload { x, y, w, h, pos } => {
                self.upload_pixels(v, x, y, w, h, pos);
                return;
            }
            Gp0State::Polyline { shaded } => {
                self.polyline_word(v, shaded);
                return;
            }
            Gp0State::Command => {}
        }
        if self.cmd.is_empty() {
            let op = v >> 24;
            self.cmd_len = Self::command_length(op);
        }
        self.cmd.push(v);
        if self.cmd.len() >= self.cmd_len {
            self.execute();
        }
    }

    fn execute(&mut self) {
        let op = self.cmd[0] >> 24;
        match op {
            0x00 | 0x03..=0x1E | 0xE0 | 0xE7..=0xFF => {}
            0x01 => {} // clear texture cache
            0x02 => self.fill_rect(),
            0x1F => self.irq_flag = true,
            0x20..=0x3F => self.draw_polygon_cmd(),
            0x40..=0x5F => {
                let shaded = op & 0x10 != 0;
                if op & 0x08 != 0 {
                    // Polyline: draw the first segment, then stream.
                    self.draw_line_cmd();
                    self.state = Gp0State::Polyline { shaded };
                    // Keep the last vertex (and colour) for the next segment.
                    let last_color = if shaded { self.cmd[2] } else { self.cmd[0] };
                    let last_vtx = *self.cmd.last().unwrap();
                    self.cmd.clear();
                    self.cmd.push(self.cmd_header_op_color(op, last_color));
                    self.cmd.push(last_vtx);
                    return;
                }
                self.draw_line_cmd();
            }
            0x60..=0x7F => self.draw_rect_cmd(),
            0x80..=0x9F => self.copy_vram(),
            0xA0..=0xBF => {
                let x = self.cmd[1] & 0x3FF;
                let y = (self.cmd[1] >> 16) & 0x1FF;
                let w = ((self.cmd[2] & 0xFFFF).wrapping_sub(1) & 0x3FF) + 1;
                let h = (((self.cmd[2] >> 16) & 0xFFFF).wrapping_sub(1) & 0x1FF) + 1;
                self.state = Gp0State::Upload { x, y, w, h, pos: 0 };
            }
            0xC0..=0xDF => {
                let x = self.cmd[1] & 0x3FF;
                let y = (self.cmd[1] >> 16) & 0x1FF;
                let w = ((self.cmd[2] & 0xFFFF).wrapping_sub(1) & 0x3FF) + 1;
                let h = (((self.cmd[2] >> 16) & 0xFFFF).wrapping_sub(1) & 0x1FF) + 1;
                self.download = Some(Download { x, y, w, h, pos: 0 });
            }
            0xE1..=0xE6 => self.gp0_env(self.cmd[0]),
            _ => {}
        }
        self.cmd.clear();
    }

    fn cmd_header_op_color(&self, op: u32, color: u32) -> u32 {
        (op << 24) | (color & 0xFF_FFFF)
    }

    fn polyline_word(&mut self, v: u32, shaded: bool) {
        if v & 0xF000_F000 == 0x5000_5000 {
            self.state = Gp0State::Command;
            self.cmd.clear();
            return;
        }
        // cmd holds [op|color0, v0]; collect [color1,] v1
        self.cmd.push(v);
        let need = if shaded { 4 } else { 3 };
        if self.cmd.len() >= need {
            let op = self.cmd[0] >> 24;
            if shaded {
                // cmd = [c0, v0, c1, v1]
                let c0 = self.cmd[0] & 0xFF_FFFF;
                let p0 = self.cmd[1];
                let c1 = self.cmd[2] & 0xFF_FFFF;
                let p1 = self.cmd[3];
                self.draw_line(p0, p1, c0, c1, true, op & 0x02 != 0);
                self.cmd.clear();
                self.cmd.push((op << 24) | c1);
                self.cmd.push(p1);
            } else {
                let c0 = self.cmd[0] & 0xFF_FFFF;
                let p0 = self.cmd[1];
                let p1 = self.cmd[2];
                self.draw_line(p0, p1, c0, c0, false, op & 0x02 != 0);
                self.cmd.clear();
                self.cmd.push((op << 24) | c0);
                self.cmd.push(p1);
            }
        }
    }

    fn gp0_env(&mut self, v: u32) {
        match v >> 24 {
            0xE1 => {
                self.texpage = (self.texpage & !0x87FF) | (v & 0x7FF);
                if self.tex_disable_allowed {
                    self.texpage = (self.texpage & !(1 << 11)) | (v & (1 << 11));
                }
                self.rect_flip_x = v & (1 << 12) != 0;
                self.rect_flip_y = v & (1 << 13) != 0;
            }
            0xE2 => {
                self.tex_window = v & 0xFFFFF;
                self.e2 = v;
            }
            0xE3 => {
                self.draw_x1 = (v & 0x3FF) as i32;
                self.draw_y1 = ((v >> 10) & 0x1FF) as i32;
                self.e3 = v;
            }
            0xE4 => {
                self.draw_x2 = (v & 0x3FF) as i32;
                self.draw_y2 = ((v >> 10) & 0x1FF) as i32;
                self.e4 = v;
            }
            0xE5 => {
                self.off_x = sign_extend_11(v & 0x7FF);
                self.off_y = sign_extend_11((v >> 11) & 0x7FF);
                self.e5 = v;
            }
            0xE6 => {
                self.mask_set = v & 1 != 0;
                self.mask_check = v & 2 != 0;
            }
            _ => {}
        }
    }

    fn upload_pixels(&mut self, v: u32, x: u32, y: u32, w: u32, h: u32, mut pos: u32) {
        let total = w * h;
        for i in 0..2 {
            if pos >= total {
                break;
            }
            let px = (x + pos % w) as usize & (VRAM_W - 1);
            let py = (y + pos / w) as usize & (VRAM_H - 1);
            let pix = (v >> (16 * i)) as u16;
            let idx = py * VRAM_W + px;
            if !(self.mask_check && self.vram[idx] & 0x8000 != 0) {
                self.vram[idx] = pix | ((self.mask_set as u16) << 15);
            }
            pos += 1;
        }
        self.state = if pos >= total { Gp0State::Command } else { Gp0State::Upload { x, y, w, h, pos } };
    }

    fn fill_rect(&mut self) {
        let c = self.cmd[0];
        let color = rgb24_to_15(c);
        let x0 = self.cmd[1] & 0x3F0;
        let y0 = (self.cmd[1] >> 16) & 0x1FF;
        let w = ((self.cmd[2] & 0x3FF) + 0xF) & !0xF;
        let h = (self.cmd[2] >> 16) & 0x1FF;
        for dy in 0..h {
            let y = ((y0 + dy) & 0x1FF) as i32;
            if self.skip_line(y) {
                continue;
            }
            for dx in 0..w {
                let x = ((x0 + dx) & 0x3FF) as usize;
                self.vram[y as usize * VRAM_W + x] = color;
            }
        }
    }

    fn copy_vram(&mut self) {
        let sx = self.cmd[1] & 0x3FF;
        let sy = (self.cmd[1] >> 16) & 0x1FF;
        let dx = self.cmd[2] & 0x3FF;
        let dy = (self.cmd[2] >> 16) & 0x1FF;
        let w = ((self.cmd[3] & 0xFFFF).wrapping_sub(1) & 0x3FF) + 1;
        let h = (((self.cmd[3] >> 16) & 0xFFFF).wrapping_sub(1) & 0x1FF) + 1;
        let mut line = vec![0u16; w as usize];
        for y in 0..h {
            let srow = ((sy + y) & 0x1FF) as usize * VRAM_W;
            for x in 0..w {
                line[x as usize] = self.vram[srow + ((sx + x) & 0x3FF) as usize];
            }
            let drow = ((dy + y) & 0x1FF) as usize * VRAM_W;
            for x in 0..w {
                let idx = drow + ((dx + x) & 0x3FF) as usize;
                if self.mask_check && self.vram[idx] & 0x8000 != 0 {
                    continue;
                }
                self.vram[idx] = line[x as usize] | ((self.mask_set as u16) << 15);
            }
        }
    }

    // ------------------------------------------------------------------
    // Display output
    // ------------------------------------------------------------------

    pub fn display_info(&self) -> DisplayInfo {
        let div = self.dot_divider() as u32;
        let hw = self.h_end.saturating_sub(self.h_start);
        let mut width = ((hw / div) + 2) & !3;
        let hres = match (self.display_mode >> 6) & 1 {
            1 => 368,
            _ => [256, 320, 512, 640][(self.display_mode & 3) as usize],
        };
        if width == 0 || width > hres {
            width = hres;
        }
        let mut height = self.v_end.saturating_sub(self.v_start).min(if self.is_pal_mode() { 288 } else { 240 });
        if height == 0 {
            height = if self.is_pal_mode() { 256 } else { 240 };
        }
        if self.vres480() {
            height *= 2;
        }
        DisplayInfo {
            x: self.display_x,
            y: self.display_y,
            width,
            height,
            bpp24: self.display_mode & (1 << 4) != 0,
            interlaced: self.vres480(),
            enabled: !self.display_disabled,
            pal: self.is_pal_mode(),
        }
    }

    /// Renders the latched display area as 0xAARRGGBB pixels.
    pub fn render_display(&self, out: &mut Vec<u32>) -> (u32, u32) {
        let d = self.latched;
        out.clear();
        out.resize((d.width * d.height) as usize, 0xFF00_0000);
        if !d.enabled {
            return (d.width, d.height);
        }
        for row in 0..d.height {
            let vy = ((d.y + row) & 0x1FF) as usize;
            let line = &self.vram[vy * VRAM_W..vy * VRAM_W + VRAM_W];
            let o = &mut out[(row * d.width) as usize..((row + 1) * d.width) as usize];
            if d.bpp24 {
                let base = (d.x as usize) * 2;
                for (col, px) in o.iter_mut().enumerate() {
                    let b0 = base + col * 3;
                    let byte = |i: usize| {
                        let w = line[(i / 2) & (VRAM_W - 1)];
                        if i & 1 == 0 {
                            (w & 0xFF) as u32
                        } else {
                            (w >> 8) as u32
                        }
                    };
                    let (r, g, b) = (byte(b0), byte(b0 + 1), byte(b0 + 2));
                    *px = 0xFF00_0000 | (r << 16) | (g << 8) | b;
                }
            } else {
                for (col, px) in o.iter_mut().enumerate() {
                    let c = line[(d.x as usize + col) & (VRAM_W - 1)];
                    *px = rgb15_to_argb(c);
                }
            }
        }
        (d.width, d.height)
    }
}

#[inline]
pub fn sign_extend_11(v: u32) -> i32 {
    ((v << 21) as i32) >> 21
}

#[inline]
pub fn rgb24_to_15(c: u32) -> u16 {
    let r = (c >> 3) & 0x1F;
    let g = (c >> 11) & 0x1F;
    let b = (c >> 19) & 0x1F;
    (r | (g << 5) | (b << 10)) as u16
}

#[inline]
pub fn rgb15_to_argb(c: u16) -> u32 {
    let r = (c & 0x1F) as u32;
    let g = ((c >> 5) & 0x1F) as u32;
    let b = ((c >> 10) & 0x1F) as u32;
    // Expand 5-bit to 8-bit the way a DAC would (replicate high bits).
    let x = |v: u32| (v << 3) | (v >> 2);
    0xFF00_0000 | (x(r) << 16) | (x(g) << 8) | x(b)
}
