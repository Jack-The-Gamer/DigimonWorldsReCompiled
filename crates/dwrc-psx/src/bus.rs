//! System bus: memory map, I/O dispatch, event scheduling and DMA.

use crate::cdrom::Cdrom;
use crate::cpu::Cpu;
use crate::dma::{Dma, Port};
use crate::gpu::Gpu;
use crate::irq::{Interrupt, Irq};
use crate::mdec::Mdec;
use crate::sio::Sio;
use crate::spu::Spu;
use crate::timers::Timers;
use crate::{HookAction, HookId, PcHook, VideoStandard};
use std::collections::HashMap;

pub const RAM_SIZE: usize = 2 * 1024 * 1024;
pub const BIOS_SIZE: usize = 512 * 1024;
pub const SCRATCH_SIZE: usize = 1024;

/// Scheduled event sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum Event {
    Gpu = 0,
    Timers,
    Cdrom,
    Spu,
    Sio,
    Dma,
}
const EVENT_COUNT: usize = 6;
pub const NEVER: u64 = u64::MAX;

/// Physical address of a virtual address (PS1 has no MMU, just segments).
#[inline(always)]
pub fn mask_region(addr: u32) -> u32 {
    const MASKS: [u32; 8] = [
        0xFFFF_FFFF, 0xFFFF_FFFF, 0xFFFF_FFFF, 0xFFFF_FFFF, // KUSEG
        0x7FFF_FFFF, // KSEG0
        0x1FFF_FFFF, // KSEG1
        0xFFFF_FFFF, 0xFFFF_FFFF, // KSEG2
    ];
    addr & MASKS[(addr >> 29) as usize]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Width {
    Byte = 1,
    Half = 2,
    Word = 4,
}

/// Access-time table derived from the memory control registers.
#[derive(Debug, Clone, Copy, Default)]
struct AccessTime {
    byte: u32,
    half: u32,
    word: u32,
}

impl AccessTime {
    fn get(&self, w: Width) -> u32 {
        match w {
            Width::Byte => self.byte,
            Width::Half => self.half,
            Width::Word => self.word,
        }
    }
    /// Timing formula from the nocash PSX specification.
    fn compute(delay: u32, com: u32) -> Self {
        let access = ((delay >> 4) & 0xF) as i32;
        let (com0, com2, com3) = ((com & 0xF) as i32, ((com >> 8) & 0xF) as i32, ((com >> 12) & 0xF) as i32);
        let mut first = 0i32;
        let mut seq = 0i32;
        let mut min = 0i32;
        if delay & (1 << 8) != 0 {
            first += com0 - 1;
            seq += com0 - 1;
        }
        if delay & (1 << 10) != 0 {
            first += com2;
            seq += com2;
        }
        if delay & (1 << 11) != 0 {
            min = com3;
        }
        if first < 6 {
            first += 1;
        }
        first += access + 2;
        seq += access + 2;
        if first < min + 6 {
            first = min + 6;
        }
        if seq < min + 2 {
            seq = min + 2;
        }
        let bus16 = delay & (1 << 12) != 0;
        let byte = first;
        let half = if bus16 { first } else { first + seq };
        let word = if bus16 { first + seq } else { first + 3 * seq };
        Self { byte: (byte - 1).max(0) as u32, half: (half - 1).max(0) as u32, word: (word - 1).max(0) as u32 }
    }
}

/// Code invalidation range accumulated for the execution engine.
#[derive(Default)]
struct Invalidation {
    lo: u32,
    hi: u32,
    any: bool,
}

/// A PC hook at one address.
struct Hook {
    id: HookId,
    /// Removed when it runs (`add_hook_once`).
    once: bool,
    /// Taken out while it runs.
    f: Option<PcHook>,
}

pub struct Bus {
    pub ram: Box<[u8]>,
    pub scratch: Box<[u8]>,
    pub bios: Box<[u8]>,
    pub memctrl: [u32; 9],
    pub ram_size_reg: u32,
    pub cache_ctrl: u32,
    pub irq: Irq,
    pub dma: Dma,
    pub timers: Timers,
    pub gpu: Gpu,
    pub cdrom: Cdrom,
    pub spu: Spu,
    pub mdec: Mdec,
    pub sio: Sio,

    /// Global CPU cycle counter.
    pub cycles: u64,
    /// Earliest pending event; engines run until `cycles >= next_event`.
    pub next_event: u64,
    events: [u64; EVENT_COUNT],
    cap: u64,
    pub frame_done: bool,

    bios_time: AccessTime,
    spu_time: AccessTime,
    cdrom_time: AccessTime,
    exp1_time: AccessTime,
    exp2_time: AccessTime,

    /// One bit per 4 KiB page of RAM that holds translated code.
    pub code_pages: [u64; RAM_SIZE / 4096 / 64],
    invalidation: Invalidation,

    hook_bits: Box<[u64]>,
    /// Every hook at an address, in the order they were added.
    hooks: HashMap<u32, Vec<Hook>>,
    pub has_hooks: bool,
    hooks_version: u64,
    next_hook: u64,
    /// Set by hooks/debuggers to make the engine return early.
    pub stop_requested: bool,

    /// Console TTY output captured from BIOS putchar (debugging aid).
    pub tty: String,
    pub tty_echo: bool,
    pub post: u8,
}

impl Bus {
    pub fn new(bios: &[u8], standard: VideoStandard) -> Self {
        let mut b = vec![0u8; BIOS_SIZE].into_boxed_slice();
        let n = bios.len().min(BIOS_SIZE);
        b[..n].copy_from_slice(&bios[..n]);
        let mut bus = Self {
            ram: vec![0u8; RAM_SIZE].into_boxed_slice(),
            scratch: vec![0u8; SCRATCH_SIZE].into_boxed_slice(),
            bios: b,
            memctrl: [0; 9],
            ram_size_reg: 0x0000_0B88,
            cache_ctrl: 0,
            irq: Irq::default(),
            dma: Dma::new(),
            timers: Timers::new(),
            gpu: Gpu::new(standard),
            cdrom: Cdrom::new(),
            spu: Spu::new(),
            mdec: Mdec::new(),
            sio: Sio::new(),
            cycles: 0,
            next_event: 0,
            events: [NEVER; EVENT_COUNT],
            cap: NEVER,
            frame_done: false,
            bios_time: AccessTime::default(),
            spu_time: AccessTime::default(),
            cdrom_time: AccessTime::default(),
            exp1_time: AccessTime::default(),
            exp2_time: AccessTime::default(),
            code_pages: [0; RAM_SIZE / 4096 / 64],
            invalidation: Invalidation::default(),
            hook_bits: vec![0u64; RAM_SIZE / 4 / 64].into_boxed_slice(),
            hooks: HashMap::new(),
            has_hooks: false,
            hooks_version: 0,
            next_hook: 0,
            stop_requested: false,
            tty: String::new(),
            tty_echo: false,
            post: 0,
        };
        bus.reset();
        bus
    }

    pub fn reset(&mut self) {
        self.ram.fill(0);
        self.scratch.fill(0);
        // Power-on memory control values (as left by the BIOS).
        self.memctrl = [
            0x1F00_0000, // exp1 base
            0x1F80_2000, // exp2 base
            0x0013_243F, // exp1 delay
            0x0000_3022, // exp3 delay
            0x0013_243F, // bios delay
            0x2009_31E1, // spu delay
            0x0002_0843, // cdrom delay
            0x0007_0777, // exp2 delay
            0x0003_1125, // common delay
        ];
        self.recompute_timings();
        self.cycles = 0;
        self.events = [NEVER; EVENT_COUNT];
        self.irq = Irq::default();
        self.dma = Dma::new();
        self.timers = Timers::new();
        let (std, wide, ui) = (self.gpu.standard, self.gpu.widescreen(), self.gpu.widescreen_ui());
        self.gpu = Gpu::new(std);
        self.gpu.set_widescreen_ui(ui);
        self.gpu.set_widescreen(wide);
        self.cdrom.reset();
        self.spu = Spu::new();
        self.mdec = Mdec::new();
        self.sio.reset();
        self.schedule(Event::Gpu, self.gpu.next_event_cycle(0));
        self.schedule(Event::Spu, crate::CYCLES_PER_SAMPLE * crate::spu::SAMPLES_PER_EVENT);
        self.update_next_event();
    }

    fn recompute_timings(&mut self) {
        let com = self.memctrl[8];
        self.exp1_time = AccessTime::compute(self.memctrl[2], com);
        self.bios_time = AccessTime::compute(self.memctrl[4], com);
        self.spu_time = AccessTime::compute(self.memctrl[5], com);
        self.cdrom_time = AccessTime::compute(self.memctrl[6], com);
        self.exp2_time = AccessTime::compute(self.memctrl[7], com);
    }

    /// Cycles a CPU instruction fetch from BIOS ROM costs (uncached).
    pub fn bios_fetch_cycles(&self) -> u32 {
        self.bios_time.word
    }

    // ------------------------------------------------------------------
    // Scheduling
    // ------------------------------------------------------------------

    #[inline]
    pub fn schedule(&mut self, ev: Event, at: u64) {
        self.events[ev as usize] = at;
        self.update_next_event();
    }

    #[inline]
    fn update_next_event(&mut self) {
        let mut n = self.cap;
        for &e in &self.events {
            n = n.min(e);
        }
        self.next_event = n;
    }

    /// Limits the next run slice (used by `run_cycles` and lockstep).
    pub fn cap_next_event(&mut self, at: u64) {
        self.cap = at;
        self.update_next_event();
    }

    pub fn run_events(&mut self) {
        self.cap = NEVER;
        loop {
            let now = self.cycles;
            let mut fired = false;
            for i in 0..EVENT_COUNT {
                if self.events[i] <= now {
                    self.events[i] = NEVER;
                    fired = true;
                    self.dispatch(i);
                }
            }
            if !fired {
                break;
            }
        }
        self.update_next_event();
    }

    fn dispatch(&mut self, i: usize) {
        let now = self.cycles;
        match i {
            x if x == Event::Gpu as usize => self.sync_gpu(),
            x if x == Event::Timers as usize => self.sync_timers(),
            x if x == Event::Cdrom as usize => self.sync_cdrom(),
            x if x == Event::Spu as usize => {
                self.sync_spu();
                let next = now + crate::CYCLES_PER_SAMPLE * crate::spu::SAMPLES_PER_EVENT;
                self.schedule(Event::Spu, next.min(self.spu.next_irq_cycle(now)));
            }
            x if x == Event::Sio as usize => self.sync_sio(),
            x if x == Event::Dma as usize => self.dma_event(),
            _ => {}
        }
    }

    pub fn sync_gpu(&mut self) {
        let now = self.cycles;
        let hblanks_before = self.gpu.hblank_count;
        let vblank_before = self.gpu.in_vblank();
        if self.gpu.sync(now, &mut self.irq) {
            self.frame_done = true;
        }
        let hblanks = self.gpu.hblank_count - hblanks_before;
        if hblanks > 0 || vblank_before != self.gpu.in_vblank() {
            self.sync_timers_inner(hblanks, vblank_before);
        }
        let next = self.gpu.next_event_cycle(now);
        self.schedule(Event::Gpu, next);
    }

    fn sync_timers_inner(&mut self, hblanks: u64, vblank_before: bool) {
        let now = self.cycles;
        let gpu_info = self.gpu.timer_info();
        self.timers.sync(now, &gpu_info, &mut self.irq);
        if hblanks > 0 {
            self.timers.on_hblanks(hblanks, &mut self.irq);
        }
        let vb = self.gpu.in_vblank();
        if vb != vblank_before {
            self.timers.on_vblank(vb, &mut self.irq);
        }
        let next = self.timers.next_event_cycle(now, &gpu_info);
        self.schedule(Event::Timers, next);
    }

    pub fn sync_timers(&mut self) {
        let vb = self.gpu.in_vblank();
        self.sync_timers_inner(0, vb);
    }

    pub fn sync_cdrom(&mut self) {
        let now = self.cycles;
        self.cdrom.sync(now, &mut self.irq, &mut self.spu);
        let next = self.cdrom.next_event_cycle();
        self.schedule(Event::Cdrom, next);
    }

    pub fn sync_spu(&mut self) {
        let now = self.cycles;
        // CD audio is produced by the CD-ROM side; make sure it is current.
        self.spu.sync(now, &mut self.irq, &mut self.cdrom);
    }

    pub fn sync_sio(&mut self) {
        let now = self.cycles;
        self.sio.sync(now, &mut self.irq);
        let next = self.sio.next_event_cycle();
        self.schedule(Event::Sio, next);
    }

    // ------------------------------------------------------------------
    // Hooks and code tracking
    // ------------------------------------------------------------------

    /// Adds a hook that runs whenever the CPU reaches `vaddr`, after the
    /// hooks already there (see `run_hook`).
    pub fn add_hook(&mut self, vaddr: u32, hook: PcHook) -> HookId {
        self.insert_hook(vaddr, hook, false)
    }

    /// Adds a hook that runs the next time the CPU reaches `vaddr`, and is
    /// removed as it starts.
    pub fn add_hook_once(&mut self, vaddr: u32, hook: PcHook) -> HookId {
        self.insert_hook(vaddr, hook, true)
    }

    fn insert_hook(&mut self, vaddr: u32, hook: PcHook, once: bool) -> HookId {
        let phys = mask_region(vaddr) & 0x1F_FFFF;
        let w = (phys >> 2) as usize;
        self.hook_bits[w / 64] |= 1 << (w % 64);
        let id = HookId(self.next_hook);
        self.next_hook += 1;
        self.hooks.entry(phys).or_default().push(Hook { id, once, f: Some(hook) });
        self.has_hooks = true;
        self.hooks_version += 1;
        id
    }

    /// Removes one hook; the others at its address stay. A hook may remove
    /// itself (or another) while it runs.
    pub fn remove_hook(&mut self, id: HookId) {
        let Some((&phys, list)) = self.hooks.iter_mut().find(|(_, l)| l.iter().any(|h| h.id == id)) else { return };
        list.retain(|h| h.id != id);
        if list.is_empty() {
            self.hooks.remove(&phys);
            let w = (phys >> 2) as usize;
            self.hook_bits[w / 64] &= !(1 << (w % 64));
        }
        self.has_hooks = !self.hooks.is_empty();
        self.hooks_version += 1;
    }

    /// Whether the hook is still installed.
    pub fn has_hook(&self, id: HookId) -> bool {
        self.hooks.values().any(|l| l.iter().any(|h| h.id == id))
    }

    #[inline(always)]
    pub fn hook_at(&self, pc: u32) -> bool {
        let phys = mask_region(pc);
        if phys >= 0x80_0000 {
            return false;
        }
        let w = ((phys & 0x1F_FFFF) >> 2) as usize;
        self.hook_bits[w / 64] & (1 << (w % 64)) != 0
    }

    /// Runs the hooks at `pc` in the order they were added, and returns
    /// `Redirect` if one of them moved the PC. The hooks after that one
    /// don't run: the CPU is no longer at their address. Hooks added while
    /// these run wait for the next time `pc` is reached; hooks removed
    /// meanwhile don't run.
    pub fn run_hook(&mut self, cpu: &mut Cpu, pc: u32) -> HookAction {
        let phys = mask_region(pc) & 0x1F_FFFF;
        let end = self.next_hook;
        let mut next = 0;
        while let Some((id, once, mut f)) = self.take_hook(phys, next, end) {
            next = id.0 + 1;
            if once {
                self.remove_hook(id);
            }
            let r = f(cpu, self);
            // The hook may have removed itself.
            if let Some(h) = self.hooks.get_mut(&phys).and_then(|l| l.iter_mut().find(|h| h.id == id)) {
                h.f = Some(f);
            }
            if r == HookAction::Redirect {
                return r;
            }
        }
        HookAction::Continue
    }

    /// Takes out the first hook at `phys` numbered from `from` up to (not
    /// including) `end`. Lists are in id order.
    fn take_hook(&mut self, phys: u32, from: u64, end: u64) -> Option<(HookId, bool, PcHook)> {
        let h = self.hooks.get_mut(&phys)?.iter_mut().find(|h| h.id.0 >= from).filter(|h| h.id.0 < end)?;
        Some((h.id, h.once, h.f.take()?))
    }

    pub fn hook_addresses(&self) -> Vec<u32> {
        self.hooks.keys().map(|p| 0x8000_0000 | p).collect()
    }

    #[inline(always)]
    pub fn note_code_write(&mut self, vaddr: u32, len: u32) {
        let phys = mask_region(vaddr);
        if phys as usize >= 0x80_0000 {
            return;
        }
        let start = phys & 0x1F_FFFF;
        let end = start.saturating_add(len.max(1)).min(RAM_SIZE as u32);
        let mut p = start >> 12;
        let mut hit = false;
        while p <= (end - 1) >> 12 {
            if self.code_pages[(p / 64) as usize] & (1 << (p % 64)) != 0 {
                hit = true;
                break;
            }
            p += 1;
        }
        if hit {
            let inv = &mut self.invalidation;
            if !inv.any {
                inv.lo = start;
                inv.hi = end;
                inv.any = true;
            } else {
                inv.lo = inv.lo.min(start);
                inv.hi = inv.hi.max(end);
            }
        }
    }

    #[inline(always)]
    pub fn has_invalidation(&self) -> bool {
        self.invalidation.any
    }

    /// Bumped whenever PC hooks are added or removed.
    pub fn hooks_version(&self) -> u64 {
        self.hooks_version
    }

    pub fn take_code_invalidation(&mut self) -> Option<(u32, u32)> {
        if self.invalidation.any {
            self.invalidation.any = false;
            Some((self.invalidation.lo, self.invalidation.hi - self.invalidation.lo))
        } else {
            None
        }
    }

    pub fn mark_code_page(&mut self, phys: u32) {
        let p = (phys & 0x1F_FFFF) >> 12;
        self.code_pages[(p / 64) as usize] |= 1 << (p % 64);
    }

    pub fn clear_code_pages(&mut self) {
        self.code_pages = [0; RAM_SIZE / 4096 / 64];
    }

    // ------------------------------------------------------------------
    // Memory access (used by the interpreter, the recompiler's slow path,
    // DMA and debug tooling).
    // ------------------------------------------------------------------

    #[inline(always)]
    pub fn ram_index(phys: u32) -> usize {
        (phys as usize) & (RAM_SIZE - 1)
    }

    /// Debug/peek read without side effects or timing (RAM, scratchpad, BIOS only).
    pub fn peek32(&self, vaddr: u32) -> u32 {
        let p = mask_region(vaddr);
        let mut b = [0u8; 4];
        for (i, out) in b.iter_mut().enumerate() {
            *out = self.peek8(p.wrapping_add(i as u32) | (vaddr & 0xE000_0000));
        }
        u32::from_le_bytes(b)
    }

    pub fn peek8(&self, vaddr: u32) -> u8 {
        let p = mask_region(vaddr);
        match p {
            0..=0x7F_FFFF => self.ram[Self::ram_index(p)],
            0x1F80_0000..=0x1F80_03FF => self.scratch[(p & 0x3FF) as usize],
            0x1FC0_0000..=0x1FC7_FFFF => self.bios[(p & 0x7FFFF) as usize],
            _ => 0,
        }
    }

    pub fn poke8(&mut self, vaddr: u32, v: u8) {
        let p = mask_region(vaddr);
        match p {
            0..=0x7F_FFFF => {
                self.ram[Self::ram_index(p)] = v;
                self.note_code_write(p, 1);
            }
            0x1F80_0000..=0x1F80_03FF => self.scratch[(p & 0x3FF) as usize] = v,
            _ => {}
        }
    }

    pub fn peek16(&self, vaddr: u32) -> u16 {
        self.peek8(vaddr) as u16 | ((self.peek8(vaddr.wrapping_add(1)) as u16) << 8)
    }

    pub fn poke16(&mut self, vaddr: u32, v: u16) {
        self.poke8(vaddr, v as u8);
        self.poke8(vaddr.wrapping_add(1), (v >> 8) as u8);
    }

    pub fn poke32(&mut self, vaddr: u32, v: u32) {
        for i in 0..4 {
            self.poke8(vaddr.wrapping_add(i), (v >> (8 * i)) as u8);
        }
    }

    #[inline(always)]
    fn ram_read(&self, p: u32, w: Width) -> u32 {
        let i = Self::ram_index(p);
        let r = &self.ram;
        match w {
            Width::Byte => r[i] as u32,
            Width::Half => u16::from_le_bytes([r[i], r[i + 1]]) as u32,
            Width::Word => u32::from_le_bytes([r[i], r[i + 1], r[i + 2], r[i + 3]]),
        }
    }

    #[inline(always)]
    fn ram_write(&mut self, p: u32, v: u32, w: Width) {
        let i = Self::ram_index(p);
        match w {
            Width::Byte => self.ram[i] = v as u8,
            Width::Half => self.ram[i..i + 2].copy_from_slice(&(v as u16).to_le_bytes()),
            Width::Word => self.ram[i..i + 4].copy_from_slice(&v.to_le_bytes()),
        }
        if self.code_pages[(i >> 18) & 7] != 0 {
            self.note_code_write(p, w as u32);
        }
    }

    /// CPU data read. Adds access timing to `cycles`.
    #[inline]
    pub fn read(&mut self, vaddr: u32, w: Width) -> u32 {
        let p = mask_region(vaddr);
        if p < 0x80_0000 {
            self.cycles += RAM_READ_CYCLES;
            return self.ram_read(p, w);
        }
        if (0x1F80_0000..0x1F80_0400).contains(&p) {
            if vaddr >= 0xA000_0000 && vaddr < 0xC000_0000 {
                log::warn!("scratchpad read through KSEG1 {vaddr:08x}");
            }
            let i = (p & 0x3FF) as usize;
            let s = &self.scratch;
            return match w {
                Width::Byte => s[i] as u32,
                Width::Half => u16::from_le_bytes([s[i], s[(i + 1) & 0x3FF]]) as u32,
                Width::Word => u32::from_le_bytes([s[i], s[(i + 1) & 0x3FF], s[(i + 2) & 0x3FF], s[(i + 3) & 0x3FF]]),
            };
        }
        self.read_slow(p, w)
    }

    #[cold]
    fn read_slow(&mut self, p: u32, w: Width) -> u32 {
        match p {
            0x1FC0_0000..=0x1FC7_FFFF => {
                self.cycles += self.bios_time.get(w) as u64;
                let i = (p & 0x7FFFF) as usize;
                let b = &self.bios;
                match w {
                    Width::Byte => b[i] as u32,
                    Width::Half => u16::from_le_bytes([b[i], b[i + 1]]) as u32,
                    Width::Word => u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]),
                }
            }
            0x1F80_1000..=0x1F80_1FFF => self.io_read(p, w),
            0x1F00_0000..=0x1F7F_FFFF => {
                self.cycles += self.exp1_time.get(w) as u64;
                // No expansion device: open bus reads as all ones.
                match w {
                    Width::Byte => 0xFF,
                    Width::Half => 0xFFFF,
                    Width::Word => 0xFFFF_FFFF,
                }
            }
            0x1F80_2000..=0x1F80_3FFF => {
                self.cycles += self.exp2_time.get(w) as u64;
                match p {
                    // DUART status: report TX ready so BIOS TTY routines don't hang.
                    0x1F80_2021 => 0x04,
                    _ => 0xFF,
                }
            }
            0x1FA0_0000..=0x1FBF_FFFF => 0xFFFF_FFFF,
            0xFFFE_0130 => self.cache_ctrl,
            _ => {
                log::warn!("unhandled read{} at {p:08x}", w as u32 * 8);
                0
            }
        }
    }

    /// CPU data write.
    #[inline]
    pub fn write(&mut self, vaddr: u32, v: u32, w: Width) {
        let p = mask_region(vaddr);
        if p < 0x80_0000 {
            self.ram_write(p, v, w);
            return;
        }
        if (0x1F80_0000..0x1F80_0400).contains(&p) {
            let i = (p & 0x3FF) as usize;
            match w {
                Width::Byte => self.scratch[i] = v as u8,
                Width::Half => {
                    self.scratch[i] = v as u8;
                    self.scratch[(i + 1) & 0x3FF] = (v >> 8) as u8;
                }
                Width::Word => {
                    for k in 0..4 {
                        self.scratch[(i + k) & 0x3FF] = (v >> (8 * k)) as u8;
                    }
                }
            }
            return;
        }
        self.write_slow(p, v, w);
    }

    #[cold]
    fn write_slow(&mut self, p: u32, v: u32, w: Width) {
        match p {
            0x1F80_1000..=0x1F80_1FFF => self.io_write(p, v, w),
            0x1F80_2000..=0x1F80_3FFF => {
                match p {
                    0x1F80_2023 | 0x1F80_2080 => {
                        // DUART TX / PCSX-style debug putchar
                        let c = v as u8 as char;
                        self.tty.push(c);
                        if self.tty_echo {
                            eprint!("{c}");
                        }
                    }
                    0x1F80_2041 | 0x1F80_2042 | 0x1F80_2070 => self.post = v as u8,
                    _ => {}
                }
            }
            0xFFFE_0130 => self.cache_ctrl = v,
            0x1FC0_0000..=0x1FC7_FFFF => {} // ROM
            0x1F00_0000..=0x1F7F_FFFF => {}
            _ => log::warn!("unhandled write{} {v:08x} at {p:08x}", w as u32 * 8),
        }
    }

    // ------------------------------------------------------------------
    // I/O registers
    // ------------------------------------------------------------------

    fn io_read(&mut self, p: u32, w: Width) -> u32 {
        let off = p & 0xFFF;
        let shift = (p & 3) * 8;
        let v = match off {
            0x000..=0x023 => {
                self.cycles += 2;
                self.memctrl[(off >> 2) as usize] >> shift
            }
            0x040..=0x04F => {
                self.cycles += 2;
                self.sync_sio();
                self.sio.read(off, w)
            }
            0x050..=0x05F => {
                self.cycles += 2;
                // SIO1 (serial port) is not connected: TX ready, nothing received.
                match off {
                    0x054 => 0x0000_0005,
                    _ => 0,
                }
            }
            0x060..=0x063 => {
                self.cycles += 2;
                self.ram_size_reg >> shift
            }
            0x070..=0x077 => {
                self.cycles += 2;
                let r = if off & 4 == 0 { self.irq.stat } else { self.irq.mask };
                r >> ((off & 3) * 8)
            }
            0x080..=0x0FF => {
                self.cycles += 2;
                self.dma.read(off) >> shift
            }
            0x100..=0x12F => {
                self.cycles += 2;
                self.sync_timers();
                self.timers.read(off, &self.gpu.timer_info(), self.cycles) >> ((off & 3) * 8)
            }
            0x800..=0x803 => {
                self.cycles += self.cdrom_time.get(w) as u64;
                self.sync_cdrom();
                let mut v = 0u32;
                for i in 0..(w as u32) {
                    v |= (self.cdrom.read((off + i) & 3) as u32) << (8 * i);
                }
                self.after_cdrom_access();
                v
            }
            0x810..=0x817 => {
                self.cycles += 2;
                self.sync_gpu();
                if off & 4 == 0 {
                    self.gpu.read_data()
                } else {
                    self.gpu.status()
                }
            }
            0x820..=0x827 => {
                self.cycles += 2;
                if off & 4 == 0 {
                    self.mdec.read_data()
                } else {
                    self.mdec.status()
                }
            }
            0xC00..=0xFFF => {
                self.cycles += self.spu_time.get(w) as u64;
                self.sync_spu();
                let lo = self.spu.read(off & !1) as u32;
                match w {
                    Width::Word => lo | ((self.spu.read((off & !1) + 2) as u32) << 16),
                    _ => lo >> ((off & 1) * 8),
                }
            }
            _ => {
                log::debug!("unhandled io read {p:08x}");
                0
            }
        };
        match w {
            Width::Byte => v & 0xFF,
            Width::Half => v & 0xFFFF,
            Width::Word => v,
        }
    }

    fn io_write(&mut self, p: u32, v: u32, w: Width) {
        let off = p & 0xFFF;
        // Narrow writes to 32-bit registers are shifted into position.
        let shifted = v << ((p & 3) * 8);
        match off {
            0x000..=0x023 => {
                self.memctrl[(off >> 2) as usize] = shifted;
                self.recompute_timings();
            }
            0x040..=0x04F => {
                self.sync_sio();
                self.sio.write(off, v, w, self.cycles);
                self.sync_sio();
            }
            0x050..=0x05F => {}
            0x060..=0x063 => self.ram_size_reg = shifted,
            0x070..=0x073 => {
                self.irq.ack(shifted);
            }
            0x074..=0x077 => {
                self.irq.mask = shifted & 0x7FF;
            }
            0x080..=0x0FF => {
                let reg = off & !3;
                if let Some(port) = self.dma.write(reg, shifted, &mut self.irq) {
                    self.start_dma(port);
                }
            }
            0x100..=0x12F => {
                self.sync_timers();
                let gi = self.gpu.timer_info();
                self.timers.write(off & !3, v & 0xFFFF, &gi, self.cycles, &mut self.irq);
                let next = self.timers.next_event_cycle(self.cycles, &gi);
                self.schedule(Event::Timers, next);
            }
            0x800..=0x803 => {
                self.sync_cdrom();
                for i in 0..(w as u32) {
                    self.cdrom.write((off + i) & 3, (v >> (8 * i)) as u8, self.cycles, &mut self.irq);
                }
                self.after_cdrom_access();
            }
            0x810..=0x817 => {
                self.sync_gpu();
                if off & 4 == 0 {
                    self.gpu.gp0(v);
                } else {
                    self.gpu.gp1(v, &mut self.irq);
                }
                let next = self.gpu.next_event_cycle(self.cycles);
                self.schedule(Event::Gpu, next);
                // A GP0 write may make a pending GPU DMA possible.
            }
            0x820..=0x827 => {
                if off & 4 == 0 {
                    self.mdec.write_command(v);
                } else {
                    self.mdec.write_control(v);
                }
                self.check_pending_dma();
            }
            0xC00..=0xFFF => {
                self.sync_spu();
                match w {
                    Width::Word => {
                        self.spu.write(off & !1, v as u16, &mut self.irq);
                        self.spu.write((off & !1) + 2, (v >> 16) as u16, &mut self.irq);
                    }
                    _ => self.spu.write(off & !1, v as u16, &mut self.irq),
                }
                let next = self.cycles + crate::CYCLES_PER_SAMPLE * crate::spu::SAMPLES_PER_EVENT;
                self.schedule(Event::Spu, next.min(self.spu.next_irq_cycle(self.cycles)));
            }
            _ => log::debug!("unhandled io write {p:08x} = {v:08x}"),
        }
    }

    fn after_cdrom_access(&mut self) {
        let next = self.cdrom.next_event_cycle();
        self.schedule(Event::Cdrom, next);
    }

    // ------------------------------------------------------------------
    // DMA
    // ------------------------------------------------------------------

    fn check_pending_dma(&mut self) {
        for port in [Port::MdecOut, Port::MdecIn, Port::Gpu, Port::CdRom, Port::Spu] {
            if self.dma.is_active(port) {
                self.start_dma(port);
            }
        }
    }

    /// Executes a DMA transfer for `port` if the device is ready.
    pub fn start_dma(&mut self, port: Port) {
        if !self.dma.is_active(port) {
            return;
        }
        // Devices that are not ready keep the channel pending.
        match port {
            Port::MdecOut if !self.mdec.output_ready() => {
                log::trace!("dma MdecOut pending (no output yet)");
                return;
            }
            _ => {}
        }
        let ch = self.dma.channel(port);
        log::trace!("dma {:?} mode {} madr {:06x} bcr {:08x} chcr {:08x}", port, ch.sync_mode(), ch.base, ch.block_control, ch.control);
        let words = match ch.sync_mode() {
            2 => self.dma_linked_list(port),
            _ => self.dma_block(port),
        };
        // The CPU is stalled while most DMA transfers run.
        self.cycles += words as u64;
        self.dma.finish(port, &mut self.irq);
        if port == Port::MdecIn {
            // Input may have produced output for a waiting MDEC-out channel.
            if self.dma.is_active(Port::MdecOut) {
                self.start_dma(Port::MdecOut);
            }
        }
    }

    fn dma_event(&mut self) {
        self.check_pending_dma();
    }

    fn dma_block(&mut self, port: Port) -> u32 {
        let ch = *self.dma.channel(port);
        let step: u32 = if ch.decrement() { 4u32.wrapping_neg() } else { 4 };
        let mut addr = ch.base & 0x1F_FFFC;
        let words = ch.transfer_words();
        let to_ram = !ch.from_ram();
        let start_addr = addr;
        for i in 0..words {
            let cur = addr & 0x1F_FFFC;
            if to_ram {
                let v = match port {
                    Port::Otc => {
                        if i == words - 1 {
                            0x00FF_FFFF
                        } else {
                            addr.wrapping_sub(4) & 0x1F_FFFF
                        }
                    }
                    Port::Gpu => self.gpu.read_data(),
                    Port::CdRom => self.cdrom.dma_read_word(),
                    Port::MdecOut => self.mdec.read_data(),
                    Port::Spu => self.spu.dma_read(),
                    _ => 0,
                };
                self.ram[cur as usize..cur as usize + 4].copy_from_slice(&v.to_le_bytes());
            } else {
                let v = u32::from_le_bytes(self.ram[cur as usize..cur as usize + 4].try_into().unwrap());
                match port {
                    Port::Gpu => self.gpu.gp0(v),
                    Port::MdecIn => self.mdec.write_command(v),
                    Port::Spu => self.spu.dma_write(v),
                    _ => {}
                }
            }
            addr = addr.wrapping_add(step);
        }
        if to_ram && words > 0 {
            let (lo, len) = if ch.decrement() {
                (addr.wrapping_add(4) & 0x1F_FFFC, words * 4)
            } else {
                (start_addr, words * 4)
            };
            self.note_code_write(lo, len);
        }
        // Sync mode 1 leaves MADR pointing after the transfer.
        if ch.sync_mode() == 1 {
            self.dma.channel_mut(port).base = addr & 0xFF_FFFF;
            self.dma.channel_mut(port).block_control &= 0xFFFF;
        }
        words
    }

    fn dma_linked_list(&mut self, port: Port) -> u32 {
        let mut addr = self.dma.channel(port).base & 0x1F_FFFC;
        let mut total = 0u32;
        let mut guard = 0;
        loop {
            let header = u32::from_le_bytes(self.ram[addr as usize..addr as usize + 4].try_into().unwrap());
            let count = header >> 24;
            self.gpu.packet_addr = 0x8000_0000 | addr;
            for i in 0..count {
                let a = (addr + 4 * (i + 1)) & 0x1F_FFFC;
                let v = u32::from_le_bytes(self.ram[a as usize..a as usize + 4].try_into().unwrap());
                if port == Port::Gpu {
                    self.gpu.gp0(v);
                }
            }
            total += count + 1;
            // The end marker is any address with bit 23 set.
            if header & 0x80_0000 != 0 {
                break;
            }
            self.gpu.packet_addr = 0;
            addr = header & 0x1F_FFFC;
            guard += 1;
            if guard > 0x10_0000 {
                log::warn!("GPU linked list did not terminate");
                break;
            }
        }
        self.gpu.packet_addr = 0;
        self.dma.channel_mut(port).base = 0x00FF_FFFF;
        total
    }

    /// Raises an interrupt line (used by engines/hooks/tests).
    pub fn raise_irq(&mut self, i: Interrupt) {
        self.irq.raise(i);
    }
}

/// Extra cycles a RAM data read stalls the CPU.
pub const RAM_READ_CYCLES: u64 = 4;

/// The BIOS ROM, code hooks and debug output stay as they are; the memory
/// timings are worked out again from the restored registers, and every page
/// counts as changed so translated code is thrown away.
impl crate::state::Field for Bus {
    fn sync(&mut self, s: &mut crate::state::Sync) {
        crate::sync_fields!(
            self, s; ram, scratch, memctrl, ram_size_reg, cache_ctrl, irq, dma, timers, gpu, cdrom, spu, mdec, sio, cycles, next_event, events, cap,
            frame_done, post
        );
        if s.loading() {
            self.recompute_timings();
            self.clear_code_pages();
            self.invalidation = Invalidation::default();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    type Log = Arc<Mutex<Vec<&'static str>>>;

    /// A hook that writes `name` to the log and answers `action`.
    fn note(log: &Log, name: &'static str, action: HookAction) -> PcHook {
        let log = log.clone();
        Box::new(move |_, _| {
            log.lock().unwrap().push(name);
            action
        })
    }

    fn ran(log: &Log) -> Vec<&'static str> {
        std::mem::take(&mut *log.lock().unwrap())
    }

    #[test]
    fn every_hook_on_an_address_runs_in_order() {
        const PC: u32 = 0x8001_0000;
        let mut bus = Bus::new(&[], VideoStandard::Ntsc);
        let mut cpu = Cpu::new();
        let log = Log::default();
        let a = bus.add_hook(PC, note(&log, "a", HookAction::Continue));
        // The same word through KSEG1.
        bus.add_hook(0xA001_0000, note(&log, "b", HookAction::Continue));
        bus.add_hook_once(PC, note(&log, "once", HookAction::Continue));
        assert!(bus.hook_at(PC) && !bus.hook_at(PC + 4));
        assert_eq!(bus.run_hook(&mut cpu, PC), HookAction::Continue);
        assert_eq!(ran(&log), ["a", "b", "once"]);
        assert_eq!(bus.run_hook(&mut cpu, PC), HookAction::Continue);
        assert_eq!(ran(&log), ["a", "b"]);

        // Removing one keeps the others.
        let version = bus.hooks_version();
        bus.remove_hook(a);
        assert!(!bus.has_hook(a) && bus.hook_at(PC) && bus.hooks_version() > version);
        bus.run_hook(&mut cpu, PC);
        assert_eq!(ran(&log), ["b"]);

        // A redirect moved the PC: the hooks after it don't run.
        let r = bus.add_hook(PC, note(&log, "redirect", HookAction::Redirect));
        let after = bus.add_hook(PC, note(&log, "after", HookAction::Continue));
        assert_eq!(bus.run_hook(&mut cpu, PC), HookAction::Redirect);
        assert_eq!(ran(&log), ["b", "redirect"]);
        bus.remove_hook(r);
        bus.remove_hook(after);

        // Hooks changing the hooks at their own address while they run: one
        // removed doesn't run, one added waits for the next time, and a hook
        // can remove itself.
        let doomed: Arc<Mutex<Vec<HookId>>> = Default::default();
        let (l, d) = (log.clone(), doomed.clone());
        let mut late = Some(note(&log, "late", HookAction::Continue));
        let changer = bus.add_hook(
            PC,
            Box::new(move |_, bus| {
                l.lock().unwrap().push("changer");
                for id in d.lock().unwrap().drain(..) {
                    bus.remove_hook(id);
                }
                if let Some(h) = late.take() {
                    bus.add_hook(PC, h);
                }
                HookAction::Continue
            }),
        );
        let removed = bus.add_hook(PC, note(&log, "removed", HookAction::Continue));
        doomed.lock().unwrap().push(removed);
        bus.run_hook(&mut cpu, PC);
        assert_eq!(ran(&log), ["b", "changer"]);
        doomed.lock().unwrap().push(changer);
        bus.run_hook(&mut cpu, PC);
        assert_eq!(ran(&log), ["b", "changer", "late"]);
        bus.run_hook(&mut cpu, PC);
        assert_eq!(ran(&log), ["b", "late"]);
        assert!(!bus.has_hook(changer) && !bus.has_hook(removed));

        // The last one gone, the address is free again.
        assert_eq!(bus.hook_addresses(), [PC]);
        let ids: Vec<HookId> = bus.hooks.values().flatten().map(|h| h.id).collect();
        for id in ids {
            bus.remove_hook(id);
        }
        assert!(!bus.hook_at(PC) && !bus.has_hooks);
        assert_eq!(bus.run_hook(&mut cpu, PC), HookAction::Continue);
        assert!(ran(&log).is_empty());
    }
}
