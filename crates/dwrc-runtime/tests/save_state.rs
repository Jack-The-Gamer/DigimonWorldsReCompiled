//! Save states restore the console exactly: after loading one, the next
//! frames are bit-identical to the frames that followed the save (RAM, VRAM,
//! sound RAM, CPU state, audio), on both CPU engines and into a fresh
//! console. Runs on the built-in BIOS, without game data.

use dwrc_psx::{Psx, VideoStandard};
use dwrc_runtime::bios::BUILTIN_BIOS;

fn console(recompiled: bool) -> Psx {
    let mut psx = Psx::new(BUILTIN_BIOS, VideoStandard::Ntsc);
    if recompiled {
        psx.set_engine(dwrc_recomp::new_engine(dwrc_recomp::Mode::Native));
    }
    psx
}

/// FNV-1a over everything the next frames depend on and produce.
fn digest(psx: &Psx) -> u64 {
    let mut h = 0xCBF2_9CE4_8422_2325u64;
    let mut eat = |b: &[u8]| {
        for x in b {
            h = (h ^ *x as u64).wrapping_mul(0x0000_0100_0000_01B3);
        }
    };
    eat(&psx.bus.ram);
    eat(&psx.bus.scratch);
    eat(&psx.bus.spu.ram);
    for p in psx.bus.gpu.vram.iter() {
        eat(&p.to_le_bytes());
    }
    for r in psx.cpu.regs.iter().chain([psx.cpu.pc, psx.cpu.hi, psx.cpu.lo].iter()) {
        eat(&r.to_le_bytes());
    }
    eat(&psx.bus.cycles.to_le_bytes());
    h
}

/// Runs `n` frames, returning the audio they produced and the final digest.
fn play(psx: &mut Psx, n: usize) -> (Vec<i16>, u64) {
    let mut audio = Vec::new();
    let mut buf = Vec::new();
    for _ in 0..n {
        psx.run_frame();
        psx.bus.spu.take_output(&mut buf);
        audio.extend_from_slice(&buf);
        buf.clear();
    }
    (audio, digest(psx))
}

fn round_trip(recompiled: bool) {
    let mut psx = console(recompiled);
    play(&mut psx, 120);
    let state = psx.save_state();
    let (audio, after) = play(&mut psx, 90);
    assert!(!audio.is_empty());

    // Back to the saved moment in the same console.
    psx.load_state(&state).unwrap();
    assert_eq!(play(&mut psx, 90), (audio.clone(), after), "same console, recompiled = {recompiled}");

    // And in a console that never ran.
    let mut fresh = console(recompiled);
    fresh.load_state(&state).unwrap();
    assert_eq!(play(&mut fresh, 90), (audio, after), "fresh console, recompiled = {recompiled}");
}

#[test]
fn round_trip_interpreter() {
    round_trip(false);
}

#[test]
fn round_trip_recompiler() {
    round_trip(true);
}

#[test]
fn damaged_states_change_nothing() {
    let mut psx = console(false);
    play(&mut psx, 30);
    let state = psx.save_state();
    play(&mut psx, 10);
    let before = digest(&psx);
    for bad in [&state[..state.len() / 2], &state[..12], b"not a state".as_slice()] {
        assert!(psx.load_state(bad).is_err());
        assert_eq!(digest(&psx), before);
    }
    let mut longer = state.clone();
    longer.push(0);
    assert!(psx.load_state(&longer).is_err());
    assert_eq!(digest(&psx), before);
}
