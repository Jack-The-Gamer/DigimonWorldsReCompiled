//! Differential fuzzing: random R3000A programs are executed by the
//! reference interpreter and by recompiled native code; the final CPU and
//! memory state must match exactly. Runs without any game data.

use dwrc_psx::{Psx, VideoStandard};

/// Small deterministic PRNG (xorshift) so failures are reproducible.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        (x >> 16) as u32
    }
    fn below(&mut self, n: u32) -> u32 {
        self.next() % n
    }
}

const CODE: u32 = 0x8001_0000;
const DATA: u32 = 0x8010_0000; // data area base kept in $s7 (r23)
const DONE: u32 = 0x8001_F000; // final infinite loop

fn r(rs: u32, rt: u32, rd: u32, sh: u32, f: u32) -> u32 {
    (rs << 21) | (rt << 16) | (rd << 11) | (sh << 6) | f
}
fn i(op: u32, rs: u32, rt: u32, imm: u32) -> u32 {
    (op << 26) | (rs << 21) | (rt << 16) | (imm & 0xFFFF)
}

/// Registers the program may freely use (not r0, s7 = data base, k0/k1).
const REGS: [u32; 20] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 24, 25];

fn gen_program(rng: &mut Rng, len: usize) -> Vec<u32> {
    let mut p = Vec::new();
    let reg = |rng: &mut Rng| REGS[rng.below(REGS.len() as u32) as usize];
    // Seed registers.
    for &rr in REGS.iter() {
        p.push(i(0x0F, 0, rr, rng.next()));
        p.push(i(0x0D, rr, rr, rng.next()));
    }
    p.push(i(0x0F, 0, 23, DATA >> 16));
    while p.len() < len {
        let (a, b, c) = (reg(rng), reg(rng), reg(rng));
        match rng.below(16) {
            0 => {
                // R-type ALU (no overflow-trapping ADD/SUB)
                let f = [0x21, 0x23, 0x24, 0x25, 0x26, 0x27, 0x2A, 0x2B][rng.below(8) as usize];
                p.push(r(a, b, c, 0, f));
            }
            1 => {
                let f = [0x00, 0x02, 0x03][rng.below(3) as usize];
                p.push(r(0, a, c, rng.below(32), f));
            }
            2 => {
                let f = [0x04, 0x06, 0x07][rng.below(3) as usize];
                p.push(r(a, b, c, 0, f));
            }
            3 => {
                let op = [0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E][rng.below(6) as usize];
                p.push(i(op, a, c, rng.next()));
            }
            4 => p.push(i(0x0F, 0, c, rng.next())),
            5 => {
                // mult/div then read hi/lo
                let f = [0x18, 0x19, 0x1A, 0x1B][rng.below(4) as usize];
                p.push(r(a, b, 0, 0, f));
                p.push(r(0, 0, c, 0, [0x10, 0x12][rng.below(2) as usize]));
            }
            6 | 7 => {
                // aligned store then load (offsets within the data area)
                let w = rng.below(3);
                let off = rng.below(256) * 4;
                let sop = [0x28, 0x29, 0x2B][w as usize];
                p.push(i(sop, 23, b, off));
                let lop = [[0x20, 0x24], [0x21, 0x25], [0x23, 0x23]][w as usize][rng.below(2) as usize];
                p.push(i(lop, 23, c, off));
            }
            8 => {
                // load-delay hazard: use the loaded register immediately
                let off = rng.below(256) * 4;
                p.push(i(0x23, 23, c, off));
                p.push(r(c, a, b, 0, 0x21));
            }
            9 => {
                // unaligned LWL/LWR pair and SWL/SWR
                let off = rng.below(1000);
                if rng.below(2) == 0 {
                    p.push(i(0x22, 23, c, off + 3));
                    p.push(i(0x26, 23, c, off));
                } else {
                    p.push(i(0x2A, 23, b, off + 3));
                    p.push(i(0x2E, 23, b, off));
                }
            }
            10 => {
                // forward conditional branch over a few instructions
                let skip = 1 + rng.below(3);
                let op = [0x04, 0x05, 0x06, 0x07][rng.below(4) as usize];
                p.push(i(op, a, if op < 6 { b } else { 0 }, skip));
                p.push(r(a, b, c, 0, 0x21)); // delay slot
                for _ in 0..skip {
                    p.push(r(b, c, a, 0, 0x26));
                }
            }
            11 => {
                // REGIMM branch (incl. link variants)
                let rt = [0x00, 0x01, 0x10, 0x11][rng.below(4) as usize];
                p.push(i(0x01, a, rt, 2));
                p.push(r(c, a, b, 0, 0x25));
                p.push(r(a, a, c, 0, 0x23));
                p.push(r(b, b, a, 0, 0x24));
            }
            12 => {
                // jal to the next-next instruction (link + delay slot)
                let here = CODE + 4 * p.len() as u32;
                let target = here + 12;
                p.push((0x03 << 26) | ((target >> 2) & 0x03FF_FFFF));
                p.push(r(a, b, c, 0, 0x21));
                p.push(r(c, c, c, 0, 0x26)); // skipped
            }
            13 => {
                // load followed by a load to the same register (delay cancel)
                let off = rng.below(256) * 4;
                p.push(i(0x23, 23, c, off));
                p.push(i(0x23, 23, c, off + 4));
                p.push(r(c, 0, a, 0, 0x21));
            }
            14 => {
                // ALU write to a register with a load in flight
                let off = rng.below(256) * 4;
                p.push(i(0x23, 23, c, off));
                p.push(i(0x09, a, c, 7));
                p.push(r(c, 0, b, 0, 0x21));
            }
            _ => {
                // byte/halfword sign/zero extension mix
                let off = rng.below(1024);
                p.push(i(0x28, 23, a, off));
                p.push(i(0x20, 23, b, off));
                p.push(i(0x24, 23, c, off));
            }
        }
    }
    // jump to the done loop
    p.push((0x02 << 26) | ((DONE >> 2) & 0x03FF_FFFF));
    p.push(0);
    p
}

fn run(program: &[u32], recompiled: bool) -> Psx {
    let bios = vec![0u8; 512 * 1024];
    let mut psx = Psx::new(&bios, VideoStandard::Ntsc);
    if recompiled {
        psx.set_engine(dwrc_recomp::new_engine(dwrc_recomp::Mode::Native));
    }
    for (k, w) in program.iter().enumerate() {
        psx.bus.poke32(CODE + 4 * k as u32, *w);
    }
    // done: b done ; nop
    psx.bus.poke32(DONE, 0x1000_FFFF);
    psx.bus.poke32(DONE + 4, 0);
    // pseudo-random data area
    let mut rng = Rng(0x1234_5678_9ABC_DEF0);
    for k in 0..1024u32 {
        psx.bus.poke32(DATA + 4 * k, rng.next() ^ (rng.next() << 16));
    }
    psx.cpu.cop0.sr = 0; // interrupts off, no BEV
    psx.bus.cache_ctrl = 0x804;
    psx.cpu.set_pc(CODE);
    let mut guard = 0;
    while psx.cpu.pc & !0xF != DONE & !0xF {
        psx.run_cycles(2000);
        guard += 1;
        assert!(guard < 10_000, "program did not terminate (pc {:08x})", psx.cpu.pc);
    }
    psx
}

#[test]
fn random_programs_match_interpreter() {
    for seed in 1..=40u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let prog = gen_program(&mut rng, 300 + (seed as usize % 7) * 50);
        let a = run(&prog, false);
        let b = run(&prog, true);
        for k in 0..32 {
            assert_eq!(a.cpu.regs[k], b.cpu.regs[k], "seed {seed}: r{k} differs");
        }
        assert_eq!((a.cpu.hi, a.cpu.lo), (b.cpu.hi, b.cpu.lo), "seed {seed}: hi/lo differ");
        for k in 0..1024u32 {
            assert_eq!(a.bus.peek32(DATA + 4 * k), b.bus.peek32(DATA + 4 * k), "seed {seed}: data word {k} differs");
        }
    }
}
