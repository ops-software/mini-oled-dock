//! The UI's client of the shell host (SH7b, SH7c): spawns files.exe --role=shell with inherited anonymous pipes, a
//! crumbs section and a handle of this process; answers each request with the host's reply or a Fallback in time; and
//! never waits on a lock another thread holds. request() encodes and hands the bytes to the supervisor thread over an
//! unbounded channel; drain() reads the reply channel; state() reads one atomic. The supervisor thread owns the host:
//! it spawns it, writes through a writer thread, reads through a reader thread, watches the deadlines and the crumbs,
//! kills through the KILLING protocol only, records the breakers and respawns on the next request.

use std::io::{Read, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, TryLockError};

use oled_trust::frame::files::{CallKind, ItemRef, ReqId, ShellEvt, ShellReq, SHELL};
use oled_trust::frame::{self, Assembler, FrameError, WireLen, SPLIT_MARGIN};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::Threading::{CreateEventW, SetEvent, TerminateProcess, WaitForMultipleObjects, INFINITE};
use windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow;

pub use crate::host::TestHooks;
use crate::host::{anon, proc};
use crate::shell::breaker::Breakers;
use crate::shell::crumbs::SlotView;
use crate::shell::host as shell_host;
use crate::shell::supervisor::{self, Outstanding, Wait};
use crate::shell::{crumbs, writer};

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

/// Contended locks taken by request, drain, state and disabled (debug builds): stays 0 (UI_WAITS of the plan).
#[cfg(debug_assertions)]
pub static UI_WAITS: AtomicU64 = AtomicU64::new(0);

/// Counts a contended acquisition on the UI's side (debug builds): in UI_WAITS and in the client's own count.
fn waited(own: &AtomicU64) {
    #[cfg(debug_assertions)]
    UI_WAITS.fetch_add(1, Ordering::Relaxed);
    own.fetch_add(1, Ordering::Relaxed);
}

/// A Mutex as the UI thread takes it: a contended acquisition is counted, then waited for.
fn lock_ui<'a, T>(m: &'a Mutex<T>, own: &AtomicU64) -> MutexGuard<'a, T> {
    match m.try_lock() {
        Ok(g) => g,
        Err(TryLockError::Poisoned(p)) => p.into_inner(),
        Err(TryLockError::WouldBlock) => {
            waited(own);
            m.lock().unwrap_or_else(PoisonError::into_inner)
        }
    }
}

/// An RwLock read as the UI thread takes it (the supervisor writes it only for a breaker record or a spawn).
fn read_ui<'a, T>(l: &'a RwLock<T>, own: &AtomicU64) -> RwLockReadGuard<'a, T> {
    match l.try_read() {
        Ok(g) => g,
        Err(TryLockError::Poisoned(p)) => p.into_inner(),
        Err(TryLockError::WouldBlock) => {
            waited(own);
            l.read().unwrap_or_else(PoisonError::into_inner)
        }
    }
}

/// The encoded size of a request's item list is over a whole frame's limit (checked before encoding anything).
fn too_large(r: &ShellReq) -> bool {
    let items = match r {
        ShellReq::Menu { items, .. }
        | ShellReq::ModernItems { items, .. }
        | ShellReq::Exec { items, .. }
        | ShellReq::Icons { items, .. }
        | ShellReq::Thumbs { items, .. } => items,
        _ => return false,
    };
    let bytes: usize = items.iter().map(WireLen::wire_len).sum();
    bytes > (SHELL.max_payload - SPLIT_MARGIN) as usize
}

fn write_lock<T>(l: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    l.write().unwrap_or_else(PoisonError::into_inner)
}

fn now_ms() -> u64 {
    // SAFETY: no preconditions.
    unsafe { GetTickCount64() }
}

/// An auto-reset event that wakes the supervisor; shared with the readers.
struct Wake(OwnedHandle);

impl Wake {
    fn new() -> std::io::Result<Arc<Wake>> {
        // SAFETY: an unnamed auto-reset event, owned below.
        let h = unsafe { CreateEventW(None, false, false, None) }?;
        // SAFETY: just created, owned here once.
        Ok(Arc::new(Wake(unsafe { OwnedHandle::from_raw_handle(h.0) })))
    }
    fn handle(&self) -> HANDLE {
        HANDLE(self.0.as_raw_handle())
    }
    fn set(&self) {
        // SAFETY: the event is open while self lives.
        let _ = unsafe { SetEvent(self.handle()) };
    }
}

/// What a reader saw from its host.
enum FromHost {
    Hello(Result<(), FrameError>),
    Evt(ShellEvt),
    End,
}

/// The supervisor's input: the UI's commands and the readers' events.
enum Cmd {
    Req {
        req: ReqId,
        wait: Wait,
        kind: CallKind,
        ext: String,
        bytes: Vec<u8>,
        foreground: bool,
        /// When the UI asked (ms): the deadlines count from here.
        since: u64,
    },
    /// A request with no reply (CancelGen).
    Raw(Vec<u8>),
    Prespawn,
    Shutdown,
    Quit,
    Host {
        gen: u32,
        got: FromHost,
    },
}

/// The call kind, the wait and the extension a request is supervised and attributed with.
fn classify(r: &ShellReq) -> (CallKind, Wait, String) {
    let ext = |items: &[ItemRef]| match items.first() {
        Some(ItemRef::Path(p)) => crumbs::ext_of(p),
        _ => String::new(),
    };
    match r {
        ShellReq::Menu { items, .. } => (CallKind::QueryContextMenu, Wait::MenuShown, ext(items)),
        ShellReq::ModernItems { items, .. } => (CallKind::ModernItems, Wait::ModernDone, ext(items)),
        ShellReq::Exec { items, .. } => (CallKind::Exec, Wait::Busy, ext(items)),
        ShellReq::RunTyped { .. } => (CallKind::Exec, Wait::Busy, String::new()),
        ShellReq::Icons { items, .. } => (CallKind::Icon, Wait::Work, ext(items)),
        ShellReq::Thumbs { items, .. } => (CallKind::Thumb, Wait::Work, ext(items)),
        // Type names are read-only shell calls like class icons: the same 2,000 ms crumb deadline and breakers.
        ShellReq::ClassIcons { exts, .. } | ShellReq::TypeNames { exts, .. } => {
            (CallKind::ClassIcon, Wait::Work, exts.first().map(|e| e.to_string()).unwrap_or_default())
        }
        ShellReq::ShellNewTypes { .. } => (CallKind::ShellNew, Wait::Work, String::new()),
        ShellReq::QuickAccessPins { .. } => (CallKind::Pins, Wait::Work, String::new()),
        ShellReq::CancelGen { .. } | ShellReq::Bye => (CallKind::Exec, Wait::Busy, String::new()),
    }
}

/// HostState in one u64: the tag in bits 32-39, the pid below.
fn encode_state(s: HostState) -> u64 {
    let (tag, pid) = match s {
        HostState::Down => (0u64, 0u32),
        HostState::Starting => (1, 0),
        HostState::Ready { pid } => (2, pid),
        HostState::Busy { pid } => (3, pid),
        HostState::MenuOff => (4, 0),
    };
    (tag << 32) | u64::from(pid)
}

fn decode_state(v: u64) -> HostState {
    let pid = v as u32;
    match v >> 32 {
        1 => HostState::Starting,
        2 => HostState::Ready { pid },
        3 => HostState::Busy { pid },
        4 => HostState::MenuOff,
        _ => HostState::Down,
    }
}

pub struct ShellClient {
    cmd: mpsc::Sender<Cmd>,
    /// None when no event could be created: then no supervisor runs and every request falls back as Unavailable.
    wake: Option<Arc<Wake>>,
    replies: Mutex<mpsc::Receiver<ShellReply>>,
    reply_tx: mpsc::Sender<ShellReply>,
    state: Arc<AtomicU64>,
    breakers: Arc<RwLock<Breakers>>,
    updated: Arc<AtomicBool>,
    next: AtomicU32,
    notify: Arc<dyn Fn() + Send + Sync>,
    /// The supervisor's wake-ups (read by supervisor_wakes in a debug build).
    #[cfg_attr(not(debug_assertions), allow(dead_code))]
    wakes: Arc<AtomicU64>,
    /// Contended acquisitions on this client's UI side.
    waits: AtomicU64,
    hub: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl ShellClient {
    pub fn new(cfg: ShellConfig, notify: std::sync::Arc<dyn Fn() + Send + Sync>) -> Self {
        let (cmd, rx) = mpsc::channel();
        let (reply_tx, replies) = mpsc::channel();
        // Without an event the supervisor cannot run: every request then falls back as Unavailable.
        let wake = Wake::new().ok();
        let env: Vec<(String, String)> = crate::host::effective(&cfg.test)
            .map(|h| h.env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())
            .unwrap_or_default();
        let state = Arc::new(AtomicU64::new(encode_state(HostState::Down)));
        let breakers = Arc::new(RwLock::new(Breakers::default()));
        let updated = Arc::new(AtomicBool::new(false));
        let wakes = Arc::new(AtomicU64::new(0));
        let hub = wake.as_ref().and_then(|w| {
            let hub = Hub {
                exe: cfg.exe,
                env,
                rx,
                tx: cmd.clone(),
                wake: w.clone(),
                replies: reply_tx.clone(),
                notify: notify.clone(),
                state: state.clone(),
                breakers: breakers.clone(),
                updated: updated.clone(),
                wakes: wakes.clone(),
                conn: None,
                gen: 0,
                out: Vec::new(),
                kill: None,
                late: Vec::new(),
                told: false,
            };
            std::thread::Builder::new()
                .name("shell-supervisor".into())
                .stack_size(256 * 1024)
                .spawn(move || hub.run())
                .ok()
        });
        ShellClient {
            cmd,
            wake,
            replies: Mutex::new(replies),
            reply_tx,
            state,
            breakers,
            updated,
            next: AtomicU32::new(1),
            notify,
            wakes,
            waits: AtomicU64::new(0),
            hub: Mutex::new(hub),
        }
    }

    /// Starts a host now, so the first menu does not wait for one.
    pub fn prespawn(&self) {
        self.send(Cmd::Prespawn);
    }

    pub fn next_req(&self) -> ReqId {
        ReqId(self.next.fetch_add(1, Ordering::Relaxed))
    }

    /// Returns at once; Fallback replies when it cannot be served in time.
    pub fn request(&self, r: ShellReq) {
        let Some(req) = shell_host::req_id(&r) else {
            let mut bytes = Vec::new();
            if !matches!(r, ShellReq::Bye) && frame::encode(&r, &mut bytes).is_ok() {
                self.send(Cmd::Raw(bytes));
            }
            return;
        };
        let (kind, wait, ext) = classify(&r);
        if self.wake.is_none() {
            return self.fallback(req, FallbackWhy::Unavailable);
        }
        if self.updated.load(Ordering::Acquire) {
            return self.fallback(req, FallbackWhy::Updated);
        }
        if read_ui(&self.breakers, &self.waits).disabled(kind, &ext) {
            return self.fallback(req, FallbackWhy::Disabled);
        }
        let mut bytes = Vec::new();
        let limit = (SHELL.max_payload - SPLIT_MARGIN) as usize + frame::HEADER_LEN;
        if too_large(&r) || frame::encode(&r, &mut bytes).is_err() || bytes.len() > limit {
            return self.fallback(req, FallbackWhy::TooLarge);
        }
        let foreground = matches!(r, ShellReq::Menu { .. } | ShellReq::Exec { .. } | ShellReq::RunTyped { .. });
        self.send(Cmd::Req { req, wait, kind, ext, bytes, foreground, since: now_ms() });
    }

    pub fn drain(&self) -> Vec<ShellReply> {
        lock_ui(&self.replies, &self.waits).try_iter().collect()
    }

    pub fn state(&self) -> HostState {
        decode_state(self.state.load(Ordering::Acquire))
    }

    pub fn disabled(&self, kind: CallKind, ext: &str) -> bool {
        read_ui(&self.breakers, &self.waits).disabled(kind, ext)
    }

    pub fn host_pid(&self) -> Option<u32> {
        match self.state() {
            HostState::Ready { pid } | HostState::Busy { pid } => Some(pid),
            _ => None,
        }
    }

    /// Ends a host that is not busy.
    pub fn shutdown(&self) {
        self.send(Cmd::Shutdown);
    }

    fn send(&self, c: Cmd) {
        if let Some(w) = &self.wake {
            if self.cmd.send(c).is_ok() {
                w.set();
            }
        }
    }

    fn fallback(&self, req: ReqId, why: FallbackWhy) {
        if self.reply_tx.send(ShellReply::Fallback { req, why }).is_ok() {
            (self.notify)();
        }
    }
}

/// The supervisor's wake-ups so far (debug builds: supervisor_waits_infinite_when_idle).
#[cfg(debug_assertions)]
pub fn supervisor_wakes(c: &ShellClient) -> u64 {
    c.wakes.load(Ordering::Acquire)
}

/// This client's contended acquisitions on the UI side so far (debug builds: request_and_drain_never_wait).
#[cfg(debug_assertions)]
pub fn ui_waits(c: &ShellClient) -> u64 {
    c.waits.load(Ordering::Acquire)
}

impl Drop for ShellClient {
    fn drop(&mut self) {
        self.send(Cmd::Quit);
        if let Some(h) = self.hub.lock().unwrap_or_else(PoisonError::into_inner).take() {
            let _ = h.join();
        }
    }
}

/// One running host as the supervisor holds it.
struct Conn {
    gen: u32,
    host: HostProcessParts,
    pid: u32,
    ready: bool,
    busy: bool,
    since: u64,
    /// Set when the supervisor ended it.
    killed: Option<Killed>,
    /// When its reader stopped (ms): End came, and no later reply of this host can be read.
    reader_gone: Option<u64>,
}

/// Why the supervisor ended a host.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Killed {
    /// A missed deadline: the call the kill is attributed to.
    For(CallKind, String),
    /// No Ready in time or a bad Hello: rule C covers these, and no pair is recorded (no extension ran).
    Unattributed,
}

/// What a host's end is recorded against in the breakers (None: nothing). A kill of the supervisor's carries its own
/// attribution; a host that ended itself (it set KILLING: the idle exit, Bye, the end of its input, its UI gone, the
/// 1 h cap) did not crash. Else the crash is blamed on a slot in a call of a kind with a deadline, then on a slot in a call of
/// InvokeCommand or Exec (no deadline: they may run for minutes beside the call that crashed), then on the oldest
/// outstanding request; an end with no slot in a call and nothing outstanding is no crash.
fn blame(
    killed: Option<&Killed>,
    self_ended: bool,
    slots: &[SlotView],
    oldest: Option<(CallKind, String)>,
) -> Option<(CallKind, String)> {
    match killed {
        Some(Killed::For(kind, ext)) => return Some((*kind, ext.clone())),
        Some(Killed::Unattributed) => return None,
        None if self_ended => return None,
        None => {}
    }
    let in_call = |deadline: bool| {
        slots.iter().filter(|v| v.in_call).find_map(|v| {
            v.kind.filter(|k| supervisor::crumb_limit(*k).is_some() == deadline).map(|k| (k, v.ext.clone()))
        })
    };
    in_call(true).or_else(|| in_call(false)).or(oldest)
}

/// The parts of a HostProcess the supervisor keeps (its pipes went to the writer and reader threads).
struct HostProcessParts {
    child: proc::Child,
    crumbs: crumbs::Mapping,
    to_host: mpsc::Sender<Vec<u8>>,
}

/// Why a kill is pending.
#[derive(Clone, Debug, PartialEq, Eq)]
enum KillWhy {
    /// A request missed its own deadline (answered at once; the kill is skipped if its reply comes in the meantime).
    Late(ReqId),
    /// A slot stayed in a call past its kind's deadline (skipped if that call ends in the meantime).
    Crumb { slot: u8, seq: u32 },
    /// The host never said Ready.
    Start,
}

/// A kill decided and not yet done: the host was busy.
struct Kill {
    why: KillWhy,
    /// What the death is attributed to.
    attrib: Killed,
}

/// How often a pending kill reads the busy word (only while it is pending).
const KILL_POLL_MS: u64 = 50;

/// When a host whose reader stopped at `t` is shut down if it has not exited by then.
fn reader_grace_end(t: u64) -> u64 {
    t.saturating_add(supervisor::CRUMB_POLL_MS)
}

/// The crumbs slots of the STAs a call kind runs on.
fn slots_of(kind: CallKind) -> &'static [u8] {
    match kind {
        CallKind::QueryContextMenu
        | CallKind::HandleMenuMsg
        | CallKind::InvokeCommand
        | CallKind::ModernItems
        | CallKind::VerbName => &[crumbs::SLOT_MENU],
        CallKind::Exec | CallKind::ShellNew | CallKind::Pins => &[crumbs::SLOT_EXEC],
        CallKind::Icon | CallKind::Thumb | CallKind::ClassIcon => &crumbs::SLOT_IMAGE,
    }
}

/// The supervisor thread's state.
struct Hub {
    exe: PathBuf,
    env: Vec<(String, String)>,
    rx: mpsc::Receiver<Cmd>,
    tx: mpsc::Sender<Cmd>,
    wake: Arc<Wake>,
    replies: mpsc::Sender<ShellReply>,
    notify: Arc<dyn Fn() + Send + Sync>,
    state: Arc<AtomicU64>,
    breakers: Arc<RwLock<Breakers>>,
    updated: Arc<AtomicBool>,
    wakes: Arc<AtomicU64>,
    conn: Option<Conn>,
    gen: u32,
    out: Vec<Outstanding>,
    kill: Option<Kill>,
    /// Requests already answered by a deadline whose own reply may still come (dropped when it does).
    late: Vec<ReqId>,
    /// Replies were queued since the last notify.
    told: bool,
}

impl Hub {
    fn run(mut self) {
        loop {
            let now = now_ms();
            let timeout = self.next_wake(now).map_or(INFINITE, |ms| ms.min(u64::from(INFINITE - 1)) as u32);
            let mut handles = vec![self.wake.handle()];
            handles.extend(self.conn.as_ref().map(|c| c.host.child.handle()));
            // SAFETY: handles owned by this thread's state for the whole wait.
            let _ = unsafe { WaitForMultipleObjects(&handles, false, timeout) };
            self.wakes.fetch_add(1, Ordering::Relaxed);
            if self.conn.as_ref().is_some_and(|c| !c.host.child.running()) {
                self.host_exited();
            }
            loop {
                match self.rx.try_recv() {
                    Ok(Cmd::Quit) | Err(mpsc::TryRecvError::Disconnected) => {
                        self.shutdown();
                        self.tell();
                        return;
                    }
                    Ok(c) => self.on_cmd(c),
                    Err(mpsc::TryRecvError::Empty) => break,
                }
            }
            if self.conn.as_ref().is_some_and(|c| !c.host.child.running()) {
                self.host_exited();
            }
            self.supervise(now_ms());
            self.publish();
            self.tell();
        }
    }

    fn reply(&mut self, r: ShellReply) {
        if self.replies.send(r).is_ok() {
            self.told = true;
        }
    }

    fn fallback(&mut self, req: ReqId, why: FallbackWhy) {
        self.reply(ShellReply::Fallback { req, why });
    }

    fn tell(&mut self) {
        if std::mem::take(&mut self.told) {
            (self.notify)();
        }
    }

    fn publish(&self) {
        let s = match &self.conn {
            None if self.breakers.read().unwrap_or_else(PoisonError::into_inner).menu_off() => HostState::MenuOff,
            None => HostState::Down,
            Some(c) if !c.ready => HostState::Starting,
            Some(c) if c.busy => HostState::Busy { pid: c.pid },
            Some(c) => HostState::Ready { pid: c.pid },
        };
        self.state.store(encode_state(s), Ordering::Release);
    }

    fn on_cmd(&mut self, c: Cmd) {
        match c {
            Cmd::Req { req, wait, kind, ext, bytes, foreground, since } => {
                if self.updated.load(Ordering::Acquire) {
                    return self.fallback(req, FallbackWhy::Updated);
                }
                let now = now_ms();
                if !self.ensure_host(now) {
                    return self.fallback(req, FallbackWhy::Unavailable);
                }
                if let Some(c) = &self.conn {
                    if foreground {
                        // SAFETY: no pointers; lets the host take the foreground for its menu or verb.
                        let _ = unsafe { AllowSetForegroundWindow(c.pid) };
                    }
                    let _ = c.host.to_host.send(bytes);
                }
                // Deadlines count from the UI's request, not from this thread's wake.
                self.out.push(Outstanding { req, wait, since, kind, ext });
            }
            Cmd::Raw(bytes) => {
                if let Some(c) = &self.conn {
                    let _ = c.host.to_host.send(bytes);
                }
            }
            Cmd::Prespawn => {
                let _ = self.ensure_host(now_ms());
            }
            Cmd::Shutdown => self.shutdown(),
            Cmd::Quit => {}
            Cmd::Host { gen, got } => {
                if self.conn.as_ref().is_some_and(|c| c.gen == gen) {
                    self.on_host(got);
                }
            }
        }
    }

    /// A host runs (or was just started); false when none may or can be started.
    fn ensure_host(&mut self, now: u64) -> bool {
        if self.conn.is_some() {
            return true;
        }
        if self.updated.load(Ordering::Acquire) || !write_lock(&self.breakers).spawn(now) {
            return false;
        }
        let env: Vec<(&str, &str)> = self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let hp = match spawn_host(&self.exe, &env) {
            Ok(hp) => hp,
            Err(e) => {
                oled_sys::log!("shell: cannot start the shell host ({e})");
                return false;
            }
        };
        self.gen = self.gen.wrapping_add(1);
        let gen = self.gen;
        let HostProcess { child, to_host, from_host, crumbs } = hp;
        let (wtx, wrx) = mpsc::channel::<Vec<u8>>();
        let writer =
            std::thread::Builder::new().name("shell-client-writer".into()).stack_size(64 * 1024).spawn(move || {
                let mut to_host = to_host;
                while let Ok(b) = wrx.recv() {
                    if to_host.write_all(&b).is_err() {
                        break;
                    }
                }
            });
        let (tx, wake) = (self.tx.clone(), self.wake.clone());
        let reader = std::thread::Builder::new()
            .name("shell-client-reader".into())
            .stack_size(64 * 1024)
            .spawn(move || read_host(from_host, gen, &tx, &wake));
        if writer.is_err() || reader.is_err() {
            oled_sys::log!("shell: no threads for the shell host");
        }
        let pid = child.pid;
        self.conn = Some(Conn {
            gen,
            host: HostProcessParts { child, crumbs, to_host: wtx },
            pid,
            ready: false,
            busy: false,
            since: now,
            killed: None,
            reader_gone: None,
        });
        true
    }

    fn on_host(&mut self, got: FromHost) {
        match got {
            FromHost::Hello(Ok(())) => {}
            FromHost::End => {
                if let Some(c) = &mut self.conn {
                    c.reader_gone.get_or_insert(now_ms());
                }
            }
            FromHost::Hello(Err(FrameError::BadProto { got, want })) => {
                oled_sys::log!("shell: the shell host speaks protocol {got}, this client {want}: files was updated");
                self.updated.store(true, Ordering::Release);
                self.end_host(Killed::Unattributed);
            }
            FromHost::Hello(Err(e)) => {
                oled_sys::log!("shell: bad Hello from the shell host ({e:?})");
                self.end_host(Killed::Unattributed);
            }
            FromHost::Evt(e) => self.on_evt(e),
        }
    }

    /// Ends the host through the kill protocol (when it is not busy); the exit is handled by host_exited.
    fn end_host(&mut self, why: Killed) {
        if let Some(c) = &mut self.conn {
            if c.host.crumbs.section().try_kill() {
                // SAFETY: the host's process handle, open while the Conn lives; try_kill won just above.
                let _ = unsafe { TerminateProcess(c.host.child.handle(), 1) };
                c.killed = Some(why);
            }
        }
    }

    fn position(&self, req: ReqId) -> Option<usize> {
        self.out.iter().position(|o| o.req == req)
    }

    fn on_evt(&mut self, e: ShellEvt) {
        match &e {
            ShellEvt::Ready { pid } => {
                if let Some(c) = &mut self.conn {
                    c.ready = true;
                    oled_sys::log!("shell: host {pid} ready after {} ms", now_ms().saturating_sub(c.since));
                }
            }
            ShellEvt::Busy { on } => {
                if let Some(c) = &mut self.conn {
                    c.busy = *on;
                }
                for o in self.out.iter_mut().filter(|o| o.kind == CallKind::QueryContextMenu) {
                    match (o.wait, *on) {
                        (Wait::MenuOpen, true) => o.wait = Wait::Busy,
                        (Wait::Busy, false) => o.wait = Wait::MenuOpen,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        let Some(req) = writer::req_of(&e) else {
            return self.reply(ShellReply::Evt(e));
        };
        let Some(i) = self.position(req) else {
            // The request was answered by a deadline: its late reply ends the pending kill's reason.
            self.late.retain(|&r| r != req);
            return;
        };
        let last = match &e {
            ShellEvt::MenuShown { .. } => {
                if let Some(o) = self.out.get_mut(i) {
                    o.wait = Wait::MenuOpen;
                }
                false
            }
            ShellEvt::Modern { done, .. } => *done,
            ShellEvt::MenuDone { .. }
            | ShellEvt::ExecDone { .. }
            | ShellEvt::Failed { .. }
            | ShellEvt::ReqDone { .. } => true,
            _ => false,
        };
        if last {
            self.out.remove(i);
        }
        self.reply(ShellReply::Evt(e));
    }

    /// The host process ended: attribution, breakers, a Fallback for every outstanding request.
    fn host_exited(&mut self) {
        let Some(c) = self.conn.take() else { return };
        let now = now_ms();
        let s = c.host.crumbs.section();
        let slots: Vec<SlotView> = (0..crumbs::SLOTS as u8).map(|i| s.read(i, false)).collect();
        let oldest = self.out.first().map(|o| (o.kind, o.ext.clone()));
        // KILLING that this supervisor did not set: the host decided to end (idle exit, Bye, end of input).
        let self_ended = c.killed.is_none() && s.busy() & crumbs::KILLING != 0;
        let attrib = blame(c.killed.as_ref(), self_ended, &slots, oldest);
        if let Some((kind, ext)) = &attrib {
            write_lock(&self.breakers).record(*kind, ext, now);
        }
        let why = if self.updated.load(Ordering::Acquire) {
            FallbackWhy::Updated
        } else if c.killed.is_some() {
            FallbackWhy::Deadline
        } else {
            FallbackWhy::HostDied
        };
        if attrib.is_some() || c.killed.is_some() {
            oled_sys::log!("shell: host {} ended ({why:?}), attributed to {attrib:?}", c.pid);
        } else if self_ended && !self.out.is_empty() {
            oled_sys::log!("shell: host {} ended itself with {} request(s) outstanding", c.pid, self.out.len());
        }
        for o in std::mem::take(&mut self.out) {
            self.fallback(o.req, why);
        }
        self.kill = None;
        self.late.clear();
    }

    fn shutdown(&mut self) {
        if let Some(c) = &self.conn {
            let mut bye = Vec::new();
            if frame::encode(&ShellReq::Bye, &mut bye).is_ok() {
                let _ = c.host.to_host.send(bye);
            }
            if c.host.crumbs.section().try_kill() {
                // SAFETY: the host's process handle, open while the Conn lives; try_kill won just above.
                let _ = unsafe { TerminateProcess(c.host.child.handle(), 0) };
            }
        }
        // A busy host keeps running: its pipes close here and it ends itself when its call ends.
        self.conn = None;
        for o in std::mem::take(&mut self.out) {
            self.fallback(o.req, FallbackWhy::HostDied);
        }
        self.kill = None;
        self.late.clear();
    }

    /// The supervisor's next wake (ms): None (no timeout) while nothing is outstanding, no menu is displayed, no kill
    /// is pending and the host is not starting.
    fn next_wake(&self, now: u64) -> Option<u64> {
        let start =
            self.conn.as_ref().filter(|c| !c.ready).map(|c| (c.since + supervisor::START_MS).saturating_sub(now));
        let poll = self.kill.as_ref().map(|_| KILL_POLL_MS);
        let unread = self.conn.as_ref().and_then(|c| c.reader_gone).map(|t| reader_grace_end(t).saturating_sub(now));
        [supervisor::next_wake(&self.out, now), start, poll, unread].into_iter().flatten().min()
    }

    /// Answers a request with Fallback Deadline at once; its own reply, if it comes, is dropped.
    fn answer_late(&mut self, req: ReqId) -> Option<Outstanding> {
        let o = self.position(req).map(|i| self.out.remove(i))?;
        self.fallback(req, FallbackWhy::Deadline);
        self.late.push(req);
        Some(o)
    }

    /// The first reason of a kill wins; the others wait for the host's end.
    fn decide(&mut self, why: KillWhy, attrib: Killed) {
        if self.kill.is_none() {
            self.kill = Some(Kill { why, attrib });
        }
    }

    /// Deadlines, crumb checks and the pending kill. A missed deadline is answered at once; the kill then goes through
    /// try_kill only, and is skipped when the late call ends before the host stops being busy.
    fn supervise(&mut self, now: u64) {
        // The reader stopped while the host runs on (an event the codec refused, a broken pipe): no reply of this host
        // can be read any more. One poll later (a host that exits is seen as such first), it is shut down: ended
        // through the kill protocol, or, when busy, left to finish its call with its pipes closed. Every outstanding
        // request gets HostDied, and the next request starts a new host.
        if self.conn.as_ref().and_then(|c| c.reader_gone).is_some_and(|t| now >= reader_grace_end(t)) {
            oled_sys::log!("shell: the shell host's events can no longer be read; it is shut down");
            return self.shutdown();
        }
        let Some(c) = &self.conn else { return };
        let (section, ready, since) = (c.host.crumbs.section(), c.ready, c.since);
        if !ready && now.saturating_sub(since) >= supervisor::START_MS {
            self.decide(KillWhy::Start, Killed::Unattributed);
        }
        for req in supervisor::overdue(&self.out, now) {
            if let Some(o) = self.answer_late(req) {
                self.decide(KillWhy::Late(req), Killed::For(o.kind, o.ext));
            }
        }
        for slot in 0..crumbs::SLOTS as u8 {
            let v = section.read(slot, true);
            if !supervisor::crumb_overdue(&v, now) {
                continue;
            }
            let on_slot: Vec<ReqId> =
                self.out.iter().filter(|o| slots_of(o.kind).contains(&slot)).map(|o| o.req).collect();
            for req in on_slot {
                self.answer_late(req);
            }
            if let Some(kind) = v.kind {
                self.decide(KillWhy::Crumb { slot, seq: v.seq }, Killed::For(kind, v.ext.clone()));
            }
        }
        let Some(k) = &self.kill else { return };
        let resolved = match &k.why {
            KillWhy::Late(r) => !self.late.contains(r),
            KillWhy::Crumb { slot, seq } => section.read(*slot, true).seq != *seq,
            KillWhy::Start => ready,
        };
        if resolved {
            self.kill = None;
        } else if section.try_kill() {
            let attrib = k.attrib.clone();
            if let Some(c) = &mut self.conn {
                // SAFETY: the host's process handle, open while the Conn lives; try_kill won just above.
                let _ = unsafe { TerminateProcess(c.host.child.handle(), 1) };
                c.killed = Some(attrib);
            }
            self.kill = None;
        }
    }
}

/// A reader thread: the host's Hello, then its events, then End; each one wakes the supervisor.
fn read_host(mut r: std::io::PipeReader, gen: u32, tx: &mpsc::Sender<Cmd>, wake: &Wake) {
    let mut asm = Assembler::new(SHELL.max_payload);
    let mut buf = vec![0u8; 64 * 1024];
    let mut hello = false;
    loop {
        loop {
            let got = match asm.next_frame() {
                Ok(Some(f)) if !hello => {
                    hello = true;
                    FromHost::Hello(frame::decode_hello(&SHELL, &f).map(|_| ()))
                }
                Ok(Some(f)) => match frame::decode::<ShellEvt>(&f) {
                    Ok((e, _)) => FromHost::Evt(e),
                    Err(e) => {
                        oled_sys::log!("shell: an event that is not ours ({e:?})");
                        FromHost::End
                    }
                },
                Ok(None) => break,
                Err(_) => FromHost::End,
            };
            let end = matches!(got, FromHost::End);
            if tx.send(Cmd::Host { gen, got }).is_err() {
                return;
            }
            wake.set();
            if end {
                return;
            }
        }
        match r.read(&mut buf) {
            Ok(0) | Err(_) => {
                let _ = tx.send(Cmd::Host { gen, got: FromHost::End });
                wake.set();
                return;
            }
            Ok(n) => asm.push(buf.get(..n).unwrap_or_default()),
        }
    }
}

/// One started shell host: its process, the two pipe ends the client keeps and the crumbs section.
pub struct HostProcess {
    pub child: proc::Child,
    /// Requests to the host.
    pub to_host: std::io::PipeWriter,
    /// Events from the host.
    pub from_host: std::io::PipeReader,
    pub crumbs: crumbs::Mapping,
}

/// Starts files.exe --role=shell with its two pipes, its crumbs section and a SYNCHRONIZE handle of this process as
/// the only handles it inherits, `env` set on the child only; then sends the client's Hello. The host's Hello and
/// Ready arrive on from_host.
pub fn spawn_host(exe: &Path, env: &[(&str, &str)]) -> std::io::Result<HostProcess> {
    let mut hello = Vec::new();
    frame::encode_hello(&SHELL, crate::BUILD_ID, &mut hello);
    spawn_host_with_hello(exe, env, &hello)
}

/// spawn_host with the bytes of the client's Hello given (a test forges one of another protocol version).
pub fn spawn_host_with_hello(exe: &Path, env: &[(&str, &str)], hello: &[u8]) -> std::io::Result<HostProcess> {
    let (host_in, to_host) = anon::pipe_pair()?;
    let (from_host, host_out) = anon::pipe_pair()?;
    let crumbs = crumbs::Mapping::create()?;
    let parent = anon::this_process_for_watch()?;
    let handles = [
        anon::inheritable(&host_in)?,
        anon::inheritable(&host_out)?,
        anon::inheritable(crumbs.handle())?,
        anon::inheritable(&parent)?,
    ];
    let [input, output, section, watch] = handles.map(|h| h.0 as usize);
    let args = shell_host::args_of(&shell_host::HostArgs { input, output, crumbs: section, parent: watch });
    let mut line: Vec<&str> = vec!["--role=shell"];
    line.extend(args.iter().map(String::as_str));
    let child = proc::spawn_inheriting(exe, &line, env, &handles);
    // The client keeps the section; the host's ends and the parent handle are closed here (the host has its copies).
    let _ = anon::not_inheritable(crumbs.handle());
    drop((host_in, host_out, parent));
    let child = child?;
    let mut hp = HostProcess { child, to_host, from_host, crumbs };
    hp.to_host.write_all(hello)?;
    Ok(hp)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(kind: CallKind, ext: &str, in_call: bool) -> SlotView {
        SlotView {
            seq: u32::from(in_call),
            in_call,
            kind: Some(kind),
            flags: 0,
            started_ms: 0,
            ext: ext.into(),
            calls: 0,
        }
    }

    fn pair(kind: CallKind, ext: &str) -> Option<(CallKind, String)> {
        Some((kind, ext.into()))
    }

    #[test]
    fn type_names_are_read_only_calls_like_class_icons() {
        let r = ShellReq::TypeNames { req: ReqId(1), exts: vec!["pdf".into(), "txt".into()] };
        assert_eq!(classify(&r), (CallKind::ClassIcon, Wait::Work, "pdf".to_string()));
        assert_eq!(supervisor::crumb_limit(CallKind::ClassIcon), Some(2_000), "the read-only 2,000 ms deadline");
        assert_eq!(slots_of(CallKind::ClassIcon), &crumbs::SLOT_IMAGE, "the image STAs");
        assert!(!too_large(&r));
    }

    #[test]
    fn blame_table() {
        use CallKind::*;
        let idle = slot(Icon, "png", false);
        // The crumb and the oldest request disagree: the crumb is blamed.
        let crumb = [slot(QueryContextMenu, "txt", true), idle.clone()];
        assert_eq!(blame(None, false, &crumb, pair(Thumb, "jpg")), pair(QueryContextMenu, "txt"));
        // A long invoke and a verb beside a call that has a deadline: the call with the deadline.
        let beside = [slot(InvokeCommand, "txt", true), slot(Exec, "", true), slot(Icon, "png", true), idle.clone()];
        assert_eq!(blame(None, false, &beside, pair(Exec, "")), pair(Icon, "png"));
        // Only an invoke in a call: the invoke.
        let invoke = [slot(InvokeCommand, "txt", true)];
        assert_eq!(blame(None, false, &invoke, pair(Thumb, "jpg")), pair(InvokeCommand, "txt"));
        // No slot in a call: the oldest request; nothing outstanding either: no crash.
        let none = std::slice::from_ref(&idle);
        assert_eq!(blame(None, false, none, pair(QueryContextMenu, "txt")), pair(QueryContextMenu, "txt"));
        assert_eq!(blame(None, false, none, None), None);
        // A deadline kill carries its own attribution; a start or Hello failure records nothing.
        let busy = [slot(Icon, "png", true)];
        assert_eq!(blame(Some(&Killed::For(Thumb, "jpg".into())), false, &busy, None), pair(Thumb, "jpg"));
        assert_eq!(blame(Some(&Killed::Unattributed), false, &busy, pair(QueryContextMenu, "txt")), None);
        // A host that ended itself (the idle exit under a menu still on screen, Bye, its UI gone) did not crash.
        assert_eq!(blame(None, true, &crumb, pair(QueryContextMenu, "txt")), None);
        assert_eq!(blame(None, true, none, pair(QueryContextMenu, "txt")), None);
    }
}
