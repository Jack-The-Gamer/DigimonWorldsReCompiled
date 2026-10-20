//! Interrupt controller (I_STAT / I_MASK).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Interrupt {
    VBlank = 0,
    Gpu = 1,
    CdRom = 2,
    Dma = 3,
    Timer0 = 4,
    Timer1 = 5,
    Timer2 = 6,
    Sio0 = 7,
    Sio1 = 8,
    Spu = 9,
    Pio = 10,
}

#[derive(Debug, Default, Clone)]
pub struct Irq {
    pub stat: u32,
    pub mask: u32,
}

impl Irq {
    #[inline]
    pub fn raise(&mut self, i: Interrupt) {
        self.stat |= 1 << (i as u32);
    }

    /// Writing I_STAT acknowledges: bits written as 0 are cleared.
    pub fn ack(&mut self, v: u32) {
        self.stat &= v & 0x7FF;
    }

    /// State of the CPU's hardware interrupt line (COP0 CAUSE bit 10).
    #[inline(always)]
    pub fn line(&self) -> bool {
        self.stat & self.mask != 0
    }
}

impl crate::state::Field for Irq {
    fn sync(&mut self, s: &mut crate::state::Sync) {
        crate::sync_fields!(self, s; stat, mask);
    }
}
