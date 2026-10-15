//! CD-ROM controller (CXD1199 + HC05 microcontroller behaviour), including
//! command timing, sector delivery, XA-ADPCM decoding and the CD audio
//! volume matrix.

use crate::bus::NEVER;
use crate::irq::{Interrupt, Irq};
use crate::spu::Spu;
use crate::CPU_CLOCK;
use dwrc_disc::msf::{bcd_to_bin, bin_to_bcd, Msf};
use dwrc_disc::{SectorSource, RAW_SECTOR_SIZE};
use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscRegion {
    NtscU,
    NtscJ,
    Pal,
}

impl DiscRegion {
    fn letter(self) -> u8 {
        match self {
            DiscRegion::NtscU => b'A',
            DiscRegion::NtscJ => b'I',
            DiscRegion::Pal => b'E',
        }
    }
}

const STAT_ERROR: u8 = 0x01;
const STAT_MOTOR: u8 = 0x02;
const STAT_READING: u8 = 0x20;
const STAT_SEEKING: u8 = 0x40;
const STAT_PLAYING: u8 = 0x80;

const INT1: u8 = 1;
const INT2: u8 = 2;
const INT3: u8 = 3;
const INT5: u8 = 5;

const ACK_DELAY: u64 = 25_000;
const ACK_DELAY_INIT: u64 = 80_000;
/// Delay between acknowledging an interrupt and the next queued one.
const NEXT_IRQ_DELAY: u64 = 1_500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum After {
    Read,
    Play,
    SeekOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Drive {
    Idle,
    Seeking { target: u32, after: After, done: u64 },
    Reading { next: u64 },
    Playing { next: u64 },
}

#[derive(Debug, Clone)]
struct Response {
    irq: u8,
    data: Vec<u8>,
    at: u64,
}

pub struct Cdrom {
    disc: Option<Box<dyn SectorSource>>,
    region: DiscRegion,
    index: u8,
    params: VecDeque<u8>,
    response: VecDeque<u8>,
    irq_enable: u8,
    irq_flags: u8,
    pending_cmd: Option<(u8, u64)>,
    queue: VecDeque<Response>,
    last_ack: u64,
    stat: u8,
    mode: u8,
    setloc: u32,
    setloc_pending: bool,
    pos: u32,
    drive: Drive,
    filter_file: u8,
    filter_channel: u8,
    // Sector data
    sector: Box<[u8; RAW_SECTOR_SIZE]>,
    sector_valid: bool,
    last_header: [u8; 8],
    have_header: bool,
    data_fifo: Vec<u8>,
    data_pos: usize,
    request: u8,
    // Audio
    pub xa: XaDecoder,
    pub muted: bool,
    adpcm_muted: bool,
    vol_pending: [u8; 4],
    pub vol: [u8; 4],
    last_sync: u64,
}

impl Default for Cdrom {
    fn default() -> Self {
        Self::new()
    }
}

impl Cdrom {
    pub fn new() -> Self {
        Self {
            disc: None,
            region: DiscRegion::NtscU,
            index: 0,
            params: VecDeque::new(),
            response: VecDeque::new(),
            irq_enable: 0,
            irq_flags: 0,
            pending_cmd: None,
            queue: VecDeque::new(),
            last_ack: 0,
            stat: 0,
            mode: 0,
            setloc: 150,
            setloc_pending: false,
            pos: 150,
            drive: Drive::Idle,
            filter_file: 0,
            filter_channel: 0,
            sector: Box::new([0; RAW_SECTOR_SIZE]),
            sector_valid: false,
            last_header: [0; 8],
            have_header: false,
            data_fifo: Vec::new(),
            data_pos: 0,
            request: 0,
            xa: XaDecoder::default(),
            muted: false,
            adpcm_muted: false,
            vol_pending: [0x80, 0, 0x80, 0],
            vol: [0x80, 0, 0x80, 0],
            last_sync: 0,
        }
    }

    pub fn reset(&mut self) {
        let disc = self.disc.take();
        let region = self.region;
        *self = Self::new();
        self.disc = disc;
        self.region = region;
        if self.disc.is_some() {
            self.stat = STAT_MOTOR;
        }
    }

    pub fn insert_disc(&mut self, disc: Box<dyn SectorSource>, region: DiscRegion) {
        self.disc = Some(disc);
        self.region = region;
        self.stat = STAT_MOTOR;
    }

    /// Leaves the controller as the BIOS shell does after booting a disc
    /// (used by fast boot, which skips the shell).
    pub fn bios_boot_state(&mut self) {
        self.irq_enable = 0x1F;
        self.stat = STAT_MOTOR;
        self.mode = 0x80;
    }

    pub fn debug_state(&self) -> String {
        format!(
            "index {} en {:02x} flags {:02x} stat {:02x} mode {:02x} filter {}/{} pos {} drive {:?} queue {} params {} response {} fifo {}/{} sector_valid {} request {:02x} vol {:?}",
            self.index, self.irq_enable, self.irq_flags, self.stat, self.mode, self.filter_file, self.filter_channel, self.pos,
            self.drive, self.queue.len(), self.params.len(), self.response.len(), self.data_pos, self.data_fifo.len(),
            self.sector_valid, self.request, self.vol
        )
    }

    pub fn has_disc(&self) -> bool {
        self.disc.is_some()
    }

    fn double_speed(&self) -> bool {
        self.mode & 0x80 != 0
    }

    fn read_period(&self) -> u64 {
        if self.double_speed() {
            CPU_CLOCK / 150
        } else {
            CPU_CLOCK / 75
        }
    }

    fn seek_time(&self, from: u32, to: u32) -> u64 {
        let diff = from.abs_diff(to) as u64;
        let mut t = (diff * CPU_CLOCK / (72 * 60 * 75)).max(20_000);
        if diff >= 2250 {
            t += CPU_CLOCK * 3 / 10;
        }
        if self.stat & STAT_MOTOR == 0 {
            t += CPU_CLOCK; // spin-up
        }
        t
    }

    // ------------------------------------------------------------------
    // Register interface
    // ------------------------------------------------------------------

    fn status(&self) -> u8 {
        let mut s = self.index & 3;
        s |= (self.xa.playing() as u8) << 2;
        s |= (self.params.is_empty() as u8) << 3;
        s |= ((self.params.len() < 16) as u8) << 4;
        s |= (!self.response.is_empty() as u8) << 5;
        s |= ((self.data_pos < self.data_fifo.len()) as u8) << 6;
        s |= (self.pending_cmd.is_some() as u8) << 7;
        s
    }

    pub fn read(&mut self, reg: u32) -> u8 {
        match reg {
            0 => self.status(),
            1 => self.response.pop_front().unwrap_or(0),
            2 => self.read_data_byte(),
            _ => {
                if self.index & 1 == 0 {
                    self.irq_enable | 0xE0
                } else {
                    self.irq_flags | 0xE0
                }
            }
        }
    }

    pub fn write(&mut self, reg: u32, v: u8, now: u64, irq: &mut Irq) {
        log::trace!("cdrom reg{reg}.{} <- {v:02x}", self.index);
        match (reg, self.index) {
            (0, _) => self.index = v & 3,
            (1, 0) => {
                log::trace!("cdrom write cmd {v:02x} params {:02x?} at {now} (flags {:02x} en {:02x})", self.params, self.irq_flags, self.irq_enable);
                let delay = if v == 0x0A { ACK_DELAY_INIT } else { ACK_DELAY };
                self.pending_cmd = Some((v, now + delay));
            }
            (1, 3) => self.vol_pending[2] = v,
            (1, _) => {}
            (2, 0) => {
                if self.params.len() < 16 {
                    self.params.push_back(v);
                }
            }
            (2, 1) => {
                self.irq_enable = v & 0x1F;
                self.update_irq(irq);
            }
            (2, 2) => self.vol_pending[0] = v,
            (2, 3) => self.vol_pending[3] = v,
            (3, 0) => {
                self.request = v;
                if v & 0x80 != 0 {
                    if self.data_pos >= self.data_fifo.len() {
                        self.load_data_fifo();
                    }
                } else {
                    self.data_fifo.clear();
                    self.data_pos = 0;
                }
            }
            (3, 1) => {
                log::trace!("cdrom ack {v:02x} flags {:02x} at {now}", self.irq_flags);
                self.irq_flags &= !(v & 0x1F);
                if v & 0x40 != 0 {
                    self.params.clear();
                }
                self.last_ack = now;
                self.update_irq(irq);
            }
            (3, 2) => self.vol_pending[1] = v,
            (3, 3) => {
                self.adpcm_muted = v & 1 != 0;
                if v & 0x20 != 0 {
                    self.vol = self.vol_pending;
                }
            }
            _ => {}
        }
    }

    fn update_irq(&mut self, irq: &mut Irq) {
        if self.irq_flags & self.irq_enable & 0x1F != 0 {
            irq.raise(Interrupt::CdRom);
        }
    }

    fn load_data_fifo(&mut self) {
        if !self.sector_valid {
            return;
        }
        self.data_fifo.clear();
        self.data_pos = 0;
        if self.mode & 0x20 != 0 {
            self.data_fifo.extend_from_slice(&self.sector[12..12 + 0x924]);
        } else {
            let off = if self.sector[15] == 1 { 16 } else { 24 };
            self.data_fifo.extend_from_slice(&self.sector[off..off + 0x800]);
        }
        self.sector_valid = false;
    }

    fn read_data_byte(&mut self) -> u8 {
        if self.data_pos < self.data_fifo.len() {
            let b = self.data_fifo[self.data_pos];
            self.data_pos += 1;
            b
        } else {
            // Reading past the end repeats the last byte block (hardware
            // returns stale data); games never rely on it.
            self.data_fifo.last().copied().unwrap_or(0)
        }
    }

    pub fn dma_read_word(&mut self) -> u32 {
        let b = [self.read_data_byte(), self.read_data_byte(), self.read_data_byte(), self.read_data_byte()];
        u32::from_le_bytes(b)
    }

    // ------------------------------------------------------------------
    // Scheduling
    // ------------------------------------------------------------------

    pub fn next_event_cycle(&self) -> u64 {
        let mut t = NEVER;
        if let Some((_, at)) = self.pending_cmd {
            t = t.min(at);
        }
        if let Some(r) = self.queue.front() {
            let earliest = if self.irq_flags != 0 { NEVER } else { r.at.max(self.last_ack + NEXT_IRQ_DELAY) };
            t = t.min(earliest);
        }
        match self.drive {
            Drive::Seeking { done, .. } => t = t.min(done),
            Drive::Reading { next } | Drive::Playing { next } => t = t.min(next),
            Drive::Idle => {}
        }
        t
    }

    pub fn sync(&mut self, now: u64, irq: &mut Irq, _spu: &mut Spu) {
        self.last_sync = now;
        // Drive mechanics first (they can queue INT1/INT2).
        loop {
            match self.drive {
                Drive::Seeking { target, after, done } if now >= done => {
                    self.pos = target;
                    self.stat &= !STAT_SEEKING;
                    match after {
                        After::Read => {
                            self.stat |= STAT_READING;
                            self.drive = Drive::Reading { next: done + self.read_period() };
                        }
                        After::Play => {
                            self.stat |= STAT_PLAYING;
                            self.drive = Drive::Playing { next: done + self.read_period() };
                        }
                        After::SeekOnly => {
                            self.drive = Drive::Idle;
                            self.push(INT2, vec![self.stat], done);
                        }
                    }
                }
                Drive::Reading { next } if now >= next => {
                    self.read_sector(next);
                    if let Drive::Reading { .. } = self.drive {
                        self.drive = Drive::Reading { next: next + self.read_period() };
                    }
                }
                Drive::Playing { next } if now >= next => {
                    // No CD-DA tracks on the supported discs: advance silently.
                    self.pos += 1;
                    self.drive = Drive::Playing { next: next + self.read_period() };
                }
                _ => break,
            }
        }
        if let Some((cmd, at)) = self.pending_cmd {
            if now >= at {
                self.pending_cmd = None;
                self.execute(cmd, at);
                self.params.clear();
            }
        }
        // Deliver queued interrupts.
        if self.irq_flags == 0 {
            if let Some(r) = self.queue.front() {
                if now >= r.at.max(self.last_ack + NEXT_IRQ_DELAY) {
                    let r = self.queue.pop_front().unwrap();
                    log::trace!("cdrom deliver INT{} {:02x?} at {now}", r.irq, r.data);
                    self.response.clear();
                    self.response.extend(r.data);
                    self.irq_flags = r.irq;
                    self.update_irq(irq);
                }
            }
        }
    }

    fn push(&mut self, irq: u8, data: Vec<u8>, at: u64) {
        if irq == INT1 {
            // Only the newest data-ready notification is kept.
            if let Some(i) = self.queue.iter().position(|r| r.irq == INT1) {
                self.queue.remove(i);
            }
        }
        self.queue.push_back(Response { irq, data, at });
    }

    fn param(&self, i: usize) -> u8 {
        self.params.get(i).copied().unwrap_or(0)
    }

    fn error(&mut self, code: u8, at: u64) {
        self.push(INT5, vec![self.stat | STAT_ERROR, code], at);
    }

    fn stop_reading(&mut self) {
        self.drive = Drive::Idle;
        self.stat &= !(STAT_READING | STAT_SEEKING | STAT_PLAYING);
    }

    fn execute(&mut self, cmd: u8, now: u64) {
        log::trace!("cdrom cmd {cmd:02x} params {:02x?}", self.params);
        // Commands issued while the shell is "open" or without a disc.
        if self.disc.is_none() && !matches!(cmd, 0x01 | 0x0A | 0x19 | 0x1A | 0x0E) {
            self.push(INT5, vec![0x11, 0x80], now);
            return;
        }
        match cmd {
            0x01 => {
                self.push(INT3, vec![self.stat], now);
                self.stat &= !0x10;
            }
            0x02 => {
                let m = Msf::from_bcd(self.param(0), self.param(1), self.param(2));
                self.setloc = m.to_index();
                self.setloc_pending = true;
                self.push(INT3, vec![self.stat], now);
            }
            0x03 => {
                self.push(INT3, vec![self.stat], now);
                let target = if self.setloc_pending { self.setloc } else { self.pos };
                self.setloc_pending = false;
                self.stat |= STAT_MOTOR;
                self.drive = Drive::Seeking { target, after: After::Play, done: now + self.seek_time(self.pos, target) };
            }
            0x04 | 0x05 => self.push(INT3, vec![self.stat], now),
            0x06 | 0x1B => {
                self.push(INT3, vec![self.stat], now);
                self.start_read(now);
            }
            0x07 => {
                self.push(INT3, vec![self.stat], now);
                self.stat |= STAT_MOTOR;
                self.push(INT2, vec![self.stat], now + CPU_CLOCK / 10);
            }
            0x08 => {
                let was_on = self.stat & STAT_MOTOR != 0;
                self.stop_reading();
                self.push(INT3, vec![self.stat], now);
                self.stat &= !STAT_MOTOR;
                let t = if was_on { CPU_CLOCK / 10 } else { 7000 };
                self.push(INT2, vec![self.stat], now + t);
            }
            0x09 => {
                let was_reading = matches!(self.drive, Drive::Reading { .. } | Drive::Playing { .. } | Drive::Seeking { .. });
                self.push(INT3, vec![self.stat], now);
                let t = if was_reading { self.read_period() } else { 7000 };
                self.stop_reading();
                self.push(INT2, vec![self.stat], now + t);
            }
            0x0A => {
                self.stop_reading();
                self.mode = 0x20;
                self.stat = STAT_MOTOR;
                self.push(INT3, vec![self.stat], now);
                self.push(INT2, vec![self.stat], now + 120_000);
            }
            0x0B => {
                self.muted = true;
                self.push(INT3, vec![self.stat], now);
            }
            0x0C => {
                self.muted = false;
                self.push(INT3, vec![self.stat], now);
            }
            0x0D => {
                self.filter_file = self.param(0);
                self.filter_channel = self.param(1);
                self.push(INT3, vec![self.stat], now);
            }
            0x0E => {
                self.mode = self.param(0);
                self.push(INT3, vec![self.stat], now);
            }
            0x0F => self.push(INT3, vec![self.stat, self.mode, 0, self.filter_file, self.filter_channel], now),
            0x10 => {
                if self.have_header {
                    let h = self.last_header;
                    self.push(INT3, h.to_vec(), now);
                } else {
                    self.error(0x80, now);
                }
            }
            0x11 => {
                let track_start = self.disc.as_ref().and_then(|d| d.tracks().first().map(|t| t.start)).unwrap_or(150);
                let rel = Msf::from_index(self.pos.saturating_sub(track_start));
                let abs = Msf::from_index(self.pos);
                let r = rel.to_bcd();
                let a = abs.to_bcd();
                self.push(INT3, vec![0x01, 0x01, r[0], r[1], r[2], a[0], a[1], a[2]], now);
            }
            0x12 => {
                self.push(INT3, vec![self.stat], now);
                self.push(INT2, vec![self.stat], now + CPU_CLOCK / 10);
            }
            0x13 => {
                let n = self.disc.as_ref().map(|d| d.tracks().len()).unwrap_or(1) as u8;
                self.push(INT3, vec![self.stat, 0x01, bin_to_bcd(n)], now);
            }
            0x14 => {
                let t = bcd_to_bin(self.param(0)) as usize;
                let disc = self.disc.as_ref().unwrap();
                let idx = if t == 0 {
                    Some(disc.lead_out())
                } else {
                    disc.tracks().get(t - 1).map(|tr| tr.start)
                };
                match idx {
                    Some(i) => {
                        let m = Msf::from_index(i).to_bcd();
                        self.push(INT3, vec![self.stat, m[0], m[1]], now);
                    }
                    None => self.error(0x10, now),
                }
            }
            0x15 | 0x16 => {
                self.stop_reading();
                self.push(INT3, vec![self.stat], now);
                let target = self.setloc;
                self.setloc_pending = false;
                self.stat |= STAT_SEEKING | STAT_MOTOR;
                self.drive = Drive::Seeking { target, after: After::SeekOnly, done: now + self.seek_time(self.pos, target) };
            }
            0x19 => match self.param(0) {
                0x20 => self.push(INT3, vec![0x94, 0x09, 0x19, 0xC0], now),
                0x04 | 0x05 => self.push(INT3, vec![self.stat], now),
                0x22 => self.push(INT3, b"for U/C".to_vec(), now),
                _ => self.error(0x10, now),
            },
            0x1A => {
                self.push(INT3, vec![self.stat], now);
                if self.disc.is_some() {
                    let r = self.region.letter();
                    self.push(INT2, vec![STAT_MOTOR, 0x00, 0x20, 0x00, b'S', b'C', b'E', r], now + 30_000);
                } else {
                    self.push(INT5, vec![0x08, 0x40, 0, 0, 0, 0, 0, 0], now + 30_000);
                }
            }
            0x1C => {
                self.stop_reading();
                self.push(INT3, vec![self.stat], now);
            }
            0x1D => self.push(INT3, vec![self.stat], now),
            0x1E => {
                self.push(INT3, vec![self.stat], now);
                self.push(INT2, vec![self.stat], now + CPU_CLOCK / 2);
            }
            _ => {
                log::debug!("unknown CD-ROM command {cmd:02x}");
                self.error(0x40, now);
            }
        }
    }

    fn start_read(&mut self, now: u64) {
        self.stat |= STAT_MOTOR;
        self.xa.reset_filter();
        if self.setloc_pending {
            self.setloc_pending = false;
            let target = self.setloc;
            if let Drive::Reading { .. } = self.drive {
                if target == self.pos {
                    return;
                }
            }
            self.stat &= !STAT_READING;
            self.stat |= STAT_SEEKING;
            self.drive = Drive::Seeking { target, after: After::Read, done: now + self.seek_time(self.pos, target) };
        } else if let Drive::Reading { .. } = self.drive {
            // Already reading: keep going.
        } else {
            self.stat |= STAT_READING;
            self.drive = Drive::Reading { next: now + self.read_period() };
        }
    }

    fn read_sector(&mut self, at: u64) {
        let Some(disc) = self.disc.as_mut() else { return };
        let mut buf = [0u8; RAW_SECTOR_SIZE];
        if disc.read_raw(self.pos, &mut buf).is_err() {
            self.stop_reading();
            self.error(0x04, at);
            return;
        }
        self.pos += 1;
        self.last_header.copy_from_slice(&buf[12..20]);
        self.have_header = true;
        let submode = buf[18];
        let is_mode2 = buf[15] == 2;
        // XA-ADPCM real-time audio sectors are routed to the audio decoder.
        if self.mode & 0x40 != 0 && is_mode2 && submode & 0x04 != 0 && submode & 0x40 != 0 {
            let file = buf[16];
            let channel = buf[17];
            if self.mode & 0x08 != 0 && (file != self.filter_file || channel != self.filter_channel) {
                return;
            }
            if !self.adpcm_muted && !self.muted {
                self.xa.decode_sector(&buf);
            } else {
                self.xa.decode_sector_silent(&buf);
            }
            return;
        }
        // With the filter enabled, real-time sectors of other channels are skipped.
        if self.mode & 0x08 != 0 && is_mode2 && submode & 0x40 != 0 {
            let (file, channel) = (buf[16], buf[17]);
            if file != self.filter_file || channel != self.filter_channel {
                return;
            }
        }
        self.sector.copy_from_slice(&buf);
        self.sector_valid = true;
        self.push(INT1, vec![self.stat], at);
    }

    /// Next CD audio sample pair after the volume matrix (for the SPU).
    pub fn next_audio_sample(&mut self) -> (i32, i32) {
        let (l, r) = self.xa.next_sample();
        if self.muted {
            return (0, 0);
        }
        let (l, r) = (l as i32, r as i32);
        let ol = (l * self.vol[0] as i32 + r * self.vol[3] as i32) >> 7;
        let or = (r * self.vol[2] as i32 + l * self.vol[1] as i32) >> 7;
        (ol.clamp(-0x8000, 0x7FFF), or.clamp(-0x8000, 0x7FFF))
    }
}

// ----------------------------------------------------------------------
// XA-ADPCM
// ----------------------------------------------------------------------

const XA_POS: [i32; 4] = [0, 60, 115, 98];
const XA_NEG: [i32; 4] = [0, 0, -52, -55];

/// Decodes XA-ADPCM sectors and resamples them to 44.1 kHz using the
/// hardware's scheme: 7 output samples per 6 input samples through a
/// 7-phase 29-tap interpolation filter (18.9 kHz streams have every sample
/// doubled first). The filter coefficients are a windowed-sinc design with
/// the same structure as the console's "zigzag" tables.
pub struct XaDecoder {
    prev: [[i32; 2]; 2],
    ring: [[i16; 64]; 2],
    in_count: u64,
    out_count: u64,
    out: VecDeque<(i16, i16)>,
    stereo: bool,
    filter: Vec<[f32; 29]>,
}

impl Default for XaDecoder {
    fn default() -> Self {
        Self {
            prev: [[0; 2]; 2],
            ring: [[0; 64]; 2],
            in_count: 0,
            out_count: 0,
            out: VecDeque::new(),
            stereo: false,
            filter: build_zigzag_filter(),
        }
    }
}

fn build_zigzag_filter() -> Vec<[f32; 29]> {
    let pi = std::f32::consts::PI;
    let cutoff = 0.92f32;
    (0..7)
        .map(|q| {
            let frac = q as f32 / 7.0;
            let mut taps = [0f32; 29];
            for (k, t) in taps.iter_mut().enumerate() {
                let x = k as f32 - 14.0 - frac;
                let sinc = if x.abs() < 1e-6 { 1.0 } else { (pi * cutoff * x).sin() / (pi * cutoff * x) };
                let w = if x.abs() < 15.0 { 0.5 + 0.5 * (pi * x / 15.0).cos() } else { 0.0 };
                *t = sinc * w;
            }
            let sum: f32 = taps.iter().sum();
            for t in taps.iter_mut() {
                *t /= sum;
            }
            taps
        })
        .collect()
}

impl XaDecoder {
    pub fn playing(&self) -> bool {
        !self.out.is_empty()
    }

    pub fn reset_filter(&mut self) {
        self.prev = [[0; 2]; 2];
    }

    fn push_source(&mut self, l: i16, r: i16) {
        let i = self.in_count;
        self.ring[0][(i & 63) as usize] = l;
        self.ring[1][(i & 63) as usize] = r;
        self.in_count += 1;
        loop {
            let i0 = self.out_count * 6 / 7;
            if i0 + 14 > i {
                break;
            }
            let q = (self.out_count * 6 % 7) as usize;
            let taps = &self.filter[q];
            let mut acc = [0f32; 2];
            for (k, t) in taps.iter().enumerate() {
                let idx = (i0.wrapping_add(k as u64).wrapping_sub(14) & 63) as usize;
                acc[0] += self.ring[0][idx] as f32 * t;
                acc[1] += self.ring[1][idx] as f32 * t;
            }
            let c = |v: f32| v.round().clamp(-32768.0, 32767.0) as i16;
            self.out.push_back((c(acc[0]), c(acc[1])));
            self.out_count += 1;
        }
    }

    pub fn next_sample(&mut self) -> (i16, i16) {
        self.out.pop_front().unwrap_or((0, 0))
    }

    pub fn buffered(&self) -> usize {
        self.out.len()
    }

    pub fn decode_sector_silent(&mut self, _sector: &[u8]) {}

    pub fn decode_sector(&mut self, sector: &[u8]) {
        let coding = sector[19];
        let stereo = coding & 3 == 1;
        let half_rate = (coding >> 2) & 3 == 1;
        let eight_bit = (coding >> 4) & 3 == 1;
        if stereo != self.stereo {
            self.stereo = stereo;
        }
        let data = &sector[24..24 + 18 * 128];
        let mut samples: Vec<i16> = Vec::with_capacity(4032);
        let mut left: Vec<i16> = Vec::new();
        let mut right: Vec<i16> = Vec::new();
        for g in 0..18 {
            let grp = &data[g * 128..(g + 1) * 128];
            if eight_bit {
                for blk in 0..4 {
                    let ch = if stereo { blk & 1 } else { 0 };
                    let dst = if stereo {
                        if ch == 0 {
                            &mut left
                        } else {
                            &mut right
                        }
                    } else {
                        &mut samples
                    };
                    let hdr = grp[4 + blk];
                    let mut sh = hdr & 0xF;
                    if sh > 8 {
                        sh = 8;
                    }
                    let shift = 8 - sh as i32;
                    let f = ((hdr >> 4) & 3) as usize;
                    for j in 0..28 {
                        let t = grp[16 + blk + j * 4] as i8 as i32;
                        let p = &mut self.prev[ch];
                        let s = (t << shift) + ((p[0] * XA_POS[f] + p[1] * XA_NEG[f] + 32) >> 6);
                        let s = s.clamp(-0x8000, 0x7FFF);
                        p[1] = p[0];
                        p[0] = s;
                        dst.push(s as i16);
                    }
                }
            } else {
                for blk in 0..4 {
                    for nib in 0..2 {
                        let unit = blk * 2 + nib;
                        let ch = if stereo { unit & 1 } else { 0 };
                        let dst = if stereo {
                            if ch == 0 {
                                &mut left
                            } else {
                                &mut right
                            }
                        } else {
                            &mut samples
                        };
                        let hdr = grp[4 + blk * 2 + nib];
                        let mut sh = hdr & 0xF;
                        if sh > 12 {
                            sh = 9;
                        }
                        let shift = 12 - sh as i32;
                        let f = ((hdr >> 4) & 3) as usize;
                        for j in 0..28 {
                            let b = grp[16 + blk + j * 4];
                            let t = (((b >> (nib * 4)) & 0xF) as i8) << 4 >> 4;
                            let p = &mut self.prev[ch];
                            let s = ((t as i32) << shift) + ((p[0] * XA_POS[f] + p[1] * XA_NEG[f] + 32) >> 6);
                            let s = s.clamp(-0x8000, 0x7FFF);
                            p[1] = p[0];
                            p[0] = s;
                            dst.push(s as i16);
                        }
                    }
                }
            }
        }
        let reps = if half_rate { 2 } else { 1 };
        if stereo {
            for (l, r) in left.iter().zip(right.iter()) {
                for _ in 0..reps {
                    self.push_source(*l, *r);
                }
            }
        } else {
            for s in samples {
                for _ in 0..reps {
                    self.push_source(s, s);
                }
            }
        }
        // Keep latency bounded if the game streams faster than playback.
        while self.out.len() > 44100 / 2 {
            self.out.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filter_normalised() {
        for p in build_zigzag_filter() {
            let s: f32 = p.iter().sum();
            assert!((s - 1.0).abs() < 1e-4);
        }
    }
}
