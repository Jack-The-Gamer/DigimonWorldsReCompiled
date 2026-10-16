//! The emulation thread: owns the [`Session`], runs it at the console's
//! real frame rate and publishes frames and audio.

use crate::audio::AudioShared;
use dwrc_disc::GameId;
use dwrc_mods::EnabledMod;
use dwrc_psx::PadState;
use dwrc_runtime::{Config, Library, Paths, Session};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct FrameBuf {
    pub pixels: Vec<u32>,
    pub width: u32,
    pub height: u32,
    pub seq: u64,
}

#[derive(Default, Clone)]
pub struct EmuStatus {
    pub running: bool,
    pub error: Option<String>,
    pub fps: f32,
    pub speed: f32,
    pub engine: String,
    pub title: String,
    pub mod_log: Vec<String>,
    pub pal: bool,
    pub bios: String,
}

pub struct Shared {
    pub frame: Mutex<FrameBuf>,
    pub pad: [AtomicU16; 2],
    pub paused: AtomicBool,
    pub fast_forward: AtomicBool,
    pub quit: AtomicBool,
    pub status: Mutex<EmuStatus>,
}

pub enum EmuCmd {
    Reset,
}

pub struct EmuHandle {
    pub shared: Arc<Shared>,
    pub game: GameId,
    cmd: Sender<EmuCmd>,
    thread: Option<JoinHandle<()>>,
}

impl EmuHandle {
    pub fn start(paths: Paths, lib: Library, cfg: Config, game: GameId, mods: Vec<EnabledMod>, audio: Arc<AudioShared>) -> Self {
        let shared = Arc::new(Shared {
            frame: Mutex::new(FrameBuf::default()),
            pad: [AtomicU16::new(0), AtomicU16::new(0)],
            paused: AtomicBool::new(false),
            fast_forward: AtomicBool::new(false),
            quit: AtomicBool::new(false),
            status: Mutex::new(EmuStatus::default()),
        });
        let (tx, rx) = channel();
        let sh = shared.clone();
        let thread = std::thread::Builder::new()
            .name("emulation".into())
            .stack_size(16 << 20)
            .spawn(move || run(paths, lib, cfg, game, mods, sh, rx, audio))
            .expect("spawn emulation thread");
        Self { shared, game, cmd: tx, thread: Some(thread) }
    }

    pub fn send(&self, c: EmuCmd) {
        let _ = self.cmd.send(c);
    }

    pub fn set_pad(&self, port: usize, s: PadState) {
        self.shared.pad[port].store(s.buttons, Ordering::Relaxed);
    }

    pub fn stop(&mut self) {
        self.shared.quit.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for EmuHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

#[allow(clippy::too_many_arguments)]
fn run(
    paths: Paths,
    lib: Library,
    cfg: Config,
    game: GameId,
    mods: Vec<EnabledMod>,
    sh: Arc<Shared>,
    rx: Receiver<EmuCmd>,
    audio: Arc<AudioShared>,
) {
    let mut session = match Session::start(&paths, &lib, &cfg, game, mods) {
        Ok(s) => s,
        Err(e) => {
            sh.status.lock().unwrap().error = Some(format!("{e:#}"));
            return;
        }
    };
    {
        let mut st = sh.status.lock().unwrap();
        st.running = true;
        st.engine = session.psx.engine_name().to_string();
        st.title = session.ident.title.clone();
        st.mod_log = session.mods.log.clone();
        st.bios = session.bios_name.clone();
    }
    audio.clear();
    let mut pixels = Vec::new();
    let mut samples = Vec::new();
    let mut target = Instant::now();
    let mut fps_t = Instant::now();
    let mut fps_n = 0u32;
    let mut emu_time = Duration::ZERO;
    while !sh.quit.load(Ordering::Relaxed) {
        while let Ok(c) = rx.try_recv() {
            match c {
                EmuCmd::Reset => {
                    session.reset();
                    audio.clear();
                }
            }
        }
        if sh.paused.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(10));
            target = Instant::now();
            continue;
        }
        let p0 = PadState { buttons: sh.pad[0].load(Ordering::Relaxed) };
        session.set_pad(0, p0);
        session.run_frame();
        let (w, h) = session.frame_pixels(&mut pixels);
        {
            let mut f = sh.frame.lock().unwrap();
            std::mem::swap(&mut f.pixels, &mut pixels);
            f.width = w;
            f.height = h;
            f.seq += 1;
        }
        samples.clear();
        session.take_audio(&mut samples);
        let ff = sh.fast_forward.load(Ordering::Relaxed);
        if !ff {
            audio.push(&samples);
        }
        // Pace to the console's own frame timing.
        let ft = session.last_frame_time;
        emu_time += ft;
        fps_n += 1;
        if fps_t.elapsed() >= Duration::from_millis(500) {
            let el = fps_t.elapsed();
            let mut st = sh.status.lock().unwrap();
            st.fps = fps_n as f32 / el.as_secs_f32();
            st.speed = emu_time.as_secs_f32() / el.as_secs_f32();
            st.pal = session.is_pal();
            st.mod_log = session.mods.log.clone();
            fps_t = Instant::now();
            fps_n = 0;
            emu_time = Duration::ZERO;
        }
        if ff {
            target = Instant::now();
            continue;
        }
        target += ft;
        let now = Instant::now();
        if target > now {
            std::thread::sleep(target - now);
        } else if now - target > Duration::from_millis(250) {
            target = now; // fell far behind (e.g. window dragged); resync
        }
    }
    session.shutdown();
    sh.status.lock().unwrap().running = false;
}
