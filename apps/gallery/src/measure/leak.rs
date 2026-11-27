//! --measure leak --cycles N (K1a.11): a small base window stays open while a window per ready page is opened and closed
//! N times; the private bytes and the GDI and USER object counts after the first cycle and after the last. Pass (LIVE-K1
//! step 3): private growth < 1 MiB, GDI and USER growth <= 2 each.

use std::cell::RefCell;
use std::rc::Rc;

use oled_ui::command::CommandTable;
use oled_ui::cx::{EventCx, LayoutCx, PaintCx};
use oled_ui::event::{Event, Reply};
use oled_ui::geom::{SizeL, SizeP};
use oled_ui::host::{AppId, AppMsg, Caption, EventLoop, LoopCx, WindowRef};
use oled_ui::screen::Screen;
use oled_ui::strings::{Lang, UiLang};
use oled_ui::theme::ThemeSpec;
use windows::Win32::System::Threading::{GetCurrentProcess, GetGuiResources, GR_GDIOBJECTS, GR_USEROBJECTS};

use super::empty::{emit, private_bytes, Deadline};
use crate::app::{spec, PageScreen};
use crate::cli::MeasureOpts;
use crate::pages;

/// One step of the cycle every this many ms (an open, then a close).
pub const STEP_MS: u64 = 60;

/// The message that closes a page window of the run.
pub struct CloseNow;

/// A page that closes on CloseNow.
struct Closable(PageScreen);

impl Screen for Closable {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        self.0.layout(cx, avail)
    }

    fn paint(&self, cx: &mut PaintCx) {
        self.0.paint(cx);
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        self.0.event(cx, ev)
    }

    fn data(&mut self, cx: &mut EventCx, msg: AppMsg) -> Reply {
        if msg.is::<CloseNow>() {
            return Reply::Close;
        }
        self.0.data(cx, msg)
    }

    fn commands(&self) -> &CommandTable {
        self.0.commands()
    }
}

/// The base window: nothing in it.
struct Blank(CommandTable);

impl Screen for Blank {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        let _ = cx;
        avail
    }

    fn paint(&self, cx: &mut PaintCx) {
        let _ = cx;
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
        &self.0
    }
}

/// The process's private bytes, GDI objects and USER objects now.
fn counts() -> (i64, i64, i64) {
    // SAFETY: the pseudo handle needs no closing; GetGuiResources only reads.
    let (gdi, user) = unsafe {
        (GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS), GetGuiResources(GetCurrentProcess(), GR_USEROBJECTS))
    };
    (private_bytes() as i64, i64::from(gdi), i64::from(user))
}

/// The line of a leak run: the growth from the end of the first cycle to the end of the last.
pub fn leak_line(cycles: u32, pages: usize, first: (i64, i64, i64), last: (i64, i64, i64)) -> String {
    format!(
        "{{\"cycles\":{cycles},\"pages\":{pages},\"private_delta\":{},\"gdi_delta\":{},\"user_delta\":{}}}",
        last.0 - first.0,
        last.1 - first.1,
        last.2 - first.2
    )
}

/// The run's state between timer ticks.
struct Run {
    cycles: u32,
    done: u32,
    open: Vec<WindowRef>,
    pages: usize,
    first: Option<(i64, i64, i64)>,
    line: Option<String>,
}

fn step(cx: &mut LoopCx, r: &mut Run) {
    if r.open.is_empty() {
        for page in pages::all(UiLang::EN) {
            let size = page.size;
            match cx.open(spec("Gallery", size, Caption::System, None), Closable(PageScreen(page))) {
                Ok(w) => r.open.push(w),
                Err(e) => eprintln!("measure leak: {e:?}"),
            }
        }
        r.pages = r.open.len();
        if r.pages == 0 {
            cx.quit(1);
        }
        return;
    }
    for w in r.open.drain(..) {
        cx.send(w, Box::new(CloseNow));
    }
    r.done += 1;
    // The counts are read one step after the close, once the windows are destroyed.
}

pub fn run(opts: &MeasureOpts) -> i32 {
    let cycles = opts.cycles.unwrap_or(20);
    let mut lp = match EventLoop::new(AppId("gallery"), Lang::En, ThemeSpec::oled()) {
        Ok(lp) => lp,
        Err(e) => {
            eprintln!("measure leak: {e:?}");
            return 1;
        }
    };
    let base = Blank(CommandTable::new());
    if let Err(e) = lp.open(spec("Gallery", SizeL { w: 320, h: 240 }, Caption::System, None), base) {
        eprintln!("measure leak: {e:?}");
        return 1;
    }
    let Some(timer) = Deadline::new().filter(|d| d.arm(STEP_MS, STEP_MS as i32)) else {
        eprintln!("measure leak: no timer");
        return 1;
    };
    let state = Rc::new(RefCell::new(Run { cycles, done: 0, open: Vec::new(), pages: 0, first: None, line: None }));
    let s = Rc::clone(&state);
    let tick = move |cx: &mut LoopCx| {
        let mut r = s.borrow_mut();
        // A tick after a close, with nothing open: the cycle's counts.
        if r.open.is_empty() && r.done > 0 && (r.done == 1 && r.first.is_none() || r.done == r.cycles) {
            let now = counts();
            match r.first {
                None => r.first = Some(now),
                Some(first) => {
                    r.line = Some(leak_line(r.cycles, r.pages, first, now));
                    cx.quit(0);
                    return;
                }
            }
            if r.cycles == 1 {
                r.line = Some(leak_line(1, r.pages, now, now));
                cx.quit(0);
                return;
            }
        }
        step(cx, &mut r);
    };
    if let Err(e) = lp.add_handle(timer.handle(), Box::new(tick)) {
        eprintln!("measure leak: {e:?}");
        return 1;
    }
    let code = lp.run();
    drop(timer);
    let got = state.borrow_mut().line.take();
    match got {
        Some(l) => emit(&l, None),
        None => code.max(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leak_line_reports_growth() {
        let v: serde_json::Value =
            serde_json::from_str(&leak_line(20, 3, (1_000_000, 40, 30), (1_200_000, 41, 30))).unwrap();
        assert_eq!((v["cycles"].as_u64(), v["pages"].as_u64()), (Some(20), Some(3)));
        assert_eq!(
            (v["private_delta"].as_i64(), v["gdi_delta"].as_i64(), v["user_delta"].as_i64()),
            (Some(200_000), Some(1), Some(0))
        );
    }
}
