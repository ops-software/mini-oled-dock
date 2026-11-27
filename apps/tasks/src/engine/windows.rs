//! Top-level windows that count as apps, and the hung check.
//!
//! The filter is a read-only reimplementation of the dock's taskbar filter; nothing here sends a message to a
//! window. A full enumeration runs when the dirty flag is set or every RECONCILE_MS; ticks in between reuse it.
use crate::engine::frame::{Frame, Sampler, WindowFact};
use crate::engine::need::Need;
use oled_sysmon::SysmonError;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use windows::core::{w, BOOL, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowExW, GetClassNameW, GetPropW, GetWindow, GetWindowLongW, GetWindowThreadProcessId,
    IsHungAppWindow, IsWindowVisible, GET_WINDOW_CMD, GWL_EXSTYLE, WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};

pub const RECONCILE_MS: u32 = 5000;
pub fn is_excluded_class(class: &str) -> bool {
    EXCLUDED_CLASSES.contains(&class)
}

pub const EXCLUDED_CLASSES: [&str; 4] =
    ["Progman", "WorkerW", "Windows.UI.Core.CoreWindow", "Windows.Internal.Shell.TabProxyWindow"];

const GW_OWNER: GET_WINDOW_CMD = GET_WINDOW_CMD(4);
const FRAME_CLASS: &str = "ApplicationFrameWindow";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Styles {
    pub visible: bool,
    pub tool: bool,
    pub noactivate: bool,
    pub app: bool,
    pub owned: bool,
    pub cloaked: bool,
    pub taskbar_deleted: bool,
}

/// The dock's taskbar filter: visible, not a tool window, no NOACTIVATE or owned window without APPWINDOW,
/// not cloaked, not removed from the taskbar by the shell, not an excluded class.
pub fn qualifies(s: Styles, excluded_class: bool) -> bool {
    s.visible
        && !s.tool
        && (!s.noactivate || s.app)
        && (!s.owned || s.app)
        && !s.cloaked
        && !s.taskbar_deleted
        && !excluded_class
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawWindow {
    pub hwnd: isize,
    pub pid: u32,
    pub styles: Styles,
    pub excluded_class: bool,
    pub core_pid: Option<u32>,
}

pub trait WindowSource: Send {
    fn enumerate(&mut self, out: &mut Vec<RawWindow>);
    fn hung(&mut self, hwnd: isize) -> bool;
}

pub struct Win32Windows;

struct EnumCtx<'a> {
    out: &'a mut Vec<RawWindow>,
    own_pid: u32,
}

fn hwnd_of(h: isize) -> HWND {
    HWND(h as *mut c_void)
}

fn class_of(h: HWND) -> String {
    let mut cls = [0u16; 64];
    // SAFETY: any HWND is accepted; the buffer is a local array.
    let n = unsafe { GetClassNameW(h, &mut cls) }.max(0) as usize;
    String::from_utf16_lossy(cls.get(..n).unwrap_or(&[]))
}

fn read_window(h: HWND, own_pid: u32) -> Option<RawWindow> {
    let mut pid = 0u32;
    // SAFETY: every API below accepts an arbitrary HWND and fails without UB if the window disappears; no
    // message is sent to the window.
    unsafe {
        GetWindowThreadProcessId(h, Some(&mut pid));
        if pid == 0 || pid == own_pid {
            return None;
        }
        let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
        let visible = IsWindowVisible(h).as_bool();
        let mut cloaked: u32 = 0;
        let cloaked = visible
            && DwmGetWindowAttribute(
                h,
                DWMWA_CLOAKED,
                (&mut cloaked as *mut u32).cast::<c_void>(),
                std::mem::size_of::<u32>() as u32,
            )
            .is_ok()
            && cloaked != 0;
        let styles = Styles {
            visible,
            tool: ex & WS_EX_TOOLWINDOW.0 != 0,
            noactivate: ex & WS_EX_NOACTIVATE.0 != 0,
            app: ex & WS_EX_APPWINDOW.0 != 0,
            owned: !GetWindow(h, GW_OWNER).unwrap_or_default().0.is_null(),
            cloaked,
            taskbar_deleted: !GetPropW(h, w!("ITaskList_Deleted")).0.is_null(),
        };
        let class = class_of(h);
        let excluded_class = is_excluded_class(&class);
        let core_pid = if class == FRAME_CLASS {
            FindWindowExW(Some(h), None, w!("Windows.UI.Core.CoreWindow"), PCWSTR::null())
                .ok()
                .filter(|c| !c.0.is_null())
                .and_then(|c| {
                    let mut p = 0u32;
                    GetWindowThreadProcessId(c, Some(&mut p));
                    (p != 0).then_some(p)
                })
        } else {
            None
        };
        Some(RawWindow { hwnd: h.0 as isize, pid, styles, excluded_class, core_pid })
    }
}

unsafe extern "system" fn enum_cb(h: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: lparam is the address of the EnumCtx that Win32Windows::enumerate keeps alive until EnumWindows
    // returns; the callback runs on the calling thread.
    let ctx = unsafe { &mut *(lparam.0 as *mut EnumCtx) };
    if let Some(r) = read_window(h, ctx.own_pid) {
        ctx.out.push(r);
    }
    BOOL(1)
}

impl WindowSource for Win32Windows {
    fn enumerate(&mut self, out: &mut Vec<RawWindow>) {
        out.clear();
        // SAFETY: no arguments.
        let own_pid = unsafe { GetCurrentProcessId() };
        let mut ctx = EnumCtx { out, own_pid };
        // SAFETY: ctx lives until EnumWindows returns (synchronous, same thread); enum_cb only reads it back.
        let _ = unsafe { EnumWindows(Some(enum_cb), LPARAM(&mut ctx as *mut EnumCtx as isize)) };
    }
    fn hung(&mut self, hwnd: isize) -> bool {
        // SAFETY: accepts an arbitrary HWND.
        unsafe { IsHungAppWindow(hwnd_of(hwnd)) }.as_bool()
    }
}

pub struct WindowSampler<S: WindowSource> {
    src: S,
    raw: Vec<RawWindow>,
    /// u64::MAX until the first full enumeration.
    last_full_us: u64,
    dirty: Arc<AtomicBool>,
    last: Option<Vec<WindowFact>>,
}

impl<S: WindowSource> WindowSampler<S> {
    pub fn new(src: S) -> Self {
        WindowSampler {
            src,
            raw: Vec::new(),
            last_full_us: u64::MAX,
            dirty: Arc::new(AtomicBool::new(false)),
            last: None,
        }
    }
    /// Set by the owner of the window hooks; the next tick enumerates in full.
    pub fn dirty_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.dirty)
    }
}

/// need() = Windows; hung checks when out.need has Hung.
impl<S: WindowSource> Sampler for WindowSampler<S> {
    fn need(&self) -> Need {
        Need::Windows
    }
    fn sample(&mut self, now_us: u64, out: &mut Frame) -> Result<(), SysmonError> {
        let due =
            self.last_full_us == u64::MAX || now_us.saturating_sub(self.last_full_us) >= u64::from(RECONCILE_MS) * 1000;
        if self.dirty.swap(false, Ordering::AcqRel) || due {
            self.src.enumerate(&mut self.raw);
            self.last_full_us = now_us;
        }
        let want_hung = out.need.has(Need::Hung);
        let mut list = std::mem::take(&mut out.windows);
        list.clear();
        for r in &self.raw {
            if !qualifies(r.styles, r.excluded_class) {
                continue;
            }
            let Some(row) = out.procs.find_pid(r.core_pid.unwrap_or(r.pid)) else {
                continue;
            };
            let hung = want_hung && self.src.hung(r.hwnd);
            list.push(WindowFact { owner: row.key, hwnd: r.hwnd, hung });
        }
        out.windows_fresh = self.last.as_deref() != Some(list.as_slice());
        if out.windows_fresh {
            self.last = Some(list.clone());
        }
        out.windows = list;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::need::NeedSet;
    use oled_sysmon::{ProcKey, ProcRow, ProcTable};

    fn plain() -> Styles {
        Styles { visible: true, ..Styles::default() }
    }

    #[test]
    fn filter_mirrors_the_dock() {
        assert!(qualifies(plain(), false));
        assert!(!qualifies(Styles { visible: false, ..plain() }, false));
        assert!(!qualifies(Styles { tool: true, ..plain() }, false));
        assert!(!qualifies(Styles { tool: true, app: true, ..plain() }, false));
        assert!(!qualifies(Styles { noactivate: true, ..plain() }, false));
        assert!(qualifies(Styles { noactivate: true, app: true, ..plain() }, false));
        assert!(!qualifies(Styles { owned: true, ..plain() }, false));
        assert!(qualifies(Styles { owned: true, app: true, ..plain() }, false));
        assert!(!qualifies(Styles { cloaked: true, ..plain() }, false));
        assert!(!qualifies(Styles { taskbar_deleted: true, ..plain() }, false));
        assert!(!qualifies(plain(), true));
        for class in ["Progman", "WorkerW", "Windows.UI.Core.CoreWindow", "Windows.Internal.Shell.TabProxyWindow"] {
            assert!(is_excluded_class(class), "{class}");
        }
        assert!(!is_excluded_class("Notepad"));
        assert!(!is_excluded_class("ApplicationFrameWindow"));
    }

    struct Fake {
        wins: Vec<RawWindow>,
        enumerations: u32,
        hung_calls: u32,
        hung_hwnd: isize,
    }

    impl Fake {
        fn new(wins: Vec<RawWindow>) -> Fake {
            Fake { wins, enumerations: 0, hung_calls: 0, hung_hwnd: -1 }
        }
    }

    // The sampler owns the source, so the tests read the counters through a shared cell.
    struct Shared(std::sync::Arc<std::sync::Mutex<Fake>>);

    impl WindowSource for Shared {
        fn enumerate(&mut self, out: &mut Vec<RawWindow>) {
            let mut f = self.0.lock().unwrap();
            f.enumerations += 1;
            out.clear();
            out.extend_from_slice(&f.wins);
        }
        fn hung(&mut self, hwnd: isize) -> bool {
            let mut f = self.0.lock().unwrap();
            f.hung_calls += 1;
            f.hung_hwnd == hwnd
        }
    }

    fn raw(hwnd: isize, pid: u32) -> RawWindow {
        RawWindow { hwnd, pid, styles: plain(), excluded_class: false, core_pid: None }
    }

    fn table(pids: &[u32]) -> ProcTable {
        let mut t = ProcTable::default();
        for &pid in pids {
            t.push(ProcRow { key: ProcKey { pid, created: u64::from(pid) * 10 }, ..ProcRow::default() });
        }
        t.finish();
        t
    }

    fn frame(pids: &[u32], need: NeedSet) -> Frame {
        Frame { procs: table(pids), need, ..Frame::default() }
    }

    fn setup(wins: Vec<RawWindow>) -> (WindowSampler<Shared>, std::sync::Arc<std::sync::Mutex<Fake>>) {
        let cell = std::sync::Arc::new(std::sync::Mutex::new(Fake::new(wins)));
        (WindowSampler::new(Shared(cell.clone())), cell)
    }

    #[test]
    fn frame_host_maps_to_core_process() {
        let mut host = raw(1, 100);
        host.core_pid = Some(200);
        let (mut s, _) = setup(vec![host]);
        let mut f = frame(&[100, 200], NeedSet::EMPTY.with(Need::Windows));
        s.sample(0, &mut f).unwrap();
        assert_eq!(f.windows.len(), 1);
        assert_eq!(f.windows[0].owner, ProcKey { pid: 200, created: 2000 });
        assert!(f.windows_fresh);
    }

    #[test]
    fn reconcile_every_5_s_or_when_dirty() {
        let (mut s, cell) = setup(vec![raw(1, 100)]);
        let mut f = frame(&[100], NeedSet::EMPTY.with(Need::Windows));
        s.sample(1_000_000, &mut f).unwrap();
        assert_eq!(cell.lock().unwrap().enumerations, 1);
        s.sample(2_000_000, &mut f).unwrap();
        s.sample(5_999_999, &mut f).unwrap();
        assert_eq!(cell.lock().unwrap().enumerations, 1);
        s.sample(6_000_000, &mut f).unwrap();
        assert_eq!(cell.lock().unwrap().enumerations, 2);
        s.dirty_flag().store(true, Ordering::Release);
        s.sample(6_100_000, &mut f).unwrap();
        assert_eq!(cell.lock().unwrap().enumerations, 3);
        s.sample(6_200_000, &mut f).unwrap();
        assert_eq!(cell.lock().unwrap().enumerations, 3);
        assert!(!s.dirty_flag().load(Ordering::Acquire));
    }

    #[test]
    fn fresh_only_when_the_list_changed() {
        let (mut s, cell) = setup(vec![raw(1, 100)]);
        let mut f = frame(&[100], NeedSet::EMPTY.with(Need::Windows));
        s.sample(0, &mut f).unwrap();
        assert!(f.windows_fresh);
        s.sample(1_000_000, &mut f).unwrap();
        assert!(!f.windows_fresh);
        cell.lock().unwrap().wins.push(raw(2, 100));
        s.dirty_flag().store(true, Ordering::Release);
        s.sample(2_000_000, &mut f).unwrap();
        assert!(f.windows_fresh);
        assert_eq!(f.windows.len(), 2);
    }

    #[test]
    fn hung_only_when_needed() {
        let (mut s, cell) = setup(vec![raw(1, 100)]);
        cell.lock().unwrap().hung_hwnd = 1;
        let mut f = frame(&[100], NeedSet::EMPTY.with(Need::Windows));
        s.sample(0, &mut f).unwrap();
        assert_eq!(cell.lock().unwrap().hung_calls, 0);
        assert!(!f.windows[0].hung);
        let mut g = frame(&[100], NeedSet::EMPTY.with(Need::Windows).with(Need::Hung));
        s.sample(1_000_000, &mut g).unwrap();
        assert_eq!(cell.lock().unwrap().hung_calls, 1);
        assert!(g.windows[0].hung);
        assert!(g.windows_fresh, "the hung flag changes the list");
    }

    #[test]
    fn unknown_pid_dropped() {
        let (mut s, _) = setup(vec![raw(1, 100), raw(2, 999), cloaked(3, 100)]);
        let mut f = frame(&[100], NeedSet::EMPTY.with(Need::Windows));
        s.sample(0, &mut f).unwrap();
        assert_eq!(f.windows.iter().map(|w| w.hwnd).collect::<Vec<_>>(), vec![1]);
    }

    fn cloaked(hwnd: isize, pid: u32) -> RawWindow {
        RawWindow { styles: Styles { cloaked: true, ..plain() }, ..raw(hwnd, pid) }
    }

    #[test]
    fn win32_enumeration_reads_without_side_effects() {
        let mut v = Vec::new();
        Win32Windows.enumerate(&mut v);
        assert!(v.iter().all(|r| r.pid != 0));
        // SAFETY: no arguments.
        let own = unsafe { GetCurrentProcessId() };
        assert!(v.iter().all(|r| r.pid != own));
    }
}
