//! --measure theme (K1a.11): the buttons page; ThemeSpec::oled() and the same spec with seed.bg Rgb(0x1C, 0x1C, 0x1E)
//! alternate 20 times; the time from set_theme to the end of the next paint, its p95 and its max. Pass (LIVE-K1 step 6):
//! p95 <= 50 ms.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Instant;

use oled_ui::command::CommandTable;
use oled_ui::cx::{EventCx, LayoutCx, PaintCx};
use oled_ui::event::{Event, Reply};
use oled_ui::geom::SizeP;
use oled_ui::host::{AppId, Caption, EventLoop, LoopCx};
use oled_ui::screen::Screen;
use oled_ui::strings::{Lang, UiLang};
use oled_ui::theme::{Rgb, ThemeSpec};

use super::empty::{emit, Deadline};
use crate::app::{spec, PageScreen};
use crate::cli::MeasureOpts;
use crate::pages;

/// The changes measured.
pub const CHANGES: usize = 20;
/// One change every this many ms.
pub const STEP_MS: u64 = 150;

/// The nearest-rank 95th percentile (0 for no sample).
pub fn p95(samples: &[f64]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let mut v = samples.to_vec();
    v.sort_by(f64::total_cmp);
    let rank = (0.95 * v.len() as f64).ceil() as usize;
    v[rank.clamp(1, v.len()) - 1]
}

/// The line of a theme run.
pub fn theme_line(samples: &[f64]) -> String {
    let max = samples.iter().copied().fold(0.0, f64::max);
    format!("{{\"samples\":{},\"p95_ms\":{:.2},\"max_ms\":{max:.2}}}", samples.len(), p95(samples))
}

/// The spec of change `i`: the OLED one, then the grey one, in turn.
pub fn spec_of(i: usize) -> ThemeSpec {
    let mut s = ThemeSpec::oled();
    if i.is_multiple_of(2) {
        s.look.seed.bg = Rgb(0x1C, 0x1C, 0x1E);
    }
    s
}

/// The page, noting when a paint reaches the client's bottom (the end of a full repaint).
struct Timed {
    page: PageScreen,
    height: i32,
    painted: Rc<Cell<Option<Instant>>>,
}

impl Screen for Timed {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        self.height = avail.h;
        self.page.layout(cx, avail)
    }

    fn paint(&self, cx: &mut PaintCx) {
        self.page.paint(cx);
        if cx.clip().b >= self.height {
            self.painted.set(Some(Instant::now()));
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        self.page.event(cx, ev)
    }

    fn commands(&self) -> &CommandTable {
        self.page.commands()
    }
}

pub fn run(opts: &MeasureOpts) -> i32 {
    let _ = opts;
    let Some(page) = pages::by_name("buttons", UiLang::EN) else {
        eprintln!("measure theme: the buttons page is not ready");
        return 1;
    };
    let mut lp = match EventLoop::new(AppId("gallery"), Lang::En, ThemeSpec::oled()) {
        Ok(lp) => lp,
        Err(e) => {
            eprintln!("measure theme: {e:?}");
            return 1;
        }
    };
    let painted = Rc::new(Cell::new(None));
    let size = page.size;
    let screen = Timed { page: PageScreen(page), height: 0, painted: Rc::clone(&painted) };
    if let Err(e) = lp.open(spec("Gallery", size, Caption::System, None), screen) {
        eprintln!("measure theme: {e:?}");
        return 1;
    }
    let Some(timer) = Deadline::new().filter(|d| d.arm(STEP_MS, STEP_MS as i32)) else {
        eprintln!("measure theme: no timer");
        return 1;
    };
    let samples: Rc<Cell<Vec<f64>>> = Rc::new(Cell::new(Vec::new()));
    let (out, mut changed, mut i) = (Rc::clone(&samples), None::<Instant>, 0usize);
    let tick = move |cx: &mut LoopCx| {
        let mut v = out.take();
        if let (Some(t0), Some(p)) = (changed, painted.get()) {
            if p >= t0 {
                v.push(p.duration_since(t0).as_secs_f64() * 1000.0);
            }
        }
        let finished = v.len() >= CHANGES || i >= 2 * CHANGES;
        out.set(v);
        if finished {
            cx.quit(0);
            return;
        }
        painted.set(None);
        changed = Some(Instant::now());
        cx.set_theme(spec_of(i));
        i += 1;
    };
    if let Err(e) = lp.add_handle(timer.handle(), Box::new(tick)) {
        eprintln!("measure theme: {e:?}");
        return 1;
    }
    let code = lp.run();
    drop(timer);
    let v = samples.take();
    if v.is_empty() {
        return code.max(1);
    }
    emit(&theme_line(&v), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_line_reports_p95() {
        let v: Vec<f64> = (1..=20).map(f64::from).collect();
        assert_eq!(p95(&v), 19.0, "nearest rank: the 19th of 20");
        assert_eq!(p95(&[]), 0.0);
        assert_eq!(p95(&[7.0]), 7.0);
        let line: serde_json::Value = serde_json::from_str(&theme_line(&v)).unwrap();
        assert_eq!(
            (line["samples"].as_u64(), line["p95_ms"].as_f64(), line["max_ms"].as_f64()),
            (Some(20), Some(19.0), Some(20.0))
        );
        assert_ne!(spec_of(0), spec_of(1), "the two specs alternate");
        assert_eq!(spec_of(1), ThemeSpec::oled());
    }
}
