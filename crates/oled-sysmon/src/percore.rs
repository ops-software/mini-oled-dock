//! Per-core times (SystemProcessorPerformanceInformation).
use crate::cpu::{busy_fraction, logical_cpus};
use crate::error::SysmonError;
use windows::Wdk::System::SystemInformation::{NtQuerySystemInformation, SystemProcessorPerformanceInformation};

/// SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION, x64.
pub const CORE_RECORD: usize = 48;

// Field offsets inside one record. The windows crate names DpcTime and InterruptTime Reserved1[0] and [1], and
// InterruptCount Reserved2; the meaning is the documented layout of the structure.
const O_IDLE: usize = 0;
const O_KERNEL: usize = 8;
const O_USER: usize = 16;
const O_DPC: usize = 24;
const O_INTERRUPT: usize = 32;

/// 100 ns units; kernel includes idle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CoreTimes {
    pub idle: u64,
    pub kernel: u64,
    pub user: u64,
    pub dpc: u64,
    pub interrupt: u64,
}

fn u64_at(buf: &[u8], at: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&buf[at..at + 8]);
    u64::from_le_bytes(b)
}

/// One record per core; a length that is not a whole number of records is a Layout fault. Clears `out` first.
pub fn parse_cores(buf: &[u8], out: &mut Vec<CoreTimes>) -> Result<(), SysmonError> {
    out.clear();
    if !buf.len().is_multiple_of(CORE_RECORD) {
        return Err(SysmonError::Layout { offset: buf.len().min(u32::MAX as usize) as u32, what: "core record size" });
    }
    out.extend(buf.chunks_exact(CORE_RECORD).map(|r| CoreTimes {
        idle: u64_at(r, O_IDLE),
        kernel: u64_at(r, O_KERNEL),
        user: u64_at(r, O_USER),
        dpc: u64_at(r, O_DPC),
        interrupt: u64_at(r, O_INTERRUPT),
    }));
    Ok(())
}

#[derive(Debug, Default)]
pub struct CoreReader {
    buf: Vec<u64>,
}

impl CoreReader {
    pub fn new() -> CoreReader {
        CoreReader::default()
    }

    /// One NtQuerySystemInformation(SystemProcessorPerformanceInformation) into a buffer of logical_cpus() records.
    pub fn read(&mut self, out: &mut Vec<CoreTimes>) -> Result<(), SysmonError> {
        let want = logical_cpus() as usize * CORE_RECORD / 8;
        if self.buf.len() != want {
            self.buf = vec![0u64; want];
        }
        let cap = self.buf.len() * 8;
        let mut ret = 0u32;
        // SAFETY: the Vec owns cap initialised, 8-aligned bytes; the call writes at most cap bytes and ret.
        let st = unsafe {
            NtQuerySystemInformation(
                SystemProcessorPerformanceInformation,
                self.buf.as_mut_ptr().cast(),
                cap.min(u32::MAX as usize) as u32,
                &mut ret,
            )
        };
        if st.0 < 0 {
            out.clear();
            return Err(SysmonError::Status(st.0));
        }
        let got = (ret as usize).min(cap);
        // SAFETY: the Vec owns cap initialised bytes, u8 has alignment 1 and got never exceeds cap.
        let bytes = unsafe { std::slice::from_raw_parts(self.buf.as_ptr().cast::<u8>(), got) };
        parse_cores(bytes, out)
    }
}

fn delta(now: u64, prev: u64) -> u64 {
    now.saturating_sub(prev)
}

/// (delta dpc + delta interrupt) / sum of delta (kernel + user) x 100. 0.0 when the core counts differ or no time
/// passed.
pub fn dpc_irq_percent(prev: &[CoreTimes], now: &[CoreTimes]) -> f32 {
    if prev.len() != now.len() {
        return 0.0;
    }
    let (mut irq, mut total) = (0u128, 0u128);
    for (p, n) in prev.iter().zip(now) {
        irq += delta(n.dpc, p.dpc) as u128 + delta(n.interrupt, p.interrupt) as u128;
        total += delta(n.kernel, p.kernel) as u128 + delta(n.user, p.user) as u128;
    }
    if total == 0 {
        return 0.0;
    }
    (irq as f64 * 100.0 / total as f64).clamp(0.0, 100.0) as f32
}

/// Busy % per core (kernel time includes idle, so the total is kernel + user). Zeros when the core counts differ.
pub fn core_loads(prev: &[CoreTimes], now: &[CoreTimes], out: &mut Vec<f32>) {
    out.clear();
    if prev.len() != now.len() {
        out.resize(now.len(), 0.0);
        return;
    }
    out.extend(prev.iter().zip(now).map(|(p, n)| {
        let total = delta(n.kernel, p.kernel).saturating_add(delta(n.user, p.user));
        busy_fraction(delta(n.idle, p.idle), total)
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(c: CoreTimes) -> Vec<u8> {
        let mut r = Vec::new();
        for v in [c.idle, c.kernel, c.user, c.dpc, c.interrupt] {
            r.extend_from_slice(&v.to_le_bytes());
        }
        r.extend_from_slice(&7u32.to_le_bytes()); // InterruptCount
        r.extend_from_slice(&[0u8; 4]); // padding to 48
        assert_eq!(r.len(), CORE_RECORD);
        r
    }

    fn times(idle: u64, kernel: u64, user: u64, dpc: u64, interrupt: u64) -> CoreTimes {
        CoreTimes { idle, kernel, user, dpc, interrupt }
    }

    #[test]
    fn parse_cores_records() {
        let cores = [times(1, 10, 20, 3, 4), times(2, 11, 21, 5, 6), times(3, 12, 22, 7, 8), times(4, 13, 23, 9, 10)];
        let buf: Vec<u8> = cores.iter().flat_map(|c| record(*c)).collect();
        let mut out = vec![CoreTimes::default(); 9];
        parse_cores(&buf, &mut out).unwrap();
        assert_eq!(out, cores);
        let err = parse_cores(&buf[..buf.len() - 1], &mut out).unwrap_err();
        assert!(matches!(err, SysmonError::Layout { .. }));
        assert!(out.is_empty());
        parse_cores(&[], &mut out).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn dpc_share() {
        let prev = [times(0, 0, 0, 0, 0), times(0, 0, 0, 0, 0)];
        let now = [times(30, 60, 20, 2, 3), times(20, 10, 10, 3, 2)];
        assert_eq!(dpc_irq_percent(&prev, &now), 10.0);
        assert_eq!(dpc_irq_percent(&now, &now), 0.0);
        assert_eq!(dpc_irq_percent(&prev[..1], &now), 0.0);
        // Counters that go backwards give no negative share.
        assert_eq!(dpc_irq_percent(&now, &prev), 0.0);
    }

    #[test]
    fn core_loads_busy() {
        let prev = [times(0, 0, 0, 0, 0), times(0, 0, 0, 0, 0), times(5, 5, 0, 0, 0)];
        // Core 0: 25 idle of 100 -> 75 busy; core 1: all idle; core 2: unchanged.
        let now = [times(25, 80, 20, 0, 0), times(50, 50, 0, 0, 0), times(5, 5, 0, 0, 0)];
        let mut out = vec![9.0; 7];
        core_loads(&prev, &now, &mut out);
        assert_eq!(out, vec![75.0, 0.0, 0.0]);
        core_loads(&prev[..2], &now, &mut out);
        assert_eq!(out, vec![0.0; 3]);
    }

    #[test]
    fn core_reader_reads_every_logical_cpu() {
        let n = logical_cpus() as usize;
        let mut r = CoreReader::new();
        let mut a = Vec::new();
        r.read(&mut a).expect("read");
        if n <= 64 {
            assert_eq!(a.len(), n);
        }
        assert!(!a.is_empty());
        assert!(a.iter().all(|c| c.kernel >= c.idle || c.kernel > 0));
        let mut b = Vec::new();
        r.read(&mut b).expect("second read");
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(&b) {
            assert!(y.kernel >= x.kernel && y.user >= x.user && y.idle >= x.idle);
        }
    }
}
