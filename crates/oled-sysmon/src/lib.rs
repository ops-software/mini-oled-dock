//! Process and system sampling shared by tasks and the dock's metrics module.
//! Readers (snapshot, cpu, mem, percore) and pure maths (table, ema, heat, fmt, ring). No UI, no words.
pub mod cpu;
pub mod ema;
pub mod error;
pub mod fmt;
pub mod heat;
pub mod mem;
pub mod percore;
pub mod ring;
pub mod snapshot;
pub mod table;
pub use error::SysmonError;
pub use snapshot::SnapshotReader;
pub use table::{diff, NameArena, NameId, ProcKey, ProcRow, ProcTable};
