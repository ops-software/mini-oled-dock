//! Memory readers.

use windows::Win32::System::ProcessStatus::{K32GetPerformanceInfo, PERFORMANCE_INFORMATION};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

fn status() -> Option<MEMORYSTATUSEX> {
    let mut ms = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    // SAFETY: `ms` is a valid MEMORYSTATUSEX with `dwLength` set.
    unsafe { GlobalMemoryStatusEx(&mut ms) }.ok()?;
    Some(ms)
}

/// GlobalMemoryStatusEx dwMemoryLoad, min(100): the dock's RAM %.
pub fn memory_load() -> Option<u8> {
    status().map(|ms| ms.dwMemoryLoad.min(100) as u8)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemSample {
    pub load_pct: u8,
    pub total_phys: u64,
    pub avail_phys: u64,
    pub commit_total: u64,
    pub commit_limit: u64,
}

impl MemSample {
    /// Committed share of the commit limit, 0.0..=100.0 (0 when the limit is unknown).
    pub fn commit_pct(&self) -> f32 {
        if self.commit_limit == 0 {
            return 0.0;
        }
        (self.commit_total as f64 * 100.0 / self.commit_limit as f64).clamp(0.0, 100.0) as f32
    }
    pub fn used_phys(&self) -> u64 {
        self.total_phys.saturating_sub(self.avail_phys)
    }
}

/// GlobalMemoryStatusEx + K32GetPerformanceInfo (pages x PageSize). The commit fields are 0 if only
/// the second call fails.
pub fn memory() -> Option<MemSample> {
    let ms = status()?;
    let mut pi =
        PERFORMANCE_INFORMATION { cb: std::mem::size_of::<PERFORMANCE_INFORMATION>() as u32, ..Default::default() };
    // SAFETY: `pi` is a valid PERFORMANCE_INFORMATION and `cb` is its size.
    let ok = unsafe { K32GetPerformanceInfo(&mut pi, pi.cb) }.as_bool();
    let (commit_total, commit_limit) = if ok {
        let page = pi.PageSize as u64;
        ((pi.CommitTotal as u64).saturating_mul(page), (pi.CommitLimit as u64).saturating_mul(page))
    } else {
        (0, 0)
    };
    Some(MemSample {
        load_pct: ms.dwMemoryLoad.min(100) as u8,
        total_phys: ms.ullTotalPhys,
        avail_phys: ms.ullAvailPhys,
        commit_total,
        commit_limit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_helpers() {
        let s = MemSample { load_pct: 50, total_phys: 100, avail_phys: 30, commit_total: 25, commit_limit: 100 };
        assert_eq!(s.used_phys(), 70);
        assert_eq!(s.commit_pct(), 25.0);
        assert_eq!(MemSample::default().commit_pct(), 0.0);
        assert_eq!(MemSample { avail_phys: 5, ..MemSample::default() }.used_phys(), 0);
    }

    #[test]
    fn system_times_and_memory_read() {
        assert!(memory_load().expect("load") <= 100);
        let m = memory().expect("memory");
        assert!(m.load_pct <= 100);
        assert!(m.commit_limit >= m.commit_total && m.commit_total > 0);
        assert!(m.total_phys >= m.avail_phys && m.avail_phys > 0);
    }
}
