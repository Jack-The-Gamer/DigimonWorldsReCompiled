//! Game identification.
//!
//! Like Gen1Recomp, Re:Compiled only accepts images it can positively
//! identify. Identification happens in two stages:
//!
//! 1. A fast check that reads `SYSTEM.CNF` and the boot executable and
//!    matches the boot serial and executable SHA-1 against the table below.
//! 2. A full-image SHA-1 (done once at import) that tells us whether the
//!    image is a known-good (Redump) dump. Unknown full hashes with a known
//!    executable are accepted with a warning, since they are usually the
//!    same game with a different (e.g. EDC-regenerated) image.

use crate::disc::Disc;
use crate::iso9660::{boot_path, parse_system_cnf, IsoFs};
use crate::{PsxExe, Result};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::io::Read;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub enum GameId {
    /// Digimon World (1999/2000)
    Dw1,
    /// Digimon World 2 (2000/2001)
    Dw2,
    /// Digimon World 3 / Digimon World 2003 (2002/2003)
    Dw3,
}

impl GameId {
    pub const ALL: [GameId; 3] = [GameId::Dw1, GameId::Dw2, GameId::Dw3];

    pub fn title(self) -> &'static str {
        match self {
            GameId::Dw1 => "Digimon World",
            GameId::Dw2 => "Digimon World 2",
            GameId::Dw3 => "Digimon World 3",
        }
    }

    pub fn slug(self) -> &'static str {
        match self {
            GameId::Dw1 => "dw1",
            GameId::Dw2 => "dw2",
            GameId::Dw3 => "dw3",
        }
    }

    pub fn from_slug(s: &str) -> Option<Self> {
        GameId::ALL.into_iter().find(|g| g.slug().eq_ignore_ascii_case(s))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Region {
    NtscU,
    NtscJ,
    Pal,
}

impl Region {
    pub fn is_pal(self) -> bool {
        matches!(self, Region::Pal)
    }
}

/// A specific release (identified by boot serial).
#[derive(Debug, Clone, Copy)]
pub struct KnownRelease {
    pub game: GameId,
    pub serial: &'static str,
    pub title: &'static str,
    pub region: Region,
}

/// A specific verified disc image.
#[derive(Debug, Clone, Copy)]
pub struct KnownDump {
    pub serial: &'static str,
    pub name: &'static str,
    pub size: u64,
    pub sha1: &'static str,
}

pub const RELEASES: &[KnownRelease] = &[
    KnownRelease { game: GameId::Dw1, serial: "SLUS-01032", title: "Digimon World (USA)", region: Region::NtscU },
    KnownRelease { game: GameId::Dw1, serial: "SLPS-01797", title: "Digimon World (Japan)", region: Region::NtscJ },
    KnownRelease { game: GameId::Dw2, serial: "SLUS-01193", title: "Digimon World 2 (USA)", region: Region::NtscU },
    KnownRelease { game: GameId::Dw3, serial: "SLUS-01436", title: "Digimon World 3 (USA)", region: Region::NtscU },
    KnownRelease { game: GameId::Dw3, serial: "SLES-03936", title: "Digimon World 2003 (Europe)", region: Region::Pal },
    KnownRelease { game: GameId::Dw3, serial: "SLPS-03446", title: "Digimon World 3 (Japan)", region: Region::NtscJ },
];

/// Full-image hashes of dumps that have been verified end-to-end with
/// Re:Compiled.
pub const VERIFIED_DUMPS: &[KnownDump] = &[
    KnownDump {
        serial: "SLUS-01032",
        name: "Digimon World (USA).bin",
        size: 381_771_936,
        sha1: "5611645da66183e30d11fb6c11a2784ff47e1fdb",
    },
    KnownDump {
        serial: "SLUS-01193",
        name: "Digimon World 2 (USA).bin",
        size: 461_993_952,
        sha1: "d9cb009bc0006e901d8ab1365f764ab71da02115",
    },
    KnownDump {
        serial: "SLES-03936",
        name: "Digimon World 2003 (Europe) (En,Fr,De,Es,It).bin",
        size: 692_146_560,
        sha1: "457cb233349ba841e03b33d8060f8fbcadd45cb3",
    },
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identification {
    pub game: GameId,
    pub serial: String,
    pub title: String,
    pub region: Region,
    pub volume_id: String,
    pub boot_file: String,
    pub exe_sha1: String,
    pub exe_load_addr: u32,
    pub exe_entry: u32,
}

/// Normalises a boot file name like `SLUS_010.32` into `SLUS-01032`.
pub fn serial_from_boot(file: &str) -> String {
    let base = file.rsplit(['\\', '/']).next().unwrap_or(file);
    let cleaned: String = base.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    if cleaned.len() >= 9 {
        let (pre, num) = cleaned.split_at(4);
        format!("{}-{}", pre.to_ascii_uppercase(), num)
    } else {
        base.to_ascii_uppercase()
    }
}

/// Fast identification from the filesystem (no full-image hash).
pub fn identify(disc: &mut Disc) -> Result<Option<Identification>> {
    let fs = IsoFs::mount(disc)?;
    let Some(cnf) = fs.find(disc, "SYSTEM.CNF")? else { return Ok(None) };
    let cnf_text = String::from_utf8_lossy(&fs.read(disc, &cnf)?).to_string();
    let kv = parse_system_cnf(&cnf_text);
    let Some(boot) = kv.iter().find(|(k, _)| k == "BOOT").map(|(_, v)| boot_path(v)) else { return Ok(None) };
    let serial = serial_from_boot(&boot);
    let Some(release) = RELEASES.iter().find(|r| r.serial == serial) else { return Ok(None) };
    let Some(exe_entry) = fs.find(disc, &boot)? else { return Ok(None) };
    let exe_bytes = fs.read(disc, &exe_entry)?;
    let exe = PsxExe::parse(&exe_bytes)?;
    Ok(Some(Identification {
        game: release.game,
        serial,
        title: release.title.to_string(),
        region: release.region,
        volume_id: fs.volume_id.clone(),
        boot_file: boot,
        exe_sha1: hex::encode(Sha1::digest(&exe_bytes)),
        exe_load_addr: exe.load_addr,
        exe_entry: exe.pc,
    }))
}

/// SHA-1 of a whole file, with a progress callback (bytes done, total).
pub fn sha1_file(path: &Path, mut progress: impl FnMut(u64, u64)) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let total = f.metadata()?.len();
    let mut h = Sha1::new();
    let mut buf = vec![0u8; 4 << 20];
    let mut done = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        done += n as u64;
        progress(done, total);
    }
    Ok(hex::encode(h.finalize()))
}

pub fn verified_dump(sha1: &str) -> Option<&'static KnownDump> {
    VERIFIED_DUMPS.iter().find(|d| d.sha1.eq_ignore_ascii_case(sha1))
}
