//! The UI's client of the shell host (SH6-SH8): spawns files.exe --role=shell with inherited anonymous pipes, answers
//! each request with the host's reply or a Fallback in time, and never waits on a lock another thread holds. Filled
//! by F-SH.

use oled_trust::frame::files::{CallKind, ReqId, ShellEvt, ShellReq};

pub use crate::host::TestHooks;

/// TestHooks.env carries FILES_FAULT_* to the host child.
pub struct ShellConfig {
    pub exe: std::path::PathBuf,
    pub test: Option<TestHooks>,
}
/// TooLarge: a whole frame over max_payload - SPLIT_MARGIN.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FallbackWhy {
    Deadline,
    HostDied,
    Disabled,
    Updated,
    Unavailable,
    TooLarge,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShellReply {
    Evt(ShellEvt),
    Fallback { req: ReqId, why: FallbackWhy },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostState {
    Down,
    Starting,
    Ready { pid: u32 },
    Busy { pid: u32 },
    MenuOff,
}

pub struct ShellClient {
    _private: (),
}

impl ShellClient {
    pub fn new(cfg: ShellConfig, notify: std::sync::Arc<dyn Fn() + Send + Sync>) -> Self {
        let _ = (cfg, notify);
        todo!("F-SH")
    }
    pub fn prespawn(&self) {
        todo!("F-SH")
    }
    pub fn next_req(&self) -> ReqId {
        todo!("F-SH")
    }
    /// Returns at once; Fallback replies when it cannot be served in time.
    pub fn request(&self, r: ShellReq) {
        let _ = r;
        todo!("F-SH")
    }
    pub fn drain(&self) -> Vec<ShellReply> {
        todo!("F-SH")
    }
    pub fn state(&self) -> HostState {
        todo!("F-SH")
    }
    pub fn disabled(&self, kind: CallKind, ext: &str) -> bool {
        let _ = (kind, ext);
        todo!("F-SH")
    }
    pub fn host_pid(&self) -> Option<u32> {
        todo!("F-SH")
    }
    /// Ends a host that is not busy.
    pub fn shutdown(&self) {
        todo!("F-SH")
    }
}
