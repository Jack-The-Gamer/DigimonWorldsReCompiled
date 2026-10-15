//! Basic-block discovery over guest code.

use dwrc_mips::{decode, flow, Flow, Instr, Op};
use dwrc_psx::bus::mask_region;
use dwrc_psx::Bus;

/// Maximum instructions in one translated block.
pub const MAX_BLOCK: usize = 96;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    /// Ends with a branch/jump at `len-2` and its delay slot at `len-1`.
    Branch,
    /// Ends with an instruction that always raises an exception.
    Trap,
    /// Ends after an instruction that changes CPU mode (MTC0/RFE), so
    /// interrupts are re-checked.
    ModeChange,
    /// Falls through to the next address (size limit or hook boundary).
    Fallthrough,
}

#[derive(Debug, Clone)]
pub struct BlockInfo {
    pub start: u32,
    pub instrs: Vec<Instr>,
    pub end: End,
    pub uses_cop2: bool,
}

impl BlockInfo {
    pub fn end_addr(&self) -> u32 {
        self.start.wrapping_add(self.instrs.len() as u32 * 4)
    }

    /// Statically known successors (for compile-ahead discovery).
    pub fn successors(&self) -> Vec<u32> {
        let mut v = Vec::new();
        match self.end {
            End::Branch => {
                let bi = self.instrs.len() - 2;
                let bpc = self.start + bi as u32 * 4;
                match flow(bpc, self.instrs[bi]) {
                    Flow::Branch { target, .. } => {
                        v.push(target);
                        v.push(self.end_addr());
                    }
                    Flow::Jump { target, link } => {
                        v.push(target);
                        if link {
                            v.push(self.end_addr());
                        }
                    }
                    Flow::JumpReg { link_rd, .. } => {
                        if link_rd.is_some() {
                            v.push(self.end_addr());
                        }
                    }
                    _ => {}
                }
            }
            End::Fallthrough | End::ModeChange => v.push(self.end_addr()),
            End::Trap => {
                // SYSCALL/BREAK return to the next instruction.
                if let Some(last) = self.instrs.last() {
                    if matches!(decode(*last), Op::Syscall | Op::Break) {
                        v.push(self.end_addr());
                    }
                }
            }
        }
        v
    }
}

/// Is `addr` in memory we can translate (RAM or BIOS ROM)?
pub fn translatable(addr: u32) -> bool {
    let p = mask_region(addr);
    p < 0x80_0000 || (0x1FC0_0000..0x1FC8_0000).contains(&p)
}

/// Decodes the block starting at `start`. Returns `None` for code that
/// must be left to the interpreter (e.g. a branch in a delay slot).
pub fn discover(bus: &Bus, start: u32) -> Option<BlockInfo> {
    if start & 3 != 0 || !translatable(start) {
        return None;
    }
    let mut instrs = Vec::new();
    let mut pc = start;
    let mut uses_cop2 = false;
    let end;
    loop {
        if !translatable(pc) {
            end = End::Fallthrough;
            break;
        }
        if !instrs.is_empty() && bus.has_hooks && bus.hook_at(pc) {
            end = End::Fallthrough;
            break;
        }
        let i = Instr(bus.peek32(pc));
        let op = decode(i);
        if matches!(op, Op::Mfc2 | Op::Cfc2 | Op::Mtc2 | Op::Ctc2 | Op::Cop2Cmd | Op::Lwc2 | Op::Swc2) {
            uses_cop2 = true;
        }
        instrs.push(i);
        let f = flow(pc, i);
        if f.has_delay_slot() {
            let ds_pc = pc.wrapping_add(4);
            if !translatable(ds_pc) || (bus.has_hooks && bus.hook_at(ds_pc)) {
                return None;
            }
            let ds = Instr(bus.peek32(ds_pc));
            let dop = decode(ds);
            if flow(ds_pc, ds).has_delay_slot() || matches!(dop, Op::Mtc0 | Op::Rfe) {
                return None; // branch in a delay slot: leave to the interpreter
            }
            if matches!(dop, Op::Mfc2 | Op::Cfc2 | Op::Mtc2 | Op::Ctc2 | Op::Cop2Cmd | Op::Lwc2 | Op::Swc2) {
                uses_cop2 = true;
            }
            instrs.push(ds);
            end = End::Branch;
            break;
        }
        match f {
            Flow::Trap => {
                end = End::Trap;
                break;
            }
            Flow::Rfe => {
                end = End::ModeChange;
                break;
            }
            _ => {}
        }
        if op == Op::Mtc0 {
            end = End::ModeChange;
            break;
        }
        pc = pc.wrapping_add(4);
        if instrs.len() >= MAX_BLOCK {
            end = End::Fallthrough;
            break;
        }
    }
    Some(BlockInfo { start, instrs, end, uses_cop2 })
}
