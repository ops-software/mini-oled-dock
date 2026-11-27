//! Heat steps of the metric cells.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HeatStep {
    #[default]
    S0,
    S1,
    S2,
    S3,
    S4,
}

impl HeatStep {
    fn below(self) -> HeatStep {
        match self {
            HeatStep::S4 => HeatStep::S3,
            HeatStep::S3 => HeatStep::S2,
            HeatStep::S2 => HeatStep::S1,
            _ => HeatStep::S0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MetricKind {
    Cpu,
    Memory,
    Io,
    Gpu,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Machine {
    pub logical_cpus: u32,
    pub total_phys: u64,
}

const MIB: f64 = 1_048_576.0;

/// v: Cpu = % of the whole processor (converted to one-core basis inside), Memory = bytes, Io = bytes/s, Gpu = %.
pub fn heat(kind: MetricKind, v: f64, prev: HeatStep, m: &Machine) -> HeatStep {
    if v.is_nan() {
        return HeatStep::S0;
    }
    let mut raw = HeatStep::S0;
    for s in [HeatStep::S1, HeatStep::S2, HeatStep::S3, HeatStep::S4] {
        if v >= threshold(kind, s, m) {
            raw = s;
        }
    }
    if raw >= prev {
        return raw;
    }
    // Exit hysteresis: leave a step only below 80 % of the threshold that put the cell there.
    let mut s = prev;
    while s > HeatStep::S0 && v < 0.8 * threshold(kind, s, m) {
        s = s.below();
    }
    s.max(raw)
}

/// In the unit of v; S0 -> 0.
pub fn threshold(kind: MetricKind, step: HeatStep, m: &Machine) -> f64 {
    let i = match step {
        HeatStep::S0 => return 0.0,
        HeatStep::S1 => 0,
        HeatStep::S2 => 1,
        HeatStep::S3 => 2,
        HeatStep::S4 => 3,
    };
    match kind {
        // Share of one core: 100 % of one core on N logical CPUs is 100 / N of the whole processor.
        MetricKind::Cpu => [10.0, 50.0, 100.0, 400.0][i] / m.logical_cpus.max(1) as f64,
        MetricKind::Memory => m.total_phys as f64 * [1.0, 5.0, 10.0, 25.0][i] / 100.0,
        MetricKind::Io => [1.0, 10.0, 50.0, 200.0][i] * MIB,
        MetricKind::Gpu => [10.0, 30.0, 60.0, 90.0][i],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use HeatStep::*;

    const M: Machine = Machine { logical_cpus: 16, total_phys: 32 << 30 };
    const STEPS: [HeatStep; 4] = [S1, S2, S3, S4];

    #[test]
    fn heat_thresholds_table() {
        // (kind, thresholds in the unit of v)
        for kind in [MetricKind::Cpu, MetricKind::Memory, MetricKind::Io, MetricKind::Gpu] {
            for (i, s) in STEPS.iter().enumerate() {
                let t = threshold(kind, *s, &M);
                assert_eq!(heat(kind, t, S0, &M), *s, "{kind:?} at {s:?}");
                let just_below = t * 0.999;
                let want = if i == 0 { S0 } else { STEPS[i - 1] };
                assert_eq!(heat(kind, just_below, S0, &M), want, "{kind:?} below {s:?}");
            }
            assert_eq!(threshold(kind, S0, &M), 0.0);
            assert_eq!(heat(kind, 0.0, S0, &M), S0);
        }
        // 6.25 % of a 16-thread processor is 100 % of one core.
        assert_eq!(threshold(MetricKind::Cpu, S3, &M), 6.25);
        assert_eq!(heat(MetricKind::Cpu, 6.25, S0, &M), S3);
        assert_eq!(heat(MetricKind::Cpu, 25.0, S0, &M), S4);
        assert_eq!(heat(MetricKind::Cpu, f64::NAN, S3, &M), S0);
    }

    #[test]
    fn heat_exit_hysteresis() {
        let one_core = |p: f64| p / 16.0;
        assert_eq!(heat(MetricKind::Cpu, one_core(100.0), S0, &M), S3);
        assert_eq!(heat(MetricKind::Cpu, one_core(85.0), S3, &M), S3);
        assert_eq!(heat(MetricKind::Cpu, one_core(79.0), S3, &M), S2);
        // S1 to S0 below 8 % of one core.
        assert_eq!(heat(MetricKind::Cpu, one_core(9.0), S1, &M), S1);
        assert_eq!(heat(MetricKind::Cpu, one_core(7.9), S1, &M), S0);
        // A deep fall drops several steps at once.
        assert_eq!(heat(MetricKind::Cpu, one_core(5.0), S4, &M), S0);
        // Rising never waits.
        assert_eq!(heat(MetricKind::Cpu, one_core(60.0), S0, &M), S2);
    }

    #[test]
    fn heat_memory_and_io_units() {
        let five_pct = (M.total_phys as f64 * 0.05).ceil();
        assert_eq!(heat(MetricKind::Memory, five_pct, S0, &M), S2);
        assert_eq!(heat(MetricKind::Io, 10.0 * 1_048_576.0, S0, &M), S2);
        assert_eq!(heat(MetricKind::Io, 10.0 * 1_000_000.0, S0, &M), S1);
        assert_eq!(heat(MetricKind::Io, 1_048_575.0, S0, &M), S0);
    }

    #[test]
    fn zero_cpus_count_as_one() {
        let m = Machine { logical_cpus: 0, total_phys: 1 << 30 };
        assert_eq!(heat(MetricKind::Cpu, 100.0, S0, &m), S3);
    }
}
