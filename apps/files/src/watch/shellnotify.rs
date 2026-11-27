//! The shell's change notifications, on a thread of their own with a message-only window (never shown): the fallback
//! for a folder whose file system refuses ReadDirectoryChangesW, the drive notifications and the association change.
//! The thread exists only while a fallback folder or drives(true) needs it, and sleeps in GetMessage otherwise.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    ILCreateFromPathW, ILFree, SHCNRF_InterruptLevel, SHCNRF_NewDelivery, SHCNRF_ShellLevel, SHChangeNotification_Lock,
    SHChangeNotification_Unlock, SHChangeNotifyDeregister, SHChangeNotifyEntry, SHChangeNotifyRegister,
    SHGetPathFromIDListEx, GPFIDL_DEFAULT, SHCNE_ASSOCCHANGED, SHCNE_ATTRIBUTES, SHCNE_CREATE, SHCNE_DELETE,
    SHCNE_DRIVEADD, SHCNE_DRIVEREMOVED, SHCNE_MEDIAINSERTED, SHCNE_MEDIAREMOVED, SHCNE_MKDIR, SHCNE_RENAMEFOLDER,
    SHCNE_RENAMEITEM, SHCNE_RMDIR, SHCNE_UPDATEDIR, SHCNE_UPDATEITEM, SHCNRF_SOURCE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, GetWindowLongPtrW, KillTimer,
    PostMessageW, PostQuitMessage, RegisterClassExW, SetTimer, SetWindowLongPtrW, TranslateMessage, UnregisterClassW,
    GWLP_USERDATA, HWND_MESSAGE, MSG, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_NCDESTROY, WM_TIMER, WNDCLASSEXW,
};

use super::{ChangeAction, Coalescer, RawChange, Shared, WatchError, WatchEvent, WatchId};
use crate::listing::collate::utf16_to_wtf8;
use crate::listing::{Gen, WidePath};

/// The shell's delivery of a change (SHChangeNotifyRegister's message).
const WM_SHELL: u32 = WM_APP + 1;
/// The thread's mailbox has a command.
const WM_CMD: u32 = WM_APP + 2;
const TIMER_FLUSH: usize = 1;

const FOLDER_EVENTS: i32 = (SHCNE_CREATE.0
    | SHCNE_DELETE.0
    | SHCNE_MKDIR.0
    | SHCNE_RMDIR.0
    | SHCNE_RENAMEITEM.0
    | SHCNE_RENAMEFOLDER.0
    | SHCNE_UPDATEITEM.0
    | SHCNE_UPDATEDIR.0
    | SHCNE_ATTRIBUTES.0) as i32;
/// A change made by a plain Win32 call reaches the shell only at its interrupt level; shell level alone sees what
/// programs report with SHChangeNotify.
const FOLDER_SOURCES: SHCNRF_SOURCE =
    SHCNRF_SOURCE(SHCNRF_InterruptLevel.0 | SHCNRF_ShellLevel.0 | SHCNRF_NewDelivery.0);
const DRIVE_EVENTS: i32 =
    (SHCNE_DRIVEADD.0 | SHCNE_DRIVEREMOVED.0 | SHCNE_MEDIAINSERTED.0 | SHCNE_MEDIAREMOVED.0 | SHCNE_ASSOCCHANGED.0)
        as i32;

enum Cmd {
    AddFolder { id: WatchId, gen: Gen, dir: Vec<u16>, reply: Sender<Result<(), WatchError>> },
    RemoveFolder(WatchId),
    Drives(bool),
    Quit,
}

struct Mailbox {
    cmds: Mutex<VecDeque<Cmd>>,
}

/// The thread and its window, seen from the callers.
pub(crate) struct Shell {
    hwnd: isize,
    mailbox: Arc<Mailbox>,
    thread: Option<JoinHandle<()>>,
}

struct Folder {
    gen: Gen,
    reg: u32,
    pidl: *mut ITEMIDLIST,
    /// The folder's path as lower-cased units (the shell's path of a child has the parent's spelling, not
    /// necessarily the case the caller used).
    dir: Vec<u16>,
    co: Coalescer,
}

/// What the window procedure works on: owned by the thread, reached through GWLP_USERDATA.
struct State {
    shared: Arc<Shared>,
    mailbox: Arc<Mailbox>,
    folders: HashMap<u32, Folder>,
    drives_reg: Option<u32>,
    timer_on: bool,
}

impl Shell {
    pub(crate) fn new(shared: Arc<Shared>) -> Result<Shell, WatchError> {
        let mailbox = Arc::new(Mailbox { cmds: Mutex::new(VecDeque::new()) });
        let (tx, rx) = channel::<Result<isize, u32>>();
        let (m2, s2) = (mailbox.clone(), shared);
        let thread = std::thread::Builder::new()
            .name("files-watch-shell".into())
            .spawn(move || thread_main(m2, s2, tx))
            .map_err(|_| WatchError::Open(0))?;
        match rx.recv() {
            Ok(Ok(hwnd)) => Ok(Shell { hwnd, mailbox, thread: Some(thread) }),
            Ok(Err(code)) => {
                let _ = thread.join();
                Err(WatchError::Open(code))
            }
            Err(_) => Err(WatchError::Open(0)),
        }
    }

    fn post(&self, c: Cmd) {
        self.mailbox.cmds.lock().unwrap_or_else(|p| p.into_inner()).push_back(c);
        // SAFETY: the window belongs to the thread, which is joined in Drop; posting to a window that is gone fails
        // harmlessly.
        let _ = unsafe { PostMessageW(Some(HWND(self.hwnd as *mut _)), WM_CMD, WPARAM(0), LPARAM(0)) };
    }

    /// Registers `dir` for the shell's change notifications; waits for the registration's outcome.
    pub(crate) fn add_folder(&self, id: WatchId, gen: Gen, dir: &WidePath) -> Result<(), WatchError> {
        let (reply, rx) = channel();
        self.post(Cmd::AddFolder { id, gen, dir: dir.raw(), reply });
        rx.recv().unwrap_or(Err(WatchError::Open(0)))
    }

    pub(crate) fn remove_folder(&self, id: WatchId) {
        self.post(Cmd::RemoveFolder(id));
    }

    pub(crate) fn drives(&self, on: bool) {
        self.post(Cmd::Drives(on));
    }
}

impl Drop for Shell {
    fn drop(&mut self) {
        self.post(Cmd::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn thread_main(mailbox: Arc<Mailbox>, shared: Arc<Shared>, ready: Sender<Result<isize, u32>>) {
    // SAFETY: COM for the shell's pidl functions, on this thread only; undone at the end.
    let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
    let class = w!("FilesWatchSink");
    // SAFETY: a null module name asks for this executable's handle.
    let hinst = unsafe { GetModuleHandleW(PCWSTR::null()) }.map(|h| h.into()).unwrap_or_default();
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wnd_proc),
        hInstance: hinst,
        lpszClassName: class,
        ..Default::default()
    };
    // SAFETY: a fully initialised class; a second registration (another watcher in the process) fails and is fine.
    let _ = unsafe { RegisterClassExW(&wc) };
    // SAFETY: a message-only window of the class above; never shown.
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            PCWSTR::null(),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(hinst),
            None,
        )
    };
    let hwnd = match hwnd {
        Ok(h) => h,
        Err(e) => {
            let _ = ready.send(Err((e.code().0 as u32) & 0xFFFF));
            if com {
                // SAFETY: balances the CoInitializeEx above.
                unsafe { CoUninitialize() };
            }
            return;
        }
    };
    let state = Box::new(State { shared, mailbox, folders: HashMap::new(), drives_reg: None, timer_on: false });
    let raw = Box::into_raw(state);
    // SAFETY: the pointer stays valid until WM_NCDESTROY below frees it; only this thread's window proc uses it.
    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, raw as isize) };
    let _ = ready.send(Ok(hwnd.0 as isize));
    let mut msg = MSG::default();
    // SAFETY: the standard loop of the thread that owns the window; it ends on WM_QUIT.
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {
        // SAFETY: msg was just filled in by GetMessageW.
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    // SAFETY: the class belongs to this module and no window of it is left on this thread.
    let _ = unsafe { UnregisterClassW(class, Some(hinst)) };
    if com {
        // SAFETY: balances the CoInitializeEx above.
        unsafe { CoUninitialize() };
    }
}

fn state<'a>(hwnd: HWND) -> Option<&'a mut State> {
    // SAFETY: the user data is the Box leaked in thread_main, valid until WM_NCDESTROY clears it; window procedures
    // of one window run on one thread, one at a time. A caller must not keep the returned reference across a call
    // that can pump messages (SHChangeNotifyRegister, ILCreateFromPathW, Deregister): a nested window procedure would
    // make a second `&mut State`. on_commands therefore re-acquires the state around each such call.
    let p = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut State;
    unsafe { p.as_mut() }
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_SHELL => {
            if let Some(st) = state(hwnd) {
                on_shell(st, hwnd, HANDLE(wp.0 as *mut _), lp.0 as u32);
            }
            LRESULT(0)
        }
        WM_CMD => {
            on_commands(hwnd);
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_FLUSH => {
            if let Some(st) = state(hwnd) {
                // SAFETY: the one-shot timer of this window.
                let _ = unsafe { KillTimer(Some(hwnd), TIMER_FLUSH) };
                st.timer_on = false;
                let now = Instant::now();
                let mut woke = false;
                for (&id, f) in st.folders.iter_mut() {
                    woke |= f.co.flush(&st.shared, WatchId(id), f.gen, now);
                }
                arm_timer(st, hwnd);
                if woke {
                    (st.shared.notify)();
                }
            }
            LRESULT(0)
        }
        WM_NCDESTROY => {
            // SAFETY: take the state back from the window and drop it; later messages find no state.
            let p = unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) } as *mut State;
            if !p.is_null() {
                drop(unsafe { Box::from_raw(p) });
            }
            // SAFETY: default handling.
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
        // SAFETY: default handling.
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

/// One-shot timer for the earliest held delivery; none when nothing is held (no wake-ups while idle).
fn arm_timer(st: &mut State, hwnd: HWND) {
    if st.timer_on {
        return;
    }
    let now = Instant::now();
    if let Some(due) = st.folders.values().filter_map(|f| f.co.due).min() {
        let ms = u32::try_from(due.saturating_duration_since(now).as_micros().div_ceil(1000)).unwrap_or(100).max(1);
        // SAFETY: a timer of this window, killed when it fires.
        st.timer_on = unsafe { SetTimer(Some(hwnd), TIMER_FLUSH, ms, None) } != 0;
    }
}

fn on_commands(hwnd: HWND) {
    loop {
        // The state borrow ends with this statement: nothing below holds it across a Win32 call.
        let mailbox = state(hwnd).map(|st| st.mailbox.clone());
        let Some(cmd) = mailbox.and_then(|m| m.cmds.lock().unwrap_or_else(|p| p.into_inner()).pop_front()) else {
            break;
        };
        match cmd {
            Cmd::AddFolder { id, gen, dir, reply } => {
                let r = register_folder(hwnd, &dir).and_then(|(reg, pidl, dir)| match state(hwnd) {
                    Some(st) => {
                        st.folders.insert(id.0, Folder { gen, reg, pidl, dir, co: Coalescer::default() });
                        Ok(())
                    }
                    None => {
                        release(Folder { gen, reg, pidl, dir, co: Coalescer::default() });
                        Err(WatchError::Open(0))
                    }
                });
                let _ = reply.send(r);
            }
            Cmd::RemoveFolder(id) => {
                let f = state(hwnd).and_then(|st| st.folders.remove(&id.0));
                if let Some(f) = f {
                    release(f);
                }
            }
            Cmd::Drives(on) => set_drives(hwnd, on),
            Cmd::Quit => {
                let Some((folders, timer)) =
                    state(hwnd).map(|st| (std::mem::take(&mut st.folders), std::mem::replace(&mut st.timer_on, false)))
                else {
                    return;
                };
                for (_, f) in folders {
                    release(f);
                }
                set_drives(hwnd, false);
                if timer {
                    // SAFETY: this window's timer.
                    let _ = unsafe { KillTimer(Some(hwnd), TIMER_FLUSH) };
                }
                // SAFETY: the window of this thread; WM_NCDESTROY frees the state, then the loop ends.
                unsafe {
                    let _ = DestroyWindow(hwnd);
                    PostQuitMessage(0);
                }
                return;
            }
        }
    }
}

fn release(f: Folder) {
    // SAFETY: the registration id and the pidl are this folder's own, released once.
    unsafe {
        let _ = SHChangeNotifyDeregister(f.reg);
        ILFree(Some(f.pidl));
    }
}

/// Registers one folder with the shell. Takes no state borrow: both calls can pump messages. Returns the
/// registration id, the pidl and the lower-cased plain path.
fn register_folder(hwnd: HWND, dir_raw: &[u16]) -> Result<(u32, *mut ITEMIDLIST, Vec<u16>), WatchError> {
    // The shell wants a plain path, not the extended-length form raw() gives.
    let plain = plain_path(dir_raw);
    // SAFETY: NUL-terminated and alive for the call.
    let pidl = unsafe { ILCreateFromPathW(PCWSTR(plain.as_ptr())) };
    if pidl.is_null() {
        return Err(WatchError::Open(2));
    }
    let entry = SHChangeNotifyEntry { pidl, fRecursive: false.into() };
    // SAFETY: one valid entry; the window is this thread's.
    let reg = unsafe { SHChangeNotifyRegister(hwnd, FOLDER_SOURCES, FOLDER_EVENTS, WM_SHELL, 1, &entry) };
    if reg == 0 {
        // SAFETY: the pidl made above, not stored anywhere.
        unsafe { ILFree(Some(pidl)) };
        return Err(WatchError::Unsupported);
    }
    Ok((reg, pidl, lower(&plain[..plain.len() - 1])))
}

fn set_drives(hwnd: HWND, on: bool) {
    let current = state(hwnd).and_then(|st| st.drives_reg);
    match (on, current) {
        (true, None) => {
            // The desktop (a null pidl), recursive: the drive and association events are delivered there.
            let entry = SHChangeNotifyEntry { pidl: std::ptr::null_mut(), fRecursive: true.into() };
            // SAFETY: one valid entry; no state borrow is held.
            let reg = unsafe {
                SHChangeNotifyRegister(hwnd, SHCNRF_ShellLevel | SHCNRF_NewDelivery, DRIVE_EVENTS, WM_SHELL, 1, &entry)
            };
            if let Some(st) = state(hwnd) {
                st.drives_reg = (reg != 0).then_some(reg);
            } else if reg != 0 {
                // SAFETY: the id from the registration above, released once.
                let _ = unsafe { SHChangeNotifyDeregister(reg) };
            }
        }
        (false, Some(reg)) => {
            if let Some(st) = state(hwnd) {
                st.drives_reg = None;
            }
            // SAFETY: the id from the registration above, released once.
            let _ = unsafe { SHChangeNotifyDeregister(reg) };
        }
        _ => {}
    }
}

/// "\\?\X:\a" -> "X:\a", "\\?\UNC\s\x" -> "\\s\x"; NUL-terminated in and out.
fn plain_path(raw: &[u16]) -> Vec<u16> {
    let s = String::from_utf16_lossy(raw.strip_suffix(&[0]).unwrap_or(raw));
    let plain = if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        s
    };
    let mut v: Vec<u16> = plain.encode_utf16().collect();
    v.push(0);
    v
}

fn lower(units: &[u16]) -> Vec<u16> {
    String::from_utf16_lossy(units).to_lowercase().encode_utf16().collect()
}

fn path_of(pidl: *const ITEMIDLIST) -> Option<Vec<u16>> {
    if pidl.is_null() {
        return None;
    }
    let mut buf = [0u16; 1024];
    // SAFETY: a pidl delivered by the shell and a writable buffer of the length given.
    if !unsafe { SHGetPathFromIDListEx(pidl, &mut buf, GPFIDL_DEFAULT) }.as_bool() {
        return None;
    }
    let n = buf.iter().position(|&u| u == 0)?;
    Some(buf[..n].to_vec())
}

/// (the parent's lower-cased path, the last component) of a full path.
fn split(path: &[u16]) -> Option<(Vec<u16>, &[u16])> {
    let cut = path.iter().rposition(|&u| u == u16::from(b'\\'))?;
    Some((lower(&path[..cut]), &path[cut + 1..]))
}

fn change(action: ChangeAction, name: &[u16]) -> RawChange {
    let mut wtf8 = Vec::with_capacity(name.len());
    utf16_to_wtf8(name, &mut wtf8);
    RawChange { action, name: wtf8.into_boxed_slice() }
}

fn on_shell(st: &mut State, hwnd: HWND, change_handle: HANDLE, pid: u32) {
    let mut pidls: *mut *mut ITEMIDLIST = std::ptr::null_mut();
    let mut event: i32 = 0;
    // SAFETY: the handle and process id the shell delivered with the message.
    let lock = unsafe { SHChangeNotification_Lock(change_handle, pid, Some(&mut pidls), Some(&mut event)) };
    if lock.is_invalid() {
        return;
    }
    let (p1, p2) = if pidls.is_null() {
        (std::ptr::null_mut(), std::ptr::null_mut())
    } else {
        // SAFETY: the lock holds two pidl pointers (either may be null).
        unsafe { (*pidls, *pidls.add(1)) }
    };
    let ev = event as u32;
    let mut woke = false;
    if ev & (SHCNE_DRIVEADD.0 | SHCNE_DRIVEREMOVED.0 | SHCNE_MEDIAINSERTED.0 | SHCNE_MEDIAREMOVED.0) != 0 {
        woke |= st.shared.check_drives();
    }
    if ev & SHCNE_ASSOCCHANGED.0 != 0 {
        st.shared.push(WatchEvent::AssocChanged);
        woke = true;
    }
    if ev & FOLDER_EVENTS as u32 != 0 && !st.folders.is_empty() {
        let (a, b) = (path_of(p1), path_of(p2));
        woke |= route(st, ev, a.as_deref(), b.as_deref());
    }
    // SAFETY: the lock taken above, released once.
    let _ = unsafe { SHChangeNotification_Unlock(lock) };
    arm_timer(st, hwnd);
    if woke {
        (st.shared.notify)();
    }
}

/// Hands one shell event to the folders it concerns.
fn route(st: &mut State, ev: u32, a: Option<&[u16]>, b: Option<&[u16]>) -> bool {
    let now = Instant::now();
    let mut woke = false;
    let parent_a = a.and_then(split);
    let parent_b = b.and_then(split);
    let a_lower = a.map(lower);
    for (&id, f) in st.folders.iter_mut() {
        let id = WatchId(id);
        let in_a = parent_a.as_ref().filter(|(p, _)| *p == f.dir).map(|(_, n)| *n);
        let in_b = parent_b.as_ref().filter(|(p, _)| *p == f.dir).map(|(_, n)| *n);
        let is_self = a_lower.as_deref() == Some(&f.dir[..]);
        let mut items = Vec::new();
        if ev & (SHCNE_CREATE.0 | SHCNE_MKDIR.0) != 0 {
            items.extend(in_a.map(|n| change(ChangeAction::Added, n)));
        }
        if ev & (SHCNE_DELETE.0 | SHCNE_RMDIR.0) != 0 {
            if is_self {
                st.shared.push(WatchEvent::Gone { id, gen: f.gen });
                woke = true;
            }
            items.extend(in_a.map(|n| change(ChangeAction::Removed, n)));
        }
        if ev & (SHCNE_UPDATEITEM.0 | SHCNE_ATTRIBUTES.0) != 0 {
            items.extend(in_a.map(|n| change(ChangeAction::Modified, n)));
        }
        if ev & (SHCNE_RENAMEITEM.0 | SHCNE_RENAMEFOLDER.0) != 0 {
            items.extend(in_a.map(|n| change(ChangeAction::RenamedFrom, n)));
            items.extend(in_b.map(|n| change(ChangeAction::RenamedTo, n)));
        }
        if ev & SHCNE_UPDATEDIR.0 != 0 && is_self {
            f.co.clear();
            woke |= st.shared.push_rescan(id, f.gen);
        }
        if !items.is_empty() {
            woke |= f.co.add(&st.shared, id, f.gen, items, now);
        }
    }
    woke
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn plain_path_strips_the_extended_prefixes() {
        let p = |s: &str| {
            let mut raw = u(s);
            raw.push(0);
            String::from_utf16(&plain_path(&raw)[..plain_path(&raw).len() - 1]).unwrap()
        };
        assert_eq!(p(r"\\?\C:\a\b"), r"C:\a\b");
        assert_eq!(p(r"\\?\UNC\srv\share\x"), r"\\srv\share\x");
        assert_eq!(p(r"D:\x"), r"D:\x");
    }

    #[test]
    fn split_gives_a_lower_case_parent_and_the_name() {
        let path = u(r"C:\Some Dir\File.TXT");
        let (parent, name) = split(&path).unwrap();
        assert_eq!(parent, u(r"c:\some dir"));
        assert_eq!(name, &u("File.TXT")[..]);
        assert!(split(&u("nobackslash")).is_none());
    }
}
