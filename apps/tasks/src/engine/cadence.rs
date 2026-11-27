//! When the sampler wakes.
use crate::engine::need::NeedSet;

pub const IDLE_AFTER_MS: u64 = 30_000;
pub const SLOW_TICK_US: u32 = 8_000;
pub const FAST_TICK_US: u32 = 4_000;
/// Consecutive ticks over SLOW_TICK_US that turn the slow fallback on.
const SLOW_ON_TICKS: u8 = 5;
/// Consecutive ticks at or under FAST_TICK_US that turn it off.
const SLOW_OFF_TICKS: u8 = 30;
/// Longest wake-up spacing of a background, idle or slow sampler.
const BACKGROUND_MS: u32 = 2000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Refresh {
    Half,
    #[default]
    One,
    Two,
    Five,
    Paused,
}

impl Refresh {
    /// 500, 1000, 2000, 5000, None.
    pub fn ms(self) -> Option<u32> {
        match self {
            Refresh::Half => Some(500),
            Refresh::One => Some(1000),
            Refresh::Two => Some(2000),
            Refresh::Five => Some(5000),
            Refresh::Paused => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Control {
    pub visible: bool,
    pub foreground: bool,
    pub minimised_or_cloaked: bool,
    pub display_off: bool,
    pub refresh: Refresh,
    pub idle: bool,
    pub need: NeedSet,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Slow {
    slow: bool,
    over: u8,
    under: u8,
}

impl Slow {
    /// Counts consecutive slow and consecutive fast ticks; a tick between the two limits breaks both runs.
    pub fn observe(&mut self, tick_us: u32) -> bool {
        if tick_us > SLOW_TICK_US {
            self.over = self.over.saturating_add(1);
            self.under = 0;
        } else if tick_us <= FAST_TICK_US {
            self.under = self.under.saturating_add(1);
            self.over = 0;
        } else {
            self.over = 0;
            self.under = 0;
        }
        if !self.slow && self.over >= SLOW_ON_TICKS {
            self.slow = true;
            self.over = 0;
        } else if self.slow && self.under >= SLOW_OFF_TICKS {
            self.slow = false;
            self.under = 0;
        }
        self.slow
    }
    pub fn is_slow(&self) -> bool {
        self.slow
    }
}

/// None = stopped (no wake-up at all).
pub fn period_ms(c: &Control, slow: bool) -> Option<u32> {
    if !c.visible || c.minimised_or_cloaked || c.display_off || c.need.is_empty() {
        return None;
    }
    let mut p = c.refresh.ms()?;
    if !c.foreground {
        p = p.max(BACKGROUND_MS);
    }
    if c.idle || slow {
        p = p.max(BACKGROUND_MS);
    }
    Some(p)
}

pub fn idle(last_input_ms: u64, now_ms: u64, calm: bool) -> bool {
    calm && now_ms.saturating_sub(last_input_ms) >= IDLE_AFTER_MS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::need::Need;

    fn live() -> Control {
        Control {
            visible: true,
            foreground: true,
            minimised_or_cloaked: false,
            display_off: false,
            refresh: Refresh::One,
            idle: false,
            need: NeedSet::EMPTY.with(Need::Procs),
        }
    }

    #[test]
    fn period_table() {
        let c = live();
        assert_eq!(period_ms(&c, false), Some(1000));
        for (r, want) in [
            (Refresh::Half, Some(500)),
            (Refresh::One, Some(1000)),
            (Refresh::Two, Some(2000)),
            (Refresh::Five, Some(5000)),
            (Refresh::Paused, None),
        ] {
            assert_eq!(period_ms(&Control { refresh: r, ..c }, false), want, "{r:?}");
        }
        assert_eq!(period_ms(&Control { visible: false, ..c }, false), None);
        assert_eq!(period_ms(&Control { minimised_or_cloaked: true, ..c }, false), None);
        assert_eq!(period_ms(&Control { display_off: true, ..c }, false), None);
        assert_eq!(period_ms(&Control { need: NeedSet::EMPTY, ..c }, false), None);
        // Background, idle and slow lift a short period to 2 s and leave a longer one alone.
        assert_eq!(period_ms(&Control { foreground: false, ..c }, false), Some(2000));
        assert_eq!(period_ms(&Control { idle: true, ..c }, false), Some(2000));
        assert_eq!(period_ms(&c, true), Some(2000));
        assert_eq!(period_ms(&Control { refresh: Refresh::Half, foreground: false, ..c }, false), Some(2000));
        assert_eq!(
            period_ms(&Control { refresh: Refresh::Five, foreground: false, idle: true, ..c }, true),
            Some(5000)
        );
        assert_eq!(period_ms(&Control { refresh: Refresh::Paused, foreground: false, ..c }, true), None);
        // Combined: stopped wins over everything.
        assert_eq!(
            period_ms(&Control { visible: false, foreground: false, idle: true, display_off: true, ..c }, true),
            None
        );
        assert_eq!(period_ms(&Control { idle: true, foreground: false, ..c }, true), Some(2000));
    }

    #[test]
    fn slow_on_after_5_over_8_ms() {
        let mut s = Slow::default();
        for _ in 0..4 {
            assert!(!s.observe(SLOW_TICK_US + 1));
        }
        // A tick at the limit is not over it and breaks the run.
        assert!(!s.observe(SLOW_TICK_US));
        for _ in 0..4 {
            assert!(!s.observe(9_000));
        }
        assert!(s.observe(9_000));
        assert!(s.is_slow());
    }

    #[test]
    fn slow_off_after_30_fast() {
        let mut s = Slow::default();
        for _ in 0..5 {
            s.observe(20_000);
        }
        assert!(s.is_slow());
        for _ in 0..29 {
            assert!(s.observe(FAST_TICK_US));
        }
        // A middling tick breaks the run of fast ticks.
        assert!(s.observe(FAST_TICK_US + 1));
        for _ in 0..29 {
            assert!(s.observe(1_000));
        }
        assert!(!s.observe(1_000));
        assert!(!s.is_slow());
        // And it takes five over ticks again to turn it on.
        for _ in 0..4 {
            assert!(!s.observe(9_000));
        }
        assert!(s.observe(9_000));
    }

    #[test]
    fn idle_after_30_s_calm_only() {
        assert!(!idle(0, IDLE_AFTER_MS - 1, true));
        assert!(idle(0, IDLE_AFTER_MS, true));
        assert!(!idle(0, IDLE_AFTER_MS, false));
        assert!(idle(5_000, 5_000 + IDLE_AFTER_MS + 1, true));
        // A last input in the future (clock quirk) is not idle.
        assert!(!idle(10_000, 5_000, true));
    }
}
