//! The navigation cache (P3-files-app 2.2, F4.3): the listings of folders left recently, one cache per process, kept
//! within the memory budget and revalidated on a hit. The window puts the listing it leaves (window::show_new,
//! flush_hit) and takes the one it enters (window::navigate, Back, Forward, Up and the history menus). A hit is shown
//! at once and merged by name with a fresh enumeration.

use oled_trust::frame::files::Location;
use oled_ui::model::SortDir;

use crate::listing::Anchor;
use crate::model::{ColKind, Listing};
use crate::window::Shared;

/// The cache holds the listings of at most this many folders...
pub const MAX_LISTINGS: usize = 16;
/// ...and at most this many bytes of Listing::heap_bytes() together (the first thing dropped under memory pressure).
pub const MAX_BYTES: usize = 4 * 1024 * 1024;

/// Least recently put first.
#[derive(Default)]
pub struct NavCache {
    entries: Vec<(Location, Cached)>,
}

impl std::fmt::Debug for NavCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "NavCache({} listings, {} bytes)", self.entries.len(), self.heap_bytes())
    }
}

pub struct Cached {
    pub listing: Box<dyn Listing>,
    pub anchor: Option<Anchor>,
    pub sort: (ColKind, SortDir),
}

impl NavCache {
    pub fn new() -> NavCache {
        NavCache::default()
    }

    /// The heap the cached listings hold together.
    pub fn heap_bytes(&self) -> usize {
        self.entries.iter().map(|(_, c)| c.listing.heap_bytes()).sum()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn contains(&self, at: &Location) -> bool {
        self.entries.iter().any(|(l, _)| l == at)
    }

    /// Keeps `c` as the newest entry for `at` (an older copy of the same folder goes), then drops the least recently put
    /// ones while there are more than MAX_LISTINGS or more than MAX_BYTES of heap.
    fn put(&mut self, at: &Location, c: Cached) {
        self.entries.retain(|(l, _)| l != at);
        self.entries.push((at.clone(), c));
        let mut bytes = self.heap_bytes();
        while self.entries.len() > MAX_LISTINGS || (bytes > MAX_BYTES && !self.entries.is_empty()) {
            let (_, gone) = self.entries.remove(0);
            bytes -= gone.listing.heap_bytes().min(bytes);
        }
    }

    /// The listing of `at`, removed from the cache (it is shown now and goes back when the tab leaves it).
    fn take(&mut self, at: &Location) -> Option<Cached> {
        let i = self.entries.iter().position(|(l, _)| l == at)?;
        Some(self.entries.remove(i).1)
    }

    fn clear(&mut self) {
        self.entries.clear();
    }
}

/// Keeps the listing a tab left, with the row it was scrolled to and its sort.
pub fn put(shared: &Shared, at: &Location, c: Cached) {
    shared.navcache.borrow_mut().put(at, c);
}

/// The cached listing of `at`, if there is one; it leaves the cache.
pub fn take(shared: &Shared, at: &Location) -> Option<Cached> {
    shared.navcache.borrow_mut().take(at)
}

/// Memory pressure: the cache is the first thing to go.
pub fn clear(shared: &Shared) {
    shared.navcache.borrow_mut().clear();
}
