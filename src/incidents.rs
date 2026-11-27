//! Incidents the suite records for Settings' Safety page (dock-suite 2.7).
//!
//! Declared as a child module of `ui` (`#[path]`).

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IncidentKind {
    TakeoverFellBack,
    TakeoverPaused,
    /// A rolled-back update: `update\<tag>\rolled-back` exists (the guard's mark); not pushed, read by the Safety
    /// page's worker.
    UpdateRolledBack(crate::update_release::Version),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Incident {
    pub at_unix: u64,
    pub kind: IncidentKind,
}

/// How many incidents are kept.
pub(crate) const INCIDENTS_MAX: usize = 10;

/// The ring, oldest first; it takes its memory on the first push.
static RING: Mutex<VecDeque<Incident>> = Mutex::new(VecDeque::new());

/// Appends to `ring`, dropping the oldest beyond `INCIDENTS_MAX`.
fn push_into(ring: &mut VecDeque<Incident>, incident: Incident) {
    if ring.len() == INCIDENTS_MAX {
        ring.pop_front();
    }
    ring.push_back(incident);
}

/// Any thread.
pub(crate) fn push(kind: IncidentKind) {
    let at_unix = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    push_into(&mut RING.lock().unwrap_or_else(|p| p.into_inner()), Incident { at_unix, kind });
}

/// Newest first.
pub(crate) fn recent() -> Vec<Incident> {
    RING.lock().unwrap_or_else(|p| p.into_inner()).iter().rev().copied().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_keeps_ten_newest_first() {
        let mut ring = VecDeque::new();
        for at_unix in 1..=12 {
            push_into(&mut ring, Incident { at_unix, kind: IncidentKind::TakeoverPaused });
        }
        assert_eq!(ring.len(), INCIDENTS_MAX);
        let newest_first: Vec<u64> = ring.iter().rev().map(|i| i.at_unix).collect();
        assert_eq!(newest_first, [12, 11, 10, 9, 8, 7, 6, 5, 4, 3]);
    }
}
