//! A fixed-capacity ring of samples.

#[derive(Clone, Debug)]
pub struct Ring {
    buf: Box<[f32]>,
    /// Index of the next write.
    head: usize,
    len: usize,
}

impl Ring {
    /// A capacity of 0 is raised to 1.
    pub fn new(capacity: usize) -> Ring {
        Ring { buf: vec![0.0; capacity.max(1)].into_boxed_slice(), head: 0, len: 0 }
    }
    /// Overwrites the oldest sample when full.
    pub fn push(&mut self, v: f32) {
        self.buf[self.head] = v;
        self.head = (self.head + 1) % self.buf.len();
        self.len = (self.len + 1).min(self.buf.len());
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn capacity(&self) -> usize {
        self.buf.len()
    }
    /// 0 = newest.
    pub fn get(&self, back: usize) -> Option<f32> {
        if back >= self.len {
            return None;
        }
        let cap = self.buf.len();
        Some(self.buf[(self.head + cap - 1 - back) % cap])
    }
    /// Inclusive range of ages; peak-preserving resampling. Ages past the oldest sample are ignored; None when the
    /// whole range is past it.
    pub fn max_in(&self, back_from: usize, back_to: usize) -> Option<f32> {
        let (lo, hi) = if back_from <= back_to { (back_from, back_to) } else { (back_to, back_from) };
        if lo >= self.len {
            return None;
        }
        (lo..=hi.min(self.len - 1)).filter_map(|b| self.get(b)).reduce(f32::max)
    }
    pub fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_wraps() {
        let mut r = Ring::new(3);
        assert!(r.is_empty());
        for v in 1..=5 {
            r.push(v as f32);
        }
        assert_eq!((r.len(), r.capacity()), (3, 3));
        assert_eq!([r.get(0), r.get(1), r.get(2), r.get(3)], [Some(5.0), Some(4.0), Some(3.0), None]);
        r.clear();
        assert!(r.is_empty());
        assert_eq!(r.get(0), None);
        r.push(9.0);
        assert_eq!(r.get(0), Some(9.0));
        assert_eq!(Ring::new(0).capacity(), 1);
    }

    #[test]
    fn ring_get_by_age() {
        let mut r = Ring::new(4);
        r.push(1.0);
        r.push(2.0);
        assert_eq!((r.len(), r.get(0), r.get(1), r.get(2)), (2, Some(2.0), Some(1.0), None));
        r.push(3.0);
        r.push(4.0);
        assert_eq!((r.get(0), r.get(3)), (Some(4.0), Some(1.0)));
    }

    #[test]
    fn ring_max_in_range() {
        let mut r = Ring::new(5);
        for v in [1.0, 8.0, 2.0, 3.0] {
            r.push(v);
        }
        // Ages: 0 -> 3, 1 -> 2, 2 -> 8, 3 -> 1.
        assert_eq!(r.max_in(0, 1), Some(3.0));
        assert_eq!(r.max_in(0, 3), Some(8.0));
        assert_eq!(r.max_in(3, 3), Some(1.0));
        assert_eq!(r.max_in(3, 0), Some(8.0));
        assert_eq!(r.max_in(2, 100), Some(8.0));
        assert_eq!(r.max_in(4, 9), None);
        assert_eq!(Ring::new(2).max_in(0, 0), None);
    }
}
