//! Root counters (timers 0-2).

use crate::bus::NEVER;
use crate::irq::{Interrupt, Irq};

/// Video timing facts the timers depend on.
#[derive(Debug, Clone, Copy)]
pub struct TimerGpuInfo {
    pub video_clock: u64,
    pub dot_divider: u64,
    pub in_vblank: bool,
}

#[derive(Debug, Clone, Copy, Default)]
struct Counter {
    counter: u32,
    mode: u32,
    target: u32,
    irq_done: bool,
    /// Counting suspended by a sync mode.
    paused: bool,
    /// Sync mode 3 waiting for the first blank.
    waiting: bool,
    frac: u64,
}

impl Counter {
    fn sync_enabled(&self) -> bool {
        self.mode & 1 != 0
    }
    fn sync_mode(&self) -> u32 {
        (self.mode >> 1) & 3
    }
    fn source(&self) -> u32 {
        (self.mode >> 8) & 3
    }
}

pub struct Timers {
    t: [Counter; 3],
    last_sync: u64,
}

impl Default for Timers {
    fn default() -> Self {
        Self::new()
    }
}

const IRQS: [Interrupt; 3] = [Interrupt::Timer0, Interrupt::Timer1, Interrupt::Timer2];

impl Timers {
    pub fn new() -> Self {
        let mut t = [Counter::default(); 3];
        for c in &mut t {
            c.mode = 1 << 10;
        }
        Self { t, last_sync: 0 }
    }

    fn uses_sysclk(&self, i: usize) -> bool {
        match i {
            0 => self.t[0].source() & 1 == 0,
            1 => self.t[1].source() & 1 == 0,
            _ => true,
        }
    }

    fn counting(&self, i: usize) -> bool {
        let c = &self.t[i];
        !(c.paused || c.waiting)
    }

    fn add_ticks(&mut self, i: usize, ticks: u64, irq: &mut Irq) {
        if ticks == 0 {
            return;
        }
        let c = &mut self.t[i];
        let old = c.counter;
        let mut counter = old as u64 + ticks;
        let mut request = false;
        let target = c.target as u64;
        if counter >= target && ((old as u64) < target || target == 0) {
            request |= c.mode & (1 << 4) != 0;
            c.mode |= 1 << 11;
            if c.mode & (1 << 3) != 0 && target > 0 {
                counter %= target;
            }
        }
        if counter >= 0xFFFF {
            request |= c.mode & (1 << 5) != 0;
            c.mode |= 1 << 12;
            counter %= 0xFFFF;
        }
        c.counter = counter as u32;
        if request {
            let repeat = c.mode & (1 << 6) != 0;
            if c.mode & (1 << 7) == 0 {
                // Pulse mode.
                if !c.irq_done || repeat {
                    irq.raise(IRQS[i]);
                    c.irq_done = true;
                }
                c.mode |= 1 << 10;
            } else {
                // Toggle mode.
                c.mode ^= 1 << 10;
                if c.mode & (1 << 10) == 0 && (!c.irq_done || repeat) {
                    irq.raise(IRQS[i]);
                    c.irq_done = true;
                }
            }
        }
    }

    /// Advances all clock-driven counters to `now`.
    pub fn sync(&mut self, now: u64, gi: &TimerGpuInfo, irq: &mut Irq) {
        let elapsed = now.saturating_sub(self.last_sync);
        self.last_sync = now;
        if elapsed == 0 {
            return;
        }
        for i in 0..3 {
            if !self.counting(i) {
                continue;
            }
            let ticks = match i {
                0 if !self.uses_sysclk(0) => {
                    // Dot clock = video clock / divider.
                    let c = &mut self.t[0];
                    c.frac += elapsed * gi.video_clock;
                    let den = crate::CPU_CLOCK * gi.dot_divider;
                    let t = c.frac / den;
                    c.frac %= den;
                    t
                }
                1 if !self.uses_sysclk(1) => 0, // counted on hblank events
                2 if self.t[2].source() >= 2 => {
                    let c = &mut self.t[2];
                    c.frac += elapsed;
                    let t = c.frac / 8;
                    c.frac %= 8;
                    t
                }
                _ => elapsed,
            };
            self.add_ticks(i, ticks, irq);
        }
    }

    pub fn on_hblanks(&mut self, n: u64, irq: &mut Irq) {
        // Timer 0 hblank synchronisation.
        let c = &mut self.t[0];
        if c.sync_enabled() {
            match c.sync_mode() {
                1 | 2 => c.counter = 0,
                3 => {
                    if c.waiting {
                        c.waiting = false;
                    }
                }
                _ => {}
            }
        }
        if !self.uses_sysclk(1) && self.counting(1) {
            self.add_ticks(1, n, irq);
        }
    }

    pub fn on_vblank(&mut self, entering: bool, _irq: &mut Irq) {
        let c = &mut self.t[1];
        if !c.sync_enabled() {
            return;
        }
        match c.sync_mode() {
            0 => c.paused = entering,
            1 => {
                if entering {
                    c.counter = 0
                }
            }
            2 => {
                if entering {
                    c.counter = 0;
                }
                c.paused = !entering;
            }
            3 => {
                if entering && c.waiting {
                    c.waiting = false;
                }
            }
            _ => {}
        }
    }

    pub fn read(&mut self, off: u32, _gi: &TimerGpuInfo, _now: u64) -> u32 {
        let i = ((off - 0x100) >> 4) as usize;
        if i >= 3 {
            return 0;
        }
        match off & 0xC {
            0 => self.t[i].counter,
            4 => {
                let m = self.t[i].mode;
                self.t[i].mode &= !((1 << 11) | (1 << 12));
                m
            }
            8 => self.t[i].target,
            _ => 0,
        }
    }

    pub fn write(&mut self, off: u32, v: u32, gi: &TimerGpuInfo, _now: u64, _irq: &mut Irq) {
        let i = ((off - 0x100) >> 4) as usize;
        if i >= 3 {
            return;
        }
        let c = &mut self.t[i];
        match off & 0xC {
            0 => c.counter = v & 0xFFFF,
            4 => {
                c.mode = (v & 0x3FF) | (1 << 10);
                c.counter = 0;
                c.irq_done = false;
                c.frac = 0;
                c.paused = false;
                c.waiting = false;
                if c.sync_enabled() {
                    match (i, c.sync_mode()) {
                        (2, 0) | (2, 3) => c.paused = true,
                        (0, 3) | (1, 3) => c.waiting = true,
                        (1, 0) => c.paused = gi.in_vblank,
                        (1, 2) => c.paused = !gi.in_vblank,
                        _ => {}
                    }
                }
            }
            8 => c.target = v & 0xFFFF,
            _ => {}
        }
    }

    /// Next CPU cycle at which a clock-driven timer could raise an IRQ.
    pub fn next_event_cycle(&self, now: u64, gi: &TimerGpuInfo) -> u64 {
        let mut best = NEVER;
        for i in 0..3 {
            let c = &self.t[i];
            if !self.counting(i) || c.mode & (3 << 4) == 0 {
                continue;
            }
            if c.irq_done && c.mode & (1 << 6) == 0 {
                continue;
            }
            if i == 1 && !self.uses_sysclk(1) {
                continue;
            }
            let mut ticks = u64::MAX;
            if c.mode & (1 << 4) != 0 {
                let t = if c.counter < c.target { c.target - c.counter } else { 0xFFFF - c.counter + c.target };
                ticks = ticks.min(t.max(1) as u64);
            }
            if c.mode & (1 << 5) != 0 {
                ticks = ticks.min((0xFFFF - c.counter).max(1) as u64);
            }
            let cycles = match i {
                0 if !self.uses_sysclk(0) => {
                    let den = crate::CPU_CLOCK * gi.dot_divider;
                    (ticks * den).saturating_sub(c.frac).div_ceil(gi.video_clock)
                }
                2 if c.source() >= 2 => (ticks * 8).saturating_sub(c.frac),
                _ => ticks,
            };
            best = best.min(now + cycles.max(1));
        }
        best
    }
}

impl crate::state::Field for Counter {
    fn sync(&mut self, s: &mut crate::state::Sync) {
        crate::sync_fields!(self, s; counter, mode, target, irq_done, paused, waiting, frac);
    }
}

impl crate::state::Field for Timers {
    fn sync(&mut self, s: &mut crate::state::Sync) {
        crate::sync_fields!(self, s; t, last_sync);
    }
}
