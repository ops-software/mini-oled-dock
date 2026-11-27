//! The navigation cache (P3-files-app 2.2; FD-TABS fills it, F4.3): listings of folders left recently, kept within
//! the memory budget and revalidated on a hit.

use oled_trust::frame::files::Location;
use oled_ui::model::SortDir;

use crate::listing::Anchor;
use crate::model::{ColKind, Listing};
use crate::window::Shared;

#[derive(Debug, Default)]
pub struct NavCache;

pub struct Cached {
    pub listing: Box<dyn Listing>,
    pub anchor: Option<Anchor>,
    pub sort: (ColKind, SortDir),
}

impl NavCache {
    pub fn new() -> NavCache {
        NavCache
    }

    pub fn heap_bytes(&self) -> usize {
        0
    }
}

/// Filled by FD-TABS; until then the listing is dropped.
pub fn put(shared: &Shared, at: &Location, c: Cached) {
    let _ = (shared, at, c);
}

/// Filled by FD-TABS; until then None.
pub fn take(shared: &Shared, at: &Location) -> Option<Cached> {
    let _ = (shared, at);
    None
}

/// Filled by FD-TABS; until then nothing.
pub fn clear(shared: &Shared) {
    let _ = shared;
}
