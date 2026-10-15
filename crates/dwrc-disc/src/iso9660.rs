//! Read-only ISO9660 filesystem access (as written on PlayStation discs).

use crate::disc::Disc;
use crate::{DiscError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    /// Name without the `;1` version suffix.
    pub name: String,
    pub lba: u32,
    pub size: u32,
    pub is_dir: bool,
    /// Absolute byte position of this record within the directory extent,
    /// counted from the start of the parent directory's first sector. Used
    /// by the mod layer to rewrite records.
    pub record_lba: u32,
    pub record_offset: u32,
}

impl DirEntry {
    pub fn sectors(&self) -> u32 {
        self.size.div_ceil(2048)
    }
}

pub struct IsoFs {
    pub volume_id: String,
    pub system_id: String,
    pub root: DirEntry,
    pub volume_sectors: u32,
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

impl IsoFs {
    pub fn mount(disc: &mut Disc) -> Result<Self> {
        let mut pvd = [0u8; 2048];
        disc.read_user_data(16, &mut pvd)?;
        if pvd[0] != 1 || &pvd[1..6] != b"CD001" {
            return Err(DiscError::Iso("primary volume descriptor not found at LBA 16".into()));
        }
        let system_id = String::from_utf8_lossy(&pvd[8..40]).trim().to_string();
        let volume_id = String::from_utf8_lossy(&pvd[40..72]).trim().to_string();
        let volume_sectors = le32(&pvd[80..84]);
        let rr = &pvd[156..156 + 34];
        let root = DirEntry {
            name: String::new(),
            lba: le32(&rr[2..6]),
            size: le32(&rr[10..14]),
            is_dir: true,
            record_lba: 16,
            record_offset: 156,
        };
        Ok(Self { volume_id, system_id, root, volume_sectors })
    }

    pub fn read_dir(&self, disc: &mut Disc, dir: &DirEntry) -> Result<Vec<DirEntry>> {
        let mut out = Vec::new();
        let mut sector = [0u8; 2048];
        for s in 0..dir.sectors() {
            disc.read_user_data(dir.lba + s, &mut sector)?;
            let mut off = 0usize;
            while off < 2048 {
                let len = sector[off] as usize;
                if len == 0 {
                    break; // rest of this sector is padding
                }
                if off + len > 2048 || len < 34 {
                    return Err(DiscError::Iso(format!("corrupt directory record at LBA {} +{off}", dir.lba + s)));
                }
                let r = &sector[off..off + len];
                let name_len = r[32] as usize;
                let raw_name = &r[33..33 + name_len];
                let flags = r[25];
                if !(name_len == 1 && (raw_name[0] == 0 || raw_name[0] == 1)) {
                    let mut name = String::from_utf8_lossy(raw_name).to_string();
                    if let Some(i) = name.find(';') {
                        name.truncate(i);
                    }
                    if name.ends_with('.') {
                        name.pop();
                    }
                    out.push(DirEntry {
                        name,
                        lba: le32(&r[2..6]),
                        size: le32(&r[10..14]),
                        is_dir: flags & 0x02 != 0,
                        record_lba: dir.lba + s,
                        record_offset: off as u32,
                    });
                }
                off += len;
            }
        }
        Ok(out)
    }

    /// Looks up a path such as `\SLUS_010.32` or `CHDAT/MMD0/FOO.BIN`
    /// (case-insensitive, either separator, `;1` optional).
    pub fn find(&self, disc: &mut Disc, path: &str) -> Result<Option<DirEntry>> {
        let mut cur = self.root.clone();
        let comps: Vec<&str> = path.split(['/', '\\']).filter(|c| !c.is_empty()).collect();
        for (i, comp) in comps.iter().enumerate() {
            let want = comp.split(';').next().unwrap_or(comp).trim_end_matches('.');
            let entries = self.read_dir(disc, &cur)?;
            match entries.into_iter().find(|e| e.name.eq_ignore_ascii_case(want)) {
                Some(e) => {
                    if i + 1 < comps.len() && !e.is_dir {
                        return Ok(None);
                    }
                    cur = e;
                }
                None => return Ok(None),
            }
        }
        Ok(Some(cur))
    }

    pub fn read(&self, disc: &mut Disc, entry: &DirEntry) -> Result<Vec<u8>> {
        disc.read_file(entry.lba, entry.size)
    }

    /// Recursively lists every file with its full path.
    pub fn walk(&self, disc: &mut Disc) -> Result<Vec<(String, DirEntry)>> {
        let mut out = Vec::new();
        let mut stack = vec![(String::new(), self.root.clone())];
        while let Some((prefix, dir)) = stack.pop() {
            for e in self.read_dir(disc, &dir)? {
                let path = if prefix.is_empty() { e.name.clone() } else { format!("{prefix}/{}", e.name) };
                if e.is_dir {
                    stack.push((path.clone(), e.clone()));
                }
                out.push((path, e));
            }
        }
        out.sort_by_key(|(_, e)| e.lba);
        Ok(out)
    }
}

/// Parses SYSTEM.CNF into (BOOT path, other key/values).
pub fn parse_system_cnf(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let (k, v) = l.split_once('=')?;
            Some((k.trim().to_ascii_uppercase(), v.trim().to_string()))
        })
        .collect()
}

/// Extracts the executable path from a BOOT line, e.g.
/// `cdrom:\SLUS_010.32;1` -> `SLUS_010.32`.
pub fn boot_path(boot: &str) -> String {
    let s = boot.trim();
    let s = s.strip_prefix("cdrom:").or_else(|| s.strip_prefix("CDROM:")).unwrap_or(s);
    let s = s.trim_start_matches(['\\', '/']);
    let s = s.split(';').next().unwrap_or(s);
    s.split_whitespace().next().unwrap_or("").to_string()
}
