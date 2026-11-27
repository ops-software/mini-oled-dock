//! The UI's client of the ops host (OP6): spawns files.exe --role=ops on the first request, speaks the ops protocol
//! over the named pipe and hands events back through drain(). Filled by F-OPS.

use oled_trust::frame::files::*;

pub use crate::host::TestHooks;

pub struct OpsConfig {
    pub exe: std::path::PathBuf,
    pub data_dir: std::path::PathBuf,
    pub owner: u64,
    pub test: Option<TestHooks>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpsError {
    Unavailable(&'static str),
    Refused(Refusal),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UndoRecord {
    pub of: OpId,
    pub kind: UndoKind,
    pub replaced: u32,
    pub entries: Vec<UndoEntry>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpsEvent {
    Evt(OpEvt),
    Undo(UndoRecord),
    HostLost,
    HostUpdated,
}

pub struct OpsClient {
    _private: (),
}

impl OpsClient {
    /// No process before the first request.
    pub fn new(cfg: OpsConfig, notify: std::sync::Arc<dyn Fn() + Send + Sync>) -> Self {
        let _ = (cfg, notify);
        todo!("F-OPS")
    }
    pub fn copy(&self, items: &[ItemRef], dest: &WidePath, c: Conflicts) -> Result<OpId, OpsError> {
        let _ = (items, dest, c);
        todo!("F-OPS")
    }
    pub fn move_to(&self, items: &[ItemRef], dest: &WidePath, c: Conflicts) -> Result<OpId, OpsError> {
        let _ = (items, dest, c);
        todo!("F-OPS")
    }
    pub fn recycle(&self, items: &[ItemRef]) -> Result<OpId, OpsError> {
        let _ = items;
        todo!("F-OPS")
    }
    pub fn delete_permanent(&self, items: &[ItemRef]) -> Result<OpId, OpsError> {
        let _ = items;
        todo!("F-OPS")
    }
    pub fn rename(&self, item: &WidePath, to: &str) -> Result<OpId, OpsError> {
        let _ = (item, to);
        todo!("F-OPS")
    }
    pub fn new_folder(&self, parent: &WidePath, name: &str) -> Result<OpId, OpsError> {
        let _ = (parent, name);
        todo!("F-OPS")
    }
    pub fn undo(&self, rec: &UndoRecord) -> Result<OpId, OpsError> {
        let _ = rec;
        todo!("F-OPS")
    }
    pub fn cancel(&self, op: OpId) {
        let _ = op;
        todo!("F-OPS")
    }
    pub fn pause(&self, op: OpId, on: bool) {
        let _ = (op, on);
        todo!("F-OPS")
    }
    pub fn bring_forward(&self, op: OpId) {
        let _ = op;
        todo!("F-OPS")
    }
    pub fn resume_rest(&self, of: OpId) -> Result<OpId, OpsError> {
        let _ = of;
        todo!("F-OPS")
    }
    pub fn ack_done(&self, of: OpId) {
        let _ = of;
        todo!("F-OPS")
    }
    /// For AllowSetForegroundWindow before bring_forward.
    pub fn host_pid(&self) -> Option<u32> {
        todo!("F-OPS")
    }
    /// UndoPart frames come back assembled as OpsEvent::Undo (at most UNDO_MAX_ENTRIES; no Undo event for an op whose
    /// record was dropped).
    pub fn drain(&self) -> Vec<OpsEvent> {
        todo!("F-OPS")
    }
    /// Bye; running operations continue in the host.
    pub fn shutdown(&self) {
        todo!("F-OPS")
    }
}
