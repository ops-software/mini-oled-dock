//! One Tasks per session (F-T2, open point 19): the mutex, forwarding a Show to a running window, the unguarded owner.
//! Filled by TA-T6a (T6.1).
#![allow(dead_code, unused_variables)] // stub: removed by TA-T6a

use oled_trust::frame::tasks::Show;

/// Holds the single-instance mutex while it lives.
pub struct Guard;

pub enum Acquire {
    /// We own the name.
    Owner(Guard),
    /// The name is held and no window answered, and this is not a --from-chord start: a window of our own anyway.
    Unguarded,
    /// A running Tasks took the Show: exit 0.
    Forwarded,
    /// A running Tasks did not answer, or a --from-chord start found no window: exit 3.
    NoAnswer,
}

/// T6.1. Stub: Unguarded (no mutex).
pub fn acquire(show: &Show) -> Acquire {
    Acquire::Unguarded
}
