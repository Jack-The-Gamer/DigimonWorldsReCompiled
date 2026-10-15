//! DMA controller registers. Transfers themselves are executed by the bus,
//! which owns RAM and the devices.

use crate::irq::{Interrupt, Irq};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Port {
    MdecIn = 0,
    MdecOut = 1,
    Gpu = 2,
    CdRom = 3,
    Spu = 4,
    Pio = 5,
    Otc = 6,
}

impl Port {
    pub fn from_index(i: u32) -> Port {
        match i {
            0 => Port::MdecIn,
            1 => Port::MdecOut,
            2 => Port::Gpu,
            3 => Port::CdRom,
            4 => Port::Spu,
            5 => Port::Pio,
            _ => Port::Otc,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Channel {
    pub base: u32,
    pub block_control: u32,
    pub control: u32,
}

impl Channel {
    pub fn from_ram(&self) -> bool {
        self.control & 1 != 0
    }
    pub fn decrement(&self) -> bool {
        self.control & 2 != 0
    }
    pub fn sync_mode(&self) -> u32 {
        (self.control >> 9) & 3
    }
    pub fn enabled(&self) -> bool {
        self.control & (1 << 24) != 0
    }
    pub fn trigger(&self) -> bool {
        self.control & (1 << 28) != 0
    }
    pub fn transfer_words(&self) -> u32 {
        let bs = self.block_control & 0xFFFF;
        let bc = self.block_control >> 16;
        match self.sync_mode() {
            0 => {
                if bs == 0 {
                    0x10000
                } else {
                    bs
                }
            }
            1 => bs * bc.max(1),
            _ => 0,
        }
    }
}

pub struct Dma {
    pub control: u32,
    pub irq_enable: u32,
    pub irq_flags: u32,
    pub force_irq: bool,
    pub master_irq_enable: bool,
    pub irq_dummy: u32,
    pub channels: [Channel; 7],
}

impl Default for Dma {
    fn default() -> Self {
        Self::new()
    }
}

impl Dma {
    pub fn new() -> Self {
        Self {
            control: 0x0765_4321,
            irq_enable: 0,
            irq_flags: 0,
            force_irq: false,
            master_irq_enable: false,
            irq_dummy: 0,
            channels: [Channel::default(); 7],
        }
    }

    pub fn channel(&self, p: Port) -> &Channel {
        &self.channels[p as usize]
    }
    pub fn channel_mut(&mut self, p: Port) -> &mut Channel {
        &mut self.channels[p as usize]
    }

    fn master_flag(&self) -> bool {
        self.force_irq || (self.master_irq_enable && (self.irq_enable & self.irq_flags) != 0)
    }

    fn dicr(&self) -> u32 {
        self.irq_dummy
            | ((self.force_irq as u32) << 15)
            | (self.irq_enable << 16)
            | ((self.master_irq_enable as u32) << 23)
            | (self.irq_flags << 24)
            | ((self.master_flag() as u32) << 31)
    }

    pub fn read(&self, off: u32) -> u32 {
        let off = off & !3;
        let ch = ((off - 0x80) >> 4) as usize;
        let reg = off & 0xF;
        if ch < 7 {
            let c = &self.channels[ch];
            match reg {
                0 => c.base,
                4 => c.block_control,
                8 => c.control,
                _ => 0,
            }
        } else {
            match reg {
                0 => self.control,
                4 => self.dicr(),
                // Unused registers read back fixed values.
                8 => 0x7FFA_C68B,
                _ => 0x00FF_FFF7,
            }
        }
    }

    /// Returns the port to run if this write started a transfer.
    pub fn write(&mut self, off: u32, v: u32, irq: &mut Irq) -> Option<Port> {
        let ch = ((off - 0x80) >> 4) as usize;
        let reg = off & 0xF;
        if ch < 7 {
            let port = Port::from_index(ch as u32);
            let c = &mut self.channels[ch];
            match reg {
                0 => c.base = v & 0xFF_FFFF,
                4 => c.block_control = v,
                8 => {
                    c.control = if port == Port::Otc {
                        // OTC only has a few writable bits and always decrements.
                        (v & 0x5100_0000) | 2
                    } else {
                        v & 0x7177_0703
                    };
                }
                _ => {}
            }
            if reg == 8 && self.is_active(port) {
                return Some(port);
            }
            None
        } else {
            match reg {
                0 => {
                    self.control = v;
                    // Enabling a channel may start a pending transfer.
                    (0..7).map(Port::from_index).find(|&p| self.is_active(p))
                }
                4 => {
                    let before = self.master_flag();
                    self.irq_dummy = v & 0x3F;
                    self.force_irq = v & (1 << 15) != 0;
                    self.irq_enable = (v >> 16) & 0x7F;
                    self.master_irq_enable = v & (1 << 23) != 0;
                    // Writing 1 to a flag acknowledges it.
                    self.irq_flags &= !((v >> 24) & 0x7F);
                    if !before && self.master_flag() {
                        irq.raise(Interrupt::Dma);
                    }
                    None
                }
                _ => None,
            }
        }
    }

    pub fn is_active(&self, p: Port) -> bool {
        let c = &self.channels[p as usize];
        let enabled_in_dpcr = self.control & (1 << (p as u32 * 4 + 3)) != 0;
        let trig = if c.sync_mode() == 0 { c.trigger() } else { true };
        c.enabled() && trig && enabled_in_dpcr
    }

    pub fn finish(&mut self, p: Port, irq: &mut Irq) {
        let c = &mut self.channels[p as usize];
        c.control &= !((1 << 24) | (1 << 28));
        let before = self.master_flag();
        if self.irq_enable & (1 << p as u32) != 0 {
            self.irq_flags |= 1 << p as u32;
        }
        if !before && self.master_flag() {
            irq.raise(Interrupt::Dma);
        }
    }
}
