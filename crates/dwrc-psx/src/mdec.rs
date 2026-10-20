//! Motion Decoder (MDEC): run-length/quantisation decoding, IDCT and
//! YCbCr->RGB conversion used by the games' FMV (STR) playback.

use std::collections::VecDeque;

const ZIGZAG: [usize; 64] = [
    0, 1, 5, 6, 14, 15, 27, 28, 2, 4, 7, 13, 16, 26, 29, 42, 3, 8, 12, 17, 25, 30, 41, 43, 9, 11, 18, 24, 31, 40, 44,
    53, 10, 19, 23, 32, 39, 45, 52, 54, 20, 22, 33, 38, 46, 51, 55, 60, 21, 34, 37, 47, 50, 56, 59, 61, 35, 36, 48, 49,
    57, 58, 62, 63,
];

fn zagzig() -> &'static [usize; 64] {
    static T: std::sync::OnceLock<[usize; 64]> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let mut t = [0usize; 64];
        for (pos, &k) in ZIGZAG.iter().enumerate() {
            t[k] = pos;
        }
        t
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cmd {
    None,
    Decode,
    QuantTable { color: bool },
    ScaleTable,
}

pub struct Mdec {
    cmd: Cmd,
    remaining: u32,
    depth: u32,
    signed: bool,
    set_bit15: bool,
    enable_in: bool,
    enable_out: bool,
    input: Vec<u16>,
    in_pos: usize,
    params: Vec<u32>,
    pub qt_luma: [u8; 64],
    pub qt_chroma: [u8; 64],
    pub scale: [i16; 64],
    output: VecDeque<u32>,
    current_block: u32,
}

impl Default for Mdec {
    fn default() -> Self {
        Self::new()
    }
}

fn sext10(v: u16) -> i32 {
    ((v as i32) << 22) >> 22
}

impl Mdec {
    pub fn new() -> Self {
        Self {
            cmd: Cmd::None,
            remaining: 0,
            depth: 0,
            signed: false,
            set_bit15: false,
            enable_in: false,
            enable_out: false,
            input: Vec::new(),
            in_pos: 0,
            params: Vec::new(),
            qt_luma: [1; 64],
            qt_chroma: [1; 64],
            scale: [0; 64],
            output: VecDeque::new(),
            current_block: 4,
        }
    }

    pub fn status(&self) -> u32 {
        let mut s = 0u32;
        s |= (self.output.is_empty() as u32) << 31;
        s |= ((self.cmd != Cmd::None) as u32) << 29;
        s |= ((self.enable_in && self.cmd != Cmd::None) as u32) << 28;
        s |= ((self.enable_out && !self.output.is_empty()) as u32) << 27;
        s |= self.depth << 25;
        s |= (self.signed as u32) << 24;
        s |= (self.set_bit15 as u32) << 23;
        s |= (self.current_block & 7) << 16;
        s |= self.remaining.wrapping_sub(1) & 0xFFFF;
        s
    }

    pub fn write_control(&mut self, v: u32) {
        if v & (1 << 31) != 0 {
            self.cmd = Cmd::None;
            self.remaining = 0;
            self.output.clear();
            self.input.clear();
            self.in_pos = 0;
            self.params.clear();
            self.current_block = 4;
        }
        self.enable_in = v & (1 << 30) != 0;
        self.enable_out = v & (1 << 29) != 0;
    }

    pub fn output_ready(&self) -> bool {
        !self.output.is_empty()
    }

    pub fn output_len(&self) -> usize {
        self.output.len()
    }

    pub fn read_data(&mut self) -> u32 {
        self.output.pop_front().unwrap_or(0)
    }

    pub fn write_command(&mut self, v: u32) {
        if self.cmd == Cmd::None {
            let op = v >> 29;
            self.depth = (v >> 27) & 3;
            self.signed = v & (1 << 26) != 0;
            self.set_bit15 = v & (1 << 25) != 0;
            match op {
                1 => {
                    self.cmd = Cmd::Decode;
                    self.remaining = v & 0xFFFF;
                    self.input.clear();
                    self.in_pos = 0;
                }
                2 => {
                    let color = v & 1 != 0;
                    self.cmd = Cmd::QuantTable { color };
                    self.remaining = if color { 32 } else { 16 };
                    self.params.clear();
                }
                3 => {
                    self.cmd = Cmd::ScaleTable;
                    self.remaining = 32;
                    self.params.clear();
                }
                _ => {
                    self.remaining = v & 0xFFFF;
                    if self.remaining > 0 {
                        // Unknown commands still swallow their parameters.
                        self.cmd = Cmd::QuantTable { color: false };
                        self.remaining = 0;
                        self.cmd = Cmd::None;
                    }
                }
            }
            if self.remaining == 0 {
                self.cmd = Cmd::None;
            }
            return;
        }
        match self.cmd {
            Cmd::Decode => {
                self.input.push(v as u16);
                self.input.push((v >> 16) as u16);
                self.remaining -= 1;
                self.decode_available(self.remaining == 0);
                if self.remaining == 0 {
                    self.cmd = Cmd::None;
                }
            }
            Cmd::QuantTable { color } => {
                self.params.push(v);
                self.remaining -= 1;
                if self.remaining == 0 {
                    let bytes: Vec<u8> = self.params.iter().flat_map(|w| w.to_le_bytes()).collect();
                    self.qt_luma.copy_from_slice(&bytes[..64]);
                    if color {
                        self.qt_chroma.copy_from_slice(&bytes[64..128]);
                    }
                    self.cmd = Cmd::None;
                }
            }
            Cmd::ScaleTable => {
                self.params.push(v);
                self.remaining -= 1;
                if self.remaining == 0 {
                    for (i, w) in self.params.iter().enumerate() {
                        self.scale[i * 2] = *w as i16;
                        self.scale[i * 2 + 1] = (*w >> 16) as i16;
                    }
                    self.cmd = Cmd::None;
                }
            }
            Cmd::None => {}
        }
    }

    /// Decodes a block from `input[pos..]`. Returns the next position, or
    /// None if the block is incomplete.
    fn rl_decode(&self, pos: usize, qt: &[u8; 64], out: &mut [i32; 64]) -> Option<usize> {
        let mut p = pos;
        let inp = &self.input;
        // Skip padding.
        while p < inp.len() && inp[p] == 0xFE00 {
            p += 1;
        }
        if p >= inp.len() {
            return None;
        }
        out.fill(0);
        let mut n = inp[p];
        p += 1;
        let q_scale = ((n >> 10) & 0x3F) as i32;
        let mut k = 0usize;
        let mut val = sext10(n & 0x3FF) * qt[0] as i32;
        let zz = zagzig();
        loop {
            if q_scale == 0 {
                val = sext10(n & 0x3FF) * 2;
            }
            let v = val.clamp(-0x400, 0x3FF);
            if q_scale > 0 {
                out[zz[k]] = v;
            } else {
                out[k] = v;
            }
            if p >= inp.len() {
                return None;
            }
            n = inp[p];
            p += 1;
            k += ((n >> 10) & 0x3F) as usize + 1;
            if k > 63 {
                break;
            }
            val = (sext10(n & 0x3FF) * qt[k] as i32 * q_scale + 4) / 8;
        }
        Some(p)
    }

    fn idct(&self, blk: &mut [i32; 64]) {
        let mut tmp = [0i64; 64];
        // Pass 1: columns
        for x in 0..8 {
            for y in 0..8 {
                let mut sum = 0i64;
                for z in 0..8 {
                    sum += blk[y + z * 8] as i64 * self.scale[x + z * 8] as i64;
                }
                tmp[x + y * 8] = (sum + 0x8000) >> 16;
            }
        }
        // Pass 2: rows
        for x in 0..8 {
            for y in 0..8 {
                let mut sum = 0i64;
                for z in 0..8 {
                    sum += tmp[y + z * 8] * self.scale[x + z * 8] as i64;
                }
                blk[x + y * 8] = ((sum + 0x8000) >> 16) as i32;
            }
        }
    }

    fn decode_available(&mut self, finished: bool) {
        loop {
            let start = self.in_pos;
            if self.depth >= 2 {
                // Colour macroblock: Cr, Cb, Y0..Y3
                let mut blocks = [[0i32; 64]; 6];
                let mut p = start;
                let mut ok = true;
                for (i, b) in blocks.iter_mut().enumerate() {
                    let qt = if i < 2 { self.qt_chroma } else { self.qt_luma };
                    match self.rl_decode(p, &qt, b) {
                        Some(np) => p = np,
                        None => {
                            ok = false;
                            break;
                        }
                    }
                }
                if !ok {
                    break;
                }
                self.in_pos = p;
                for b in blocks.iter_mut() {
                    self.idct(b);
                }
                self.emit_color(&blocks);
            } else {
                let mut b = [0i32; 64];
                let qt = self.qt_luma;
                match self.rl_decode(start, &qt, &mut b) {
                    Some(np) => self.in_pos = np,
                    None => break,
                }
                self.idct(&mut b);
                self.emit_mono(&b);
            }
        }
        if finished {
            self.input.clear();
            self.in_pos = 0;
        }
    }

    fn emit_color(&mut self, blocks: &[[i32; 64]; 6]) {
        let (cr, cb) = (&blocks[0], &blocks[1]);
        let mut rgb = [[0u8; 3]; 256];
        for (bi, (xx, yy)) in [(0, 0), (8, 0), (0, 8), (8, 8)].into_iter().enumerate() {
            let yblk = &blocks[2 + bi];
            for y in 0..8 {
                for x in 0..8 {
                    let ci = ((x + xx) / 2) + ((y + yy) / 2) * 8;
                    let r_c = cr[ci];
                    let b_c = cb[ci];
                    let g = (-88 * b_c - 183 * r_c + 0x80) >> 8;
                    let r = (359 * r_c + 0x80) >> 8;
                    let b = (454 * b_c + 0x80) >> 8;
                    let yv = yblk[x + y * 8];
                    let mut px = [
                        (yv + r).clamp(-128, 127),
                        (yv + g).clamp(-128, 127),
                        (yv + b).clamp(-128, 127),
                    ];
                    if !self.signed {
                        for c in px.iter_mut() {
                            *c += 128;
                        }
                    }
                    rgb[(x + xx) + (y + yy) * 16] = [px[0] as u8, px[1] as u8, px[2] as u8];
                }
            }
        }
        if self.depth == 2 {
            // 24-bit: pack bytes R,G,B,R,G,B...
            let mut bytes = Vec::with_capacity(768);
            for p in rgb.iter() {
                bytes.extend_from_slice(p);
            }
            for c in bytes.chunks(4) {
                self.output.push_back(u32::from_le_bytes([c[0], c[1], c[2], c[3]]));
            }
        } else {
            let b15 = (self.set_bit15 as u32) << 15;
            for pair in rgb.chunks(2) {
                let conv = |p: [u8; 3]| -> u32 {
                    ((p[0] as u32 >> 3) | ((p[1] as u32 >> 3) << 5) | ((p[2] as u32 >> 3) << 10)) | b15
                };
                self.output.push_back(conv(pair[0]) | (conv(pair[1]) << 16));
            }
        }
    }

    fn emit_mono(&mut self, b: &[i32; 64]) {
        let px: Vec<u8> = b
            .iter()
            .map(|&v| {
                let v = v.clamp(-128, 127);
                if self.signed {
                    v as i8 as u8
                } else {
                    (v + 128) as u8
                }
            })
            .collect();
        if self.depth == 1 {
            for c in px.chunks(4) {
                self.output.push_back(u32::from_le_bytes([c[0], c[1], c[2], c[3]]));
            }
        } else {
            for c in px.chunks(8) {
                let mut w = 0u32;
                for (i, v) in c.iter().enumerate() {
                    w |= ((*v as u32) >> 4) << (i * 4);
                }
                self.output.push_back(w);
            }
        }
    }
}

impl crate::state::Field for Cmd {
    fn sync(&mut self, s: &mut crate::state::Sync) {
        let mut tag: u8 = match *self {
            Cmd::None => 0,
            Cmd::Decode => 1,
            Cmd::QuantTable { color: false } => 2,
            Cmd::QuantTable { color: true } => 3,
            Cmd::ScaleTable => 4,
        };
        tag.sync(s);
        if s.loading() {
            *self = match tag {
                1 => Cmd::Decode,
                2 => Cmd::QuantTable { color: false },
                3 => Cmd::QuantTable { color: true },
                4 => Cmd::ScaleTable,
                _ => Cmd::None,
            };
        }
    }
}

impl crate::state::Field for Mdec {
    fn sync(&mut self, s: &mut crate::state::Sync) {
        crate::sync_fields!(
            self, s; cmd, remaining, depth, signed, set_bit15, enable_in, enable_out, input, in_pos, params, qt_luma, qt_chroma, scale, output,
            current_block
        );
    }
}
