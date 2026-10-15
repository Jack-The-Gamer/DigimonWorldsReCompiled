//! Validation: every translated block is replayed through the reference
//! interpreter from the same starting state and the results compared.
//!
//! Memory traffic observed while the native block ran is recorded; the
//! interpreter replays against that log (so I/O side effects happen once),
//! and must request the same reads and produce the same writes and final
//! CPU state.

use crate::block::BlockInfo;
use crate::helpers::{Access, IRQ_LINE_TAG};
use std::cell::Cell;
use dwrc_psx::bus::Width;
use dwrc_psx::{Cpu, CpuBus};
use std::collections::HashMap;

struct ReplayBus<'a> {
    log: &'a [Access],
    pos: usize,
    cycles: u64,
    writes: Vec<Access>,
    error: Option<String>,
    /// Interrupt-line samples recorded at each `mfc0 cause`.
    irq: Vec<bool>,
    irq_pos: Cell<usize>,
}

impl CpuBus for ReplayBus<'_> {
    fn read(&mut self, addr: u32, w: Width) -> u32 {
        let width = w as u8;
        // Skip writes and pseudo entries; they are compared separately.
        while let Some(Access::Write { .. } | Access::Read { addr: IRQ_LINE_TAG, width: 0, .. }) = self.log.get(self.pos) {
            self.pos += 1;
        }
        match self.log.get(self.pos) {
            Some(Access::Read { addr: a, width: wd, value }) if *a == addr && *wd == width => {
                self.pos += 1;
                *value
            }
            other => {
                if self.error.is_none() {
                    self.error = Some(format!("interpreter read{} {addr:08x}, translated code did {other:?}", width * 8));
                }
                0
            }
        }
    }
    fn write(&mut self, addr: u32, v: u32, w: Width) {
        let width = w as u8;
        let mask = match width {
            1 => 0xFF,
            2 => 0xFFFF,
            _ => 0xFFFF_FFFF,
        };
        self.writes.push(Access::Write { addr, width, value: v & mask });
    }
    fn cycles(&mut self) -> &mut u64 {
        &mut self.cycles
    }
    fn irq_line(&self) -> bool {
        let i = self.irq_pos.get();
        self.irq_pos.set(i + 1);
        self.irq.get(i).copied().unwrap_or(false)
    }
}

#[derive(Default)]
pub struct Validator {
    checks: HashMap<u32, u32>,
    pub checked: u64,
    pub mismatches: u64,
    reports: Vec<String>,
}

/// Each block address is validated this many times.
const CHECKS_PER_BLOCK: u32 = 24;

impl Validator {
    pub fn wants(&self, pc: u32) -> bool {
        self.checks.get(&pc).copied().unwrap_or(0) < CHECKS_PER_BLOCK
    }

    /// Returns false on mismatch.
    pub fn check(&mut self, snap: Cpu, blk: &BlockInfo, real: &Cpu, log: &[Access]) -> bool {
        *self.checks.entry(blk.start).or_insert(0) += 1;
        self.checked += 1;
        let mut r = snap;
        let irq: Vec<bool> = log
            .iter()
            .filter_map(|a| match a {
                Access::Read { addr: IRQ_LINE_TAG, width: 0, value } => Some(*value != 0),
                _ => None,
            })
            .collect();
        let mut rb = ReplayBus { log, pos: 0, cycles: 0, writes: Vec::new(), error: None, irq, irq_pos: Cell::new(0) };
        let n = blk.instrs.len();
        for (k, ins) in blk.instrs.iter().enumerate() {
            r.exec_instr(&mut rb, *ins);
            // Leaving the straight-line path before the end means an exception.
            let expected = blk.start.wrapping_add(4 * (k as u32 + 1));
            if k + 1 < n && r.pc != expected {
                break;
            }
        }
        let mut diffs = Vec::new();
        if let Some(e) = rb.error.take() {
            diffs.push(e);
        }
        for i in 1..32 {
            if r.regs[i] != real.regs[i] {
                diffs.push(format!("{}: interp {:08x} native {:08x}", dwrc_mips::REG_NAMES[i], r.regs[i], real.regs[i]));
            }
        }
        if r.hi != real.hi || r.lo != real.lo {
            diffs.push(format!("hi/lo: interp {:08x}/{:08x} native {:08x}/{:08x}", r.hi, r.lo, real.hi, real.lo));
        }
        if r.pc != real.pc {
            diffs.push(format!("pc: interp {:08x} native {:08x}", r.pc, real.pc));
        }
        let lr = if r.load_reg != 0 { (r.load_reg, r.load_val) } else { (0, 0) };
        let ln = if real.load_reg != 0 { (real.load_reg, real.load_val) } else { (0, 0) };
        if lr != ln {
            diffs.push(format!("pending load: interp {lr:?} native {ln:?}"));
        }
        if r.cop0 != real.cop0 {
            diffs.push(format!("cop0: interp {:?} native {:?}", r.cop0, real.cop0));
        }
        if r.gte != real.gte {
            diffs.push("GTE state differs".into());
        }
        let native_writes: Vec<Access> = log.iter().filter(|a| matches!(a, Access::Write { .. })).cloned().collect();
        if native_writes != rb.writes {
            diffs.push(format!("writes: interp {:x?} native {:x?}", rb.writes, native_writes));
        }
        if diffs.is_empty() {
            return true;
        }
        self.mismatches += 1;
        let mut s = format!("block {:08x} ({} instrs) differs:\n", blk.start, n);
        for (k, ins) in blk.instrs.iter().enumerate() {
            let pc = blk.start + k as u32 * 4;
            s.push_str(&format!("    {pc:08x}: {}\n", dwrc_mips::disasm(pc, *ins)));
        }
        for d in &diffs {
            s.push_str(&format!("  - {d}\n"));
        }
        log::error!("{s}");
        if self.reports.len() < 50 {
            self.reports.push(s);
        }
        false
    }

    pub fn report(&self) -> String {
        let mut s = format!("validation: {} block executions checked, {} mismatches", self.checked, self.mismatches);
        for r in &self.reports {
            s.push('\n');
            s.push_str(r);
        }
        s
    }
}
