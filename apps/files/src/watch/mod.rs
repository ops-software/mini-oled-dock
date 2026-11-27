//! Change notifications (SPEC-files 5.5.6): ReadDirectoryChangesW on an I/O completion port, the shell's drive and
//! association notifications, and resolve() that turns raw changes into a store Diff. Filled by F-WATCH, which also
//! adds its submodules here.

pub mod drives;
mod iocp;
pub mod notify;
mod resolve;
mod shellnotify;

pub use resolve::resolve;

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::listing::{Gen, WidePath};

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

/// A watch's later changes wait until this long after its last delivery.
const COALESCE: Duration = Duration::from_millis(100);

/// The delivery rule of one watch: the first change after a quiet period goes out at once; later ones are held and
/// go out 100 ms after the last delivery. The owner's thread waits for `due` (and for nothing when it is None).
#[derive(Default)]
pub(crate) struct Coalescer {
    pending: Vec<RawChange>,
    last: Option<Instant>,
    pub(crate) due: Option<Instant>,
}

impl Coalescer {
    /// Queues `items`; true when a Changes event was pushed.
    pub(crate) fn add(&mut self, shared: &Shared, id: WatchId, gen: Gen, items: Vec<RawChange>, now: Instant) -> bool {
        self.pending.extend(items);
        if self.pending.is_empty() {
            return false;
        }
        match self.last {
            Some(t) if now < t + COALESCE => {
                self.due = Some(t + COALESCE);
                false
            }
            _ => self.deliver(shared, id, gen, now),
        }
    }

    /// Delivers what is held once it is due.
    pub(crate) fn flush(&mut self, shared: &Shared, id: WatchId, gen: Gen, now: Instant) -> bool {
        self.due.is_some_and(|d| d <= now) && self.deliver(shared, id, gen, now)
    }

    /// Drops what is held (an overflow's rescan covers it).
    pub(crate) fn clear(&mut self) {
        self.pending.clear();
        self.due = None;
    }

    fn deliver(&mut self, shared: &Shared, id: WatchId, gen: Gen, now: Instant) -> bool {
        self.last = Some(now);
        self.due = None;
        if self.pending.is_empty() {
            return false;
        }
        shared.push(WatchEvent::Changes { id, gen, items: std::mem::take(&mut self.pending) });
        true
    }
}

/// What the thread and the callers share: the event queue, the counters and the caller's wake-up.
pub(crate) struct Shared {
    events: std::sync::Mutex<Vec<WatchEvent>>,
    notify: std::sync::Arc<dyn Fn() + Send + Sync>,
    completions: AtomicU64,
    timeouts: AtomicU64,
    overflows: AtomicU64,
    watches: AtomicU32,
    /// The drives as last seen; None while drives(true) has not been asked.
    drives: Mutex<Option<drives::Snapshot>>,
}

impl Shared {
    /// Takes a drive snapshot and queues Drives(diff) when it differs from the last one. True when queued.
    pub(crate) fn check_drives(&self) -> bool {
        let mut base = self.drives.lock().unwrap_or_else(|p| p.into_inner());
        let Some(old) = base.as_ref() else { return false };
        let now = drives::snapshot();
        let diff = drives::diff(old, &now);
        *base = Some(now);
        drop(base);
        if diff == DriveDiff::default() {
            return false;
        }
        self.push(WatchEvent::Drives(diff));
        true
    }

    pub(crate) fn events(&self) -> std::sync::MutexGuard<'_, Vec<WatchEvent>> {
        self.events.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn push(&self, e: WatchEvent) {
        self.events().push(e);
    }

    /// A Rescan unless one for the same watch and generation is already waiting to be drained. True when queued.
    pub(crate) fn push_rescan(&self, id: WatchId, gen: Gen) -> bool {
        let mut q = self.events();
        if q.iter().any(|e| matches!(e, WatchEvent::Rescan { id: i, gen: g } if *i == id && *g == gen)) {
            return false;
        }
        q.push(WatchEvent::Rescan { id, gen });
        true
    }
}

pub struct Watcher {
    shared: Arc<Shared>,
    engine: Mutex<Option<iocp::Engine>>,
    /// The second thread: only while a fallback folder or drives(true) needs it.
    shell: Mutex<Option<shellnotify::Shell>>,
    /// The ids watched through the shell instead of ReadDirectoryChangesW.
    fallback: Mutex<HashSet<u32>>,
    drives_on: AtomicBool,
    next_id: AtomicU32,
}

impl Watcher {
    /// Threads start at the first watch or drives(true).
    pub fn new(notify: Arc<dyn Fn() + Send + Sync>) -> Self {
        Watcher {
            shared: Arc::new(Shared {
                events: Mutex::new(Vec::new()),
                notify,
                completions: AtomicU64::new(0),
                timeouts: AtomicU64::new(0),
                overflows: AtomicU64::new(0),
                watches: AtomicU32::new(0),
                drives: Mutex::new(None),
            }),
            engine: Mutex::new(None),
            shell: Mutex::new(None),
            fallback: Mutex::new(HashSet::new()),
            drives_on: AtomicBool::new(false),
            next_id: AtomicU32::new(1),
        }
    }

    pub fn watch(&self, dir: &WidePath, gen: Gen) -> Result<WatchId, WatchError> {
        let id = WatchId(self.next_id.fetch_add(1, Ordering::Relaxed));
        if !Self::forced_fallback() {
            let mut engine = self.engine.lock().unwrap_or_else(|p| p.into_inner());
            if engine.is_none() {
                *engine = Some(iocp::Engine::new(self.shared.clone())?);
            }
            match engine.as_ref().map(|e| e.watch(dir, gen, id)) {
                Some(Ok(())) => return Ok(id),
                Some(Err(WatchError::Unsupported)) | None => {} // this file system refuses: the shell's notifications
                Some(Err(e)) => return Err(e),
            }
        }
        self.watch_by_shell(dir, gen, id)?;
        self.shared.push(WatchEvent::Limited { id, gen });
        (self.shared.notify)();
        Ok(id)
    }

    /// A debug build can force the fallback for a local folder (FILES_TEST_WATCH_SHELL=1).
    fn forced_fallback() -> bool {
        cfg!(debug_assertions) && std::env::var("FILES_TEST_WATCH_SHELL").is_ok_and(|v| v == "1")
    }

    fn watch_by_shell(&self, dir: &WidePath, gen: Gen, id: WatchId) -> Result<(), WatchError> {
        let mut shell = self.shell.lock().unwrap_or_else(|p| p.into_inner());
        if shell.is_none() {
            *shell = Some(shellnotify::Shell::new(self.shared.clone())?);
        }
        let Some(s) = shell.as_ref() else { return Err(WatchError::Unsupported) };
        s.add_folder(id, gen, dir)?;
        self.fallback.lock().unwrap_or_else(|p| p.into_inner()).insert(id.0);
        self.shared.watches.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    pub fn unwatch(&self, id: WatchId) {
        if self.fallback.lock().unwrap_or_else(|p| p.into_inner()).remove(&id.0) {
            if let Some(s) = self.shell.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
                s.remove_folder(id);
            }
            self.shared.watches.fetch_sub(1, Ordering::Relaxed);
            self.shared.events().retain(|e| !iocp::event_is_of(e, id));
            self.reap_shell();
        } else if let Some(e) = self.engine.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
            e.unwatch(id);
        }
    }

    /// Ends the shell thread when no fallback folder and no drive watch needs it.
    fn reap_shell(&self) {
        let busy = !self.fallback.lock().unwrap_or_else(|p| p.into_inner()).is_empty()
            || self.drives_on.load(Ordering::Relaxed);
        if !busy {
            let gone = self.shell.lock().unwrap_or_else(|p| p.into_inner()).take();
            drop(gone); // joins the thread
        }
    }

    /// Drive add/remove and association changes.
    pub fn drives(&self, on: bool) {
        if on == self.drives_on.swap(on, Ordering::Relaxed) {
            return;
        }
        if on {
            *self.shared.drives.lock().unwrap_or_else(|p| p.into_inner()) = Some(drives::snapshot());
            let mut shell = self.shell.lock().unwrap_or_else(|p| p.into_inner());
            if shell.is_none() {
                *shell = shellnotify::Shell::new(self.shared.clone()).ok();
            }
            if let Some(s) = shell.as_ref() {
                s.drives(true);
            }
            // Without the shell thread the UI's poke_devices() still works.
        } else {
            if let Some(s) = self.shell.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
                s.drives(false);
            }
            *self.shared.drives.lock().unwrap_or_else(|p| p.into_inner()) = None;
            self.reap_shell();
        }
    }

    /// The UI calls this on WM_DEVICECHANGE.
    pub fn poke_devices(&self) {
        if self.shared.check_drives() {
            (self.shared.notify)();
        }
    }

    pub fn drain(&self) -> Vec<WatchEvent> {
        std::mem::take(&mut *self.shared.events())
    }

    pub fn stats(&self) -> WatchStats {
        let s = &self.shared;
        WatchStats {
            completions: s.completions.load(Ordering::Relaxed),
            timeouts: s.timeouts.load(Ordering::Relaxed),
            overflows: s.overflows.load(Ordering::Relaxed),
            watches: s.watches.load(Ordering::Relaxed),
        }
    }
}
