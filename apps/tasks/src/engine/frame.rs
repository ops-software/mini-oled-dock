//! The data one sampler tick produces. Real: the samplers fill it, the window reads it.
use crate::engine::need::NeedSet;
use crate::model::classify::PathMap;
use crate::model::verdict::SysState;
use oled_sysmon::cpu::CpuDelta;
use oled_sysmon::mem::MemSample;
use oled_sysmon::percore::{dpc_irq_percent, CoreTimes};
use oled_sysmon::{ProcKey, ProcTable, SysmonError};
use std::sync::Arc;

#[derive(Clone, Debug, Default)]
pub struct SysSample {
    pub cpu: Option<CpuDelta>,
    pub mem: Option<MemSample>,
    pub cores: Vec<CoreTimes>,
    pub logical_cpus: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowFact {
    pub owner: ProcKey,
    pub hwnd: isize,
    pub hung: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceEntry {
    pub pid: u32,
    pub name: Box<str>,
    pub display: Box<str>,
}

/// Sorted by pid, then name.
#[derive(Clone, Debug, Default)]
pub struct ServiceMap {
    pub entries: Vec<ServiceEntry>,
}

impl ServiceMap {
    /// The entries of one pid (binary search on the sorted entries).
    pub fn for_pid(&self, pid: u32) -> &[ServiceEntry] {
        let lo = self.entries.partition_point(|e| e.pid < pid);
        let hi = self.entries.partition_point(|e| e.pid <= pid);
        self.entries.get(lo..hi).unwrap_or(&[])
    }
}

#[derive(Debug, Default)]
pub struct Frame {
    /// t_us: the host clock (QPC in microseconds) at the tick's start.
    pub seq: u64,
    pub t_us: u64,
    pub dt_ms: u32,
    pub need: NeedSet,
    pub procs: ProcTable,
    pub sys: SysSample,
    /// Relative to the previous frame the consumer took (SamplerHost::take).
    pub added: Vec<ProcKey>,
    pub gone: Vec<ProcKey>,
    /// Shared, never deep-copied per tick.
    pub paths: Arc<PathMap>,
    pub paths_fresh: bool,
    pub paths_us: u32,
    pub windows: Vec<WindowFact>,
    pub windows_fresh: bool,
    pub services: Arc<ServiceMap>,
    pub services_fresh: bool,
    /// Win32 code of the last failed enumeration, 258 (WAIT_TIMEOUT) while one hangs.
    pub services_error: Option<u32>,
    /// error: the snapshot failed; such a frame carries no procs (take, TQ4).
    pub error: Option<SysmonError>,
    pub tick_us: u32,
}

pub trait Sampler: Send {
    fn need(&self) -> crate::engine::need::Need;
    fn sample(&mut self, now_us: u64, out: &mut Frame) -> Result<(), SysmonError>;
}

/// The machine facts of one frame for the verdict and the header. `prev_cores` is the previous taken frame's cores
/// (the DPC and interrupt share is a delta); missing readings count as 0.
pub fn sys_state(f: &Frame, prev_cores: &[CoreTimes], paused: bool) -> SysState {
    SysState {
        cpu_pct: f.sys.cpu.map_or(0.0, |c| c.fraction()),
        mem_load_pct: f.sys.mem.map_or(0, |m| m.load_pct),
        commit_pct: f.sys.mem.map_or(0.0, |m| m.commit_pct()),
        dpc_irq_pct: dpc_irq_percent(prev_cores, &f.sys.cores),
        logical_cpus: f.sys.logical_cpus,
        paused,
        read_error: f.error.map(|e| e.code()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sys_state_from_a_frame() {
        let mut f = Frame::default();
        let empty = sys_state(&f, &[], false);
        assert_eq!(empty, SysState::default());
        f.sys.cpu = Some(CpuDelta { d_idle: 25, d_total: 100 });
        f.sys.mem =
            Some(MemSample { load_pct: 42, total_phys: 100, avail_phys: 58, commit_total: 25, commit_limit: 100 });
        f.sys.logical_cpus = 8;
        let core = |kernel, user, dpc, interrupt| CoreTimes { idle: 0, kernel, user, dpc, interrupt };
        f.sys.cores = vec![core(60, 20, 2, 3), core(10, 10, 3, 2)];
        let prev = vec![CoreTimes::default(); 2];
        let s = sys_state(&f, &prev, true);
        assert_eq!(s.cpu_pct, 75.0);
        assert_eq!(s.mem_load_pct, 42);
        assert_eq!(s.commit_pct, 25.0);
        assert_eq!(s.dpc_irq_pct, 10.0);
        assert_eq!((s.logical_cpus, s.paused, s.read_error), (8, true, None));
        f.error = Some(SysmonError::Status(-5));
        assert_eq!(sys_state(&f, &prev, false).read_error, Some(-5));
        // No previous cores (first frame): no DPC share.
        assert_eq!(sys_state(&f, &[], false).dpc_irq_pct, 0.0);
    }
}
