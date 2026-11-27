//! The ops host's parts (SPEC-files 5.5.1, 5.5.9, 5.4.7): the recycle classifier today; F-OPS adds the journal,
//! flags, checks, runner, sink, pre-flight, coalescer, constants, undo, request dispatch and session handling here.

pub mod classify;

pub use classify::{classify_facts, gather_facts, BinFacts, NoBin, Recyclability};
