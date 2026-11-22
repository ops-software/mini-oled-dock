//! Stable sorts for short lists (tray icons, window groups, notification entries, monitors): an insertion sort, a few
//! instructions per element type, with no allocation. The standard library's stable sort is a merge sort that the release
//! build instantiates once per element type and comparison (about 4 KiB of code each). The result is the same: both sorts
//! are stable and use only the comparison. Not for lists that can hold thousands of elements (the work grows with the
//! square of the number of elements that are out of order).

use std::cmp::Ordering;

/// `v.sort_by(cmp)`, stable.
pub fn sort_by<T>(v: &mut [T], mut cmp: impl FnMut(&T, &T) -> Ordering) {
    for i in 1..v.len() {
        let mut j = i;
        while j > 0 && cmp(&v[j - 1], &v[j]) == Ordering::Greater {
            v.swap(j - 1, j);
            j -= 1;
        }
    }
}

/// `v.sort_by_key(key)`, stable.
pub fn sort_by_key<T, K: Ord>(v: &mut [T], mut key: impl FnMut(&T) -> K) {
    sort_by(v, |a, b| key(a).cmp(&key(b)));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small deterministic generator: the tests need no random crate.
    fn next(seed: &mut u64) -> u64 {
        *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        *seed >> 33
    }

    #[test]
    fn empty_and_single_lists_are_left_alone() {
        let mut none: Vec<u8> = Vec::new();
        sort_by(&mut none, |a, b| a.cmp(b));
        assert!(none.is_empty());
        let mut one = [7u8];
        sort_by_key(&mut one, |x| *x);
        assert_eq!(one, [7]);
    }

    #[test]
    fn gives_the_standard_sorts_result_on_many_lists_with_ties() {
        let mut seed = 42;
        for len in 0..80usize {
            for _ in 0..20 {
                // (key with many ties, original place): a stable sort keeps the places of equal keys in order
                let list: Vec<(u8, usize)> = (0..len).map(|i| ((next(&mut seed) % 6) as u8, i)).collect();
                let mut want = list.clone();
                want.sort_by_key(|e| e.0);
                let mut got = list.clone();
                sort_by_key(&mut got, |e| e.0);
                assert_eq!(got, want, "by key, {len} elements");
                let mut want = list.clone();
                want.sort_by(|a, b| b.0.cmp(&a.0));
                let mut got = list;
                sort_by(&mut got, |a, b| b.0.cmp(&a.0));
                assert_eq!(got, want, "by comparison, {len} elements");
            }
        }
    }

    #[test]
    fn a_sorted_list_costs_one_comparison_per_neighbour() {
        let mut list: Vec<u32> = (0..100).collect();
        let mut calls = 0;
        sort_by(&mut list, |a, b| {
            calls += 1;
            a.cmp(b)
        });
        assert_eq!(calls, 99);
    }
}
