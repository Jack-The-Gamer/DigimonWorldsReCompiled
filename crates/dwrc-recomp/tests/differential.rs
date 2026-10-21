//! Differential fuzzing: random R3000A programs are executed by the
//! reference interpreter and by recompiled native code; the final CPU and
//! memory state must match exactly. Blocks waiting for their translation
//! run on the interpreter with the translation's timing, so the recompiler
//! must end in the same state, to the cycle, whether its blocks ran
//! natively, on the interpreter or a mix of both. Runs without any game data.

use dwrc_psx::{HookAction, Psx, VideoStandard};
use dwrc_recomp::{Mode, Translation};

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

/// Runs `program` on the interpreter (`None`) or the recompiler.
fn run(program: &[u32], recompiler: Option<Translation>) -> Psx {
    match recompiler {
        Some(t) => run_with(program, 1, || dwrc_recomp::new_engine_with(Mode::Native, t)),
        None => run_with(program, 1, || Box::new(dwrc_psx::Interpreter)),
    }
}

/// Runs `program` `times` times over (a loop around it, so its blocks run
/// often). Looped runs pause early on, after the first blocks ran, for 60
/// single blocks 5 ms apart: background translations arrive meanwhile and
/// later in the run, however busy the machine is. The pause is the same
/// for every engine, so runs to compare are sliced alike (the run ends at
/// the first slice boundary in the final loop).
fn run_with(program: &[u32], times: u32, engine: impl FnOnce() -> Box<dyn dwrc_psx::ExecEngine>) -> Psx {
    let bios = vec![0u8; 512 * 1024];
    let mut psx = Psx::new(&bios, VideoStandard::Ntsc);
    psx.set_engine(engine());
    for (k, w) in program.iter().enumerate() {
        psx.bus.poke32(CODE + 4 * k as u32, *w);
    }
    if times > 1 {
        // $s4 counts the runs: set before CODE, then the program's final
        // jump becomes `addiu $s4, -1 ; bne $s4, $0, CODE ; nop ; j done ; nop`.
        psx.bus.poke32(CODE - 4, i(0x09, 0, 20, times));
        let tail = CODE + 4 * (program.len() as u32 - 2);
        psx.bus.poke32(tail, i(0x09, 20, 20, 0xFFFF));
        psx.bus.poke32(tail + 4, i(0x05, 20, 0, (CODE.wrapping_sub(tail + 8) >> 2) & 0xFFFF));
        psx.bus.poke32(tail + 8, 0);
        psx.bus.poke32(tail + 12, (0x02 << 26) | ((DONE >> 2) & 0x03FF_FFFF));
        psx.bus.poke32(tail + 16, 0);
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
    psx.cpu.set_pc(if times > 1 { CODE - 4 } else { CODE });
    let mut guard = 0;
    while psx.cpu.pc & !0xF != DONE & !0xF {
        psx.run_cycles(2000);
        if times > 1 && guard == 10 {
            for _ in 0..60 {
                std::thread::sleep(std::time::Duration::from_millis(5));
                psx.run_cycles(1); // one block, taking the translations that arrived
            }
        }
        guard += 1;
        assert!(guard < 100_000, "program did not terminate (pc {:08x})", psx.cpu.pc);
    }
    psx
}

#[test]
fn random_programs_match_interpreter() {
    for seed in 1..=40u64 {
        let prog = program(seed);
        let a = run(&prog, None);
        let b = run(&prog, Some(Translation::Inline));
        for k in 0..32 {
            assert_eq!(a.cpu.regs[k], b.cpu.regs[k], "seed {seed}: r{k} differs");
        }
        assert_eq!((a.cpu.hi, a.cpu.lo), (b.cpu.hi, b.cpu.lo), "seed {seed}: hi/lo differ");
        for k in 0..1024u32 {
            assert_eq!(a.bus.peek32(DATA + 4 * k), b.bus.peek32(DATA + 4 * k), "seed {seed}: data word {k} differs");
        }
    }
}

fn program(seed: u64) -> Vec<u32> {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    gen_program(&mut rng, 300 + (seed as usize % 7) * 50)
}

/// Everything two recompiler runs must agree on, cycles included.
fn same_console(a: &mut Psx, b: &mut Psx, what: &str) {
    assert_eq!(a.cpu.regs, b.cpu.regs, "{what}: registers differ");
    assert_eq!((a.cpu.hi, a.cpu.lo, a.cpu.pc), (b.cpu.hi, b.cpu.lo, b.cpu.pc), "{what}: hi/lo/pc differ");
    assert_eq!((a.cpu.load_reg, a.cpu.load_val), (b.cpu.load_reg, b.cpu.load_val), "{what}: pending load differs");
    assert_eq!(a.cpu.current_pc, b.cpu.current_pc, "{what}: current pc differs");
    assert_eq!(a.cpu.instructions, b.cpu.instructions, "{what}: instruction counts differ");
    assert_eq!(a.bus.cycles, b.bus.cycles, "{what}: cycles differ");
    assert!(a.bus.ram[..] == b.bus.ram[..], "{what}: RAM differs");
    // And everything else a save state holds.
    assert!(a.save_state() == b.save_state(), "{what}: console states differ");
}

/// A number from the engine report, after `label`.
fn reported(psx: &Psx, label: &str) -> u64 {
    let r = psx.engine_report();
    let at = r.find(label).unwrap_or_else(|| panic!("no {label:?} in {r:?}")) + label.len();
    r[at..].trim_start().split(|c: char| !c.is_ascii_digit()).next().unwrap().parse().unwrap()
}

#[test]
fn interpreted_blocks_match_translations() {
    for seed in 1..=40u64 {
        let prog = program(seed);
        let mut native = run(&prog, Some(Translation::Inline));
        let mut interpreted = run(&prog, Some(Translation::Off));
        same_console(&mut native, &mut interpreted, &format!("seed {seed}, interpreted blocks"));
    }
}

/// Translations arriving while the program runs: blocks switch from the
/// interpreter to native code at whatever moment the worker finishes them.
#[test]
fn background_translation_matches_inline() {
    for seed in 1..=8u64 {
        let prog = program(seed);
        let mut inline = run_with(&prog, 100, || dwrc_recomp::new_engine_with(Mode::Native, Translation::Inline));
        let mut background = run_with(&prog, 100, || dwrc_recomp::new_engine_with(Mode::Native, Translation::Background));
        let (native, interpreted) = (reported(&background, "run"), reported(&background, "on the interpreter"));
        assert!(native > 0 && interpreted > 0, "seed {seed}: not a mix ({native} block runs, {interpreted} on the interpreter)");
        same_console(&mut inline, &mut background, &format!("seed {seed}, background translation"));
    }
}

/// The code arena filling up while translations are on their way: the
/// engine drops every translation, the worker starts over, and nothing
/// translated before is ever installed or run again.
#[test]
fn starting_over_keeps_the_console_exact() {
    for seed in 1..=8u64 {
        let prog = program(seed);
        let mut inline = run_with(&prog, 100, || dwrc_recomp::new_engine_with(Mode::Native, Translation::Inline));
        let mut small = run_with(&prog, 100, || {
            let mut e = dwrc_recomp::RecompEngine::new(false, Translation::Background);
            e.code_limit = 4 << 10;
            Box::new(e)
        });
        assert!(reported(&small, "started over") > 0, "seed {seed}: the arena never filled: {}", small.engine_report());
        same_console(&mut inline, &mut small, &format!("seed {seed}, small code arena"));
    }
}

/// Several hooks on one address run in the order they were added, every
/// time, on every engine; a one-shot hook among them runs once (its removal
/// makes the recompiler flush).
#[test]
fn hooks_on_one_address_run_in_order() {
    const LOOP: u32 = CODE + 4;
    let program = [
        i(0x09, 0, 20, 50),                                            // addiu $s4, $0, 50
        i(0x09, 22, 22, 1),                                            // loop: addiu $s6, $s6, 1
        i(0x09, 20, 20, 0xFFFF),                                       // addiu $s4, $s4, -1
        i(0x05, 20, 0, (LOOP.wrapping_sub(CODE + 16) >> 2) & 0xFFFF), // bne $s4, $0, loop
        0,
        (0x02 << 26) | ((DONE >> 2) & 0x03FF_FFFF), // j done
        0,
    ];
    let run = |engine: Box<dyn dwrc_psx::ExecEngine>| {
        let mut psx = Psx::new(&vec![0u8; 512 * 1024], VideoStandard::Ntsc);
        psx.set_engine(engine);
        for (k, w) in program.iter().enumerate() {
            psx.bus.poke32(CODE + 4 * k as u32, *w);
        }
        psx.bus.poke32(DONE, 0x1000_FFFF);
        psx.bus.poke32(DONE + 4, 0);
        // $s5 holds a value only this order of hooks gives; $s6 counts the loops.
        psx.bus.add_hook(
            LOOP,
            Box::new(|cpu, _| {
                cpu.regs[21] = cpu.regs[21].wrapping_mul(3).wrapping_add(1);
                HookAction::Continue
            }),
        );
        psx.bus.add_hook(
            LOOP,
            Box::new(|cpu, _| {
                cpu.regs[21] = cpu.regs[21].rotate_left(5) ^ cpu.regs[22];
                HookAction::Continue
            }),
        );
        psx.bus.add_hook_once(
            LOOP,
            Box::new(|cpu, _| {
                cpu.regs[21] = cpu.regs[21].wrapping_add(1000);
                HookAction::Continue
            }),
        );
        psx.cpu.cop0.sr = 0;
        psx.bus.cache_ctrl = 0x804;
        psx.cpu.set_pc(CODE);
        let mut guard = 0;
        while psx.cpu.pc & !0xF != DONE & !0xF {
            psx.run_cycles(2000);
            guard += 1;
            assert!(guard < 10_000, "program did not terminate (pc {:08x})", psx.cpu.pc);
        }
        psx
    };
    let mut expected = 0u32;
    for n in 0..50 {
        expected = expected.wrapping_mul(3).wrapping_add(1).rotate_left(5) ^ n;
        if n == 0 {
            expected = expected.wrapping_add(1000);
        }
    }
    let interpreted = run(Box::new(dwrc_psx::Interpreter));
    assert_eq!((interpreted.cpu.regs[21], interpreted.cpu.regs[22]), (expected, 50), "interpreter");
    let mut inline = run(dwrc_recomp::new_engine_with(Mode::Native, Translation::Inline));
    assert_eq!((inline.cpu.regs[21], inline.cpu.regs[22]), (expected, 50), "recompiler");
    for t in [Translation::Background, Translation::Off] {
        let mut other = run(dwrc_recomp::new_engine_with(Mode::Native, t));
        same_console(&mut inline, &mut other, &format!("hooks, {t:?}"));
    }
}
