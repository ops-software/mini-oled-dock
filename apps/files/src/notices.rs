//! The updated notice and Reopen (P3-files-app 2.2, F4.14; P2-plumbing F-P7). When the dock reports that the exe was
//! replaced, every main window says so with [Reopen]; nothing restarts on its own. Reopen keeps the open windows'
//! locations in session.json's reopen list and ends the event loop; app.rs then releases the mutex and the IPC window
//! and starts the installed exe with no argument (restart_if_due), and that process restores the list instead of Home.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::rc::Rc;

use oled_trust::frame::files::Location;
use oled_ui::{EventCx, Reply};
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{CreateProcessW, CREATE_NO_WINDOW, PROCESS_INFORMATION, STARTUPINFOW};

use crate::listing::WidePath;
use crate::session::Session;
use crate::status::{self, Notice, NoticeAct};
use crate::tr::{k, t, tf};
use crate::window::{self, FilesScreen, LoopReq, Shared, WinKind};
use crate::{instance, linger, names, tab};

/// Windows and tabs a reopen keeps.
pub const REOPEN_WINDOWS: usize = 8;
pub const REOPEN_TABS: usize = 8;
/// The updated notice stays longer than others: the update usually lands while the user looks elsewhere.
pub const UPDATED_HOLD_MS: u32 = 60_000;

/// Msg::Updated (to every window): the notice "<name> was updated" with [Reopen]. A window that lingers closes
/// instead, and no window of this process lingers any more: a later start then runs the new exe.
pub fn on_updated(s: &mut FilesScreen, cx: &mut EventCx, version: u32) {
    let (major, minor, patch) = (version >> 16, (version >> 8) & 0xFF, version & 0xFF);
    oled_sys::log!("files: the exe was updated to {major}.{minor}.{patch} while this process runs");
    s.shared.exe_updated.set(true);
    if s.linger.on() {
        if linger::close_now(s, cx) == Reply::Close {
            cx.close_window();
        }
        return;
    }
    updated_notice(s, cx);
}

/// "<name> was updated" with [Reopen], held UPDATED_HOLD_MS.
fn updated_notice(s: &mut FilesScreen, cx: &mut EventCx) {
    let lang = cx.lang();
    let name = lang.kit(oled_ui::strings::k::KIT_APP_FILES);
    let mut n = Notice::new(tf(k::FILES_NOTICE_UPDATED, lang, &[("name", &name)]));
    n.action = Some((t(k::FILES_ACTION_REOPEN, lang), NoticeAct::Reopen));
    n.hold_ms = UPDATED_HOLD_MS;
    status::notice(s, cx, n);
}

/// [Reopen]: this window's tabs, then the other main windows' locations, go to session.json's reopen list, and the
/// event loop ends (app.rs restarts the exe once the mutex and the IPC window are gone). Not while operations run:
/// their window and this process's undo would go (F3.5), and the new process could not follow them (it talks to
/// another build's operations host); the notice comes back instead, for a Reopen once they are done.
pub fn reopen(s: &mut FilesScreen, cx: &mut EventCx) -> Reply {
    let shared = Rc::clone(&s.shared);
    let running = shared.ops.borrow().running();
    if running > 0 {
        oled_sys::log!("files: reopen waits: {running} operation(s) still running");
        updated_notice(s, cx);
        return Reply::Redraw;
    }
    let mine: Vec<String> = s.tabs.iter().take(REOPEN_TABS).map(|t| key_of(t.nav.location())).collect();
    let mut list = vec![mine];
    for w in shared.windows.borrow().iter().filter(|w| w.kind == WinKind::Main && w.win != s.win) {
        if list.len() >= REOPEN_WINDOWS {
            break;
        }
        list.push(vec![w.location.as_ref().map_or_else(String::new, key_of)]);
    }
    let windows = list.len();
    shared.session.borrow_mut().reopen = list;
    if !shared.session.borrow().save(&*shared.services.own) {
        oled_sys::log!("files: {} was not written: the restart opens Home", names::SESSION_FILE);
    }
    oled_sys::log!("files: reopen: {windows} window(s) kept, the process starts again");
    window::request_loop(&shared, LoopReq::Quit);
    Reply::None
}

/// A location as the reopen list keeps it: a folder's path, "" for Home (and the pages a tab cannot list).
fn key_of(l: &Location) -> String {
    match l {
        Location::Fs(p) => p.to_string_lossy(),
        _ => String::new(),
    }
}

/// The reverse of key_of; None for a path that does not parse (a damaged file).
fn location_of(key: &str) -> Option<Location> {
    if key.is_empty() {
        return Some(Location::Home);
    }
    WidePath::parse(key).ok().map(Location::Fs)
}

/// app.rs's Step::Reopen: the saved windows (at most 8, each at most 8 tabs, entries that do not parse dropped), and
/// the list is cleared so it is used once.
pub fn take_reopen(session: &mut Session) -> Vec<Vec<Location>> {
    std::mem::take(&mut session.reopen)
        .into_iter()
        .take(REOPEN_WINDOWS)
        .map(|w| w.iter().take(REOPEN_TABS).filter_map(|key| location_of(key)).collect::<Vec<_>>())
        .filter(|w| !w.is_empty())
        .collect()
}

/// A restored window's further tabs, right of the first in their saved order, each listing at once (app.rs, before
/// the window opens: no EventCx yet).
pub fn restore_tabs(s: &mut FilesScreen, rest: Vec<Location>) {
    let now = s.shared.services.clock.now_ms();
    for at in rest.into_iter().rev() {
        let t = tab::new_tab(&s.shared, s.win, at);
        let id = s.tabs.open(t, false);
        let fx = s.tabs.by_id_mut(id).map(|t| t.nav.refresh(now)).unwrap_or_default();
        window::run_effects(s, None, id, fx);
    }
}

/// After the event loop, with the mutex and the IPC window released: when Reopen ended it, the installed exe starts
/// with no argument (it restores the reopen list). True when it was started.
pub fn restart_if_due(shared: &Shared) -> bool {
    restart_with(shared, &mut spawn_exe)
}

/// restart_if_due with the start given (tests record it): `spawn(exe, command line)`.
pub fn restart_with(shared: &Shared, spawn: &mut dyn FnMut(&Path, &[u16]) -> bool) -> bool {
    if shared.session.borrow().reopen.is_empty() {
        return false;
    }
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            oled_sys::log!("files: reopen: no exe path ({e})");
            return false;
        }
    };
    let wide: Vec<u16> = exe.as_os_str().encode_wide().collect();
    let started = spawn(&exe, &instance::quote_arg(&wide));
    if !started {
        oled_sys::log!("files: reopen: the new process did not start; the list waits for the next start");
    }
    started
}

/// CreateProcessW of the exe with the command line (its quoted path alone), in this process's folder.
fn spawn_exe(exe: &Path, line: &[u16]) -> bool {
    let app: Vec<u16> = exe.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut cmd: Vec<u16> = line.iter().copied().chain(Some(0)).collect();
    let si = STARTUPINFOW { cb: std::mem::size_of::<STARTUPINFOW>() as u32, ..Default::default() };
    let mut pi = PROCESS_INFORMATION::default();
    // CREATE_NO_WINDOW is the package's rule for every start (lint no_window_spawns); a GUI program ignores it.
    // SAFETY: `app` is NUL-terminated, `cmd` is a writable NUL-terminated buffer, `si` and `pi` live for the call;
    // no handle is inherited and the folder is this process's.
    let made = unsafe {
        CreateProcessW(
            PCWSTR(app.as_ptr()),
            Some(PWSTR(cmd.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_NO_WINDOW,
            None,
            PCWSTR::null(),
            &si,
            &mut pi,
        )
    };
    match made {
        Ok(()) => {
            // SAFETY: both handles were returned open by CreateProcessW and are closed once.
            unsafe {
                let _ = CloseHandle(pi.hThread);
                let _ = CloseHandle(pi.hProcess);
            }
            true
        }
        Err(e) => {
            oled_sys::log!("files: reopen: CreateProcessW failed ({e})");
            false
        }
    }
}
