//! The executor (T3.7): Close, End, End now, End process tree and Reopen run on an act thread, every key re-checked
//! immediately before it is touched. The types, the Executor trait and the RecordingExecutor are TF0's; the order of
//! a kill list and its checks run over the Os trait, so tests drive them with a fake.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use oled_sys::admin::policy::{descendants, HelperFacts, ProcEdge, TargetFacts};
use oled_sysmon::ProcKey;
use oled_ui::host::Proxy;
use oled_ui::model::RowKey;
use windows::core::{w, BOOL, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::SystemInformation::GetSystemWindowsDirectoryW;
use windows::Win32::System::Threading::{
    IsProcessCritical, OpenProcess, QueryFullProcessImageNameW, TerminateProcess, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, GetClassNameW, GetWindowThreadProcessId, PostMessageW, WM_CLOSE,
};

use super::access::{self, Refusal, ACCESS_DENIED};
use super::launch::Launcher;
use crate::model::classify::{path_class, PathClass};

/// What a batch does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActVerb {
    Close,
    End,
    EndNow,
    EndTree,
    Reopen,
}

/// One row's part of a batch: its processes, its windows (Close) and its launch string (Reopen).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub row: RowKey,
    pub members: Vec<ProcKey>,
    pub windows: Vec<isize>,
    pub launch: Option<String>,
}

/// The keys no batch may terminate, built on the UI thread from the latest frame when the batch is made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KillGuard {
    pub helper: HelperFacts,
    /// Our parent, its parent and so on, at most 64.
    pub own_chain: Vec<ProcKey>,
    pub shell: Option<ProcKey>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Batch {
    pub verb: ActVerb,
    pub targets: Vec<Target>,
    pub guard: KillGuard,
    /// The process edges of the latest frame when the batch is made.
    pub edges: Vec<ProcEdge>,
}

/// A target's result: the worst of its members' (Failed > NeedsAdmin > Done > Gone); Refused when every key was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Posted,
    Done,
    Gone,
    NeedsAdmin,
    Refused(Refusal),
    Failed(u32),
}

/// AppMsg: one per target; refused counts the keys skipped by the guard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionDone {
    pub row: RowKey,
    pub outcome: Outcome,
    pub refused: u16,
}

/// The UI's one way to act on processes.
pub trait Executor: Send + Sync {
    fn submit(&self, b: Batch);
}

/// Keeps every batch and runs nothing; every Harness test and scene.
#[derive(Default)]
pub struct RecordingExecutor {
    pub batches: Mutex<Vec<Batch>>,
}

impl Executor for RecordingExecutor {
    fn submit(&self, b: Batch) {
        self.batches.lock().unwrap_or_else(|p| p.into_inner()).push(b);
    }
}

/// ERROR_INVALID_PARAMETER: OpenProcess of a pid that is gone.
pub const INVALID_PARAMETER: u32 = 87;
/// ERROR_NOT_ENOUGH_MEMORY: the act thread could not start.
pub const NOT_ENOUGH_MEMORY: u32 = 8;
/// ERROR_NOT_FOUND: a Reopen without a launch string.
pub const NOT_FOUND: u32 = 1168;
/// ERROR_INVALID_WINDOW_HANDLE: a window that is gone.
pub const INVALID_WINDOW: u32 = 1400;

/// The act thread's stack: a kill list needs little (its buffers are on the heap).
const ACT_STACK: usize = 64 * 1024;
/// Reopen calls ShellExecuteExW, which loads shell extensions on the calling thread: the stack they expect.
const REOPEN_STACK: usize = 1024 * 1024;

/// An open process handle of the Os seam.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OsHandle(pub isize);

/// The system calls of an act thread, behind a trait so the order of a kill list and its checks run over a fake in
/// tests (tests/exec_t3.rs); only the act thread uses the Win32 one.
pub trait Os {
    /// OpenProcess(PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION); Err is the Win32 code.
    fn open(&mut self, pid: u32) -> Result<OsHandle, u32>;
    /// GetProcessTimes: the create time.
    fn created(&mut self, h: OsHandle) -> Option<u64>;
    /// ProcessIdToSessionId (the pid cannot be reused while the handle is open).
    fn session(&mut self, pid: u32) -> Option<u32>;
    /// QueryFullProcessImageNameW.
    fn image_path(&mut self, h: OsHandle) -> Result<String, u32>;
    /// IsProcessCritical.
    fn critical(&mut self, h: OsHandle) -> Option<bool>;
    /// TerminateProcess(h, 1).
    fn terminate(&mut self, h: OsHandle) -> Result<(), u32>;
    /// CloseHandle.
    fn close(&mut self, h: OsHandle);
    /// The pid that owns the window now, by the frame's owner rule (a packaged app's frame: its CoreWindow child's
    /// process); None when it is gone.
    fn window_pid(&mut self, hwnd: isize) -> Option<u32>;
    /// PostMessageW(hwnd, WM_CLOSE, 0, 0).
    fn post_close(&mut self, hwnd: isize) -> Result<(), u32>;
}

/// The rank of an outcome in a merge: Failed > NeedsAdmin > Done > Gone.
fn rank(o: Outcome) -> u8 {
    match o {
        Outcome::Gone => 0,
        Outcome::Posted | Outcome::Refused(_) => 1,
        Outcome::Done => 2,
        Outcome::NeedsAdmin => 3,
        Outcome::Failed(_) => 4,
    }
}

/// The worse of two members' outcomes (Failed > NeedsAdmin > Done > Gone).
pub fn worst(a: Outcome, b: Outcome) -> Outcome {
    if rank(b) > rank(a) {
        b
    } else {
        a
    }
}

/// The keys an End of `members` may reach: each member, root first, then its descendants by the create-time rule
/// (policy::descendants), each key once, in that order (parents before their children; end_target keeps every key
/// below a refused one).
pub fn kill_list(members: &[ProcKey], edges: &[ProcEdge]) -> Vec<ProcKey> {
    let mut out = Vec::with_capacity(members.len());
    let mut seen: HashSet<ProcKey> = HashSet::with_capacity(members.len());
    let mut tree = Vec::new();
    for m in members {
        descendants(*m, edges, &mut tree);
        for k in &tree {
            if seen.insert(*k) {
                out.push(*k);
            }
        }
    }
    out
}

/// What became of one key of a kill list.
enum KeyEnd {
    Refused(Refusal),
    Outcome(Outcome),
}

/// One key, checked again immediately before it is touched: (1) the key alone, before anything is opened; (2) the
/// opened process: its create time (a mismatch is Gone and nothing is killed), session, image and critical flag
/// through access::refusal; (3) only then TerminateProcess. The handle is closed on every path.
fn end_key(os: &mut dyn Os, key: ProcKey, g: &KillGuard, system_root: &str) -> KeyEnd {
    if let Some(r) = access::key_refusal(key, g) {
        return KeyEnd::Refused(r);
    }
    let h = match os.open(key.pid) {
        Ok(h) => h,
        Err(ACCESS_DENIED) => return KeyEnd::Outcome(Outcome::NeedsAdmin),
        Err(INVALID_PARAMETER) => return KeyEnd::Outcome(Outcome::Gone),
        Err(code) => return KeyEnd::Outcome(Outcome::Failed(code)),
    };
    let end = judge_and_end(os, h, key, g, system_root);
    os.close(h);
    end
}

fn judge_and_end(os: &mut dyn Os, h: OsHandle, key: ProcKey, g: &KillGuard, system_root: &str) -> KeyEnd {
    let created = os.created(h);
    if created != Some(key.created) {
        return KeyEnd::Outcome(Outcome::Gone);
    }
    let path = match os.image_path(h) {
        Ok(p) => p,
        Err(code) => return KeyEnd::Outcome(Outcome::Failed(code)),
    };
    let image_name = path.rsplit(['\\', '/']).next().unwrap_or_default().to_string();
    let facts = TargetFacts {
        key,
        live_created: created,
        // Unknown counts as session 0: refused.
        session: os.session(key.pid).unwrap_or(0),
        image_name,
        has_image_path: !path.is_empty(),
        under_system_root: path_class(Some(&path), system_root) == PathClass::SystemRoot,
        critical_flag: os.critical(h),
        protected: false,
        hosts_reboot_service: false,
    };
    if let Some(r) = access::refusal(&facts, g) {
        return KeyEnd::Refused(r);
    }
    match os.terminate(h) {
        Ok(()) => KeyEnd::Outcome(Outcome::Done),
        Err(ACCESS_DENIED) => KeyEnd::Outcome(Outcome::NeedsAdmin),
        Err(code) => KeyEnd::Outcome(Outcome::Failed(code)),
    }
}

/// End, End now, End process tree: the kill list of the target's members, every key checked; the worst outcome of
/// the keys that were not refused, Refused when every key was. A refused key keeps its whole tree: a descendant whose
/// parent (by the create-time rule) was refused or kept is kept too, never opened, and counted as refused, so ending
/// a group that holds the shell, Tasks, its ancestors or the dock never reaches what they started. A member is judged
/// on its own keys even when it is below another member.
fn end_target(os: &mut dyn Os, t: &Target, b: &Batch, system_root: &str) -> ActionDone {
    let mut out: Option<Outcome> = None;
    let mut first_refusal = None;
    let mut refused: u16 = 0;
    let parent_pid: HashMap<ProcKey, u32> = b.edges.iter().map(|e| (e.key, e.parent_pid)).collect();
    // The kept keys by pid, with the oldest create time of each pid: a key is below one when its parent pid is that
    // pid and it was created later (the kill list is parents first, so a parent is judged before its children).
    let mut kept: HashMap<u32, u64> = HashMap::new();
    for key in kill_list(&t.members, &b.edges) {
        let below_kept = !t.members.contains(&key)
            && parent_pid.get(&key).and_then(|p| kept.get(p)).is_some_and(|&created| key.created > created);
        let end = if below_kept { None } else { Some(end_key(os, key, &b.guard, system_root)) };
        match end {
            Some(KeyEnd::Outcome(o)) => out = Some(out.map_or(o, |w| worst(w, o))),
            refusal => {
                if let Some(KeyEnd::Refused(r)) = refusal {
                    first_refusal.get_or_insert(r);
                }
                refused = refused.saturating_add(1);
                kept.entry(key.pid).and_modify(|c| *c = (*c).min(key.created)).or_insert(key.created);
            }
        }
    }
    let outcome = match (out, first_refusal) {
        (Some(o), _) => o,
        (None, Some(r)) => Outcome::Refused(r),
        (None, None) => Outcome::Gone,
    };
    ActionDone { row: t.row, outcome, refused }
}

/// Close: WM_CLOSE to each window of the target that a member still owns and the guard allows (never to the shell,
/// Tasks, its ancestors or the dock). Posted when one was posted; Tasks never ends anything by itself.
fn close_target(os: &mut dyn Os, t: &Target, g: &KillGuard) -> ActionDone {
    let (mut posted, mut failed, mut first_refusal) = (false, None, None);
    let mut refused: u16 = 0;
    for &hwnd in &t.windows {
        let Some(pid) = os.window_pid(hwnd) else { continue };
        // A window whose owner is no member any more (it closed and the handle was reused) is not this app's.
        let Some(&owner) = t.members.iter().find(|k| k.pid == pid) else { continue };
        if let Some(r) = access::key_refusal(owner, g) {
            refused = refused.saturating_add(1);
            first_refusal.get_or_insert(r);
            continue;
        }
        match os.post_close(hwnd) {
            Ok(()) => posted = true,
            Err(INVALID_WINDOW) => {}
            Err(code) => failed = Some(code),
        }
    }
    let outcome = match (posted, failed, first_refusal) {
        (true, _, _) => Outcome::Posted,
        (false, Some(code), _) => Outcome::Failed(code),
        (false, None, Some(r)) => Outcome::Refused(r),
        (false, None, None) => Outcome::Gone,
    };
    ActionDone { row: t.row, outcome, refused }
}

/// Reopen: the launch string through the launcher (ShellExecuteExW at medium).
fn reopen_target(launcher: &dyn Launcher, t: &Target) -> ActionDone {
    let outcome = match t.launch.as_deref() {
        Some(l) => match launcher.open_launch(l) {
            Ok(()) => Outcome::Done,
            Err(code) => Outcome::Failed(code),
        },
        None => Outcome::Failed(NOT_FOUND),
    };
    ActionDone { row: t.row, outcome, refused: 0 }
}

/// One batch, in target order: one ActionDone per target. `system_root` is the Windows folder (the critical names
/// count only under it).
pub fn run(os: &mut dyn Os, b: &Batch, system_root: &str, launcher: &dyn Launcher) -> Vec<ActionDone> {
    b.targets
        .iter()
        .map(|t| match b.verb {
            ActVerb::Close => close_target(os, t, &b.guard),
            ActVerb::End | ActVerb::EndNow | ActVerb::EndTree => end_target(os, t, b, system_root),
            ActVerb::Reopen => reopen_target(launcher, t),
        })
        .collect()
}

/// The real executor: each batch on a new "tasks-act" thread, its results through the attached proxy. T3.7.
pub struct Win32Executor {
    launcher: Arc<dyn Launcher>,
    proxy: Mutex<Option<Proxy>>,
    system_root: String,
}

impl Win32Executor {
    pub fn new(l: Arc<dyn Launcher>) -> Arc<Self> {
        Arc::new(Win32Executor { launcher: l, proxy: Mutex::new(None), system_root: windows_dir() })
    }

    pub fn attach(&self, p: Proxy) {
        *self.proxy.lock().unwrap_or_else(|e| e.into_inner()) = Some(p);
    }
}

impl Executor for Win32Executor {
    /// The batch on its own thread; the UI thread never waits for a process. Each result comes back as an ActionDone;
    /// when the thread cannot start, every target comes back as Failed.
    fn submit(&self, b: Batch) {
        let proxy = self.proxy.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let rows: Vec<RowKey> = b.targets.iter().map(|t| t.row).collect();
        let (launcher, root, to_ui) = (Arc::clone(&self.launcher), self.system_root.clone(), proxy.clone());
        let stack = if b.verb == ActVerb::Reopen { REOPEN_STACK } else { ACT_STACK };
        let spawned = std::thread::Builder::new().name("tasks-act".into()).stack_size(stack).spawn(move || {
            // Reopen runs the shell on this thread: COM single-threaded, as the launcher's contract asks.
            // SAFETY: no pointers; balanced by CoUninitialize below when it succeeded.
            let com = b.verb == ActVerb::Reopen && unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
            let done = run(&mut Win32Os, &b, &root, &*launcher);
            if com {
                // SAFETY: this thread's CoInitializeEx succeeded above.
                unsafe { CoUninitialize() };
            }
            let ended = done.iter().filter(|d| d.outcome == Outcome::Done).count();
            let refused: u32 = done.iter().map(|d| u32::from(d.refused)).sum();
            oled_sys::log!("tasks: act {:?}: {} targets, {ended} done, {refused} refused", b.verb, done.len());
            if let Some(p) = to_ui {
                for d in done {
                    let _ = p.send(Box::new(d));
                }
            }
        });
        if let Err(e) = spawned {
            oled_sys::log!("tasks: no act thread ({e})");
            if let Some(p) = proxy {
                for row in rows {
                    let _ =
                        p.send(Box::new(ActionDone { row, outcome: Outcome::Failed(NOT_ENOUGH_MEMORY), refused: 0 }));
                }
            }
        }
    }
}

/// The Windows folder (GetSystemWindowsDirectoryW); empty when it cannot be read (then no path counts as under it,
/// and the critical names count only for processes without a path).
fn windows_dir() -> String {
    let mut buf = [0u16; 260];
    // SAFETY: the buffer is writable for its whole length, which the slice carries.
    let n = unsafe { GetSystemWindowsDirectoryW(Some(&mut buf)) } as usize;
    buf.get(..n).map(String::from_utf16_lossy).unwrap_or_default()
}

/// The act thread's system calls.
struct Win32Os;

fn handle(h: OsHandle) -> HANDLE {
    HANDLE(h.0 as *mut core::ffi::c_void)
}

/// The frame window of a packaged app, which its host process (ApplicationFrameHost) owns.
const FRAME_CLASS: &str = "ApplicationFrameWindow";

/// The pid that owns a window now; None when it is gone.
fn thread_pid(h: HWND) -> Option<u32> {
    let mut pid = 0u32;
    // SAFETY: pid is a local the call writes; a window that is gone gives thread 0.
    let thread = unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    (thread != 0 && pid != 0).then_some(pid)
}

fn is_frame(h: HWND) -> bool {
    let mut class = [0u16; 64];
    // SAFETY: any HWND is accepted; the buffer is a local array whose length the slice carries.
    let n = unsafe { GetClassNameW(h, &mut class) }.max(0) as usize;
    class.get(..n).is_some_and(|c| c.iter().copied().eq(FRAME_CLASS.encode_utf16()))
}

impl Os for Win32Os {
    fn open(&mut self, pid: u32) -> Result<OsHandle, u32> {
        // SAFETY: OpenProcess takes no pointers; end_key closes the handle on every path.
        match unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
            Ok(h) => Ok(OsHandle(h.0 as isize)),
            Err(e) => Err(access::win32_code(&e)),
        }
    }

    fn created(&mut self, h: OsHandle) -> Option<u64> {
        access::creation_time(handle(h))
    }

    fn session(&mut self, pid: u32) -> Option<u32> {
        let mut s = 0u32;
        // SAFETY: s is a local the call writes.
        unsafe { ProcessIdToSessionId(pid, &mut s) }.ok().map(|()| s)
    }

    fn image_path(&mut self, h: OsHandle) -> Result<String, u32> {
        // The longest path Windows gives, on the heap (the act thread's stack is small).
        let mut buf = vec![0u16; 32_768];
        let mut len = buf.len() as u32;
        // SAFETY: buf is writable for len units; the call writes at most len and sets it to the length written.
        match unsafe { QueryFullProcessImageNameW(handle(h), PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) } {
            Ok(()) => Ok(buf.get(..len as usize).map(String::from_utf16_lossy).unwrap_or_default()),
            Err(e) => Err(access::win32_code(&e)),
        }
    }

    fn critical(&mut self, h: OsHandle) -> Option<bool> {
        let mut c = BOOL(0);
        // SAFETY: h is an open process handle; c is a local the call writes.
        unsafe { IsProcessCritical(handle(h), &mut c) }.ok().map(|()| c.as_bool())
    }

    fn terminate(&mut self, h: OsHandle) -> Result<(), u32> {
        // SAFETY: h is an open handle with PROCESS_TERMINATE whose create time and rules were checked just now.
        unsafe { TerminateProcess(handle(h), 1) }.map_err(|e| access::win32_code(&e))
    }

    fn close(&mut self, h: OsHandle) {
        access::close(handle(h));
    }

    /// The owner rule of the frame (engine/windows.rs read_window): a packaged app's frame belongs to the process of its
    /// CoreWindow child when it has one, else, like every other window, to the process that created it.
    fn window_pid(&mut self, hwnd: isize) -> Option<u32> {
        let h = HWND(hwnd as *mut core::ffi::c_void);
        let pid = thread_pid(h)?;
        if is_frame(h) {
            // SAFETY: any HWND is accepted; a window that is gone gives an error or a null handle, no message is sent.
            let core = unsafe { FindWindowExW(Some(h), None, w!("Windows.UI.Core.CoreWindow"), PCWSTR::null()) };
            if let Some(app) = core.ok().filter(|c| !c.0.is_null()).and_then(thread_pid) {
                return Some(app);
            }
        }
        Some(pid)
    }

    fn post_close(&mut self, hwnd: isize) -> Result<(), u32> {
        // SAFETY: no pointers; a window that is gone makes the call fail with ERROR_INVALID_WINDOW_HANDLE.
        unsafe { PostMessageW(Some(HWND(hwnd as *mut core::ffi::c_void)), WM_CLOSE, WPARAM(0), LPARAM(0)) }
            .map_err(|e| access::win32_code(&e))
    }
}
