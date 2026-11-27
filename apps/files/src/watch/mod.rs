//! Change notifications (SPEC-files 5.5.6): ReadDirectoryChangesW on an I/O completion port, the shell's drive and
//! association notifications, and resolve() that turns raw changes into a store Diff. Filled by F-WATCH, which also
//! adds its submodules here.

use crate::listing::{Diff, Gen, WidePath};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WatchId(pub u32);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeAction {
    Added,
    Removed,
    Modified,
    RenamedFrom,
    RenamedTo,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawChange {
    pub action: ChangeAction,
    pub name: Box<[u8]>,
} // WTF-8, one component
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct DriveDiff {
    pub added: u32,
    pub removed: u32,
} // bit n = drive letter A + n
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WatchEvent {
    Changes { id: WatchId, gen: Gen, items: Vec<RawChange> },
    Rescan { id: WatchId, gen: Gen },  // overflow: re-enumerate and rescan_merge
    Limited { id: WatchId, gen: Gen }, // fell back to shell notifications ("Live updates limited")
    Gone { id: WatchId, gen: Gen },    // the folder itself went away
    Drives(DriveDiff),
    AssocChanged, // SHCNE_ASSOCCHANGED (icons.bin rebuild)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchError {
    Open(u32),
    Unsupported,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WatchStats {
    pub completions: u64,
    pub timeouts: u64,
    pub overflows: u64,
    pub watches: u32,
}

pub struct Watcher {
    _private: (),
}

impl Watcher {
    /// Threads start at the first watch or drives(true).
    pub fn new(notify: std::sync::Arc<dyn Fn() + Send + Sync>) -> Self {
        let _ = notify;
        todo!("F-WATCH")
    }
    pub fn watch(&self, dir: &WidePath, gen: Gen) -> Result<WatchId, WatchError> {
        let _ = (dir, gen);
        todo!("F-WATCH")
    }
    pub fn unwatch(&self, id: WatchId) {
        let _ = id;
        todo!("F-WATCH")
    }
    /// Drive add/remove and association changes.
    pub fn drives(&self, on: bool) {
        let _ = on;
        todo!("F-WATCH")
    }
    /// The UI calls this on WM_DEVICECHANGE.
    pub fn poke_devices(&self) {
        todo!("F-WATCH")
    }
    pub fn drain(&self) -> Vec<WatchEvent> {
        todo!("F-WATCH")
    }
    pub fn stats(&self) -> WatchStats {
        todo!("F-WATCH")
    }
}

/// Stats added, modified and renamed-to names; run off the UI thread.
pub fn resolve(dir: &WidePath, gen: Gen, items: &[RawChange]) -> Diff {
    let _ = (dir, gen, items);
    todo!("F-WATCH")
}
