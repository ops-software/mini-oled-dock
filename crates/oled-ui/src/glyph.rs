//! The kit's glyph set (F-K1, P2-kit 2.3): today's shapes of gfx/glyphs.rs and the new ones, and the glyph cache.

use std::collections::HashMap;
use std::rc::Rc;

use crate::w3::{glyph_hit_counts, glyphs as g};

/// A monochrome icon drawn from a shape function on the 16 x 16 grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Glyph {
    // Today's shapes (gfx/glyphs.rs after W3).
    ChevronUp,
    ChevronDown,
    ChevronLeft,
    ChevronRight,
    Close,
    Check,
    Gear,
    Info,
    Refresh,
    Folder,
    Document,
    Image,
    Archive,
    Executable,
    Copy,
    Globe,
    Clock,
    Download,
    Lock,
    Power,
    Restart,
    Pencil,
    Eye,
    EyeOff,
    Grid,
    List,
    Monitor,
    Window,
    Prompt,
    // New shapes (K1a.3).
    Search,
    Filter,
    More,
    Pin,
    Cut,
    Paste,
    Delete,
    SortUp,
    SortDown,
    Expander,
    Cpu,
    Memory,
    Disk,
    Network,
    Shield,
    Minimize,
    Maximize,
    Restore,
    Plus,
    Star,
    Cloud,
    Link,
    Error,
    // Row motion and sort outlines (TK2).
    Pause,
    SortUpOutline,
    SortDownOutline,
}

impl Glyph {
    /// Every glyph, in declaration order.
    pub const ALL: [Glyph; 55] = [
        Glyph::ChevronUp,
        Glyph::ChevronDown,
        Glyph::ChevronLeft,
        Glyph::ChevronRight,
        Glyph::Close,
        Glyph::Check,
        Glyph::Gear,
        Glyph::Info,
        Glyph::Refresh,
        Glyph::Folder,
        Glyph::Document,
        Glyph::Image,
        Glyph::Archive,
        Glyph::Executable,
        Glyph::Copy,
        Glyph::Globe,
        Glyph::Clock,
        Glyph::Download,
        Glyph::Lock,
        Glyph::Power,
        Glyph::Restart,
        Glyph::Pencil,
        Glyph::Eye,
        Glyph::EyeOff,
        Glyph::Grid,
        Glyph::List,
        Glyph::Monitor,
        Glyph::Window,
        Glyph::Prompt,
        Glyph::Search,
        Glyph::Filter,
        Glyph::More,
        Glyph::Pin,
        Glyph::Cut,
        Glyph::Paste,
        Glyph::Delete,
        Glyph::SortUp,
        Glyph::SortDown,
        Glyph::Expander,
        Glyph::Cpu,
        Glyph::Memory,
        Glyph::Disk,
        Glyph::Network,
        Glyph::Shield,
        Glyph::Minimize,
        Glyph::Maximize,
        Glyph::Restore,
        Glyph::Plus,
        Glyph::Star,
        Glyph::Cloud,
        Glyph::Link,
        Glyph::Error,
        Glyph::Pause,
        Glyph::SortUpOutline,
        Glyph::SortDownOutline,
    ];

    /// The glyph's shape on the 16 x 16 grid. The match is exhaustive: a new variant without a shape does not compile.
    pub fn shape(self) -> fn(f32, f32) -> bool {
        match self {
            Glyph::ChevronUp | Glyph::SortUp => g::chevron_up_shape,
            Glyph::ChevronDown | Glyph::SortDown => g::chevron_down_shape,
            Glyph::ChevronLeft => g::chevron_left_shape,
            Glyph::ChevronRight | Glyph::Expander => g::chevron_right_shape,
            Glyph::Close => g::close_shape,
            Glyph::Check => g::check_shape,
            Glyph::Gear => g::gear_shape,
            Glyph::Info => g::info_shape,
            Glyph::Refresh => g::refresh_shape,
            Glyph::Folder => g::folder_shape,
            Glyph::Document => g::document_shape,
            Glyph::Image => g::image_shape,
            Glyph::Archive => g::archive_shape,
            Glyph::Executable => g::executable_shape,
            Glyph::Copy => g::copy_shape,
            Glyph::Globe => g::globe_shape,
            Glyph::Clock => g::clock_shape,
            Glyph::Download => g::download_shape,
            Glyph::Lock => g::lock_shape,
            Glyph::Power => g::power_shape,
            Glyph::Restart => g::restart_shape,
            Glyph::Pencil => g::pencil_shape,
            Glyph::Eye => |u, v| g::eye_shape(u, v, false),
            Glyph::EyeOff => |u, v| g::eye_shape(u, v, true),
            Glyph::Grid => g::grid_shape,
            Glyph::List => g::list_shape,
            Glyph::Monitor => g::monitor_shape,
            Glyph::Window => g::window_shape,
            Glyph::Prompt => g::prompt_shape,
            Glyph::Search => g::search_shape,
            Glyph::Filter => g::filter_shape,
            Glyph::More => g::more_shape,
            Glyph::Pin => g::pin_shape,
            Glyph::Cut => g::cut_shape,
            Glyph::Paste => g::paste_shape,
            Glyph::Delete => g::delete_shape,
            Glyph::Cpu => g::cpu_shape,
            Glyph::Memory => g::memory_shape,
            Glyph::Disk => g::disk_shape,
            Glyph::Network => g::ethernet_shape,
            Glyph::Shield => g::shield_shape,
            Glyph::Minimize => g::minimize_shape,
            Glyph::Maximize => g::maximize_shape,
            Glyph::Restore => g::restore_shape,
            Glyph::Plus => g::plus_shape,
            Glyph::Star => g::star_shape,
            Glyph::Cloud => g::cloud_shape,
            Glyph::Link => g::link_shape,
            Glyph::Error => g::error_shape,
            Glyph::Pause => g::pause_shape,
            Glyph::SortUpOutline => g::sort_up_outline_shape,
            Glyph::SortDownOutline => g::sort_down_outline_shape,
        }
    }
}

/// The glyph cache's budget (P2-kit 4: private, kit parts).
const CACHE_BUDGET: usize = 256 * 1024;

/// One cached grid and when it was last used.
struct Cached {
    hits: Rc<[u8]>,
    used: u64,
}

/// The 4 x 4 hit counts of each (glyph, size) drawn so far, least recently used out first, 256 KiB.
pub(crate) struct GlyphCache {
    map: HashMap<(Glyph, i32), Cached>,
    clock: u64,
    bytes: usize,
    /// How many hit-count grids were computed (a test reads it).
    pub(crate) computed: u64,
}

impl GlyphCache {
    pub(crate) fn new() -> GlyphCache {
        GlyphCache { map: HashMap::new(), clock: 0, bytes: 0, computed: 0 }
    }

    /// The hit counts of `glyph` in a `px` square, computed once.
    pub(crate) fn hits(&mut self, glyph: Glyph, px: i32) -> Rc<[u8]> {
        self.clock += 1;
        if let Some(c) = self.map.get_mut(&(glyph, px)) {
            c.used = self.clock;
            return Rc::clone(&c.hits);
        }
        let hits: Rc<[u8]> = glyph_hit_counts(px, &glyph.shape()).into();
        self.computed += 1;
        self.bytes += hits.len();
        self.map.insert((glyph, px), Cached { hits: Rc::clone(&hits), used: self.clock });
        while self.bytes > CACHE_BUDGET {
            let Some(oldest) =
                self.map.iter().filter(|(k, _)| **k != (glyph, px)).min_by_key(|(_, c)| c.used).map(|(k, _)| *k)
            else {
                break;
            };
            if let Some(c) = self.map.remove(&oldest) {
                self.bytes -= c.hits.len();
            }
        }
        hits
    }

    /// Drops every entry (WM_DPICHANGED: the pixel sizes change with the DPI).
    #[allow(dead_code)] // called by the host's DPI handler (K1a.10)
    pub(crate) fn clear(&mut self) {
        self.map.clear();
        self.bytes = 0;
    }

    #[cfg(test)]
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_glyph_has_a_shape() {
        // The exhaustive match in shape() is the compile-time half; this is the runtime one: ALL lists each variant once
        // and every shape draws something at 32 px.
        let mut seen = std::collections::HashSet::new();
        for g in Glyph::ALL {
            assert!(seen.insert(g), "{g:?} listed twice");
            let hits = glyph_hit_counts(32, &g.shape());
            assert!(hits.iter().any(|h| *h > 0), "{g:?} draws nothing");
        }
        assert_eq!(seen.len(), 55);
    }

    #[test]
    fn glyph_cache_reuses_hits() {
        let mut c = GlyphCache::new();
        let a = c.hits(Glyph::Search, 16);
        assert_eq!(c.computed, 1);
        let b = c.hits(Glyph::Search, 16);
        assert_eq!(c.computed, 1);
        assert!(Rc::ptr_eq(&a, &b));
        c.hits(Glyph::Search, 24);
        c.hits(Glyph::Plus, 16);
        assert_eq!(c.computed, 3);
    }

    #[test]
    fn glyph_cache_bounded() {
        let mut c = GlyphCache::new();
        let sizes = [64, 72, 80, 88, 96, 104, 112, 120, 128, 136];
        for (i, px) in sizes.iter().enumerate() {
            for g in Glyph::ALL.iter().take(8 + i) {
                c.hits(*g, *px);
            }
        }
        assert!(c.bytes() <= CACHE_BUDGET, "{}", c.bytes());
        let before = c.computed;
        c.hits(Glyph::Search, 136);
        c.hits(Glyph::Search, 136);
        assert!(c.computed <= before + 1);
    }
}
