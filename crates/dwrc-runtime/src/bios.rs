//! BIOS image identification.

use dwrc_disc::ident::Region;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};

pub const BIOS_SIZE: usize = 512 * 1024;

/// The built-in BIOS, used when the player has not added their own:
/// OpenBIOS by the PCSX-Redux authors (MIT licence, no Sony code), built
/// without its menu shell so it boots the disc directly. See
/// `assets/openbios/README.md`.
pub static BUILTIN_BIOS: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/openbios/openbios.bin"));

/// How the built-in BIOS is named in the interface.
pub const BUILTIN_NAME: &str = "built-in OpenBIOS";

/// BIOS dumps verified with Re:Compiled.
const KNOWN: &[(&str, &str, Region)] = &[
    ("0555c6fae8906f3f09baf5988f00e55f88e9f30b", "SCPH-5501 v3.0 (USA)", Region::NtscU),
    ("f6bc2d1f5eb6593de7d089c425ac681d6fffd3f0", "SCPH-5502/5552 v3.0 (Europe)", Region::Pal),
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BiosInfo {
    pub sha1: String,
    pub region: Region,
    pub version: String,
    pub known_name: Option<String>,
}

/// Identifies a BIOS image. Unknown but well-formed images are accepted
/// (region taken from the version string) and reported as unverified.
pub fn identify_bios(data: &[u8]) -> Option<BiosInfo> {
    if data.len() != BIOS_SIZE {
        return None;
    }
    let sha1 = hex::encode(Sha1::digest(data));
    if let Some((_, name, region)) = KNOWN.iter().find(|(h, _, _)| *h == sha1) {
        let version = version_string(data).unwrap_or_default();
        return Some(BiosInfo { sha1, region: *region, version, known_name: Some(name.to_string()) });
    }
    // Must at least look like a PlayStation BIOS.
    let text = String::from_utf8_lossy(data);
    if !text.contains("Sony Computer Entertainment") {
        return None;
    }
    let version = version_string(data)?;
    let region = match version.trim().chars().last() {
        Some('A') => Region::NtscU,
        Some('E') => Region::Pal,
        Some('J') | Some('T') => Region::NtscJ,
        _ => return None,
    };
    Some(BiosInfo { sha1, region, version, known_name: None })
}

fn version_string(data: &[u8]) -> Option<String> {
    let needle = b"System ROM Version";
    let pos = data.windows(needle.len()).position(|w| w == needle)?;
    let end = data[pos..].iter().position(|&b| b == 0 || b == b'\n' || b == b'\r').map(|e| pos + e)?;
    Some(String::from_utf8_lossy(&data[pos..end]).trim().to_string())
}

pub fn region_label(r: Region) -> &'static str {
    match r {
        Region::NtscU => "NTSC-U (America)",
        Region::Pal => "PAL (Europe)",
        Region::NtscJ => "NTSC-J (Japan)",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_bios_is_present() {
        assert_eq!(BUILTIN_BIOS.len(), BIOS_SIZE);
        assert!(String::from_utf8_lossy(BUILTIN_BIOS).contains("OpenBios"));
        // It is not mistaken for a Sony BIOS.
        assert!(identify_bios(BUILTIN_BIOS).is_none());
    }
}
