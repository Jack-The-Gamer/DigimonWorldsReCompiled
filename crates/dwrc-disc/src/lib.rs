//! Disc access for Digimon Worlds Re:Compiled.
//!
//! Players supply their own disc images. This crate reads them (BIN/CUE),
//! exposes raw 2352-byte sectors exactly as the PlayStation CD-ROM drive
//! would see them, parses the ISO9660 filesystem and PS-X EXE headers, and
//! identifies which game (and which exact dump) an image is.

pub mod cue;
pub mod disc;
pub mod exe;
pub mod ident;
pub mod iso9660;
pub mod msf;

pub use disc::{Disc, SectorSource, Track, TrackKind, RAW_SECTOR_SIZE};
pub use exe::PsxExe;
pub use ident::{identify, GameId, Identification, KnownDump};
pub use iso9660::{DirEntry, IsoFs};
pub use msf::Msf;

#[derive(Debug, thiserror::Error)]
pub enum DiscError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("cue sheet error: {0}")]
    Cue(String),
    #[error("ISO9660 error: {0}")]
    Iso(String),
    #[error("not a PS-X EXE: {0}")]
    Exe(String),
    #[error("sector {0} is outside the disc")]
    OutOfRange(u32),
}

pub type Result<T> = std::result::Result<T, DiscError>;
