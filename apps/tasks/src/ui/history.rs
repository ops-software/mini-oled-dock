//! The header's traces: 60 samples per column, peak-preserving, drawn as column spans (TA-HDR, TH1).
//!
//! Three rings of 60 samples (machine CPU %, memory load %, I/O bytes per second) are pushed once per good frame, so a
//! paused or stopped sampler leaves a gap in time that is not filled. The traces are repainted, never tweened and never
//! blitted: each paint resamples the ring to the device columns of its rect with oled_ui's resample_peak.

use oled_sysmon::ring::Ring;
use oled_ui::widgets::resample_peak;

/// Samples kept per trace.
pub const SAMPLES: usize = 60;
/// The I/O ceiling only falls after it has been needed this long.
pub const CEIL_HOLD_MS: u64 = 30_000;
/// Height of a trace, logical px, and its top inside the header.
pub const TRACE_H: i32 = 12;
pub const TRACE_TOP: i32 = 44;
/// The floor of the I/O scale: 1 KB/s.
pub const IO_FLOOR: f32 = 1_000.0;

/// The 1-2-5 steps of the I/O ceiling, 1 KB/s up to 10 GB/s.
pub fn io_steps() -> Vec<f32> {
    let mut v = Vec::new();
    let mut base = IO_FLOOR;
    while base < 10_000_000_000.0 {
        v.extend([base, base * 2.0, base * 5.0]);
        base *= 10.0;
    }
    v.push(10_000_000_000.0);
    v
}

/// The thickness of the top line in device px: 1, 2, 2 at 100, 150 and 200 % (SPEC-tasks 5.3.9).
pub fn hairline(scale: f32) -> i32 {
    if scale >= 1.5 {
        2
    } else {
        1
    }
}

/// One trace as spans, in window coordinates: the fill from the baseline and the line along its top.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Trace {
    /// Device columns from the rect's left edge to the first drawn column (the part of the window with no sample yet).
    pub x_off: i32,
    pub fill: Vec<(i16, i16)>,
    pub line: Vec<(i16, i16)>,
}

/// The samples of the three traces and the I/O scale.
pub struct History {
    cpu: Ring,
    mem: Ring,
    io: Ring,
    steps: Vec<f32>,
    /// Index of the I/O ceiling in `steps`.
    ceil: usize,
    /// Since when the peak has been below half the next lower ceiling.
    low_since: Option<u64>,
}

impl Default for History {
    fn default() -> History {
        History {
            cpu: Ring::new(SAMPLES),
            mem: Ring::new(SAMPLES),
            io: Ring::new(SAMPLES),
            steps: io_steps(),
            ceil: 0,
            low_since: None,
        }
    }
}

impl History {
    /// One good frame: the machine's CPU share (0..=100), memory load (0..=100) and I/O rate; a value not measured yet
    /// counts as 0. `now_ms` is the clock of the I/O ceiling's 30 s rule.
    pub fn push(&mut self, now_ms: u64, cpu: Option<f32>, mem: Option<u8>, io: Option<f32>) {
        let clean = |v: f32| if v.is_finite() { v.max(0.0) } else { 0.0 };
        self.cpu.push(clean(cpu.unwrap_or(0.0)).min(100.0));
        self.mem.push(f32::from(mem.unwrap_or(0).min(100)));
        self.io.push(clean(io.unwrap_or(0.0)));
        self.steer(now_ms);
    }

    /// Rises at once to the step that holds the peak; falls one step only after 30 s with the peak below half of the
    /// next lower step.
    fn steer(&mut self, now_ms: u64) {
        let peak = self.io.max_in(0, SAMPLES - 1).unwrap_or(0.0);
        let need = self.steps.iter().position(|s| *s >= peak).unwrap_or(self.steps.len() - 1);
        if need > self.ceil {
            self.ceil = need;
            self.low_since = None;
            return;
        }
        let lower = matches!(self.ceil.checked_sub(1), Some(i) if peak < self.steps[i] / 2.0);
        if !lower {
            self.low_since = None;
            return;
        }
        let since = *self.low_since.get_or_insert(now_ms);
        if now_ms.saturating_sub(since) >= CEIL_HOLD_MS {
            self.ceil -= 1;
            self.low_since = Some(now_ms);
        }
    }

    pub fn len(&self) -> usize {
        self.cpu.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cpu.is_empty()
    }

    /// The I/O trace's top, bytes per second.
    pub fn io_ceiling(&self) -> f32 {
        self.steps[self.ceil]
    }

    fn series(r: &Ring) -> Vec<f32> {
        (0..r.len()).rev().filter_map(|b| r.get(b)).collect()
    }

    /// Oldest first.
    pub fn cpu(&self) -> Vec<f32> {
        History::series(&self.cpu)
    }

    pub fn mem(&self) -> Vec<f32> {
        History::series(&self.mem)
    }

    pub fn io(&self) -> Vec<f32> {
        History::series(&self.io)
    }

    /// The trace of column `which` (0 CPU, 1 memory, 2 I/O) in a rect `width` device px wide whose baseline is the
    /// row `base`: `height` px high, the line `hair` px thick. The 60 slots span the width, the newest at the right
    /// edge, so a fresh window fills from the right.
    pub fn trace(&self, which: usize, width: i32, height: i32, hair: i32, base: i32) -> Trace {
        let (values, max) = match which {
            0 => (self.cpu(), 100.0),
            1 => (self.mem(), 100.0),
            _ => (self.io(), self.io_ceiling()),
        };
        trace_of(&values, max, width, height, hair, base)
    }
}

/// Pure: `values` (oldest first, 0..=max) laid on the right of `width` columns of a 60-slot window.
pub fn trace_of(values: &[f32], max: f32, width: i32, height: i32, hair: i32, base: i32) -> Trace {
    if values.is_empty() || width <= 0 || height <= 0 || max <= 0.0 {
        return Trace::default();
    }
    let n = values.len().min(SAMPLES);
    let values = &values[values.len() - n..];
    let cols = ((i64::from(width) * n as i64 + SAMPLES as i64 / 2) / SAMPLES as i64).clamp(1, i64::from(width)) as i32;
    let at = |v: i32| v.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
    let mut t = Trace { x_off: width - cols, ..Trace::default() };
    for v in resample_peak(values, cols as usize) {
        let h = ((v / max).clamp(0.0, 1.0) * height as f32).round() as i32;
        let top = base - h.max(hair);
        t.fill.push((at(base - h), at(base)));
        t.line.push((at(top), at(top + hair)));
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_order_and_gaps() {
        let mut h = History::default();
        for i in 0..70 {
            h.push(i * 1000, Some(i as f32), Some(50), None);
        }
        let c = h.cpu();
        assert_eq!(c.len(), SAMPLES, "60 samples are kept");
        assert_eq!((c[0], c[59]), (10.0, 69.0), "oldest first, 60 newest kept");
        // Missing values are 0; a frame that never arrived (paused) adds nothing: no gap is filled.
        let mut g = History::default();
        g.push(0, None, None, None);
        assert_eq!((g.len(), g.cpu()[0], g.mem()[0], g.io()[0]), (1, 0.0, 0.0, 0.0));
        assert!(History::default().is_empty());
    }

    #[test]
    fn io_ceiling_steps() {
        let mut h = History::default();
        assert_eq!(h.io_ceiling(), 1_000.0, "never below 1 KB/s");
        // Rises at once.
        h.push(0, None, None, Some(3_000_000.0));
        assert_eq!(h.io_ceiling(), 5_000_000.0);
        h.push(1000, None, None, Some(1.0e12));
        assert_eq!(h.io_ceiling(), 10_000_000_000.0, "capped at 10 GB/s");
        // The spike leaves the ring after 60 samples; then the ceiling waits 30 s per step.
        let mut h = History::default();
        h.push(0, None, None, Some(3_000_000.0));
        let mut t = 1000u64;
        for _ in 0..SAMPLES {
            h.push(t, None, None, Some(10.0));
            t += 1000;
        }
        // The first sample with the spike gone: the condition starts to hold now.
        let held_from = t - 1000;
        assert_eq!(h.io_ceiling(), 5_000_000.0, "just started to hold");
        while t < held_from + 29_000 {
            h.push(t, None, None, Some(10.0));
            t += 1000;
        }
        assert_eq!(h.io_ceiling(), 5_000_000.0, "stays for 29 s");
        h.push(held_from + 30_000, None, None, Some(10.0));
        assert_eq!(h.io_ceiling(), 2_000_000.0, "falls one step at 30 s");
        h.push(held_from + 59_000, None, None, Some(10.0));
        assert_eq!(h.io_ceiling(), 2_000_000.0, "one step per 30 s");
        h.push(held_from + 60_000, None, None, Some(10.0));
        assert_eq!(h.io_ceiling(), 1_000_000.0);
        // Never below the floor.
        for k in 0..40 {
            h.push(held_from + 61_000 + k * 30_000, None, None, Some(0.0));
        }
        assert_eq!(h.io_ceiling(), 1_000.0);
    }

    #[test]
    fn io_ceiling_needs_2x_headroom_to_fall() {
        let mut h = History::default();
        h.push(0, None, None, Some(900_000.0));
        assert_eq!(h.io_ceiling(), 1_000_000.0);
        // A steady 300 KB/s: the next lower step (500 KB/s) would hold it with less than 2x headroom, so the ceiling
        // never falls, however long it waits.
        let mut t = 1000;
        for _ in 0..200 {
            h.push(t, None, None, Some(300_000.0));
            t += 1000;
        }
        assert_eq!(h.io_ceiling(), 1_000_000.0);
        // 200 KB/s is below half of 500 KB/s: one step after 30 s.
        let from = t;
        for _ in 0..(SAMPLES + 31) {
            h.push(t, None, None, Some(200_000.0));
            t += 1000;
        }
        assert!(t > from + 30_000);
        assert_eq!(h.io_ceiling(), 500_000.0);
    }

    #[test]
    fn spike_survives_resampling() {
        let mut v = vec![0.0f32; 60];
        v[31] = 100.0;
        let t = trace_of(&v, 100.0, 52, 12, 1, 100);
        assert_eq!(t.x_off, 0);
        assert_eq!(t.fill.len(), 52);
        assert!(t.fill.iter().any(|&(top, bottom)| bottom - top == 12), "the spike keeps its full height");
        assert_eq!(t.fill.iter().filter(|&&(top, bottom)| bottom > top).count(), 1, "and its width of one column");
        // The same at a width wider than the slots.
        let w = trace_of(&v, 100.0, 200, 12, 2, 100);
        assert!(w.fill.iter().any(|&(top, bottom)| bottom - top == 12));
    }

    #[test]
    fn a_young_window_fills_from_the_right() {
        let t = trace_of(&[50.0; 30], 100.0, 60, 12, 1, 50);
        assert_eq!((t.x_off, t.fill.len()), (30, 30));
        assert_eq!(t.fill[0], (44, 50), "half height");
        assert_eq!(t.line[0], (44, 45), "the line sits on the top row");
        // Zero keeps a hairline on the baseline and no fill.
        let z = trace_of(&[0.0; 60], 100.0, 60, 12, 2, 50);
        assert_eq!((z.fill[0], z.line[0]), ((50, 50), (48, 50)));
        assert_eq!(trace_of(&[], 100.0, 60, 12, 1, 50), Trace::default());
    }

    #[test]
    fn hairline_by_scale() {
        assert_eq!([hairline(1.0), hairline(1.25), hairline(1.5), hairline(2.0)], [1, 1, 2, 2]);
    }
}
