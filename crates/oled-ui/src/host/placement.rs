//! Window placement (P2-kit 2.5; K1a.10, KA2): where a window reopens, per monitor and DPI. The rules are a pure
//! function over a monitor list (tested on fake lists); the glue reads the monitors, applies the result before the first
//! show and saves the placement through the PlacementStore on WM_EXITSIZEMOVE and on WM_DESTROY.

use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
    MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowPlacement, SW_SHOWMAXIMIZED, WINDOWPLACEMENT, WPF_RESTORETOMAXIMIZED,
};

use super::window::{self, WinHost};
use super::{def_proc, WindowKind, WindowSpec};
use crate::geom::{PointP, RectP, Scale, SizeL};

/// The key a window's placement is stored under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PlacementKey(pub &'static str);

/// Where a window was.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placement {
    /// The monitor's device name.
    pub monitor: String,
    /// Logical px, relative to that monitor's work area: the client area of the restored window.
    pub rect: RectP,
    pub dpi: u32,
    pub maximized: bool,
}

/// Where an app keeps its windows' placements.
pub trait PlacementStore {
    fn load(&self, key: PlacementKey) -> Option<Placement>;
    fn save(&mut self, key: PlacementKey, p: &Placement);
}

/// A monitor as the placement sees it: its device name, its rect and work area (physical px, virtual-screen
/// coordinates) and its effective DPI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Monitor {
    pub(crate) device: String,
    pub(crate) rect: RectP,
    pub(crate) work: RectP,
    pub(crate) dpi: u32,
}

/// Where a window opens: the monitor (an index into the list), the client rect of the restored window (physical px,
/// screen coordinates) and whether it opens maximised.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Placed {
    pub(crate) monitor: usize,
    pub(crate) rect: RectP,
    pub(crate) maximized: bool,
}

/// The frame around a client area: left, top, right and bottom insets in physical px (all >= 0).
pub(crate) type Insets = RECT;

/// The monitor under `p`, else the nearest one, else the first.
fn monitor_at(mons: &[Monitor], p: PointP) -> usize {
    if let Some(i) = mons.iter().position(|m| m.rect.contains(p)) {
        return i;
    }
    let dist = |m: &Monitor| {
        let dx = (m.rect.l - p.x).max(p.x - (m.rect.r - 1)).max(0) as i64;
        let dy = (m.rect.t - p.y).max(p.y - (m.rect.b - 1)).max(0) as i64;
        dx * dx + dy * dy
    };
    mons.iter().enumerate().min_by_key(|(_, m)| dist(m)).map_or(0, |(i, _)| i)
}

/// A client rect fitted to a monitor: at least the minimum (which wins over the work area), at most the work area less
/// the frame, its frame moved inside the work area.
fn clamp(r: RectP, m: &Monitor, min: SizeL, ins: Insets) -> RectP {
    let s = Scale { dpi: m.dpi };
    let (fw, fh) = (ins.left + ins.right, ins.top + ins.bottom);
    let cw = r.w().min(m.work.w() - fw).max(s.px(min.w)).max(1);
    let ch = r.h().min(m.work.h() - fh).max(s.px(min.h)).max(1);
    let l = (r.l - ins.left).min(m.work.r - (cw + fw)).max(m.work.l);
    let t = (r.t - ins.top).min(m.work.b - (ch + fh)).max(m.work.t);
    RectP::wh(l + ins.left, t + ins.top, cw, ch)
}

/// A client of `size` logical px centred on a monitor's work area, fitted to it.
fn centre(m: &Monitor, size: SizeL, min: SizeL, ins: Insets) -> RectP {
    let s = Scale { dpi: m.dpi };
    let (cw, ch) = (s.px(size.w), s.px(size.h));
    let (ow, oh) = (cw + ins.left + ins.right, ch + ins.top + ins.bottom);
    let l = m.work.l + (m.work.w() - ow) / 2 + ins.left;
    let t = m.work.t + (m.work.h() - oh) / 2 + ins.top;
    clamp(RectP::wh(l, t, cw, ch), m, min, ins)
}

/// The largest logical size, and the farthest logical offset from its work area's origin, a saved rect may have.
const SAVED_MAX_SIZE: i32 = 32_768;
const SAVED_MAX_OFFSET: i32 = 65_536;

/// True when a saved rect (read from a store file: outside data) is one place() may compute with: its corner within
/// SAVED_MAX_OFFSET of the work area's origin, its size 1 to SAVED_MAX_SIZE each way. Checked before any arithmetic.
fn trusted(r: RectP) -> bool {
    let corner = |v: i32| (-SAVED_MAX_OFFSET..=SAVED_MAX_OFFSET).contains(&v);
    let size = |a: i32, b: i32| (1..=i64::from(SAVED_MAX_SIZE)).contains(&(i64::from(b) - i64::from(a)));
    corner(r.l) && corner(r.t) && size(r.l, r.r) && size(r.t, r.b)
}

/// Where a window opens (SPEC-platform 5.3.1): no record, or opened by a chord: centred on the pointer's monitor at the
/// spec's size; a record whose monitor is gone, or whose window would be less than half visible on it: centred on the
/// pointer's monitor at the saved logical size; otherwise the saved logical rect at that monitor's DPI. Always fitted
/// to the work area with the spec's minimum; a maximised record reopens maximised (on its own monitor when it is there).
/// A record outside the trusted bounds (a corrupt or hand-edited file) counts as no record.
pub(crate) fn place(
    saved: Option<&Placement>,
    mons: &[Monitor],
    pointer: PointP,
    spec: &WindowSpec,
    frame: &dyn Fn(u32) -> Insets,
) -> Placed {
    let min = match spec.kind {
        WindowKind::Opaque { min, .. } => min,
        WindowKind::Layered { .. } => SizeL::default(),
    };
    if mons.is_empty() {
        return Placed { monitor: 0, rect: RectP::wh(0, 0, spec.size.w.max(1), spec.size.h.max(1)), maximized: false };
    }
    let here = monitor_at(mons, pointer);
    let fresh = |i: usize, size: SizeL, maximized: bool| {
        let m = &mons[i];
        Placed { monitor: i, rect: centre(m, size, min, frame(m.dpi)), maximized }
    };
    let Some(p) = saved.filter(|p| !spec.opened_by_chord && trusted(p.rect)) else {
        return fresh(here, spec.size, false);
    };
    let size = SizeL { w: p.rect.w(), h: p.rect.h() };
    let Some(i) = mons.iter().position(|m| m.device == p.monitor) else { return fresh(here, size, p.maximized) };
    let (m, ins) = (&mons[i], frame(mons[i].dpi));
    let s = Scale { dpi: m.dpi };
    let client = RectP::wh(m.work.l + s.px(p.rect.l), m.work.t + s.px(p.rect.t), s.px(size.w), s.px(size.h));
    let outer =
        RectP { l: client.l - ins.left, t: client.t - ins.top, r: client.r + ins.right, b: client.b + ins.bottom };
    if outer.intersect(m.work).area() * 2 < outer.area() {
        return fresh(here, size, p.maximized);
    }
    Placed { monitor: i, rect: clamp(client, m, min, ins), maximized: p.maximized }
}

/// The record of a restored client rect (physical px, screen coordinates): the monitor holding its centre (else the
/// nearest), the rect in logical px relative to that monitor's work area at its DPI.
pub(crate) fn record(client: RectP, maximized: bool, mons: &[Monitor]) -> Option<Placement> {
    let centre = PointP { x: (client.l + client.r) / 2, y: (client.t + client.b) / 2 };
    let m = mons.get(monitor_at(mons, centre))?;
    let s = Scale { dpi: m.dpi };
    let rect = RectP::wh(
        s.logical(client.l - m.work.l),
        s.logical(client.t - m.work.t),
        s.logical(client.w()),
        s.logical(client.h()),
    );
    Some(Placement { monitor: m.device.clone(), rect, dpi: m.dpi, maximized })
}

/// Every monitor: device name, rect, work area and effective DPI (EnumDisplayMonitors, GetMonitorInfoW,
/// GetDpiForMonitor).
pub(crate) fn monitors() -> Vec<Monitor> {
    unsafe extern "system" fn each(h: HMONITOR, _dc: HDC, _clip: *mut RECT, data: LPARAM) -> BOOL {
        // SAFETY: `data` is the Vec monitors() passed, alive and borrowed by nobody else during the enumeration.
        let out = unsafe { &mut *(data.0 as *mut Vec<Monitor>) };
        let mut mi = MONITORINFOEXW::default();
        mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        // SAFETY: `mi` is a writable MONITORINFOEXW whose cbSize says so.
        if unsafe { GetMonitorInfoW(h, (&mut mi as *mut MONITORINFOEXW).cast::<MONITORINFO>()) }.as_bool() {
            let n = mi.szDevice.iter().position(|c| *c == 0).unwrap_or(mi.szDevice.len());
            let (mut dx, mut dy) = (96u32, 96u32);
            // SAFETY: the out parameters are writable u32s.
            if unsafe { GetDpiForMonitor(h, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) }.is_err() {
                dx = 96;
            }
            out.push(Monitor {
                device: String::from_utf16_lossy(&mi.szDevice[..n]),
                rect: mi.monitorInfo.rcMonitor.into(),
                work: mi.monitorInfo.rcWork.into(),
                dpi: dx,
            });
        }
        BOOL(1)
    }
    let mut v: Vec<Monitor> = Vec::new();
    // SAFETY: the callback only touches `v`, which outlives the synchronous enumeration.
    let _ = unsafe { EnumDisplayMonitors(None, None, Some(each), LPARAM(&mut v as *mut Vec<Monitor> as isize)) };
    v
}

/// WINDOWPLACEMENT's rects are in workspace coordinates: screen coordinates less the primary monitor's work-area origin.
fn workspace_origin() -> PointP {
    // SAFETY: MonitorFromPoint with MONITOR_DEFAULTTOPRIMARY always returns a monitor.
    let mon = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: `mi` is writable with cbSize set.
    if unsafe { GetMonitorInfoW(mon, &mut mi) }.as_bool() {
        PointP { x: mi.rcWork.left - mi.rcMonitor.left, y: mi.rcWork.top - mi.rcMonitor.top }
    } else {
        PointP::default()
    }
}

/// The window's placement now: its restored client rect (GetWindowPlacement) and whether it is maximised.
fn current(host: &WinHost) -> Option<Placement> {
    let mut wp = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
    // SAFETY: our window; `wp` is writable with its length set. GetWindowPlacement sends no message.
    unsafe { GetWindowPlacement(host.hwnd, &mut wp) }.ok()?;
    let o = workspace_origin();
    let n = wp.rcNormalPosition;
    let outer = RectP { l: n.left + o.x, t: n.top + o.y, r: n.right + o.x, b: n.bottom + o.y };
    let maximized = wp.showCmd == SW_SHOWMAXIMIZED.0 as u32 || wp.flags.contains(WPF_RESTORETOMAXIMIZED);
    let mons = monitors();
    let centre = PointP { x: (outer.l + outer.r) / 2, y: (outer.t + outer.b) / 2 };
    let dpi = mons.get(monitor_at(&mons, centre)).map_or(host.scale.dpi, |m| m.dpi);
    let ins = window::frame_for(&host.kind, dpi);
    let client = RectP { l: outer.l + ins.left, t: outer.t + ins.top, r: outer.r - ins.right, b: outer.b - ins.bottom };
    record(client, maximized, &mons)
}

/// Saves the window's placement, if it has a key and the loop a store.
fn save(host: &WinHost) {
    let Some(key) = host.placement else { return };
    if host.lp.store.borrow().is_none() {
        return;
    }
    if let Some(p) = current(host) {
        if let Some(store) = host.lp.store.borrow_mut().as_mut() {
            store.save(key, &p);
        }
    }
}

/// WM_EXITSIZEMOVE: saves the placement.
pub(crate) fn on_exitsizemove(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let _ = window::with_host(hwnd, |h| save(h));
    def_proc(hwnd, msg, wp, lp)
}

/// On destroy (hooks::on_destroy): saves the placement.
pub(crate) fn on_destroy(host: &mut WinHost, hwnd: HWND) {
    let _ = hwnd;
    save(host);
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::*;
    use crate::host::Caption;

    fn mon(device: &str, rect: RectP, dpi: u32) -> Monitor {
        // A 48 px taskbar-like strip at the bottom of each work area.
        let work = RectP { b: rect.b - 48, ..rect };
        Monitor { device: device.to_string(), rect, work, dpi }
    }

    fn spec(w: i32, h: i32, min: SizeL, chord: bool) -> WindowSpec {
        WindowSpec {
            kind: WindowKind::Opaque { min, caption: Caption::System },
            class: "MiniOledTest",
            title: Cow::Borrowed("t"),
            size: SizeL { w, h },
            owner: None,
            esc_closes: false,
            placement: Some(PlacementKey("main")),
            icon_resource: None,
            opened_by_chord: chord,
        }
    }

    /// No frame: the client rect is the window rect.
    fn none(_dpi: u32) -> Insets {
        RECT::default()
    }

    /// A frame that scales with the DPI: 8 px sides and bottom, a 31 px caption at 96.
    fn scaled(dpi: u32) -> Insets {
        let s = Scale { dpi };
        RECT { left: s.px(8), top: s.px(31), right: s.px(8), bottom: s.px(8) }
    }

    fn rec(device: &str, rect: RectP, dpi: u32, maximized: bool) -> Placement {
        Placement { monitor: device.to_string(), rect, dpi, maximized }
    }

    const NO_MIN: SizeL = SizeL { w: 0, h: 0 };

    #[test]
    fn first_run_centres_on_pointer_monitor() {
        let mons = [mon("A", RectP::wh(0, 0, 1920, 1080), 96), mon("B", RectP::wh(1920, 0, 2560, 1440), 144)];
        let p = place(None, &mons, PointP { x: 2000, y: 100 }, &spec(640, 480, NO_MIN, false), &none);
        assert_eq!(p.monitor, 1);
        assert_eq!(p.rect, RectP::wh(1920 + (2560 - 960) / 2, (1392 - 720) / 2, 960, 720), "640 x 480 at 150 %");
        assert!(!p.maximized);
    }

    #[test]
    fn missing_monitor_falls_back() {
        let mons = [mon("A", RectP::wh(0, 0, 1920, 1080), 96)];
        let saved = rec("GONE", RectP::wh(100, 100, 500, 400), 96, false);
        let p = place(Some(&saved), &mons, PointP { x: 5, y: 5 }, &spec(640, 480, NO_MIN, false), &none);
        assert_eq!(p.monitor, 0);
        assert_eq!(p.rect, RectP::wh((1920 - 500) / 2, (1032 - 400) / 2, 500, 400), "centred at the saved size");
    }

    #[test]
    fn half_visible_rule() {
        let mons = [mon("A", RectP::wh(0, 0, 1000, 1048), 96)];
        let s = spec(640, 480, NO_MIN, false);
        // 400 wide, 150 of it on the monitor: less than half visible -> centred.
        let off = rec("A", RectP::wh(850, 100, 400, 300), 96, false);
        let p = place(Some(&off), &mons, PointP::default(), &s, &none);
        assert_eq!(p.rect, RectP::wh(300, 350, 400, 300));
        // 250 of 400 on it: kept where it was, moved back inside the work area.
        let half = rec("A", RectP::wh(750, 100, 400, 300), 96, false);
        let p = place(Some(&half), &mons, PointP::default(), &s, &none);
        assert_eq!(p.rect, RectP::wh(600, 100, 400, 300), "pushed in from the right edge");
        let inside = rec("A", RectP::wh(10, 20, 400, 300), 96, false);
        assert_eq!(place(Some(&inside), &mons, PointP::default(), &s, &none).rect, RectP::wh(10, 20, 400, 300));
    }

    #[test]
    fn smaller_resolution_clamps() {
        let mons = [mon("A", RectP::wh(0, 0, 1280, 768), 96)];
        let saved = rec("A", RectP::wh(100, 50, 1800, 1000), 96, false);
        let p = place(Some(&saved), &mons, PointP::default(), &spec(640, 480, NO_MIN, false), &scaled);
        assert_eq!(p.rect, RectP { l: 8, t: 31, r: 1280 - 8, b: 720 - 8 }, "the whole frame fits the work area");
    }

    #[test]
    fn negative_origin_monitor() {
        let mons = [mon("A", RectP::wh(0, 0, 1920, 1080), 96), mon("L", RectP::wh(-1600, -200, 1600, 900), 120)];
        let p = place(None, &mons, PointP { x: -800, y: 0 }, &spec(400, 300, NO_MIN, false), &none);
        assert_eq!(p.monitor, 1);
        assert_eq!(p.rect, RectP::wh(-1600 + (1600 - 500) / 2, -200 + (852 - 375) / 2, 500, 375));
        let saved = record(p.rect, false, &mons).unwrap();
        assert_eq!(saved.monitor, "L");
        assert_eq!(place(Some(&saved), &mons, PointP::default(), &spec(400, 300, NO_MIN, false), &none).rect, p.rect);
    }

    #[test]
    fn dpi_100_to_200_keeps_logical_size() {
        let at100 = [mon("A", RectP::wh(0, 0, 1920, 1080), 96)];
        let saved = record(RectP::wh(100, 80, 640, 560), false, &at100).unwrap();
        assert_eq!((saved.rect, saved.dpi), (RectP::wh(100, 80, 640, 560), 96));
        let at200 = [mon("A", RectP::wh(0, 0, 3840, 2160), 192)];
        let p = place(Some(&saved), &at200, PointP::default(), &spec(320, 240, NO_MIN, false), &scaled);
        assert_eq!(p.rect, RectP::wh(200, 160, 1280, 1120), "the same logical rect at 200 %");
    }

    #[test]
    fn maximised_reopens_maximised() {
        let mons = [mon("A", RectP::wh(0, 0, 1920, 1080), 96), mon("B", RectP::wh(1920, 0, 1920, 1080), 96)];
        let saved = rec("B", RectP::wh(100, 100, 800, 600), 96, true);
        let p = place(Some(&saved), &mons, PointP { x: 10, y: 10 }, &spec(640, 480, NO_MIN, false), &none);
        assert_eq!((p.monitor, p.maximized), (1, true), "on its own monitor, not the pointer's");
        assert_eq!(p.rect, RectP::wh(2020, 100, 800, 600), "its restored rect stays the saved one");
    }

    #[test]
    fn chord_open_ignores_the_record() {
        let mons = [mon("A", RectP::wh(0, 0, 1920, 1080), 96), mon("B", RectP::wh(1920, 0, 1920, 1080), 96)];
        let saved = rec("A", RectP::wh(100, 100, 800, 600), 96, true);
        let p = place(Some(&saved), &mons, PointP { x: 2500, y: 10 }, &spec(640, 480, NO_MIN, true), &none);
        assert_eq!((p.monitor, p.maximized), (1, false));
        assert_eq!(p.rect, RectP::wh(1920 + 640, (1032 - 480) / 2, 640, 480), "centred at the spec's size");
    }

    #[test]
    fn minimum_size_wins() {
        let mons = [mon("A", RectP::wh(0, 0, 1920, 1080), 144)];
        let min = SizeL { w: 480, h: 360 };
        let saved = rec("A", RectP::wh(10, 10, 200, 100), 144, false);
        let p = place(Some(&saved), &mons, PointP::default(), &spec(640, 480, min, false), &none);
        assert_eq!((p.rect.w(), p.rect.h()), (720, 540), "a saved rect below the minimum grows to it");
        let tiny = [mon("A", RectP::wh(0, 0, 400, 348), 96)];
        let p = place(None, &tiny, PointP::default(), &spec(640, 480, min, false), &none);
        assert_eq!((p.rect.w(), p.rect.h()), (480, 360), "the minimum wins over the work area");
        assert_eq!((p.rect.l, p.rect.t), (0, 0), "its corner stays on the work area");
    }

    #[test]
    fn record_round_trips_through_place() {
        let mons = [mon("A", RectP::wh(0, 0, 1920, 1080), 96), mon("B", RectP::wh(1920, -100, 2560, 1440), 144)];
        for r in [RectP::wh(30, 40, 600, 450), RectP::wh(2100, 50, 900, 600), RectP::wh(1950, -40, 1200, 900)] {
            let saved = record(r, false, &mons).unwrap();
            let p = place(Some(&saved), &mons, PointP::default(), &spec(320, 240, NO_MIN, false), &none);
            assert_eq!(p.rect, r, "{saved:?}");
        }
        assert!(record(RectP::wh(0, 0, 10, 10), false, &[]).is_none());
    }

    #[test]
    fn absurd_record_falls_back() {
        // A work area that does not start at 0: the record's offsets are added to a non-zero origin.
        let mons = [mon("A", RectP::wh(1920, 200, 1920, 1080), 96)];
        let s = spec(640, 480, NO_MIN, false);
        let at = PointP { x: 2000, y: 300 };
        let fresh = place(None, &mons, at, &s, &scaled);
        let absurd = [
            RectP { l: 0, t: 0, r: i32::MAX, b: 720 },
            RectP { l: i32::MIN, t: i32::MIN, r: i32::MAX, b: i32::MAX },
            RectP { l: i32::MAX - 10, t: 0, r: i32::MAX, b: 100 },
            RectP { l: 0, t: i32::MIN, r: 400, b: i32::MIN + 300 },
            RectP { l: 100, t: 100, r: 50, b: 400 },
            RectP::wh(100, 100, 0, 300),
            RectP::wh(0, 0, 40_000, 300),
            RectP::wh(-70_000, 0, 400, 300),
        ];
        for r in absurd {
            for maximized in [false, true] {
                let saved = rec("A", r, 96, maximized);
                assert_eq!(place(Some(&saved), &mons, at, &s, &scaled), fresh, "{r:?} counts as no record");
            }
        }
        // The largest record it trusts is placed (less than half visible: centred at its size, fitted).
        let big = rec("A", RectP::wh(0, 0, 32_768, 32_768), 96, false);
        assert_eq!(place(Some(&big), &mons, at, &s, &scaled).rect, RectP::wh(1920 + 8, 200 + 31, 1904, 993));
    }
}
