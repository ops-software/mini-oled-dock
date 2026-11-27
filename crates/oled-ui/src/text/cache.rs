//! The text run cache (P2-kit 5.3.10): laid-out runs and their coverage masks, least recently used out first, 512 KiB.

use std::collections::HashMap;
use std::rc::Rc;

use super::engine::{FontId, TextLayout};

/// The budget: mask bytes + text bytes + 64 per entry.
pub(crate) const BUDGET: usize = 512 * 1024;
const ENTRY_OVERHEAD: usize = 64;

/// A text run's coverage (0..=255, row-major).
#[derive(Debug)]
pub(crate) struct Mask {
    pub w: i32,
    pub h: i32,
    pub data: Vec<u8>,
}

struct Entry<T> {
    value: T,
    font: FontId,
    used: u64,
    cost: usize,
}

/// Layouts and masks under one byte budget.
pub(crate) struct RunCache {
    layouts: HashMap<u64, Entry<Rc<TextLayout>>>,
    masks: HashMap<u64, Entry<Rc<Mask>>>,
    clock: u64,
    bytes: usize,
    budget: usize,
}

/// FNV-1a 64 over byte slices.
pub(crate) fn fnv(parts: &[&[u8]]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in parts {
        for b in *p {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h ^= 0xff; // a separator, so ("ab", "c") and ("a", "bc") differ
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

impl RunCache {
    pub(crate) fn new() -> RunCache {
        RunCache::with_budget(BUDGET)
    }

    pub(crate) fn with_budget(budget: usize) -> RunCache {
        RunCache { layouts: HashMap::new(), masks: HashMap::new(), clock: 0, bytes: 0, budget }
    }

    /// Bytes counted against the budget.
    #[cfg(test)]
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }

    /// Entries held.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.layouts.len() + self.masks.len()
    }

    pub(crate) fn layout(&mut self, key: u64) -> Option<Rc<TextLayout>> {
        self.clock += 1;
        let e = self.layouts.get_mut(&key)?;
        e.used = self.clock;
        Some(Rc::clone(&e.value))
    }

    pub(crate) fn mask(&mut self, key: u64) -> Option<Rc<Mask>> {
        self.clock += 1;
        let e = self.masks.get_mut(&key)?;
        e.used = self.clock;
        Some(Rc::clone(&e.value))
    }

    pub(crate) fn put_layout(&mut self, key: u64, l: Rc<TextLayout>) {
        self.clock += 1;
        let cost = l.text.len() + l.lines.len() * 24 + ENTRY_OVERHEAD;
        let font = l.font;
        if let Some(old) = self.layouts.insert(key, Entry { value: l, font, used: self.clock, cost }) {
            self.bytes -= old.cost;
        }
        self.bytes += cost;
        self.trim();
    }

    pub(crate) fn put_mask(&mut self, key: u64, font: FontId, text_len: usize, m: Rc<Mask>) {
        self.clock += 1;
        let cost = m.data.len() + text_len + ENTRY_OVERHEAD;
        if let Some(old) = self.masks.insert(key, Entry { value: m, font, used: self.clock, cost }) {
            self.bytes -= old.cost;
        }
        self.bytes += cost;
        self.trim();
    }

    /// Drops every entry of the given fonts.
    pub(crate) fn evict_fonts(&mut self, fonts: &[FontId]) {
        let mut freed = 0;
        self.layouts.retain(|_, e| {
            let gone = fonts.contains(&e.font);
            if gone {
                freed += e.cost;
            }
            !gone
        });
        self.masks.retain(|_, e| {
            let gone = fonts.contains(&e.font);
            if gone {
                freed += e.cost;
            }
            !gone
        });
        self.bytes -= freed;
    }

    /// Evicts the least recently used entries until the budget holds.
    fn trim(&mut self) {
        while self.bytes > self.budget {
            let lo_l = self.layouts.iter().min_by_key(|(_, e)| e.used).map(|(k, e)| (*k, e.used));
            let lo_m = self.masks.iter().min_by_key(|(_, e)| e.used).map(|(k, e)| (*k, e.used));
            match (lo_l, lo_m) {
                (Some((k, ul)), Some((_, um))) if ul < um => {
                    self.bytes -= self.layouts.remove(&k).map_or(0, |e| e.cost);
                }
                (_, Some((k, _))) => {
                    self.bytes -= self.masks.remove(&k).map_or(0, |e| e.cost);
                }
                (Some((k, _)), None) => {
                    self.bytes -= self.layouts.remove(&k).map_or(0, |e| e.cost);
                }
                (None, None) => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mask(n: usize) -> Rc<Mask> {
        Rc::new(Mask { w: n as i32, h: 1, data: vec![1; n] })
    }

    #[test]
    fn least_recently_used_goes_first() {
        let mut c = RunCache::with_budget(1000);
        c.put_mask(1, FontId(0), 0, mask(400));
        c.put_mask(2, FontId(0), 0, mask(400));
        assert!(c.mask(1).is_some()); // 1 is now newer than 2
        c.put_mask(3, FontId(0), 0, mask(400)); // over budget: 2 goes
        assert!(c.mask(2).is_none());
        assert!(c.mask(1).is_some() && c.mask(3).is_some());
        assert!(c.bytes() <= 1000);
    }

    #[test]
    fn evict_fonts_frees_bytes() {
        let mut c = RunCache::new();
        c.put_mask(1, FontId(0), 3, mask(100));
        c.put_mask(2, FontId(1), 3, mask(100));
        c.evict_fonts(&[FontId(0)]);
        assert_eq!(c.len(), 1);
        assert_eq!(c.bytes(), 100 + 3 + ENTRY_OVERHEAD);
        c.evict_fonts(&[FontId(1)]);
        assert_eq!((c.len(), c.bytes()), (0, 0));
    }

    #[test]
    fn keys_depend_on_the_split() {
        assert_ne!(fnv(&[b"ab", b"c"]), fnv(&[b"a", b"bc"]));
        assert_eq!(fnv(&[b"x"]), fnv(&[b"x"]));
    }
}
