//! CPU time readers and the dock's busy-percent rule.

use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::Threading::{GetActiveProcessorCount, GetSystemTimes};

/// 100 ns units; kernel includes idle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CpuTimes {
    pub idle: u64,
    pub kernel: u64,
    pub user: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CpuDelta {
    pub d_idle: u64,
    pub d_total: u64,
}

impl CpuDelta {
    /// The dock's integer rule.
    pub fn percent(self) -> u8 {
        busy_percent(self.d_idle, self.d_total)
    }
    /// 0.0..=100.0.
    pub fn fraction(self) -> f32 {
        busy_fraction(self.d_idle, self.d_total)
    }
}

/// CPU busy percentage between two `(idle, kernel + user)` deltas (kernel time includes idle).
pub fn busy_percent(d_idle: u64, d_total: u64) -> u8 {
    if d_total == 0 {
        return 0;
    }
    let busy = d_total.saturating_sub(d_idle);
    ((busy * 100 + d_total / 2) / d_total).min(100) as u8
}

/// The same share without rounding to an integer, 0.0..=100.0.
pub fn busy_fraction(d_idle: u64, d_total: u64) -> f32 {
    if d_total == 0 {
        return 0.0;
    }
    let busy = d_total.saturating_sub(d_idle);
    (busy as f64 * 100.0 / d_total as f64).clamp(0.0, 100.0) as f32
}

fn ft(f: FILETIME) -> u64 {
    ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64
}

/// GetSystemTimes.
pub fn system_times() -> Option<CpuTimes> {
    let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
    // SAFETY: out pointers to locals.
    unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }.ok()?;
    Some(CpuTimes { idle: ft(idle), kernel: ft(kernel), user: ft(user) })
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemCpu {
    idle: u64,
    total: u64,
}

impl SystemCpu {
    /// Unprimed.
    pub const fn new() -> SystemCpu {
        SystemCpu { idle: 0, total: 0 }
    }
    /// None on the priming call.
    pub fn feed(&mut self, t: CpuTimes) -> Option<CpuDelta> {
        let (i, total) = (t.idle, t.kernel.saturating_add(t.user));
        let out = (self.total != 0)
            .then(|| CpuDelta { d_idle: i.saturating_sub(self.idle), d_total: total.saturating_sub(self.total) });
        self.idle = i;
        self.total = total;
        out
    }
    /// system_times().and_then(feed): None also on a failed read.
    pub fn sample(&mut self) -> Option<CpuDelta> {
        system_times().and_then(|t| self.feed(t))
    }
}

/// GetActiveProcessorCount(ALL_PROCESSOR_GROUPS), at least 1.
pub fn logical_cpus() -> u32 {
    // SAFETY: plain query, no pointers.
    unsafe { GetActiveProcessorCount(0xFFFF) }.max(1)
}

/// Share of the whole processor, clamped 0..=100.
pub fn process_cpu_percent(d_cpu_100ns: u64, d_wall_100ns: u64, logical: u32) -> f32 {
    if d_wall_100ns == 0 {
        return 0.0;
    }
    let cap = d_wall_100ns as f64 * logical.max(1) as f64;
    (d_cpu_100ns as f64 * 100.0 / cap).clamp(0.0, 100.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_percent_matches_the_dock() {
        assert_eq!(busy_percent(0, 0), 0);
        assert_eq!(busy_percent(75, 100), 25);
        assert_eq!(busy_percent(0, 100), 100);
        assert_eq!(busy_percent(200, 100), 0);
        assert_eq!(busy_percent(0, 1), 100);
        assert_eq!(busy_percent(1, 3), 67);
        assert_eq!(CpuDelta { d_idle: 75, d_total: 100 }.percent(), 25);
    }

    #[test]
    fn feed_primes_then_deltas() {
        let mut s = SystemCpu::new();
        assert_eq!(s.feed(CpuTimes { idle: 100, kernel: 150, user: 50 }), None);
        let d = s.feed(CpuTimes { idle: 180, kernel: 250, user: 150 });
        assert_eq!(d, Some(CpuDelta { d_idle: 80, d_total: 200 }));
        // A total that goes backwards gives a zero delta and 0 percent.
        let d = s.feed(CpuTimes { idle: 200, kernel: 100, user: 50 }).unwrap();
        assert_eq!(d.d_total, 0);
        assert_eq!(d.percent(), 0);
        assert_eq!(d.fraction(), 0.0);
    }

    #[test]
    fn fraction_is_one_decimal_ready() {
        assert_eq!(busy_fraction(25, 100), 75.0);
        assert_eq!(busy_fraction(0, 0), 0.0);
        assert_eq!(busy_fraction(500, 100), 0.0);
        assert_eq!(busy_fraction(0, 7), 100.0);
    }

    #[test]
    fn process_cpu_share() {
        assert_eq!(process_cpu_percent(10_000_000, 10_000_000, 4), 25.0);
        assert_eq!(process_cpu_percent(5, 0, 4), 0.0);
        assert_eq!(process_cpu_percent(u64::MAX, 1, 1), 100.0);
        assert_eq!(process_cpu_percent(10, 10, 0), 100.0);
    }

    #[test]
    fn system_times_read() {
        let t = system_times().expect("GetSystemTimes");
        assert!(t.kernel > 0 && t.kernel.saturating_add(t.user) >= t.idle);
        assert!(logical_cpus() >= 1);
    }
}
