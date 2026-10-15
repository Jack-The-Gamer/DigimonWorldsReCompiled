//! SIO0: the controller / memory card serial port.

use crate::bus::{Width, NEVER};
use crate::irq::{Interrupt, Irq};
use crate::memcard::MemoryCard;
use crate::pad::Pad;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    None,
    Pad,
    Card,
}

pub struct Sio {
    pub pads: [Pad; 2],
    pub cards: [Option<MemoryCard>; 2],
    mode: u16,
    ctrl: u16,
    baud: u16,
    rx: u8,
    rx_full: bool,
    irq_flag: bool,
    ack_level: bool,
    target: Target,
    /// Byte transfer in flight: (complete_at, tx byte)
    transfer: Option<(u64, u8)>,
    ack_at: u64,
    ack_end_at: u64,
}

impl Default for Sio {
    fn default() -> Self {
        Self::new()
    }
}

const ACK_DELAY: u64 = 338;
const ACK_PULSE: u64 = 96;

impl Sio {
    pub fn new() -> Self {
        Self {
            pads: [Pad::new(), Pad::disconnected()],
            cards: [Some(MemoryCard::new_formatted()), None],
            mode: 0,
            ctrl: 0,
            baud: 0x88,
            rx: 0xFF,
            rx_full: false,
            irq_flag: false,
            ack_level: false,
            target: Target::None,
            transfer: None,
            ack_at: NEVER,
            ack_end_at: NEVER,
        }
    }

    pub fn reset(&mut self) {
        let pads = self.pads.clone();
        let cards = std::mem::take(&mut self.cards);
        *self = Self::new();
        self.pads = pads;
        self.cards = cards;
    }

    fn port(&self) -> usize {
        ((self.ctrl >> 13) & 1) as usize
    }

    fn selected(&self) -> bool {
        self.ctrl & 2 != 0
    }

    fn byte_cycles(&self) -> u64 {
        let mul = match self.mode & 3 {
            2 => 16,
            3 => 64,
            _ => 1,
        };
        (self.baud.max(1) as u64) * mul * 8
    }

    pub fn read(&mut self, off: u32, w: Width) -> u32 {
        match off & 0xF {
            0x0 => {
                let v = if self.rx_full { self.rx } else { 0xFF };
                self.rx_full = false;
                // Reading wider than a byte returns the FIFO contents repeated.
                match w {
                    Width::Byte => v as u32,
                    _ => (v as u32) | ((v as u32) << 8) | ((v as u32) << 16) | ((v as u32) << 24),
                }
            }
            0x4 => {
                let mut s = 0u32;
                let tx_idle = self.transfer.is_none();
                s |= 1; // TX FIFO not full
                s |= (self.rx_full as u32) << 1;
                s |= (tx_idle as u32) << 2;
                s |= (self.ack_level as u32) << 7;
                s |= (self.irq_flag as u32) << 9;
                s
            }
            0x8 => self.mode as u32,
            0xA => self.ctrl as u32,
            0xE => self.baud as u32,
            _ => 0,
        }
    }

    pub fn write(&mut self, off: u32, v: u32, _w: Width, now: u64) {
        match off & 0xF {
            0x0 => {
                let b = v as u8;
                if self.ctrl & 1 != 0 || true {
                    self.transfer = Some((now + self.byte_cycles(), b));
                }
            }
            0x8 => self.mode = v as u16,
            0xA => {
                let v = v as u16;
                if v & 0x40 != 0 {
                    // Reset
                    self.ctrl = 0;
                    self.mode = 0;
                    self.rx_full = false;
                    self.irq_flag = false;
                    self.transfer = None;
                    self.ack_at = NEVER;
                    self.target = Target::None;
                    for p in &mut self.pads {
                        p.reset_protocol();
                    }
                    for c in self.cards.iter_mut().flatten() {
                        c.reset_protocol();
                    }
                    return;
                }
                if v & 0x10 != 0 {
                    self.irq_flag = false;
                }
                let was_selected = self.selected();
                self.ctrl = v & !0x10;
                if !self.selected() || (was_selected && self.port() != ((v >> 13) & 1) as usize) {
                    self.target = Target::None;
                    for p in &mut self.pads {
                        p.reset_protocol();
                    }
                    for c in self.cards.iter_mut().flatten() {
                        c.reset_protocol();
                    }
                }
            }
            0xE => self.baud = v as u16,
            _ => {}
        }
    }

    pub fn next_event_cycle(&self) -> u64 {
        let mut t = NEVER;
        if let Some((at, _)) = self.transfer {
            t = t.min(at);
        }
        t.min(self.ack_at).min(self.ack_end_at)
    }

    pub fn sync(&mut self, now: u64, irq: &mut Irq) {
        if let Some((at, tx)) = self.transfer {
            if now >= at {
                self.transfer = None;
                let (rx, ack) = self.exchange(tx);
                log::trace!("sio port{} tx {tx:02x} rx {rx:02x} ack {ack} ctrl {:04x}", self.port(), self.ctrl);
                self.rx = rx;
                self.rx_full = true;
                if ack {
                    self.ack_at = at + ACK_DELAY;
                }
            }
        }
        if now >= self.ack_at {
            let at = self.ack_at;
            self.ack_at = NEVER;
            self.ack_level = true;
            self.ack_end_at = at + ACK_PULSE;
            if self.ctrl & (1 << 12) != 0 && !self.irq_flag {
                self.irq_flag = true;
                irq.raise(Interrupt::Sio0);
            }
        }
        if now >= self.ack_end_at {
            self.ack_end_at = NEVER;
            self.ack_level = false;
        }
    }

    fn exchange(&mut self, tx: u8) -> (u8, bool) {
        if !self.selected() {
            return (0xFF, false);
        }
        let port = self.port();
        if self.target == Target::None {
            self.target = match tx {
                0x01 => Target::Pad,
                0x81 => Target::Card,
                _ => Target::None,
            };
        }
        match self.target {
            Target::Pad => {
                let r = self.pads[port].transfer(tx);
                if !r.1 {
                    self.target = Target::None;
                }
                r
            }
            Target::Card => match self.cards[port].as_mut() {
                Some(c) => {
                    let r = c.transfer(tx);
                    if !r.1 {
                        self.target = Target::None;
                    }
                    r
                }
                None => {
                    self.target = Target::None;
                    (0xFF, false)
                }
            },
            Target::None => (0xFF, false),
        }
    }
}
