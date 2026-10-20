//! The Re:Compiled static recompiler.
//!
//! Guest MIPS code from the player's disc is translated to native machine
//! code with Cranelift, ahead of execution: when execution first reaches a
//! code region, every block statically reachable from it (direct branches,
//! calls and return sites) is queued for translation on a worker thread,
//! and blocks run on the interpreter, with identical results, until their
//! translation is ready. Code that the game loads later (overlays) is
//! translated the same way when it is reached, translations are discarded
//! automatically when the game overwrites the memory they came from, and
//! code loaded again is not translated again.
//!
//! Because translation happens on the player's machine, no game code is
//! ever distributed, and no compiler toolchain is required.
//!
//! The translated code runs against exactly the same hardware model as the
//! reference interpreter (`dwrc-psx`), and `Mode::Validate` checks every
//! translated block against the interpreter instruction by instruction.

pub mod block;
pub mod codegen;
pub mod engine;
pub mod helpers;
pub mod validate;

pub use engine::{RecompEngine, Translation};

use dwrc_psx::ExecEngine;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Run translated native code.
    Native,
    /// Run translated code and verify each block against the interpreter.
    Validate,
}

/// The engine for `mode`: check mode translates each block before it first
/// runs, the native engine translates in the background.
pub fn new_engine(mode: Mode) -> Box<dyn ExecEngine> {
    match mode {
        Mode::Native => new_engine_with(mode, Translation::Background),
        Mode::Validate => new_engine_with(mode, Translation::Inline),
    }
}

pub fn new_engine_with(mode: Mode, translation: Translation) -> Box<dyn ExecEngine> {
    Box::new(RecompEngine::new(mode == Mode::Validate, translation))
}
