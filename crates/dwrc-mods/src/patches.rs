//! Memory patches declared in `mod.toml`.

use crate::manifest::PatchDef;
use anyhow::{anyhow, bail, Result};
use dwrc_psx::Bus;

#[derive(Debug, Clone)]
pub struct MemoryPatch {
    pub address: u32,
    pub bytes: Vec<u8>,
    pub once: bool,
    pub condition: Option<(u32, Vec<u8>)>,
    applied: bool,
}

pub fn parse_hex(s: &str) -> Result<Vec<u8>> {
    let clean: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    let clean = clean.trim_start_matches("0x");
    if clean.len() % 2 != 0 {
        bail!("odd number of hex digits in '{s}'");
    }
    hex::decode(clean).map_err(|e| anyhow!("bad hex '{s}': {e}"))
}

impl MemoryPatch {
    pub fn from_def(d: &PatchDef) -> Result<Self> {
        let bytes = if let Some(b) = &d.bytes {
            parse_hex(b)?
        } else if let Some(v) = d.u32 {
            v.to_le_bytes().to_vec()
        } else if let Some(v) = d.u16 {
            v.to_le_bytes().to_vec()
        } else if let Some(v) = d.u8 {
            vec![v]
        } else {
            bail!("patch at {:08x} has no bytes/u8/u16/u32", d.address);
        };
        let condition = match (d.if_address, &d.if_bytes) {
            (Some(a), Some(b)) => Some((a, parse_hex(b)?)),
            (None, None) => None,
            _ => bail!("if_address and if_bytes must be used together"),
        };
        Ok(Self { address: d.address, bytes, once: d.when == "once", condition, applied: false })
    }

    pub fn apply(&mut self, bus: &mut Bus) {
        if self.once && self.applied {
            return;
        }
        if let Some((a, want)) = &self.condition {
            for (i, b) in want.iter().enumerate() {
                if bus.peek8(a.wrapping_add(i as u32)) != *b {
                    return;
                }
            }
        }
        for (i, b) in self.bytes.iter().enumerate() {
            let a = self.address.wrapping_add(i as u32);
            if bus.peek8(a) != *b {
                bus.poke8(a, *b);
            }
        }
        self.applied = true;
    }
}
