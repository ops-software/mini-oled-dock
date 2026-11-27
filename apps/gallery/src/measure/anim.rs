//! S-ANIM: the frame clock's pacing (K3.4). `--measure anim [--monitor <i>] [--pacing a|b]` opens a window on monitor i
//! (default 0, in the order of --list-monitors) with candidate A (the default) or B of oled-ui's frame pacing, runs ten
//! 2 s linear tweens of a plate across 400 px, logs the QPC of each painted frame (one line per tween), then counts the
//! loop's wake-ups in the 5 s after the last tween. The summary line has the refresh period of the monitor the window
//! is on (display settings; DWM's single desktop period stays as dwm_period_ms), the source, p95 of
//! |interval - period|, the share of intervals at 15-16 ms and those wake-ups. Pass rule for B (LIVE-K3 step 1): p95
//! <= 1 ms at 60 Hz and at 144 Hz where a monitor offers it, no 15-16 ms intervals on a monitor faster than 60 Hz, 0
//! wake-ups after the tweens. It only reads the display settings and changes nothing.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use oled_ui::command::CommandTable;
use oled_ui::cx::{AnimId, Ease, EventCx, LayoutCx, PaintCx};
use oled_ui::event::{Event, Reply, TimerId};
use oled_ui::geom::{RectP, SizeL, SizeP};
use oled_ui::host::{AppId, AppMsg, Caption, EventLoop, LoopCx, LoopStats, Placement, PlacementKey, PlacementStore};
use oled_ui::node::{Node, NodeId, Role, Text, WidgetId};
use oled_ui::screen::Screen;
use oled_ui::strings::Lang;
use oled_ui::theme::ThemeSpec;
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetCompositionTimingInfo, DWM_TIMING_INFO};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromWindow, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow;

use super::empty::{emit, private_bytes, Deadline};
use super::idle::between;
use super::monitors::refresh_hz;
use crate::app::spec;
use crate::cli::{MeasureOpts, Pacing};

/// The tweens of a run, each this long, with this pause before the next one.
pub const TWEENS: usize = 10;
pub const TWEEN_MS: u32 = 2000;
pub const GAP_MS: u32 = 500;
/// How far the plate travels, logical px.
pub const TRAVEL: i32 = 400;
/// The wake-ups are counted this long after the last tween.
pub const AFTER_MS: u64 = 5000;
/// The variable oled-ui's frame pacing reads in the gallery's process (host::pacing::RIG_VAR).
pub const RIG_VAR: &str = "OLED_UI_SPIKE_PACING";

const SIZE: SizeL = SizeL { w: 560, h: 160 };
const PLATE: i32 = 96;
const MARGIN: i32 = 32;
const ID: WidgetId = WidgetId(1);
const NEXT: TimerId = TimerId(1);

/// Starts the first tween (sent through the window's proxy once the loop runs).
struct Start;

/// The plate's target in tween k: across and back.
pub fn tween_target(k: usize) -> f32 {
    if k.is_multiple_of(2) {
        1.0
    } else {
        0.0
    }
}

/// The refresh period the jitter is measured against, ms, and where it came from: the monitor's own rate (display
/// settings, `hz`) when Windows gave a real one, else DWM's single desktop period (`dwm_ms`).
pub fn period_for(hz: u32, dwm_ms: f64) -> (f64, &'static str) {
    if hz >= 24 {
        (1000.0 / f64::from(hz), "display_settings")
    } else {
        (dwm_ms, "dwm")
    }
}

/// The period of a run (pure): the monitor the window ended on (the last sample, the placed one with none), its own
/// refresh rate through `hz_of`, DWM's `dwm_ms` only when Windows gave no real rate. Returns (period ms, source,
/// refresh rate, whether the window changed monitor during the run).
pub fn run_period(
    seen: &[String],
    placed: &str,
    hz_of: impl Fn(&str) -> u32,
    dwm_ms: f64,
) -> (f64, &'static str, u32, bool) {
    let last = seen.last().map_or(placed, |s| s.as_str());
    let moved = seen.iter().any(|d| d != last);
    let hz = hz_of(last);
    let (period, source) = period_for(hz, dwm_ms);
    (period, source, hz, moved)
}

/// The intervals between consecutive frames, ms.
pub fn intervals_ms(qpc: &[i64], freq: i64) -> Vec<f64> {
    let f = freq.max(1) as f64;
    qpc.windows(2).map(|w| (w[1] - w[0]) as f64 * 1000.0 / f).collect()
}

/// The nearest-rank p95 of the values (0 with none).
pub fn p95(values: &[f64]) -> f64 {
    let mut v: Vec<f64> = values.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f64::total_cmp);
    let rank = (v.len() * 95).div_ceil(100).max(1);
    v[rank - 1]
}

/// p95 of |interval - period|, ms.
pub fn jitter_p95(intervals: &[f64], period_ms: f64) -> f64 {
    let d: Vec<f64> = intervals.iter().map(|i| (i - period_ms).abs()).collect();
    p95(&d)
}

/// The share of intervals at 15-16 ms (15.0 <= i < 16.7): a 64 Hz system tick, the quantisation of candidate A.
pub fn share_15_16(intervals: &[f64]) -> f64 {
    if intervals.is_empty() {
        return 0.0;
    }
    intervals.iter().filter(|i| (15.0..16.7).contains(*i)).count() as f64 / intervals.len() as f64
}

/// The line of one tween: its index and the QPC of each painted frame.
pub fn tween_line(k: usize, qpc: &[i64]) -> String {
    let list: Vec<String> = qpc.iter().map(i64::to_string).collect();
    format!("{{\"tween\":{k},\"qpc\":[{}]}}", list.join(","))
}

/// What the summary line reports.
#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    pub monitor: u32,
    pub pacing: Pacing,
    pub qpc_freq: i64,
    /// The refresh period the jitter uses, ms: the monitor's own (see period_for).
    pub period_ms: f64,
    /// Where period_ms came from: "display_settings" or "dwm".
    pub period_source: &'static str,
    /// The monitor's refresh rate in display settings (0 when Windows did not say).
    pub refresh_hz: u32,
    /// DWM's one desktop period, ms (0 when DWM gave none): the number older records used for every monitor.
    pub dwm_period_ms: f64,
    /// The window was on more than one monitor during the run.
    pub moved: bool,
    pub frames: usize,
    pub intervals: Vec<f64>,
    pub wakeups_after: u64,
    pub timers_live_after: u32,
    pub private_bytes: u64,
}

/// The summary line.
pub fn summary_line(s: &Summary) -> String {
    let pacing = match s.pacing {
        Pacing::A => "a",
        Pacing::B => "b",
    };
    format!(
        "{{\"monitor\":{},\"pacing\":\"{pacing}\",\"qpc_freq\":{},\"period_ms\":{:.3},\"period_source\":\"{}\",\"refresh_hz\":{},\"dwm_period_ms\":{:.3},\"moved\":{},\"frames\":{},\"intervals\":{},\"p95_jitter_ms\":{:.3},\"share_15_16\":{:.3},\"wakeups_after\":{},\"timers_live_after\":{},\"private_bytes\":{}}}",
        s.monitor,
        s.qpc_freq,
        s.period_ms,
        s.period_source,
        s.refresh_hz,
        s.dwm_period_ms,
        s.moved,
        s.frames,
        s.intervals.len(),
        jitter_p95(&s.intervals, s.period_ms),
        share_15_16(&s.intervals),
        s.wakeups_after,
        s.timers_live_after,
        s.private_bytes
    )
}

/// The QPC now (0 when Windows does not say).
fn qpc() -> i64 {
    let mut v = 0i64;
    // SAFETY: a writable i64.
    let _ = unsafe { QueryPerformanceCounter(&mut v) };
    v
}

/// The QPC frequency (1 when Windows does not say).
fn qpc_freq() -> i64 {
    let mut v = 0i64;
    // SAFETY: a writable i64.
    let _ = unsafe { QueryPerformanceFrequency(&mut v) };
    v.max(1)
}

/// DWM's one desktop refresh period in QPC ticks (as the pacing reads it); 0 when DWM gives none.
fn dwm_period() -> u64 {
    let mut info = DWM_TIMING_INFO { cbSize: std::mem::size_of::<DWM_TIMING_INFO>() as u32, ..Default::default() };
    // SAFETY: `info` is writable with cbSize set; a null window asks for the desktop's timing. Read only.
    match unsafe { DwmGetCompositionTimingInfo(HWND::default(), &mut info) } {
        Ok(()) => info.qpcRefreshPeriod,
        Err(_) => 0,
    }
}

/// Monitor i (the order of --list-monitors): its device name and effective DPI.
fn monitor(i: usize) -> Option<(String, u32)> {
    unsafe extern "system" fn each(h: HMONITOR, _dc: HDC, _clip: *mut RECT, data: LPARAM) -> BOOL {
        // SAFETY: `data` is the Vec monitor() passed, alive and borrowed by nobody else during the enumeration.
        let out = unsafe { &mut *(data.0 as *mut Vec<(String, u32)>) };
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
            out.push((String::from_utf16_lossy(&mi.szDevice[..n]), dx));
        }
        BOOL(1)
    }
    let mut v: Vec<(String, u32)> = Vec::new();
    // SAFETY: the callback only touches `v`, which outlives the synchronous enumeration.
    let _ = unsafe { EnumDisplayMonitors(None, None, Some(each), LPARAM(&mut v as *mut Vec<(String, u32)> as isize)) };
    v.into_iter().nth(i)
}

/// Places the window where the placement says (a monitor and a rect in its work area); saves nothing.
pub(super) struct OnMonitor(pub(super) Placement);

impl PlacementStore for OnMonitor {
    fn load(&self, key: PlacementKey) -> Option<Placement> {
        let _ = key;
        Some(self.0.clone())
    }

    fn save(&mut self, key: PlacementKey, p: &Placement) {
        let _ = (key, p);
    }
}

/// The device name of the monitor the thread's active window is on (None: no active window).
fn active_device() -> Option<String> {
    // SAFETY: reads the thread's active window; sends no message.
    let hwnd = unsafe { GetActiveWindow() };
    if hwnd.0.is_null() {
        return None;
    }
    // SAFETY: a window of this thread; the monitor handle is only read.
    let h = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let mut mi = MONITORINFOEXW::default();
    mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: `mi` is a writable MONITORINFOEXW whose cbSize says so.
    if !unsafe { GetMonitorInfoW(h, (&mut mi as *mut MONITORINFOEXW).cast::<MONITORINFO>()) }.as_bool() {
        return None;
    }
    let n = mi.szDevice.iter().position(|c| *c == 0).unwrap_or(mi.szDevice.len());
    Some(String::from_utf16_lossy(&mi.szDevice[..n]))
}

/// The plate and its tweens; each painted frame's QPC goes to the current tween's list.
struct AnimScreen {
    frames: Rc<RefCell<Vec<Vec<i64>>>>,
    /// The monitor the window was on at the first frame of each tween (a window dragged away is seen here).
    devices: Rc<RefCell<Vec<String>>>,
    /// Where the window was placed: the monitor when the active window is not known.
    placed: String,
    /// The tween running (None before the first one).
    tween: Cell<Option<usize>>,
    track: RectP,
    commands: CommandTable,
}

impl AnimScreen {
    fn anim() -> AnimId {
        AnimId { node: NodeId::new(ID, 0), channel: 0 }
    }

    /// Tween k: the plate jumps to its start and travels to tween_target(k); the next one is due after a pause.
    fn begin(&mut self, cx: &mut EventCx, k: usize) {
        self.tween.set(Some(k));
        self.frames.borrow_mut().push(Vec::new());
        let to = tween_target(k);
        cx.animate(Self::anim(), 1.0 - to, 0, Ease::Linear);
        cx.animate(Self::anim(), to, TWEEN_MS, Ease::Linear);
        if k + 1 < TWEENS {
            cx.set_timer(NEXT, TWEEN_MS + GAP_MS);
        }
    }
}

impl Screen for AnimScreen {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        self.track = RectP::wh(cx.px(MARGIN), cx.px(MARGIN), cx.px(TRAVEL + PLATE), cx.px(PLATE));
        // The node covers the whole track, so each frame of the clock repaints the plate's old and new place.
        cx.push(Node::new(NodeId::new(ID, 0), Role::Image, self.track).label(Text::from("Plate")));
        avail
    }

    fn paint(&self, cx: &mut PaintCx) {
        let t = cx.anim(Self::anim()).unwrap_or(0.0);
        let x = self.track.l + (t * cx.px(TRAVEL) as f32).round() as i32;
        let plate = RectP::wh(x, self.track.t, cx.px(PLATE), cx.px(PLATE));
        let ink = cx.tokens().color.fg_primary;
        cx.fill(plate, cx.px(7), ink);
        // One record per frame: the band that holds the track's middle row.
        let mid = RectP::wh(self.track.l, self.track.t + self.track.h() / 2, 1, 1);
        if let Some(k) = self.tween.get().filter(|_| cx.visible(mid)) {
            if let Some(list) = self.frames.borrow_mut().get_mut(k) {
                if list.is_empty() {
                    self.devices.borrow_mut().push(active_device().unwrap_or_else(|| self.placed.clone()));
                }
                list.push(qpc());
            }
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        match ev {
            Event::Timer(NEXT) => {
                let k = self.tween.get().map_or(0, |k| k + 1);
                if k < TWEENS {
                    self.begin(cx, k);
                }
                Reply::None
            }
            Event::CloseRequested => Reply::Close,
            _ => Reply::None,
        }
    }

    fn data(&mut self, cx: &mut EventCx, msg: AppMsg) -> Reply {
        if msg.downcast::<Start>().is_ok() && self.tween.get().is_none() {
            self.begin(cx, 0);
        }
        Reply::None
    }

    fn commands(&self) -> &CommandTable {
        &self.commands
    }
}

pub fn run(opts: &MeasureOpts) -> i32 {
    let index = opts.monitor.unwrap_or(0);
    let pacing = opts.pacing.unwrap_or(Pacing::A);
    let Some((device, dpi)) = monitor(index as usize) else {
        eprintln!("measure anim: no monitor {index} (see --list-monitors)");
        return 2;
    };
    // Read by oled-ui's pacing in this process only (the gallery's AppId), before the loop exists.
    std::env::set_var(RIG_VAR, if pacing == Pacing::B { "b" } else { "a" });
    let mut lp = match EventLoop::new(AppId("gallery"), Lang::En, ThemeSpec::oled()) {
        Ok(lp) => lp,
        Err(e) => {
            eprintln!("measure anim: {e:?}");
            return 1;
        }
    };
    let rect = RectP::wh(48, 48, SIZE.w, SIZE.h);
    lp.set_placement_store(Box::new(OnMonitor(Placement { monitor: device.clone(), rect, dpi, maximized: false })));
    let frames: Rc<RefCell<Vec<Vec<i64>>>> = Rc::new(RefCell::new(Vec::new()));
    let devices: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let screen = AnimScreen {
        frames: Rc::clone(&frames),
        devices: Rc::clone(&devices),
        placed: device.clone(),
        tween: Cell::new(None),
        track: RectP::default(),
        commands: CommandTable::new(),
    };
    let w = match lp.open(spec("Gallery", SIZE, Caption::System, Some(PlacementKey("anim"))), screen) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("measure anim: {e:?}");
            return 1;
        }
    };
    let total = TWEENS as u64 * u64::from(TWEEN_MS + GAP_MS);
    let Some(timer) = Deadline::new().filter(|d| d.arm(total, 0)).map(Rc::new) else {
        eprintln!("measure anim: no timer");
        return 1;
    };
    let after: Rc<Cell<Option<(LoopStats, LoopStats)>>> = Rc::new(Cell::new(None));
    let (out, t) = (Rc::clone(&after), Rc::clone(&timer));
    let start: Cell<Option<LoopStats>> = Cell::new(None);
    let tick = move |cx: &mut LoopCx| match start.get() {
        None => {
            start.set(Some(cx.stats()));
            if !t.arm(AFTER_MS, 0) {
                cx.quit(1);
            }
        }
        Some(s0) => {
            out.set(Some((s0, cx.stats())));
            cx.quit(0);
        }
    };
    if let Err(e) = lp.add_handle(timer.handle(), Box::new(tick)) {
        eprintln!("measure anim: {e:?}");
        return 1;
    }
    if !lp.proxy(w).send(Box::new(Start)) {
        eprintln!("measure anim: the window is gone");
        return 1;
    }
    let code = lp.run();
    let Some((s0, s1)) = after.get() else {
        eprintln!("measure anim: the window was closed before the end");
        return code.max(1);
    };
    let freq = qpc_freq();
    let mut intervals = Vec::new();
    let mut count = 0;
    for (k, list) in frames.borrow().iter().enumerate() {
        println!("{}", tween_line(k, list));
        intervals.extend(intervals_ms(list, freq));
        count += list.len();
    }
    let (wakeups_after, _) = between(s0, s1);
    // The monitor the window ended on (the first tween's, when no sample was taken), its own refresh rate.
    let dwm_period_ms = dwm_period() as f64 * 1000.0 / freq as f64;
    let (period_ms, period_source, hz, moved) = run_period(&devices.borrow(), &device, refresh_hz, dwm_period_ms);
    let s = Summary {
        monitor: index,
        pacing,
        qpc_freq: freq,
        period_ms,
        period_source,
        refresh_hz: hz,
        dwm_period_ms,
        moved,
        frames: count,
        intervals,
        wakeups_after,
        timers_live_after: s1.timers_live,
        private_bytes: private_bytes(),
    };
    emit(&summary_line(&s), opts.out.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_statistics() {
        // A 10 MHz counter: 16.667 ms frames with one late frame.
        let qpc = [0, 166_670, 333_340, 500_010, 900_000];
        let iv = intervals_ms(&qpc, 10_000_000);
        assert_eq!(iv.len(), 4);
        assert!((iv[0] - 16.667).abs() < 1e-9 && (iv[3] - 39.999).abs() < 1e-9);
        assert!((jitter_p95(&iv, 16.667) - 23.332).abs() < 1e-9, "the late frame is the p95 of four");
        assert_eq!(p95(&[]), 0.0);
        let even: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(p95(&even), 95.0, "nearest rank");
        assert_eq!(share_15_16(&[15.6, 15.6, 6.9, 16.7]), 0.5, "15.0 <= i < 16.7");
        assert_eq!(share_15_16(&[]), 0.0);
        assert!(intervals_ms(&[5], 10).is_empty());
    }

    #[test]
    fn period_is_the_monitors_own() {
        let dwm = 5.556;
        let (p60, s60) = period_for(60, dwm);
        assert!((p60 - 16.6667).abs() < 1e-3 && s60 == "display_settings", "60 Hz: {p60}");
        let (p144, _) = period_for(144, dwm);
        assert!((p144 - 6.9444).abs() < 1e-3, "144 Hz: {p144}");
        // Windows says 0 or 1 for "the hardware default": DWM's number, named as such.
        assert_eq!(period_for(0, dwm), (dwm, "dwm"));
        assert_eq!(period_for(1, dwm), (dwm, "dwm"));
        // Intervals of a 60 Hz monitor are no jitter against its own period, but were 11 ms of it against 5.556.
        let iv = [16.667; 20];
        assert!(jitter_p95(&iv, p60) < 1e-3 && jitter_p95(&iv, dwm) > 11.0);
    }

    #[test]
    fn run_period_follows_the_monitor_it_ended_on() {
        let hz_of = |d: &str| match d {
            "DISPLAY5" => 144,
            "DISPLAY2" => 60,
            _ => 0,
        };
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let (p, src, hz, moved) = run_period(&s(&["DISPLAY2", "DISPLAY2"]), "DISPLAY5", hz_of, 5.556);
        assert!((p - 16.6667).abs() < 1e-3 && src == "display_settings" && hz == 60 && !moved);
        let (p, _, hz, moved) = run_period(&s(&["DISPLAY5", "DISPLAY2"]), "DISPLAY5", hz_of, 5.556);
        assert!((p - 16.6667).abs() < 1e-3 && hz == 60 && moved, "the last monitor's rate, and it moved");
        let (p, _, hz, moved) = run_period(&[], "DISPLAY5", hz_of, 5.556);
        assert!((p - 6.9444).abs() < 1e-3 && hz == 144 && !moved, "no samples: the placed monitor");
        assert_eq!(run_period(&[], "DISPLAY9", hz_of, 5.556), (5.556, "dwm", 0, false));
    }

    #[test]
    fn tweens_go_across_and_back() {
        let t: Vec<f32> = (0..4).map(tween_target).collect();
        assert_eq!(t, [1.0, 0.0, 1.0, 0.0]);
        assert_eq!(TWEENS as u32 * (TWEEN_MS + GAP_MS), 25_000, "the count starts after the last tween's pause");
    }

    #[test]
    fn lines_are_json() {
        let v: serde_json::Value = serde_json::from_str(&tween_line(3, &[10, 20])).unwrap();
        assert_eq!((v["tween"].as_u64(), v["qpc"][1].as_i64()), (Some(3), Some(20)));
        let s = Summary {
            monitor: 1,
            pacing: Pacing::B,
            qpc_freq: 10_000_000,
            period_ms: 6.944,
            period_source: "display_settings",
            refresh_hz: 144,
            dwm_period_ms: 5.556,
            moved: false,
            frames: 3,
            intervals: vec![6.9, 7.0],
            wakeups_after: 0,
            timers_live_after: 0,
            private_bytes: 4_000_000,
        };
        let v: serde_json::Value = serde_json::from_str(&summary_line(&s)).unwrap();
        assert_eq!(v["pacing"].as_str(), Some("b"));
        assert_eq!((v["period_source"].as_str(), v["refresh_hz"].as_u64()), (Some("display_settings"), Some(144)));
        assert!(v["dwm_period_ms"].as_f64().is_some_and(|d| (d - 5.556).abs() < 1e-9) && v["moved"] == false);
        assert_eq!(
            (v["frames"].as_u64(), v["intervals"].as_u64(), v["wakeups_after"].as_u64()),
            (Some(3), Some(2), Some(0))
        );
        assert!(v["p95_jitter_ms"].as_f64().is_some_and(|j| (j - 0.056).abs() < 1e-9));
    }

    #[test]
    fn the_variable_is_the_pacings() {
        assert_eq!(RIG_VAR, "OLED_UI_SPIKE_PACING");
    }

    #[test]
    fn the_plate_records_frames_only_during_tweens() {
        use oled_ui::geom::PointL;
        use oled_ui::harness::Harness;
        use oled_ui::theme::Tokens;
        let frames = Rc::new(RefCell::new(Vec::new()));
        let devices = Rc::new(RefCell::new(Vec::new()));
        let screen = AnimScreen {
            frames: Rc::clone(&frames),
            devices: Rc::clone(&devices),
            placed: "\\\\.\\DISPLAY1".to_string(),
            tween: Cell::new(None),
            track: RectP::default(),
            commands: CommandTable::new(),
        };
        let mut h = Harness::new(screen, SIZE, 96, Tokens::oled());
        let _ = h.render();
        assert!(frames.borrow().is_empty(), "no tween, no record");
        h.send(Box::new(Start));
        h.advance(1000);
        let mid = h.render();
        // Halfway: the plate's left edge is near MARGIN + TRAVEL / 2.
        let row = (MARGIN + PLATE / 2) as u32;
        let lit = |x: i32| mid.px[(row * mid.w + x as u32) as usize] & 0xFF != 0;
        assert!(lit(MARGIN + TRAVEL / 2 + PLATE / 2) && !lit(MARGIN + 4), "the plate moved");
        let want = ["\\\\.\\DISPLAY1"];
        assert_eq!(*devices.borrow(), want, "one monitor sample per tween: the placed one, no active window");
        assert_eq!(frames.borrow().len(), 1);
        let _ = h.render();
        assert_eq!(frames.borrow()[0].len(), 2, "one record per painted frame of the tween");
        h.advance(1500);
        assert_eq!(frames.borrow().len(), 2, "the next tween began after the pause");
        h.hover(PointL { x: 1, y: 1 });
        assert!(!h.is_idle(), "a tween is running");
    }
}
