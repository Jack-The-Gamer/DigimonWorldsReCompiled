//! Save state files: a whole console, saved to a numbered slot per release
//! and loaded back later (`dwrc.save_state` / `dwrc.load_state` from mods).
//!
//! A file is `DWRCSTATE1\n`, a JSON header (program version, release, BIOS)
//! and the console state from [`Psx::save_state`], deflate-compressed. A
//! state only loads with the release and BIOS it was saved with: RAM holds
//! the BIOS kernel's data and the game's code, so anything else would crash.
//! Memory cards are not part of a state (loading never takes saves back).

use anyhow::{anyhow, bail, Context, Result};
use dwrc_mods::script::StateRequest;
use dwrc_mods::ModRuntime;
use dwrc_psx::Psx;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::io::{Read, Write};
use std::path::PathBuf;

const MAGIC: &[u8] = b"DWRCSTATE1\n";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Header {
    program: String,
    serial: String,
    title: String,
    bios_sha1: String,
    bios_name: String,
    saved_at: u64,
    frame: u64,
}

/// Where one release's save states live, and what they must match.
pub struct StateStore {
    dir: PathBuf,
    serial: String,
    title: String,
    bios_sha1: String,
    bios_name: String,
}

impl StateStore {
    /// `dir` is the folder for this release's states (`Paths::states_dir`).
    pub fn new(dir: PathBuf, serial: &str, title: &str, bios: &[u8], bios_name: &str) -> Self {
        Self { dir, serial: serial.into(), title: title.into(), bios_sha1: hex::encode(Sha1::digest(bios)), bios_name: bios_name.into() }
    }

    pub fn path(&self, slot: u32) -> PathBuf {
        self.dir.join(format!("slot{slot}.state"))
    }

    pub fn save(&self, psx: &mut Psx, slot: u32) -> Result<()> {
        let header = Header {
            program: env!("CARGO_PKG_VERSION").into(),
            serial: self.serial.clone(),
            title: self.title.clone(),
            bios_sha1: self.bios_sha1.clone(),
            bios_name: self.bios_name.clone(),
            saved_at: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
            frame: psx.frame_count,
        };
        let head = serde_json::to_vec(&header)?;
        let mut out = Vec::with_capacity(1 << 20);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&(head.len() as u32).to_le_bytes());
        out.extend_from_slice(&head);
        let mut z = flate2::write::ZlibEncoder::new(out, flate2::Compression::fast());
        z.write_all(&psx.save_state())?;
        let out = z.finish()?;
        std::fs::create_dir_all(&self.dir)?;
        let path = self.path(slot);
        let tmp = path.with_extension("state.tmp");
        std::fs::write(&tmp, &out).and_then(|_| std::fs::rename(&tmp, &path)).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    pub fn load(&self, psx: &mut Psx, slot: u32) -> Result<()> {
        let path = self.path(slot);
        let data = match std::fs::read(&path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!("slot {slot} is empty"),
            Err(e) => return Err(anyhow!("reading {}: {e}", path.display())),
        };
        let (header, body) = parse(&data).ok_or_else(|| anyhow!("slot {slot} is not a save state"))?;
        if header.serial != self.serial {
            bail!("slot {slot} is from {} ({}), not {}", header.title, header.serial, self.title);
        }
        if header.bios_sha1 != self.bios_sha1 {
            bail!("slot {slot} was saved with another BIOS ({}); this game is running on {}", header.bios_name, self.bios_name);
        }
        let mut state = Vec::with_capacity(4 << 20);
        flate2::read::ZlibDecoder::new(body).read_to_end(&mut state).map_err(|_| anyhow!("slot {slot} is damaged"))?;
        psx.load_state(&state).map_err(|e| anyhow!("slot {slot}: {e}"))
    }
}

fn parse(data: &[u8]) -> Option<(Header, &[u8])> {
    let rest = data.strip_prefix(MAGIC)?;
    let n = u32::from_le_bytes(rest.get(..4)?.try_into().ok()?) as usize;
    let header = serde_json::from_slice(rest.get(4..4 + n)?).ok()?;
    Some((header, &rest[4 + n..]))
}

/// Carries out the scripts' save state requests (between frames) and
/// returns messages for the screen. `loaded` is set when a state was loaded.
pub fn handle_requests(store: &StateStore, psx: &mut Psx, mods: &mut ModRuntime, loaded: &mut bool) -> Vec<String> {
    let mut notices = Vec::new();
    for (id, req) in mods.take_state_requests() {
        let r = match req {
            StateRequest::Save(slot) => store.save(psx, slot).map(|_| format!("Saved state {slot}")),
            StateRequest::Load(slot) => store.load(psx, slot).map(|_| {
                *loaded = true;
                format!("Loaded state {slot}")
            }),
        };
        match r {
            Ok(msg) => {
                log::info!("[{id}] {msg}");
                notices.push(msg);
            }
            Err(e) => {
                let msg = format!("{e:#}");
                let mut c = msg.chars();
                let msg = c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default();
                log::warn!("[{id}] {msg}");
                notices.push(msg);
            }
        }
    }
    notices
}
