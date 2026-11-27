//! The ops host's parts (SPEC-files 5.5.1, 5.5.9, 5.4.7): the journal, the flags, the item checks and the recycle
//! classifier; then the runner, sink, pre-flight, coalescer, constants, undo, request dispatch and session handling.

pub mod binrec;
pub mod checks;
pub mod classify;
pub mod coalesce;
pub mod consts;
pub mod flags;
pub mod host;
pub mod journal;
pub mod preflight;
pub mod resume;
pub mod runner;
pub mod session;
pub mod sink;
pub mod undo;

pub use classify::{classify_facts, gather_facts, BinFacts, NoBin, Recyclability};
