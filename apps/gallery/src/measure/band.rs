//! S-BAND: the tearing and frame-time rig (K2a.11). `gallery --measure band --monitor i [--secs N]` opens a maximised
//! opaque window on monitor i showing a full-window table whose first 16 px column is a ruler: every content row y carries
//! y mod 65536 as 16 one-pixel cells (white = 1, the black window = 0, the most significant bit at the left). It scrolls
//! at 2000 px/s through the kit's retained-scroll path (Effect::Scroll: the window's pixels move, the exposed strip is
//! painted in bands), times every frame with the system's performance counter (from the scroll to the end of the frame's
//! last band paint: the move and the paint with the blits between its bands; the last band's blit is not counted) and
//! prints one JSON line: the frame count, the p50 and p95 frame time, the private bytes and the window's size and DPI.
//! `--size WxH` opens a window of that logical size instead (the base of the maximised-minus-small private bytes).
//! band_capture.exe captures the composed frames of the monitor and counts torn ones with decode and classify.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;

use oled_ui::command::CommandTable;
use oled_ui::cx::{EventCx, LayoutCx, PaintCx};
use oled_ui::event::{Event, Reply, TimerId};
use oled_ui::geom::{RectP, SizeL, SizeP};
use oled_ui::host::{AppId, AppMsg, Caption, EventLoop, LoopCx, Placement, PlacementKey, PlacementStore};
use oled_ui::model::Align;
use oled_ui::screen::Screen;
use oled_ui::strings::Lang;
use oled_ui::text::Trim;
use oled_ui::theme::{Ink, ThemeSpec, TypeRole};
use windows::core::BOOL;
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

use super::empty::{emit, private_bytes, Deadline};
use crate::app::spec;
use crate::cli::MeasureOpts;

/// The ruler's width: one physical px per bit.
pub const RULER_W: usize = 16;
/// The scroll speed, px per second.
pub const SPEED: f64 = 2000.0;
/// The default run, s.
pub const SECS: u32 = 60;
/// The scroll timer: asked for at once, paced by Windows (USER_TIMER_MINIMUM and the system clock).
const TICK: TimerId = TimerId(1);
const TICK_MS: u32 = 1;
/// The table's row and its first text column, logical px.
const ROW_H: i32 = 28;
const TEXT_X: i32 = 32;

/// The ruler cells of a content row: its index mod 65536, the most significant bit first (true = white).
pub fn ruler_bits(index: u64) -> [bool; RULER_W] {
    let v = index as u16;
    std::array::from_fn(|k| (v >> (RULER_W - 1 - k)) & 1 == 1)
}

/// The value one row of a captured ruler reads (16 pixels 0xAARRGGBB, left first): a cell whose channel mean is 192
/// or more is 1, 64 or less is 0; None when a cell is in between (not the ruler: something covers it) or the row is short.
pub fn decode(px: &[u32]) -> Option<u16> {
    let cells = px.get(..RULER_W)?;
    let mut v = 0u16;
    for p in cells {
        let mean = (((p >> 16) & 0xFF) + ((p >> 8) & 0xFF) + (p & 0xFF)) / 3;
        let bit = match mean {
            192.. => 1,
            0..=64 => 0,
            _ => return None,
        };
        v = (v << 1) | bit;
    }
    Some(v)
}

/// What one captured frame shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Read {
    /// One run of consecutive indices: the frame shows one offset.
    Clean,
    /// More than one run: a tear shows two (or more) offsets.
    Torn { runs: usize },
    /// Fewer than 90 % of its rows decode (covered, or not the rig's window).
    Unreadable,
}

/// A frame's ruler, top to bottom (the value of each screen row, None where it does not decode).
pub fn classify(rows: &[Option<u16>]) -> Read {
    let read = rows.iter().filter(|r| r.is_some()).count();
    if rows.is_empty() || read * 10 < rows.len() * 9 {
        return Read::Unreadable;
    }
    let mut runs = 0;
    let mut last: Option<(usize, u16)> = None;
    for (i, r) in rows.iter().enumerate() {
        let Some(v) = *r else { continue };
        match last {
            Some((j, u)) if v == u.wrapping_add((i - j) as u16) => {}
            _ => runs += 1,
        }
        last = Some((i, v));
    }
    if runs == 1 {
        Read::Clean
    } else {
        Read::Torn { runs }
    }
}

/// The 50th and 95th percentile of the frame times (nearest rank), ms; zeros without a frame.
pub fn percentiles(mut ms: Vec<f64>) -> (f64, f64) {
    if ms.is_empty() {
        return (0.0, 0.0);
    }
    ms.sort_by(f64::total_cmp);
    let at = |p: f64| ms[((p * ms.len() as f64).ceil() as usize).clamp(1, ms.len()) - 1];
    (at(0.50), at(0.95))
}

/// What a band run measured.
#[derive(Clone, Debug, PartialEq)]
pub struct BandRun {
    pub monitor: Option<u32>,
    pub secs: u32,
    pub size: SizeP,
    pub dpi: u32,
    pub frames: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub private_bytes: u64,
}

/// The run's JSON line.
pub fn band_line(r: &BandRun) -> String {
    let monitor = r.monitor.map_or_else(|| "null".to_string(), |m| m.to_string());
    format!(
        "{{\"mode\":\"band\",\"monitor\":{monitor},\"secs\":{},\"width\":{},\"height\":{},\"dpi\":{},\"frames\":{},\"p50_ms\":{:.3},\"p95_ms\":{:.3},\"private_bytes\":{}}}",
        r.secs, r.size.w, r.size.h, r.dpi, r.frames, r.p50_ms, r.p95_ms, r.private_bytes
    )
}

/// The full-window table with its ruler, scrolled by its own timer.
pub struct BandScreen {
    /// The content index of the window's top row.
    offset: u64,
    size: SizeP,
    row_h: i32,
    start: Option<Instant>,
    /// The frame in progress: when its scroll was asked for, and the end of its last band paint so far.
    frame: Cell<Option<(Instant, Option<Instant>)>>,
    /// The finished frames, ms.
    times: Rc<RefCell<Vec<f64>>>,
    commands: CommandTable,
}

/// The message that starts the scroll (Screen::data).
pub struct Start;

impl BandScreen {
    pub fn new(times: Rc<RefCell<Vec<f64>>>) -> BandScreen {
        BandScreen {
            offset: 0,
            size: SizeP::default(),
            row_h: ROW_H,
            start: None,
            frame: Cell::new(None),
            times,
            commands: CommandTable::new(),
        }
    }

    /// The content scrolled to `to`: the window's pixels move by the difference.
    fn scroll_to(&mut self, cx: &mut EventCx, to: u64) {
        let dy = (self.offset as i64 - to as i64).clamp(-(1 << 30), 1 << 30) as i32;
        if dy == 0 {
            return;
        }
        self.offset = to;
        cx.scroll(RectP::wh(0, 0, self.size.w, self.size.h), dy);
        self.frame.set(Some((Instant::now(), None)));
    }

    /// The finished frame's time goes to the list.
    fn close_frame(&self) {
        if let Some((t0, Some(end))) = self.frame.take() {
            self.times.borrow_mut().push(end.duration_since(t0).as_secs_f64() * 1000.0);
        }
    }
}

impl Screen for BandScreen {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        self.size = avail;
        self.row_h = cx.px(ROW_H).max(1);
        avail
    }

    fn paint(&self, cx: &mut PaintCx) {
        let clip = cx.clip();
        let white = Ink::white(255);
        for y in clip.t..clip.b {
            let bits = ruler_bits(self.offset + y.max(0) as u64);
            let mut k = 0;
            while k < RULER_W {
                if !bits[k] {
                    k += 1;
                    continue;
                }
                let from = k;
                while k < RULER_W && bits[k] {
                    k += 1;
                }
                cx.hline(from as i32, k as i32, y, white);
            }
        }
        let t = cx.tokens().clone();
        let rh = self.row_h as u64;
        let first = (self.offset + clip.t.max(0) as u64) / rh;
        let last = (self.offset + (clip.b.max(1) - 1) as u64) / rh;
        let x = cx.px(TEXT_X);
        for r in first..=last {
            let top = (r * rh) as i64 - self.offset as i64;
            let row = RectP::wh(x - cx.px(8), top as i32, self.size.w - x, self.row_h);
            if r.is_multiple_of(2) {
                cx.fill(row, 0, t.color.fill_hover);
            }
            let s = cx.scale();
            let col = |l: i32, w: i32| RectP { l: x + s.px(l), r: x + s.px(l + w), ..row };
            let name = format!("Row {r}");
            cx.text_in(&name, TypeRole::Body, col(0, 240), Align::Start, Trim::End, t.color.fg_primary);
            let size = format!("{} KB", (r * 37) % 9000);
            cx.text_in(&size, TypeRole::Body, col(260, 100), Align::End, Trim::None, t.color.fg_secondary);
            let kind = if r.is_multiple_of(3) { "Text document" } else { "Picture" };
            cx.text_in(kind, TypeRole::Body, col(400, 200), Align::Start, Trim::End, t.color.fg_secondary);
        }
        if let Some((t0, _)) = self.frame.get() {
            self.frame.set(Some((t0, Some(Instant::now()))));
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        match ev {
            Event::Timer(id) if *id == TICK => {
                self.close_frame();
                let now = Instant::now();
                let start = *self.start.get_or_insert(now);
                let to = (now.duration_since(start).as_secs_f64() * SPEED) as u64;
                self.scroll_to(cx, to);
                cx.set_timer(TICK, TICK_MS);
                Reply::None
            }
            Event::CloseRequested => Reply::Close,
            _ => Reply::None,
        }
    }

    fn data(&mut self, cx: &mut EventCx, msg: AppMsg) -> Reply {
        if msg.downcast::<Start>().is_ok() && self.start.is_none() {
            cx.set_timer(TICK, TICK_MS);
        }
        Reply::None
    }

    fn commands(&self) -> &CommandTable {
        &self.commands
    }
}

/// Monitor i of the enumeration `--list-monitors` prints (EnumDisplayMonitors order): its device name and DPI.
pub fn monitor(i: u32) -> Option<(String, u32)> {
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
    v.into_iter().nth(i as usize)
}

/// A placement store that opens the window maximised on one monitor and saves nothing.
struct OnMonitor(Placement);

impl PlacementStore for OnMonitor {
    fn load(&self, key: PlacementKey) -> Option<Placement> {
        let _ = key;
        Some(self.0.clone())
    }

    fn save(&mut self, key: PlacementKey, p: &Placement) {
        let _ = (key, p);
    }
}

/// The window's client size (physical px) and DPI.
fn measured(hwnd: Option<windows::Win32::Foundation::HWND>) -> (SizeP, u32) {
    let Some(h) = hwnd else { return (SizeP::default(), 0) };
    let mut r = RECT::default();
    // SAFETY: our window; `r` is writable; neither call sends a message.
    let (ok, dpi) = unsafe { (GetClientRect(h, &mut r).is_ok(), GetDpiForWindow(h)) };
    let size = if ok { SizeP { w: r.right - r.left, h: r.bottom - r.top } } else { SizeP::default() };
    (size, dpi)
}

/// `--measure band (--monitor i | --size WxH) [--secs N]`: one JSON line, exit 0; 2 for a monitor that does not exist.
pub fn run(opts: &MeasureOpts) -> i32 {
    let secs = opts.secs.unwrap_or(SECS);
    let mut lp = match EventLoop::new(AppId("gallery"), Lang::En, ThemeSpec::oled()) {
        Ok(lp) => lp,
        Err(e) => {
            eprintln!("measure band: {e:?}");
            return 1;
        }
    };
    let (size, key, monitor) = match (opts.size, opts.monitor) {
        (Some((w, h)), _) => (SizeL { w: w as i32, h: h as i32 }, None, None),
        (None, i) => {
            let i = i.unwrap_or(0);
            let Some((device, dpi)) = monitor(i) else {
                eprintln!("measure band: no monitor {i} (gallery --list-monitors)");
                return 2;
            };
            let p = Placement { monitor: device, rect: RectP::wh(40, 40, 640, 560), dpi, maximized: true };
            lp.set_placement_store(Box::new(OnMonitor(p)));
            (SizeL { w: 640, h: 560 }, Some(PlacementKey("band")), Some(i))
        }
    };
    let times = Rc::new(RefCell::new(Vec::new()));
    let screen = BandScreen::new(Rc::clone(&times));
    let w = match lp.open(spec("S-BAND", size, Caption::System, key), screen) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("measure band: {e:?}");
            return 1;
        }
    };
    lp.proxy(w).send(Box::new(Start));
    let Some(deadline) = Deadline::new().filter(|d| d.arm(u64::from(secs) * 1000, 0)) else {
        eprintln!("measure band: no timer");
        return 1;
    };
    let hwnd = lp.hwnd(w);
    let line: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let (out, t) = (Rc::clone(&line), Rc::clone(&times));
    let done = move |cx: &mut LoopCx| {
        let (size, dpi) = measured(hwnd);
        let (p50_ms, p95_ms) = percentiles(t.borrow().clone());
        let frames = t.borrow().len();
        let run = BandRun { monitor, secs, size, dpi, frames, p50_ms, p95_ms, private_bytes: private_bytes() };
        *out.borrow_mut() = Some(band_line(&run));
        cx.quit(0);
    };
    if let Err(e) = lp.add_handle(deadline.handle(), Box::new(done)) {
        eprintln!("measure band: {e:?}");
        return 1;
    }
    let code = lp.run();
    drop(deadline);
    let got = line.borrow_mut().take();
    match got {
        Some(l) => emit(&l, opts.out.as_deref()),
        None => {
            eprintln!("measure band: the window was closed before the end");
            code.max(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oled_ui::geom::PointL;
    use oled_ui::harness::Harness;
    use oled_ui::theme::Tokens;

    const WHITE: u32 = 0xFFFF_FFFF;
    const BLACK: u32 = 0xFF00_0000;

    /// The ruler of content rows from `first`, `rows` of them, as a 16-px column of pixels per row.
    fn column(first: u64, rows: usize) -> Vec<Vec<u32>> {
        (0..rows as u64)
            .map(|y| ruler_bits(first + y).iter().map(|b| if *b { WHITE } else { BLACK }).collect())
            .collect()
    }

    fn read(col: &[Vec<u32>]) -> Vec<Option<u16>> {
        col.iter().map(|r| decode(r)).collect()
    }

    #[test]
    fn ruler_roundtrip() {
        for first in [0u64, 1, 12_345, 65_530, 65_536 * 3 + 7, u64::from(u32::MAX)] {
            let col = column(first, 40);
            for (y, row) in col.iter().enumerate() {
                assert_eq!(decode(row), Some((first + y as u64) as u16), "row {y} from {first}");
            }
            assert_eq!(classify(&read(&col)), Read::Clean, "from {first}, across the wrap too");
        }
        assert!(ruler_bits(0x8001)[0], "the most significant bit at the left");
        assert!(ruler_bits(0x8001)[15] && !ruler_bits(0x8001)[1]);
        // A grey cell (text, antialiasing, another window) does not decode.
        let mut row = column(9, 1).remove(0);
        row[3] = 0xFF80_8080;
        assert_eq!(decode(&row), None);
        assert_eq!(decode(&row[..8]), None, "a short row");
    }

    #[test]
    fn torn_frame_detected() {
        // The top 300 rows already show the new offset, the rest the old one: two runs.
        let mut col = column(10_000, 300);
        col.extend(column(10_000 + 300 - 33, 700));
        assert_eq!(classify(&read(&col)), Read::Torn { runs: 2 });
        // A few covered rows inside one run do not tear it.
        let mut col = column(500, 1000);
        for r in col.iter_mut().skip(400).take(50) {
            r[0] = 0xFF80_8080;
        }
        assert_eq!(classify(&read(&col)), Read::Clean);
        // Mostly covered: not read.
        let mut col = column(500, 1000);
        for r in col.iter_mut().take(200) {
            r[5] = 0xFF60_6060;
        }
        assert_eq!(classify(&read(&col)), Read::Unreadable);
        assert_eq!(classify(&[]), Read::Unreadable);
    }

    #[test]
    fn band_line_shape_and_percentiles() {
        let (p50, p95) = percentiles((1..=100).map(f64::from).collect());
        assert_eq!((p50, p95), (50.0, 95.0));
        assert_eq!(percentiles(Vec::new()), (0.0, 0.0));
        let run = BandRun {
            monitor: Some(1),
            secs: 60,
            size: SizeP { w: 3840, h: 2091 },
            dpi: 144,
            frames: 3800,
            p50_ms: 1.25,
            p95_ms: 3.5,
            private_bytes: 5_000_000,
        };
        let v: serde_json::Value = serde_json::from_str(&band_line(&run)).unwrap();
        assert_eq!(
            (v["monitor"].as_u64(), v["frames"].as_u64(), v["p95_ms"].as_f64()),
            (Some(1), Some(3800), Some(3.5))
        );
        let small = BandRun { monitor: None, ..run };
        let v: serde_json::Value = serde_json::from_str(&band_line(&small)).unwrap();
        assert!(v["monitor"].is_null());
    }

    /// The rig's screen draws the ruler of its offset on every row, and keeps it through its own scroll.
    #[test]
    fn the_screen_draws_a_readable_ruler_that_scrolls() {
        let times = Rc::new(RefCell::new(Vec::new()));
        let mut h = Harness::new(BandScreen::new(times), SizeL { w: 400, h: 300 }, 96, Tokens::oled());
        let img = h.render();
        let col: Vec<Option<u16>> =
            (0..img.h).map(|y| decode(&img.px[(y * img.w) as usize..(y * img.w) as usize + RULER_W])).collect();
        assert_eq!(classify(&col), Read::Clean);
        assert_eq!(col[0], Some(0));
        h.screen_mut().offset = 70_000;
        let img = h.render();
        assert_eq!(decode(&img.px[..RULER_W]), Some((70_000u64 % 65_536) as u16));
        h.hover(PointL { x: 200, y: 150 });
        assert!(h.is_idle(), "it scrolls only once started");
    }
}
