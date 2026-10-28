//! Accurate PlayStation hardware core for Digimon Worlds Re:Compiled.
//!
//! This crate models the console the games were written for: the R3000A
//! CPU (with a reference interpreter), the GTE, GPU, SPU, CD-ROM
//! controller, MDEC, DMA, timers, interrupt controller, controllers and
//! memory cards. The static recompiler (`dwrc-recomp`) plugs in as an
//! alternative [`ExecEngine`] that runs translated native code against the
//! very same hardware, so both paths share one definition of "correct".

pub mod bus;
pub mod cdrom;
pub mod cpu;
pub mod dma;
pub mod gpu;
pub mod gte;
pub mod irq;
pub mod mdec;
pub mod memcard;
pub mod pad;
pub mod sio;
pub mod spu;
mod spu_gauss;
pub mod state;
pub mod timers;

pub use bus::Bus;
pub use cpu::{Cpu, CpuBus};
pub use pad::{Button, PadState};

/// CPU clock in Hz (identical on NTSC and PAL consoles).
pub const CPU_CLOCK: u64 = 33_868_800;
/// SPU output sample rate.
pub const AUDIO_RATE: u32 = 44_100;
/// CPU cycles per SPU sample.
pub const CYCLES_PER_SAMPLE: u64 = CPU_CLOCK / AUDIO_RATE as u64; // 768

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum VideoStandard {
    Ntsc,
    Pal,
}

/// The executing engine for CPU code. The reference interpreter is the
/// default; the recompiler provides a native implementation.
pub trait ExecEngine: Send {
    /// Runs CPU code until `bus.cycles >= bus.next_event` (or a hook asks
    /// to stop). Must service interrupts and keep timing.
    fn run(&mut self, cpu: &mut Cpu, bus: &mut Bus);
    /// Called when RAM that may contain code was modified (DMA, stores
    /// while the cache is isolated, explicit invalidation).
    fn invalidate(&mut self, _start: u32, _len: u32) {}
    /// Forgets everything derived from memory (a save state was loaded).
    fn flush(&mut self, _bus: &mut Bus) {}
    /// Host time the emulation thread has spent translating code so far, in
    /// milliseconds (tools; translation on other threads is not counted).
    fn translate_ms(&self) -> f64 {
        0.0
    }
    fn name(&self) -> &'static str;
    /// Human-readable statistics (shown by tools).
    fn report(&self) -> String {
        String::new()
    }
}

/// The reference interpreter engine.
#[derive(Default)]
pub struct Interpreter;

impl ExecEngine for Interpreter {
    fn run(&mut self, cpu: &mut Cpu, bus: &mut Bus) {
        cpu.run_interpreter(bus);
    }
    fn name(&self) -> &'static str {
        "interpreter"
    }
}

/// What a PC hook wants the CPU to do afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookAction {
    /// Execute the original instruction at this address.
    Continue,
    /// The hook changed `cpu.pc`/`cpu.next_pc` (e.g. returned from a
    /// replaced function); resume there. Hooks at the same address added
    /// after this one don't run this time.
    Redirect,
}

pub type PcHook = Box<dyn FnMut(&mut Cpu, &mut Bus) -> HookAction + Send>;

/// One PC hook, as returned by `Bus::add_hook` (for `Bus::remove_hook`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HookId(u64);

/// A complete console.
pub struct Psx {
    pub cpu: Cpu,
    pub bus: Bus,
    engine: Box<dyn ExecEngine>,
    pub frame_count: u64,
    /// The executable fast boot side-loads (host state, kept to re-arm it).
    fast_boot: Option<dwrc_disc::PsxExe>,
    /// Fast boot's hook at `SHELL_ENTRY`, gone once it ran.
    fast_boot_hook: Option<HookId>,
}

impl Psx {
    pub fn new(bios: &[u8], standard: VideoStandard) -> Self {
        let bus = Bus::new(bios, standard);
        let mut cpu = Cpu::new();
        cpu.reset();
        Self { cpu, bus, engine: Box::new(Interpreter), frame_count: 0, fast_boot: None, fast_boot_hook: None }
    }

    pub fn set_engine(&mut self, engine: Box<dyn ExecEngine>) {
        self.engine = engine;
    }

    pub fn engine_name(&self) -> &'static str {
        self.engine.name()
    }

    pub fn engine_report(&self) -> String {
        self.engine.report()
    }

    pub fn engine_translate_ms(&self) -> f64 {
        self.engine.translate_ms()
    }

    pub fn reset(&mut self) {
        self.cpu.reset();
        self.bus.reset();
    }

    pub fn insert_disc(&mut self, disc: Box<dyn dwrc_disc::SectorSource>, region: cdrom::DiscRegion) {
        self.bus.cdrom.insert_disc(disc, region);
    }

    /// Runs until the next vertical blank (one video frame).
    pub fn run_frame(&mut self) {
        self.bus.frame_done = false;
        let start = self.bus.cycles;
        while !self.bus.frame_done {
            self.engine.run(&mut self.cpu, &mut self.bus);
            self.bus.run_events();
            if let Some((s, l)) = self.bus.take_code_invalidation() {
                self.engine.invalidate(s, l);
            }
            // Safety valve: a frame never takes more than ~0.2s of CPU time.
            if self.bus.cycles - start > CPU_CLOCK / 5 {
                log::warn!("frame did not complete (display disabled?) - forcing frame end");
                break;
            }
        }
        self.frame_count += 1;
    }

    /// Runs for an exact number of CPU cycles (used by tests and lockstep).
    pub fn run_cycles(&mut self, cycles: u64) {
        let target = self.bus.cycles + cycles;
        while self.bus.cycles < target {
            self.bus.cap_next_event(target);
            self.engine.run(&mut self.cpu, &mut self.bus);
            self.bus.run_events();
            if let Some((s, l)) = self.bus.take_code_invalidation() {
                self.engine.invalidate(s, l);
            }
        }
    }

    /// Side-loads a PS-X EXE the way the BIOS shell would (fast boot).
    pub fn sideload_exe(&mut self, exe: &dwrc_disc::PsxExe) {
        sideload_exe(&mut self.cpu, &mut self.bus, exe);
    }

    /// Arranges for `exe` to be side-loaded when the BIOS reaches the shell
    /// entry point, skipping the boot animation (fast boot). The kernel is
    /// fully initialised by then, exactly as for a normal disc boot.
    pub fn enable_fast_boot(&mut self, exe: dwrc_disc::PsxExe) {
        self.fast_boot = Some(exe);
        self.arm_fast_boot();
    }

    fn arm_fast_boot(&mut self) {
        if let Some(id) = self.fast_boot_hook.take() {
            self.bus.remove_hook(id);
        }
        let mut exe = self.fast_boot.clone();
        let id = self.bus.add_hook_once(
            SHELL_ENTRY,
            Box::new(move |cpu, bus| {
                if let Some(e) = exe.take() {
                    sideload_exe(cpu, bus, &e);
                    HookAction::Redirect
                } else {
                    HookAction::Continue
                }
            }),
        );
        self.fast_boot_hook = Some(id);
    }

    /// Whether fast boot is still waiting for the BIOS to reach the shell.
    pub fn fast_boot_pending(&self) -> bool {
        self.fast_boot.is_some() && self.fast_boot_hook.is_some_and(|id| self.bus.has_hook(id))
    }

    /// Sets fast boot up for a state that was just loaded: armed again for a
    /// state saved during the boot, disarmed for one saved in the game, where
    /// 0x80030000 holds the game's own code (running it would restart it).
    pub fn set_fast_boot_pending(&mut self, pending: bool) {
        if self.fast_boot.is_none() || pending == self.fast_boot_pending() {
            return;
        }
        if pending {
            self.arm_fast_boot();
        } else if let Some(id) = self.fast_boot_hook.take() {
            self.bus.remove_hook(id);
        }
    }
}

/// Address the BIOS jumps to once the kernel is initialised (shell entry).
pub const SHELL_ENTRY: u32 = 0x8003_0000;

/// Copies an executable into RAM and sets up registers like BIOS `Exec()`.
pub fn sideload_exe(cpu: &mut Cpu, bus: &mut Bus, exe: &dwrc_disc::PsxExe) {
    let base = exe.load_addr & 0x1F_FFFF;
    for (i, b) in exe.text.iter().enumerate() {
        bus.ram[(base as usize + i) & (bus::RAM_SIZE - 1)] = *b;
    }
    if exe.bss_size != 0 {
        let b = exe.bss_addr & 0x1F_FFFF;
        for i in 0..exe.bss_size {
            bus.ram[((b + i) as usize) & (bus::RAM_SIZE - 1)] = 0;
        }
    }
    bus.note_code_write(exe.load_addr, exe.text.len() as u32);
    bus.cdrom.bios_boot_state();
    cpu.regs[28] = exe.gp;
    if exe.sp_base != 0 {
        let sp = exe.sp_base.wrapping_add(exe.sp_offset);
        cpu.regs[29] = sp;
        cpu.regs[30] = sp;
    }
    cpu.set_pc(exe.pc);
    cpu.flush_icache();
}

crate::sync_enum!(VideoStandard { Ntsc = 0, Pal = 1 });

/// What a save state starts with, and its layout version (bumped whenever a
/// device's fields change, so an old state is refused rather than misread).
const STATE_MAGIC: &[u8; 8] = b"DWRCPSX\0";
const STATE_VERSION: u32 = 1;

impl Psx {
    /// The whole console as bytes (see [`state`]). Taken between frames.
    pub fn save_state(&mut self) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 << 20);
        out.extend_from_slice(STATE_MAGIC);
        out.extend_from_slice(&STATE_VERSION.to_le_bytes());
        let mut s = state::Sync::save(&mut out);
        self.sync_state(&mut s);
        out
    }

    /// Restores a state from [`Psx::save_state`] taken with the same BIOS and
    /// disc. On any problem the console is left exactly as it was.
    pub fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let body = match data.strip_prefix(STATE_MAGIC.as_slice()) {
            Some(b) if b.len() >= 4 => b,
            _ => return Err("not a Re:Compiled save state".into()),
        };
        let version = u32::from_le_bytes(body[..4].try_into().unwrap());
        if version != STATE_VERSION {
            return Err(format!("saved by another version of Re:Compiled (state format {version}, this one reads {STATE_VERSION})"));
        }
        let before = self.save_state();
        let mut s = state::Sync::load(&body[4..]);
        self.sync_state(&mut s);
        let ok = s.finished();
        if !ok {
            let mut s = state::Sync::load(&before[STATE_MAGIC.len() + 4..]);
            self.sync_state(&mut s);
        }
        // RAM changed under the engine either way, and the widescreen side
        // strips belong to the pictures before.
        self.engine.flush(&mut self.bus);
        self.bus.gpu.clear_widescreen();
        if ok {
            Ok(())
        } else {
            Err("the save state is damaged".into())
        }
    }

    fn sync_state(&mut self, s: &mut state::Sync) {
        use state::Field;
        self.bus.sync(s);
        self.cpu.sync(s);
        self.frame_count.sync(s);
    }
}
