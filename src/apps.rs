//! Top-level windows "to show", grouping by app, WinEventHook, activation.
//!
//! THREADING: EVERYTHING on the UI thread (thread-local state). The WinEvent callback (out-of-context)
//! is run by Windows on the thread that installed the hook (the UI thread, through its
//! message loop): it only does `PostMessageW(WM_DOCK_APPS_DIRTY)`; the UI then, after the 100 ms debounce
//! (`TIMER_APPS_DEBOUNCE`), calls `refresh`. No slow COM calls here: the identity
//! (AUMID) uses `SHGetPropertyStoreForWindow`, which is fast (it reads already published properties)
//! and must be called only once per HWND (result cached in the module). The UI thread has COM STA
//! initialized by `main` (see the main.rs docs) precisely for this reason.
//!
//! Program name and icon are NOT here: they are resolved by the shell thread (`shell_thread`).
//!
//! GROUPING KEY = window AUMID ?? package AUMID (`GetApplicationUserModelId`) ??
//! normalized lowercase exe path. `ApplicationFrameHost` windows without an AUMID: `hwnd:<n>`.
//!
//! IMPLEMENTATION DETAILS
//! - Identity cache per HWND (`State::cache`). The expensive part (OpenProcess, exe path,
//!   package AUMID, window properties) is done only once; the entry is however re-evaluated
//!   if the window AUMID was missing (apps may publish it after creation): every 3 s,
//!   or every 500 ms for an `ApplicationFrameHost` frame still unresolved. Entries for
//!   windows that disappeared from the enumeration are dropped on every `refresh` (no HWND reuse).
//! - `ApplicationFrameHost`: the frame process is the host, not the app. The child window
//!   `Windows.UI.Core.CoreWindow` (process of the real app) is looked up: exe and package AUMID
//!   are taken from there if the window property is missing. If nothing is found: key `hwnd:<n>`.
//! - MRU order: each window has an increasing `stamp` updated when it becomes the
//!   foreground window (read in `refresh`, or right after a click). `AppGroup::windows` is sorted by
//!   decreasing `stamp`. The multi-window cycle instead uses the stable order of appearance
//!   (`seq`), so each click moves to the next window without bouncing between the last two.
//! - No `RefCell` stays borrowed while calling APIs that may pump messages
//!   (`SendMessageTimeoutW`, `SetForegroundWindow`...): the Win32 calls are outside the `borrow`s.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::time::{Duration, Instant};

use windows::core::{w, BOOL, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, PROPERTYKEY, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PropVariantToStringAlloc};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, EnumWindows, FindWindowExW, GetAncestor, GetClassNameW, GetDesktopWindow,
    GetForegroundWindow, GetPropW, GetWindow, GetWindowLongW, GetWindowThreadProcessId, IsIconic, IsWindow,
    IsWindowVisible, PostMessageW, SendMessageTimeoutW, SetForegroundWindow, ShowWindowAsync, SwitchToThisWindow,
    ASFW_ANY, CHILDID_SELF, EVENT_OBJECT_CLOAKED, EVENT_OBJECT_CREATE, EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE,
    EVENT_OBJECT_SHOW, EVENT_OBJECT_UNCLOAKED, EVENT_SYSTEM_FOREGROUND, GA_PARENT, GA_ROOT, GA_ROOTOWNER,
    GET_WINDOW_CMD, GWL_EXSTYLE, OBJID_WINDOW, SC_MINIMIZE, SC_RESTORE, SMTO_ABORTIFHUNG, SMTO_BLOCK, SW_MINIMIZE,
    SW_RESTORE, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_CLOSE, WM_SYSCOMMAND, WS_EX_APPWINDOW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};

use crate::log;
use crate::msg::WM_DOCK_APPS_DIRTY;
use oled_shellmodel::identity::is_application_frame_host;
use oled_shellmodel::process::{exe_path as image_path, package_aumid, OwnedProcess, RootCache};
// The identity rules live in the shell model; callers keep these paths.
pub use oled_shellmodel::identity::{aumid_from_launch, key_from_exe_path, same_exe_other_version, updated_exe_path};
pub(crate) use oled_shellmodel::process::{
    creation_time_of_handle as creation_time, exe_path_of_handle as query_exe_path, exe_path_of_pid,
    parent_pid_of_handle as parent_pid,
};

const GW_OWNER: GET_WINDOW_CMD = GET_WINDOW_CMD(4);

/// `PKEY_AppUserModel_ID` {9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3}, 5. Defined here to avoid depending
/// on the `Win32_Storage_EnhancedStorage` feature (which exports thousands of constants).
const PKEY_APP_USER_MODEL_ID: PROPERTYKEY =
    PROPERTYKEY { fmtid: windows::core::GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3), pid: 5 };

/// Re-check schedule (time since first resolution) for windows without a window-level AUMID; after
/// the last step the re-check keeps going at the last interval (see `recheck_due`).
const AUMID_RECHECK: [Duration; 4] =
    [Duration::from_secs(3), Duration::from_secs(10), Duration::from_secs(30), Duration::from_secs(120)];
/// Same, for an unresolved `ApplicationFrameHost` frame.
const AFH_RECHECK: [Duration; 6] = [
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(10),
    Duration::from_secs(30),
];
/// Timeout for synchronous messages to other apps' windows (minimize/restore).
const SYSCMD_TIMEOUT_MS: u32 = 300;

/// Identity of a window: which app it belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowIdentity {
    pub pid: u32,
    /// Exe path (original case, from `QueryFullProcessImageNameW`).
    pub exe_path: String,
    /// AUMID of the window or, failing that, of the package.
    pub aumid: Option<String>,
    /// Grouping key (see the module docs).
    pub key: String,
}

/// A group = an app entry with at least one open window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppGroup {
    /// Grouping key.
    pub key: String,
    pub exe_path: String,
    pub aumid: Option<String>,
    /// HWNDs of the group's windows (as isize), in MRU order: the most recent first.
    pub windows: Vec<isize>,
    /// true if one of the group's windows is the foreground window.
    pub active: bool,
    /// Sequence number of first appearance (display order of non-pinned apps).
    pub first_seen: u64,
}

/// Outcome of `click_group`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClickOutcome {
    /// No window for the key: the caller (UI) must launch the app.
    NoWindows,
    /// A window was activated (restored if minimized).
    Activated(isize),
    /// The group's foreground window was minimized.
    Minimized(isize),
}

/// Per-HWND cache entry.
struct Entry {
    ident: WindowIdentity,
    /// Process owning the window at resolution time (guard against HWND reuse).
    pid: u32,
    /// Stable order of appearance of the window (for the multi-window cycle).
    seq: u64,
    /// MRU stamp: higher = activated more recently.
    stamp: u64,
    /// true if the AUMID came from the window property (no longer needs re-evaluation).
    win_aumid_found: bool,
    /// true if it is an `ApplicationFrameHost` frame with the key fallen back to `hwnd:<n>`.
    afh_unresolved: bool,
    /// First resolution (backoff origin).
    first: Instant,
    /// Number of re-resolutions done so far (index into the backoff schedule).
    checks: u32,
}

#[derive(Default)]
struct State {
    groups: Vec<AppGroup>,
    next_group_seq: u64,
    next_win_seq: u64,
    stamp_counter: u64,
    /// Last tracked window seen in the foreground (0 = none).
    last_fg: isize,
    cache: HashMap<isize, Entry>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
    static HOOKS: RefCell<Vec<HWINEVENTHOOK>> = const { RefCell::new(Vec::new()) };
}

#[inline]
fn hw(h: isize) -> HWND {
    HWND(h as *mut c_void)
}

// ------------------------------------------------------------------ WinEventHook

/// Installs the `SetWinEventHook` hooks (OUTOFCONTEXT | SKIPOWNPROCESS) for
/// `EVENT_OBJECT_SHOW/HIDE/DESTROY/CREATE`, `EVENT_SYSTEM_FOREGROUND`, `EVENT_OBJECT_CLOAKED/UNCLOAKED`.
/// The callback posts `WM_DOCK_APPS_DIRTY` (wParam = event, lParam = HWND) to the dock window, and hands every raw
/// event to `taskbar::on_raw_event` while the taskbar wants them (one hook for both).
/// Called from the UI thread (needs its message loop). Idempotent.
pub fn install_winevent_hook() -> Result<(), String> {
    if HOOKS.with(|h| !h.borrow().is_empty()) {
        return Ok(());
    }
    // CREATE(0x8000)..HIDE(0x8003) = CREATE, DESTROY, SHOW, HIDE; CLOAKED/UNCLOAKED; FOREGROUND.
    let ranges: [(u32, u32, &str); 3] = [
        (EVENT_OBJECT_CREATE, EVENT_OBJECT_HIDE, "create..hide"),
        (EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED, "cloaked..uncloaked"),
        (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, "foreground"),
    ];
    let mut installed: Vec<HWINEVENTHOOK> = Vec::new();
    for (min, max, name) in ranges {
        // SAFETY: `winevent_proc` is a 'static `extern "system"` function with the WINEVENTPROC signature;
        // without WINEVENT_INCONTEXT no module is needed (hmod = None). The current (UI) thread has a
        // message loop, a requirement for out-of-context delivery.
        let hook = unsafe {
            SetWinEventHook(min, max, None, Some(winevent_proc), 0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS)
        };
        if hook.is_invalid() {
            let err = format!("SetWinEventHook({name}) failed: {:?}", windows::core::Error::from_thread());
            for h in installed {
                // SAFETY: handle just obtained from SetWinEventHook on this thread.
                let _ = unsafe { UnhookWinEvent(h) };
            }
            return Err(err);
        }
        installed.push(hook);
    }
    log!("apps: WinEventHooks installed ({})", installed.len());
    HOOKS.with(|h| *h.borrow_mut() = installed);
    Ok(())
}

/// true while the hooks are installed (a thread-local of the UI thread, the thread the taskbar's `install_hook`
/// runs on).
pub(crate) fn hooks_installed() -> bool {
    HOOKS.with(|h| !h.borrow().is_empty())
}

/// Removes the hooks (`UnhookWinEvent`). Idempotent. UI thread.
pub fn remove_winevent_hook() {
    let hooks = HOOKS.with(|h| std::mem::take(&mut *h.borrow_mut()));
    if hooks.is_empty() {
        return;
    }
    for hook in hooks {
        // SAFETY: handle obtained from SetWinEventHook on this same thread, never released before
        // (the list was emptied with `take`).
        let _ = unsafe { UnhookWinEvent(hook) };
    }
    log!("apps: WinEventHooks removed");
}

/// The window styles that decide whether a window could be in the bar (the four style checks that open
/// `is_taskbar_window`).
#[derive(Clone, Copy)]
struct Styles {
    visible: bool,
    tool: bool,
    noactivate: bool,
    app: bool,
    owned: bool,
}

/// Reads the styles of `h` (no message is sent to the window).
fn styles_of(h: HWND) -> Styles {
    // SAFETY: all the APIs below accept arbitrary HWNDs and fail without UB if the window disappears.
    unsafe {
        let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
        Styles {
            visible: IsWindowVisible(h).as_bool(),
            tool: ex & WS_EX_TOOLWINDOW.0 != 0,
            noactivate: ex & WS_EX_NOACTIVATE.0 != 0,
            app: ex & WS_EX_APPWINDOW.0 != 0,
            owned: !GetWindow(h, GW_OWNER).unwrap_or_default().0.is_null(),
        }
    }
}

/// True when a window with these styles passes the first filter of `is_taskbar_window`.
fn could_be_listed(s: Styles) -> bool {
    s.visible && !s.tool && (!s.noactivate || s.app) && (!s.owned || s.app)
}

/// Whether `h` is in the list the bar shows (the cache); None when the cache is borrowed (unknown).
fn listed(h: isize) -> Option<bool> {
    STATE.with(|s| s.try_borrow().ok().map(|st| st.cache.contains_key(&h)))
}

/// Whether a window event can change the bar and so must post `WM_DOCK_APPS_DIRTY`. DESTROY, HIDE and CLOAKED
/// matter only for a listed window; CREATE, SHOW and UNCLOAKED for a listed one or one that could be listed
/// now; FOREGROUND always. Unknown listing: post (fail open). `styles` runs only when it decides.
fn post_wanted(event: u32, listed: Option<bool>, styles: impl FnOnce() -> Styles) -> bool {
    if event == EVENT_SYSTEM_FOREGROUND || listed != Some(false) {
        return true;
    }
    match event {
        EVENT_OBJECT_DESTROY | EVENT_OBJECT_HIDE | EVENT_OBJECT_CLOAKED => false,
        EVENT_OBJECT_CREATE | EVENT_OBJECT_SHOW | EVENT_OBJECT_UNCLOAKED => could_be_listed(styles()),
        _ => true,
    }
}

/// WinEvent callback (UI thread, during the message loop). Filters child-control noise and windows that cannot
/// change the bar, and posts `WM_DOCK_APPS_DIRTY`. Reads `STATE` only with `try_borrow` (no reentrancy panic).
unsafe extern "system" fn winevent_proc(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    #[cfg(feature = "wake-probe")]
    let _probe = crate::probe::winevent(crate::probe::Src::Apps, event, hwnd);
    if hwnd.0.is_null() {
        return;
    }
    // The taskbar has no hook of its own: it sees the same events here, before the filters below (a SHOW on a
    // taskbar's child must still reach it).
    if crate::taskbar::wants_events() {
        crate::taskbar::on_raw_event(event, hwnd, id_object);
    }
    if event != EVENT_SYSTEM_FOREGROUND {
        // Only events on the window itself (not on caret, cursor, accessible elements...).
        if id_object != OBJID_WINDOW.0 || id_child != CHILDID_SELF as i32 {
            return;
        }
        // Discard child windows: the parent of top-levels (and owned windows) is the desktop. DESTROY, HIDE and
        // CLOAKED matter only for a listed window, which is top-level: the listing check below decides alone.
        if !matches!(event, EVENT_OBJECT_DESTROY | EVENT_OBJECT_HIDE | EVENT_OBJECT_CLOAKED) {
            // SAFETY: GetAncestor/GetDesktopWindow accept arbitrary HWNDs and have no side effects.
            let (parent, desktop) = unsafe { (GetAncestor(hwnd, GA_PARENT), GetDesktopWindow()) };
            if !parent.0.is_null() && parent != desktop {
                return;
            }
        }
    }
    if !post_wanted(event, listed(hwnd.0 as isize), || styles_of(hwnd)) {
        return;
    }
    let ui = crate::hwnd();
    if ui.0.is_null() {
        return;
    }
    probe!(crate::probe::winevent_pass(crate::probe::Src::Apps, crate::probe::Stage::AppsDirty, event));
    // SAFETY: PostMessageW is thread-safe; if the window no longer exists it just fails.
    let _ = unsafe { PostMessageW(Some(ui), WM_DOCK_APPS_DIRTY, WPARAM(event as usize), LPARAM(hwnd.0 as isize)) };
}

// ------------------------------------------------------------------ enumeration

/// Re-enumerates the windows (`EnumWindows` + `is_taskbar_window`), associates them with groups (creating the
/// new ones with progressive `first_seen`, removing those without windows) and updates `active` and
/// the MRU order. Returns true if anything changed (groups, windows, `active`).
/// `own` = the dock window, to exclude. UI thread.
pub fn refresh(own: HWND) -> bool {
    let z = enum_taskbar_windows(own); // top first
                                       // Resolve from the bottom towards the top: new windows get increasing `seq`/`stamp`, so
                                       // the highest in Z ends up being the most "recent".
    let mut live: Vec<isize> = Vec::with_capacity(z.len());
    for &h in z.iter().rev() {
        if ensure_entry(h, false).is_some() {
            live.push(h);
        }
    }
    // The active app stays highlighted while the bar holds the foreground it took by Win.
    let mut fg = foreground_target_of(crate::ui::effective_foreground(), &live);
    // A window that just came to the foreground (e.g. a UWP frame restored long after the dock
    // started) gets one forced re-check if its identity is still provisional.
    let fg_changed = STATE.with(|s| s.borrow().last_fg) != fg;
    if fg != 0 && fg_changed {
        let provisional =
            STATE.with(|s| s.borrow().cache.get(&fg).is_some_and(|e| e.afh_unresolved || !e.win_aumid_found));
        if provisional && ensure_entry(fg, true).is_none() {
            live.retain(|&h| h != fg);
            fg = 0;
        }
    }

    STATE.with(|s| {
        let st = &mut *s.borrow_mut();

        let live_set: HashSet<isize> = live.iter().copied().collect();
        st.cache.retain(|h, _| live_set.contains(h));

        if fg != st.last_fg {
            st.last_fg = fg;
            if fg != 0 {
                st.stamp_counter += 1;
                let c = st.stamp_counter;
                if let Some(e) = st.cache.get_mut(&fg) {
                    e.stamp = c;
                }
            }
        }

        struct Acc {
            key: String,
            exe_path: String,
            aumid: Option<String>,
            min_seq: u64,
            wins: Vec<(isize, u64)>,
        }
        let mut accs: Vec<Acc> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        for &h in &live {
            let Some(e) = st.cache.get(&h) else { continue };
            let i = *index.entry(e.ident.key.clone()).or_insert_with(|| {
                accs.push(Acc {
                    key: e.ident.key.clone(),
                    exe_path: e.ident.exe_path.clone(),
                    aumid: e.ident.aumid.clone(),
                    min_seq: e.seq,
                    wins: Vec::new(),
                });
                accs.len() - 1
            });
            let a = &mut accs[i];
            // Group exe/AUMID from the oldest window: stable even if windows arrive
            // with a different exe but the same key (helper processes with the same AUMID).
            if e.seq < a.min_seq {
                a.min_seq = e.seq;
                a.exe_path = e.ident.exe_path.clone();
                a.aumid = e.ident.aumid.clone();
            }
            a.wins.push((h, e.stamp));
        }

        let old_first: HashMap<&str, u64> = st.groups.iter().map(|g| (g.key.as_str(), g.first_seen)).collect();
        // New groups: progressive first_seen in the order of appearance of their oldest window.
        let mut fresh: Vec<usize> =
            (0..accs.len()).filter(|&i| !old_first.contains_key(accs[i].key.as_str())).collect();
        crate::tinysort::sort_by_key(&mut fresh, |&i| accs[i].min_seq);
        let mut assigned: HashMap<usize, u64> = HashMap::new();
        let mut next = st.next_group_seq;
        for i in fresh {
            assigned.insert(i, next);
            next += 1;
        }

        let mut new_groups: Vec<AppGroup> = accs
            .into_iter()
            .enumerate()
            .map(|(i, mut a)| {
                crate::tinysort::sort_by(&mut a.wins, |x, y| y.1.cmp(&x.1)); // decreasing stamp = MRU
                let windows: Vec<isize> = a.wins.iter().map(|w| w.0).collect();
                let first_seen =
                    old_first.get(a.key.as_str()).copied().or_else(|| assigned.get(&i).copied()).unwrap_or(0);
                let active = fg != 0 && windows.contains(&fg);
                AppGroup { key: a.key, exe_path: a.exe_path, aumid: a.aumid, windows, active, first_seen }
            })
            .collect();
        crate::tinysort::sort_by_key(&mut new_groups, |g| g.first_seen);

        st.next_group_seq = next;
        let changed = new_groups != st.groups;
        if changed {
            st.groups = new_groups;
        }
        changed
    })
}

/// Snapshot of the current groups, sorted by increasing `first_seen`. UI thread.
pub fn groups() -> Vec<AppGroup> {
    STATE.with(|s| s.borrow().groups.clone())
}

/// Ensures that `cache[h]` exists and is up to date (according to the re-evaluation rules).
/// `None` if the window is gone or the process cannot be opened (the entry is dropped).
/// `force` re-resolves an existing entry regardless of the schedule.
fn ensure_entry(h: isize, force: bool) -> Option<()> {
    let pid_now = window_pid(hw(h));
    if pid_now == 0 {
        STATE.with(|s| s.borrow_mut().cache.remove(&h));
        return None;
    }
    let needs = STATE.with(|s| match s.borrow().cache.get(&h) {
        Some(e) if e.pid == pid_now => {
            if e.afh_unresolved {
                force || recheck_due(&AFH_RECHECK, e)
            } else if !e.win_aumid_found {
                force || recheck_due(&AUMID_RECHECK, e)
            } else {
                false
            }
        }
        _ => true,
    });
    if !needs {
        return Some(());
    }
    // The expensive calls are outside any borrow.
    let Some(r) = resolve_identity(hw(h), pid_now) else {
        STATE.with(|s| s.borrow_mut().cache.remove(&h));
        return None;
    };
    STATE.with(|s| {
        let st = &mut *s.borrow_mut();
        let (seq, stamp, first, checks) = match st.cache.get(&h) {
            Some(e) if e.pid == pid_now => (e.seq, e.stamp, e.first, e.checks.saturating_add(1)),
            _ => {
                st.next_win_seq += 1;
                st.stamp_counter += 1;
                (st.next_win_seq, st.stamp_counter, Instant::now(), 0)
            }
        };
        st.cache.insert(
            h,
            Entry {
                ident: r.ident,
                pid: pid_now,
                seq,
                stamp,
                win_aumid_found: r.win_aumid_found,
                afh_unresolved: r.afh_unresolved,
                first,
                checks,
            },
        );
    });
    Some(())
}

/// True when the next re-check of `e` is due: follows `sched` (offsets from the first resolution),
/// then repeats every last interval for as long as the window lives.
fn recheck_due(sched: &[Duration], e: &Entry) -> bool {
    let Some(&last) = sched.last() else {
        return false;
    };
    let n = e.checks as usize;
    let due = match sched.get(n) {
        Some(&d) => d,
        None => {
            let extra = u32::try_from(n + 1 - sched.len()).unwrap_or(u32::MAX);
            last.saturating_add(last.saturating_mul(extra))
        }
    };
    e.first.elapsed() >= due
}

struct Resolved {
    /// The window process's own exe, before the helper -> root promotion (diagnostics).
    own_exe: String,
    ident: WindowIdentity,
    win_aumid_found: bool,
    afh_unresolved: bool,
}

fn window_pid(h: HWND) -> u32 {
    let mut pid = 0u32;
    // SAFETY: arbitrary HWND allowed; `pid` is a valid local u32 for writing.
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    pid
}

/// Full identity resolution (without cache).
fn resolve_identity(hwnd: HWND, pid: u32) -> Option<Resolved> {
    let proc = OwnedProcess::open(pid)?;
    let exe_path = image_path(&proc)?;
    let win_aumid = window_aumid(hwnd);
    let is_afh = is_application_frame_host(&exe_path);

    // The "real" process of the app: for a UWP frame it is the one of the child CoreWindow.
    let mut pkg_aumid: Option<String> = None;
    let mut real_exe: Option<String> = None;
    if is_afh {
        if let Some(core) = find_core_window(hwnd) {
            let core_pid = window_pid(core);
            if core_pid != 0 && core_pid != pid {
                if let Some(p2) = OwnedProcess::open(core_pid) {
                    real_exe = image_path(&p2);
                    pkg_aumid = package_aumid(&p2);
                }
            }
        }
    } else {
        pkg_aumid = package_aumid(&proc);
    }

    // Helper process (e.g. steamwebhelper.exe) -> root of its install tree (steam.exe). Only for plain
    // Win32 apps: windows with an AUMID are already grouped and named through it.
    let own_exe = real_exe.clone().unwrap_or_else(|| exe_path.clone());
    let root_exe = if pkg_aumid.is_none() && !is_afh {
        Some(ROOT_CACHE.with(|c| c.borrow_mut().root_exe_of(pid, &proc, &exe_path)))
            .filter(|r| !r.eq_ignore_ascii_case(&exe_path))
    } else {
        None
    };
    let win_aumid_found = win_aumid.is_some();
    // A promoted helper window drops its AUMID (e.g. Steam's "Valve.Steam.Client") so that it groups,
    // is named and gets its icon from the root exe, like a pinned steam.exe.
    let aumid = if root_exe.is_some() { None } else { win_aumid.or(pkg_aumid) };
    let exe_path = root_exe.unwrap_or(exe_path);
    let (key, afh_unresolved) = match (&aumid, is_afh) {
        (Some(a), _) => (a.clone(), false),
        (None, true) => (format!("hwnd:{}", hwnd.0 as isize), true),
        (None, false) => (key_from_exe_path(&exe_path), false),
    };
    Some(Resolved {
        own_exe,
        ident: WindowIdentity { pid, exe_path: real_exe.unwrap_or(exe_path), aumid, key },
        win_aumid_found,
        afh_unresolved,
    })
}

// ------------------------------------------------------------------ helper -> root process

#[link(name = "ntdll")]
extern "system" {
    fn NtQueryInformationProcess(process: HANDLE, class: u32, info: *mut c_void, len: u32, ret_len: *mut u32) -> i32;
}

thread_local! {
    /// pid -> (creation time of the pid, root exe), kept by the shell model. The cache stays on this thread and
    /// outside every `STATE` borrow: the walk up the parents is the expensive call.
    static ROOT_CACHE: RefCell<RootCache> = RefCell::new(RootCache::default());
}

/// `ProcessCommandLineInformation`: a UNICODE_STRING whose text follows it in the caller's buffer.
const PROCESS_COMMAND_LINE_INFORMATION: u32 = 60;
/// The buffer `command_line` reads into, in bytes: a longer command line is not read.
const COMMAND_LINE_MAX: usize = 8192;

/// The command line of the process `proc` (a handle with PROCESS_QUERY_LIMITED_INFORMATION), or None when it cannot be
/// read or does not fit `COMMAND_LINE_MAX` bytes. Another process wrote it: the text is taken only from inside the
/// bytes the call filled (`text_span`), never through the returned pointer.
pub(crate) fn command_line(proc: HANDLE) -> Option<String> {
    /// `UNICODE_STRING`.
    #[repr(C)]
    struct UnicodeString {
        length: u16,
        maximum_length: u16,
        buffer: *const u16,
    }
    // u64 words: the buffer is aligned for the header.
    let mut buf = vec![0u64; COMMAND_LINE_MAX / 8];
    let cap = buf.len() * 8;
    let mut filled = 0u32;
    // SAFETY: `buf` has `cap` writable bytes; `proc` is a handle with PROCESS_QUERY_LIMITED_INFORMATION, enough for
    // this class; `filled` receives the bytes written.
    let st = unsafe {
        NtQueryInformationProcess(
            proc,
            PROCESS_COMMAND_LINE_INFORMATION,
            buf.as_mut_ptr().cast(),
            cap as u32,
            &mut filled,
        )
    };
    if st < 0 {
        return None; // a command line longer than the buffer too
    }
    // SAFETY: `buf` is 8-aligned and longer than the header, which the call wrote; any bytes read as integers and a
    // pointer that is only compared, never followed.
    let head = unsafe { std::ptr::read(buf.as_ptr().cast::<UnicodeString>()) };
    let offset = (head.buffer as usize).checked_sub(buf.as_ptr() as usize)?;
    let filled = (filled as usize).min(cap);
    let span = text_span(offset, usize::from(head.length), std::mem::size_of::<UnicodeString>(), filled)?;
    // SAFETY: the same `cap` bytes of `buf`, read as u16 (cap is even, the alignment 8).
    let words = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u16>(), cap / 2) };
    Some(String::from_utf16_lossy(words.get(span)?))
}

/// The u16 range of a command line's text in `command_line`'s buffer, from the byte `offset` and `len` the call gave:
/// after the `header`, even, and inside the `filled` bytes; None otherwise. Pure.
fn text_span(offset: usize, len: usize, header: usize, filled: usize) -> Option<std::ops::Range<usize>> {
    let end = offset.checked_add(len)?;
    (offset >= header && offset.is_multiple_of(2) && len.is_multiple_of(2) && end <= filled)
        .then_some(offset / 2..end / 2)
}

// ------------------------------------------------------------------ diagnostics (--list-apps)

/// One top-level app window with its resolved identity (for `--list-apps`).
pub struct ListedWindow {
    pub title: String,
    /// Exe of the process owning the window (before helper -> root promotion).
    pub own_exe: String,
    pub ident: WindowIdentity,
}

/// Enumerates the current top-level app windows with the same filters and identity resolution as the
/// dock. Needs COM on the calling thread. Touches nothing (no hooks, no taskbar).
pub fn list_windows() -> Vec<ListedWindow> {
    use windows::Win32::UI::WindowsAndMessaging::GetWindowTextW;
    let mut out = Vec::new();
    for h in enum_taskbar_windows(HWND::default()) {
        let pid = window_pid(hw(h));
        if pid == 0 {
            continue;
        }
        let Some(r) = resolve_identity(hw(h), pid) else { continue };
        let mut buf = [0u16; 128];
        // SAFETY: `buf` is a writable buffer of the declared length; arbitrary HWND allowed.
        let n = unsafe { GetWindowTextW(hw(h), &mut buf) }.max(0) as usize;
        let title = String::from_utf16_lossy(&buf[..n.min(buf.len())]);
        out.push(ListedWindow { title, own_exe: r.own_exe, ident: r.ident });
    }
    out
}

/// AUMID published by the window (`PKEY_AppUserModel_ID`) through `SHGetPropertyStoreForWindow`.
/// Requires COM initialized on the calling thread.
fn window_aumid(hwnd: HWND) -> Option<String> {
    // SAFETY: arbitrary HWND allowed (fails if the window does not exist). The PROPVARIANT returned
    // by GetValue is owned by us: it must be freed with PropVariantClear (the type has no Drop).
    // The string allocated by PropVariantToStringAlloc must be freed with CoTaskMemFree.
    unsafe {
        let store: IPropertyStore = match SHGetPropertyStoreForWindow(hwnd) {
            Ok(s) => s,
            Err(e) => {
                use std::sync::atomic::{AtomicBool, Ordering};
                static WARNED: AtomicBool = AtomicBool::new(false);
                // CO_E_NOTINITIALIZED = 0x800401F0: report only once, it is a setup error.
                if e.code().0 as u32 == 0x800401F0 && !WARNED.swap(true, Ordering::Relaxed) {
                    log!("apps: SHGetPropertyStoreForWindow: COM not initialized on the UI thread");
                }
                return None;
            }
        };
        let mut pv = store.GetValue(&PKEY_APP_USER_MODEL_ID).ok()?;
        let out = match PropVariantToStringAlloc(&pv) {
            Ok(p) => {
                let s = p.to_string().ok();
                CoTaskMemFree(Some(p.0 as *const c_void));
                s
            }
            Err(_) => None,
        };
        let _ = PropVariantClear(&mut pv);
        out.filter(|s| !s.is_empty())
    }
}

/// AppUserModelID the window `h` published itself, if any (the id its jump list is stored under,
/// also for a helper window grouped under its root exe). UI thread.
pub fn window_app_id(h: isize) -> Option<String> {
    window_aumid(hw(h))
}

/// Child window `Windows.UI.Core.CoreWindow` of an `ApplicationFrameHost` frame.
fn find_core_window(frame: HWND) -> Option<HWND> {
    // SAFETY: arbitrary HWND allowed; the strings are NUL-terminated literals ('static).
    unsafe { FindWindowExW(Some(frame), None, w!("Windows.UI.Core.CoreWindow"), PCWSTR::null()) }
        .ok()
        .filter(|h| !h.0.is_null())
}

// ------------------------------------------------------------------ foreground / MRU

/// Which of the tracked windows `live` matches the foreground window? The foreground
/// may be a popup owned by a tracked window (we walk up to the root owner).
/// 0 if the foreground window is not tracked (desktop, Start menu, the dock itself...).
fn foreground_target(live: &[isize]) -> isize {
    // SAFETY: no preconditions.
    foreground_target_of(unsafe { GetForegroundWindow() }, live)
}

/// `foreground_target` for the foreground `fg` (e.g. `ui::effective_foreground`).
fn foreground_target_of(fg: HWND, live: &[isize]) -> isize {
    // SAFETY: GetAncestor accepts arbitrary HWNDs.
    let (fg, root_owner, root) = unsafe {
        if fg.0.is_null() {
            return 0;
        }
        (fg, GetAncestor(fg, GA_ROOTOWNER), GetAncestor(fg, GA_ROOT))
    };
    for cand in [fg, root_owner, root] {
        let c = cand.0 as isize;
        if c != 0 && live.contains(&c) {
            return c;
        }
    }
    0
}

/// true if `hwnd` is the foreground window (or the root owner of the foreground window,
/// for owned dialogs/popups).
pub fn is_foreground(hwnd: isize) -> bool {
    if hwnd == 0 {
        return false;
    }
    foreground_target(&[hwnd]) == hwnd
}

/// Records `h` as activated now (MRU) and as the last foreground window.
fn touch(h: isize) {
    STATE.with(|s| {
        let st = &mut *s.borrow_mut();
        st.stamp_counter += 1;
        let c = st.stamp_counter;
        if let Some(e) = st.cache.get_mut(&h) {
            e.stamp = c;
            st.last_fg = h;
        }
    });
}

// ------------------------------------------------------------------ window actions

/// Left click on a group: no windows -> `NoWindows`; one window: if foreground it
/// is minimized (`Minimized`), otherwise it is activated (`Activated`, `SW_RESTORE` if minimized, then
/// `AllowSetForegroundWindow` + `SetForegroundWindow`/`SwitchToThisWindow`); multiple windows: cycles
/// through them at every click (simple MRU). UI thread.
pub fn click_group(key: &str) -> ClickOutcome {
    // Windows of the group still alive, in MRU order, with their `seq` (stable order).
    let wins: Vec<(isize, u64)> = STATE.with(|s| {
        let st = s.borrow();
        st.groups
            .iter()
            .find(|g| g.key == key)
            .map(|g| g.windows.iter().map(|&h| (h, st.cache.get(&h).map(|e| e.seq).unwrap_or(0))).collect())
            .unwrap_or_default()
    });
    // SAFETY: IsWindow accepts arbitrary HWNDs.
    let wins: Vec<(isize, u64)> =
        wins.into_iter().filter(|&(h, _)| unsafe { IsWindow(Some(hw(h))) }.as_bool()).collect();
    if wins.is_empty() {
        return ClickOutcome::NoWindows;
    }

    let handles: Vec<isize> = wins.iter().map(|w| w.0).collect();
    // 0 if no window of the group is in the foreground (the bar focused by Win counts as the window before).
    let fg = foreground_target_of(crate::ui::effective_foreground(), &handles);

    if fg == 0 {
        // Inactive group: the most recent one.
        let target = wins[0].0;
        activate_window(target);
        return ClickOutcome::Activated(target);
    }
    if wins.len() == 1 {
        minimize_window(fg);
        return ClickOutcome::Minimized(fg);
    }
    // Active group with multiple windows: move to the next one in stable order of appearance.
    let mut by_seq = wins.clone();
    crate::tinysort::sort_by_key(&mut by_seq, |w| w.1);
    let pos = by_seq.iter().position(|w| w.0 == fg).unwrap_or(0);
    let target = by_seq[(pos + 1) % by_seq.len()].0;
    activate_window(target);
    ClickOutcome::Activated(target)
}

/// The window the wheel cycle moves to. `wins` = (hwnd, seq, minimized) of a group in MRU order, `fg` the group's window
/// that has the foreground (0 = none). Candidates are the windows that are not minimized when `skip_min`, else all. No
/// candidate: the most recent window of the group (it is restored). The foreground among the candidates: the next (or
/// the previous) one by `seq`, wrapping; None when that is the foreground itself (a single candidate). The foreground
/// outside the candidates: the most recent candidate.
pub(crate) fn cycle_target(wins: &[(isize, u64, bool)], fg: isize, forward: bool, skip_min: bool) -> Option<isize> {
    let mut cands: Vec<(isize, u64)> = wins.iter().filter(|w| !(skip_min && w.2)).map(|w| (w.0, w.1)).collect();
    let Some(first) = cands.first().map(|c| c.0) else {
        return wins.first().map(|w| w.0);
    };
    let Some(pos) = cands.iter().position(|c| c.0 == fg) else { return Some(first) };
    crate::tinysort::sort_by_key(&mut cands, |c| c.1);
    let pos = cands.iter().position(|c| c.0 == fg).unwrap_or(pos);
    let n = cands.len();
    let next = if forward { (pos + 1) % n } else { (pos + n - 1) % n };
    cands.get(next).map(|c| c.0).filter(|&h| h != fg)
}

/// Wheel cycle: activates the window of group `key` that `cycle_target` picks and returns it. UI thread.
pub fn cycle_group(key: &str, forward: bool, skip_min: bool) -> Option<isize> {
    // Windows of the group still alive, in MRU order, with their `seq`.
    let wins: Vec<(isize, u64)> = STATE.with(|s| {
        let st = s.borrow();
        st.groups
            .iter()
            .find(|g| g.key == key)
            .map(|g| g.windows.iter().map(|&h| (h, st.cache.get(&h).map(|e| e.seq).unwrap_or(0))).collect())
            .unwrap_or_default()
    });
    // SAFETY: IsWindow and IsIconic accept arbitrary HWNDs.
    let wins: Vec<(isize, u64, bool)> = wins
        .into_iter()
        .filter(|&(h, _)| unsafe { IsWindow(Some(hw(h))) }.as_bool())
        .map(|(h, seq)| (h, seq, unsafe { IsIconic(hw(h)) }.as_bool()))
        .collect();
    let handles: Vec<isize> = wins.iter().map(|w| w.0).collect();
    let fg = foreground_target_of(crate::ui::effective_foreground(), &handles);
    let target = cycle_target(&wins, fg, forward, skip_min)?;
    activate_window(target);
    Some(target)
}

/// Windows of group `key` in stable order of appearance (`seq`), for the window picker. UI thread.
pub fn windows_in_order(key: &str) -> Vec<isize> {
    STATE.with(|s| {
        let st = s.borrow();
        let mut v: Vec<(isize, u64)> = st
            .groups
            .iter()
            .find(|g| g.key == key)
            .map(|g| g.windows.iter().map(|&h| (h, st.cache.get(&h).map(|e| e.seq).unwrap_or(0))).collect())
            .unwrap_or_default();
        crate::tinysort::sort_by_key(&mut v, |w| w.1);
        v.into_iter().map(|w| w.0).collect()
    })
}

/// Sends `WM_CLOSE` (PostMessageW) to all windows of the group. UI thread.
pub fn close_group(key: &str) {
    let wins: Vec<isize> =
        STATE.with(|s| s.borrow().groups.iter().find(|g| g.key == key).map(|g| g.windows.clone()).unwrap_or_default());
    log!("apps::close_group: {} windows", wins.len());
    for h in wins {
        // SAFETY: PostMessageW accepts arbitrary HWNDs; it does not block and fails if the window is gone.
        let _ = unsafe { PostMessageW(Some(hw(h)), WM_CLOSE, WPARAM(0), LPARAM(0)) };
    }
}

/// Sends `WM_SYSCOMMAND` with a timeout (does not block the UI on hung apps). `true` if delivered. A window whose
/// thread does not answer the ping (`ui::answers`, 50 ms at most) is not sent it: the caller's ShowWindowAsync does it.
/// SMTO_BLOCK keeps the 300 ms a true bound for an app that stops answering after the ping: without it the UI thread
/// dispatches the messages other processes send the dock while it waits, and each one starts the timeout again (live
/// check V-14). A send that times out is reported as not delivered, so the caller's ShowWindowAsync acts. SMTO_BLOCK
/// does not serve the popup's low-level mouse hook either, which Windows calls through a message to this thread: the
/// hook is removed for the wait (`popup::suspend_mouse_hook`), or every mouse event would wait for it.
fn sys_command(h: isize, cmd: u32) -> bool {
    if !crate::ui::answers(hw(h)) {
        return false;
    }
    let mut res = 0usize;
    let hook = crate::popup::suspend_mouse_hook();
    // SAFETY: arbitrary HWND allowed; `res` is valid for writing; SMTO_BLOCK, SMTO_ABORTIFHUNG and
    // a 300 ms timeout bound the wait if the target thread does not respond.
    let r = unsafe {
        SendMessageTimeoutW(
            hw(h),
            WM_SYSCOMMAND,
            WPARAM(cmd as usize),
            LPARAM(0),
            SMTO_BLOCK | SMTO_ABORTIFHUNG,
            SYSCMD_TIMEOUT_MS,
            Some(&mut res),
        )
    };
    crate::popup::resume_mouse_hook(hook);
    r.0 != 0
}

fn minimize_window(h: isize) {
    if !sys_command(h, SC_MINIMIZE) {
        // SAFETY: ShowWindowAsync does not block (it queues the command to the owning thread).
        let _ = unsafe { ShowWindowAsync(hw(h), SW_MINIMIZE) };
    }
    log!("apps: minimize {h:#x}");
}

/// Brings `h` to the foreground; if minimized, restores it. Updates the MRU. UI thread (also used by
/// the window picker).
pub(crate) fn activate_window(h: isize) {
    let w = hw(h);
    // SAFETY: all the APIs accept arbitrary HWNDs and fail without UB if the window disappears.
    unsafe {
        if IsIconic(w).as_bool() && !sys_command(h, SC_RESTORE) {
            let _ = ShowWindowAsync(w, SW_RESTORE);
        }
        // Allows any process (including the target app) to take the foreground.
        let _ = AllowSetForegroundWindow(ASFW_ANY);
        let ok = SetForegroundWindow(w).as_bool();
        // Posted to the app's thread: raised in place, a hung app would hold the UI thread.
        crate::ui::raise(w);
        if !ok || !is_foreground(h) {
            // Foreground lock: `SwitchToThisWindow` bypasses it and is
            // the same call used by Alt+Tab; it also restores if still minimized.
            SwitchToThisWindow(w, true);
        }
    }
    touch(h);
    log!("apps: activate {h:#x}");
}

// ------------------------------------------------------------------ filters

struct Ctx {
    own: HWND,
    out: Vec<isize>,
}

/// List of the HWNDs that pass `is_taskbar_window`, in Z order (top first).
pub fn enum_taskbar_windows(own: HWND) -> Vec<isize> {
    let mut ctx = Ctx { own, out: Vec::new() };
    // SAFETY: `ctx` lives until EnumWindows returns (synchronous, same thread); the callback
    // receives it through LPARAM and uses it only during the call.
    let _ = unsafe { EnumWindows(Some(enum_cb), LPARAM(&mut ctx as *mut Ctx as isize)) };
    ctx.out
}

unsafe extern "system" fn enum_cb(h: HWND, lp: LPARAM) -> BOOL {
    // SAFETY: lp is the `*mut Ctx` passed by `enum_taskbar_windows`, valid during the enumeration.
    let ctx = unsafe { &mut *(lp.0 as *mut Ctx) };
    if is_taskbar_window(h, ctx.own) {
        ctx.out.push(h.0 as isize);
    }
    BOOL(1)
}

/// Window filter: visible, not toolwindow, owner discarded except with
/// APPWINDOW, not cloaked, Progman/WorkerW classes discarded, dock window discarded, and
/// `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` must succeed.
/// In addition (ideas only, no code copied, from lightbar/ManagedShell/Seelen): discarding
/// `WS_EX_NOACTIVATE` windows without APPWINDOW, those marked `ITaskList_Deleted`, shell
/// top-level `CoreWindow`s (Start, search...) and Edge's `TabProxyWindow`s.
pub fn is_taskbar_window(h: HWND, own: HWND) -> bool {
    if h == own {
        return false;
    }
    // SAFETY: all the APIs below accept arbitrary HWNDs and fail without UB if the window disappears.
    unsafe {
        // Visible, not a tool window, no NOACTIVATE or owned window without APPWINDOW (like Alt+Tab).
        if !could_be_listed(styles_of(h)) {
            return false;
        }
        let mut cloaked: u32 = 0;
        if DwmGetWindowAttribute(
            h,
            DWMWA_CLOAKED,
            (&mut cloaked as *mut u32).cast::<c_void>(),
            std::mem::size_of::<u32>() as u32,
        )
        .is_ok()
            && cloaked != 0
        {
            return false;
        }
        let mut cls = [0u16; 64];
        let n = GetClassNameW(h, &mut cls).max(0) as usize;
        let class = String::from_utf16_lossy(&cls[..n.min(cls.len())]);
        if excluded_class(&class) {
            return false;
        }
        // Property set by Explorer on windows removed from the taskbar.
        if !GetPropW(h, w!("ITaskList_Deleted")).0.is_null() {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(p) => {
                let _ = CloseHandle(p);
            }
            Err(_) => return false,
        }
    }
    true
}

/// Window classes that are never bar apps: the desktop, shell `CoreWindow`s, Edge's tab proxies and
/// the dock's own Settings window (a dock window, not an app).
fn excluded_class(class: &str) -> bool {
    matches!(
        class,
        "Progman"
            | "WorkerW"
            | "Windows.UI.Core.CoreWindow"
            | "Windows.Internal.Shell.TabProxyWindow"
            | crate::settings_window::CLASS
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycle_target_table() {
        // (hwnd, seq, minimized) in MRU order: 30 is the most recent, 10 the oldest by appearance.
        let wins = [(30, 3, false), (10, 1, false), (20, 2, false)];
        assert_eq!(cycle_target(&wins, 30, true, true), Some(10), "forward wraps to the first by seq");
        assert_eq!(cycle_target(&wins, 10, true, true), Some(20));
        assert_eq!(cycle_target(&wins, 20, true, true), Some(30));
        assert_eq!(cycle_target(&wins, 10, false, true), Some(30), "back wraps to the last by seq");
        assert_eq!(cycle_target(&wins, 30, false, true), Some(20));
        // A minimized window is skipped.
        let wins = [(30, 3, false), (20, 2, true), (10, 1, false)];
        assert_eq!(cycle_target(&wins, 10, true, true), Some(30));
        assert_eq!(cycle_target(&wins, 30, false, true), Some(10));
        assert_eq!(cycle_target(&wins, 10, true, false), Some(20), "not skipped when skip_min is off");
        // All minimized: the most recent one is restored.
        let wins = [(20, 2, true), (10, 1, true)];
        assert_eq!(cycle_target(&wins, 0, true, true), Some(20));
        assert_eq!(cycle_target(&wins, 0, false, true), Some(20));
        // The foreground outside the group (or hidden by the skip): the most recent candidate.
        let wins = [(30, 3, false), (20, 2, true), (10, 1, false)];
        assert_eq!(cycle_target(&wins, 0, true, true), Some(30));
        assert_eq!(cycle_target(&wins, 99, false, true), Some(30));
        assert_eq!(cycle_target(&wins, 20, true, true), Some(30));
        // One window that has the foreground: nothing to move to. No windows: nothing.
        assert_eq!(cycle_target(&[(10, 1, false)], 10, true, true), None);
        assert_eq!(cycle_target(&[], 0, true, true), None);
    }

    fn plain() -> Styles {
        Styles { visible: true, tool: false, noactivate: false, app: false, owned: false }
    }

    /// 1.5.2 T3b: another process's command line is read only from inside the bytes the call filled, after the header.
    #[test]
    fn a_command_line_is_read_only_inside_the_buffer() {
        assert_eq!(text_span(16, 10, 16, 64), Some(8..13));
        assert_eq!(text_span(16, 0, 16, 16), Some(8..8), "an empty line");
        assert_eq!(text_span(8, 10, 16, 64), None, "over the header");
        assert_eq!(text_span(17, 10, 16, 64), None, "odd offset");
        assert_eq!(text_span(16, 11, 16, 64), None, "odd length");
        assert_eq!(text_span(16, 50, 16, 64), None, "past the filled bytes");
        assert_eq!(text_span(usize::MAX - 1, 4, 16, usize::MAX), None, "overflow");
        // This test's own process: the real call, read the same way.
        // SAFETY: the pseudo-handle of the current process, with full access.
        let own = command_line(unsafe { windows::Win32::System::Threading::GetCurrentProcess() });
        assert!(own.is_some_and(|c| c.to_ascii_lowercase().contains("dock")), "the test exe's own command line");
    }

    #[test]
    fn could_be_listed_mirrors_the_taskbar_filter() {
        assert!(could_be_listed(plain()));
        assert!(!could_be_listed(Styles { tool: true, ..plain() }));
        assert!(!could_be_listed(Styles { tool: true, app: true, ..plain() }));
        assert!(!could_be_listed(Styles { noactivate: true, ..plain() }));
        assert!(could_be_listed(Styles { noactivate: true, app: true, ..plain() }));
        assert!(!could_be_listed(Styles { owned: true, ..plain() }));
        assert!(could_be_listed(Styles { owned: true, app: true, ..plain() }));
        assert!(!could_be_listed(Styles { visible: false, ..plain() }));
    }

    const EVENTS: [u32; 7] = [
        EVENT_OBJECT_CREATE,
        EVENT_OBJECT_SHOW,
        EVENT_OBJECT_HIDE,
        EVENT_OBJECT_DESTROY,
        EVENT_OBJECT_CLOAKED,
        EVENT_OBJECT_UNCLOAKED,
        EVENT_SYSTEM_FOREGROUND,
    ];

    #[test]
    fn tool_window_events_never_post() {
        let tool = || Styles { tool: true, ..plain() };
        for e in [EVENT_OBJECT_CREATE, EVENT_OBJECT_SHOW, EVENT_OBJECT_HIDE, EVENT_OBJECT_DESTROY] {
            assert!(!post_wanted(e, Some(false), tool), "event {e:#x}");
        }
    }

    #[test]
    fn an_unlisted_destroy_never_posts() {
        for e in [EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE, EVENT_OBJECT_CLOAKED] {
            assert!(!post_wanted(e, Some(false), || panic!("styles are not read for {e:#x}")));
        }
    }

    #[test]
    fn a_listed_window_always_posts() {
        for e in EVENTS {
            assert!(post_wanted(e, Some(true), || panic!("styles are not read for a listed window")));
        }
    }

    #[test]
    fn a_shown_candidate_posts() {
        for e in [EVENT_OBJECT_CREATE, EVENT_OBJECT_SHOW, EVENT_OBJECT_UNCLOAKED] {
            assert!(post_wanted(e, Some(false), plain));
            assert!(!post_wanted(e, Some(false), || Styles { visible: false, ..plain() }));
        }
    }

    #[test]
    fn foreground_always_posts() {
        for l in [None, Some(false), Some(true)] {
            assert!(post_wanted(EVENT_SYSTEM_FOREGROUND, l, || panic!("styles are not read for FOREGROUND")));
        }
    }

    #[test]
    fn an_unknown_listing_posts() {
        for e in EVENTS {
            assert!(post_wanted(e, None, || panic!("styles are not read when the listing is unknown")));
        }
    }

    #[test]
    fn settings_window_is_not_an_app() {
        assert!(excluded_class("DockSettings") && excluded_class("Progman") && excluded_class("WorkerW"));
        assert!(!excluded_class("Notepad"));
    }
}
