//! The enumeration pool (two workers, stuck-share handling, network queue): filled by F-ENUM (EN3).

use super::enum_worker::Enumerator;
use super::types::{Batch, CancelToken, TabId};
use super::{Gen, WidePath};

pub type BatchSink = std::sync::Arc<dyn Fn(TabId, Batch) + Send + Sync>;
pub type MakeEnumerator = std::sync::Arc<dyn Fn() -> Box<dyn Enumerator> + Send + Sync>;

pub struct EnumJob {
    pub tab: TabId,
    pub gen: Gen,
    pub dir: WidePath,
    pub cancel: CancelToken,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct PoolStats {
    pub live: u32,
    pub stuck: u32,
    pub queued_network: u32,
}

pub struct EnumPool {
    _private: (),
}

impl EnumPool {
    /// No thread until the first submit.
    pub fn new(sink: BatchSink) -> Self {
        let _ = sink;
        todo!("F-ENUM")
    }

    /// Tests and probes inject fakes.
    pub fn with_enumerator(sink: BatchSink, make: MakeEnumerator) -> Self {
        let _ = (sink, make);
        todo!("F-ENUM")
    }

    pub fn submit(&self, job: EnumJob) {
        let _ = job;
        todo!("F-ENUM")
    }

    pub fn stats(&self) -> PoolStats {
        todo!("F-ENUM")
    }
}

/// UNC, or GetDriveTypeW == DRIVE_REMOTE; call off the UI thread.
pub fn is_network(dir: &WidePath) -> bool {
    let _ = dir;
    todo!("F-ENUM")
}
