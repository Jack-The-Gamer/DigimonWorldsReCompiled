//! Minimal but robust CUE sheet parser (the subset PlayStation dumps use).

use crate::msf::Msf;
use crate::{DiscError, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CueTrackMode {
    Mode1_2352,
    Mode2_2352,
    Audio,
}

#[derive(Debug, Clone)]
pub struct CueTrack {
    pub number: u8,
    pub mode: CueTrackMode,
    /// Index of the FILE this track lives in.
    pub file: usize,
    /// INDEX 00 position inside its file, if present.
    pub index0: Option<Msf>,
    /// INDEX 01 position inside its file.
    pub index1: Msf,
    /// PREGAP that is *not* stored in the file.
    pub pregap: Option<Msf>,
}

#[derive(Debug, Clone)]
pub struct CueSheet {
    pub files: Vec<PathBuf>,
    pub tracks: Vec<CueTrack>,
}

fn unquote(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    if let Some(rest) = s.strip_prefix('"') {
        if let Some(end) = rest.find('"') {
            return (&rest[..end], &rest[end + 1..]);
        }
    }
    match s.find(char::is_whitespace) {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, ""),
    }
}

impl CueSheet {
    pub fn parse(text: &str, base_dir: &Path) -> Result<Self> {
        let mut files = Vec::new();
        let mut tracks: Vec<CueTrack> = Vec::new();
        for (line_no, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() {
                continue;
            }
            let (kw, rest) = match line.find(char::is_whitespace) {
                Some(i) => (&line[..i], line[i..].trim()),
                None => (line, ""),
            };
            let err = |m: &str| DiscError::Cue(format!("line {}: {}", line_no + 1, m));
            match kw.to_ascii_uppercase().as_str() {
                "FILE" => {
                    let (name, _ty) = unquote(rest);
                    let p = Path::new(name);
                    let p = if p.is_absolute() { p.to_path_buf() } else { base_dir.join(p) };
                    files.push(p);
                }
                "TRACK" => {
                    if files.is_empty() {
                        return Err(err("TRACK before FILE"));
                    }
                    let mut it = rest.split_whitespace();
                    let number: u8 = it.next().and_then(|n| n.parse().ok()).ok_or_else(|| err("bad track number"))?;
                    let mode = match it.next().map(|m| m.to_ascii_uppercase()) {
                        Some(m) if m == "MODE2/2352" => CueTrackMode::Mode2_2352,
                        Some(m) if m == "MODE1/2352" => CueTrackMode::Mode1_2352,
                        Some(m) if m == "AUDIO" => CueTrackMode::Audio,
                        Some(m) => return Err(err(&format!("unsupported track mode {m} (only raw 2352-byte images are supported)"))),
                        None => return Err(err("missing track mode")),
                    };
                    tracks.push(CueTrack { number, mode, file: files.len() - 1, index0: None, index1: Msf::default(), pregap: None });
                }
                "INDEX" => {
                    let t = tracks.last_mut().ok_or_else(|| err("INDEX before TRACK"))?;
                    let mut it = rest.split_whitespace();
                    let idx: u8 = it.next().and_then(|n| n.parse().ok()).ok_or_else(|| err("bad index"))?;
                    let msf = it.next().and_then(Msf::parse).ok_or_else(|| err("bad index time"))?;
                    match idx {
                        0 => t.index0 = Some(msf),
                        1 => t.index1 = msf,
                        _ => {}
                    }
                }
                "PREGAP" => {
                    let t = tracks.last_mut().ok_or_else(|| err("PREGAP before TRACK"))?;
                    t.pregap = Some(Msf::parse(rest).ok_or_else(|| err("bad pregap"))?);
                }
                _ => {} // REM, CATALOG, FLAGS, POSTGAP, TITLE, ...
            }
        }
        if tracks.is_empty() {
            return Err(DiscError::Cue("no tracks".into()));
        }
        Ok(Self { files, tracks })
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let dir = path.parent().unwrap_or(Path::new("."));
        Self::parse(&text, dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_single_track() {
        let cue = "FILE \"Digimon World (USA).bin\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n";
        let s = CueSheet::parse(cue, Path::new("/x")).unwrap();
        assert_eq!(s.files[0], Path::new("/x/Digimon World (USA).bin"));
        assert_eq!(s.tracks.len(), 1);
        assert_eq!(s.tracks[0].mode, CueTrackMode::Mode2_2352);
    }
}
