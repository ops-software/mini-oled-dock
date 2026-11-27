//! The text system's one-time start off the UI thread. The first line GDI draws in a process loads and starts the
//! shaping engine, a cost of milliseconds paid once per process (later lines, in any font and on any thread, cost a
//! fraction of it). A window's first paint draws text, so when the first window opens a short-lived worker thread draws
//! one line through an engine of its own while the UI thread creates and shows the window: the first paint finds the
//! engine started. The worker's fonts, DCs and DIB are freed with its engine; it arms no timer and leaves nothing
//! behind. A process that draws no text turns it off before its first window (skip): it then loads no text system.

use std::sync::atomic::{AtomicBool, Ordering};

use super::{GdiTextEngine, LayoutOpts, TextEngine, Trim};
use crate::geom::Scale;
use crate::theme::TypeRole;

/// The warm-up was started, or turned off, in this process.
static STARTED: AtomicBool = AtomicBool::new(false);

/// Starts the warm-up on a worker thread, once per process; false when it was started or turned off before, or no
/// thread could be made.
pub(crate) fn start() -> bool {
    start_once(&STARTED, spawn)
}

/// No warm-up in this process: a later start() makes no thread.
pub(crate) fn skip() {
    STARTED.store(true, Ordering::Relaxed);
}

/// Runs `spawn` if `started` was false (and sets it); false when it was true or `spawn` failed.
fn start_once(started: &AtomicBool, spawn: impl FnOnce() -> bool) -> bool {
    if started.swap(true, Ordering::Relaxed) {
        return false;
    }
    spawn()
}

/// The worker thread; false when none could be made.
fn spawn() -> bool {
    std::thread::Builder::new().name("oled-text-warm-up".into()).spawn(run).is_ok()
}

/// Lays out (and so rasterises) one line in the body font; true when it drew something.
pub(crate) fn run() -> bool {
    let Ok(mut engine) = GdiTextEngine::new() else { return false };
    let font = engine.font(TypeRole::Body, None, Scale { dpi: 96 }, 1.0);
    let line = engine.layout("x", font, &LayoutOpts { max_width: None, trim: Trim::None, wrap: false });
    line.width > 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{AppId, EventLoop};
    use crate::strings::Lang;
    use crate::theme::ThemeSpec;

    #[test]
    fn the_warm_up_draws_one_line_and_starts_once() {
        assert!(run(), "one line laid out, so rasterised");
        let started = AtomicBool::new(false);
        assert!(start_once(&started, spawn), "the first window starts it");
        assert!(!start_once(&started, || panic!("a second worker")), "once per process");
    }

    #[test]
    fn a_loop_that_turns_it_off_starts_no_worker() {
        let mut lp = EventLoop::new(AppId("test"), Lang::En, ThemeSpec::oled()).unwrap();
        lp.no_text_warm_up();
        assert!(!start_once(&STARTED, || panic!("a worker after the opt-out")), "turned off: no thread is made");
        assert!(!start(), "the first window starts nothing");
    }
}
