//! Minute/Second/Frame addressing as used by the CD-ROM controller.

/// Number of sectors in the 2 second lead-in pregap that precedes LBA 0.
pub const PREGAP_SECTORS: u32 = 150;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Msf {
    pub m: u8,
    pub s: u8,
    pub f: u8,
}

impl Msf {
    pub const fn new(m: u8, s: u8, f: u8) -> Self {
        Self { m, s, f }
    }

    /// Absolute sector index counted from MSF 00:00:00.
    pub fn to_index(self) -> u32 {
        (self.m as u32 * 60 + self.s as u32) * 75 + self.f as u32
    }

    pub fn from_index(i: u32) -> Self {
        let f = (i % 75) as u8;
        let s = ((i / 75) % 60) as u8;
        let m = (i / 75 / 60) as u8;
        Self { m, s, f }
    }

    /// Logical block address (MSF 00:02:00 == LBA 0).
    pub fn to_lba(self) -> i64 {
        self.to_index() as i64 - PREGAP_SECTORS as i64
    }

    pub fn from_lba(lba: u32) -> Self {
        Self::from_index(lba + PREGAP_SECTORS)
    }

    pub fn from_bcd(m: u8, s: u8, f: u8) -> Self {
        Self { m: bcd_to_bin(m), s: bcd_to_bin(s), f: bcd_to_bin(f) }
    }

    pub fn to_bcd(self) -> [u8; 3] {
        [bin_to_bcd(self.m), bin_to_bcd(self.s), bin_to_bcd(self.f)]
    }

    pub fn parse(text: &str) -> Option<Self> {
        let mut it = text.trim().split(':');
        let m = it.next()?.parse().ok()?;
        let s = it.next()?.parse().ok()?;
        let f = it.next()?.parse().ok()?;
        if it.next().is_some() || s >= 60 || f >= 75 {
            return None;
        }
        Some(Self { m, s, f })
    }
}

impl std::fmt::Display for Msf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:02}:{:02}:{:02}", self.m, self.s, self.f)
    }
}

pub fn bcd_to_bin(v: u8) -> u8 {
    (v >> 4) * 10 + (v & 0x0F)
}

pub fn bin_to_bcd(v: u8) -> u8 {
    ((v / 10) << 4) | (v % 10)
}

pub fn is_valid_bcd(v: u8) -> bool {
    (v & 0x0F) <= 9 && (v >> 4) <= 9
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip() {
        let m = Msf::from_lba(0);
        assert_eq!(m, Msf::new(0, 2, 0));
        assert_eq!(m.to_lba(), 0);
        assert_eq!(Msf::from_lba(16).to_bcd(), [0x00, 0x02, 0x16]);
        assert_eq!(Msf::from_bcd(0x12, 0x34, 0x56), Msf::new(12, 34, 56));
    }
}
