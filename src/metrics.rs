//! System metrics for the METRICS module of the bar (`render::ItemId::Metrics`): CPU % and RAM %, read
//! and computed by `oled_sysmon::cpu` (`GetSystemTimes` deltas) and `oled_sysmon::mem`
//! (`GlobalMemoryStatusEx`); this module only keeps the previous totals and the last values.
//!
//! COST: two kernel32 calls per sample, no allocation, no thread. The UI owns a `Sampler` only while
//! `show_metrics` is on and samples it from the sysmon timer, which runs only while the bar is
//! visible (`ui::sysmon_sync`). Disabled = the `Sampler` does not exist.
//!
//! THREADING: UI thread (any thread would do: no state outside the `Sampler`).

use oled_sysmon::cpu::{CpuDelta, SystemCpu};

/// CPU and RAM usage, percent 0..=100.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub cpu: u8,
    pub ram: u8,
}

/// CPU sampler: remembers the previous `GetSystemTimes` totals.
#[derive(Clone, Copy, Debug, Default)]
pub struct Sampler {
    cpu: SystemCpu,
    /// Last computed values (returned again if a sample fails).
    last: Usage,
}

impl Sampler {
    /// New sampler primed with the current totals (the first `sample` gives a real delta).
    pub fn new() -> Sampler {
        let mut s = Sampler::default();
        s.sample();
        s
    }

    /// Values of the last sample (CPU 0 right after `new`: no interval yet).
    pub fn last(&self) -> Usage {
        self.last
    }

    /// Takes a sample: CPU since the previous call, current RAM load.
    pub fn sample(&mut self) -> Usage {
        let (c, r) = (self.cpu.sample(), oled_sysmon::mem::memory_load());
        self.apply(c, r)
    }

    /// Folds one reading into the last values; a failed read (`None`) keeps the old value.
    fn apply(&mut self, cpu: Option<CpuDelta>, ram: Option<u8>) -> Usage {
        if let Some(d) = cpu {
            self.last.cpu = d.percent();
        }
        if let Some(r) = ram {
            self.last.ram = r;
        }
        self.last
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oled_sysmon::cpu::{busy_percent, CpuTimes};

    /// The sampler as it was before the move to oled-sysmon, with the two OS calls replaced by
    /// injected values.
    #[derive(Default)]
    struct OldSampler {
        idle: u64,
        total: u64,
        last: Usage,
    }

    fn old_cpu_percent(d_idle: u64, d_total: u64) -> u8 {
        if d_total == 0 {
            return 0;
        }
        let busy = d_total.saturating_sub(d_idle);
        ((busy * 100 + d_total / 2) / d_total).min(100) as u8
    }

    impl OldSampler {
        fn step(&mut self, times: Option<(u64, u64, u64)>, ram: Option<u8>) -> Usage {
            if let Some((idle, kernel, user)) = times {
                let (i, t) = (idle, kernel + user);
                if self.total != 0 {
                    self.last.cpu = old_cpu_percent(i.saturating_sub(self.idle), t.saturating_sub(self.total));
                }
                self.idle = i;
                self.total = t;
            }
            if let Some(r) = ram {
                self.last.ram = r;
            }
            self.last
        }
    }

    fn new_step(s: &mut Sampler, times: Option<(u64, u64, u64)>, ram: Option<u8>) -> Usage {
        let c = times.and_then(|(idle, kernel, user)| s.cpu.feed(CpuTimes { idle, kernel, user }));
        s.apply(c, ram)
    }

    /// 41 consecutive `GetSystemTimes` triples 50 ms apart (idle, kernel, user), offsets only.
    #[rustfmt::skip]
    const RECORDED: [(u64, u64, u64); 41] = [
        (1000000, 1000000, 1000000), (7250000, 7875000, 2250000), (12250000, 14125000, 3500000),
        (17875000, 20531250, 4593750), (24281250, 28187500, 6468750), (28812500, 33500000, 7875000),
        (36000000, 40843750, 9281250), (42093750, 46937500, 10687500), (48187500, 53656250, 12250000),
        (55375000, 61156250, 13343750), (61625000, 67875000, 14750000), (67406250, 73812500, 16312500),
        (73187500, 79906250, 17718750), (80062500, 87406250, 19281250), (86000000, 93656250, 20843750),
        (91781250, 100062500, 22562500), (96468750, 106781250, 23343750), (102562500, 113187500, 25062500),
        (109437500, 120531250, 26156250), (115218750, 126781250, 27250000), (122406250, 133968750, 28656250),
        (128343750, 140218750, 29906250), (135062500, 147093750, 31625000), (141156250, 153187500, 33187500),
        (148343750, 160843750, 34281250), (153656250, 166781250, 35843750), (159437500, 172875000, 37250000),
        (165843750, 179437500, 39281250), (172250000, 185843750, 40843750), (179281250, 193187500, 41937500),
        (185375000, 199437500, 43187500), (192875000, 206937500, 45218750), (198187500, 212718750, 46156250),
        (205218750, 219750000, 47875000), (211156250, 225687500, 49437500), (216937500, 231625000, 51000000),
        (223968750, 238968750, 52562500), (230531250, 246000000, 54125000), (236000000, 252562500, 55062500),
        (241625000, 258968750, 56156250), (246781250, 265375000, 57250000),
    ];

    struct Xorshift(u64);

    impl Xorshift {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    #[test]
    fn t0_output_is_identical() {
        let (mut old, mut new) = (OldSampler::default(), Sampler::default());
        for (n, t) in RECORDED.iter().enumerate() {
            let ram = Some((n as u32 * 7 % 101) as u8);
            assert_eq!(old.step(Some(*t), ram), new_step(&mut new, Some(*t), ram), "recorded step {n}");
        }
        for seed in 1..=10_000u64 {
            let mut rng = Xorshift(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let (mut old, mut new) = (OldSampler::default(), Sampler::default());
            let (mut idle, mut kernel, mut user) = (0u64, 0u64, 0u64);
            for step in 0..40 {
                let r = rng.next();
                let mode = r % 8;
                let delta = (r >> 8) % 5_000_000;
                match mode {
                    // Zero deltas.
                    0 => {}
                    // Idle above total.
                    1 => {
                        idle += delta * 4;
                        kernel += delta;
                    }
                    // Totals that go backwards.
                    2 => {
                        kernel = kernel.saturating_sub(delta);
                        user = user.saturating_sub(delta / 2);
                        idle = idle.saturating_sub(delta);
                    }
                    // Large totals (below 2^50, so that `busy * 100` cannot overflow either sampler).
                    3 => {
                        idle = (rng.next() >> 3) % (1 << 50);
                        kernel = (rng.next() >> 3) % (1 << 50);
                        user = (rng.next() >> 3) % (1 << 50);
                    }
                    _ => {
                        idle += delta / 2;
                        kernel += delta;
                        user += delta / 3;
                    }
                }
                let times = ((r >> 40) & 7 != 0).then_some((idle, kernel, user));
                let ram = match (r >> 48) % 10 {
                    0 => None,
                    _ => Some(((r >> 52) % 101) as u8),
                };
                assert_eq!(
                    old.step(times, ram),
                    new_step(&mut new, times, ram),
                    "seed {seed} step {step} times {times:?} ram {ram:?}"
                );
            }
        }
    }

    #[test]
    fn cpu_math() {
        assert_eq!(busy_percent(0, 0), 0);
        assert_eq!(busy_percent(75, 100), 25);
        assert_eq!(busy_percent(0, 100), 100);
        assert_eq!(busy_percent(200, 100), 0);
    }
}
