//! R3000A CPU: register file, COP0, instruction cache and the reference
//! interpreter.
//!
//! The interpreter is the ground truth for Re:Compiled. It models branch
//! and load delay slots, unaligned load merging, exceptions, interrupt
//! timing (including the GTE-instruction-at-interrupt quirk), the
//! instruction cache (contents *and* timing) and multiply/divide/GTE
//! result stalls.

use crate::bus::{mask_region, Bus, Width};
use crate::gte::Gte;
use crate::HookAction;
use dwrc_mips::{decode, Instr, Op};

pub mod exc {
    pub const INT: u32 = 0x00;
    pub const ADEL: u32 = 0x04;
    pub const ADES: u32 = 0x05;
    pub const IBE: u32 = 0x06;
    pub const DBE: u32 = 0x07;
    pub const SYSCALL: u32 = 0x08;
    pub const BP: u32 = 0x09;
    pub const RI: u32 = 0x0A;
    pub const CPU: u32 = 0x0B;
    pub const OV: u32 = 0x0C;
}

/// The memory interface the CPU core executes against. Implemented by the
/// real [`Bus`] and by the recompiler's replay bus used for validation.
pub trait CpuBus {
    fn read(&mut self, addr: u32, w: Width) -> u32;
    fn write(&mut self, addr: u32, v: u32, w: Width);
    fn cycles(&mut self) -> &mut u64;
    fn irq_line(&self) -> bool;
}

impl CpuBus for Bus {
    #[inline(always)]
    fn read(&mut self, addr: u32, w: Width) -> u32 {
        Bus::read(self, addr, w)
    }
    #[inline(always)]
    fn write(&mut self, addr: u32, v: u32, w: Width) {
        Bus::write(self, addr, v, w)
    }
    #[inline(always)]
    fn cycles(&mut self) -> &mut u64 {
        &mut self.cycles
    }
    #[inline(always)]
    fn irq_line(&self) -> bool {
        self.irq.line()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cop0 {
    pub bpc: u32,
    pub bda: u32,
    pub jumpdest: u32,
    pub dcic: u32,
    pub bad_vaddr: u32,
    pub bdam: u32,
    pub bpcm: u32,
    pub sr: u32,
    pub cause: u32,
    pub epc: u32,
}

pub const SR_IEC: u32 = 1 << 0;
pub const SR_ISC: u32 = 1 << 16;
pub const SR_BEV: u32 = 1 << 22;
pub const SR_CU2: u32 = 1 << 30;

#[derive(Clone)]
pub struct Cpu {
    pub regs: [u32; 32],
    pub hi: u32,
    pub lo: u32,
    /// Address of the next instruction to execute.
    pub pc: u32,
    /// Address of the instruction after that (branch target when a branch
    /// was just executed).
    pub next_pc: u32,
    /// Address of the instruction currently executing.
    pub current_pc: u32,
    /// Load in flight from the previous instruction (0 = none).
    pub load_reg: usize,
    pub load_val: u32,
    /// Load issued by the instruction executing now.
    pub next_load_reg: usize,
    pub next_load_val: u32,
    /// The instruction currently executing is in a branch delay slot.
    pub in_delay_slot: bool,
    /// The next instruction will be in a delay slot.
    pub next_in_delay: bool,
    pub cop0: Cop0,
    pub gte: Gte,
    icache_tag: [u32; 256],
    icache_valid: [u8; 256],
    icache_data: [u32; 1024],
    pub muldiv_ready: u64,
    pub gte_ready: u64,
    /// Capture BIOS putchar (B0:3D / A0:3C) into `bus.tty`.
    pub trace_tty: bool,
    pub instructions: u64,
    /// Exceptions and interrupts taken (a statistic, not part of states).
    pub exceptions: u64,
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Cpu {
    pub fn new() -> Self {
        Self {
            regs: [0; 32],
            hi: 0,
            lo: 0,
            pc: 0xBFC0_0000,
            next_pc: 0xBFC0_0004,
            current_pc: 0,
            load_reg: 0,
            load_val: 0,
            next_load_reg: 0,
            next_load_val: 0,
            in_delay_slot: false,
            next_in_delay: false,
            cop0: Cop0::default(),
            gte: Gte::default(),
            icache_tag: [0xFFFF_FFFF; 256],
            icache_valid: [0; 256],
            icache_data: [0; 1024],
            muldiv_ready: 0,
            gte_ready: 0,
            trace_tty: true,
            instructions: 0,
            exceptions: 0,
        }
    }

    pub fn reset(&mut self) {
        let trace = self.trace_tty;
        *self = Self::new();
        self.trace_tty = trace;
        self.cop0.sr = SR_BEV;
    }

    /// Jumps to `addr` cleanly (no delay slot pending).
    pub fn set_pc(&mut self, addr: u32) {
        self.pc = addr;
        self.next_pc = addr.wrapping_add(4);
        self.next_in_delay = false;
        self.in_delay_slot = false;
    }

    pub fn flush_icache(&mut self) {
        self.icache_valid = [0; 256];
        self.icache_tag = [0xFFFF_FFFF; 256];
    }

    #[inline(always)]
    pub fn reg(&self, r: usize) -> u32 {
        self.regs[r]
    }

    /// Writes a GPR immediately (cancels an in-flight load to it).
    #[inline(always)]
    pub fn set_reg(&mut self, r: usize, v: u32) {
        self.regs[r] = v;
        self.regs[0] = 0;
        if self.load_reg == r {
            self.load_reg = 0;
        }
    }

    /// Schedules a GPR write after the load delay slot.
    #[inline(always)]
    fn set_reg_delayed(&mut self, r: usize, v: u32) {
        if r == 0 {
            return;
        }
        if self.load_reg == r {
            self.load_reg = 0;
        }
        self.next_load_reg = r;
        self.next_load_val = v;
    }

    /// Commits any in-flight load (used when the pipeline is flushed).
    pub fn flush_load_delay(&mut self) {
        if self.load_reg != 0 {
            self.regs[self.load_reg] = self.load_val;
            self.load_reg = 0;
        }
        self.next_load_reg = 0;
    }

    pub fn cause_with_irq<B: CpuBus>(&self, bus: &B) -> u32 {
        (self.cop0.cause & !(1 << 10)) | ((bus.irq_line() as u32) << 10)
    }

    #[inline(always)]
    pub fn interrupt_pending(&self, bus: &Bus) -> bool {
        let sr = self.cop0.sr;
        if sr & SR_IEC == 0 {
            return false;
        }
        let ip = ((self.cop0.cause >> 8) & 3) | ((bus.irq.line() as u32) << 2);
        (ip & (sr >> 8) & 0xFF) != 0
    }

    /// Enters the exception handler. `epc_pc` is the address of the
    /// faulting instruction; `in_delay` whether it sat in a delay slot.
    pub fn exception(&mut self, code: u32, epc_pc: u32, in_delay: bool, cop: u32) {
        let mut cause = self.cop0.cause & !(0x8000_007C | (3 << 28));
        cause |= code << 2;
        cause |= cop << 28;
        let epc = if in_delay {
            cause |= 1 << 31;
            epc_pc.wrapping_sub(4)
        } else {
            epc_pc
        };
        self.cop0.cause = cause;
        self.cop0.epc = epc;
        let sr = self.cop0.sr;
        self.cop0.sr = (sr & !0x3F) | ((sr << 2) & 0x3F);
        let handler = if sr & SR_BEV != 0 { 0xBFC0_0180 } else { 0x8000_0080 };
        self.pc = handler;
        self.next_pc = handler.wrapping_add(4);
        self.next_in_delay = false;
        self.exceptions += 1;
    }

    /// Takes a hardware interrupt before executing the instruction at `pc`.
    pub fn take_interrupt(&mut self, bus: &mut Bus) {
        self.flush_load_delay();
        // A GTE command at the interrupted address is executed anyway; the
        // BIOS handler skips it when it returns.
        let phys = mask_region(self.pc);
        let next = if phys < 0x80_0000 || (0x1FC0_0000..0x1FC8_0000).contains(&phys) { bus.peek32(self.pc) } else { 0 };
        let ni = Instr(next);
        if decode(ni) == Op::Cop2Cmd && self.cop0.sr & SR_CU2 != 0 {
            self.gte_stall(bus);
            let c = self.gte.execute(ni.0);
            self.gte_ready = bus.cycles + c as u64;
        }
        let pc = self.pc;
        let in_delay = self.next_in_delay;
        self.exception(exc::INT, pc, in_delay, 0);
    }

    // ------------------------------------------------------------------
    // Instruction fetch
    // ------------------------------------------------------------------

    #[inline(always)]
    fn fetch(&mut self, bus: &mut Bus, pc: u32) -> u32 {
        let cached = pc < 0xA000_0000 && bus.cache_ctrl & 0x800 != 0;
        let phys = mask_region(pc);
        if cached {
            let line = ((pc >> 4) & 0xFF) as usize;
            let word = ((pc >> 2) & 3) as usize;
            let tag = phys & !0xFFF;
            if self.icache_tag[line] == tag && self.icache_valid[line] & (1 << word) != 0 {
                bus.cycles += 1;
                return self.icache_data[line * 4 + word];
            }
            // Miss: fill from the requested word to the end of the line.
            let base = phys & !0xF;
            let mut valid = 0u8;
            for w in word..4 {
                self.icache_data[line * 4 + w] = Self::read_code(bus, base + (w as u32) * 4);
                valid |= 1 << w;
            }
            self.icache_tag[line] = tag;
            self.icache_valid[line] = valid;
            let filled = (4 - word) as u64;
            bus.cycles += if phys < 0x80_0000 { 3 + filled } else { bus.bios_fetch_cycles() as u64 * filled };
            self.icache_data[line * 4 + word]
        } else {
            bus.cycles += if phys < 0x80_0000 { 4 } else { bus.bios_fetch_cycles() as u64 };
            Self::read_code(bus, phys)
        }
    }

    /// Charges instruction-fetch timing for `count` instructions starting at
    /// `pc` through the instruction cache model (used by the recompiler so
    /// translated code keeps the interpreter's timing).
    pub fn icache_touch(&mut self, bus: &mut Bus, pc: u32, count: u32) {
        for k in 0..count {
            self.fetch(bus, pc.wrapping_add(k * 4));
        }
    }

    #[inline(always)]
    fn read_code(bus: &Bus, phys: u32) -> u32 {
        if phys < 0x80_0000 {
            let i = (phys as usize) & (crate::bus::RAM_SIZE - 1);
            u32::from_le_bytes([bus.ram[i], bus.ram[i + 1], bus.ram[i + 2], bus.ram[i + 3]])
        } else if (0x1FC0_0000..0x1FC8_0000).contains(&phys) {
            let i = (phys & 0x7FFFF) as usize;
            u32::from_le_bytes([bus.bios[i], bus.bios[i + 1], bus.bios[i + 2], bus.bios[i + 3]])
        } else {
            0
        }
    }

    /// Invalidates the icache line holding `addr` (store with cache isolated).
    pub fn icache_isolated_store(&mut self, addr: u32) {
        let line = ((addr >> 4) & 0xFF) as usize;
        self.icache_valid[line] = 0;
    }

    // ------------------------------------------------------------------
    // Interpreter
    // ------------------------------------------------------------------

    /// Runs until the bus' next scheduled event.
    pub fn run_interpreter(&mut self, bus: &mut Bus) {
        while bus.cycles < bus.next_event {
            self.step(bus);
            if bus.stop_requested {
                bus.stop_requested = false;
                break;
            }
        }
    }

    /// Executes exactly one instruction (or enters an interrupt).
    #[inline]
    pub fn step(&mut self, bus: &mut Bus) {
        if self.interrupt_pending(bus) {
            self.take_interrupt(bus);
        }
        let pc = self.pc;
        if bus.has_hooks && bus.hook_at(pc) && bus.run_hook(self, pc) == HookAction::Redirect {
            return;
        }
        self.exec_one(bus);
    }

    /// Fetches and executes the instruction at `pc` without checking
    /// interrupts or hooks.
    #[inline]
    pub fn exec_one(&mut self, bus: &mut Bus) {
        let pc = self.pc;
        if self.trace_tty && (pc == 0xA0 || pc == 0xB0) {
            self.trace_bios_call(bus, pc);
        }
        if pc & 3 != 0 {
            self.current_pc = pc;
            self.cop0.bad_vaddr = pc;
            let d = self.next_in_delay;
            self.exception(exc::ADEL, pc, d, 0);
            return;
        }
        let instr = Instr(self.fetch(bus, pc));
        self.exec_instr(bus, instr);
    }

    /// Executes an already-fetched instruction at `pc`: delay-slot
    /// bookkeeping, execution and load-delay retirement.
    #[inline(always)]
    pub fn exec_instr<B: CpuBus>(&mut self, bus: &mut B, instr: Instr) {
        self.current_pc = self.pc;
        self.pc = self.next_pc;
        self.next_pc = self.pc.wrapping_add(4);
        self.in_delay_slot = self.next_in_delay;
        self.next_in_delay = false;
        self.execute(bus, instr);
        // Retire the previous instruction's load.
        if self.load_reg != 0 {
            self.regs[self.load_reg] = self.load_val;
        }
        self.load_reg = self.next_load_reg;
        self.load_val = self.next_load_val;
        self.next_load_reg = 0;
        self.instructions += 1;
    }

    fn trace_bios_call(&mut self, bus: &mut Bus, pc: u32) {
        let f = self.regs[9];
        if (pc == 0xA0 && f == 0x3C) || (pc == 0xB0 && f == 0x3D) {
            let c = self.regs[4] as u8 as char;
            bus.tty.push(c);
            if bus.tty_echo {
                eprint!("{c}");
            }
        }
    }

    #[inline(always)]
    fn branch(&mut self, target: u32) {
        self.next_pc = target;
    }

    #[inline(always)]
    fn mark_delay(&mut self) {
        self.next_in_delay = true;
    }

    fn muldiv_stall<B: CpuBus>(&mut self, bus: &mut B) {
        let c = bus.cycles();
        if *c < self.muldiv_ready {
            *c = self.muldiv_ready;
        }
    }

    fn gte_stall<B: CpuBus>(&mut self, bus: &mut B) {
        let c = bus.cycles();
        if *c < self.gte_ready {
            *c = self.gte_ready;
        }
    }

    fn raise(&mut self, code: u32) {
        let pc = self.current_pc;
        let d = self.in_delay_slot;
        self.exception(code, pc, d, 0);
    }

    fn cop_unusable(&mut self, cop: u32) {
        let pc = self.current_pc;
        let d = self.in_delay_slot;
        self.exception(exc::CPU, pc, d, cop);
    }

    #[inline(always)]
    fn load<B: CpuBus>(&mut self, bus: &mut B, addr: u32, w: Width) -> Option<u32> {
        if addr & (w as u32 - 1) != 0 {
            self.cop0.bad_vaddr = addr;
            self.raise(exc::ADEL);
            return None;
        }
        Some(bus.read(addr, w))
    }

    #[inline(always)]
    fn store<B: CpuBus>(&mut self, bus: &mut B, addr: u32, v: u32, w: Width) {
        if addr & (w as u32 - 1) != 0 {
            self.cop0.bad_vaddr = addr;
            self.raise(exc::ADES);
            return;
        }
        if self.cop0.sr & SR_ISC != 0 {
            self.icache_isolated_store(addr);
            return;
        }
        bus.write(addr, v, w);
    }

    pub fn execute<B: CpuBus>(&mut self, bus: &mut B, i: Instr) {
        let rs = i.rs();
        let rt = i.rt();
        let rd = i.rd();
        let s = self.regs[rs];
        let t = self.regs[rt];
        match decode(i) {
            Op::Sll => self.set_reg(rd, t << i.shamt()),
            Op::Srl => self.set_reg(rd, t >> i.shamt()),
            Op::Sra => self.set_reg(rd, ((t as i32) >> i.shamt()) as u32),
            Op::Sllv => self.set_reg(rd, t << (s & 31)),
            Op::Srlv => self.set_reg(rd, t >> (s & 31)),
            Op::Srav => self.set_reg(rd, ((t as i32) >> (s & 31)) as u32),
            Op::Jr => {
                self.branch(s);
                self.mark_delay();
            }
            Op::Jalr => {
                let ret = self.next_pc;
                self.branch(s);
                self.set_reg(rd, ret);
                self.mark_delay();
            }
            Op::Syscall => self.raise(exc::SYSCALL),
            Op::Break => self.raise(exc::BP),
            Op::Mfhi => {
                self.muldiv_stall(bus);
                self.set_reg(rd, self.hi)
            }
            Op::Mthi => self.hi = s,
            Op::Mflo => {
                self.muldiv_stall(bus);
                self.set_reg(rd, self.lo)
            }
            Op::Mtlo => self.lo = s,
            Op::Mult => {
                let r = (s as i32 as i64) * (t as i32 as i64);
                self.hi = (r >> 32) as u32;
                self.lo = r as u32;
                let a = if (s as i32) < 0 { !s } else { s };
                self.muldiv_ready = *bus.cycles() + mult_cycles(a);
            }
            Op::Multu => {
                let r = (s as u64) * (t as u64);
                self.hi = (r >> 32) as u32;
                self.lo = r as u32;
                self.muldiv_ready = *bus.cycles() + mult_cycles(s);
            }
            Op::Div => {
                let n = s as i32;
                let d = t as i32;
                if d == 0 {
                    self.hi = n as u32;
                    self.lo = if n >= 0 { 0xFFFF_FFFF } else { 1 };
                } else if n as u32 == 0x8000_0000 && d == -1 {
                    self.hi = 0;
                    self.lo = 0x8000_0000;
                } else {
                    self.hi = (n % d) as u32;
                    self.lo = (n / d) as u32;
                }
                self.muldiv_ready = *bus.cycles() + 36;
            }
            Op::Divu => {
                if t == 0 {
                    self.hi = s;
                    self.lo = 0xFFFF_FFFF;
                } else {
                    self.hi = s % t;
                    self.lo = s / t;
                }
                self.muldiv_ready = *bus.cycles() + 36;
            }
            Op::Add => match (s as i32).checked_add(t as i32) {
                Some(v) => self.set_reg(rd, v as u32),
                None => self.raise(exc::OV),
            },
            Op::Addu => self.set_reg(rd, s.wrapping_add(t)),
            Op::Sub => match (s as i32).checked_sub(t as i32) {
                Some(v) => self.set_reg(rd, v as u32),
                None => self.raise(exc::OV),
            },
            Op::Subu => self.set_reg(rd, s.wrapping_sub(t)),
            Op::And => self.set_reg(rd, s & t),
            Op::Or => self.set_reg(rd, s | t),
            Op::Xor => self.set_reg(rd, s ^ t),
            Op::Nor => self.set_reg(rd, !(s | t)),
            Op::Slt => self.set_reg(rd, ((s as i32) < (t as i32)) as u32),
            Op::Sltu => self.set_reg(rd, (s < t) as u32),
            Op::Bltz | Op::Bgez | Op::Bltzal | Op::Bgezal => {
                let op = decode(i);
                let cond = match op {
                    Op::Bltz | Op::Bltzal => (s as i32) < 0,
                    _ => (s as i32) >= 0,
                };
                if matches!(op, Op::Bltzal | Op::Bgezal) {
                    // The link register is written regardless of the condition.
                    let ret = self.next_pc;
                    self.set_reg(31, ret);
                }
                if cond {
                    self.branch(self.pc.wrapping_add(i.simm() << 2));
                }
                self.mark_delay();
            }
            Op::J => {
                self.branch((self.pc & 0xF000_0000) | (i.target() << 2));
                self.mark_delay();
            }
            Op::Jal => {
                let ret = self.next_pc;
                self.branch((self.pc & 0xF000_0000) | (i.target() << 2));
                self.set_reg(31, ret);
                self.mark_delay();
            }
            Op::Beq => {
                if s == t {
                    self.branch(self.pc.wrapping_add(i.simm() << 2));
                }
                self.mark_delay();
            }
            Op::Bne => {
                if s != t {
                    self.branch(self.pc.wrapping_add(i.simm() << 2));
                }
                self.mark_delay();
            }
            Op::Blez => {
                if (s as i32) <= 0 {
                    self.branch(self.pc.wrapping_add(i.simm() << 2));
                }
                self.mark_delay();
            }
            Op::Bgtz => {
                if (s as i32) > 0 {
                    self.branch(self.pc.wrapping_add(i.simm() << 2));
                }
                self.mark_delay();
            }
            Op::Addi => match (s as i32).checked_add(i.simm() as i32) {
                Some(v) => self.set_reg(rt, v as u32),
                None => self.raise(exc::OV),
            },
            Op::Addiu => self.set_reg(rt, s.wrapping_add(i.simm())),
            Op::Slti => self.set_reg(rt, ((s as i32) < (i.simm() as i32)) as u32),
            Op::Sltiu => self.set_reg(rt, (s < i.simm()) as u32),
            Op::Andi => self.set_reg(rt, s & i.imm()),
            Op::Ori => self.set_reg(rt, s | i.imm()),
            Op::Xori => self.set_reg(rt, s ^ i.imm()),
            Op::Lui => self.set_reg(rt, i.imm() << 16),
            Op::Mfc0 => {
                let v = match rd {
                    3 => self.cop0.bpc,
                    5 => self.cop0.bda,
                    6 => self.cop0.jumpdest,
                    7 => self.cop0.dcic,
                    8 => self.cop0.bad_vaddr,
                    9 => self.cop0.bdam,
                    11 => self.cop0.bpcm,
                    12 => self.cop0.sr,
                    13 => self.cause_with_irq(bus),
                    14 => self.cop0.epc,
                    15 => 0x0000_0002,
                    _ => {
                        self.raise(exc::RI);
                        return;
                    }
                };
                self.set_reg_delayed(rt, v);
            }
            Op::Mtc0 => match rd {
                3 => self.cop0.bpc = t,
                5 => self.cop0.bda = t,
                6 => {}
                7 => self.cop0.dcic = t & 0xFF80_F03F,
                9 => self.cop0.bdam = t,
                11 => self.cop0.bpcm = t,
                12 => self.cop0.sr = t & 0xF27F_FF3F,
                13 => self.cop0.cause = (self.cop0.cause & !0x300) | (t & 0x300),
                _ => {}
            },
            Op::Rfe => {
                let sr = self.cop0.sr;
                self.cop0.sr = (sr & !0xF) | ((sr >> 2) & 0xF);
            }
            Op::Mfc2 | Op::Cfc2 | Op::Mtc2 | Op::Ctc2 | Op::Cop2Cmd | Op::Lwc2 | Op::Swc2
                if self.cop0.sr & SR_CU2 == 0 =>
            {
                self.cop_unusable(2);
            }
            Op::Mfc2 => {
                self.gte_stall(bus);
                let v = self.gte.read_data(rd);
                self.set_reg_delayed(rt, v);
            }
            Op::Cfc2 => {
                self.gte_stall(bus);
                let v = self.gte.read_ctrl(rd);
                self.set_reg_delayed(rt, v);
            }
            Op::Mtc2 => self.gte.write_data(rd, t),
            Op::Ctc2 => self.gte.write_ctrl(rd, t),
            Op::Cop2Cmd => {
                self.gte_stall(bus);
                let c = self.gte.execute(i.0);
                self.gte_ready = *bus.cycles() + c as u64;
            }
            Op::Lwc2 => {
                let addr = s.wrapping_add(i.simm());
                if let Some(v) = self.load(bus, addr, Width::Word) {
                    self.gte.write_data(rt, v);
                }
            }
            Op::Swc2 => {
                self.gte_stall(bus);
                let addr = s.wrapping_add(i.simm());
                let v = self.gte.read_data(rt);
                self.store(bus, addr, v, Width::Word);
            }
            Op::CopUnusable => {
                let cop = i.op() & 3;
                self.cop_unusable(cop);
            }
            Op::Lb => {
                let addr = s.wrapping_add(i.simm());
                if let Some(v) = self.load(bus, addr, Width::Byte) {
                    self.set_reg_delayed(rt, v as u8 as i8 as i32 as u32);
                }
            }
            Op::Lbu => {
                let addr = s.wrapping_add(i.simm());
                if let Some(v) = self.load(bus, addr, Width::Byte) {
                    self.set_reg_delayed(rt, v & 0xFF);
                }
            }
            Op::Lh => {
                let addr = s.wrapping_add(i.simm());
                if let Some(v) = self.load(bus, addr, Width::Half) {
                    self.set_reg_delayed(rt, v as u16 as i16 as i32 as u32);
                }
            }
            Op::Lhu => {
                let addr = s.wrapping_add(i.simm());
                if let Some(v) = self.load(bus, addr, Width::Half) {
                    self.set_reg_delayed(rt, v & 0xFFFF);
                }
            }
            Op::Lw => {
                let addr = s.wrapping_add(i.simm());
                if let Some(v) = self.load(bus, addr, Width::Word) {
                    self.set_reg_delayed(rt, v);
                }
            }
            Op::Lwl | Op::Lwr => {
                let addr = s.wrapping_add(i.simm());
                let word = bus.read(addr & !3, Width::Word);
                // LWL/LWR see the value of an in-flight load to the same register.
                let cur = if self.load_reg == rt && rt != 0 { self.load_val } else { t };
                let v = if decode(i) == Op::Lwl { lwl(cur, word, addr) } else { lwr(cur, word, addr) };
                self.set_reg_delayed(rt, v);
            }
            Op::Sb => {
                let addr = s.wrapping_add(i.simm());
                self.store(bus, addr, t & 0xFF, Width::Byte);
            }
            Op::Sh => {
                let addr = s.wrapping_add(i.simm());
                self.store(bus, addr, t & 0xFFFF, Width::Half);
            }
            Op::Sw => {
                let addr = s.wrapping_add(i.simm());
                self.store(bus, addr, t, Width::Word);
            }
            Op::Swl | Op::Swr => {
                let addr = s.wrapping_add(i.simm());
                let swl = decode(i) == Op::Swl;
                if self.cop0.sr & SR_ISC != 0 {
                    self.icache_isolated_store(addr);
                    return;
                }
                // Byte-enable writes: only the affected bytes are stored.
                let base = addr & !3;
                let off = addr & 3;
                if swl {
                    // SWL stores the high (off+1) bytes of rt to base..=base+off.
                    for k in 0..=off {
                        let b = (t >> (8 * (3 - off + k))) & 0xFF;
                        bus.write(base + k, b, Width::Byte);
                    }
                } else {
                    // SWR stores the low (4-off) bytes of rt to base+off..base+3.
                    for k in off..4 {
                        let b = (t >> (8 * (k - off))) & 0xFF;
                        bus.write(base + k, b, Width::Byte);
                    }
                }
            }
            Op::Illegal => self.raise(exc::RI),
        }
    }
}

#[inline]
pub fn lwl(cur: u32, word: u32, addr: u32) -> u32 {
    match addr & 3 {
        0 => (cur & 0x00FF_FFFF) | (word << 24),
        1 => (cur & 0x0000_FFFF) | (word << 16),
        2 => (cur & 0x0000_00FF) | (word << 8),
        _ => word,
    }
}

#[inline]
pub fn lwr(cur: u32, word: u32, addr: u32) -> u32 {
    match addr & 3 {
        0 => word,
        1 => (cur & 0xFF00_0000) | (word >> 8),
        2 => (cur & 0xFFFF_0000) | (word >> 16),
        _ => (cur & 0xFFFF_FF00) | (word >> 24),
    }
}

/// Multiplier latency depends on the magnitude of the first operand.
#[inline]
pub fn mult_cycles(a: u32) -> u64 {
    if a < 0x800 {
        6
    } else if a < 0x10_0000 {
        9
    } else {
        13
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unaligned_merge() {
        // Classic LWR/LWL pair loading 0x44332211 from an unaligned address.
        let w0 = 0x4433_2211;
        assert_eq!(lwl(0, w0, 3), w0);
        assert_eq!(lwr(0, w0, 0), w0);
        assert_eq!(lwr(0xAABB_CCDD, 0x1122_3344, 1), 0xAA11_2233);
        assert_eq!(lwl(0xAABB_CCDD, 0x1122_3344, 1), 0x3344_CCDD);
    }
}

impl crate::state::Field for Cop0 {
    fn sync(&mut self, s: &mut crate::state::Sync) {
        crate::sync_fields!(self, s; bpc, bda, jumpdest, dcic, bad_vaddr, bdam, bpcm, sr, cause, epc);
    }
}

impl crate::state::Field for Cpu {
    fn sync(&mut self, s: &mut crate::state::Sync) {
        crate::sync_fields!(
            self, s; regs, hi, lo, pc, next_pc, current_pc, load_reg, load_val, next_load_reg, next_load_val, in_delay_slot,
            next_in_delay, cop0, gte, icache_tag, icache_valid, icache_data, muldiv_ready, gte_ready, instructions
        );
    }
}
