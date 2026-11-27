//! The overlapped ReadDirectoryChangesW watcher: one thread, one I/O completion port. Every watched folder has one
//! pending read at any time; a completion parses the buffer, queues the changes under the coalescing rule and re-arms
//! the read. The thread waits on the port with the earliest due delivery as its timeout, and INFINITE when nothing is
//! due, so an idle watcher never wakes.
//!
//! Locking: `Inner` (the watches) is taken before the shared event queue, never the other way round. The
//! user callback runs with no lock held.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, ERROR_NOT_SUPPORTED, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadDirectoryChangesW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY,
    FILE_NOTIFY_CHANGE_ATTRIBUTES, FILE_NOTIFY_CHANGE_CREATION, FILE_NOTIFY_CHANGE_DIR_NAME,
    FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::IO::{
    CancelIoEx, CreateIoCompletionPort, GetQueuedCompletionStatus, PostQueuedCompletionStatus, OVERLAPPED,
};

use super::notify;
use super::{Coalescer, RawChange, Shared, WatchError, WatchEvent, WatchId};
use crate::listing::collate::utf16_to_wtf8;
use crate::listing::{Gen, WidePath};

/// The read buffer of a watch (the most a network folder accepts).
const BUF_BYTES: usize = 64 * 1024;

const ERROR_INVALID_FUNCTION: u32 = 1;
const ERROR_NOTIFY_ENUM_DIR: u32 = 1022;
const WAIT_TIMEOUT: u32 = 258;
const INFINITE: u32 = u32::MAX;

/// The buffer size of a new watch; a debug build lets a test shrink it (FILES_TEST_WATCH_BUF) to force overflows.
fn buf_bytes() -> usize {
    #[cfg(debug_assertions)]
    if let Some(n) = std::env::var("FILES_TEST_WATCH_BUF").ok().and_then(|v| v.parse::<usize>().ok()) {
        return n.clamp(64, BUF_BYTES) & !3;
    }
    BUF_BYTES
}

fn win32_code(e: &windows::core::Error) -> u32 {
    let hr = e.code().0 as u32;
    if hr >> 16 == 0x8007 {
        hr & 0xFFFF
    } else {
        hr
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    /// One read is pending on the handle.
    Armed,
    /// unwatch() cancelled the read and closed the handle; the aborted completion frees the watch.
    Closing,
    /// The folder went away (or a re-arm failed): the handle is closed, no read is pending, the id stays valid.
    Gone,
}

/// One watched folder. The OVERLAPPED is first and the box never moves, so the kernel's pointer stays valid until
/// the completion of the pending read has been taken off the port.
#[repr(C)]
struct Watch {
    ov: OVERLAPPED,
    handle: HANDLE,
    id: WatchId,
    gen: Gen,
    state: State,
    /// A read is pending in the kernel: set when ReadDirectoryChangesW succeeds, cleared only when this watch's own
    /// completion is taken off the port (whatever the state). The box may be freed only while this is false.
    pending: bool,
    buf: Box<[u32]>,
    co: Coalescer,
}

// SAFETY: a Watch is touched only under the Inner mutex; its handle and the OVERLAPPED pointers are plain values.
unsafe impl Send for Watch {}

impl Watch {
    /// Issues the one pending read. Called with the Inner lock held, on a watch with no read pending.
    fn arm(&mut self) -> Result<(), u32> {
        self.ov = OVERLAPPED::default();
        let filter = FILE_NOTIFY_CHANGE_FILE_NAME
            | FILE_NOTIFY_CHANGE_DIR_NAME
            | FILE_NOTIFY_CHANGE_ATTRIBUTES
            | FILE_NOTIFY_CHANGE_SIZE
            | FILE_NOTIFY_CHANGE_LAST_WRITE
            | FILE_NOTIFY_CHANGE_CREATION;
        // SAFETY: the handle is open and overlapped; the buffer and the OVERLAPPED live in this box, which is not
        // freed or moved until the completion of this read has been taken (Closing and the thread's shutdown wait
        // for it).
        unsafe {
            ReadDirectoryChangesW(
                self.handle,
                self.buf.as_mut_ptr().cast(),
                (self.buf.len() * 4) as u32,
                false,
                filter,
                None,
                Some(&mut self.ov),
                None,
            )
        }
        .map_err(|e| win32_code(&e))?;
        self.pending = true;
        Ok(())
    }

    fn close_handle(&mut self) {
        if !self.handle.is_invalid() {
            // SAFETY: the handle was opened by watch() and is closed once; the field is invalidated at once.
            let _ = unsafe { CloseHandle(self.handle) };
            self.handle = INVALID_HANDLE_VALUE;
        }
    }
}

struct Inner {
    watches: HashMap<u32, Box<Watch>>,
    quit: bool,
}

#[derive(Clone, Copy)]
struct Port(isize);

impl Port {
    fn h(self) -> HANDLE {
        HANDLE(self.0 as *mut _)
    }
}

/// The thread, the port and the watches.
pub(crate) struct Engine {
    port: Port,
    inner: Arc<Mutex<Inner>>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

fn lock(m: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Engine {
    pub(crate) fn new(shared: Arc<Shared>) -> Result<Engine, WatchError> {
        // SAFETY: a new port that no file is associated with yet; one concurrent thread.
        let port = unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, None, 0, 1) }
            .map_err(|e| WatchError::Open(win32_code(&e)))?;
        let port = Port(port.0 as isize);
        let inner = Arc::new(Mutex::new(Inner { watches: HashMap::new(), quit: false }));
        let (i2, s2) = (inner.clone(), shared.clone());
        let thread = std::thread::Builder::new()
            .name("files-watch".into())
            .spawn(move || run(port, i2, s2))
            .map_err(|_| WatchError::Open(0))?;
        Ok(Engine { port, inner, shared, thread: Some(thread) })
    }

    pub(crate) fn watch(&self, dir: &WidePath, gen: Gen, id: WatchId) -> Result<(), WatchError> {
        let path = dir.raw();
        // SAFETY: `path` is NUL-terminated and lives for the call; no security attributes or template.
        let handle = unsafe {
            CreateFileW(
                PCWSTR(path.as_ptr()),
                FILE_LIST_DIRECTORY.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                None,
            )
        }
        .map_err(|e| WatchError::Open(win32_code(&e)))?;
        let mut w = Box::new(Watch {
            ov: OVERLAPPED::default(),
            handle,
            id,
            gen,
            state: State::Armed,
            pending: false,
            buf: vec![0u32; buf_bytes() / 4].into_boxed_slice(),
            co: Coalescer::default(),
        });
        // SAFETY: the handle is a fresh overlapped directory handle; the key is the watch id (0 is the thread's own).
        if let Err(e) = unsafe { CreateIoCompletionPort(handle, Some(self.port.h()), id.0 as usize, 0) } {
            w.close_handle();
            return Err(WatchError::Open(win32_code(&e)));
        }
        let mut inner = lock(&self.inner);
        // The first read is issued under the lock, so the thread cannot see its completion before the insert.
        if let Err(code) = w.arm() {
            w.close_handle();
            return Err(if code == ERROR_INVALID_FUNCTION || code == ERROR_NOT_SUPPORTED.0 {
                WatchError::Unsupported
            } else {
                WatchError::Open(code)
            });
        }
        inner.watches.insert(id.0, w);
        self.shared.watches.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    pub(crate) fn unwatch(&self, id: WatchId) {
        let mut inner = lock(&self.inner);
        let Some(w) = inner.watches.get_mut(&id.0) else { return };
        match w.state {
            State::Armed => {
                w.state = State::Closing;
                // SAFETY: the handle is open and the OVERLAPPED is the pending read's; the box stays until the
                // aborted completion arrives.
                let _ = unsafe { CancelIoEx(w.handle, Some(&w.ov)) };
                w.close_handle();
                self.shared.watches.fetch_sub(1, Ordering::Relaxed);
            }
            State::Gone => {
                inner.watches.remove(&id.0);
                self.shared.watches.fetch_sub(1, Ordering::Relaxed);
            }
            State::Closing => {}
        }
        drop(inner);
        self.shared.events().retain(|e| !event_is_of(e, id));
    }
}

pub(crate) fn event_is_of(e: &WatchEvent, id: WatchId) -> bool {
    match e {
        WatchEvent::Changes { id: i, .. }
        | WatchEvent::Rescan { id: i, .. }
        | WatchEvent::Limited { id: i, .. }
        | WatchEvent::Gone { id: i, .. } => *i == id,
        WatchEvent::Drives(_) | WatchEvent::AssocChanged => false,
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        lock(&self.inner).quit = true;
        // SAFETY: the port is open until the thread has been joined below.
        let _ = unsafe { PostQueuedCompletionStatus(self.port.h(), 0, 0, None) };
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        // SAFETY: the thread is gone; nothing waits on the port any more.
        let _ = unsafe { CloseHandle(self.port.h()) };
    }
}

fn run(port: Port, inner: Arc<Mutex<Inner>>, shared: Arc<Shared>) {
    loop {
        let timeout = {
            let g = lock(&inner);
            if g.quit {
                break;
            }
            let now = Instant::now();
            match g.watches.values().filter_map(|w| w.co.due).min() {
                Some(due) => {
                    let d = due.saturating_duration_since(now);
                    u32::try_from(d.as_micros().div_ceil(1000)).unwrap_or(INFINITE - 1).max(1)
                }
                None => INFINITE,
            }
        };
        let (mut bytes, mut key, mut ov): (u32, usize, *mut OVERLAPPED) = (0, 0, std::ptr::null_mut());
        // SAFETY: the out pointers are locals; the port is open until Engine::drop has joined this thread.
        let got = unsafe { GetQueuedCompletionStatus(port.h(), &mut bytes, &mut key, &mut ov, timeout) };
        let mut woke = false;
        match got {
            Ok(()) if ov.is_null() => {} // the thread's own wake (key 0): the loop re-reads quit
            Ok(()) => woke = complete(&inner, &shared, key, ov, Ok(bytes)),
            Err(e) if ov.is_null() => {
                if win32_code(&e) == WAIT_TIMEOUT {
                    shared.timeouts.fetch_add(1, Ordering::Relaxed);
                } else {
                    break; // the port itself failed: nothing sensible is left to wait on
                }
            }
            Err(e) => woke = complete(&inner, &shared, key, ov, Err(win32_code(&e))),
        }
        woke |= flush_due(&inner, &shared);
        if woke {
            (shared.notify)();
        }
    }
    shutdown(port, &inner);
}

/// Cancels and closes every watch with a read pending (Armed and Closing alike); returns how many reads are
/// outstanding. `cancel` is called once per pending watch, before its handle is closed.
fn shutdown_begin(watches: &mut HashMap<u32, Box<Watch>>, mut cancel: impl FnMut(&Watch)) -> usize {
    let mut outstanding = 0;
    for w in watches.values_mut() {
        if w.pending {
            cancel(w);
            outstanding += 1;
        }
        w.close_handle();
        w.state = State::Closing;
    }
    // A watch with no read pending can be freed now.
    watches.retain(|_, w| w.pending);
    outstanding
}

/// Takes the completion `ov`: only when it is the OVERLAPPED of a watch that is still pending does it clear that
/// flag; a foreign or repeated pointer changes nothing. Returns the watch's id.
fn take_completion(watches: &mut HashMap<u32, Box<Watch>>, ov: *const OVERLAPPED) -> Option<u32> {
    let w = watches.values_mut().find(|w| w.pending && std::ptr::eq(&w.ov, ov))?;
    w.pending = false;
    Some(w.id.0)
}

/// One completion seen during shutdown: true when it ended an outstanding read (its watch is freed).
fn shutdown_complete(watches: &mut HashMap<u32, Box<Watch>>, ov: *const OVERLAPPED) -> bool {
    match take_completion(watches, ov) {
        Some(id) => {
            watches.remove(&id);
            true
        }
        None => false,
    }
}

/// At the deadline: the watches still pending are leaked (the kernel may still write into them), the others are
/// freed. Returns the ids leaked.
fn leak_pending(watches: &mut HashMap<u32, Box<Watch>>) -> Vec<u32> {
    let mut leaked = Vec::new();
    for (id, w) in watches.drain() {
        if w.pending {
            leaked.push(id);
            std::mem::forget(w);
        }
    }
    leaked
}

/// Cancels and closes every watch, then takes their completions so the buffers can be freed.
fn shutdown(port: Port, inner: &Mutex<Inner>) {
    let mut outstanding = {
        let mut g = lock(inner);
        // SAFETY: open handle, the pending read's OVERLAPPED.
        shutdown_begin(&mut g.watches, |w| {
            let _ = unsafe { CancelIoEx(w.handle, Some(&w.ov)) };
        })
    };
    let deadline = Instant::now() + Duration::from_secs(3);
    while outstanding > 0 && Instant::now() < deadline {
        let (mut bytes, mut key, mut ov): (u32, usize, *mut OVERLAPPED) = (0, 0, std::ptr::null_mut());
        // SAFETY: as in run().
        let _ = unsafe { GetQueuedCompletionStatus(port.h(), &mut bytes, &mut key, &mut ov, 100) };
        if !ov.is_null() && shutdown_complete(&mut lock(inner).watches, ov) {
            outstanding -= 1;
        }
    }
    // A completion that never came leaves its watch leaked, never freed.
    leak_pending(&mut lock(inner).watches);
}

/// One completion of watch `key`. Returns true when an event was queued.
fn complete(inner: &Mutex<Inner>, shared: &Shared, key: usize, ov: *mut OVERLAPPED, res: Result<u32, u32>) -> bool {
    let mut g = lock(inner);
    let id = key as u32;
    let Some(w) = g.watches.get_mut(&id) else { return false };
    if !w.pending || !std::ptr::eq(ov.cast_const(), &w.ov) {
        return false;
    }
    w.pending = false; // this watch's own completion is taken: no read is pending until arm() succeeds again
    if w.state == State::Closing {
        g.watches.remove(&id); // the cancelled (or already finished) read: now the buffer can go
        return false;
    }
    shared.completions.fetch_add(1, Ordering::Relaxed);
    let now = Instant::now();
    let mut woke;
    match res {
        Ok(0) => woke = rescan(shared, w, true),
        Ok(n) => {
            let bytes: &[u8] = {
                // SAFETY: the buffer holds n valid bytes written by the completed read; u32 to u8 never misaligns.
                let all = unsafe { std::slice::from_raw_parts(w.buf.as_ptr().cast::<u8>(), w.buf.len() * 4) };
                &all[..(n as usize).min(all.len())]
            };
            match notify::parse(bytes) {
                Ok(items) => {
                    let changes = items
                        .into_iter()
                        .map(|(action, name)| {
                            let mut wtf8 = Vec::with_capacity(name.len());
                            utf16_to_wtf8(&name, &mut wtf8);
                            RawChange { action, name: wtf8.into_boxed_slice() }
                        })
                        .collect();
                    woke = w.co.add(shared, w.id, w.gen, changes, now);
                }
                // A buffer that cannot be trusted is as good as an overflow.
                Err(_) => woke = rescan(shared, w, true),
            }
        }
        Err(code) if code == ERROR_NOTIFY_ENUM_DIR => woke = rescan(shared, w, true),
        Err(_) => {
            // The folder was deleted or moved (access denied, not found), the volume went away, or the read was
            // aborted from outside: nothing more will come.
            w.close_handle();
            w.state = State::Gone;
            shared.push(WatchEvent::Gone { id: w.id, gen: w.gen });
            return true;
        }
    }
    if w.arm().is_err() {
        w.close_handle();
        w.state = State::Gone;
        shared.push(WatchEvent::Gone { id: w.id, gen: w.gen });
        woke = true;
    }
    woke
}

/// Overflow: what was pending is covered by the re-enumeration the UI will run.
fn rescan(shared: &Shared, w: &mut Watch, count: bool) -> bool {
    if count {
        shared.overflows.fetch_add(1, Ordering::Relaxed);
    }
    w.co.clear();
    shared.push_rescan(w.id, w.gen)
}

fn flush_due(inner: &Mutex<Inner>, shared: &Shared) -> bool {
    let mut g = lock(inner);
    let now = Instant::now();
    let mut woke = false;
    for w in g.watches.values_mut() {
        woke |= w.co.flush(shared, w.id, w.gen, now);
    }
    woke
}

#[cfg(test)]
mod tests {
    use super::*;

    fn watch(id: u32, state: State, pending: bool) -> Watch {
        Watch {
            ov: OVERLAPPED::default(),
            handle: INVALID_HANDLE_VALUE,
            id: WatchId(id),
            gen: Gen(1),
            state,
            pending,
            buf: vec![0u32; 16].into_boxed_slice(),
            co: Coalescer::default(),
        }
    }

    fn map(ws: Vec<Watch>) -> HashMap<u32, Box<Watch>> {
        ws.into_iter().map(|w| (w.id.0, Box::new(w))).collect()
    }

    fn ov(m: &HashMap<u32, Box<Watch>>, id: u32) -> *const OVERLAPPED {
        &m[&id].ov
    }

    #[test]
    fn shutdown_counts_a_closing_watch_with_no_armed_one() {
        let mut m = map(vec![watch(1, State::Closing, true)]);
        let mut cancelled = 0;
        let n = shutdown_begin(&mut m, |_| cancelled += 1);
        assert_eq!((n, cancelled), (1, 1), "the pending read of a Closing watch is outstanding");
        assert_eq!(m.len(), 1, "its box stays until the completion is taken");
        let o = ov(&m, 1);
        assert!(shutdown_complete(&mut m, o));
        assert!(m.is_empty());
    }

    #[test]
    fn shutdown_frees_watches_with_nothing_pending_at_once() {
        let mut m =
            map(vec![watch(1, State::Gone, false), watch(2, State::Armed, true), watch(3, State::Closing, false)]);
        assert_eq!(shutdown_begin(&mut m, |_| {}), 1);
        assert_eq!(m.keys().copied().collect::<Vec<_>>(), vec![2]);
    }

    #[test]
    fn a_foreign_or_repeated_overlapped_decrements_nothing() {
        let mut m = map(vec![watch(1, State::Armed, true), watch(2, State::Closing, true)]);
        assert_eq!(shutdown_begin(&mut m, |_| {}), 2);
        let foreign = OVERLAPPED::default();
        assert!(!shutdown_complete(&mut m, &foreign));
        // The Closing watch's completion must not stand in for the Armed one's.
        let (o2, o1) = (ov(&m, 2), ov(&m, 1));
        assert!(shutdown_complete(&mut m, o2));
        assert!(!shutdown_complete(&mut m, o2), "a stale pointer of a freed watch matches nothing");
        assert!(m.contains_key(&1) && m[&1].pending, "the other read is still outstanding");
        assert!(shutdown_complete(&mut m, o1));
        assert!(m.is_empty());
    }

    #[test]
    fn a_completion_clears_pending_in_any_state() {
        let mut m = map(vec![watch(1, State::Closing, true), watch(2, State::Armed, true)]);
        let (o1, o2) = (ov(&m, 1), ov(&m, 2));
        assert_eq!(take_completion(&mut m, o1), Some(1));
        assert!(!m[&1].pending && m[&2].pending);
        assert_eq!(take_completion(&mut m, o1), None, "not pending any more");
        assert_eq!(take_completion(&mut m, o2), Some(2));
    }

    #[test]
    fn the_deadline_leaks_only_the_pending_watches() {
        let mut m =
            map(vec![watch(1, State::Closing, true), watch(2, State::Closing, false), watch(3, State::Armed, true)]);
        let mut leaked = leak_pending(&mut m);
        leaked.sort();
        assert_eq!(leaked, vec![1, 3]);
        assert!(m.is_empty());
    }
}
