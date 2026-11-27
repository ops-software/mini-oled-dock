//! Breadcrumbs shared with the client: which extension call each STA is in, and the busy and KILLING flags of the
//! kill protocol (SH3). Filled by F-SH.
// Read by another process: no unwrap, no expect, no indexing.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use oled_trust::frame::files::CallKind;

/// The STA binds its slot once.
pub fn bind(slot: u8) {
    let _ = slot;
    todo!("F-SH")
}

/// Every extension call runs inside an enter guard.
pub fn enter(kind: CallKind, ext: &str) -> CallGuard {
    let _ = (kind, ext);
    todo!("F-SH")
}

pub struct CallGuard {
    _private: (),
}

/// Every invoke runs inside a busy guard; fails once a killer has set KILLING.
pub fn busy_enter() -> Result<BusyGuard, Killing> {
    todo!("F-SH")
}

pub struct BusyGuard {
    _private: (),
}

/// A kill decision was taken: no invoke may start.
pub struct Killing;
