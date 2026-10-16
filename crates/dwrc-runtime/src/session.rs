//! A play session: one game running on the PS1 core with its BIOS, disc,
//! memory cards, mods and execution engine.

use crate::config::{Config, EngineKind};
use crate::library::{BiosChoice, DiscStatus, Library};
use crate::paths::Paths;
use anyhow::{anyhow, bail, Context, Result};
use dwrc_disc::ident::Region;
use dwrc_disc::iso9660::IsoFs;
use dwrc_disc::{Disc, GameId, Identification, PsxExe, SectorSource};
use dwrc_mods::{EnabledMod, ModRuntime};
use dwrc_psx::cdrom::DiscRegion;
use dwrc_psx::memcard::{MemoryCard, CARD_SIZE};
use dwrc_psx::{PadState, Psx, VideoStandard, CPU_CLOCK};
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub struct Session {
    pub game: GameId,
    pub ident: Identification,
    pub psx: Psx,
    pub mods: ModRuntime,
    pub enabled_mods: Vec<String>,
    card_paths: [Option<PathBuf>; 2],
    last_flush: Instant,
    last_frame_cycles: u64,
    /// Emulated duration of the last frame.
    pub last_frame_time: Duration,
    pub frame: u64,
    pub engine: EngineKind,
    /// The BIOS this session runs (a console dump or the built-in one).
    pub bios_name: String,
}

/// Information shown before starting (so the UI can explain problems).
pub fn preflight(lib: &Library, game: GameId) -> Result<()> {
    match lib.status(game) {
        DiscStatus::NotImported => bail!("{} has not been imported yet.", game.title()),
        DiscStatus::Missing => bail!("The disc image for {} was moved or deleted. Import it again.", game.title()),
        DiscStatus::Changed => bail!("The disc image for {} changed since it was verified. Import it again.", game.title()),
        DiscStatus::Ready => {}
    }
    Ok(())
}

fn load_card(path: &PathBuf) -> MemoryCard {
    match std::fs::read(path) {
        Ok(d) if d.len() == CARD_SIZE => MemoryCard::from_bytes(d.into_boxed_slice()),
        Ok(d) => {
            log::warn!("{} is {} bytes, expected {CARD_SIZE}; using a new card", path.display(), d.len());
            MemoryCard::new_formatted()
        }
        Err(_) => MemoryCard::new_formatted(),
    }
}

impl Session {
    pub fn start(paths: &Paths, lib: &Library, cfg: &Config, game: GameId, mods: Vec<EnabledMod>) -> Result<Self> {
        preflight(lib, game)?;
        let entry = &lib.games[&game];
        let ident = entry.ident.clone();
        let choice = lib.bios_choice(ident.region, cfg.emulation.bios);
        let bios_name = choice.label();
        let (bios, console_bios) = match choice {
            BiosChoice::Console(b) => (std::fs::read(&b.path).with_context(|| format!("reading {}", b.path.display()))?, true),
            BiosChoice::BuiltIn => (crate::bios::BUILTIN_BIOS.to_vec(), false),
        };

        let mut disc = Disc::open(&entry.disc_path)?;

        // Mods: disc overrides first (they edit the virtual disc).
        let enabled_mods: Vec<String> = mods.iter().map(|m| m.info.id().to_string()).collect();
        let mut mod_rt = {
            let mut ctx = dwrc_mods::disc::DiscContext::new(&mut disc);
            ModRuntime::build(game, &mods, &mut ctx)
        };
        for l in &mod_rt.log {
            log::info!("{l}");
        }
        // Read the boot executable from the (possibly modded) disc.
        let fs = IsoFs::mount(&mut disc)?;
        let exe_entry = fs.find(&mut disc, &ident.boot_file)?.ok_or_else(|| anyhow!("boot executable missing"))?;
        let exe = PsxExe::parse(&fs.read(&mut disc, &exe_entry)?)?;

        let (standard, region) = match ident.region {
            Region::Pal => (VideoStandard::Pal, DiscRegion::Pal),
            Region::NtscJ => (VideoStandard::Ntsc, DiscRegion::NtscJ),
            Region::NtscU => (VideoStandard::Ntsc, DiscRegion::NtscU),
        };
        let mut psx = Psx::new(&bios, standard);
        psx.insert_disc(Box::new(disc) as Box<dyn SectorSource>, region);

        let saves = paths.saves_dir(game);
        std::fs::create_dir_all(&saves)?;
        let c1 = saves.join("card1.mcd");
        let c2 = saves.join("card2.mcd");
        psx.bus.sio.cards[0] = Some(load_card(&c1));
        psx.bus.sio.cards[1] = if cfg.emulation.memory_card_2 { Some(load_card(&c2)) } else { None };
        let card_paths = [Some(c1), if cfg.emulation.memory_card_2 { Some(c2) } else { None }];

        // Fast boot skips the Sony BIOS's logo; the built-in BIOS boots the
        // disc directly anyway.
        if cfg.emulation.fast_boot && console_bios {
            psx.enable_fast_boot(exe.clone());
        }
        let engine = cfg.emulation.engine;
        match engine {
            EngineKind::Interpreter => {}
            EngineKind::Recompiled => psx.set_engine(dwrc_recomp::new_engine(dwrc_recomp::Mode::Native)),
            EngineKind::Validate => psx.set_engine(dwrc_recomp::new_engine(dwrc_recomp::Mode::Validate)),
        }
        mod_rt.install(&mut psx);
        log::info!("starting {} ({}) with the {} engine and the {bios_name} BIOS", ident.title, ident.serial, psx.engine_name());
        Ok(Self {
            game,
            ident,
            psx,
            mods: mod_rt,
            enabled_mods,
            card_paths,
            last_flush: Instant::now(),
            last_frame_cycles: 0,
            last_frame_time: Duration::from_millis(16),
            frame: 0,
            engine,
            bios_name,
        })
    }

    pub fn set_pad(&mut self, port: usize, mut state: PadState) {
        if port == 0 {
            state.buttons = self.mods.filter_input(state.buttons);
        }
        self.psx.bus.sio.pads[port].state = state;
        self.psx.bus.sio.pads[port].connected = port == 0 || state.buttons != 0 || self.psx.bus.sio.pads[port].connected;
    }

    pub fn run_frame(&mut self) {
        self.psx.run_frame();
        self.frame += 1;
        self.mods.on_frame(&mut self.psx, self.frame);
        let c = self.psx.bus.cycles;
        let dc = c - self.last_frame_cycles;
        self.last_frame_cycles = c;
        self.last_frame_time = Duration::from_nanos(dc * 1_000_000_000 / CPU_CLOCK);
        if self.last_flush.elapsed() > Duration::from_secs(2) {
            self.flush_memcards();
        }
    }

    /// Renders the current display into ARGB pixels.
    pub fn frame_pixels(&self, out: &mut Vec<u32>) -> (u32, u32) {
        self.psx.bus.gpu.render_display(out)
    }

    pub fn take_audio(&mut self, out: &mut Vec<i16>) {
        self.psx.bus.spu.take_output(out);
    }

    pub fn is_pal(&self) -> bool {
        self.psx.bus.gpu.is_pal_mode()
    }

    /// Writes changed memory cards to disk.
    pub fn flush_memcards(&mut self) {
        self.last_flush = Instant::now();
        for (i, p) in self.card_paths.iter().enumerate() {
            let (Some(p), Some(card)) = (p, self.psx.bus.sio.cards[i].as_mut()) else { continue };
            if card.dirty {
                let tmp = p.with_extension("mcd.tmp");
                if std::fs::write(&tmp, &card.data).and_then(|_| std::fs::rename(&tmp, p)).is_ok() {
                    card.dirty = false;
                } else {
                    log::error!("could not save memory card {}", p.display());
                }
            }
        }
    }

    pub fn reset(&mut self) {
        self.flush_memcards();
        self.psx.reset();
    }

    pub fn shutdown(&mut self) {
        self.flush_memcards();
        self.mods.save_all();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shutdown();
    }
}
