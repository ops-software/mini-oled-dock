//! The process and per-core samplers.
use crate::engine::frame::{Frame, Sampler};
use crate::engine::need::Need;
use oled_sysmon::percore::CoreReader;
use oled_sysmon::{SnapshotReader, SysmonError};

#[derive(Default)]
pub struct ProcSampler {
    reader: SnapshotReader,
}

impl ProcSampler {
    pub fn new() -> ProcSampler {
        ProcSampler::default()
    }
}

/// need() = Procs; on error clears out.procs and sets out.error (no fifth table).
impl Sampler for ProcSampler {
    fn need(&self) -> Need {
        Need::Procs
    }
    fn sample(&mut self, _now_us: u64, out: &mut Frame) -> Result<(), SysmonError> {
        match self.reader.read(&mut out.procs) {
            Ok(()) => {
                out.error = None;
                Ok(())
            }
            Err(e) => {
                out.procs.clear();
                out.error = Some(e);
                Err(e)
            }
        }
    }
}

#[derive(Default)]
pub struct CoreSampler {
    reader: CoreReader,
}

impl CoreSampler {
    pub fn new() -> CoreSampler {
        CoreSampler::default()
    }
}

/// need() = PerCore; fills out.sys.cores.
impl Sampler for CoreSampler {
    fn need(&self) -> Need {
        Need::PerCore
    }
    fn sample(&mut self, _now_us: u64, out: &mut Frame) -> Result<(), SysmonError> {
        self.reader.read(&mut out.sys.cores)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_sampler_reads_this_machine() {
        let mut s = ProcSampler::new();
        let mut f = Frame::default();
        s.sample(0, &mut f).expect("snapshot");
        assert!(f.error.is_none());
        assert!(f.procs.len() > 10);
        let me = std::process::id();
        assert!(f.procs.find_pid(me).is_some());
        assert_eq!(s.need(), Need::Procs);
    }

    #[test]
    fn core_sampler_fills_the_cores() {
        let mut s = CoreSampler::new();
        let mut f = Frame::default();
        s.sample(0, &mut f).expect("cores");
        assert!(!f.sys.cores.is_empty());
        assert_eq!(s.need(), Need::PerCore);
    }
}
