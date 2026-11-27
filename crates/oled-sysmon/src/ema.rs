//! Time-constant smoothing.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ema {
    pub tau_ms: u32,
    value: f32,
    primed: bool,
}

impl Ema {
    pub const fn new(tau_ms: u32) -> Ema {
        Ema { tau_ms, value: 0.0, primed: false }
    }
    /// First push primes with x; alpha = 1 - exp(-dt/tau). A non-finite x is ignored.
    pub fn push(&mut self, x: f32, dt_ms: u32) -> f32 {
        if !x.is_finite() {
            return self.value;
        }
        if !self.primed {
            self.value = x;
            self.primed = true;
            return x;
        }
        let alpha = if self.tau_ms == 0 { 1.0 } else { 1.0 - (-(dt_ms as f64) / self.tau_ms as f64).exp() };
        let next = self.value as f64 + alpha * (x as f64 - self.value as f64);
        let (lo, hi) = if x < self.value { (x, self.value) } else { (self.value, x) };
        self.value = (next as f32).clamp(lo, hi);
        self.value
    }
    /// None until primed.
    pub fn value(&self) -> Option<f32> {
        self.primed.then_some(self.value)
    }
    pub fn reset(&mut self) {
        self.value = 0.0;
        self.primed = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ema_primes_and_converges() {
        let mut e = Ema::new(1000);
        assert_eq!(e.value(), None);
        assert_eq!(e.push(10.0, 500), 10.0);
        let v = e.push(20.0, 1000);
        let want = 10.0 + (1.0 - (-1.0f64).exp()) * 10.0;
        assert!((v as f64 - want).abs() < 1e-4, "{v} vs {want}");
        assert_eq!(e.value(), Some(v));
        e.reset();
        assert_eq!(e.value(), None);
    }

    #[test]
    fn ema_zero_dt_keeps_value() {
        let mut e = Ema::new(1000);
        e.push(10.0, 0);
        assert_eq!(e.push(50.0, 0), 10.0);
    }

    #[test]
    fn ema_huge_dt_jumps() {
        let mut e = Ema::new(1000);
        e.push(10.0, 0);
        assert_eq!(e.push(77.0, 3_600_000), 77.0);
    }

    #[test]
    fn ema_ignores_non_finite() {
        let mut e = Ema::new(1000);
        assert_eq!(e.push(f32::NAN, 10), 0.0);
        assert_eq!(e.value(), None);
        e.push(4.0, 10);
        assert_eq!(e.push(f32::INFINITY, 10), 4.0);
    }

    #[test]
    fn ema_stays_between_inputs() {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut e = Ema::new(700);
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        for _ in 0..5000 {
            let x = (next() % 100_000) as f32 / 1000.0;
            let dt = (next() % 3000) as u32;
            lo = lo.min(x);
            hi = hi.max(x);
            let v = e.push(x, dt);
            assert!(v >= lo && v <= hi, "{v} outside {lo}..{hi}");
        }
    }
}
