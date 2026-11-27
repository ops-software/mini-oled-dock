//! --measure idle [--page <name>] --secs N (K1a.11): a page (default buttons) settles 5 s, then every MsgWait return and
//! every kit timer armed during N s are counted; one JSON line with the private bytes. Pass (LIVE-K1 step 2): 0 and 0.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use oled_ui::host::{AppId, Caption, EventLoop, LoopCx, LoopStats};
use oled_ui::strings::{Lang, UiLang};
use oled_ui::theme::ThemeSpec;

use super::empty::{emit, private_bytes, Deadline};
use crate::app::{spec, PageScreen};
use crate::cli::MeasureOpts;
use crate::pages;

/// The settling time before the count.
pub const SETTLE_MS: u64 = 5000;

/// The line of an idle run: the wake-ups and the kit timers armed during it, the kit timers still armed at its end.
pub fn idle_line(secs: u32, wakeups: u64, timers_armed: u64, timers_live: u32, private_bytes: u64) -> String {
    format!(
        "{{\"secs\":{secs},\"wakeups\":{wakeups},\"timers_armed\":{timers_armed},\"timers_live\":{timers_live},\"private_bytes\":{private_bytes}}}"
    )
}

/// What happened between two readings of the loop's counters; the measure's own timer wake (the second reading's) is not
/// counted.
pub fn between(start: LoopStats, end: LoopStats) -> (u64, u64) {
    (end.wakes.saturating_sub(start.wakes).saturating_sub(1), end.timers_armed.saturating_sub(start.timers_armed))
}

pub fn run(opts: &MeasureOpts) -> i32 {
    let secs = opts.secs.unwrap_or(60);
    let name = opts.page.as_deref().unwrap_or("buttons");
    let Some(page) = pages::by_name(name, UiLang::EN) else {
        eprintln!("measure idle: {name:?} is not a ready page");
        return 2;
    };
    let mut lp = match EventLoop::new(AppId("gallery"), Lang::En, ThemeSpec::oled()) {
        Ok(lp) => lp,
        Err(e) => {
            eprintln!("measure idle: {e:?}");
            return 1;
        }
    };
    let size = page.size;
    if let Err(e) = lp.open(spec("Gallery", size, Caption::System, None), PageScreen(page)) {
        eprintln!("measure idle: {e:?}");
        return 1;
    }
    let Some(timer) = Deadline::new().filter(|d| d.arm(SETTLE_MS, 0)).map(Rc::new) else {
        eprintln!("measure idle: no timer");
        return 1;
    };
    let line: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let (out, t) = (Rc::clone(&line), Rc::clone(&timer));
    let start: Cell<Option<LoopStats>> = Cell::new(None);
    let tick = move |cx: &mut LoopCx| match start.get() {
        None => {
            start.set(Some(cx.stats()));
            if !t.arm(u64::from(secs) * 1000, 0) {
                cx.quit(1);
            }
        }
        Some(s0) => {
            let s1 = cx.stats();
            let (wakeups, armed) = between(s0, s1);
            *out.borrow_mut() = Some(idle_line(secs, wakeups, armed, s1.timers_live, private_bytes()));
            cx.quit(0);
        }
    };
    if let Err(e) = lp.add_handle(timer.handle(), Box::new(tick)) {
        eprintln!("measure idle: {e:?}");
        return 1;
    }
    let code = lp.run();
    let got = line.borrow_mut().take();
    match got {
        Some(l) => emit(&l, opts.out.as_deref()),
        None => {
            eprintln!("measure idle: the window was closed before the end");
            code.max(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_counts_exclude_the_measure_timer() {
        let s0 = LoopStats { wakes: 10, timers_armed: 4, timers_live: 0 };
        let s1 = LoopStats { wakes: 11, timers_armed: 4, timers_live: 0 };
        assert_eq!(between(s0, s1), (0, 0), "only the end timer woke the loop");
        assert_eq!(between(s0, LoopStats { wakes: 14, timers_armed: 6, timers_live: 1 }), (3, 2));
        let v: serde_json::Value = serde_json::from_str(&idle_line(60, 0, 0, 0, 3_000_000)).unwrap();
        assert_eq!(
            (v["secs"].as_u64(), v["wakeups"].as_u64(), v["timers_armed"].as_u64()),
            (Some(60), Some(0), Some(0))
        );
        assert_eq!(v["private_bytes"].as_u64(), Some(3_000_000));
    }
}
