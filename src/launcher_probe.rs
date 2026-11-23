//! The owner-probe build (L0 and the live checks, SPEC-launcher Labels): switches and timings compiled in only with the
//! `owner-probe` cargo feature (lint 4), never in a release the owner installs. What the keyboard hook reads (L1a-10
//! step 7): the menu-mask key, the callback timer, the UI-freeze switch and the accept-injected switch (L0-2 driven by a
//! test agent with synthetic keys); what the chord flow reads (L1a-15e): the
//! DROP, FORCE_YIELD and MOTW_TEST switches; and the probe log lines of an open (numbers and exe file names only, lint 2),
//! written while `DOCK_LAUNCHER_PROBE=1`. Each switch is read once.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Acquire, Ordering::Relaxed, Ordering::Release};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetMessageTime, GetWindowThreadProcessId, WM_KEYDOWN, WM_SYSKEYDOWN,
};

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

/// `DOCK_LAUNCHER_MOTW_TEST=1`: executable files may be opened (`motw_ok` of `verbs_for`), for live check 6 only: the
/// files pump gives it to the verbs of its file rows (`launcher_files::probe_verbs`).
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
    /// From the entry of a key message to the return of `UpdateLayeredWindow` (us), and its wait in the queue (us).
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
/// The wait is read in microseconds from two QueryPerformanceCounter stamps (the tick clock moves in 15.6 ms steps, which
/// cannot judge KEY-1's 4 ms): the keyboard hook's stamp of this message's own key (`key_seen`), found by the message's
/// time however many keys wait behind it, and this one. No key-down of that time in the ring: no sample.
pub fn work_start() -> Option<(Instant, u32)> {
    with_open(|_| ())?;
    // SAFETY: no preconditions; the time of the message being handled, on the clock of the hook's `kb.time`.
    let time = unsafe { GetMessageTime() } as u32;
    let queued = queue_us(qpc(), KEYS.stamp_of(time), qpc_freq());
    Some((Instant::now(), queued))
}

/// The key-downs `KeyStamps` keeps: the keys the UI may still have queued behind a stall.
const KEY_SLOTS: usize = 32;

/// The keyboard hook's last key-downs, each slot `kb.time << 32 | the counter's low 32 bits` (0 = empty). The handling of
/// a key message finds its own key by the message's time (S-GESTURE: the `GetMessageTime` of a key message, and of the
/// WM_CHAR its WM_KEYDOWN posted, equals the hook's `kb.time`). One atomic per slot, so a slot is always read whole: never
/// one key's time with another key's stamp. Written by the hook thread only, read by the UI thread: no lock, no
/// allocation, no wait.
struct KeyStamps {
    slots: [AtomicU64; KEY_SLOTS],
    /// The key-downs recorded (wrapping): the next one goes in slot `next % KEY_SLOTS`.
    next: AtomicU32,
}

impl KeyStamps {
    const fn new() -> KeyStamps {
        KeyStamps { slots: [const { AtomicU64::new(0) }; KEY_SLOTS], next: AtomicU32::new(0) }
    }

    /// The hook saw the key-down of `time` at counter `stamp`.
    fn record(&self, time: u32, stamp: i64) {
        let n = self.next.fetch_add(1, Relaxed);
        if let Some(slot) = self.slots.get(n as usize % KEY_SLOTS) {
            slot.store((u64::from(time) << 32) | u64::from(stamp as u32), Release);
        }
    }

    /// The counter's low 32 bits at the newest key-down of `time`; None when no slot holds one.
    fn stamp_of(&self, time: u32) -> Option<u32> {
        let n = self.next.load(Relaxed);
        (1..=KEY_SLOTS as u32)
            .filter_map(|back| self.slots.get(n.wrapping_sub(back) as usize % KEY_SLOTS))
            .map(|slot| slot.load(Acquire))
            .find(|&v| v != 0 && (v >> 32) as u32 == time)
            .map(|v| v as u32)
    }
}

static KEYS: KeyStamps = KeyStamps::new();

fn qpc() -> i64 {
    let mut t = 0i64;
    // SAFETY: `t` is writable; the call cannot fail on Windows XP and later.
    let _ = unsafe { QueryPerformanceCounter(&mut t) };
    t
}

/// The counter's frequency (ticks per second; 0 when the clock does not answer).
fn qpc_freq() -> i64 {
    static FREQ: OnceLock<i64> = OnceLock::new();
    *FREQ.get_or_init(|| {
        let mut f = 0i64;
        // SAFETY: `f` is writable.
        let _ = unsafe { QueryPerformanceFrequency(&mut f) };
        f
    })
}

/// The wait in microseconds from the key's stamp (the counter's low 32 bits) to `now` (0 when no stamp matched, 0 for a
/// bad frequency). The low bits wrap after 2^32 ticks: over 5 minutes at the 10 MHz counter of Windows 11.
fn queue_us(now: i64, stamp: Option<u32>, freq: i64) -> u32 {
    let Some(stamp) = stamp else { return 0 };
    ticks_to_us(i64::from((now as u32).wrapping_sub(stamp)), freq)
}

/// `ticks` of the counter in microseconds (0 for a bad frequency or a negative count).
fn ticks_to_us(ticks: i64, freq: i64) -> u32 {
    if freq <= 0 || ticks <= 0 {
        return 0;
    }
    u32::try_from(i128::from(ticks) * 1_000_000 / i128::from(freq)).unwrap_or(u32::MAX)
}

/// The keyboard hook sees event `msg` of `time` (`kb.time`): a key-down gives the first stamp of the queue measure.
pub fn key_seen(msg: u32, time: u32) {
    if msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN {
        KEYS.record(time, qpc());
    }
}

/// The handling of that key message ended (its frame presented).
pub fn work_end(start: Option<(Instant, u32)>) {
    let Some((t0, queued)) = start else { return };
    let us = u32::try_from(t0.elapsed().as_micros()).unwrap_or(u32::MAX);
    with_open(|p| {
        p.keys += 1;
        p.work.push(us);
        if queued > 0 {
            p.queue.push(queued); // 0 = no stamp of this key (none matched its message time): no sample
        }
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
    if let Some((p50, p95, max)) = p.queue.percentiles() {
        let us = p50;
        log!("launcher probe: queue p50 {us} us");
        let us = p95;
        log!("launcher probe: queue p95 {us} us");
        let us = max;
        log!("launcher probe: queue max {us} us");
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
    /// A files post merged (`popup_launcher::on_files`, FILES-2).
    Files,
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
            UiWork::Files => log!("launcher probe: ui work on_files {us} us"),
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
    let image = image_of(fg);
    log!("launcher probe: foreground {image} after {ms} ms");
}

/// The exe file name of the process that owns `h` ("?" when it cannot be read); never a window title.
fn image_of(h: HWND) -> String {
    let mut pid = 0u32;
    // SAFETY: `pid` is writable; a window that is gone leaves it 0.
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    let path = if pid == 0 { None } else { crate::apps::exe_path_of_pid(pid) };
    path.as_deref().and_then(|p| p.rsplit(['\\', '/']).next()).unwrap_or("?").to_string()
}

/// Right before Win + `vk` is sent: for the Clipboard history command (Win+V) while the probe writes, the image of the
/// window in front, which the restore gave the foreground back to (`win_key_sent` logs it).
pub fn before_win_key(vk: u16) -> Option<String> {
    // SAFETY: no preconditions.
    (vk == 0x56 && probe_on()).then(|| image_of(unsafe { GetForegroundWindow() }))
}

/// After the Clipboard history command's Win+V (live check V-10): the image `before_win_key` read.
pub fn win_key_sent(image: Option<String>) {
    if let Some(image) = image {
        log!("launcher probe: clipboard history injected, foreground {image} restored");
    }
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
    fn counter_ticks_read_in_microseconds() {
        assert_eq!(ticks_to_us(10_000_000, 10_000_000), 1_000_000);
        assert_eq!(ticks_to_us(2_500, 10_000_000), 250, "a 4 ms budget is judged to the microsecond");
        assert_eq!(ticks_to_us(0, 10_000_000), 0);
        assert_eq!(ticks_to_us(-5, 10_000_000), 0);
        assert_eq!(ticks_to_us(5, 0), 0, "no clock");
        assert_eq!(ticks_to_us(i64::MAX, 1), u32::MAX);
    }

    #[test]
    fn an_unset_key_stamp_gives_no_wait() {
        assert_eq!(queue_us(9_000_000_000, None, 10_000_000), 0, "never the whole uptime");
        assert_eq!(queue_us(1_002_500, Some(1_000_000), 10_000_000), 250);
        assert_eq!(queue_us(0x1_0000_0100, Some(0xFFFF_FF00), 10_000_000), 51, "the low 32 bits wrap");
        assert_eq!(KeyStamps::new().stamp_of(0), None, "an empty slot is no key of time 0");
    }

    #[test]
    fn a_key_message_reads_its_own_key_stamp() {
        let keys = KeyStamps::new();
        keys.record(1_000, 10_000_000); // the first key, at 1 s on a 10 MHz counter
        keys.record(1_060, 10_600_000); // the next one, 60 ms later, while the first still waits
        assert_eq!(keys.stamp_of(1_000), Some(10_000_000), "the first key's message reads its own stamp");
        assert_eq!(queue_us(11_210_000, keys.stamp_of(1_000), 10_000_000), 121_000, "the whole 121 ms wait");
        assert_eq!(queue_us(11_210_000, keys.stamp_of(1_060), 10_000_000), 61_000);
        assert_eq!(keys.stamp_of(2_000), None, "a time no key-down had: no sample");
        keys.record(1_060, 10_700_000);
        assert_eq!(keys.stamp_of(1_060), Some(10_700_000), "the newest key-down of that time");
        for k in 0..KEY_SLOTS as u32 {
            keys.record(5_000 + k, 20_000_000 + i64::from(k));
        }
        assert_eq!(keys.stamp_of(1_000), None, "overwritten: no sample rather than another key's stamp");
        assert_eq!(keys.stamp_of(5_000), Some(20_000_000), "the oldest key still kept");
    }

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

// ---------------------------------------------------------------- files (plan L2-F3)

/// `DOCK_LAUNCHER_FILES_STALL_MS=<ms>`: the files helper sleeps that long before each query (`--stall`, live check V-4).
/// Read once.
pub fn files_stall_ms() -> Option<u32> {
    static MS: OnceLock<Option<u32>> = OnceLock::new();
    *MS.get_or_init(|| std::env::var("DOCK_LAUNCHER_FILES_STALL_MS").ok().and_then(|v| v.trim().parse().ok()))
}

/// The pump wrote the request of `gen`, `ms` after it came (its quiet time and any wait for the helper).
pub fn files_written(gen: u64, ms: u64) {
    if probe_on() {
        log!("launcher probe: files request gen {gen} written after {ms} ms quiet");
    }
}

/// A helper's state frame said 0, `ms` after its spawn; its private bytes then (FILES-4).
pub fn files_state(ms: u64, private: Option<usize>) {
    if probe_on() {
        log!("launcher probe: files spawn to state {ms} ms");
        if let Some(bytes) = private {
            log!("launcher probe: files helper private {bytes} B at state 0");
        }
    }
}

/// The rows frame of `gen`: `n` rows `ms` after the write, `m` the helper's spawn-to-state time when this was its first
/// request (else 0), and its private bytes after its first rows (FILES-1, FILES-4). Logged for dropped generations too.
pub fn files_rows(gen: u64, n: usize, ms: u64, m: u64, private: Option<usize>) {
    if probe_on() {
        log!("launcher probe: files gen {gen} rows {n} after {ms} ms, spawn {m} ms");
        if let Some(bytes) = private {
            log!("launcher probe: files helper private {bytes} B after rows");
        }
    }
}

/// Whether a late batch moved the selection (the stability rule says it never does).
#[derive(Debug)]
enum Selection {
    Unchanged,
    Moved,
}

/// A files batch of the current generation was merged (V-2).
pub fn late_batch(gen: u64, n: usize, moved: bool) {
    if probe_on() {
        let kind = if moved { Selection::Moved } else { Selection::Unchanged };
        log!("launcher probe: late batch gen {gen} rows {n} selection {kind:?}");
    }
}

/// The private bytes of a process the dock started (the files helper), read from its handle.
pub fn process_private(h: std::os::windows::io::RawHandle) -> Option<usize> {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    let mut c = PROCESS_MEMORY_COUNTERS_EX {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    // SAFETY: `h` is the helper's process handle (full access, open while the helper is held); `c` is writable and `cb`
    // is its size; the EX counters start with the plain ones.
    unsafe { GetProcessMemoryInfo(HANDLE(h), std::ptr::from_mut(&mut c).cast::<PROCESS_MEMORY_COUNTERS>(), c.cb) }
        .ok()?;
    Some(c.PrivateUsage)
}
