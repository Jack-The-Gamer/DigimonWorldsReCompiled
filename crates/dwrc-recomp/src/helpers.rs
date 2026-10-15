#![allow(clippy::not_unsafe_ptr_arg_deref)] // helpers are only called by translated code with a valid context
//! Runtime interface between translated code and the hardware model.
//!
//! Translated blocks receive a pointer to a [`JitCtx`] and call these
//! `extern "C"` helpers for anything that is not plain register/RAM work.
//! Every helper funnels into the same `Bus`/`Cpu` code the interpreter
//! uses, which is what keeps both engines behaviourally identical.

use dwrc_psx::bus::Width;
use dwrc_psx::cpu::{exc, mult_cycles, SR_ISC};
use dwrc_psx::{Bus, Cpu};

/// Memory traffic recorded in validation mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Access {
    Read { addr: u32, width: u8, value: u32 },
    Write { addr: u32, width: u8, value: u32 },
}

#[repr(C)]
pub struct JitCtx {
    pub regs: *mut u32,
    pub ram: *mut u8,
    pub scratch: *mut u8,
    pub cycles: *mut u64,
    pub code_pages: *const u64,
    pub sr: *const u32,
    pub load_reg: *mut usize,
    pub load_val: *mut u32,
    pub cpu: *mut Cpu,
    pub bus: *mut Bus,
    /// Non-null in validation mode.
    pub log: *mut Vec<Access>,
}

impl JitCtx {
    pub fn new(cpu: &mut Cpu, bus: &mut Bus, log: *mut Vec<Access>) -> Self {
        Self {
            regs: cpu.regs.as_mut_ptr(),
            ram: bus.ram.as_mut_ptr(),
            scratch: bus.scratch.as_mut_ptr(),
            cycles: &mut bus.cycles,
            code_pages: bus.code_pages.as_ptr(),
            sr: &cpu.cop0.sr,
            load_reg: &mut cpu.load_reg,
            load_val: &mut cpu.load_val,
            cpu,
            bus,
            log,
        }
    }
}

#[inline(always)]
unsafe fn parts<'a>(ctx: *mut JitCtx) -> (&'a mut Cpu, &'a mut Bus, Option<&'a mut Vec<Access>>) {
    let c = &mut *ctx;
    (&mut *c.cpu, &mut *c.bus, c.log.as_mut())
}

fn width(w: u8) -> Width {
    match w {
        1 => Width::Byte,
        2 => Width::Half,
        _ => Width::Word,
    }
}

#[inline(always)]
unsafe fn read(ctx: *mut JitCtx, addr: u32, w: u8) -> u32 {
    let (_, bus, log) = parts(ctx);
    let v = bus.read(addr, width(w));
    if let Some(l) = log {
        l.push(Access::Read { addr, width: w, value: v });
    }
    v
}

#[inline(always)]
unsafe fn write(ctx: *mut JitCtx, addr: u32, v: u32, w: u8) {
    let (cpu, bus, log) = parts(ctx);
    if cpu.cop0.sr & SR_ISC != 0 {
        cpu.icache_isolated_store(addr);
        return;
    }
    if let Some(l) = log {
        l.push(Access::Write { addr, width: w, value: v });
    }
    bus.write(addr, v, width(w));
}

pub extern "C" fn rd8(ctx: *mut JitCtx, addr: u32) -> u32 {
    unsafe { read(ctx, addr, 1) }
}
pub extern "C" fn rd16(ctx: *mut JitCtx, addr: u32) -> u32 {
    unsafe { read(ctx, addr, 2) }
}
pub extern "C" fn rd32(ctx: *mut JitCtx, addr: u32) -> u32 {
    unsafe { read(ctx, addr, 4) }
}
pub extern "C" fn wr8(ctx: *mut JitCtx, addr: u32, v: u32) {
    unsafe { write(ctx, addr, v & 0xFF, 1) }
}
pub extern "C" fn wr16(ctx: *mut JitCtx, addr: u32, v: u32) {
    unsafe { write(ctx, addr, v & 0xFFFF, 2) }
}
pub extern "C" fn wr32(ctx: *mut JitCtx, addr: u32, v: u32) {
    unsafe { write(ctx, addr, v, 4) }
}

/// SWL/SWR as byte-enable writes (same as the interpreter).
pub extern "C" fn swl(ctx: *mut JitCtx, addr: u32, t: u32) {
    let base = addr & !3;
    let off = addr & 3;
    for k in 0..=off {
        let b = (t >> (8 * (3 - off + k))) & 0xFF;
        unsafe { write(ctx, base + k, b, 1) };
    }
}
pub extern "C" fn swr(ctx: *mut JitCtx, addr: u32, t: u32) {
    let base = addr & !3;
    let off = addr & 3;
    for k in off..4 {
        let b = (t >> (8 * (k - off))) & 0xFF;
        unsafe { write(ctx, base + k, b, 1) };
    }
}

/// Raises an exception for the instruction at `pc`. Returns the handler.
pub extern "C" fn exception(ctx: *mut JitCtx, code: u32, pc: u32, in_delay: u32, cop: u32, bad_vaddr: u32) -> u32 {
    let (cpu, _, _) = unsafe { parts(ctx) };
    if code == exc::ADEL || code == exc::ADES {
        cpu.cop0.bad_vaddr = bad_vaddr;
    }
    cpu.current_pc = pc;
    cpu.exception(code, pc, in_delay != 0, cop);
    cpu.pc
}

fn stall_muldiv(cpu: &Cpu, bus: &mut Bus) {
    if bus.cycles < cpu.muldiv_ready {
        bus.cycles = cpu.muldiv_ready;
    }
}

pub extern "C" fn mult(ctx: *mut JitCtx, s: u32, t: u32) {
    let (cpu, bus, _) = unsafe { parts(ctx) };
    let r = (s as i32 as i64) * (t as i32 as i64);
    cpu.hi = (r >> 32) as u32;
    cpu.lo = r as u32;
    let a = if (s as i32) < 0 { !s } else { s };
    cpu.muldiv_ready = bus.cycles + mult_cycles(a);
}
pub extern "C" fn multu(ctx: *mut JitCtx, s: u32, t: u32) {
    let (cpu, bus, _) = unsafe { parts(ctx) };
    let r = (s as u64) * (t as u64);
    cpu.hi = (r >> 32) as u32;
    cpu.lo = r as u32;
    cpu.muldiv_ready = bus.cycles + mult_cycles(s);
}
pub extern "C" fn div(ctx: *mut JitCtx, s: u32, t: u32) {
    let (cpu, bus, _) = unsafe { parts(ctx) };
    let n = s as i32;
    let d = t as i32;
    if d == 0 {
        cpu.hi = n as u32;
        cpu.lo = if n >= 0 { 0xFFFF_FFFF } else { 1 };
    } else if n as u32 == 0x8000_0000 && d == -1 {
        cpu.hi = 0;
        cpu.lo = 0x8000_0000;
    } else {
        cpu.hi = (n % d) as u32;
        cpu.lo = (n / d) as u32;
    }
    cpu.muldiv_ready = bus.cycles + 36;
}
pub extern "C" fn divu(ctx: *mut JitCtx, s: u32, t: u32) {
    let (cpu, bus, _) = unsafe { parts(ctx) };
    if t == 0 {
        cpu.hi = s;
        cpu.lo = 0xFFFF_FFFF;
    } else {
        cpu.hi = s % t;
        cpu.lo = s / t;
    }
    cpu.muldiv_ready = bus.cycles + 36;
}
pub extern "C" fn mfhi(ctx: *mut JitCtx) -> u32 {
    let (cpu, bus, _) = unsafe { parts(ctx) };
    stall_muldiv(cpu, bus);
    cpu.hi
}
pub extern "C" fn mflo(ctx: *mut JitCtx) -> u32 {
    let (cpu, bus, _) = unsafe { parts(ctx) };
    stall_muldiv(cpu, bus);
    cpu.lo
}
pub extern "C" fn mthi(ctx: *mut JitCtx, v: u32) {
    unsafe { (*(*ctx).cpu).hi = v };
}
pub extern "C" fn mtlo(ctx: *mut JitCtx, v: u32) {
    unsafe { (*(*ctx).cpu).lo = v };
}

/// Pseudo-address used to log the interrupt line seen by `mfc0 cause`.
pub const IRQ_LINE_TAG: u32 = 0xFFFF_FFFC;

pub extern "C" fn mfc0(ctx: *mut JitCtx, rd: u32) -> u32 {
    let (cpu, bus, log) = unsafe { parts(ctx) };
    if rd == 13 {
        if let Some(l) = log {
            l.push(Access::Read { addr: IRQ_LINE_TAG, width: 0, value: bus.irq.line() as u32 });
        }
    }
    let c = &cpu.cop0;
    match rd {
        3 => c.bpc,
        5 => c.bda,
        6 => c.jumpdest,
        7 => c.dcic,
        8 => c.bad_vaddr,
        9 => c.bdam,
        11 => c.bpcm,
        12 => c.sr,
        13 => cpu.cause_with_irq(bus),
        14 => c.epc,
        15 => 2,
        _ => 0,
    }
}
pub extern "C" fn mtc0(ctx: *mut JitCtx, rd: u32, t: u32) {
    let (cpu, _, _) = unsafe { parts(ctx) };
    let c = &mut cpu.cop0;
    match rd {
        3 => c.bpc = t,
        5 => c.bda = t,
        7 => c.dcic = t & 0xFF80_F03F,
        9 => c.bdam = t,
        11 => c.bpcm = t,
        12 => c.sr = t & 0xF27F_FF3F,
        13 => c.cause = (c.cause & !0x300) | (t & 0x300),
        _ => {}
    }
}
pub extern "C" fn rfe(ctx: *mut JitCtx) {
    let (cpu, _, _) = unsafe { parts(ctx) };
    let sr = cpu.cop0.sr;
    cpu.cop0.sr = (sr & !0xF) | ((sr >> 2) & 0xF);
}

fn gte_stall(cpu: &Cpu, bus: &mut Bus) {
    if bus.cycles < cpu.gte_ready {
        bus.cycles = cpu.gte_ready;
    }
}

pub extern "C" fn gte_cmd(ctx: *mut JitCtx, instr: u32) {
    let (cpu, bus, _) = unsafe { parts(ctx) };
    gte_stall(cpu, bus);
    let c = cpu.gte.execute(instr);
    cpu.gte_ready = bus.cycles + c as u64;
}
pub extern "C" fn mfc2(ctx: *mut JitCtx, rd: u32) -> u32 {
    let (cpu, bus, _) = unsafe { parts(ctx) };
    gte_stall(cpu, bus);
    cpu.gte.read_data(rd as usize)
}
pub extern "C" fn cfc2(ctx: *mut JitCtx, rd: u32) -> u32 {
    let (cpu, bus, _) = unsafe { parts(ctx) };
    gte_stall(cpu, bus);
    cpu.gte.read_ctrl(rd as usize)
}
pub extern "C" fn mtc2(ctx: *mut JitCtx, rd: u32, v: u32) {
    unsafe { (*(*ctx).cpu).gte.write_data(rd as usize, v) };
}
pub extern "C" fn ctc2(ctx: *mut JitCtx, rd: u32, v: u32) {
    unsafe { (*(*ctx).cpu).gte.write_ctrl(rd as usize, v) };
}
pub extern "C" fn lwc2(ctx: *mut JitCtx, addr: u32, rt: u32) {
    let v = unsafe { read(ctx, addr, 4) };
    unsafe { (*(*ctx).cpu).gte.write_data(rt as usize, v) };
}
pub extern "C" fn swc2(ctx: *mut JitCtx, addr: u32, rt: u32) {
    let (cpu, bus, _) = unsafe { parts(ctx) };
    gte_stall(cpu, bus);
    let v = cpu.gte.read_data(rt as usize);
    unsafe { write(ctx, addr, v, 4) };
}
