//! Sound Processing Unit: 24 ADPCM voices with ADSR envelopes, noise,
//! pitch modulation, 4-point interpolation, the hardware reverb unit,
//! capture buffers, sound RAM transfers and CD audio mixing.

use crate::bus::NEVER;
use crate::cdrom::Cdrom;
use crate::irq::{Interrupt, Irq};
use crate::CYCLES_PER_SAMPLE;

pub const SPU_RAM_SIZE: usize = 512 * 1024;
/// Samples generated per scheduled SPU event.
pub const SAMPLES_PER_EVENT: u64 = 32;

const ADPCM_POS: [i32; 5] = [0, 60, 115, 98, 122];
const ADPCM_NEG: [i32; 5] = [0, 0, -52, -55, -60];

/// Reverb resampling FIR (nocash), sums to ~0x8000.
const REVERB_FIR: [i32; 39] = [
    -1, 0, 2, 0, -10, 0, 35, 0, -103, 0, 266, 0, -616, 0, 1332, 0, -2960, 0, 10246, 16384, 10246, 0, -2960, 0, 1332, 0,
    -616, 0, 266, 0, -103, 0, 35, 0, -10, 0, 2, 0, -1,
];

fn gauss_table() -> &'static [i32; 512] {
    &crate::spu_gauss::GAUSS
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Phase {
    Attack,
    Decay,
    Sustain,
    Release,
    #[default]
    Off,
}

#[derive(Debug, Clone, Copy, Default)]
struct Envelope {
    level: i32,
    counter: u32,
}

/// Generic PS1 envelope step (shared by ADSR and volume sweeps).
#[inline]
fn env_tick(env: &mut Envelope, shift: u32, step_bits: u32, decrease: bool, exponential: bool) {
    if env.counter > 0 {
        env.counter -= 1;
        return;
    }
    let mut cycles = 1u32 << shift.saturating_sub(11);
    let base_step: i32 = if decrease { -8 + step_bits as i32 } else { 7 - step_bits as i32 };
    let mut step = base_step << 11u32.saturating_sub(shift);
    if exponential && !decrease && env.level > 0x6000 {
        cycles *= 4;
    }
    if exponential && decrease {
        step = (step * env.level) >> 15;
    }
    env.level = (env.level + step).clamp(0, 0x7FFF);
    env.counter = cycles.saturating_sub(1);
}

#[derive(Debug, Clone, Copy, Default)]
struct Volume {
    reg: u16,
    level: i32,
    env: Envelope,
}

impl Volume {
    fn set(&mut self, v: u16) {
        self.reg = v;
        if v & 0x8000 == 0 {
            self.level = ((v << 1) as i16) as i32;
        }
    }
    fn tick(&mut self) {
        if self.reg & 0x8000 == 0 {
            return;
        }
        let exp = self.reg & 0x4000 != 0;
        let dec = self.reg & 0x2000 != 0;
        let shift = ((self.reg >> 2) & 0x1F) as u32;
        let step = (self.reg & 3) as u32;
        self.env.level = self.level.abs();
        env_tick(&mut self.env, shift, step, dec, exp);
        let neg = self.reg & 0x1000 != 0;
        self.level = if neg { -self.env.level } else { self.env.level };
    }
}

#[derive(Debug, Clone, Copy)]
struct Voice {
    vol_l: Volume,
    vol_r: Volume,
    pitch: u16,
    start: u16,
    repeat: u16,
    adsr: u32,
    // runtime
    addr: u32,
    counter: u32,
    buf: [i16; 31],
    prev: [i32; 2],
    phase: Phase,
    env: Envelope,
    flags: u8,
    out: i32,
    ignore_loop_addr: bool,
    needs_decode: bool,
}

impl Default for Voice {
    fn default() -> Self {
        Self {
            vol_l: Volume::default(),
            vol_r: Volume::default(),
            pitch: 0,
            start: 0,
            repeat: 0,
            adsr: 0,
            addr: 0,
            counter: 0,
            buf: [0; 31],
            prev: [0; 2],
            phase: Phase::Off,
            env: Envelope::default(),
            flags: 0,
            out: 0,
            ignore_loop_addr: false,
            needs_decode: false,
        }
    }
}

impl Voice {
    fn sustain_level(&self) -> i32 {
        (((self.adsr & 0xF) + 1) * 0x800).min(0x7FFF) as i32
    }

    fn tick_adsr(&mut self) {
        let a = self.adsr;
        match self.phase {
            Phase::Attack => {
                let exp = a & (1 << 15) != 0;
                env_tick(&mut self.env, (a >> 10) & 0x1F, (a >> 8) & 3, false, exp);
                if self.env.level >= 0x7FFF {
                    self.phase = Phase::Decay;
                    self.env.counter = 0;
                }
            }
            Phase::Decay => {
                env_tick(&mut self.env, ((a >> 4) & 0xF) << 2, 0, true, true);
                if self.env.level <= self.sustain_level() {
                    self.phase = Phase::Sustain;
                    self.env.counter = 0;
                }
            }
            Phase::Sustain => {
                let exp = a & (1 << 31) != 0;
                let dec = a & (1 << 30) != 0;
                env_tick(&mut self.env, (a >> 24) & 0x1F, (a >> 22) & 3, dec, exp);
            }
            Phase::Release => {
                let exp = a & (1 << 21) != 0;
                env_tick(&mut self.env, (a >> 16) & 0x1F, 0, true, exp);
                if self.env.level == 0 {
                    self.phase = Phase::Off;
                }
            }
            Phase::Off => {}
        }
    }
}

#[derive(Clone)]
struct Reverb {
    regs: [u16; 32],
    base: u32,
    cur: u32,
    in_l: [i32; 64],
    in_r: [i32; 64],
    in_pos: usize,
    up_l: [i32; 64],
    up_r: [i32; 64],
    up_pos: usize,
    out_l: i32,
    out_r: i32,
    odd: bool,
}

impl Default for Reverb {
    fn default() -> Self {
        Self {
            regs: [0; 32],
            base: 0,
            cur: 0,
            in_l: [0; 64],
            in_r: [0; 64],
            in_pos: 0,
            up_l: [0; 64],
            up_r: [0; 64],
            up_pos: 0,
            out_l: 0,
            out_r: 0,
            odd: false,
        }
    }
}

pub struct Spu {
    pub ram: Box<[u8]>,
    voices: [Voice; 24],
    main_l: Volume,
    main_r: Volume,
    reverb_vol_l: i16,
    reverb_vol_r: i16,
    kon: u32,
    koff: u32,
    pmon: u32,
    non: u32,
    eon: u32,
    endx: u32,
    pub ctrl: u16,
    stat_irq: bool,
    irq_addr: u32,
    transfer_addr_reg: u16,
    transfer_addr: u32,
    transfer_ctrl: u16,
    fifo: Vec<u16>,
    cd_vol: [i16; 2],
    ext_vol: [i16; 2],
    unknown: [u16; 0x40],
    noise_level: u16,
    noise_timer: i32,
    capture_pos: u32,
    reverb: Reverb,
    last_sync: u64,
    remainder: u64,
    /// Interleaved stereo output at 44.1 kHz.
    pub output: Vec<i16>,
    pub output_enabled: bool,
    pending_irq_from_dma: bool,
    /// Fast-forward: the console runs `speed` times faster than real time.
    /// One output sample is kept per `speed` samples (the others averaged
    /// in), so what the player hears plays in real time.
    pub speed: u32,
    /// Voices that keep real-time pitch and timing while `speed > 1`: they
    /// advance only on the kept samples (a music driver slowed to real time
    /// plays its notes on these). The others run at the console's speed.
    pub realtime_voices: u32,
    ff_phase: u32,
    /// Sped-up voices' mix (L, R) and reverb input (L, R), plus CD audio
    /// (L, R), summed over the samples between two kept ones.
    ff_acc: [i32; 6],
}

impl Default for Spu {
    fn default() -> Self {
        Self::new()
    }
}

impl Spu {
    pub fn new() -> Self {
        Self {
            ram: vec![0u8; SPU_RAM_SIZE].into_boxed_slice(),
            voices: [Voice::default(); 24],
            main_l: Volume::default(),
            main_r: Volume::default(),
            reverb_vol_l: 0,
            reverb_vol_r: 0,
            kon: 0,
            koff: 0,
            pmon: 0,
            non: 0,
            eon: 0,
            endx: 0,
            ctrl: 0,
            stat_irq: false,
            irq_addr: 0,
            transfer_addr_reg: 0,
            transfer_addr: 0,
            transfer_ctrl: 4,
            fifo: Vec::new(),
            cd_vol: [0; 2],
            ext_vol: [0; 2],
            unknown: [0; 0x40],
            noise_level: 1,
            noise_timer: 0,
            capture_pos: 0,
            reverb: Reverb::default(),
            last_sync: 0,
            remainder: 0,
            output: Vec::with_capacity(4096),
            output_enabled: true,
            pending_irq_from_dma: false,
            speed: 1,
            realtime_voices: 0,
            ff_phase: 0,
            ff_acc: [0; 6],
        }
    }

    /// Sets the fast-forward factor (1 = real time).
    pub fn set_speed(&mut self, speed: u32) {
        let speed = speed.clamp(1, 8);
        if speed != self.speed {
            self.speed = speed;
            self.ff_phase = 0;
            self.ff_acc = [0; 6];
        }
    }

    fn ram16(&self, addr: u32) -> i16 {
        let a = (addr as usize) & (SPU_RAM_SIZE - 2);
        i16::from_le_bytes([self.ram[a], self.ram[a + 1]])
    }

    fn write_ram16(&mut self, addr: u32, v: i16, irq: &mut Irq) {
        let a = (addr as usize) & (SPU_RAM_SIZE - 2);
        self.ram[a..a + 2].copy_from_slice(&v.to_le_bytes());
        self.check_irq(a as u32, irq);
    }

    fn check_irq(&mut self, addr: u32, irq: &mut Irq) {
        if self.ctrl & 0x40 != 0 && self.ctrl & 0x8000 != 0 && (addr & !7) == self.irq_addr && !self.stat_irq {
            self.stat_irq = true;
            irq.raise(Interrupt::Spu);
        }
    }

    pub fn next_irq_cycle(&self, _now: u64) -> u64 {
        NEVER
    }

    // ------------------------------------------------------------------
    // Registers
    // ------------------------------------------------------------------

    fn status(&self) -> u16 {
        let mode = (self.ctrl >> 4) & 3;
        let mut s = self.ctrl & 0x3F;
        s |= (self.stat_irq as u16) << 6;
        let dma_req = mode >= 2;
        s |= (dma_req as u16) << 7;
        s |= ((mode == 2) as u16) << 8;
        s |= ((mode == 3) as u16) << 9;
        s |= ((self.capture_pos >= 0x100) as u16) << 11;
        s
    }

    pub fn read(&self, off: u32) -> u16 {
        let off = off & 0x3FE;
        match off {
            0x000..=0x17F => {
                let v = &self.voices[(off >> 4) as usize];
                match off & 0xF {
                    0x0 => v.vol_l.reg,
                    0x2 => v.vol_r.reg,
                    0x4 => v.pitch,
                    0x6 => v.start,
                    0x8 => v.adsr as u16,
                    0xA => (v.adsr >> 16) as u16,
                    0xC => v.env.level as u16,
                    _ => v.repeat,
                }
            }
            0x180 => self.main_l.reg,
            0x182 => self.main_r.reg,
            0x184 => self.reverb_vol_l as u16,
            0x186 => self.reverb_vol_r as u16,
            0x188 => self.kon as u16,
            0x18A => (self.kon >> 16) as u16,
            0x18C => self.koff as u16,
            0x18E => (self.koff >> 16) as u16,
            0x190 => self.pmon as u16,
            0x192 => (self.pmon >> 16) as u16,
            0x194 => self.non as u16,
            0x196 => (self.non >> 16) as u16,
            0x198 => self.eon as u16,
            0x19A => (self.eon >> 16) as u16,
            0x19C => self.endx as u16,
            0x19E => (self.endx >> 16) as u16,
            0x1A2 => (self.reverb.base >> 3) as u16,
            0x1A4 => (self.irq_addr >> 3) as u16,
            0x1A6 => self.transfer_addr_reg,
            0x1AA => self.ctrl,
            0x1AC => self.transfer_ctrl,
            0x1AE => self.status(),
            0x1B0 => self.cd_vol[0] as u16,
            0x1B2 => self.cd_vol[1] as u16,
            0x1B4 => self.ext_vol[0] as u16,
            0x1B6 => self.ext_vol[1] as u16,
            0x1B8 => self.main_l.level as u16,
            0x1BA => self.main_r.level as u16,
            0x1C0..=0x1FF => self.reverb.regs[((off - 0x1C0) >> 1) as usize],
            0x200..=0x25F => {
                let v = &self.voices[((off - 0x200) >> 2) as usize];
                if off & 2 == 0 {
                    v.vol_l.level as u16
                } else {
                    v.vol_r.level as u16
                }
            }
            _ => self.unknown[((off >> 1) & 0x3F) as usize],
        }
    }

    pub fn write(&mut self, off: u32, v: u16, irq: &mut Irq) {
        let off = off & 0x3FE;
        match off {
            0x000..=0x17F => {
                let voice = &mut self.voices[(off >> 4) as usize];
                match off & 0xF {
                    0x0 => voice.vol_l.set(v),
                    0x2 => voice.vol_r.set(v),
                    0x4 => voice.pitch = v,
                    0x6 => voice.start = v,
                    0x8 => voice.adsr = (voice.adsr & 0xFFFF_0000) | v as u32,
                    0xA => voice.adsr = (voice.adsr & 0xFFFF) | ((v as u32) << 16),
                    0xC => voice.env.level = (v as i16 as i32).max(0),
                    _ => {
                        voice.repeat = v;
                        voice.ignore_loop_addr = true;
                    }
                }
            }
            0x180 => self.main_l.set(v),
            0x182 => self.main_r.set(v),
            0x184 => self.reverb_vol_l = v as i16,
            0x186 => self.reverb_vol_r = v as i16,
            0x188 => self.key_on((v as u32) & 0xFFFF),
            0x18A => self.key_on(((v as u32) & 0xFF) << 16),
            0x18C => self.key_off((v as u32) & 0xFFFF),
            0x18E => self.key_off(((v as u32) & 0xFF) << 16),
            0x190 => self.pmon = (self.pmon & 0xFFFF_0000) | (v as u32 & 0xFFFE),
            0x192 => self.pmon = (self.pmon & 0xFFFF) | ((v as u32 & 0xFF) << 16),
            0x194 => self.non = (self.non & 0xFFFF_0000) | v as u32,
            0x196 => self.non = (self.non & 0xFFFF) | ((v as u32 & 0xFF) << 16),
            0x198 => self.eon = (self.eon & 0xFFFF_0000) | v as u32,
            0x19A => self.eon = (self.eon & 0xFFFF) | ((v as u32 & 0xFF) << 16),
            0x19C | 0x19E => {}
            0x1A2 => {
                self.reverb.base = (v as u32) << 3;
                self.reverb.cur = self.reverb.base;
            }
            0x1A4 => self.irq_addr = (v as u32) << 3,
            0x1A6 => {
                self.transfer_addr_reg = v;
                self.transfer_addr = (v as u32) << 3;
            }
            0x1A8 => {
                if self.fifo.len() < 32 {
                    self.fifo.push(v);
                }
            }
            0x1AA => {
                let old_mode = (self.ctrl >> 4) & 3;
                self.ctrl = v;
                if v & 0x40 == 0 {
                    self.stat_irq = false;
                }
                let mode = (v >> 4) & 3;
                if mode == 1 && old_mode != 1 {
                    self.flush_fifo(irq);
                }
                if v & 0x8000 == 0 {
                    // SPU disabled: voices are silenced.
                }
            }
            0x1AC => self.transfer_ctrl = v,
            0x1AE => {}
            0x1B0 => self.cd_vol[0] = v as i16,
            0x1B2 => self.cd_vol[1] = v as i16,
            0x1B4 => self.ext_vol[0] = v as i16,
            0x1B6 => self.ext_vol[1] = v as i16,
            0x1B8 | 0x1BA => {}
            0x1C0..=0x1FF => self.reverb.regs[((off - 0x1C0) >> 1) as usize] = v,
            _ => self.unknown[((off >> 1) & 0x3F) as usize] = v,
        }
        if off == 0x1A8 && (self.ctrl >> 4) & 3 == 1 {
            self.flush_fifo(irq);
        }
    }

    fn flush_fifo(&mut self, irq: &mut Irq) {
        let data = std::mem::take(&mut self.fifo);
        for h in data {
            let a = self.transfer_addr;
            self.write_ram16(a, h as i16, irq);
            self.transfer_addr = (self.transfer_addr + 2) & (SPU_RAM_SIZE as u32 - 1);
        }
    }

    pub fn dma_write(&mut self, w: u32) {
        let mut dummy = Irq::default();
        for h in [w as u16, (w >> 16) as u16] {
            let a = self.transfer_addr;
            self.write_ram16(a, h as i16, &mut dummy);
            self.transfer_addr = (self.transfer_addr + 2) & (SPU_RAM_SIZE as u32 - 1);
        }
        self.pending_irq_from_dma |= dummy.stat != 0;
    }

    pub fn dma_read(&mut self) -> u32 {
        let lo = self.ram16(self.transfer_addr) as u16 as u32;
        self.transfer_addr = (self.transfer_addr + 2) & (SPU_RAM_SIZE as u32 - 1);
        let hi = self.ram16(self.transfer_addr) as u16 as u32;
        self.transfer_addr = (self.transfer_addr + 2) & (SPU_RAM_SIZE as u32 - 1);
        lo | (hi << 16)
    }

    fn key_on(&mut self, bits: u32) {
        self.kon |= bits;
        for i in 0..24 {
            if bits & (1 << i) != 0 {
                let v = &mut self.voices[i];
                v.addr = (v.start as u32) << 3;
                v.counter = 0;
                v.phase = Phase::Attack;
                v.env = Envelope::default();
                v.prev = [0; 2];
                v.buf = [0; 31];
                v.flags = 0;
                v.ignore_loop_addr = false;
                v.counter = 0;
                self.endx &= !(1 << i);
                // The first block is decoded on the next sample tick.
                v.needs_decode = true;
            }
        }
    }

    fn key_off(&mut self, bits: u32) {
        self.koff |= bits;
        for i in 0..24 {
            if bits & (1 << i) != 0 && self.voices[i].phase != Phase::Off {
                self.voices[i].phase = Phase::Release;
                self.voices[i].env.counter = 0;
            }
        }
    }

    // ------------------------------------------------------------------
    // Sample generation
    // ------------------------------------------------------------------

    pub fn sync(&mut self, now: u64, irq: &mut Irq, cdrom: &mut Cdrom) {
        let elapsed = now.saturating_sub(self.last_sync) + self.remainder;
        self.last_sync = now;
        let n = elapsed / CYCLES_PER_SAMPLE;
        self.remainder = elapsed % CYCLES_PER_SAMPLE;
        if self.pending_irq_from_dma {
            self.pending_irq_from_dma = false;
            irq.raise(Interrupt::Spu);
        }
        for _ in 0..n {
            self.tick(irq, cdrom);
        }
    }

    fn decode_block(&mut self, vi: usize, irq: &mut Irq) {
        let addr = self.voices[vi].addr;
        self.check_irq(addr, irq);
        let a = (addr as usize) & (SPU_RAM_SIZE - 16);
        let hdr = self.ram[a];
        let flags = self.ram[a + 1];
        let mut sh = (hdr & 0xF) as i32;
        if sh > 12 {
            sh = 9;
        }
        let shift = 12 - sh;
        let f = (((hdr >> 4) & 7) as usize).min(4);
        let v = &mut self.voices[vi];
        // keep last 3 samples as history
        v.buf.copy_within(28..31, 0);
        for j in 0..28 {
            let byte = self.ram[a + 2 + j / 2];
            let nib = if j & 1 == 0 { byte & 0xF } else { byte >> 4 };
            let t = ((nib as i8) << 4 >> 4) as i32;
            let s = (t << shift) + ((v.prev[0] * ADPCM_POS[f] + v.prev[1] * ADPCM_NEG[f] + 32) >> 6);
            let s = s.clamp(-0x8000, 0x7FFF);
            v.prev[1] = v.prev[0];
            v.prev[0] = s;
            v.buf[3 + j] = s as i16;
        }
        v.flags = flags;
        if flags & 4 != 0 && !v.ignore_loop_addr {
            v.repeat = (addr >> 3) as u16;
        }
    }

    fn tick(&mut self, irq: &mut Irq, cdrom: &mut Cdrom) {
        let gauss = gauss_table();
        let spu_on = self.ctrl & 0x8000 != 0;
        // Fast-forward: is this one of the samples the player hears?
        let ff = self.speed > 1;
        let kept = !ff || self.ff_phase == 0;
        if ff {
            self.ff_phase = (self.ff_phase + 1) % self.speed;
        }
        let mut sum = [0i32; 2];
        let mut rev = [0i32; 2];
        let mut fast = [0i32; 4];
        let mut prev_out = 0i32;
        let mut cap = [0i32; 2];
        self.tick_noise();
        for vi in 0..24 {
            let realtime = ff && self.realtime_voices & (1 << vi) != 0;
            if realtime && !kept {
                // Held between kept samples: real-time voices neither play
                // on nor sound here.
                prev_out = self.voices[vi].out;
                if vi == 1 {
                    cap[0] = prev_out;
                }
                if vi == 3 {
                    cap[1] = prev_out;
                }
                continue;
            }
            if self.voices[vi].needs_decode {
                self.voices[vi].needs_decode = false;
                self.decode_block(vi, irq);
            }
            let v = &self.voices[vi];
            let active = v.phase != Phase::Off || v.env.level > 0;
            let mut out = 0i32;
            if active && spu_on {
                let i = (v.counter >> 12) as usize;
                let interp = ((v.counter >> 4) & 0xFF) as usize;
                let sample = if self.non & (1 << vi) != 0 {
                    self.noise_level as i16 as i32
                } else {
                    let b = &v.buf;
                    let s0 = b[i] as i32;
                    let s1 = b[i + 1] as i32;
                    let s2 = b[i + 2] as i32;
                    let s3 = b[i + 3] as i32;
                    ((gauss[0xFF - interp] * s0) >> 15)
                        + ((gauss[0x1FF - interp] * s1) >> 15)
                        + ((gauss[0x100 + interp] * s2) >> 15)
                        + ((gauss[interp] * s3) >> 15)
                };
                out = (sample * v.env.level) >> 15;
            }
            let v = &mut self.voices[vi];
            v.out = out;
            let l = (out * v.vol_l.level) >> 15;
            let r = (out * v.vol_r.level) >> 15;
            let reverb = self.eon & (1 << vi) != 0;
            if ff && !realtime {
                fast[0] += l;
                fast[1] += r;
                if reverb {
                    fast[2] += l;
                    fast[3] += r;
                }
            } else {
                sum[0] += l;
                sum[1] += r;
                if reverb {
                    rev[0] += l;
                    rev[1] += r;
                }
            }
            if vi == 1 {
                cap[0] = out;
            }
            if vi == 3 {
                cap[1] = out;
            }
            // Advance pitch counter.
            let mut step = v.pitch as u32;
            if self.pmon & (1 << vi) != 0 && vi > 0 {
                let factor = (prev_out.clamp(-0x8000, 0x7FFF) + 0x8000) as i64;
                step = ((step as i64 * factor) >> 15) as u32 & 0xFFFF;
            }
            prev_out = out;
            let step = step.min(0x4000);
            if active {
                v.tick_adsr();
                v.vol_l.tick();
                v.vol_r.tick();
                v.counter += step;
                if v.counter >= 28 << 12 {
                    v.counter -= 28 << 12;
                    // Block finished: handle loop flags.
                    let flags = v.flags;
                    if flags & 1 != 0 {
                        self.endx |= 1 << vi;
                        v.addr = (v.repeat as u32) << 3;
                        if flags & 2 == 0 {
                            v.phase = Phase::Release;
                            v.env.level = 0;
                            v.phase = Phase::Off;
                        }
                    } else {
                        v.addr = (v.addr + 16) & (SPU_RAM_SIZE as u32 - 1);
                    }
                    self.decode_block(vi, irq);
                }
            }
        }
        self.kon = 0;
        self.koff = 0;

        // CD audio
        let (cd_l, cd_r) = cdrom.next_audio_sample();
        let (mut cdo_l, mut cdo_r) = (0, 0);
        if self.ctrl & 1 != 0 {
            cdo_l = (cd_l * self.cd_vol[0] as i32) >> 15;
            cdo_r = (cd_r * self.cd_vol[1] as i32) >> 15;
        }
        // Capture buffers: CD left/right, voice 1, voice 3.
        let cp = self.capture_pos * 2;
        self.write_ram16(cp, cd_l as i16, irq);
        self.write_ram16(0x400 + cp, cd_r as i16, irq);
        self.write_ram16(0x800 + cp, cap[0].clamp(-0x8000, 0x7FFF) as i16, irq);
        self.write_ram16(0xC00 + cp, cap[1].clamp(-0x8000, 0x7FFF) as i16, irq);
        self.capture_pos = (self.capture_pos + 1) & 0x1FF;
        if ff {
            // Sped-up voices and CD audio: averaged over the samples since
            // the last kept one, which only the kept sample mixes in.
            for (a, x) in self.ff_acc.iter_mut().zip([fast[0], fast[1], fast[2], fast[3], cdo_l, cdo_r]) {
                *a += x;
            }
            self.main_l.tick();
            self.main_r.tick();
            if !kept {
                return;
            }
            let n = self.speed as i32;
            let acc = std::mem::take(&mut self.ff_acc);
            sum[0] += acc[0] / n;
            sum[1] += acc[1] / n;
            rev[0] += acc[2] / n;
            rev[1] += acc[3] / n;
            cdo_l = acc[4] / n;
            cdo_r = acc[5] / n;
        }
        if self.ctrl & 1 != 0 && self.ctrl & 4 != 0 {
            rev[0] += cdo_l;
            rev[1] += cdo_r;
        }

        let (rl, rr) = self.process_reverb(rev[0].clamp(-0x8000, 0x7FFF), rev[1].clamp(-0x8000, 0x7FFF), irq);

        let unmuted = self.ctrl & 0x4000 != 0;
        let mut l = if unmuted { sum[0] } else { 0 };
        let mut r = if unmuted { sum[1] } else { 0 };
        l += cdo_l + rl;
        r += cdo_r + rr;
        let l = l.clamp(-0x8000, 0x7FFF);
        let r = r.clamp(-0x8000, 0x7FFF);
        if !ff {
            self.main_l.tick();
            self.main_r.tick();
        }
        let l = ((l * self.main_l.level) >> 15).clamp(-0x8000, 0x7FFF);
        let r = ((r * self.main_r.level) >> 15).clamp(-0x8000, 0x7FFF);
        if self.output_enabled {
            self.output.push(l as i16);
            self.output.push(r as i16);
        }
    }

    fn tick_noise(&mut self) {
        let shift = ((self.ctrl >> 10) & 0xF) as i32;
        let step = ((self.ctrl >> 8) & 3) as i32 + 4;
        self.noise_timer -= step;
        let n = self.noise_level;
        let parity = ((n >> 15) ^ (n >> 12) ^ (n >> 11) ^ (n >> 10) ^ 1) & 1;
        if self.noise_timer < 0 {
            self.noise_level = (n << 1) | parity;
            self.noise_timer += 0x20000 >> shift;
            if self.noise_timer < 0 {
                self.noise_timer += 0x20000 >> shift;
            }
        }
    }

    // ------------------------------------------------------------------
    // Reverb
    // ------------------------------------------------------------------

    fn rv_addr(&self, rel: u32) -> u32 {
        let base = self.reverb.base;
        let size = SPU_RAM_SIZE as u32 - base;
        if size == 0 {
            return base;
        }
        let off = (self.reverb.cur - base + rel) % size;
        base + off
    }

    fn rv_read(&self, reg_addr: u16, minus: i32) -> i32 {
        let rel = ((reg_addr as u32) << 3).wrapping_add((minus * 2) as u32);
        self.ram16(self.rv_addr(rel & 0x7FFFE)) as i32
    }

    fn rv_write(&mut self, reg_addr: u16, v: i32, irq: &mut Irq) {
        if self.ctrl & 0x80 == 0 {
            return;
        }
        let a = self.rv_addr((reg_addr as u32) << 3);
        self.write_ram16(a, v.clamp(-0x8000, 0x7FFF) as i16, irq);
    }

    fn process_reverb(&mut self, in_l: i32, in_r: i32, irq: &mut Irq) -> (i32, i32) {
        // Downsample input through the FIR.
        let rv = &mut self.reverb;
        rv.in_l[rv.in_pos & 63] = in_l;
        rv.in_r[rv.in_pos & 63] = in_r;
        rv.in_pos += 1;
        rv.odd = !rv.odd;
        if rv.odd {
            let fir = |buf: &[i32; 64], pos: usize| -> i32 {
                let mut acc = 0i64;
                for (k, c) in REVERB_FIR.iter().enumerate() {
                    acc += *c as i64 * buf[(pos + 64 - 39 + k) & 63] as i64;
                }
                (acc >> 15) as i32
            };
            let lin = fir(&rv.in_l, rv.in_pos);
            let rin = fir(&rv.in_r, rv.in_pos);
            let (lo, ro) = self.reverb_core(lin, rin, irq);
            let rv = &mut self.reverb;
            rv.up_l[rv.up_pos & 63] = lo;
            rv.up_r[rv.up_pos & 63] = ro;
            rv.up_pos += 1;
            rv.up_l[rv.up_pos & 63] = 0;
            rv.up_r[rv.up_pos & 63] = 0;
            rv.up_pos += 1;
        }
        let rv = &self.reverb;
        let up = |buf: &[i32; 64]| -> i32 {
            let mut acc = 0i64;
            for (k, c) in REVERB_FIR.iter().enumerate() {
                acc += *c as i64 * buf[(rv.up_pos + 64 - 39 + k) & 63] as i64;
            }
            (acc >> 14) as i32
        };
        let l = up(&rv.up_l);
        let r = up(&rv.up_r);
        let l = (l.clamp(-0x8000, 0x7FFF) * self.reverb_vol_l as i32) >> 15;
        let r = (r.clamp(-0x8000, 0x7FFF) * self.reverb_vol_r as i32) >> 15;
        (l, r)
    }

    fn reverb_core(&mut self, lin_raw: i32, rin_raw: i32, irq: &mut Irq) -> (i32, i32) {
        let g = |i: usize, s: &Self| s.reverb.regs[i];
        let s = &*self;
        let d_apf1 = g(0, s);
        let d_apf2 = g(1, s);
        let v_iir = g(2, s) as i16 as i32;
        let v_comb = [g(3, s) as i16 as i32, g(4, s) as i16 as i32, g(5, s) as i16 as i32, g(6, s) as i16 as i32];
        let v_wall = g(7, s) as i16 as i32;
        let v_apf1 = g(8, s) as i16 as i32;
        let v_apf2 = g(9, s) as i16 as i32;
        let m_lsame = g(10, s);
        let m_rsame = g(11, s);
        let m_lcomb = [g(12, s), g(14, s), g(20, s), g(22, s)];
        let m_rcomb = [g(13, s), g(15, s), g(21, s), g(23, s)];
        let d_lsame = g(16, s);
        let d_rsame = g(17, s);
        let m_ldiff = g(18, s);
        let m_rdiff = g(19, s);
        let d_ldiff = g(24, s);
        let d_rdiff = g(25, s);
        let m_lapf1 = g(26, s);
        let m_rapf1 = g(27, s);
        let m_lapf2 = g(28, s);
        let m_rapf2 = g(29, s);
        let v_lin = g(30, s) as i16 as i32;
        let v_rin = g(31, s) as i16 as i32;

        let mul = |a: i32, b: i32| (a * b) >> 15;
        let sat = |v: i32| v.clamp(-0x8000, 0x7FFF);
        let lin = mul(lin_raw, v_lin);
        let rin = mul(rin_raw, v_rin);

        // Same-side reflection
        let ls = sat(mul(sat(lin + mul(self.rv_read(d_lsame, 0), v_wall) - self.rv_read(m_lsame, -1)), v_iir) + self.rv_read(m_lsame, -1));
        let rs = sat(mul(sat(rin + mul(self.rv_read(d_rsame, 0), v_wall) - self.rv_read(m_rsame, -1)), v_iir) + self.rv_read(m_rsame, -1));
        self.rv_write(m_lsame, ls, irq);
        self.rv_write(m_rsame, rs, irq);
        // Different-side reflection
        let ld = sat(mul(sat(lin + mul(self.rv_read(d_rdiff, 0), v_wall) - self.rv_read(m_ldiff, -1)), v_iir) + self.rv_read(m_ldiff, -1));
        let rd = sat(mul(sat(rin + mul(self.rv_read(d_ldiff, 0), v_wall) - self.rv_read(m_rdiff, -1)), v_iir) + self.rv_read(m_rdiff, -1));
        self.rv_write(m_ldiff, ld, irq);
        self.rv_write(m_rdiff, rd, irq);
        // Early echo
        let mut lout = 0;
        let mut rout = 0;
        for k in 0..4 {
            lout += mul(v_comb[k], self.rv_read(m_lcomb[k], 0));
            rout += mul(v_comb[k], self.rv_read(m_rcomb[k], 0));
        }
        let lout = sat(lout);
        let rout = sat(rout);
        // APF1
        let ap = |s: &Self, m: u16, d: u16| s.rv_read(m.wrapping_sub(d), 0);
        let l1 = sat(lout - mul(v_apf1, ap(self, m_lapf1, d_apf1)));
        let r1 = sat(rout - mul(v_apf1, ap(self, m_rapf1, d_apf1)));
        self.rv_write(m_lapf1, l1, irq);
        self.rv_write(m_rapf1, r1, irq);
        let lout = sat(mul(l1, v_apf1) + ap(self, m_lapf1, d_apf1));
        let rout = sat(mul(r1, v_apf1) + ap(self, m_rapf1, d_apf1));
        // APF2
        let l2 = sat(lout - mul(v_apf2, ap(self, m_lapf2, d_apf2)));
        let r2 = sat(rout - mul(v_apf2, ap(self, m_rapf2, d_apf2)));
        self.rv_write(m_lapf2, l2, irq);
        self.rv_write(m_rapf2, r2, irq);
        let lout = sat(mul(l2, v_apf2) + ap(self, m_lapf2, d_apf2));
        let rout = sat(mul(r2, v_apf2) + ap(self, m_rapf2, d_apf2));

        // Advance buffer address.
        let rv = &mut self.reverb;
        rv.cur = (rv.cur + 2) & 0x7FFFE;
        if rv.cur < rv.base {
            rv.cur = rv.base;
        }
        rv.out_l = lout;
        rv.out_r = rout;
        (lout, rout)
    }

    pub fn take_output(&mut self, out: &mut Vec<i16>) {
        out.append(&mut self.output);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A square wave: one looping ADPCM block, 14 samples high then 14 low,
    /// on voices 0 and 1 (left and right), at one source sample per tick.
    fn square_spu() -> Spu {
        let mut s = Spu::new();
        let mut irq = Irq::default();
        let a = 0x1000usize;
        s.ram[a] = 0x00; // shift 12, filter 0
        s.ram[a + 1] = 0x07; // loop start + end + repeat
        for j in 0..14 {
            // Two 4-bit samples per byte: 7 (high), then -7 (low).
            s.ram[a + 2 + j] = if j < 7 { 0x77 } else { 0x99 };
        }
        s.write(0x1AA, 0xC000, &mut irq); // on, unmuted
        s.write(0x180, 0x3FFF, &mut irq);
        s.write(0x182, 0x3FFF, &mut irq);
        for (vi, (l, r)) in [(0x3FFFu16, 0u16), (0, 0x3FFF)].into_iter().enumerate() {
            let b = vi as u32 * 0x10;
            s.write(b, l, &mut irq);
            s.write(b + 2, r, &mut irq);
            s.write(b + 4, 0x1000, &mut irq);
            s.write(b + 6, (a >> 3) as u16, &mut irq);
            s.write(b + 8, 0x000F, &mut irq); // fastest attack, full sustain
            s.write(b + 0xA, 0x0000, &mut irq);
        }
        s.write(0x188, 0b11, &mut irq);
        s
    }

    /// Output samples (per channel) and sign changes per channel over `ticks`.
    fn run(s: &mut Spu, ticks: u64) -> (usize, [usize; 2]) {
        let (mut irq, mut cd) = (Irq::default(), Cdrom::new());
        s.sync(s.last_sync + ticks * CYCLES_PER_SAMPLE, &mut irq, &mut cd);
        let mut out = Vec::new();
        s.take_output(&mut out);
        let mut flips = [0; 2];
        for c in 0..2 {
            let ch: Vec<i16> = out.iter().skip(c).step_by(2).copied().skip(64).collect();
            flips[c] = ch.windows(2).filter(|w| (w[0] >= 0) != (w[1] >= 0)).count();
        }
        (out.len() / 2, flips)
    }

    #[test]
    fn fast_forward_keeps_real_time_voices() {
        // Real time: 2800 ticks -> 2800 samples, 100 periods (200 sign changes).
        let mut s = square_spu();
        let (n, f) = run(&mut s, 2800 + 64);
        assert_eq!(n, 2864);
        assert!((198..=201).contains(&f[0]) && (198..=201).contains(&f[1]), "{f:?}");

        // Twice as fast, voice 0 (left) in real time: half as many samples
        // come out; the left channel keeps its pitch, the right doubles it.
        let mut s = square_spu();
        s.set_speed(2);
        s.realtime_voices = 1;
        let (n, f) = run(&mut s, 2 * (2800 + 64));
        assert_eq!(n, 2864);
        assert!((198..=201).contains(&f[0]), "real-time voice: {f:?}");
        assert!((396..=401).contains(&f[1]), "sped-up voice: {f:?}");
    }
}

crate::sync_enum!(Phase { Attack = 0, Decay = 1, Sustain = 2, Release = 3, Off = 4 });

impl crate::state::Field for Envelope {
    fn sync(&mut self, s: &mut crate::state::Sync) {
        crate::sync_fields!(self, s; level, counter);
    }
}

impl crate::state::Field for Volume {
    fn sync(&mut self, s: &mut crate::state::Sync) {
        crate::sync_fields!(self, s; reg, level, env);
    }
}

impl crate::state::Field for Voice {
    fn sync(&mut self, s: &mut crate::state::Sync) {
        crate::sync_fields!(
            self, s; vol_l, vol_r, pitch, start, repeat, adsr, addr, counter, buf, prev, phase, env, flags, out, ignore_loop_addr, needs_decode
        );
    }
}

impl crate::state::Field for Reverb {
    fn sync(&mut self, s: &mut crate::state::Sync) {
        crate::sync_fields!(self, s; regs, base, cur, in_l, in_r, in_pos, up_l, up_r, up_pos, out_l, out_r, odd);
    }
}

/// Audio already produced (`output`) belongs to the host, and the speed
/// settings (`speed`, `realtime_voices`) to the mods running now.
impl crate::state::Field for Spu {
    fn sync(&mut self, s: &mut crate::state::Sync) {
        crate::sync_fields!(
            self, s; ram, voices, main_l, main_r, reverb_vol_l, reverb_vol_r, kon, koff, pmon, non, eon, endx, ctrl, stat_irq, irq_addr,
            transfer_addr_reg, transfer_addr, transfer_ctrl, fifo, cd_vol, ext_vol, unknown, noise_level, noise_timer, capture_pos, reverb,
            last_sync, remainder, pending_irq_from_dma, ff_phase, ff_acc
        );
    }
}
