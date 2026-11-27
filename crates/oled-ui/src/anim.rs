//! The animation clock (SPEC-platform 5.3.6): one per window, sampled on the frame clock (the fake clock in the
//! Harness). Durations come in already zeroed under reduced motion. K1a.4 owns it.

use crate::cx::{AnimId, Ease};

/// One running animation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Tween {
    pub(crate) from: f32,
    pub(crate) to: f32,
    pub(crate) start_ms: u64,
    pub(crate) dur_ms: u32,
    pub(crate) ease: Ease,
}

impl Tween {
    /// The value at `now_ms`.
    pub(crate) fn at(&self, now_ms: u64) -> f32 {
        if self.dur_ms == 0 {
            return self.to;
        }
        let t = (now_ms.saturating_sub(self.start_ms) as f32 / self.dur_ms as f32).clamp(0.0, 1.0);
        let e = match self.ease {
            Ease::OutCubic => 1.0 - (1.0 - t).powi(3),
            Ease::In => t * t,
            Ease::Linear => t,
        };
        self.from + (self.to - self.from) * e
    }

    /// True while now < start + dur.
    pub(crate) fn live(&self, now_ms: u64) -> bool {
        now_ms < self.start_ms + u64::from(self.dur_ms)
    }
}

/// The window's tweens.
pub(crate) struct AnimClock {
    tweens: Vec<(AnimId, Tween)>,
}

impl AnimClock {
    pub(crate) fn new() -> AnimClock {
        AnimClock { tweens: Vec::new() }
    }

    /// Starts (or retargets, from the current value) the animation of `id`; a zero duration jumps to `to`.
    pub(crate) fn start(&mut self, id: AnimId, to: f32, dur_ms: u32, ease: Ease, now_ms: u64) {
        let from = self.value(id, now_ms).unwrap_or(to);
        self.tweens.retain(|t| t.0 != id);
        self.tweens.push((id, Tween { from, to, start_ms: now_ms, dur_ms, ease }));
    }

    /// The value of `id` at `now_ms` (its end value once finished), None if it never ran.
    pub(crate) fn value(&self, id: AnimId, now_ms: u64) -> Option<f32> {
        self.tweens.iter().find(|t| t.0 == id).map(|t| t.1.at(now_ms))
    }

    /// True while any tween runs: the frame clock is armed only then.
    pub(crate) fn is_live(&self, now_ms: u64) -> bool {
        self.tweens.iter().any(|t| t.1.live(now_ms))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{NodeId, WidgetId};

    #[test]
    fn tweens_ease_and_end() {
        let id = AnimId { node: NodeId::new(WidgetId(1), 0), channel: 0 };
        let mut c = AnimClock::new();
        assert_eq!(c.value(id, 0), None);
        c.start(id, 1.0, 100, Ease::Linear, 0);
        assert_eq!(c.value(id, 0), Some(1.0), "a first start has no current value: it starts at its target");
        c.start(id, 0.0, 100, Ease::Linear, 0);
        assert_eq!(c.value(id, 50), Some(0.5));
        assert!(c.is_live(99) && !c.is_live(100));
        let t = Tween { from: 0.0, to: 1.0, start_ms: 0, dur_ms: 100, ease: Ease::OutCubic };
        assert!((t.at(50) - 0.875).abs() < 1e-6);
        let t = Tween { ease: Ease::In, ..t };
        assert!((t.at(50) - 0.25).abs() < 1e-6);
    }
}
