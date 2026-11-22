//! The owner-probe build (L0 and the live checks, SPEC-launcher Labels): switches and timings compiled in only with the
//! `owner-probe` cargo feature (lint 4), never in a release the owner installs. What the keyboard hook reads (L1a-10
//! step 7): the menu-mask key, the callback timer, the UI-freeze switch and the accept-injected switch (L0-2 driven by a
//! test agent with synthetic keys); what the chord flow reads (L1a-15e): the
//! DROP, FORCE_YIELD and MOTW_TEST switches; and the probe log lines of an open (numbers and exe file names only, lint 2),
//! written while `DOCK_LAUNCHER_PROBE=1`. Each switch is read once.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HWND;
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetMessageTime, GetWindowThreadProcessId};

use crate::log;

/// A switch that is on when its variable is "1", read once into `cell`.
fn switch(cell: &OnceLock<bool>, name: &str) -> bool {
    *cell.get_or_init(|| std::env::var(name).is_ok_and(|v| v.trim() == "1"))
}

/// `DOCK_LAUNCHER_PROBE=1`: the probe log lines below are written.
fn probe_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    switch(&ON, "DOCK_LAUNCHER_PROBE")
}

/// `DOCK_LAUNCHER_DROP=1`: the UI acknowledges every chord post and drops it, so nothing opens (S-ALTSPACE).
fn drop_posts() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    switch(&ON, "DOCK_LAUNCHER_DROP")
}

/// `DOCK_LAUNCHER_FORCE_YIELD=1`: the YIELD pass flag is always set, so every chord passes and is logged (S-PT).
pub fn force_yield() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    switch(&ON, "DOCK_LAUNCHER_FORCE_YIELD")
}

/// `DOCK_LAUNCHER_MOTW_TEST=1`: executable files may be opened (`motw_ok` of `verbs_for`), for live check 6 only.
#[allow(dead_code)] // the files provider passes it to verbs_for with its file rows (L2-1)
pub fn motw_test() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    switch(&ON, "DOCK_LAUNCHER_MOTW_TEST")
}

/// The chord flow, right after the acknowledgement: true when DROP is on (logged); the caller ends the post's window.
pub fn drop_post() -> bool {
    let drop = drop_posts();
    if drop && probe_on() {
        log!("launcher probe: post dropped (DROP)");
    }
    drop
}

/// At most 64 values of one open.
#[derive(Clone, Copy)]
pub struct Samples {
    v: [u32; 64],
    n: usize,
}

impl Samples {
    pub const fn new() -> Samples {
        Samples { v: [0; 64], n: 0 }
    }

    /// One more value; beyond 64 they are dropped.
    pub fn push(&mut self, x: u32) {
        if let Some(slot) = self.v.get_mut(self.n) {
            *slot = x;
            self.n += 1;
        }
    }

    pub fn len(&self) -> usize {
        self.n
    }

    /// p50, p95 and the maximum, by nearest rank; None when nothing was measured.
    pub fn percentiles(&self) -> Option<(u32, u32, u32)> {
        let mut s = self.v;
        let s = s.get_mut(..self.len())?;
        s.sort_unstable();
        let rank = |p: usize| s.get((p * s.len()).div_ceil(100).saturating_sub(1)).copied();
        Some((rank(50)?, rank(95)?, *s.last()?))
    }
}

/// What the probe measures during one open.
struct OpenProbe {
    /// `kb.time` of the chord.
    chord_time: u32,
    focused: bool,
    present_ms: Option<u32>,
    /// The split of `present_ms` (T22, live check L-3): the post's wait from the chord's `kb.time` to the chord flow (ms,
    /// on the same clock), then `prepare_open`, the window (the host's open up to its first render: the window, the
    /// surface and the content) and that render (us).
    queue_ms: u32,
    prepare_us: u32,
    window_us: u32,
    render_us: u32,
    /// The end of the last step measured.
    mark: Instant,
    keys: u32,
    /// From the entry of a key message to the return of `UpdateLayeredWindow` (us), and its wait in the queue (ms).
    work: Samples,
    queue: Samples,
    /// `HOOK_US` when the launcher opened.
    hook_base: [u32; 64],
}

thread_local! {
    static OPEN: RefCell<Option<OpenProbe>> = const { RefCell::new(None) };
    /// The 400 ms watch after a passed (yield) or opened chord: its kb.time.
    static WATCH: Cell<Option<u32>> = const { Cell::new(None) };
}

fn now() -> u32 {
    // SAFETY: no preconditions.
    unsafe { GetTickCount() }
}

fn with_open<R>(f: impl FnOnce(&mut OpenProbe) -> R) -> Option<R> {
    OPEN.with(|o| o.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

/// The chord flow is opening the launcher for the chord pressed at `chord_time`.
pub fn opened(chord_time: u32) {
    if !probe_on() {
        return;
    }
    let mut hook_base = [0u32; 64];
    for (b, h) in hook_base.iter_mut().zip(HOOK_US.iter()) {
        *b = h.load(Relaxed);
    }
    let p = OpenProbe {
        chord_time,
        focused: false,
        present_ms: None,
        queue_ms: now().wrapping_sub(chord_time),
        prepare_us: 0,
        window_us: 0,
        render_us: 0,
        mark: Instant::now(),
        keys: 0,
        work: Samples::new(),
        queue: Samples::new(),
        hook_base,
    };
    OPEN.with(|o| {
        if let Ok(mut g) = o.try_borrow_mut() {
            *g = Some(p);
        }
    });
    WATCH.with(|w| w.set(Some(chord_time)));
}

/// The launcher could not open: nothing of this open is measured.
pub fn open_failed() {
    OPEN.with(|o| {
        if let Ok(mut g) = o.try_borrow_mut() {
            *g = None;
        }
    });
}

/// The microseconds since `mark`, which moves to now.
fn lap(mark: &mut Instant) -> u32 {
    let t = Instant::now();
    let us = u32::try_from(t.duration_since(*mark).as_micros()).unwrap_or(u32::MAX);
    *mark = t;
    us
}

/// The chord flow's `prepare_open` returned.
pub fn prepared() {
    with_open(|p| p.prepare_us = lap(&mut p.mark));
}

/// `popup::open` is about to render the first frame of a popup; `launcher`: it is the launcher's.
pub fn render_start(launcher: bool) {
    if launcher {
        with_open(|p| {
            if p.present_ms.is_none() {
                p.window_us = lap(&mut p.mark);
            }
        });
    }
}

/// `popup::open` presented the first frame of a popup; `launcher`: it was the launcher's.
pub fn presented(launcher: bool) {
    if launcher {
        let t = now();
        with_open(|p| {
            if p.present_ms.is_none() {
                p.render_us = lap(&mut p.mark);
                p.present_ms = Some(t.wrapping_sub(p.chord_time));
            }
        });
    }
}

/// The launcher's first activation of this open.
pub fn focused() {
    let t = now();
    let first = with_open(|p| (!std::mem::replace(&mut p.focused, true)).then(|| t.wrapping_sub(p.chord_time)));
    if let Some(Some(ms)) = first {
        log!("launcher probe: chord to foreground {ms} ms");
    }
}

/// `popup::mark_active` (WM_ACTIVATE): `in_front` false is an activation in the dock's thread while another window kept the
/// foreground (variant 1 of `ui::force_foreground` on the synthetic-F15 path, B1). Logged while a launcher open is
/// measured.
pub fn activated(in_front: bool) {
    if !in_front && with_open(|_| ()).is_some() {
        log!("launcher probe: activated in its thread, foreground elsewhere");
    }
}

/// `popup::open`: the foreground attempt won (`Retry::Focused`), and whether `hwnd` is in front right after it, before
/// the content hears `focus(true)` (B1, live check L-2). Logged while a launcher open is measured.
pub fn won_foreground(hwnd: HWND) {
    if with_open(|_| ()).is_none() {
        return;
    }
    // SAFETY: no preconditions.
    if unsafe { GetForegroundWindow() } == hwnd {
        log!("launcher probe: foreground attempt won, launcher in front");
    } else {
        log!("launcher probe: foreground attempt won, launcher not in front");
    }
}

/// The entry of a `WM_KEYDOWN` or `WM_CHAR` handling: its start and its wait in the queue, while a launcher is open.
pub fn work_start() -> Option<(Instant, u32)> {
    with_open(|_| ())?;
    // SAFETY: no preconditions; the time of the message being handled.
    let queued = now().wrapping_sub(unsafe { GetMessageTime() } as u32);
    Some((Instant::now(), queued))
}

/// The handling of that key message ended (its frame presented).
pub fn work_end(start: Option<(Instant, u32)>) {
    let Some((t0, queued)) = start else { return };
    let us = u32::try_from(t0.elapsed().as_micros()).unwrap_or(u32::MAX);
    with_open(|p| {
        p.keys += 1;
        p.work.push(us);
        p.queue.push(queued);
    });
}

/// The keyboard hook's p99 callback time (us, its bucket) and the events counted since `base`.
fn hook_p99(base: &[u32; 64]) -> Option<(u32, u32)> {
    let mut delta = [0u32; 64];
    for ((d, b), h) in delta.iter_mut().zip(base).zip(HOOK_US.iter()) {
        *d = h.load(Relaxed).wrapping_sub(*b);
    }
    let count: u32 = delta.iter().sum();
    let want = (u64::from(count) * 99).div_ceil(100);
    let mut seen = 0u64;
    for (us, d) in (0u32..).zip(delta) {
        seen += u64::from(d);
        if count > 0 && seen >= want {
            return Some((us, count));
        }
    }
    None
}

/// The launcher closed: the open's numbers.
pub fn closed() {
    let Some(p) = OPEN.with(|o| o.try_borrow_mut().ok().and_then(|mut g| g.take())) else { return };
    let n = p.keys;
    log!("launcher probe: session {n} keys");
    if let Some((p50, p95, max)) = p.work.percentiles() {
        let us = p50;
        log!("launcher probe: work p50 {us} us");
        let us = p95;
        log!("launcher probe: work p95 {us} us");
        let us = max;
        log!("launcher probe: work max {us} us");
    }
    if let Some((_, ms, _)) = p.queue.percentiles() {
        log!("launcher probe: queue p95 {ms} ms");
    }
    if let Some(ms) = p.present_ms {
        let (queue, prepare, window, render) = (p.queue_ms, p.prepare_us, p.window_us, p.render_us);
        log!(
            "launcher probe: chord to present {ms} ms (queue {queue} ms, prepare {prepare} us, window {window} us, render {render} us)"
        );
    }
    if let Some((us, count)) = hook_p99(&p.hook_base) {
        log!("launcher probe: hook p99 {us} us over {count} events");
    }
}

/// MEM-2's budget for the icons the launcher keeps between opens: the cache's byte cap (L3 T20).
const ICON_CACHE_BUDGET: usize = crate::launcher::catalog::ICON_CACHE_BYTES;

/// At each close: the pixel bytes of the icons kept between opens (live check L-4's fill cycle).
pub fn icon_cache(bytes: usize) {
    if probe_on() {
        let n = ICON_CACHE_BUDGET;
        log!("launcher probe: icon cache {bytes} B of {n}");
    }
}

/// The launcher's UI-thread handlers that KEY-11 bounds: none may take more than `UI_WORK_US` outside a key.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UiWork {
    Catalog,
    SettingsReady,
    PrepareOpen,
    Icons,
}

/// KEY-11: 2 ms.
const UI_WORK_US: u32 = 2_000;

/// A handler that took `us` is logged.
fn ui_work_over(us: u32) -> bool {
    us > UI_WORK_US
}

/// Times one handler from `ui_work` to its drop (every return of the handler).
pub struct UiTimer {
    what: UiWork,
    t0: Instant,
}

/// The start of a handler of `what`; the line `launcher probe: ui work <name> N us` comes at the drop when it took more
/// than 2,000 us.
pub fn ui_work(what: UiWork) -> UiTimer {
    UiTimer { what, t0: Instant::now() }
}

impl Drop for UiTimer {
    fn drop(&mut self) {
        let us = u32::try_from(self.t0.elapsed().as_micros()).unwrap_or(u32::MAX);
        if !ui_work_over(us) || !probe_on() {
            return;
        }
        match self.what {
            UiWork::Catalog => log!("launcher probe: ui work on_catalog {us} us"),
            UiWork::SettingsReady => log!("launcher probe: ui work on_settings_ready {us} us"),
            UiWork::PrepareOpen => log!("launcher probe: ui work prepare_open {us} us"),
            UiWork::Icons => log!("launcher probe: ui work on_icons {us} us"),
        }
    }
}

/// `DOCK_LAUNCHER_NO_WARM=1`: the warm timer skips the window warm (`popup_launcher::warm_window`) and nothing else, for
/// the two series of live check L-3 (c). Read once; each skip is logged.
pub fn no_warm() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    let on = switch(&ON, "DOCK_LAUNCHER_NO_WARM");
    if on && probe_on() {
        log!("launcher probe: window warm skipped (NO_WARM)");
    }
    on
}

/// The warm timer's window warm, started at `t0`, is done.
pub fn warmed(t0: Instant) {
    if probe_on() {
        let us = u32::try_from(t0.elapsed().as_micros()).unwrap_or(u32::MAX);
        log!("launcher probe: window warm {us} us");
    }
}

/// `WM_DOCK_LAUNCHER_YIELDED` while a chord is set: the chord pressed at `chord_time` passed under YIELD.
pub fn yielded(chord_time: Option<u32>) {
    let Some(chord_time) = chord_time else { return };
    if probe_on() {
        log!("launcher probe: chord passed (yield)");
        WATCH.with(|w| w.set(Some(chord_time)));
    }
}

/// A foreground change: within 400 ms of a watched chord, the image that took the foreground and when.
pub fn foreground(fg: HWND) {
    let Some(chord_time) = WATCH.with(Cell::get) else { return };
    let ms = now().wrapping_sub(chord_time);
    if ms > 400 {
        WATCH.with(|w| w.set(None));
        return;
    }
    let mut pid = 0u32;
    // SAFETY: `pid` is writable; a window that is gone leaves it 0.
    unsafe { GetWindowThreadProcessId(fg, Some(&mut pid)) };
    let path = if pid == 0 { None } else { crate::apps::exe_path_of_pid(pid) };
    let image = path.as_deref().and_then(|p| p.rsplit(['\\', '/']).next()).unwrap_or("?");
    log!("launcher probe: foreground {image} after {ms} ms");
}

/// Ctrl+Shift+Enter: the hook's kb.time and the message's time (S-GESTURE: they must be equal).
pub fn gesture(hook_time: Option<u32>, m: u32) {
    if !probe_on() {
        return;
    }
    match hook_time {
        Some(t) => log!("launcher probe: gesture hook {t} message {m}"),
        None => log!("launcher probe: gesture hook none message {m}"),
    }
}

/// `arm_once` armed one of the launcher's timers (`armed`: SetTimer accepted it).
pub fn timer_armed(id: usize, ms: u32, armed: bool) {
    if armed && probe_on() {
        log!("launcher probe: timer {id:?} armed {ms} ms");
    }
}

/// `DOCK_LAUNCHER_ACCEPT_INJECTED=1`: the keyboard hook treats keys injected by other apps (LLKHF_INJECTED without the
/// dock's own INJECT_TAG) as physical, for everything it does with them: the Win key, the launcher chord, the type-ahead
/// and the gesture evidence. It exists so a test agent can drive the L0 session (L0-2) with SendInput while the owner is
/// away; events tagged INJECT_TAG still pass through, or the hook would loop on its own re-injections. `hook::start` reads
/// it once into an atomic that the callback only loads. Results obtained with synthetic keys must be labelled as such:
/// they show what the dock does with SendInput, not with a person's key presses.
pub fn accept_injected() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let on = std::env::var("DOCK_LAUNCHER_ACCEPT_INJECTED").is_ok_and(|v| v.trim() == "1");
        if on {
            log!("launcher probe: injected keys count as physical (synthetic-key run, label its results)");
        }
        on
    })
}

/// `DOCK_LAUNCHER_MASK`: `e8` -> 0xE8, `ctrl` -> 0xA2 (left Ctrl), `none` -> 0; unset or anything else -> 0xE8.
pub fn parse_mask(v: &str) -> u32 {
    match v {
        "ctrl" => 0xA2,
        "none" => 0,
        _ => 0xE8,
    }
}

/// The menu-mask key the hook injects under Alt when it swallows the chord (S-ALTSPACE, L0-2 step 4).
pub fn mask_key() -> u32 {
    static MASK: OnceLock<u32> = OnceLock::new();
    *MASK.get_or_init(|| parse_mask(std::env::var("DOCK_LAUNCHER_MASK").as_deref().unwrap_or("")))
}

/// The keyboard hook callback's own duration: one bucket per microsecond, the last for 63 us and more.
pub static HOOK_US: [AtomicU32; 64] = [const { AtomicU32::new(0) }; 64];

/// Counts one callback that took `d`.
pub fn record_hook_us(d: Duration) {
    let us = usize::try_from(d.as_micros()).unwrap_or(usize::MAX).min(63);
    if let Some(bucket) = HOOK_US.get(us) {
        bucket.fetch_add(1, Relaxed);
    }
}

/// `DOCK_TEST_FREEZE_UI_MS`: the first launcher acknowledgement stalls the UI thread this long (the frozen-UI case of
/// live check 7), the launcher's own copy of WS-C 17.1 step 6, whose helper exists only in debug builds.
pub fn freeze_ui_once() {
    static FREEZE_MS: OnceLock<Option<u64>> = OnceLock::new();
    static FROZE: AtomicBool = AtomicBool::new(false);
    let ms =
        *FREEZE_MS.get_or_init(|| std::env::var("DOCK_TEST_FREEZE_UI_MS").ok().and_then(|v| v.trim().parse().ok()));
    if let Some(ms) = ms {
        if !FROZE.swap(true, Relaxed) {
            log!("TEST: freezing the UI thread for {ms} ms");
            std::thread::sleep(Duration::from_millis(ms));
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn mask_key_values() {
        assert_eq!(parse_mask("e8"), 0xE8);
        assert_eq!(parse_mask("ctrl"), 0xA2);
        assert_eq!(parse_mask("none"), 0);
        assert_eq!(parse_mask(""), 0xE8);
        assert_eq!(parse_mask("x"), 0xE8);
    }

    #[test]
    fn percentiles_of_samples() {
        let mut s = Samples::new();
        assert_eq!(s.percentiles(), None, "nothing measured");
        for v in 1..=20 {
            s.push(v);
        }
        assert_eq!(s.percentiles(), Some((10, 19, 20)), "nearest rank: p50, p95 and max");
        let mut one = Samples::new();
        one.push(7);
        assert_eq!(one.percentiles(), Some((7, 7, 7)));
        // The order they came in does not matter.
        let mut rev = Samples::new();
        for v in (1..=20).rev() {
            rev.push(v);
        }
        assert_eq!(rev.percentiles(), Some((10, 19, 20)));
    }

    #[test]
    fn samples_cap_at_64() {
        let mut s = Samples::new();
        for v in 0..100 {
            s.push(v);
        }
        assert_eq!(s.len(), 64, "at most 64 values per open");
        assert_eq!(s.percentiles().map(|p| p.2), Some(63), "the first 64 are kept");
    }

    #[test]
    fn ui_work_over_two_ms_is_logged() {
        assert!(!ui_work_over(0));
        assert!(!ui_work_over(2_000), "KEY-11: 2 ms is within the budget");
        assert!(ui_work_over(2_001));
        assert!(ui_work_over(u32::MAX));
    }

    #[test]
    fn the_split_laps_from_the_last_mark() {
        // No timing is asserted: a mark 5 ms in the past gives at least 5,000 us, and the mark moves to now.
        let start = Instant::now() - Duration::from_millis(5);
        let mut mark = start;
        assert!(lap(&mut mark) >= 5_000);
        assert!(mark > start, "the next step is measured from here");
    }

    #[test]
    fn hook_time_buckets() {
        let before = HOOK_US[63].load(Relaxed);
        record_hook_us(Duration::from_secs(1));
        assert_eq!(HOOK_US[63].load(Relaxed), before + 1, "63 us and more");
        let b = HOOK_US[5].load(Relaxed);
        record_hook_us(Duration::from_nanos(5_900));
        assert_eq!(HOOK_US[5].load(Relaxed), b + 1);
    }
}
