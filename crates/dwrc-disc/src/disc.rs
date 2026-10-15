//! Raw sector access over a BIN/CUE image, with an optional sector overlay
//! used by the mod system to replace or append data without touching the
//! player's original image.

use crate::cue::{CueSheet, CueTrackMode};
use crate::msf::{bin_to_bcd, Msf, PREGAP_SECTORS};
use crate::{DiscError, Result};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const RAW_SECTOR_SIZE: usize = 2352;
pub const SYNC_PATTERN: [u8; 12] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    Mode1,
    Mode2,
    Audio,
}

impl TrackKind {
    pub fn is_data(self) -> bool {
        !matches!(self, TrackKind::Audio)
    }
}

#[derive(Debug, Clone)]
pub struct Track {
    pub number: u8,
    pub kind: TrackKind,
    /// Absolute sector index (from MSF 00:00:00) of INDEX 01.
    pub start: u32,
    /// Absolute sector index of INDEX 00 (pregap start).
    pub pregap_start: u32,
    /// Number of sectors from INDEX 01 to the end of the track.
    pub length: u32,
    file: usize,
    /// Sector offset inside `file` that corresponds to `pregap_start`, or
    /// `None` when the pregap is not stored in the file.
    file_sector_at_start: u64,
}

impl Track {
    pub fn start_msf(&self) -> Msf {
        Msf::from_index(self.start)
    }
    pub fn end(&self) -> u32 {
        self.start + self.length
    }
}

/// A source of raw sectors. Implemented by [`Disc`]; the mod layer wraps it.
pub trait SectorSource: Send {
    /// Reads the raw 2352-byte sector at absolute index `index`
    /// (MSF 00:00:00 based). Positions outside any track yield a synthesized
    /// empty sector.
    fn read_raw(&mut self, index: u32, out: &mut [u8; RAW_SECTOR_SIZE]) -> Result<()>;
    fn tracks(&self) -> &[Track];
    /// Absolute index of the lead-out.
    fn lead_out(&self) -> u32;
}

struct OpenFile {
    path: PathBuf,
    file: File,
    sectors: u64,
    // Small read-ahead cache: CD reads are strongly sequential.
    cache_first: u64,
    cache_count: u64,
    cache: Vec<u8>,
}

const READ_AHEAD: u64 = 32;

impl OpenFile {
    fn read_sector(&mut self, sector: u64, out: &mut [u8; RAW_SECTOR_SIZE]) -> Result<()> {
        if sector >= self.sectors {
            out.fill(0);
            return Ok(());
        }
        if !(sector >= self.cache_first && sector < self.cache_first + self.cache_count) {
            let count = READ_AHEAD.min(self.sectors - sector);
            self.cache.resize(count as usize * RAW_SECTOR_SIZE, 0);
            self.file.seek(SeekFrom::Start(sector * RAW_SECTOR_SIZE as u64))?;
            self.file.read_exact(&mut self.cache)?;
            self.cache_first = sector;
            self.cache_count = count;
        }
        let off = ((sector - self.cache_first) as usize) * RAW_SECTOR_SIZE;
        out.copy_from_slice(&self.cache[off..off + RAW_SECTOR_SIZE]);
        Ok(())
    }
}

/// A disc image opened from a CUE sheet (or a bare single-track BIN).
pub struct Disc {
    files: Vec<OpenFile>,
    tracks: Vec<Track>,
    lead_out: u32,
    /// Sector replacements keyed by absolute index (mods, patched data).
    overlay: HashMap<u32, Arc<[u8; RAW_SECTOR_SIZE]>>,
    /// Sectors appended after the last track (relocated mod files).
    extra_end: u32,
}

impl Disc {
    /// Opens a disc from a `.cue` path, or a single-track `.bin` path.
    pub fn open(path: &Path) -> Result<Self> {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        if ext == "cue" {
            Self::from_cue(&CueSheet::load(path)?)
        } else {
            // Bare BIN: assume a single MODE2/2352 data track.
            let sheet = CueSheet::parse(
                &format!("FILE \"{}\" BINARY\nTRACK 01 MODE2/2352\nINDEX 01 00:00:00\n", path.display()),
                Path::new(""),
            )?;
            Self::from_cue(&sheet)
        }
    }

    pub fn from_cue(sheet: &CueSheet) -> Result<Self> {
        let mut files = Vec::new();
        for p in &sheet.files {
            let file = File::open(p).map_err(|e| {
                DiscError::Io(std::io::Error::new(e.kind(), format!("{}: {e}", p.display())))
            })?;
            let len = file.metadata()?.len();
            if len % RAW_SECTOR_SIZE as u64 != 0 {
                log::warn!("{} is not a multiple of 2352 bytes; trailing data ignored", p.display());
            }
            files.push(OpenFile {
                path: p.clone(),
                file,
                sectors: len / RAW_SECTOR_SIZE as u64,
                cache_first: 0,
                cache_count: 0,
                cache: Vec::new(),
            });
        }

        let mut tracks = Vec::new();
        // Disc position (absolute index) at which the current file's sector 0 sits.
        let mut file_base: i64 = 0;
        let mut cur_file: Option<usize> = None;
        let mut prev_file_end: i64 = PREGAP_SECTORS as i64;
        let mut extra_pregap_total: i64 = 0;
        for (ti, t) in sheet.tracks.iter().enumerate() {
            let kind = match t.mode {
                CueTrackMode::Mode1_2352 => TrackKind::Mode1,
                CueTrackMode::Mode2_2352 => TrackKind::Mode2,
                CueTrackMode::Audio => TrackKind::Audio,
            };
            let pregap_not_in_file = t.pregap.map(|m| m.to_index() as i64).unwrap_or(0);
            if cur_file != Some(t.file) {
                // New file.
                if ti == 0 {
                    // Track 1 INDEX 01 is always at 00:02:00.
                    file_base = PREGAP_SECTORS as i64 - t.index1.to_index() as i64;
                } else {
                    file_base = prev_file_end + pregap_not_in_file;
                }
                cur_file = Some(t.file);
            } else if pregap_not_in_file > 0 {
                extra_pregap_total += pregap_not_in_file;
            }
            let base = file_base + extra_pregap_total;
            let start = base + t.index1.to_index() as i64;
            let pregap_start = match t.index0 {
                Some(i0) => base + i0.to_index() as i64,
                None => start - pregap_not_in_file,
            };
            let file_sector_at_start = (pregap_start - base).max(0) as u64;
            tracks.push(Track {
                number: t.number,
                kind,
                start: start.max(0) as u32,
                pregap_start: pregap_start.max(0) as u32,
                length: 0,
                file: t.file,
                file_sector_at_start,
            });
            let fsz = files[t.file].sectors as i64;
            prev_file_end = base + fsz;
        }
        // Compute lengths.
        for i in 0..tracks.len() {
            let end = if i + 1 < tracks.len() && tracks[i + 1].file == tracks[i].file {
                tracks[i + 1].pregap_start as i64
            } else {
                let f = &files[tracks[i].file];
                let base = tracks[i].pregap_start as i64 - tracks[i].file_sector_at_start as i64;
                base + f.sectors as i64
            };
            tracks[i].length = (end - tracks[i].start as i64).max(0) as u32;
        }
        let lead_out = tracks.last().map(|t| t.end()).unwrap_or(PREGAP_SECTORS);
        Ok(Self { files, tracks, lead_out, overlay: HashMap::new(), extra_end: 0 })
    }

    pub fn file_paths(&self) -> Vec<PathBuf> {
        self.files.iter().map(|f| f.path.clone()).collect()
    }

    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.sectors * RAW_SECTOR_SIZE as u64).sum()
    }

    /// Replaces the sector at absolute index `index` for this session.
    pub fn overlay_sector(&mut self, index: u32, data: [u8; RAW_SECTOR_SIZE]) {
        self.overlay.insert(index, Arc::new(data));
        if index >= self.lead_out {
            self.extra_end = self.extra_end.max(index + 1 - self.lead_out);
        }
    }

    /// First free absolute index after the original data (and anything
    /// already appended), for relocating enlarged files.
    pub fn append_position(&self) -> u32 {
        self.lead_out + self.extra_end
    }

    pub fn has_overlay(&self) -> bool {
        !self.overlay.is_empty()
    }

    fn track_for(&self, index: u32) -> Option<&Track> {
        self.tracks.iter().find(|t| index >= t.pregap_start && index < t.end())
    }

    /// Reads the 2048-byte user data of a Mode1/Mode2-Form1 sector at `lba`.
    pub fn read_user_data(&mut self, lba: u32, out: &mut [u8]) -> Result<()> {
        let mut raw = [0u8; RAW_SECTOR_SIZE];
        self.read_raw(lba + PREGAP_SECTORS, &mut raw)?;
        let off = if raw[15] == 1 { 16 } else { 24 };
        out[..2048].copy_from_slice(&raw[off..off + 2048]);
        Ok(())
    }

    /// Reads `len` bytes of a file starting at `lba` (2048-byte data sectors).
    pub fn read_file(&mut self, lba: u32, len: u32) -> Result<Vec<u8>> {
        let mut out = vec![0u8; len as usize];
        let mut buf = [0u8; 2048];
        let mut done = 0usize;
        let mut sector = lba;
        while done < out.len() {
            self.read_user_data(sector, &mut buf)?;
            let n = (out.len() - done).min(2048);
            out[done..done + n].copy_from_slice(&buf[..n]);
            done += n;
            sector += 1;
        }
        Ok(out)
    }
}

/// Builds an empty Mode 2 sector with a valid header for `index`.
pub fn synth_sector(index: u32, out: &mut [u8; RAW_SECTOR_SIZE], data: bool) {
    out.fill(0);
    if data {
        out[..12].copy_from_slice(&SYNC_PATTERN);
        let msf = Msf::from_index(index);
        out[12] = bin_to_bcd(msf.m);
        out[13] = bin_to_bcd(msf.s);
        out[14] = bin_to_bcd(msf.f);
        out[15] = 2;
    }
}

impl SectorSource for Disc {
    fn read_raw(&mut self, index: u32, out: &mut [u8; RAW_SECTOR_SIZE]) -> Result<()> {
        if let Some(s) = self.overlay.get(&index) {
            out.copy_from_slice(&s[..]);
            return Ok(());
        }
        let Some(t) = self.track_for(index).cloned() else {
            synth_sector(index, out, true);
            return Ok(());
        };
        let rel = (index - t.pregap_start) as u64;
        let file_sector = t.file_sector_at_start + rel;
        // Pregap sectors that are not stored in the file come back empty.
        let in_file_start = t.pregap_start as i64 - t.file_sector_at_start as i64;
        if (index as i64) < in_file_start {
            synth_sector(index, out, t.kind.is_data());
            return Ok(());
        }
        self.files[t.file].read_sector(file_sector, out)
    }

    fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    fn lead_out(&self) -> u32 {
        self.lead_out + self.extra_end
    }
}
