//! The recompiled execution engine: block cache, dispatcher, compile-ahead
//! discovery, invalidation and (optionally) validation.

use crate::block::{self, BlockInfo};
use crate::codegen::{BlockFn, Compiler};
use crate::helpers::{Access, JitCtx};
use crate::validate::Validator;
use dwrc_psx::bus::mask_region;
use dwrc_psx::{Bus, Cpu, ExecEngine, HookAction};
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Clone)]
struct Entry {
    f: BlockFn,
    len: u32,
    info: Option<Box<BlockInfo>>,
}

#[derive(Debug, Default, Clone)]
pub struct Stats {
    pub blocks_compiled: u64,
    pub blocks_run: u64,
    pub interpreted: u64,
    pub invalidated: u64,
    pub compile_time_ms: f64,
}

pub struct RecompEngine {
    compiler: Compiler,
    /// Direct-mapped lookup for KSEG0 RAM (0x8000_0000..0x8020_0000).
    ram_table: Vec<Option<Entry>>,
    other: HashMap<u32, Entry>,
    /// Block start PCs per 4 KiB physical page, with their physical ranges.
    pages: HashMap<u32, Vec<(u32, u32, u32)>>,
    no_compile: HashSet<u32>,
    hooks_version: u64,
    validator: Option<Validator>,
    log: Vec<Access>,
    pub stats: Stats,
    /// Blocks translated ahead of execution per cache miss.
    pub compile_ahead: usize,
}

const RAM_WORDS: usize = 0x20_0000 / 4;
const CODE_LIMIT: usize = 192 << 20;

impl RecompEngine {
    pub fn new(validate: bool) -> Self {
        Self {
            compiler: Compiler::new(!validate),
            ram_table: vec![None; RAM_WORDS],
            other: HashMap::new(),
            pages: HashMap::new(),
            no_compile: HashSet::new(),
            hooks_version: u64::MAX,
            validator: if validate { Some(Validator::default()) } else { None },
            log: Vec::new(),
            stats: Stats::default(),
            compile_ahead: 512,
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
        if self.compiler.code_bytes > CODE_LIMIT {
            self.compiler.reset();
        }
    }

    fn compile_one(&mut self, pc: u32, bus: &mut Bus) -> Option<BlockInfo> {
        if self.no_compile.contains(&pc) {
            return None;
        }
        let Some(info) = block::discover(bus, pc) else {
            self.no_compile.insert(pc);
            return None;
        };
        let t0 = std::time::Instant::now();
        let f = self.compiler.compile(&info);
        self.stats.compile_time_ms += t0.elapsed().as_secs_f64() * 1000.0;
        match f {
            Some(f) => {
                self.stats.blocks_compiled += 1;
                let end = info.end_addr();
                let keep = if self.validator.is_some() { Some(Box::new(info.clone())) } else { None };
                self.insert(pc, Entry { f, len: info.instrs.len() as u32, info: keep }, bus, end);
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
        if self.compiler.code_bytes > CODE_LIMIT {
            log::info!("translated code cache full; starting over");
            self.flush_all(bus);
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

impl ExecEngine for RecompEngine {
    fn run(&mut self, cpu: &mut Cpu, bus: &mut Bus) {
        if bus.hooks_version() != self.hooks_version {
            self.hooks_version = bus.hooks_version();
            self.flush_all(bus);
        }
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
                self.compile_from(pc, bus);
            }
            let Some(e) = self.lookup(pc).cloned() else {
                cpu.exec_one(bus);
                self.stats.interpreted += 1;
                continue;
            };
            cpu.icache_touch(bus, pc, e.len);
            let snapshot = if self.validator.as_ref().map(|v| v.wants(pc)).unwrap_or(false) { Some(cpu.clone()) } else { None };
            self.log.clear();
            let log_ptr: *mut Vec<Access> = if self.validator.is_some() { &mut self.log } else { std::ptr::null_mut() };
            let mut ctx = JitCtx::new(cpu, bus, log_ptr);
            let ret = unsafe { (e.f)(&mut ctx) };
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

    fn name(&self) -> &'static str {
        if self.validator.is_some() {
            "recompiler (validating)"
        } else {
            "recompiler"
        }
    }

    fn report(&self) -> String {
        let mut s = format!(
            "blocks compiled {}, run {}, interpreted instrs {}, invalidated {}, compile {:.1} ms, code {} KiB",
            self.stats.blocks_compiled,
            self.stats.blocks_run,
            self.stats.interpreted,
            self.stats.invalidated,
            self.stats.compile_time_ms,
            self.compiler.code_bytes / 1024
        );
        if let Some(r) = self.validation_report() {
            s.push('\n');
            s.push_str(&r);
        }
        s
    }
}
