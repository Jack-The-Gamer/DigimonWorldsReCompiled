//! The Re:Compiled static recompiler.
//!
//! Guest MIPS code from the player's disc is translated to native machine
//! code with Cranelift, ahead of execution: when execution first reaches a
//! code region, every block statically reachable from it (direct branches,
//! calls and return sites) is translated in one pass. Code that the game
//! loads later (overlays) is translated the same way when it is reached,
//! and translations are discarded automatically when the game overwrites
//! the memory they came from.
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

pub use engine::RecompEngine;

use dwrc_psx::ExecEngine;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Run translated native code.
    Native,
    /// Run translated code and verify each block against the interpreter.
    Validate,
}

pub fn new_engine(mode: Mode) -> Box<dyn ExecEngine> {
    Box::new(RecompEngine::new(mode == Mode::Validate))
}
