//! Test kit of the launcher: a seeded generator and a counting allocator (test builds only).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

/// xorshift64: tiny, deterministic, good enough for fixtures. A zero seed would stay zero, so it is replaced.
pub struct XorShift(u64);

impl XorShift {
    pub fn new(seed: u64) -> Self {
        XorShift(if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed })
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// A uniform-enough pick from a non-empty slice.
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        let i = (self.next_u64() % items.len() as u64) as usize;
        &items[i]
    }

    /// A string of 0..=max_len characters mixing digits, operators, letters, spaces, separators, quotes,
    /// control characters and bidi marks. Surrogates cannot occur in a `String`.
    pub fn ascii_or_bmp(&mut self, max_len: usize) -> String {
        const SPECIAL: [char; 14] =
            ['+', '-', '*', '/', '(', ')', ',', '.', '"', '\'', '\u{0}', '\u{7}', '\u{202E}', '\u{FEFF}'];
        let len = (self.next_u64() % (max_len as u64 + 1)) as usize;
        let mut s = String::with_capacity(len);
        for _ in 0..len {
            let c = match self.next_u64() % 8 {
                0 | 1 => char::from(b'0' + (self.next_u64() % 10) as u8),
                2 | 3 => char::from(b'a' + (self.next_u64() % 26) as u8),
                4 => char::from(b'A' + (self.next_u64() % 26) as u8),
                5 => ' ',
                6 => *self.pick(&SPECIAL),
                _ => char::from_u32(0x00A0 + (self.next_u64() % 0xF000) as u32).unwrap_or('?'),
            };
            s.push(c);
        }
        s
    }
}

thread_local! {
    static COUNT: Cell<u64> = const { Cell::new(0) };
    /// Bytes live now and the highest value reached, both relative to the thread's start (a block freed on another
    /// thread than the one that made it makes `LIVE` negative, which is why it is signed).
    static LIVE: Cell<i64> = const { Cell::new(0) };
    static PEAK: Cell<i64> = const { Cell::new(0) };
}

/// Counts the allocations of the calling thread (other test threads do not disturb a measurement).
pub struct Counting;

fn bump() {
    let _ = COUNT.try_with(|c| c.set(c.get() + 1));
}

fn grow(bytes: usize) {
    let bytes = i64::try_from(bytes).unwrap_or(i64::MAX);
    let _ = LIVE.try_with(|l| {
        let now = l.get().saturating_add(bytes);
        l.set(now);
        let _ = PEAK.try_with(|p| p.set(p.get().max(now)));
    });
}

fn shrink(bytes: usize) {
    let bytes = i64::try_from(bytes).unwrap_or(i64::MAX);
    let _ = LIVE.try_with(|l| l.set(l.get().saturating_sub(bytes)));
}

// SAFETY: every call is forwarded unchanged to `System`; the counter is a thread-local `Cell` that never allocates.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        bump();
        grow(layout.size());
        System.alloc(layout)
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        bump();
        grow(layout.size());
        System.alloc_zeroed(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        shrink(layout.size());
        System.dealloc(ptr, layout)
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        bump();
        shrink(layout.size());
        grow(new_size);
        System.realloc(ptr, layout, new_size)
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// Runs `f` and returns its result with the number of allocations the calling thread made meanwhile.
pub fn allocations_during<R>(f: impl FnOnce() -> R) -> (R, u64) {
    let before = COUNT.with(|c| c.get());
    let r = f();
    let after = COUNT.with(|c| c.get());
    (r, after - before)
}

/// Runs `f` and returns its result with the most bytes the calling thread held live above where it started (the
/// result itself counts while it is alive at the end).
pub fn peak_bytes_during<R>(f: impl FnOnce() -> R) -> (R, usize) {
    let start = LIVE.with(|l| l.get());
    let outer = PEAK.with(|p| p.get());
    PEAK.with(|p| p.set(start));
    let r = f();
    let peak = PEAK.with(|p| p.get());
    // An enclosing measurement keeps its own high-water mark.
    PEAK.with(|p| p.set(p.get().max(outer)));
    (r, usize::try_from(peak.saturating_sub(start)).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peak_counts_the_high_water_mark_not_the_end() {
        let ((), peak) = peak_bytes_during(|| {
            let big = vec![1u8; 1 << 20];
            std::hint::black_box(&big);
            drop(big);
            let small = vec![2u8; 1 << 10];
            std::hint::black_box(&small);
        });
        assert!((1 << 20..(1 << 20) + 4096).contains(&peak), "{peak}");
        let ((), none) = peak_bytes_during(|| ());
        assert_eq!(none, 0);
    }

    #[test]
    fn xorshift_is_deterministic() {
        let mut r = XorShift::new(0x5EED);
        let got: Vec<u64> = (0..5).map(|_| r.next_u64()).collect();
        assert_eq!(
            got,
            [
                0x0000_1794_81EA_4510,
                0xC7BA_5713_31EE_D59A,
                0x8EDB_EC5A_EBE9_2EB1,
                0xD23D_1303_60FB_70AC,
                0xFB79_A804_6C69_2D4D
            ]
        );
    }

    #[test]
    fn counting_allocator_counts_a_box() {
        let (b, n) = allocations_during(|| Box::new(1));
        assert_eq!(n, 1);
        assert_eq!(*b, 1);
        let (_, none) = allocations_during(|| 1 + 1);
        assert_eq!(none, 0);
    }

    #[test]
    fn generators_respect_their_bounds() {
        let mut r = XorShift::new(7);
        for _ in 0..200 {
            assert!(r.ascii_or_bmp(12).chars().count() <= 12);
        }
        let items = [1, 2, 3];
        for _ in 0..50 {
            assert!(items.contains(r.pick(&items)));
        }
        assert_ne!(XorShift::new(0).next_u64(), 0);
    }
}
