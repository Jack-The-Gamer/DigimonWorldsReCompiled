//! Geometry Transformation Engine (COP2).
//!
//! Bit-accurate implementation following the nocash PSX specification:
//! 44-bit MAC overflow tracking, saturation flags, the UNR division used by
//! perspective projection, and the hardware quirks (RTP IR3 flag, MVMVA
//! with the far-color vector, the "garbage" matrix, H sign-extension on
//! read, ...).

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Gte {
    // Data registers
    pub v: [[i16; 3]; 3],
    pub rgbc: [u8; 4],
    pub otz: u16,
    pub ir: [i16; 4],
    pub sxy: [[i16; 2]; 3],
    pub sz: [u16; 4],
    pub rgb_fifo: [[u8; 4]; 3],
    pub res1: u32,
    pub mac: [i32; 4],
    pub lzcs: u32,
    pub lzcr: u32,
    // Control registers
    pub rt: [[i16; 3]; 3],
    pub tr: [i32; 3],
    pub llm: [[i16; 3]; 3],
    pub bk: [i32; 3],
    pub lcm: [[i16; 3]; 3],
    pub fc: [i32; 3],
    pub ofx: i32,
    pub ofy: i32,
    pub h: u16,
    pub dqa: i16,
    pub dqb: i32,
    pub zsf3: i16,
    pub zsf4: i16,
    pub flag: u32,
}

impl Default for Gte {
    fn default() -> Self {
        Self {
            v: [[0; 3]; 3],
            rgbc: [0; 4],
            otz: 0,
            ir: [0; 4],
            sxy: [[0; 2]; 3],
            sz: [0; 4],
            rgb_fifo: [[0; 4]; 3],
            res1: 0,
            mac: [0; 4],
            lzcs: 0,
            lzcr: 32,
            rt: [[0; 3]; 3],
            tr: [0; 3],
            llm: [[0; 3]; 3],
            bk: [0; 3],
            lcm: [[0; 3]; 3],
            fc: [0; 3],
            ofx: 0,
            ofy: 0,
            h: 0,
            dqa: 0,
            dqb: 0,
            zsf3: 0,
            zsf4: 0,
            flag: 0,
        }
    }
}

fn unr_table() -> &'static [u8; 257] {
    static TABLE: std::sync::OnceLock<[u8; 257]> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = [0u8; 257];
        for (i, e) in t.iter_mut().enumerate() {
            let v = ((0x40000 / (i as i32 + 0x100) + 1) / 2 - 0x101).max(0);
            *e = v as u8;
        }
        t
    })
}

fn pack(lo: i16, hi: i16) -> u32 {
    (lo as u16 as u32) | ((hi as u16 as u32) << 16)
}

fn unpack(v: u32) -> (i16, i16) {
    (v as u16 as i16, (v >> 16) as u16 as i16)
}

/// Command timing in CPU cycles (nocash).
fn command_cycles(op: u32) -> u32 {
    match op {
        0x01 => 15,
        0x06 => 8,
        0x0C => 6,
        0x10 => 8,
        0x11 => 8,
        0x12 => 8,
        0x13 => 19,
        0x14 => 13,
        0x16 => 44,
        0x1B => 17,
        0x1C => 11,
        0x1E => 14,
        0x20 => 30,
        0x28 => 5,
        0x29 => 8,
        0x2A => 17,
        0x2D => 5,
        0x2E => 6,
        0x30 => 23,
        0x3D => 5,
        0x3E => 5,
        0x3F => 39,
        _ => 2,
    }
}

impl Gte {
    // ------------------------------------------------------------------
    // Register access
    // ------------------------------------------------------------------

    pub fn read_data(&self, r: usize) -> u32 {
        match r {
            0 => pack(self.v[0][0], self.v[0][1]),
            1 => self.v[0][2] as i32 as u32,
            2 => pack(self.v[1][0], self.v[1][1]),
            3 => self.v[1][2] as i32 as u32,
            4 => pack(self.v[2][0], self.v[2][1]),
            5 => self.v[2][2] as i32 as u32,
            6 => u32::from_le_bytes(self.rgbc),
            7 => self.otz as u32,
            8..=11 => self.ir[r - 8] as i32 as u32,
            12..=14 => pack(self.sxy[r - 12][0], self.sxy[r - 12][1]),
            15 => pack(self.sxy[2][0], self.sxy[2][1]),
            16..=19 => self.sz[r - 16] as u32,
            20..=22 => u32::from_le_bytes(self.rgb_fifo[r - 20]),
            23 => self.res1,
            24..=27 => self.mac[r - 24] as u32,
            28 | 29 => {
                let c = |v: i16| ((v >> 7).clamp(0, 0x1F)) as u32;
                c(self.ir[1]) | (c(self.ir[2]) << 5) | (c(self.ir[3]) << 10)
            }
            30 => self.lzcs,
            31 => self.lzcr,
            _ => 0,
        }
    }

    pub fn write_data(&mut self, r: usize, v: u32) {
        match r {
            0 | 2 | 4 => {
                let (x, y) = unpack(v);
                self.v[r / 2][0] = x;
                self.v[r / 2][1] = y;
            }
            1 | 3 | 5 => self.v[r / 2][2] = v as i16,
            6 => self.rgbc = v.to_le_bytes(),
            7 => self.otz = v as u16,
            8..=11 => self.ir[r - 8] = v as i16,
            12..=14 => {
                let (x, y) = unpack(v);
                self.sxy[r - 12] = [x, y];
            }
            15 => {
                let (x, y) = unpack(v);
                self.sxy[0] = self.sxy[1];
                self.sxy[1] = self.sxy[2];
                self.sxy[2] = [x, y];
            }
            16..=19 => self.sz[r - 16] = v as u16,
            20..=22 => self.rgb_fifo[r - 20] = v.to_le_bytes(),
            23 => self.res1 = v,
            24..=27 => self.mac[r - 24] = v as i32,
            28 => {
                self.ir[1] = ((v & 0x1F) << 7) as i16;
                self.ir[2] = (((v >> 5) & 0x1F) << 7) as i16;
                self.ir[3] = (((v >> 10) & 0x1F) << 7) as i16;
            }
            29 => {}
            30 => {
                self.lzcs = v;
                self.lzcr = if (v as i32) < 0 { v.leading_ones() } else { v.leading_zeros() };
            }
            _ => {}
        }
    }

    fn mat_read(m: &[[i16; 3]; 3], r: usize) -> u32 {
        match r {
            0 => pack(m[0][0], m[0][1]),
            1 => pack(m[0][2], m[1][0]),
            2 => pack(m[1][1], m[1][2]),
            3 => pack(m[2][0], m[2][1]),
            _ => m[2][2] as i32 as u32,
        }
    }

    fn mat_write(m: &mut [[i16; 3]; 3], r: usize, v: u32) {
        let (lo, hi) = unpack(v);
        match r {
            0 => {
                m[0][0] = lo;
                m[0][1] = hi
            }
            1 => {
                m[0][2] = lo;
                m[1][0] = hi
            }
            2 => {
                m[1][1] = lo;
                m[1][2] = hi
            }
            3 => {
                m[2][0] = lo;
                m[2][1] = hi
            }
            _ => m[2][2] = lo,
        }
    }

    pub fn read_ctrl(&self, r: usize) -> u32 {
        match r {
            0..=4 => Self::mat_read(&self.rt, r),
            5..=7 => self.tr[r - 5] as u32,
            8..=12 => Self::mat_read(&self.llm, r - 8),
            13..=15 => self.bk[r - 13] as u32,
            16..=20 => Self::mat_read(&self.lcm, r - 16),
            21..=23 => self.fc[r - 21] as u32,
            24 => self.ofx as u32,
            25 => self.ofy as u32,
            // H is unsigned but reads back sign-extended (hardware quirk).
            26 => self.h as i16 as i32 as u32,
            27 => self.dqa as i32 as u32,
            28 => self.dqb as u32,
            29 => self.zsf3 as i32 as u32,
            30 => self.zsf4 as i32 as u32,
            31 => self.flag,
            _ => 0,
        }
    }

    pub fn write_ctrl(&mut self, r: usize, v: u32) {
        match r {
            0..=4 => Self::mat_write(&mut self.rt, r, v),
            5..=7 => self.tr[r - 5] = v as i32,
            8..=12 => Self::mat_write(&mut self.llm, r - 8, v),
            13..=15 => self.bk[r - 13] = v as i32,
            16..=20 => Self::mat_write(&mut self.lcm, r - 16, v),
            21..=23 => self.fc[r - 21] = v as i32,
            24 => self.ofx = v as i32,
            25 => self.ofy = v as i32,
            26 => self.h = v as u16,
            27 => self.dqa = v as i16,
            28 => self.dqb = v as i32,
            29 => self.zsf3 = v as i16,
            30 => self.zsf4 = v as i16,
            31 => {
                self.flag = v & 0x7FFF_F000;
                self.update_flag_summary();
            }
            _ => {}
        }
    }

    fn update_flag_summary(&mut self) {
        if self.flag & 0x7F87_E000 != 0 {
            self.flag |= 1 << 31;
        } else {
            self.flag &= !(1 << 31);
        }
    }

    // ------------------------------------------------------------------
    // Arithmetic helpers
    // ------------------------------------------------------------------

    /// Checks a MAC1..3 intermediate for 44-bit overflow and wraps it.
    #[inline]
    fn a(&mut self, i: usize, v: i64) -> i64 {
        if v > 0x7FF_FFFF_FFFF {
            self.flag |= 1 << (31 - i);
        } else if v < -0x800_0000_0000 {
            self.flag |= 1 << (28 - i);
        }
        (v << 20) >> 20
    }

    #[inline]
    fn set_mac(&mut self, i: usize, v: i64, shift: u32) -> i64 {
        self.a(i, v);
        let r = v >> shift;
        self.mac[i] = r as i32;
        r
    }

    #[inline]
    fn set_ir(&mut self, i: usize, v: i64, lm: bool) {
        let min = if lm { 0 } else { -0x8000 };
        let r = if v < min {
            self.flag |= 1 << (25 - i);
            min
        } else if v > 0x7FFF {
            self.flag |= 1 << (25 - i);
            0x7FFF
        } else {
            v
        };
        self.ir[i] = r as i16;
    }

    #[inline]
    fn f(&mut self, v: i64) -> i64 {
        if v > 0x7FFF_FFFF {
            self.flag |= 1 << 16;
        } else if v < -0x8000_0000 {
            self.flag |= 1 << 15;
        }
        v
    }

    fn set_mac0(&mut self, v: i64) -> i64 {
        self.f(v);
        self.mac[0] = v as i32;
        v
    }

    fn set_ir0(&mut self, v: i64) {
        let r = if v < 0 {
            self.flag |= 1 << 12;
            0
        } else if v > 0x1000 {
            self.flag |= 1 << 12;
            0x1000
        } else {
            v
        };
        self.ir[0] = r as i16;
    }

    fn sat_sxy(&mut self, v: i64, bit: u32) -> i16 {
        if v < -0x400 {
            self.flag |= 1 << bit;
            -0x400
        } else if v > 0x3FF {
            self.flag |= 1 << bit;
            0x3FF
        } else {
            v as i16
        }
    }

    fn sat_z(&mut self, v: i64) -> u16 {
        if v < 0 {
            self.flag |= 1 << 18;
            0
        } else if v > 0xFFFF {
            self.flag |= 1 << 18;
            0xFFFF
        } else {
            v as u16
        }
    }

    fn sat_color(&mut self, v: i32, bit: u32) -> u8 {
        if v < 0 {
            self.flag |= 1 << bit;
            0
        } else if v > 0xFF {
            self.flag |= 1 << bit;
            0xFF
        } else {
            v as u8
        }
    }

    fn push_color(&mut self) {
        let r = self.sat_color(self.mac[1] >> 4, 21);
        let g = self.sat_color(self.mac[2] >> 4, 20);
        let b = self.sat_color(self.mac[3] >> 4, 19);
        self.rgb_fifo[0] = self.rgb_fifo[1];
        self.rgb_fifo[1] = self.rgb_fifo[2];
        self.rgb_fifo[2] = [r, g, b, self.rgbc[3]];
    }

    fn push_sz(&mut self, z: u16) {
        self.sz[0] = self.sz[1];
        self.sz[1] = self.sz[2];
        self.sz[2] = self.sz[3];
        self.sz[3] = z;
    }

    fn push_sxy(&mut self, x: i16, y: i16) {
        self.sxy[0] = self.sxy[1];
        self.sxy[1] = self.sxy[2];
        self.sxy[2] = [x, y];
    }

    /// UNR reciprocal division used by RTPS/RTPT.
    fn divide(&mut self) -> u32 {
        let h = self.h as u32;
        let sz3 = self.sz[3] as u32;
        if h < sz3 * 2 {
            let z = (sz3 as u16).leading_zeros();
            let n = h << z;
            let d = sz3 << z;
            let u = unr_table()[((d - 0x7FC0) >> 7) as usize] as u32 + 0x101;
            let d = (0x200_0080 - (d * u)) >> 8;
            let d = (0x000_0080 + (d * u)) >> 8;
            let r = ((n as u64 * d as u64 + 0x8000) >> 16) as u32;
            r.min(0x1FFFF)
        } else {
            self.flag |= 1 << 17;
            0x1FFFF
        }
    }

    /// Multiplies matrix `m` by vector `vec`, adding `tr << 12`, into MAC/IR.
    fn mat_vec(&mut self, m: &[[i16; 3]; 3], vec: [i16; 3], tr: [i32; 3], shift: u32, lm: bool) {
        for row in 0..3 {
            let mut acc = self.a(row + 1, (tr[row] as i64) << 12);
            for col in 0..3 {
                acc = self.a(row + 1, acc + m[row][col] as i64 * vec[col] as i64);
            }
            let r = self.set_mac(row + 1, acc, shift);
            self.set_ir(row + 1, r, lm);
        }
    }

    fn ir_vec(&self) -> [i16; 3] {
        [self.ir[1], self.ir[2], self.ir[3]]
    }

    // ------------------------------------------------------------------
    // Commands
    // ------------------------------------------------------------------

    /// Executes a GTE command; returns its duration in CPU cycles.
    pub fn execute(&mut self, cmd: u32) -> u32 {
        let op = cmd & 0x3F;
        let sf = if cmd & (1 << 19) != 0 { 12 } else { 0 };
        let lm = cmd & (1 << 10) != 0;
        self.flag = 0;
        match op {
            0x01 => self.rtp(0, sf, lm, true),
            0x06 => self.nclip(),
            0x0C => self.op(sf, lm),
            0x10 => self.dpcs(sf, lm, false),
            0x11 => self.intpl(sf, lm),
            0x12 => self.mvmva(cmd, sf, lm),
            0x13 => self.ncd(0, sf, lm),
            0x14 => self.cdp(sf, lm),
            0x16 => {
                for i in 0..3 {
                    self.ncd(i, sf, lm);
                }
            }
            0x1B => self.ncc(0, sf, lm),
            0x1C => self.cc(sf, lm),
            0x1E => self.nc(0, sf, lm),
            0x20 => {
                for i in 0..3 {
                    self.nc(i, sf, lm);
                }
            }
            0x28 => self.sqr(sf, lm),
            0x29 => self.dcpl(sf, lm),
            0x2A => {
                for _ in 0..3 {
                    self.dpcs(sf, lm, true);
                }
            }
            0x2D => self.avsz3(),
            0x2E => self.avsz4(),
            0x30 => {
                self.rtp(0, sf, lm, false);
                self.rtp(1, sf, lm, false);
                self.rtp(2, sf, lm, true);
            }
            0x3D => self.gpf(sf, lm),
            0x3E => self.gpl(sf, lm),
            0x3F => {
                for i in 0..3 {
                    self.ncc(i, sf, lm);
                }
            }
            _ => log::debug!("unknown GTE command {cmd:08x}"),
        }
        self.update_flag_summary();
        command_cycles(op)
    }

    fn rtp(&mut self, vi: usize, sf: u32, lm: bool, last: bool) {
        let v = self.v[vi];
        let rt = self.rt;
        // MAC1..3 = TR*1000h + RT*V
        let mut z_raw = 0i64;
        for row in 0..3 {
            let mut acc = self.a(row + 1, (self.tr[row] as i64) << 12);
            for col in 0..3 {
                acc = self.a(row + 1, acc + rt[row][col] as i64 * v[col] as i64);
            }
            let r = self.set_mac(row + 1, acc, sf);
            if row < 2 {
                self.set_ir(row + 1, r, lm);
            } else {
                z_raw = acc;
                // IR3 saturates on the shifted value but the flag is
                // computed from MAC3 >> 12 regardless of sf.
                let min = if lm { 0 } else { -0x8000 };
                self.ir[3] = r.clamp(min, 0x7FFF) as i16;
                let fv = acc >> 12;
                if fv < -0x8000 || fv > 0x7FFF {
                    self.flag |= 1 << 22;
                }
            }
        }
        let sz = self.sat_z(z_raw >> 12);
        self.push_sz(sz);
        let n = self.divide() as i64;
        let x = self.set_mac0(n * self.ir[1] as i64 + self.ofx as i64);
        let sx = self.sat_sxy(x >> 16, 14);
        let y = self.set_mac0(n * self.ir[2] as i64 + self.ofy as i64);
        let sy = self.sat_sxy(y >> 16, 13);
        self.push_sxy(sx, sy);
        if last {
            let d = self.set_mac0(n * self.dqa as i64 + self.dqb as i64);
            self.set_ir0(d >> 12);
        }
    }

    fn nclip(&mut self) {
        let [x0, y0] = [self.sxy[0][0] as i64, self.sxy[0][1] as i64];
        let [x1, y1] = [self.sxy[1][0] as i64, self.sxy[1][1] as i64];
        let [x2, y2] = [self.sxy[2][0] as i64, self.sxy[2][1] as i64];
        let v = x0 * y1 + x1 * y2 + x2 * y0 - x0 * y2 - x1 * y0 - x2 * y1;
        self.set_mac0(v);
    }

    fn op(&mut self, sf: u32, lm: bool) {
        let d1 = self.rt[0][0] as i64;
        let d2 = self.rt[1][1] as i64;
        let d3 = self.rt[2][2] as i64;
        let (ir1, ir2, ir3) = (self.ir[1] as i64, self.ir[2] as i64, self.ir[3] as i64);
        let m1 = self.set_mac(1, ir3 * d2 - ir2 * d3, sf);
        let m2 = self.set_mac(2, ir1 * d3 - ir3 * d1, sf);
        let m3 = self.set_mac(3, ir2 * d1 - ir1 * d2, sf);
        self.set_ir(1, m1, lm);
        self.set_ir(2, m2, lm);
        self.set_ir(3, m3, lm);
    }

    fn sqr(&mut self, sf: u32, lm: bool) {
        for i in 1..4 {
            let v = self.ir[i] as i64;
            let m = self.set_mac(i, v * v, sf);
            self.set_ir(i, m, lm);
        }
    }

    fn avsz3(&mut self) {
        let s = self.sz[1] as i64 + self.sz[2] as i64 + self.sz[3] as i64;
        let v = self.set_mac0(self.zsf3 as i64 * s);
        self.otz = self.sat_z(v >> 12);
    }

    fn avsz4(&mut self) {
        let s = self.sz[0] as i64 + self.sz[1] as i64 + self.sz[2] as i64 + self.sz[3] as i64;
        let v = self.set_mac0(self.zsf4 as i64 * s);
        self.otz = self.sat_z(v >> 12);
    }

    fn mvmva(&mut self, cmd: u32, sf: u32, lm: bool) {
        let mx = (cmd >> 17) & 3;
        let vx = (cmd >> 15) & 3;
        let tx = (cmd >> 13) & 3;
        let m = match mx {
            0 => self.rt,
            1 => self.llm,
            2 => self.lcm,
            _ => {
                let r = (self.rgbc[0] as i16) << 4;
                [[-r, r, self.ir[0]], [self.rt[0][2]; 3], [self.rt[1][1]; 3]]
            }
        };
        let v = match vx {
            0 => self.v[0],
            1 => self.v[1],
            2 => self.v[2],
            _ => self.ir_vec(),
        };
        let t = match tx {
            0 => self.tr,
            1 => self.bk,
            2 => self.fc,
            _ => [0; 3],
        };
        if tx == 2 {
            // Buggy far-color translation: the first column is only used
            // for flag computation.
            for row in 0..3 {
                let tmp = self.a(row + 1, ((t[row] as i64) << 12) + m[row][0] as i64 * v[0] as i64);
                self.set_ir(row + 1, tmp >> sf, false);
                let mut acc = self.a(row + 1, m[row][1] as i64 * v[1] as i64);
                acc = self.a(row + 1, acc + m[row][2] as i64 * v[2] as i64);
                let r = self.set_mac(row + 1, acc, sf);
                self.set_ir(row + 1, r, lm);
            }
        } else {
            self.mat_vec(&m, v, t, sf, lm);
        }
    }

    /// Light-source normal transformation shared by NC*/NCC*/NCD*.
    fn light(&mut self, vi: usize, sf: u32, lm: bool) {
        let llm = self.llm;
        self.mat_vec(&llm, self.v[vi], [0; 3], sf, lm);
        let lcm = self.lcm;
        let bk = self.bk;
        let ir = self.ir_vec();
        self.mat_vec(&lcm, ir, bk, sf, lm);
    }

    fn nc(&mut self, vi: usize, sf: u32, lm: bool) {
        self.light(vi, sf, lm);
        self.push_color();
    }

    fn color_mul(&mut self, sf: u32, lm: bool) {
        // MAC = [R*IR1, G*IR2, B*IR3] << 4
        for i in 0..3 {
            let c = self.rgbc[i] as i64;
            let m = self.set_mac(i + 1, (c * self.ir[i + 1] as i64) << 4, sf);
            self.set_ir(i + 1, m, lm);
        }
    }

    fn ncc(&mut self, vi: usize, sf: u32, lm: bool) {
        self.light(vi, sf, lm);
        self.color_mul(sf, lm);
        self.push_color();
    }

    /// MAC = MAC + (FC - MAC) * IR0, given pre-shift MAC values.
    fn depth_cue(&mut self, mac: [i64; 3], sf: u32, lm: bool) {
        for i in 0..3 {
            let fc = (self.fc[i] as i64) << 12;
            let diff = self.a(i + 1, fc - mac[i]);
            self.set_ir(i + 1, diff >> sf, false);
        }
        for i in 0..3 {
            let v = self.a(i + 1, self.ir[i + 1] as i64 * self.ir[0] as i64 + mac[i]);
            let r = self.set_mac(i + 1, v, sf);
            self.set_ir(i + 1, r, lm);
        }
    }

    fn ncd(&mut self, vi: usize, sf: u32, lm: bool) {
        self.light(vi, sf, lm);
        let mac = [0, 1, 2].map(|i| (self.rgbc[i] as i64 * self.ir[i + 1] as i64) << 4);
        self.depth_cue(mac, sf, lm);
        self.push_color();
    }

    fn cc(&mut self, sf: u32, lm: bool) {
        let lcm = self.lcm;
        let bk = self.bk;
        let ir = self.ir_vec();
        self.mat_vec(&lcm, ir, bk, sf, lm);
        self.color_mul(sf, lm);
        self.push_color();
    }

    fn cdp(&mut self, sf: u32, lm: bool) {
        let lcm = self.lcm;
        let bk = self.bk;
        let ir = self.ir_vec();
        self.mat_vec(&lcm, ir, bk, sf, lm);
        let mac = [0, 1, 2].map(|i| (self.rgbc[i] as i64 * self.ir[i + 1] as i64) << 4);
        self.depth_cue(mac, sf, lm);
        self.push_color();
    }

    fn dpcs(&mut self, sf: u32, lm: bool, from_fifo: bool) {
        let c = if from_fifo { self.rgb_fifo[0] } else { self.rgbc };
        let mac = [0, 1, 2].map(|i| (c[i] as i64) << 16);
        self.depth_cue(mac, sf, lm);
        self.push_color();
    }

    fn dcpl(&mut self, sf: u32, lm: bool) {
        let mac = [0, 1, 2].map(|i| (self.rgbc[i] as i64 * self.ir[i + 1] as i64) << 4);
        self.depth_cue(mac, sf, lm);
        self.push_color();
    }

    fn intpl(&mut self, sf: u32, lm: bool) {
        let mac = [1, 2, 3].map(|i| (self.ir[i] as i64) << 12);
        self.depth_cue(mac, sf, lm);
        self.push_color();
    }

    fn gpf(&mut self, sf: u32, lm: bool) {
        for i in 1..4 {
            let m = self.set_mac(i, self.ir[i] as i64 * self.ir[0] as i64, sf);
            self.set_ir(i, m, lm);
        }
        self.push_color();
    }

    fn gpl(&mut self, sf: u32, lm: bool) {
        for i in 1..4 {
            let base = (self.mac[i] as i64) << sf;
            let v = self.a(i, base + self.ir[i] as i64 * self.ir[0] as i64);
            let m = self.set_mac(i, v, sf);
            self.set_ir(i, m, lm);
        }
        self.push_color();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unr_table_matches_spec_endpoints() {
        let t = unr_table();
        assert_eq!(t[0], 0xFF);
        assert_eq!(t[256], 0x00);
    }

    #[test]
    fn rtps_identity_projection() {
        let mut g = Gte::default();
        // Identity rotation (1.0 = 0x1000), no translation.
        g.rt = [[0x1000, 0, 0], [0, 0x1000, 0], [0, 0, 0x1000]];
        g.h = 0x100;
        g.ofx = 160 << 16;
        g.ofy = 120 << 16;
        g.v[0] = [100, 50, 0x400];
        g.execute(0x0018_0001); // RTPS sf=1
        assert_eq!(g.sz[3], 0x400);
        // x' = 100 * 256 / 1024 = 25 -> 185
        assert_eq!(g.sxy[2], [185, 132]);
    }

    #[test]
    fn lzcr() {
        let mut g = Gte::default();
        g.write_data(30, 0x0000_FFFF);
        assert_eq!(g.read_data(31), 16);
        g.write_data(30, 0xFFF0_0000);
        assert_eq!(g.read_data(31), 12);
    }
}

impl crate::state::Field for Gte {
    fn sync(&mut self, s: &mut crate::state::Sync) {
        crate::sync_fields!(self, s; v, rgbc, otz, ir, sxy, sz, rgb_fifo, res1, mac, lzcs, lzcr, rt, tr, llm, bk, lcm, fc, ofx, ofy, h, dqa, dqb, zsf3, zsf4, flag);
    }
}
