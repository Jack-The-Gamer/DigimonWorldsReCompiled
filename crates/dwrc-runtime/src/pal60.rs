//! 60 Hz for PAL releases that have a 60 Hz mode of their own.
//!
//! Digimon World 2003 (Europe, SLES-03936) plays at 50 frames a second, and
//! its game logic does not make up for it: it runs about 17% slower than
//! Digimon World 3 (USA). Its executable still contains a complete NTSC
//! (60 Hz) mode, behind two words of data that the released game never
//! changes:
//!
//! * `0x8005CCAC` (0 as shipped): when set, start-up calls
//!   `SetVideoMode(MODE_NTSC)`, the display and the sound setup take their
//!   NTSC values, and libsnd's sequencer is set up for 60 calls a second
//!   (`SsSetTickMode(SS_NOTICK)` instead of `SS_NOTICK | 50`);
//! * `0x8005CCB0` (1 as shipped): when set, the picture is moved 24 lines
//!   down to sit in the middle of a PAL screen. Left at 1 in the 60 Hz mode,
//!   it pushes the bottom of the picture off an NTSC screen.
//!
//! Setting the first and clearing the second when the executable starts
//! gives exactly the USA release's video timing and display range (lines
//! 16 to 256), music at the USA release's tempo and the intro movie in full.
//! It is the game's own code doing it, so both CPU engines run it alike.

use dwrc_disc::PsxExe;
use dwrc_psx::{HookAction, Psx};

/// A release whose own 60 Hz mode is switched on by writing words of data:
/// (address, value as shipped, value for 60 Hz).
struct SixtyHz {
    serial: &'static str,
    writes: &'static [(u32, u32, u32)],
}

const RELEASES: &[SixtyHz] = &[SixtyHz { serial: "SLES-03936", writes: &[(0x8005_CCAC, 0, 1), (0x8005_CCB0, 1, 0)] }];

/// Whether a release (by serial) can run at 60 Hz with this option.
pub fn supported(serial: &str) -> bool {
    RELEASES.iter().any(|r| r.serial == serial)
}

/// The word at `addr` in the executable's initialised data, if it has one.
fn exe_word(exe: &PsxExe, addr: u32) -> Option<u32> {
    let off = addr.wrapping_sub(exe.load_addr) as usize;
    exe.text.get(off..off + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Switches on the release's own 60 Hz mode when its executable starts.
/// Returns false (and changes nothing) for other releases, or when the
/// executable's data does not hold the expected values (another build).
pub fn enable(psx: &mut Psx, serial: &str, exe: &PsxExe) -> bool {
    let Some(r) = RELEASES.iter().find(|r| r.serial == serial) else { return false };
    if !r.writes.iter().all(|&(addr, shipped, _)| exe_word(exe, addr) == Some(shipped)) {
        log::warn!("{serial}: unexpected executable; staying at 50 Hz");
        return false;
    }
    let writes = r.writes;
    psx.bus.add_hook(
        exe.pc,
        Box::new(move |_cpu, bus| {
            for &(addr, _, value) in writes {
                bus.poke32(addr, value);
            }
            HookAction::Continue
        }),
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exe_with(words: &[(u32, u32)]) -> PsxExe {
        let mut text = vec![0u8; 0x5_0000];
        for &(addr, v) in words {
            let off = (addr - 0x8001_0000) as usize;
            text[off..off + 4].copy_from_slice(&v.to_le_bytes());
        }
        PsxExe {
            pc: 0x8001_0E90,
            gp: 0,
            load_addr: 0x8001_0000,
            text_size: text.len() as u32,
            bss_addr: 0,
            bss_size: 0,
            sp_base: 0x801F_FFF0,
            sp_offset: 0,
            region_marker: String::new(),
            text,
        }
    }

    #[test]
    fn switches_on_only_the_known_build() {
        assert!(supported("SLES-03936"));
        assert!(!supported("SLUS-01436"));
        let bios = vec![0u8; 512 * 1024];
        let good = exe_with(&[(0x8005_CCAC, 0), (0x8005_CCB0, 1)]);
        let mut psx = Psx::new(&bios, dwrc_psx::VideoStandard::Pal);
        assert!(enable(&mut psx, "SLES-03936", &good));
        assert!(psx.bus.hook_at(good.pc));
        // Another build (the flags hold other values) is left alone.
        let other = exe_with(&[(0x8005_CCAC, 7), (0x8005_CCB0, 1)]);
        let mut psx = Psx::new(&bios, dwrc_psx::VideoStandard::Pal);
        assert!(!enable(&mut psx, "SLES-03936", &other));
        assert!(!psx.bus.hook_at(other.pc));
        assert!(!enable(&mut psx, "SLUS-01436", &good));
    }
}
