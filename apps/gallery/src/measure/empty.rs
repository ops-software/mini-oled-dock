//! S-EMPTY: the empty kit window's private bytes and first present (K1a.7). The helpers the other measure modes share
//! (private bytes, a one-shot waitable timer, the output line) live here too.

use std::borrow::Cow;
use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use oled_ui::command::CommandTable;
use oled_ui::cx::{EventCx, LayoutCx, PaintCx};
use oled_ui::event::{Event, Reply};
use oled_ui::geom::{RectP, SizeL, SizeP};
use oled_ui::host::{AppId, Caption, EventLoop, LoopCx, WindowKind, WindowSpec};
use oled_ui::model::Align;
use oled_ui::screen::Screen;
use oled_ui::strings::Lang;
use oled_ui::text::Trim;
use oled_ui::theme::{ThemeSpec, TypeRole};
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::System::Ole::OleInitialize;
use windows::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows::Win32::System::Threading::{
    CreateWaitableTimerExW, GetCurrentProcess, GetProcessTimes, SetWaitableTimer, TIMER_ALL_ACCESS,
};
use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetDpiForWindow};

use crate::cli::MeasureOpts;

/// The process's private bytes now (PROCESS_MEMORY_COUNTERS_EX.PrivateUsage); 0 when Windows does not say.
pub fn private_bytes() -> u64 {
    let mut c = PROCESS_MEMORY_COUNTERS_EX {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    // SAFETY: `c` is a writable PROCESS_MEMORY_COUNTERS_EX whose size is passed; the pseudo handle needs no closing.
    let ok = unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&mut c as *mut PROCESS_MEMORY_COUNTERS_EX).cast::<PROCESS_MEMORY_COUNTERS>(),
            c.cb,
        )
    };
    if ok.is_ok() {
        c.PrivateUsage as u64
    } else {
        0
    }
}

/// A one-shot (or periodic) waitable timer for a measure's deadlines, closed on drop.
pub struct Deadline(HANDLE);

impl Deadline {
    /// A synchronization timer (a satisfied wait resets it); None when Windows refuses one.
    pub fn new() -> Option<Deadline> {
        // SAFETY: an unnamed timer with default security; the handle is owned by the value and closed in Drop.
        unsafe { CreateWaitableTimerExW(None, None, 0, TIMER_ALL_ACCESS.0) }.ok().map(Deadline)
    }

    /// Signalled `ms` from now, then every `period_ms` if it is not 0.
    pub fn arm(&self, ms: u64, period_ms: i32) -> bool {
        let due = -(ms.min(i64::MAX as u64 / 10_000) as i64 * 10_000);
        // SAFETY: our timer; `due` lives for the call; no completion routine.
        unsafe { SetWaitableTimer(self.0, &due, period_ms, None, None, false) }.is_ok()
    }

    pub fn handle(&self) -> HANDLE {
        self.0
    }
}

impl Drop for Deadline {
    fn drop(&mut self) {
        // SAFETY: the timer this value created; nothing else closes it.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// Prints a measure's line and, with --out, writes it to that file too. The exit code: 0, or 1 when the file fails.
pub fn emit(line: &str, out: Option<&Path>) -> i32 {
    println!("{line}");
    if let Some(p) = out {
        if let Err(e) = std::fs::write(p, format!("{line}\n")) {
            eprintln!("measure: {}: {e}", p.display());
            return 1;
        }
    }
    0
}

/// The line of S-EMPTY: private bytes, the first present in ms after the process started (null without a window), the
/// window's DPI and whether OLE was initialised.
pub fn empty_line(private_bytes: u64, first_present_ms: Option<f64>, dpi: u32, ole: bool) -> String {
    let first = first_present_ms.map_or_else(|| "null".to_string(), |v| format!("{v:.1}"));
    format!("{{\"private_bytes\":{private_bytes},\"first_present_ms\":{first},\"dpi\":{dpi},\"ole\":{ole}}}")
}

/// 100 ns ticks since 1601 (the FILETIME epoch) of a time.
fn filetime_ticks(t: SystemTime) -> u64 {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    (d.as_secs() + 11_644_473_600) * 10_000_000 + u64::from(d.subsec_nanos() / 100)
}

/// Milliseconds from the process's creation (GetProcessTimes) to `t`.
fn ms_since_start(t: SystemTime) -> Option<f64> {
    let (mut created, mut exited, mut kernel, mut user) =
        (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    // SAFETY: four writable FILETIMEs; the pseudo handle needs no closing.
    unsafe { GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user) }.ok()?;
    let start = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
    Some(filetime_ticks(t).saturating_sub(start) as f64 / 10_000.0)
}

/// One Body line ("Empty"), or nothing with --no-text; notes when the first paint ends.
struct EmptyScreen {
    text: bool,
    height: i32,
    first: Rc<Cell<Option<SystemTime>>>,
    commands: CommandTable,
}

impl Screen for EmptyScreen {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        let _ = cx;
        self.height = avail.h;
        avail
    }

    fn paint(&self, cx: &mut PaintCx) {
        if self.text {
            let r = RectP::wh(cx.px(16), cx.px(16), cx.px(240), cx.px(20));
            let ink = cx.tokens().color.fg_primary;
            cx.text_in("Empty", TypeRole::Body, r, Align::Start, Trim::End, ink);
        }
        // The band that reaches the bottom of the client ends the first paint.
        if self.first.get().is_none() && cx.clip().b >= self.height {
            self.first.set(Some(SystemTime::now()));
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        let _ = cx;
        if matches!(ev, Event::CloseRequested) {
            Reply::Close
        } else {
            Reply::None
        }
    }

    fn commands(&self) -> &CommandTable {
        &self.commands
    }
}

/// `--measure empty [--ole] [--no-text] [--no-window] [--secs N] [--out f]`: one 640 x 560 kit window centred on the
/// pointer's monitor, N s (default 60) in the loop, then one JSON line.
pub fn run(opts: &MeasureOpts) -> i32 {
    let secs = u64::from(opts.secs.unwrap_or(60));
    if opts.ole {
        // SAFETY: once on this thread, before any window; the process ends without uninitialising.
        let _ = unsafe { OleInitialize(None) };
    }
    let lp = match EventLoop::new(AppId("gallery"), Lang::En, ThemeSpec::oled()) {
        Ok(lp) => lp,
        Err(e) => {
            eprintln!("measure empty: {e:?}");
            return 1;
        }
    };
    if opts.no_window {
        // The process base: the loop and no window.
        std::thread::sleep(Duration::from_secs(secs));
        // SAFETY: no precondition.
        let dpi = unsafe { GetDpiForSystem() };
        let line = empty_line(private_bytes(), None, dpi, opts.ole);
        drop(lp);
        return emit(&line, opts.out.as_deref());
    }
    window(lp, secs, opts)
}

fn window(mut lp: EventLoop, secs: u64, opts: &MeasureOpts) -> i32 {
    let first = Rc::new(Cell::new(None));
    let screen =
        EmptyScreen { text: !opts.no_text, height: 0, first: Rc::clone(&first), commands: CommandTable::new() };
    let spec = WindowSpec {
        kind: WindowKind::Opaque { min: SizeL { w: 320, h: 240 }, caption: Caption::System },
        class: "MiniOledGallery",
        title: Cow::Borrowed("Empty"),
        size: SizeL { w: 640, h: 560 },
        owner: None,
        esc_closes: false,
        placement: None,
        icon_resource: None,
        opened_by_chord: true,
    };
    if opts.no_text {
        // A window without text measures no text system: none starts beside it either.
        lp.no_text_warm_up();
    }
    let w = match lp.open(spec, screen) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("measure empty: {e:?}");
            return 1;
        }
    };
    let Some(deadline) = Deadline::new().filter(|d| d.arm(secs * 1000, 0)) else {
        eprintln!("measure empty: no timer");
        return 1;
    };
    let hwnd = lp.hwnd(w);
    let line: Rc<Cell<Option<String>>> = Rc::new(Cell::new(None));
    let (out, ole) = (Rc::clone(&line), opts.ole);
    let done = move |cx: &mut LoopCx| {
        // SAFETY: our window, alive until the loop ends.
        let dpi = hwnd.map_or(0, |h| unsafe { GetDpiForWindow(h) });
        out.set(Some(empty_line(private_bytes(), first.get().and_then(ms_since_start), dpi, ole)));
        cx.quit(0);
    };
    if let Err(e) = lp.add_handle(deadline.handle(), Box::new(done)) {
        eprintln!("measure empty: {e:?}");
        return 1;
    }
    lp.run();
    drop(deadline);
    match line.take() {
        Some(l) => emit(&l, opts.out.as_deref()),
        None => {
            eprintln!("measure empty: the window was closed before the end");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measure_empty_json_shape() {
        let line = empty_line(3_145_728, Some(21.26), 144, false);
        assert_eq!(line, r#"{"private_bytes":3145728,"first_present_ms":21.3,"dpi":144,"ole":false}"#);
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["private_bytes"].as_u64(), Some(3_145_728));
        assert_eq!(v["first_present_ms"].as_f64(), Some(21.3));
        assert_eq!((v["dpi"].as_u64(), v["ole"].as_bool()), (Some(144), Some(false)));
        let base: serde_json::Value = serde_json::from_str(&empty_line(1, None, 96, true)).unwrap();
        assert!(base["first_present_ms"].is_null() && base["ole"] == true, "no window: no first present");
    }

    #[test]
    fn filetime_epoch() {
        assert_eq!(filetime_ticks(UNIX_EPOCH), 116_444_736_000_000_000);
        assert_eq!(filetime_ticks(UNIX_EPOCH + Duration::from_millis(1)), 116_444_736_000_010_000);
    }
}
