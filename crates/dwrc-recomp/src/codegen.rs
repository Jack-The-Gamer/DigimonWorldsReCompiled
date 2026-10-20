//! MIPS R3000A -> native code translation with Cranelift.
//!
//! Each guest basic block becomes one native function
//! `extern "C" fn(*mut JitCtx) -> u32` returning the next guest PC.
//!
//! Semantics mirror the reference interpreter exactly:
//! * guest registers live in SSA variables inside a block and are written
//!   back at every exit;
//! * load delay slots are resolved at translation time (an in-flight load
//!   at the end of a block is handed back to the CPU state, exactly as the
//!   interpreter would hold it);
//! * branch delay slots execute after the branch condition is evaluated;
//! * exceptions (SYSCALL, BREAK, overflow, address errors, reserved
//!   instructions) leave the block through the same exception entry code
//!   the interpreter uses;
//! * RAM and scratchpad accesses use inline fast paths; everything else
//!   (I/O, BIOS, cache-isolated stores, stores into translated code) goes
//!   through the shared bus helpers.

use crate::block::{BlockInfo, End};
use crate::helpers::{self, JitCtx};
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{types, AbiParam, InstBuilder, MemFlags, SigRef, Type, Value};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::Module;
use dwrc_mips::{decode, gpr_usage, Flow, Instr, Op};
use dwrc_psx::bus::RAM_READ_CYCLES;
use dwrc_psx::cpu::exc;
use std::mem::offset_of;

pub type BlockFn = unsafe extern "C" fn(*mut JitCtx) -> u32;

pub struct Compiler {
    module: JITModule,
    ctx: Context,
    fctx: FunctionBuilderContext,
    ptr: Type,
    call_conv: cranelift_codegen::isa::CallConv,
    /// Use inline RAM/scratchpad fast paths (disabled in validation mode so
    /// every access is observable).
    pub fast_paths: bool,
    pub code_bytes: usize,
    pub blocks: usize,
}

// SAFETY: a `Compiler` (and the JIT module inside it) is owned by exactly
// one execution engine, which is only ever used by one thread at a time; it
// is moved to the emulation thread, never shared.
unsafe impl Send for Compiler {}

fn new_module() -> JITModule {
    let mut flags = settings::builder();
    flags.set("use_colocated_libcalls", "false").unwrap();
    flags.set("is_pic", "false").unwrap();
    flags.set("opt_level", "speed").unwrap();
    let isa_builder = cranelift_native::builder().expect("host architecture not supported by Cranelift");
    let isa = isa_builder.finish(settings::Flags::new(flags)).expect("ISA");
    JITModule::new(JITBuilder::with_isa(isa, cranelift_module::default_libcall_names()))
}

impl Compiler {
    pub fn new(fast_paths: bool) -> Self {
        let module = new_module();
        let ptr = module.target_config().pointer_type();
        let call_conv = module.target_config().default_call_conv;
        let ctx = module.make_context();
        Self { module, ctx, fctx: FunctionBuilderContext::new(), ptr, call_conv, fast_paths, code_bytes: 0, blocks: 0 }
    }

    /// Drops all generated code. Only call when no translated code is
    /// executing and every function pointer handed out has been forgotten.
    pub fn reset(&mut self) {
        let fast = self.fast_paths;
        let old = std::mem::replace(self, Compiler::new(fast));
        unsafe { old.module.free_memory() };
    }

    /// Drops all generated code for good.
    ///
    /// # Safety
    /// No translated code may run afterwards: every function pointer handed
    /// out must have been forgotten.
    pub unsafe fn free(self) {
        unsafe { self.module.free_memory() };
    }

    pub fn compile(&mut self, blk: &BlockInfo) -> Option<BlockFn> {
        self.ctx.func.signature.params.clear();
        self.ctx.func.signature.returns.clear();
        self.ctx.func.signature.params.push(AbiParam::new(self.ptr));
        self.ctx.func.signature.returns.push(AbiParam::new(types::I32));
        {
            let mut b = FunctionBuilder::new(&mut self.ctx.func, &mut self.fctx);
            let mut t = Tr::new(&mut b, self.ptr, self.fast_paths, self.call_conv);
            t.translate(blk);
            b.finalize();
        }
        let id = match self.module.declare_anonymous_function(&self.ctx.func.signature) {
            Ok(id) => id,
            Err(e) => {
                log::error!("declare failed: {e}");
                self.module.clear_context(&mut self.ctx);
                return None;
            }
        };
        if let Err(e) = self.module.define_function(id, &mut self.ctx) {
            log::error!("translation of block {:08x} failed: {e:?}", blk.start);
            self.module.clear_context(&mut self.ctx);
            return None;
        }
        if let Some(c) = self.ctx.compiled_code() {
            self.code_bytes += c.code_buffer().len();
        }
        self.module.clear_context(&mut self.ctx);
        if let Err(e) = self.module.finalize_definitions() {
            log::error!("finalize failed: {e}");
            return None;
        }
        self.blocks += 1;
        let p = self.module.get_finalized_function(id);
        Some(unsafe { std::mem::transmute::<*const u8, BlockFn>(p) })
    }
}

struct Sigs {
    /// (ctx, a) -> i32
    r1: SigRef,
    /// (ctx) -> i32
    r0: SigRef,
    /// (ctx, a, b)
    w2: SigRef,
    /// (ctx, a)
    w1: SigRef,
    /// (ctx)
    v0: SigRef,
    /// (ctx, code, pc, in_delay, cop, badv) -> i32
    exc: SigRef,
}

struct Tr<'a, 'b> {
    b: &'a mut FunctionBuilder<'b>,
    ptr: Type,
    fast: bool,
    ctx: Value,
    regs_p: Value,
    ram_p: Value,
    scratch_p: Value,
    cycles_p: Value,
    code_pages_p: Value,
    isc_ok: Value,
    vars: Vec<Variable>,
    dirty: [bool; 32],
    cyc: Variable,
    cyc_dirty: bool,
    /// Load issued by the previous instruction (not yet visible).
    load: Option<(usize, Value)>,
    /// Load issued by the current instruction.
    next_load: Option<(usize, Value)>,
    sigs: Sigs,
}

fn flags() -> MemFlags {
    MemFlags::trusted()
}

impl<'a, 'b> Tr<'a, 'b> {
    fn new(b: &'a mut FunctionBuilder<'b>, ptr: Type, fast: bool, cc: cranelift_codegen::isa::CallConv) -> Self {
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        b.seal_block(entry);
        let ctx = b.block_params(entry)[0];
        let ld = |b: &mut FunctionBuilder, off: usize| b.ins().load(ptr, flags(), ctx, off as i32);
        let regs_p = ld(b, offset_of!(JitCtx, regs));
        let ram_p = ld(b, offset_of!(JitCtx, ram));
        let scratch_p = ld(b, offset_of!(JitCtx, scratch));
        let cycles_p = ld(b, offset_of!(JitCtx, cycles));
        let code_pages_p = ld(b, offset_of!(JitCtx, code_pages));
        let sr_p = ld(b, offset_of!(JitCtx, sr));
        let sr = b.ins().load(types::I32, flags(), sr_p, 0);
        let isc = b.ins().band_imm(sr, dwrc_psx::cpu::SR_ISC as i64);
        let isc_ok = b.ins().icmp_imm(IntCC::Equal, isc, 0);
        let mut vars = Vec::with_capacity(32);
        for i in 0..32 {
            let v = Variable::from_u32(i as u32);
            b.declare_var(v, types::I32);
            vars.push(v);
        }
        let cyc = Variable::from_u32(32);
        b.declare_var(cyc, types::I64);
        let zero = b.ins().iconst(types::I64, 0);
        b.def_var(cyc, zero);
        let mk = |b: &mut FunctionBuilder, params: &[Type], ret: Option<Type>| {
            let mut s = cranelift_codegen::ir::Signature::new(cc);
            s.params.push(AbiParam::new(ptr));
            for p in params {
                s.params.push(AbiParam::new(*p));
            }
            if let Some(r) = ret {
                s.returns.push(AbiParam::new(r));
            }
            b.import_signature(s)
        };
        let i32t = types::I32;
        let sigs = Sigs {
            r1: mk(b, &[i32t], Some(i32t)),
            r0: mk(b, &[], Some(i32t)),
            w2: mk(b, &[i32t, i32t], None),
            w1: mk(b, &[i32t], None),
            v0: mk(b, &[], None),
            exc: mk(b, &[i32t, i32t, i32t, i32t, i32t], Some(i32t)),
        };
        Self {
            b,
            ptr,
            fast,
            ctx,
            regs_p,
            ram_p,
            scratch_p,
            cycles_p,
            code_pages_p,
            isc_ok,
            vars,
            dirty: [false; 32],
            cyc,
            cyc_dirty: false,
            load: None,
            next_load: None,
            sigs,
        }
    }

    // ------------------------------------------------------------------
    // Register file
    // ------------------------------------------------------------------

    fn c32(&mut self, v: u32) -> Value {
        self.b.ins().iconst(types::I32, v as i32 as i64)
    }

    fn get(&mut self, r: usize) -> Value {
        if r == 0 {
            self.c32(0)
        } else {
            self.b.use_var(self.vars[r])
        }
    }

    fn set(&mut self, r: usize, v: Value) {
        if r != 0 {
            self.b.def_var(self.vars[r], v);
            self.dirty[r] = true;
        }
        if matches!(self.load, Some((lr, _)) if lr == r) {
            self.load = None;
        }
    }

    fn set_delayed(&mut self, r: usize, v: Value) {
        if r == 0 {
            return;
        }
        if matches!(self.load, Some((lr, _)) if lr == r) {
            self.load = None;
        }
        self.next_load = Some((r, v));
    }

    /// End of an instruction: the previous load becomes visible.
    fn retire(&mut self) {
        if let Some((r, v)) = self.load.take() {
            self.b.def_var(self.vars[r], v);
            self.dirty[r] = true;
        }
        self.load = self.next_load.take();
    }

    fn load_entry_regs(&mut self, blk: &BlockInfo) {
        let mut need = [false; 32];
        for i in &blk.instrs {
            let (src, _) = gpr_usage(*i);
            for r in src.into_iter().flatten() {
                need[r] = true;
            }
            if matches!(decode(*i), Op::Lwl | Op::Lwr) {
                need[i.rt()] = true;
            }
        }
        for (r, n) in need.iter().enumerate().skip(1) {
            if *n {
                let v = self.b.ins().load(types::I32, flags(), self.regs_p, (r * 4) as i32);
                self.b.def_var(self.vars[r], v);
            }
        }
    }

    /// Stores dirty registers (plus a still-pending load, if `commit_load`).
    fn writeback(&mut self, commit_load: Option<(usize, Value)>) {
        for r in 1..32 {
            if self.dirty[r] && commit_load.map(|(lr, _)| lr != r).unwrap_or(true) {
                let v = self.b.use_var(self.vars[r]);
                self.b.ins().store(flags(), v, self.regs_p, (r * 4) as i32);
            }
        }
        if let Some((r, v)) = commit_load {
            self.b.ins().store(flags(), v, self.regs_p, (r * 4) as i32);
        }
    }

    fn flush_cycles(&mut self) {
        if !self.cyc_dirty {
            return;
        }
        let c = self.b.ins().load(types::I64, flags(), self.cycles_p, 0);
        let d = self.b.use_var(self.cyc);
        let n = self.b.ins().iadd(c, d);
        self.b.ins().store(flags(), n, self.cycles_p, 0);
        let z = self.b.ins().iconst(types::I64, 0);
        self.b.def_var(self.cyc, z);
    }

    fn call(&mut self, f: usize, sig: SigRef, args: &[Value]) -> Option<Value> {
        self.flush_cycles();
        let callee = self.b.ins().iconst(self.ptr, f as i64);
        let mut a = Vec::with_capacity(args.len() + 1);
        a.push(self.ctx);
        a.extend_from_slice(args);
        let inst = self.b.ins().call_indirect(sig, callee, &a);
        self.b.inst_results(inst).first().copied()
    }

    // ------------------------------------------------------------------
    // Exits
    // ------------------------------------------------------------------

    /// Leaves the block through the exception entry (on the current path).
    fn exception_exit(&mut self, code: u32, pc: u32, in_delay: bool, cop: u32, badv: Option<Value>) {
        let pending = self.load;
        self.writeback(pending);
        let code_v = self.c32(code);
        let pc_v = self.c32(pc);
        let d = self.c32(in_delay as u32);
        let cop_v = self.c32(cop);
        let bv = match badv {
            Some(v) => v,
            None => self.c32(0),
        };
        let r = self.call(helpers::exception as *const () as usize, self.sigs.exc, &[code_v, pc_v, d, cop_v, bv]).unwrap();
        self.b.ins().return_(&[r]);
    }

    /// Branches to an exception exit when `cond` is true.
    fn exception_if(&mut self, cond: Value, code: u32, pc: u32, in_delay: bool, badv: Option<Value>) {
        let exc_b = self.b.create_block();
        let cont = self.b.create_block();
        self.b.ins().brif(cond, exc_b, &[], cont, &[]);
        self.b.seal_block(exc_b);
        self.b.switch_to_block(exc_b);
        let saved_cyc_dirty = self.cyc_dirty;
        self.exception_exit(code, pc, in_delay, 0, badv);
        self.cyc_dirty = saved_cyc_dirty;
        self.b.seal_block(cont);
        self.b.switch_to_block(cont);
    }

    fn normal_exit(&mut self, next: Value) {
        self.writeback(None);
        if let Some((r, v)) = self.load {
            let lr_p = self.b.ins().load(self.ptr, flags(), self.ctx, offset_of!(JitCtx, load_reg) as i32);
            let lv_p = self.b.ins().load(self.ptr, flags(), self.ctx, offset_of!(JitCtx, load_val) as i32);
            let rv = self.b.ins().iconst(self.ptr, r as i64);
            self.b.ins().store(flags(), rv, lr_p, 0);
            self.b.ins().store(flags(), v, lv_p, 0);
        }
        self.flush_cycles();
        self.b.ins().return_(&[next]);
    }

    // ------------------------------------------------------------------
    // Memory
    // ------------------------------------------------------------------

    fn extend(&mut self, v: Value, bytes: u32, signed: bool) -> Value {
        match (bytes, signed) {
            (1, true) => {
                let t = self.b.ins().ishl_imm(v, 24);
                self.b.ins().sshr_imm(t, 24)
            }
            (2, true) => {
                let t = self.b.ins().ishl_imm(v, 16);
                self.b.ins().sshr_imm(t, 16)
            }
            _ => v,
        }
    }

    fn mem_load(&mut self, addr: Value, bytes: u32, signed: bool) -> Value {
        let helper = match bytes {
            1 => helpers::rd8 as *const () as usize,
            2 => helpers::rd16 as *const () as usize,
            _ => helpers::rd32 as *const () as usize,
        };
        if !self.fast {
            let v = self.call(helper, self.sigs.r1, &[addr]).unwrap();
            return self.extend(v, bytes, signed);
        }
        let ty = match bytes {
            1 => types::I8,
            2 => types::I16,
            _ => types::I32,
        };
        let ram_b = self.b.create_block();
        let chk_b = self.b.create_block();
        let scr_b = self.b.create_block();
        let slow_b = self.b.create_block();
        let done = self.b.create_block();
        self.b.append_block_param(done, types::I32);

        let t = self.b.ins().band_imm(addr, 0x5F80_0000);
        let is_ram = self.b.ins().icmp_imm(IntCC::Equal, t, 0);
        self.b.ins().brif(is_ram, ram_b, &[], chk_b, &[]);
        self.b.seal_block(ram_b);
        self.b.seal_block(chk_b);

        // RAM
        self.b.switch_to_block(ram_b);
        let off = self.b.ins().band_imm(addr, 0x1F_FFFF);
        let off = self.b.ins().uextend(self.ptr, off);
        let host = self.b.ins().iadd(self.ram_p, off);
        let raw = self.b.ins().load(ty, flags(), host, 0);
        let v = self.widen(raw, ty, signed);
        let c = self.b.use_var(self.cyc);
        let c = self.b.ins().iadd_imm(c, RAM_READ_CYCLES as i64);
        self.b.def_var(self.cyc, c);
        self.b.ins().jump(done, &[v.into()]);

        // Scratchpad?
        self.b.switch_to_block(chk_b);
        let t2 = self.b.ins().band_imm(addr, 0x7FFF_FC00);
        let is_scr = self.b.ins().icmp_imm(IntCC::Equal, t2, 0x1F80_0000);
        self.b.ins().brif(is_scr, scr_b, &[], slow_b, &[]);
        self.b.seal_block(scr_b);
        self.b.seal_block(slow_b);

        self.b.switch_to_block(scr_b);
        let off = self.b.ins().band_imm(addr, 0x3FF);
        let off = self.b.ins().uextend(self.ptr, off);
        let host = self.b.ins().iadd(self.scratch_p, off);
        let raw = self.b.ins().load(ty, flags(), host, 0);
        let v = self.widen(raw, ty, signed);
        self.b.ins().jump(done, &[v.into()]);

        // Everything else
        self.b.switch_to_block(slow_b);
        self.cyc_dirty = true; // the RAM path may have added cycles
        let v = self.call(helper, self.sigs.r1, &[addr]).unwrap();
        let v = self.extend(v, bytes, signed);
        self.b.ins().jump(done, &[v.into()]);

        self.b.seal_block(done);
        self.b.switch_to_block(done);
        self.cyc_dirty = true;
        self.b.block_params(done)[0]
    }

    fn widen(&mut self, raw: Value, ty: Type, signed: bool) -> Value {
        if ty == types::I32 {
            raw
        } else if signed {
            self.b.ins().sextend(types::I32, raw)
        } else {
            self.b.ins().uextend(types::I32, raw)
        }
    }

    fn mem_store(&mut self, addr: Value, val: Value, bytes: u32) {
        let helper = match bytes {
            1 => helpers::wr8 as *const () as usize,
            2 => helpers::wr16 as *const () as usize,
            _ => helpers::wr32 as *const () as usize,
        };
        if !self.fast {
            self.call(helper, self.sigs.w2, &[addr, val]);
            return;
        }
        let ram_b = self.b.create_block();
        let chk_b = self.b.create_block();
        let scr_b = self.b.create_block();
        let slow_b = self.b.create_block();
        let done = self.b.create_block();

        // RAM, cache not isolated, and not a page holding translated code.
        let t = self.b.ins().band_imm(addr, 0x5F80_0000);
        let is_ram = self.b.ins().icmp_imm(IntCC::Equal, t, 0);
        let widx = self.b.ins().ushr_imm(addr, 18);
        let widx = self.b.ins().band_imm(widx, 7);
        let widx = self.b.ins().uextend(self.ptr, widx);
        let woff = self.b.ins().ishl_imm(widx, 3);
        let waddr = self.b.ins().iadd(self.code_pages_p, woff);
        let word = self.b.ins().load(types::I64, flags(), waddr, 0);
        let bit = self.b.ins().ushr_imm(addr, 12);
        let bit = self.b.ins().band_imm(bit, 63);
        let bit = self.b.ins().uextend(types::I64, bit);
        let sh = self.b.ins().ushr(word, bit);
        let sh = self.b.ins().band_imm(sh, 1);
        let clean = self.b.ins().icmp_imm(IntCC::Equal, sh, 0);
        let ok = self.b.ins().band(is_ram, clean);
        let ok = self.b.ins().band(ok, self.isc_ok);
        self.b.ins().brif(ok, ram_b, &[], chk_b, &[]);
        self.b.seal_block(ram_b);
        self.b.seal_block(chk_b);

        self.b.switch_to_block(ram_b);
        let off = self.b.ins().band_imm(addr, 0x1F_FFFF);
        let off = self.b.ins().uextend(self.ptr, off);
        let host = self.b.ins().iadd(self.ram_p, off);
        self.narrow_store(val, host, bytes);
        self.b.ins().jump(done, &[]);

        self.b.switch_to_block(chk_b);
        let t2 = self.b.ins().band_imm(addr, 0x7FFF_FC00);
        let is_scr = self.b.ins().icmp_imm(IntCC::Equal, t2, 0x1F80_0000);
        let ok2 = self.b.ins().band(is_scr, self.isc_ok);
        self.b.ins().brif(ok2, scr_b, &[], slow_b, &[]);
        self.b.seal_block(scr_b);
        self.b.seal_block(slow_b);

        self.b.switch_to_block(scr_b);
        let off = self.b.ins().band_imm(addr, 0x3FF);
        let off = self.b.ins().uextend(self.ptr, off);
        let host = self.b.ins().iadd(self.scratch_p, off);
        self.narrow_store(val, host, bytes);
        self.b.ins().jump(done, &[]);

        self.b.switch_to_block(slow_b);
        self.call(helper, self.sigs.w2, &[addr, val]);
        self.b.ins().jump(done, &[]);

        self.b.seal_block(done);
        self.b.switch_to_block(done);
    }

    fn narrow_store(&mut self, val: Value, host: Value, bytes: u32) {
        match bytes {
            1 => {
                let v = self.b.ins().ireduce(types::I8, val);
                self.b.ins().store(flags(), v, host, 0);
            }
            2 => {
                let v = self.b.ins().ireduce(types::I16, val);
                self.b.ins().store(flags(), v, host, 0);
            }
            _ => {
                self.b.ins().store(flags(), val, host, 0);
            }
        }
    }

    fn misaligned(&mut self, addr: Value, bytes: u32) -> Value {
        let m = self.b.ins().band_imm(addr, (bytes - 1) as i64);
        self.b.ins().icmp_imm(IntCC::NotEqual, m, 0)
    }

    // ------------------------------------------------------------------
    // Translation
    // ------------------------------------------------------------------

    fn translate(&mut self, blk: &BlockInfo) {
        self.load_entry_regs(blk);
        if blk.uses_cop2 {
            // If COP2 is disabled, let the interpreter raise the exception.
            let sr_p = self.b.ins().load(self.ptr, flags(), self.ctx, offset_of!(JitCtx, sr) as i32);
            let sr = self.b.ins().load(types::I32, flags(), sr_p, 0);
            let cu2 = self.b.ins().band_imm(sr, dwrc_psx::cpu::SR_CU2 as i64);
            let off = self.b.ins().icmp_imm(IntCC::Equal, cu2, 0);
            let fb = self.b.create_block();
            let cont = self.b.create_block();
            self.b.ins().brif(off, fb, &[], cont, &[]);
            self.b.seal_block(fb);
            self.b.switch_to_block(fb);
            let r = self.c32(blk.start | 1);
            self.b.ins().return_(&[r]);
            self.b.seal_block(cont);
            self.b.switch_to_block(cont);
        }
        let n = blk.instrs.len();
        let mut k = 0;
        while k < n {
            let pc = blk.start.wrapping_add(k as u32 * 4);
            let i = blk.instrs[k];
            let f = dwrc_mips::flow(pc, i);
            if blk.end == End::Branch && k == n - 2 {
                self.branch(pc, i, f, blk.instrs[k + 1]);
                return;
            }
            let in_delay = false;
            if !self.instr(pc, i, in_delay) {
                return; // block ended with an exception
            }
            self.retire();
            k += 1;
        }
        // Fallthrough / mode change exit.
        let next = self.c32(blk.end_addr());
        self.normal_exit(next);
    }

    fn branch(&mut self, pc: u32, i: Instr, f: Flow, ds: Instr) {
        let s = self.get(i.rs());
        let t = self.get(i.rt());
        let fall = pc.wrapping_add(8);
        // Evaluate condition and target before the delay slot runs.
        let (cond, target): (Option<Value>, Value) = match (decode(i), f) {
            (Op::Beq, Flow::Branch { target, .. }) => (Some(self.b.ins().icmp(IntCC::Equal, s, t)), self.c32(target)),
            (Op::Bne, Flow::Branch { target, .. }) => (Some(self.b.ins().icmp(IntCC::NotEqual, s, t)), self.c32(target)),
            (Op::Blez, Flow::Branch { target, .. }) => (Some(self.b.ins().icmp_imm(IntCC::SignedLessThanOrEqual, s, 0)), self.c32(target)),
            (Op::Bgtz, Flow::Branch { target, .. }) => (Some(self.b.ins().icmp_imm(IntCC::SignedGreaterThan, s, 0)), self.c32(target)),
            (Op::Bltz | Op::Bltzal, Flow::Branch { target, .. }) => (Some(self.b.ins().icmp_imm(IntCC::SignedLessThan, s, 0)), self.c32(target)),
            (Op::Bgez | Op::Bgezal, Flow::Branch { target, .. }) => {
                (Some(self.b.ins().icmp_imm(IntCC::SignedGreaterThanOrEqual, s, 0)), self.c32(target))
            }
            (_, Flow::Jump { target, .. }) => (None, self.c32(target)),
            (_, Flow::JumpReg { .. }) => (None, s),
            _ => unreachable!("not a branch"),
        };
        // Link register is written by the branch itself.
        match decode(i) {
            Op::Jal | Op::Bltzal | Op::Bgezal => {
                let r = self.c32(fall);
                self.set(31, r);
            }
            Op::Jalr => {
                let r = self.c32(fall);
                self.set(i.rd(), r);
            }
            _ => {}
        }
        self.retire();
        // Delay slot.
        if !self.instr(pc.wrapping_add(4), ds, true) {
            return;
        }
        self.retire();
        let next = match cond {
            Some(c) => {
                let fv = self.c32(fall);
                self.b.ins().select(c, target, fv)
            }
            None => target,
        };
        self.normal_exit(next);
    }

    /// Translates one non-branch instruction. Returns false if it ended the
    /// block (unconditional exception).
    fn instr(&mut self, pc: u32, i: Instr, in_delay: bool) -> bool {
        let rs = i.rs();
        let rt = i.rt();
        let rd = i.rd();
        let op = decode(i);
        macro_rules! s {
            () => {
                self.get(rs)
            };
        }
        macro_rules! t {
            () => {
                self.get(rt)
            };
        }
        match op {
            Op::Sll => {
                let v = t!();
                let r = self.b.ins().ishl_imm(v, i.shamt() as i64);
                self.set(rd, r);
            }
            Op::Srl => {
                let v = t!();
                let r = self.b.ins().ushr_imm(v, i.shamt() as i64);
                self.set(rd, r);
            }
            Op::Sra => {
                let v = t!();
                let r = self.b.ins().sshr_imm(v, i.shamt() as i64);
                self.set(rd, r);
            }
            Op::Sllv | Op::Srlv | Op::Srav => {
                let a = t!();
                let sh = s!();
                let sh = self.b.ins().band_imm(sh, 31);
                let r = match op {
                    Op::Sllv => self.b.ins().ishl(a, sh),
                    Op::Srlv => self.b.ins().ushr(a, sh),
                    _ => self.b.ins().sshr(a, sh),
                };
                self.set(rd, r);
            }
            Op::Syscall => {
                self.exception_exit(exc::SYSCALL, pc, in_delay, 0, None);
                return false;
            }
            Op::Break => {
                self.exception_exit(exc::BP, pc, in_delay, 0, None);
                return false;
            }
            Op::Illegal => {
                self.exception_exit(exc::RI, pc, in_delay, 0, None);
                return false;
            }
            Op::CopUnusable => {
                self.exception_exit(exc::CPU, pc, in_delay, i.op() & 3, None);
                return false;
            }
            Op::Mfhi => {
                let v = self.call(helpers::mfhi as *const () as usize, self.sigs.r0, &[]).unwrap();
                self.set(rd, v);
            }
            Op::Mflo => {
                let v = self.call(helpers::mflo as *const () as usize, self.sigs.r0, &[]).unwrap();
                self.set(rd, v);
            }
            Op::Mthi => {
                let v = s!();
                self.call(helpers::mthi as *const () as usize, self.sigs.w1, &[v]);
            }
            Op::Mtlo => {
                let v = s!();
                self.call(helpers::mtlo as *const () as usize, self.sigs.w1, &[v]);
            }
            Op::Mult | Op::Multu | Op::Div | Op::Divu => {
                let a = s!();
                let b = t!();
                let f = match op {
                    Op::Mult => helpers::mult as *const () as usize,
                    Op::Multu => helpers::multu as *const () as usize,
                    Op::Div => helpers::div as *const () as usize,
                    _ => helpers::divu as *const () as usize,
                };
                self.call(f, self.sigs.w2, &[a, b]);
            }
            Op::Add | Op::Addi | Op::Sub => {
                let a = s!();
                let b = match op {
                    Op::Addi => self.c32(i.simm()),
                    _ => t!(),
                };
                let r = if op == Op::Sub { self.b.ins().isub(a, b) } else { self.b.ins().iadd(a, b) };
                let of = if op == Op::Sub {
                    let x = self.b.ins().bxor(a, b);
                    let y = self.b.ins().bxor(a, r);
                    self.b.ins().band(x, y)
                } else {
                    let x = self.b.ins().bxor(a, r);
                    let y = self.b.ins().bxor(b, r);
                    self.b.ins().band(x, y)
                };
                let ovf = self.b.ins().icmp_imm(IntCC::SignedLessThan, of, 0);
                self.exception_if(ovf, exc::OV, pc, in_delay, None);
                let dst = if op == Op::Addi { rt } else { rd };
                self.set(dst, r);
            }
            Op::Addu | Op::Subu | Op::And | Op::Or | Op::Xor | Op::Nor | Op::Slt | Op::Sltu => {
                let a = s!();
                let b = t!();
                let r = match op {
                    Op::Addu => self.b.ins().iadd(a, b),
                    Op::Subu => self.b.ins().isub(a, b),
                    Op::And => self.b.ins().band(a, b),
                    Op::Or => self.b.ins().bor(a, b),
                    Op::Xor => self.b.ins().bxor(a, b),
                    Op::Nor => {
                        let o = self.b.ins().bor(a, b);
                        self.b.ins().bnot(o)
                    }
                    Op::Slt => {
                        let c = self.b.ins().icmp(IntCC::SignedLessThan, a, b);
                        self.b.ins().uextend(types::I32, c)
                    }
                    _ => {
                        let c = self.b.ins().icmp(IntCC::UnsignedLessThan, a, b);
                        self.b.ins().uextend(types::I32, c)
                    }
                };
                self.set(rd, r);
            }
            Op::Addiu => {
                let a = s!();
                let r = self.b.ins().iadd_imm(a, i.simm() as i32 as i64);
                self.set(rt, r);
            }
            Op::Slti => {
                let a = s!();
                let c = self.b.ins().icmp_imm(IntCC::SignedLessThan, a, i.simm() as i32 as i64);
                let r = self.b.ins().uextend(types::I32, c);
                self.set(rt, r);
            }
            Op::Sltiu => {
                let a = s!();
                let c = self.b.ins().icmp_imm(IntCC::UnsignedLessThan, a, i.simm() as i32 as i64);
                let r = self.b.ins().uextend(types::I32, c);
                self.set(rt, r);
            }
            Op::Andi => {
                let a = s!();
                let r = self.b.ins().band_imm(a, i.imm() as i64);
                self.set(rt, r);
            }
            Op::Ori => {
                let a = s!();
                let r = self.b.ins().bor_imm(a, i.imm() as i64);
                self.set(rt, r);
            }
            Op::Xori => {
                let a = s!();
                let r = self.b.ins().bxor_imm(a, i.imm() as i64);
                self.set(rt, r);
            }
            Op::Lui => {
                let r = self.c32(i.imm() << 16);
                self.set(rt, r);
            }
            Op::Mfc0 => {
                if !matches!(rd, 3 | 5 | 6 | 7 | 8 | 9 | 11 | 12 | 13 | 14 | 15) {
                    self.exception_exit(exc::RI, pc, in_delay, 0, None);
                    return false;
                }
                let r = self.c32(rd as u32);
                let v = self.call(helpers::mfc0 as *const () as usize, self.sigs.r1, &[r]).unwrap();
                self.set_delayed(rt, v);
            }
            Op::Mtc0 => {
                let r = self.c32(rd as u32);
                let v = t!();
                self.call(helpers::mtc0 as *const () as usize, self.sigs.w2, &[r, v]);
            }
            Op::Rfe => {
                self.call(helpers::rfe as *const () as usize, self.sigs.v0, &[]);
            }
            Op::Mfc2 | Op::Cfc2 => {
                let r = self.c32(rd as u32);
                let f = if op == Op::Mfc2 { helpers::mfc2 as *const () as usize } else { helpers::cfc2 as *const () as usize };
                let v = self.call(f, self.sigs.r1, &[r]).unwrap();
                self.set_delayed(rt, v);
            }
            Op::Mtc2 | Op::Ctc2 => {
                let r = self.c32(rd as u32);
                let v = t!();
                let f = if op == Op::Mtc2 { helpers::mtc2 as *const () as usize } else { helpers::ctc2 as *const () as usize };
                self.call(f, self.sigs.w2, &[r, v]);
            }
            Op::Cop2Cmd => {
                let c = self.c32(i.0);
                self.call(helpers::gte_cmd as *const () as usize, self.sigs.w1, &[c]);
            }
            Op::Lwc2 | Op::Swc2 => {
                let base = s!();
                let addr = self.b.ins().iadd_imm(base, i.simm() as i32 as i64);
                let mis = self.misaligned(addr, 4);
                let code = if op == Op::Lwc2 { exc::ADEL } else { exc::ADES };
                self.exception_if(mis, code, pc, in_delay, Some(addr));
                let r = self.c32(rt as u32);
                let f = if op == Op::Lwc2 { helpers::lwc2 as *const () as usize } else { helpers::swc2 as *const () as usize };
                self.call(f, self.sigs.w2, &[addr, r]);
            }
            Op::Lb | Op::Lbu | Op::Lh | Op::Lhu | Op::Lw => {
                let base = s!();
                let addr = self.b.ins().iadd_imm(base, i.simm() as i32 as i64);
                let (bytes, signed) = match op {
                    Op::Lb => (1, true),
                    Op::Lbu => (1, false),
                    Op::Lh => (2, true),
                    Op::Lhu => (2, false),
                    _ => (4, false),
                };
                if bytes > 1 {
                    let mis = self.misaligned(addr, bytes);
                    self.exception_if(mis, exc::ADEL, pc, in_delay, Some(addr));
                }
                let v = self.mem_load(addr, bytes, signed);
                self.set_delayed(rt, v);
            }
            Op::Lwl | Op::Lwr => {
                let base = s!();
                let addr = self.b.ins().iadd_imm(base, i.simm() as i32 as i64);
                let aligned = self.b.ins().band_imm(addr, !3i64 & 0xFFFF_FFFF);
                let word = self.mem_load(aligned, 4, false);
                let cur = match self.load {
                    Some((lr, v)) if lr == rt && rt != 0 => v,
                    _ => self.get(rt),
                };
                let off = self.b.ins().band_imm(addr, 3);
                let off8 = self.b.ins().ishl_imm(off, 3);
                let v = if op == Op::Lwl {
                    // shift = 24 - off*8 ; mask = 0x00FFFFFF >> off*8
                    let c24 = self.c32(24);
                    let sh = self.b.ins().isub(c24, off8);
                    let w = self.b.ins().ishl(word, sh);
                    let m0 = self.c32(0x00FF_FFFF);
                    let m = self.b.ins().ushr(m0, off8);
                    let k = self.b.ins().band(cur, m);
                    self.b.ins().bor(k, w)
                } else {
                    // shift = off*8 ; mask = 0xFFFFFF00 << (24 - off*8)
                    let w = self.b.ins().ushr(word, off8);
                    let c24 = self.c32(24);
                    let sh = self.b.ins().isub(c24, off8);
                    let m0 = self.c32(0xFFFF_FF00);
                    let m = self.b.ins().ishl(m0, sh);
                    let k = self.b.ins().band(cur, m);
                    self.b.ins().bor(k, w)
                };
                self.set_delayed(rt, v);
            }
            Op::Sb | Op::Sh | Op::Sw => {
                let base = s!();
                let addr = self.b.ins().iadd_imm(base, i.simm() as i32 as i64);
                let bytes = match op {
                    Op::Sb => 1,
                    Op::Sh => 2,
                    _ => 4,
                };
                if bytes > 1 {
                    let mis = self.misaligned(addr, bytes);
                    self.exception_if(mis, exc::ADES, pc, in_delay, Some(addr));
                }
                let v = t!();
                self.mem_store(addr, v, bytes);
            }
            Op::Swl | Op::Swr => {
                let base = s!();
                let addr = self.b.ins().iadd_imm(base, i.simm() as i32 as i64);
                let v = t!();
                let f = if op == Op::Swl { helpers::swl as *const () as usize } else { helpers::swr as *const () as usize };
                self.call(f, self.sigs.w2, &[addr, v]);
            }
            Op::J | Op::Jal | Op::Jr | Op::Jalr | Op::Beq | Op::Bne | Op::Blez | Op::Bgtz | Op::Bltz | Op::Bgez | Op::Bltzal | Op::Bgezal => {
                unreachable!("branches are handled by `branch`")
            }
        }
        true
    }
}
