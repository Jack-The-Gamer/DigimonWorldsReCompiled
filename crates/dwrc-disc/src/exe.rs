//! PS-X EXE executable header.

use crate::{DiscError, Result};

#[derive(Debug, Clone)]
pub struct PsxExe {
    pub pc: u32,
    pub gp: u32,
    pub load_addr: u32,
    pub text_size: u32,
    pub bss_addr: u32,
    pub bss_size: u32,
    pub sp_base: u32,
    pub sp_offset: u32,
    pub region_marker: String,
    /// The text section (code + data) that is copied to `load_addr`.
    pub text: Vec<u8>,
}

fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

impl PsxExe {
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 0x800 || &data[0..8] != b"PS-X EXE" {
            return Err(DiscError::Exe("missing 'PS-X EXE' signature".into()));
        }
        let text_size = le32(data, 0x1C);
        let end = (0x800 + text_size as usize).min(data.len());
        let marker_end = data[0x4C..0x800].iter().position(|&b| b == 0).unwrap_or(0x7B4);
        Ok(Self {
            pc: le32(data, 0x10),
            gp: le32(data, 0x14),
            load_addr: le32(data, 0x18),
            text_size,
            bss_addr: le32(data, 0x28),
            bss_size: le32(data, 0x2C),
            sp_base: le32(data, 0x30),
            sp_offset: le32(data, 0x34),
            region_marker: String::from_utf8_lossy(&data[0x4C..0x4C + marker_end]).to_string(),
            text: data[0x800..end].to_vec(),
        })
    }

    pub fn end_addr(&self) -> u32 {
        self.load_addr.wrapping_add(self.text.len() as u32)
    }
}
