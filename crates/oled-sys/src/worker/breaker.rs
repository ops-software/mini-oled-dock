//! The restart breaker of a worker (F-P4): after `max_restarts` failures within `window` it opens, and stays open
//! until the oldest of those failures is `window` old. Pure: the caller gives the instants. `max_restarts` 0 turns
//! it off (it never opens).

use std::collections::VecDeque;
use std::time::Instant;

use super::Breaker;

pub struct BreakerState {
    breaker: Breaker,
    /// The last `max_restarts` failures, oldest first.
    failures: VecDeque<Instant>,
}
impl BreakerState {
    pub fn new(b: Breaker) -> Self {
        BreakerState { breaker: b, failures: VecDeque::with_capacity(usize::from(b.max_restarts)) }
    }
    pub fn allow(&self, now: std::time::Instant) -> bool {
        !self.is_open(now)
    }
    pub fn failed(&mut self, now: std::time::Instant) {
        let keep = usize::from(self.breaker.max_restarts);
        if keep == 0 {
            return;
        }
        if self.failures.len() == keep {
            self.failures.pop_front();
        }
        self.failures.push_back(now);
    }
    pub fn is_open(&self, now: std::time::Instant) -> bool {
        let full = self.breaker.max_restarts > 0 && self.failures.len() >= usize::from(self.breaker.max_restarts);
        full && self.failures.front().is_some_and(|&oldest| now.saturating_duration_since(oldest) < self.breaker.window)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn breaker_opens_after_three_in_sixty() {
        let t0 = Instant::now();
        let s = |n: u64| t0 + Duration::from_secs(n);
        let mut b = BreakerState::new(Breaker::DEFAULT);
        assert!(b.allow(t0) && !b.is_open(t0));
        b.failed(s(0));
        b.failed(s(10));
        assert!(b.allow(s(11)), "two failures keep it closed");
        b.failed(s(50));
        assert!(b.is_open(s(50)), "three within 60 s open it");
        assert!(!b.allow(s(51)), "the fourth start is refused");
        assert!(b.is_open(s(59)), "open until the oldest failure is 60 s old");
        assert!(b.allow(s(60)), "closed once the oldest is 60 s old");
        b.failed(s(61));
        assert!(b.is_open(s(61)), "failures at 10, 50 and 61 are within 60 s");
        assert!(b.allow(s(70)), "the oldest (10) is 60 s old at 70");
        let mut spread = BreakerState::new(Breaker::DEFAULT);
        for n in [0, 40, 80, 120] {
            spread.failed(s(n));
            assert!(spread.allow(s(n)), "never three within 60 s");
        }
        let mut off = BreakerState::new(Breaker { max_restarts: 0, window: Duration::from_secs(60) });
        off.failed(s(0));
        assert!(off.allow(s(0)), "0 turns the breaker off");
    }
}
