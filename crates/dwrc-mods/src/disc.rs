//! Virtual-disc file replacement.
//!
//! Replacements are written into the disc's in-memory sector overlay, so
//! the player's image is never touched. For ISO9660 files a larger
//! replacement is relocated past the end of the disc and its directory
//! record is rewritten, which is how games using `CdSearchFile` find it.

use crate::ModInfo;
use anyhow::{anyhow, Context, Result};
use dwrc_disc::disc::synth_sector;
use dwrc_disc::iso9660::IsoFs;
use dwrc_disc::msf::{bin_to_bcd, Msf, PREGAP_SECTORS};
use dwrc_disc::{Disc, SectorSource, RAW_SECTOR_SIZE};
use std::path::Path;

pub struct DiscContext<'a> {
    pub disc: &'a mut Disc,
    fs: Option<IsoFs>,
}

impl<'a> DiscContext<'a> {
    pub fn new(disc: &'a mut Disc) -> Self {
        let fs = IsoFs::mount(disc).ok();
        Self { disc, fs }
    }

    /// Applies a mod's file and LBA replacements. Returns how many were applied.
    pub fn apply_mod(&mut self, m: &ModInfo) -> Result<usize> {
        let mut n = 0;
        let files_dir = m.root.join(&m.manifest.info.files_dir);
        if files_dir.is_dir() {
            let mut list = Vec::new();
            collect(&files_dir, &files_dir, &mut list);
            for (rel, path) in list {
                let data = std::fs::read(&path)?;
                self.replace_file(&rel, &data).with_context(|| format!("replacing {rel}"))?;
                n += 1;
            }
        }
        for l in &m.manifest.lba {
            let data = std::fs::read(m.root.join(&l.file))?;
            let sectors = data.len().div_ceil(2048) as u32;
            if let Some(max) = l.max_sectors {
                if sectors > max {
                    return Err(anyhow!("{} needs {sectors} sectors but only {max} are available at LBA {}", l.file, l.lba));
                }
            }
            self.write_data(l.lba, &data, false)?;
            n += 1;
        }
        Ok(n)
    }

    /// Writes `data` as 2048-byte data sectors starting at `lba`, keeping
    /// the original sector headers where they exist.
    pub fn write_data(&mut self, lba: u32, data: &[u8], mark_eof: bool) -> Result<()> {
        let count = data.len().div_ceil(2048).max(1);
        for i in 0..count {
            let index = lba + i as u32 + PREGAP_SECTORS;
            let mut raw = [0u8; RAW_SECTOR_SIZE];
            if index < self.disc.lead_out() {
                self.disc.read_raw(index, &mut raw).map_err(|e| anyhow!("{e}"))?;
            } else {
                synth_sector(index, &mut raw, true);
            }
            if raw[15] != 2 && raw[15] != 1 {
                raw[15] = 2;
            }
            let last = i + 1 == count;
            if raw[15] == 2 {
                // Mode 2 Form 1 sub-header (written twice).
                let sub = if last && mark_eof { 0x89 } else { 0x08 };
                if index >= self.disc.lead_out() || mark_eof {
                    raw[16..20].copy_from_slice(&[0, 0, sub, 0]);
                    raw[20..24].copy_from_slice(&[0, 0, sub, 0]);
                }
            }
            let msf = Msf::from_index(index);
            raw[12] = bin_to_bcd(msf.m);
            raw[13] = bin_to_bcd(msf.s);
            raw[14] = bin_to_bcd(msf.f);
            let off = if raw[15] == 1 { 16 } else { 24 };
            let start = i * 2048;
            let end = (start + 2048).min(data.len());
            raw[off..off + 2048].fill(0);
            if start < end {
                raw[off..off + (end - start)].copy_from_slice(&data[start..end]);
            }
            self.disc.overlay_sector(index, raw);
        }
        Ok(())
    }

    /// Replaces an ISO9660 file by path.
    pub fn replace_file(&mut self, iso_path: &str, data: &[u8]) -> Result<()> {
        let fs = self.fs.as_ref().ok_or_else(|| anyhow!("disc has no ISO9660 filesystem; use [[lba]] replacements"))?;
        let entry = fs
            .find(self.disc, iso_path)
            .map_err(|e| anyhow!("{e}"))?
            .ok_or_else(|| anyhow!("'{iso_path}' does not exist on the disc"))?;
        let need = data.len().div_ceil(2048) as u32;
        let have = entry.sectors().max(1);
        let lba = if need <= have {
            entry.lba
        } else {
            // Relocate after the end of the disc.
            self.disc.append_position() - PREGAP_SECTORS
        };
        self.write_data(lba, data, true)?;
        // Patch the directory record (both-endian LBA and size fields).
        let rec_index = entry.record_lba + PREGAP_SECTORS;
        let mut raw = [0u8; RAW_SECTOR_SIZE];
        self.disc.read_raw(rec_index, &mut raw).map_err(|e| anyhow!("{e}"))?;
        let base = if raw[15] == 1 { 16 } else { 24 } + entry.record_offset as usize;
        raw[base + 2..base + 6].copy_from_slice(&lba.to_le_bytes());
        raw[base + 6..base + 10].copy_from_slice(&lba.to_be_bytes());
        let size = data.len() as u32;
        raw[base + 10..base + 14].copy_from_slice(&size.to_le_bytes());
        raw[base + 14..base + 18].copy_from_slice(&size.to_be_bytes());
        self.disc.overlay_sector(rec_index, raw);
        log::info!("mod: replaced {iso_path} ({} bytes at LBA {lba})", data.len());
        Ok(())
    }
}

fn collect(base: &Path, dir: &Path, out: &mut Vec<(String, std::path::PathBuf)>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            collect(base, &p, out);
        } else if let Ok(rel) = p.strip_prefix(base) {
            let rel = rel.to_string_lossy().replace('\\', "/");
            out.push((rel, p));
        }
    }
}
