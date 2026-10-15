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

/// A file whose location or size changed.
#[derive(Debug, Clone, Copy)]
pub struct Relocation {
    pub old_lba: u32,
    pub old_size: u32,
    pub new_lba: u32,
    pub new_size: u32,
}

pub struct DiscContext<'a> {
    pub disc: &'a mut Disc,
    fs: Option<IsoFs>,
    relocations: Vec<Relocation>,
}

impl<'a> DiscContext<'a> {
    pub fn new(disc: &'a mut Disc) -> Self {
        let fs = IsoFs::mount(disc).ok();
        Self { disc, fs, relocations: Vec::new() }
    }

    /// Patches file tables inside the boot executable: many games (Digimon
    /// World among them) keep their own `(LBA, size)` table instead of
    /// reading the ISO directory. Every adjacent `(old LBA, old size)` pair
    /// is rewritten. Returns the number of entries patched.
    pub fn finish(&mut self) -> Result<usize> {
        if self.relocations.is_empty() {
            return Ok(0);
        }
        let fs = match &self.fs {
            Some(f) => f,
            None => return Ok(0),
        };
        let Some(cnf) = fs.find(self.disc, "SYSTEM.CNF").map_err(|e| anyhow!("{e}"))? else { return Ok(0) };
        let text = String::from_utf8_lossy(&fs.read(self.disc, &cnf).map_err(|e| anyhow!("{e}"))?).to_string();
        let boot = dwrc_disc::iso9660::parse_system_cnf(&text)
            .into_iter()
            .find(|(k, _)| k == "BOOT")
            .map(|(_, v)| dwrc_disc::iso9660::boot_path(&v));
        let Some(boot) = boot else { return Ok(0) };
        let Some(exe) = fs.find(self.disc, &boot).map_err(|e| anyhow!("{e}"))? else { return Ok(0) };
        let mut data = fs.read(self.disc, &exe).map_err(|e| anyhow!("{e}"))?;
        let mut patched = 0;
        for r in &self.relocations {
            let mut i = 0x800; // skip the PS-X EXE header
            while i + 8 <= data.len() {
                let a = u32::from_le_bytes(data[i..i + 4].try_into().unwrap());
                let b = u32::from_le_bytes(data[i + 4..i + 8].try_into().unwrap());
                if a == r.old_lba && b == r.old_size {
                    data[i..i + 4].copy_from_slice(&r.new_lba.to_le_bytes());
                    data[i + 4..i + 8].copy_from_slice(&r.new_size.to_le_bytes());
                    patched += 1;
                }
                i += 4;
            }
        }
        if patched > 0 {
            self.write_data(exe.lba, &data, true)?;
            log::info!("mod: patched {patched} file-table entr(y/ies) in {boot}");
        }
        Ok(patched)
    }

    /// Applies a mod's file and LBA replacements. Returns how many were applied.
    pub fn apply_mod(&mut self, m: &ModInfo) -> Result<usize> {
        let mut n = 0;
        let files_dir = m.root.join(&m.manifest.info.files_dir);
        if files_dir.is_dir() {
            let mut list = Vec::new();
            collect(&files_dir, &files_dir, &mut list);
            for (rel, path) in list {
                let name = rel.rsplit('/').next().unwrap_or(&rel).to_ascii_lowercase();
                if name.starts_with("readme") {
                    continue;
                }
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
        if lba != entry.lba || size != entry.size {
            self.relocations.push(Relocation { old_lba: entry.lba, old_size: entry.size, new_lba: lba, new_size: size });
        }
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
