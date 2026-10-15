//! MIPS R3000A (PlayStation CPU) instruction decoding and disassembly.
//!
//! Shared by the reference interpreter, the static recompiler and the
//! developer tools so that every component agrees on what an instruction is.

use std::fmt;

/// A raw 32-bit instruction word with field accessors.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Instr(pub u32);

impl Instr {
    #[inline(always)]
    pub fn op(self) -> u32 {
        self.0 >> 26
    }
    #[inline(always)]
    pub fn rs(self) -> usize {
        ((self.0 >> 21) & 0x1F) as usize
    }
    #[inline(always)]
    pub fn rt(self) -> usize {
        ((self.0 >> 16) & 0x1F) as usize
    }
    #[inline(always)]
    pub fn rd(self) -> usize {
        ((self.0 >> 11) & 0x1F) as usize
    }
    #[inline(always)]
    pub fn shamt(self) -> u32 {
        (self.0 >> 6) & 0x1F
    }
    #[inline(always)]
    pub fn funct(self) -> u32 {
        self.0 & 0x3F
    }
    #[inline(always)]
    pub fn imm(self) -> u32 {
        self.0 & 0xFFFF
    }
    #[inline(always)]
    pub fn simm(self) -> u32 {
        (self.0 as u16) as i16 as i32 as u32
    }
    #[inline(always)]
    pub fn target(self) -> u32 {
        self.0 & 0x03FF_FFFF
    }
    #[inline(always)]
    pub fn cop_op(self) -> u32 {
        (self.0 >> 21) & 0x1F
    }
}

impl fmt::Debug for Instr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Instr({:08x})", self.0)
    }
}

/// Decoded operation. Field values are taken from the [`Instr`] as needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Op {
    // SPECIAL
    Sll,
    Srl,
    Sra,
    Sllv,
    Srlv,
    Srav,
    Jr,
    Jalr,
    Syscall,
    Break,
    Mfhi,
    Mthi,
    Mflo,
    Mtlo,
    Mult,
    Multu,
    Div,
    Divu,
    Add,
    Addu,
    Sub,
    Subu,
    And,
    Or,
    Xor,
    Nor,
    Slt,
    Sltu,
    // REGIMM
    Bltz,
    Bgez,
    Bltzal,
    Bgezal,
    // Normal
    J,
    Jal,
    Beq,
    Bne,
    Blez,
    Bgtz,
    Addi,
    Addiu,
    Slti,
    Sltiu,
    Andi,
    Ori,
    Xori,
    Lui,
    // Coprocessors
    Mfc0,
    Mtc0,
    Rfe,
    Mfc2,
    Cfc2,
    Mtc2,
    Ctc2,
    Cop2Cmd,
    Lwc2,
    Swc2,
    /// Coprocessor 1/3 or unusable COPn access (raises Coprocessor Unusable).
    CopUnusable,
    Lb,
    Lh,
    Lwl,
    Lw,
    Lbu,
    Lhu,
    Lwr,
    Sb,
    Sh,
    Swl,
    Sw,
    Swr,
    /// Opcodes that raise Reserved Instruction.
    Illegal,
}

#[inline]
pub fn decode(i: Instr) -> Op {
    match i.op() {
        0x00 => match i.funct() {
            0x00 => Op::Sll,
            0x02 => Op::Srl,
            0x03 => Op::Sra,
            0x04 => Op::Sllv,
            0x06 => Op::Srlv,
            0x07 => Op::Srav,
            0x08 => Op::Jr,
            0x09 => Op::Jalr,
            0x0C => Op::Syscall,
            0x0D => Op::Break,
            0x10 => Op::Mfhi,
            0x11 => Op::Mthi,
            0x12 => Op::Mflo,
            0x13 => Op::Mtlo,
            0x18 => Op::Mult,
            0x19 => Op::Multu,
            0x1A => Op::Div,
            0x1B => Op::Divu,
            0x20 => Op::Add,
            0x21 => Op::Addu,
            0x22 => Op::Sub,
            0x23 => Op::Subu,
            0x24 => Op::And,
            0x25 => Op::Or,
            0x26 => Op::Xor,
            0x27 => Op::Nor,
            0x2A => Op::Slt,
            0x2B => Op::Sltu,
            _ => Op::Illegal,
        },
        0x01 => {
            // The R3000A decodes REGIMM loosely: bit 16 selects GEZ/LTZ and
            // the link variant is chosen when bits 20..17 == 0b1000.
            let rt = i.rt() as u32;
            let ge = rt & 1 != 0;
            let link = (rt & 0x1E) == 0x10;
            match (ge, link) {
                (false, false) => Op::Bltz,
                (true, false) => Op::Bgez,
                (false, true) => Op::Bltzal,
                (true, true) => Op::Bgezal,
            }
        }
        0x02 => Op::J,
        0x03 => Op::Jal,
        0x04 => Op::Beq,
        0x05 => Op::Bne,
        0x06 => Op::Blez,
        0x07 => Op::Bgtz,
        0x08 => Op::Addi,
        0x09 => Op::Addiu,
        0x0A => Op::Slti,
        0x0B => Op::Sltiu,
        0x0C => Op::Andi,
        0x0D => Op::Ori,
        0x0E => Op::Xori,
        0x0F => Op::Lui,
        0x10 => match i.cop_op() {
            0x00 => Op::Mfc0,
            0x04 => Op::Mtc0,
            0x10 if i.funct() == 0x10 => Op::Rfe,
            _ => Op::Illegal,
        },
        0x12 => {
            if i.0 & (1 << 25) != 0 {
                Op::Cop2Cmd
            } else {
                match i.cop_op() {
                    0x00 => Op::Mfc2,
                    0x02 => Op::Cfc2,
                    0x04 => Op::Mtc2,
                    0x06 => Op::Ctc2,
                    _ => Op::Illegal,
                }
            }
        }
        0x11 | 0x13 => Op::CopUnusable,
        0x20 => Op::Lb,
        0x21 => Op::Lh,
        0x22 => Op::Lwl,
        0x23 => Op::Lw,
        0x24 => Op::Lbu,
        0x25 => Op::Lhu,
        0x26 => Op::Lwr,
        0x28 => Op::Sb,
        0x29 => Op::Sh,
        0x2A => Op::Swl,
        0x2B => Op::Sw,
        0x2E => Op::Swr,
        0x31 | 0x33 => Op::CopUnusable, // LWC1 / LWC3
        0x32 => Op::Lwc2,
        0x39 | 0x3B => Op::CopUnusable, // SWC1 / SWC3
        0x3A => Op::Swc2,
        _ => Op::Illegal,
    }
}

/// Control-flow classification used by the recompiler's analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    /// Falls through to the next instruction.
    Next,
    /// Conditional branch with a delay slot (target computed from `pc`).
    Branch { target: u32, link: bool },
    /// Unconditional direct jump with a delay slot.
    Jump { target: u32, link: bool },
    /// Indirect jump through a register with a delay slot.
    JumpReg { rs: usize, link_rd: Option<usize> },
    /// Raises an exception (syscall/break/illegal).
    Trap,
    /// Return from exception (does not end a block on its own).
    Rfe,
}

pub fn flow(pc: u32, i: Instr) -> Flow {
    let btarget = pc.wrapping_add(4).wrapping_add(i.simm() << 2);
    match decode(i) {
        Op::Beq | Op::Bne | Op::Blez | Op::Bgtz | Op::Bltz | Op::Bgez => Flow::Branch { target: btarget, link: false },
        Op::Bltzal | Op::Bgezal => Flow::Branch { target: btarget, link: true },
        Op::J => Flow::Jump { target: (pc.wrapping_add(4) & 0xF000_0000) | (i.target() << 2), link: false },
        Op::Jal => Flow::Jump { target: (pc.wrapping_add(4) & 0xF000_0000) | (i.target() << 2), link: true },
        Op::Jr => Flow::JumpReg { rs: i.rs(), link_rd: None },
        Op::Jalr => Flow::JumpReg { rs: i.rs(), link_rd: Some(i.rd()) },
        Op::Syscall | Op::Break | Op::Illegal | Op::CopUnusable => Flow::Trap,
        Op::Rfe => Flow::Rfe,
        _ => Flow::Next,
    }
}

impl Flow {
    pub fn has_delay_slot(self) -> bool {
        matches!(self, Flow::Branch { .. } | Flow::Jump { .. } | Flow::JumpReg { .. })
    }
}

/// Register usage of an instruction: (sources, destination GPR).
pub fn gpr_usage(i: Instr) -> ([Option<usize>; 2], Option<usize>) {
    use Op::*;
    let (rs, rt, rd) = (Some(i.rs()), Some(i.rt()), Some(i.rd()));
    let d = |r: Option<usize>| r.filter(|&r| r != 0);
    match decode(i) {
        Sll | Srl | Sra => ([rt, None], d(rd)),
        Sllv | Srlv | Srav | Add | Addu | Sub | Subu | And | Or | Xor | Nor | Slt | Sltu => ([rs, rt], d(rd)),
        Jr => ([rs, None], None),
        Jalr => ([rs, None], d(rd)),
        Mfhi | Mflo => ([None, None], d(rd)),
        Mthi | Mtlo => ([rs, None], None),
        Mult | Multu | Div | Divu => ([rs, rt], None),
        Beq | Bne => ([rs, rt], None),
        Blez | Bgtz | Bltz | Bgez => ([rs, None], None),
        Bltzal | Bgezal => ([rs, None], Some(31)),
        Jal => ([None, None], Some(31)),
        Addi | Addiu | Slti | Sltiu | Andi | Ori | Xori => ([rs, None], d(rt)),
        Lui => ([None, None], d(rt)),
        Mfc0 | Mfc2 | Cfc2 => ([None, None], d(rt)),
        Mtc0 | Mtc2 | Ctc2 => ([rt, None], None),
        Lb | Lh | Lw | Lbu | Lhu => ([rs, None], d(rt)),
        Lwl | Lwr => ([rs, rt], d(rt)),
        Sb | Sh | Sw | Swl | Swr => ([rs, rt], None),
        Lwc2 | Swc2 => ([rs, None], None),
        _ => ([None, None], None),
    }
}

pub fn is_load(op: Op) -> bool {
    matches!(op, Op::Lb | Op::Lh | Op::Lw | Op::Lbu | Op::Lhu | Op::Lwl | Op::Lwr | Op::Mfc0 | Op::Mfc2 | Op::Cfc2)
}

pub const REG_NAMES: [&str; 32] = [
    "zero", "at", "v0", "v1", "a0", "a1", "a2", "a3", "t0", "t1", "t2", "t3", "t4", "t5", "t6", "t7", "s0", "s1", "s2",
    "s3", "s4", "s5", "s6", "s7", "t8", "t9", "k0", "k1", "gp", "sp", "fp", "ra",
];

const GTE_CMDS: [(u32, &str); 22] = [
    (0x01, "rtps"),
    (0x06, "nclip"),
    (0x0C, "op"),
    (0x10, "dpcs"),
    (0x11, "intpl"),
    (0x12, "mvmva"),
    (0x13, "ncds"),
    (0x14, "cdp"),
    (0x16, "ncdt"),
    (0x1B, "nccs"),
    (0x1C, "cc"),
    (0x1E, "ncs"),
    (0x20, "nct"),
    (0x28, "sqr"),
    (0x29, "dcpl"),
    (0x2A, "dpct"),
    (0x2D, "avsz3"),
    (0x2E, "avsz4"),
    (0x30, "rtpt"),
    (0x3D, "gpf"),
    (0x3E, "gpl"),
    (0x3F, "ncct"),
];

/// Disassembles one instruction at `pc` into human-readable text.
pub fn disasm(pc: u32, i: Instr) -> String {
    use Op::*;
    let r = |n: usize| REG_NAMES[n];
    let op = decode(i);
    let name = format!("{op:?}").to_ascii_lowercase();
    match op {
        Sll if i.0 == 0 => "nop".into(),
        Sll | Srl | Sra => format!("{name} {}, {}, {}", r(i.rd()), r(i.rt()), i.shamt()),
        Sllv | Srlv | Srav => format!("{name} {}, {}, {}", r(i.rd()), r(i.rt()), r(i.rs())),
        Jr => format!("jr {}", r(i.rs())),
        Jalr => format!("jalr {}, {}", r(i.rd()), r(i.rs())),
        Syscall | Break => format!("{name} {:#x}", (i.0 >> 6) & 0xFFFFF),
        Mfhi | Mflo => format!("{name} {}", r(i.rd())),
        Mthi | Mtlo => format!("{name} {}", r(i.rs())),
        Mult | Multu | Div | Divu => format!("{name} {}, {}", r(i.rs()), r(i.rt())),
        Add | Addu | Sub | Subu | And | Or | Xor | Nor | Slt | Sltu => {
            format!("{name} {}, {}, {}", r(i.rd()), r(i.rs()), r(i.rt()))
        }
        Bltz | Bgez | Bltzal | Bgezal | Blez | Bgtz => {
            format!("{name} {}, {:08x}", r(i.rs()), pc.wrapping_add(4).wrapping_add(i.simm() << 2))
        }
        Beq | Bne => format!("{name} {}, {}, {:08x}", r(i.rs()), r(i.rt()), pc.wrapping_add(4).wrapping_add(i.simm() << 2)),
        J | Jal => format!("{name} {:08x}", (pc.wrapping_add(4) & 0xF000_0000) | (i.target() << 2)),
        Addi | Addiu | Slti | Sltiu => format!("{name} {}, {}, {}", r(i.rt()), r(i.rs()), i.simm() as i32),
        Andi | Ori | Xori => format!("{name} {}, {}, {:#x}", r(i.rt()), r(i.rs()), i.imm()),
        Lui => format!("lui {}, {:#x}", r(i.rt()), i.imm()),
        Mfc0 | Mtc0 => format!("{name} {}, cop0r{}", r(i.rt()), i.rd()),
        Mfc2 | Mtc2 => format!("{name} {}, gte_d{}", r(i.rt()), i.rd()),
        Cfc2 | Ctc2 => format!("{name} {}, gte_c{}", r(i.rt()), i.rd()),
        Rfe => "rfe".into(),
        Cop2Cmd => {
            let cmd = i.0 & 0x3F;
            let n = GTE_CMDS.iter().find(|(c, _)| *c == cmd).map(|(_, n)| *n).unwrap_or("gte?");
            format!("{n} {:#07x}", i.0 & 0x1FF_FFFF)
        }
        Lb | Lh | Lwl | Lw | Lbu | Lhu | Lwr | Sb | Sh | Swl | Sw | Swr => {
            format!("{name} {}, {}({})", r(i.rt()), i.simm() as i32, r(i.rs()))
        }
        Lwc2 | Swc2 => format!("{name} gte_d{}, {}({})", i.rt(), i.simm() as i32, r(i.rs())),
        CopUnusable => format!("cop? {:08x}", i.0),
        Illegal => format!(".word {:08x}", i.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decodes_common() {
        assert_eq!(decode(Instr(0x27BDFFE8)), Op::Addiu); // addiu sp, sp, -24
        assert_eq!(disasm(0, Instr(0x27BDFFE8)), "addiu sp, sp, -24");
        assert_eq!(decode(Instr(0x03E00008)), Op::Jr);
        assert_eq!(decode(Instr(0x0C000000)), Op::Jal);
        assert_eq!(decode(Instr(0x4A180001)), Op::Cop2Cmd);
        assert_eq!(disasm(0, Instr(0)), "nop");
        assert_eq!(flow(0x80010000, Instr(0x1000FFFF)), Flow::Branch { target: 0x80010000, link: false });
    }
}
