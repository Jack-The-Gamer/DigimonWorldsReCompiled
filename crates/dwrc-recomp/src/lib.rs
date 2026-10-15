//! Static recompiler (being implemented).
use dwrc_psx::{ExecEngine, Interpreter};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode { Native, Validate }

pub fn new_engine(_mode: Mode) -> Box<dyn ExecEngine> {
    Box::new(Interpreter)
}
