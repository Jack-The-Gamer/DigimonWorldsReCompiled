//! The recompiled execution engine: block cache, dispatcher, compile-ahead
//! discovery, background translation, invalidation and (optionally)
//! validation.
//!
//! Translating a block with Cranelift takes about half a millisecond, so a
//! game loading a few hundred blocks of new code at once (a battle starting)
//! would stop for a moment while they are translated. By default blocks are
//! translated on a worker thread instead; until a block's translation is
//! ready, the block runs on the reference interpreter *with the timing of
//! its translation* (every instruction fetch charged first, interrupts only
//! at the block's end), so the console ends up in exactly the same state
//! either way and the game never sees which of the two ran.

use crate::block::{self, BlockInfo};
use crate::codegen::{BlockFn, Compiler};
use crate::helpers::{Access, JitCtx};
use crate::validate::Validator;
use dwrc_mips::Instr;
use dwrc_psx::bus::mask_region;
use dwrc_psx::cpu::SR_CU2;
use dwrc_psx::{Bus, Cpu, ExecEngine, HookAction};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{mpsc, Arc, Condvar, Mutex};

/// Where blocks are translated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Translation {
    /// On the emulation thread, before a block first runs (check mode).
    Inline,
    /// On a worker thread; blocks run on the interpreter until then.
    Background,
    /// Never: every block runs on the interpreter (tests).
    Off,
}

#[derive(Clone)]
struct Entry {
    /// Native code, or `None` while the block runs on the interpreter.
    f: Option<BlockFn>,
    len: u32,
    /// The decoded block, for interpreting it and for check mode.
    info: Option<Arc<BlockInfo>>,
}

#[derive(Debug, Default, Clone)]
pub struct Stats {
    pub blocks_compiled: u64,
    /// Blocks whose code had been translated before (an overlay loaded again).
    pub blocks_reused: u64,
    pub blocks_run: u64,
    /// Block runs on the interpreter while their translation was pending.
    pub blocks_interpreted: u64,
    pub interpreted: u64,
    pub invalidated: u64,
    pub compile_time_ms: f64,
    /// Times the translated code filled its arena and was all dropped.
    pub restarts: u64,
}

pub struct RecompEngine {
    backend: Backend,
    /// Direct-mapped lookup for KSEG0 RAM (0x8000_0000..0x8020_0000).
    ram_table: Vec<Option<Entry>>,
    other: HashMap<u32, Entry>,
    /// Block start PCs per 4 KiB physical page, with their physical ranges.
    pages: HashMap<u32, Vec<(u32, u32, u32)>>,
    no_compile: HashSet<u32>,
    /// Every translation made, by start address and code, so code that comes
    /// back (an overlay loaded again) is not translated again.
    known: HashMap<u32, Vec<(Arc<BlockInfo>, BlockFn)>>,
    hooks_version: u64,
    validator: Option<Validator>,
    log: Vec<Access>,
    pub stats: Stats,
    /// Blocks translated ahead of execution per cache miss.
    pub compile_ahead: usize,
    /// Size of translated code at which all of it is dropped and translation
    /// starts over.
    pub code_limit: usize,
}

enum Backend {
    Inline(Compiler),
    Worker(Worker),
    Off,
}

const RAM_WORDS: usize = 0x20_0000 / 4;
const CODE_LIMIT: usize = 192 << 20;

impl RecompEngine {
    pub fn new(validate: bool, translation: Translation) -> Self {
        let backend = match translation {
            Translation::Inline => Backend::Inline(Compiler::new(!validate)),
            Translation::Off => Backend::Off,
            Translation::Background => match Worker::start(!validate) {
                Ok(w) => Backend::Worker(w),
                Err(e) => {
                    log::warn!("no translation thread ({e}); translating on the emulation thread");
                    Backend::Inline(Compiler::new(!validate))
                }
            },
        };
        Self {
            backend,
            ram_table: vec![None; RAM_WORDS],
            other: HashMap::new(),
            pages: HashMap::new(),
            no_compile: HashSet::new(),
            known: HashMap::new(),
            hooks_version: u64::MAX,
            validator: if validate { Some(Validator::default()) } else { None },
            log: Vec::new(),
            stats: Stats::default(),
            compile_ahead: 512,
            code_limit: CODE_LIMIT,
        }
    }

    #[inline(always)]
    fn lookup(&self, pc: u32) -> Option<&Entry> {
        if (0x8000_0000..0x8020_0000).contains(&pc) {
            self.ram_table[((pc & 0x1F_FFFF) >> 2) as usize].as_ref()
        } else {
            self.other.get(&pc)
        }
    }

    fn lookup_mut(&mut self, pc: u32) -> Option<&mut Entry> {
        if (0x8000_0000..0x8020_0000).contains(&pc) {
            self.ram_table[((pc & 0x1F_FFFF) >> 2) as usize].as_mut()
        } else {
            self.other.get_mut(&pc)
        }
    }

    fn insert(&mut self, pc: u32, e: Entry, bus: &mut Bus, end: u32) {
        let ps = mask_region(pc);
        let pe = mask_region(end.wrapping_sub(4)) + 4;
        if ps < 0x80_0000 {
            let (s, e2) = (ps & 0x1F_FFFF, ((pe - 1) & 0x1F_FFFF) + 1);
            let mut p = s >> 12;
            while p <= (e2 - 1) >> 12 {
                bus.mark_code_page(p << 12);
                self.pages.entry(p).or_default().push((pc, s, e2));
                p += 1;
            }
        }
        if (0x8000_0000..0x8020_0000).contains(&pc) {
            self.ram_table[((pc & 0x1F_FFFF) >> 2) as usize] = Some(e);
        } else {
            self.other.insert(pc, e);
        }
    }

    fn remove(&mut self, pc: u32) {
        if (0x8000_0000..0x8020_0000).contains(&pc) {
            self.ram_table[((pc & 0x1F_FFFF) >> 2) as usize] = None;
        } else {
            self.other.remove(&pc);
        }
    }

    pub fn flush_all(&mut self, bus: &mut Bus) {
        self.ram_table.iter_mut().for_each(|e| *e = None);
        self.other.clear();
        self.pages.clear();
        self.no_compile.clear();
        bus.clear_code_pages();
        if let Backend::Inline(c) = &mut self.backend {
            if c.code_bytes > self.code_limit {
                self.known.clear();
                c.reset();
                self.stats.restarts += 1;
            }
        }
    }

    fn code_bytes(&self) -> usize {
        match &self.backend {
            Backend::Inline(c) => c.code_bytes,
            Backend::Worker(w) => w.code_bytes,
            Backend::Off => 0,
        }
    }

    /// An earlier translation of exactly this code, if there is one.
    fn known_fn(&self, info: &BlockInfo) -> Option<BlockFn> {
        self.known.get(&info.start)?.iter().find(|(k, _)| k.instrs == info.instrs && k.end == info.end).map(|(_, f)| *f)
    }

    fn insert_native(&mut self, info: Arc<BlockInfo>, f: BlockFn, bus: &mut Bus) {
        let (pc, end, len) = (info.start, info.end_addr(), info.instrs.len() as u32);
        let keep = if self.validator.is_some() { Some(info) } else { None };
        self.insert(pc, Entry { f: Some(f), len, info: keep }, bus, end);
    }

    fn compile_one(&mut self, pc: u32, bus: &mut Bus) -> Option<Arc<BlockInfo>> {
        if self.no_compile.contains(&pc) {
            return None;
        }
        let Some(info) = block::discover(bus, pc) else {
            self.no_compile.insert(pc);
            return None;
        };
        let info = Arc::new(info);
        if let Some(f) = self.known_fn(&info) {
            self.stats.blocks_reused += 1;
            self.insert_native(info.clone(), f, bus);
            return Some(info);
        }
        let Backend::Inline(compiler) = &mut self.backend else { return None };
        let t0 = std::time::Instant::now();
        let f = compiler.compile(&info);
        self.stats.compile_time_ms += t0.elapsed().as_secs_f64() * 1000.0;
        match f {
            Some(f) => {
                self.stats.blocks_compiled += 1;
                self.known.entry(pc).or_default().push((info.clone(), f));
                self.insert_native(info.clone(), f, bus);
                Some(info)
            }
            None => {
                self.no_compile.insert(pc);
                None
            }
        }
    }

    /// Translates the block at `pc` and statically reachable blocks.
    fn compile_from(&mut self, pc: u32, bus: &mut Bus) {
        let mut queue = VecDeque::from([pc]);
        let mut budget = self.compile_ahead.max(1);
        let mut seen = HashSet::new();
        while let Some(a) = queue.pop_front() {
            if budget == 0 {
                break;
            }
            if !seen.insert(a) || self.lookup(a).is_some() || self.no_compile.contains(&a) || !block::translatable(a) {
                continue;
            }
            budget -= 1;
            if let Some(info) = self.compile_one(a, bus) {
                for s in info.successors() {
                    queue.push_back(s);
                }
            }
        }
        if self.code_bytes() > self.code_limit {
            log::info!("translated code cache full; starting over");
            self.flush_all(bus);
        }
    }

    /// Background translation: the block at `pc` runs on the interpreter
    /// until its translation arrives; it and the blocks statically reachable
    /// from it are queued for the worker (it first).
    fn request_from(&mut self, pc: u32, bus: &mut Bus) {
        let mut queue = VecDeque::from([pc]);
        let mut budget = if let Backend::Worker(_) = self.backend { self.compile_ahead.max(1) } else { 1 };
        let mut seen = HashSet::new();
        while let Some(a) = queue.pop_front() {
            if budget == 0 {
                break;
            }
            if !seen.insert(a) || self.lookup(a).is_some() || self.no_compile.contains(&a) || !block::translatable(a) {
                continue;
            }
            // Queued ahead already (and what follows it with it).
            if a != pc && matches!(&self.backend, Backend::Worker(w) if w.pending.contains_key(&a)) {
                continue;
            }
            budget -= 1;
            let Some(info) = block::discover(bus, a) else {
                self.no_compile.insert(a);
                continue;
            };
            let info = Arc::new(info);
            for s in info.successors() {
                queue.push_back(s);
            }
            if let Some(f) = self.known_fn(&info) {
                self.stats.blocks_reused += 1;
                self.insert_native(info, f, bus);
                continue;
            }
            let demand = a == pc;
            if demand {
                let (end, len) = (info.end_addr(), info.instrs.len() as u32);
                self.insert(a, Entry { f: None, len, info: Some(info.clone()) }, bus, end);
            }
            if let Backend::Worker(w) = &mut self.backend {
                w.request(info, demand);
            }
        }
    }

    /// Installs the translations the worker has finished.
    fn take_results(&mut self, bus: &mut Bus) {
        let Backend::Worker(w) = &mut self.backend else { return };
        let done = w.take();
        self.stats.compile_time_ms = w.compile_ms;
        for (info, f) in done {
            let Some(f) = f else { continue };
            self.stats.blocks_compiled += 1;
            self.known.entry(info.start).or_default().push((info.clone(), f));
            let pc = info.start;
            let same = |e: &BlockInfo| e.instrs == info.instrs && e.end == info.end;
            match self.lookup(pc).map(|e| (e.f.is_some(), e.info.clone())) {
                // Running on the interpreter: exactly this code from now on.
                Some((false, Some(cur))) if same(&cur) => {
                    let keep = self.validator.is_some();
                    let e = self.lookup_mut(pc).unwrap();
                    e.f = Some(f);
                    if !keep {
                        e.info = None;
                    }
                }
                // The code there changed while it was being translated.
                Some((false, Some(cur))) => {
                    if let Backend::Worker(w) = &mut self.backend {
                        w.request(cur, true);
                    }
                }
                Some(_) => {}
                // Translated ahead: install it if the code is still there.
                None => {
                    if !self.no_compile.contains(&pc) && block::discover(bus, pc).is_some_and(|d| same(&d)) {
                        self.insert_native(info, f, bus);
                    }
                }
            }
        }
        if self.code_bytes() > self.code_limit {
            // No translated code runs between blocks: forget every pointer
            // into it before the worker starts over.
            log::info!("translated code cache full; starting over");
            self.flush_all(bus);
            self.known.clear();
            if let Backend::Worker(w) = &mut self.backend {
                w.reset();
                self.stats.restarts += 1;
            }
        }
    }

    fn handle_invalidation(&mut self, bus: &mut Bus) {
        if let Some((start, len)) = bus.take_code_invalidation() {
            self.invalidate_range(start & 0x1F_FFFF, len);
        }
    }

    fn invalidate_range(&mut self, start: u32, len: u32) {
        let end = start.saturating_add(len);
        let mut p = start >> 12;
        while p <= end.saturating_sub(1) >> 12 {
            if let Some(list) = self.pages.get_mut(&p) {
                let mut keep = Vec::with_capacity(list.len());
                let mut kill = Vec::new();
                for &(pc, s, e) in list.iter() {
                    if s < end && start < e {
                        kill.push(pc);
                    } else {
                        keep.push((pc, s, e));
                    }
                }
                *list = keep;
                for pc in kill {
                    self.remove(pc);
                    self.stats.invalidated += 1;
                }
            }
            p += 1;
        }
        // Addresses that failed to compile may now hold different code.
        self.no_compile.retain(|pc| {
            let a = mask_region(*pc) & 0x1F_FFFF;
            !(a >= start && a < end)
        });
    }

    pub fn validation_report(&self) -> Option<String> {
        self.validator.as_ref().map(|v| v.report())
    }
}

/// Runs a block on the reference interpreter exactly as its translation
/// runs: every instruction fetch is charged first, the block runs to its end
/// (or an exception) without taking interrupts, and the CPU is left as the
/// dispatcher leaves it after native code, down to the fields native code
/// does not touch. Returns the instructions run by `exec_one` (the
/// COP2-disabled exit, as for native code).
fn interpret_block(cpu: &mut Cpu, bus: &mut Bus, info: &BlockInfo) -> u64 {
    let n = info.instrs.len() as u32;
    cpu.icache_touch(bus, info.start, n);
    if info.uses_cop2 && cpu.cop0.sr & SR_CU2 == 0 {
        cpu.set_pc(info.start);
        cpu.exec_one(bus);
        return 1;
    }
    let (instructions, exceptions) = (cpu.instructions, cpu.exceptions);
    let (current_pc, load_val, next_load_val) = (cpu.current_pc, cpu.load_val, cpu.next_load_val);
    for (k, &i) in info.instrs.iter().enumerate() {
        if cpu.pc != info.start.wrapping_add(k as u32 * 4) {
            break; // an exception left the block
        }
        cpu.exec_instr(bus, i);
    }
    cpu.instructions = instructions + n as u64;
    // Native code only sets the faulting instruction's address, and leaves
    // the load values alone unless a load is still in flight.
    if cpu.exceptions == exceptions {
        cpu.current_pc = current_pc;
    }
    if cpu.load_reg == 0 {
        cpu.load_val = load_val;
    }
    cpu.next_load_val = next_load_val;
    let pc = cpu.pc;
    cpu.set_pc(pc);
    0
}

impl ExecEngine for RecompEngine {
    fn run(&mut self, cpu: &mut Cpu, bus: &mut Bus) {
        if bus.hooks_version() != self.hooks_version {
            self.hooks_version = bus.hooks_version();
            self.flush_all(bus);
        }
        self.take_results(bus);
        while bus.cycles < bus.next_event {
            if cpu.interrupt_pending(bus) {
                cpu.take_interrupt(bus);
            }
            let pc = cpu.pc;
            if bus.has_hooks && bus.hook_at(pc) {
                match bus.run_hook(cpu, pc) {
                    HookAction::Redirect => continue,
                    HookAction::Continue => {
                        cpu.exec_one(bus);
                        self.stats.interpreted += 1;
                        continue;
                    }
                }
            }
            if cpu.load_reg != 0 || cpu.next_in_delay || pc & 3 != 0 {
                cpu.exec_one(bus);
                self.stats.interpreted += 1;
                continue;
            }
            if self.lookup(pc).is_none() {
                if let Backend::Inline(_) = self.backend {
                    self.compile_from(pc, bus);
                } else {
                    self.take_results(bus);
                    if self.lookup(pc).is_none() {
                        self.request_from(pc, bus);
                    }
                }
            }
            let Some(e) = self.lookup(pc).cloned() else {
                cpu.exec_one(bus);
                self.stats.interpreted += 1;
                continue;
            };
            match e.f {
                Some(f) => {
                    cpu.icache_touch(bus, pc, e.len);
                    let snapshot = if self.validator.as_ref().map(|v| v.wants(pc)).unwrap_or(false) { Some(cpu.clone()) } else { None };
                    self.log.clear();
                    let log_ptr: *mut Vec<Access> = if self.validator.is_some() { &mut self.log } else { std::ptr::null_mut() };
                    let mut ctx = JitCtx::new(cpu, bus, log_ptr);
                    let ret = unsafe { f(&mut ctx) };
                    if ret & 1 != 0 {
                        // COP2 disabled: let the interpreter raise the exception.
                        cpu.set_pc(ret & !1);
                        cpu.exec_one(bus);
                        self.stats.interpreted += 1;
                    } else {
                        cpu.set_pc(ret);
                        cpu.instructions += e.len as u64;
                    }
                    self.stats.blocks_run += 1;
                    if let (Some(snap), Some(v)) = (snapshot, self.validator.as_mut()) {
                        if let Some(info) = &e.info {
                            if !v.check(snap, info, cpu, &self.log) {
                                // Never run this translation again; interpret instead.
                                self.remove(pc);
                                self.no_compile.insert(pc);
                            }
                        }
                    }
                }
                None => {
                    if let Some(info) = &e.info {
                        self.stats.interpreted += interpret_block(cpu, bus, info);
                        self.stats.blocks_interpreted += 1;
                    }
                }
            }
            if bus.has_invalidation() {
                self.handle_invalidation(bus);
            }
            if bus.stop_requested {
                bus.stop_requested = false;
                break;
            }
        }
    }

    fn invalidate(&mut self, start: u32, len: u32) {
        self.invalidate_range(start & 0x1F_FFFF, len);
    }

    fn flush(&mut self, bus: &mut Bus) {
        self.flush_all(bus);
    }

    fn translate_ms(&self) -> f64 {
        match self.backend {
            Backend::Inline(_) => self.stats.compile_time_ms,
            _ => 0.0,
        }
    }

    fn name(&self) -> &'static str {
        if self.validator.is_some() {
            "recompiler (validating)"
        } else {
            "recompiler"
        }
    }

    fn report(&self) -> String {
        let s = &self.stats;
        let place = match self.backend {
            Backend::Inline(_) => "",
            Backend::Worker(_) => " in the background",
            Backend::Off => " (translation off)",
        };
        let mut r = format!(
            "blocks compiled {} (reused {}), run {} (on the interpreter {}), interpreted instrs {}, invalidated {}, compile {:.1} ms{place}, code {} KiB",
            s.blocks_compiled,
            s.blocks_reused,
            s.blocks_run,
            s.blocks_interpreted,
            s.interpreted,
            s.invalidated,
            s.compile_time_ms,
            self.code_bytes() / 1024
        );
        if s.restarts > 0 {
            r.push_str(&format!(", started over {} times", s.restarts));
        }
        if let Some(v) = self.validation_report() {
            r.push('\n');
            r.push_str(&v);
        }
        r
    }
}

// ----------------------------------------------------------------------
// Translation thread
// ----------------------------------------------------------------------

/// A block to translate, in one generation of the code arena.
struct Job {
    generation: u64,
    info: Arc<BlockInfo>,
}

#[derive(Default)]
struct Queue {
    /// Blocks the game is running now (on the interpreter), first.
    demand: VecDeque<Job>,
    /// Blocks reachable from them.
    ahead: VecDeque<Job>,
    generation: u64,
    stop: bool,
}

struct Done {
    generation: u64,
    info: Arc<BlockInfo>,
    f: Option<BlockFn>,
    ms: f64,
    code_bytes: usize,
}

/// The translation thread, with its own Cranelift module. The functions it
/// returns stay valid until the engine asks it to start over (`reset`),
/// which the engine only does once it has forgotten all of them.
struct Worker {
    shared: Arc<(Mutex<Queue>, Condvar)>,
    results: mpsc::Receiver<Done>,
    thread: Option<std::thread::JoinHandle<()>>,
    generation: u64,
    /// Blocks queued and not back yet, by start address (true: on demand).
    pending: HashMap<u32, bool>,
    code_bytes: usize,
    compile_ms: f64,
}

impl Worker {
    fn start(fast_paths: bool) -> std::io::Result<Self> {
        let shared = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
        let (tx, results) = mpsc::channel();
        let s = shared.clone();
        let thread = std::thread::Builder::new().name("dwrc-translate".into()).spawn(move || translate_loop(&s, &tx, fast_paths))?;
        Ok(Self { shared, results, thread: Some(thread), generation: 0, pending: HashMap::new(), code_bytes: 0, compile_ms: 0.0 })
    }

    fn request(&mut self, info: Arc<BlockInfo>, demand: bool) {
        match self.pending.get(&info.start) {
            Some(true) => return,
            Some(false) if !demand => return,
            _ => {}
        }
        self.pending.insert(info.start, demand);
        let (lock, cv) = &*self.shared;
        let mut q = lock.lock().unwrap();
        let job = Job { generation: self.generation, info };
        if demand {
            q.demand.push_back(job);
        } else {
            q.ahead.push_back(job);
        }
        cv.notify_one();
    }

    /// Translations finished since the last call, in the current arena.
    fn take(&mut self) -> Vec<(Arc<BlockInfo>, Option<BlockFn>)> {
        let mut out = Vec::new();
        while let Ok(d) = self.results.try_recv() {
            self.compile_ms += d.ms;
            if d.generation != self.generation {
                continue;
            }
            self.code_bytes = d.code_bytes;
            self.pending.remove(&d.info.start);
            out.push((d.info, d.f));
        }
        out
    }

    /// Frees all translated code and starts a new arena.
    fn reset(&mut self) {
        self.generation += 1;
        self.pending.clear();
        self.code_bytes = 0;
        let (lock, cv) = &*self.shared;
        let mut q = lock.lock().unwrap();
        q.demand.clear();
        q.ahead.clear();
        q.generation = self.generation;
        cv.notify_one();
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        {
            let (lock, cv) = &*self.shared;
            lock.lock().unwrap().stop = true;
            cv.notify_one();
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn translate_loop(shared: &(Mutex<Queue>, Condvar), tx: &mpsc::Sender<Done>, fast_paths: bool) {
    let (lock, cv) = shared;
    let mut compiler = Compiler::new(fast_paths);
    let mut generation = 0;
    // Translations made in this arena, so a block queued twice (ahead, then
    // on demand) is translated once.
    let mut done: HashSet<(u32, Vec<Instr>)> = HashSet::new();
    loop {
        let job = {
            let mut q = lock.lock().unwrap();
            loop {
                if q.stop {
                    drop(q);
                    // The engine is gone: nothing runs translated code now.
                    unsafe { compiler.free() };
                    return;
                }
                if q.generation != generation {
                    generation = q.generation;
                    done.clear();
                    compiler.reset();
                }
                match q.demand.pop_front().or_else(|| q.ahead.pop_front()) {
                    Some(j) if j.generation != generation => continue,
                    Some(j) => break j,
                    None => q = cv.wait(q).unwrap(),
                }
            }
        };
        // Already translated (queued ahead, then on demand): the engine has
        // that result; this one only tells it the job is done.
        let (f, ms) = if done.insert((job.info.start, job.info.instrs.clone())) {
            let t0 = std::time::Instant::now();
            let f = compiler.compile(&job.info);
            (f, t0.elapsed().as_secs_f64() * 1000.0)
        } else {
            (None, 0.0)
        };
        if tx.send(Done { generation, info: job.info, f, ms, code_bytes: compiler.code_bytes }).is_err() {
            return;
        }
    }
}
