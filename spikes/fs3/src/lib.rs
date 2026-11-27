//! Spikes FS-3 and FS-6 of the files engine: the pieces the probes share. Never shipped and never merged; the probes
//! answer questions the ops host and the icon cache need before they are built (records in the design folder).

pub mod exts;
pub mod guard;
pub mod json;
pub mod rules;
pub mod sink;
pub mod trust;
pub mod util;
pub mod watch;
