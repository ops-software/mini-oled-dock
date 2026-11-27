//! The frame clock's pacing (K1a.8 candidate A, K3.4 candidate B; KA2 then KE2): a frame source exists only while a
//! tween lives. Candidate A is one frame timer per window (SetTimer at USER_TIMER_MINIMUM), armed when a tween starts
//! and killed after the frame where the last one ends. Candidate B is one high-resolution waitable timer per UI thread
//! in the loop's wait set (hooks::wait_handles), due at the next vblank that DwmGetCompositionTimingInfo predicts,
//! re-armed after each frame and closed when the thread's last animating window stops. Each frame repaints the animated
//! nodes' halos. PACING picks the candidate; S-ANIM decides it. The AnimClock (anim.rs) is not touched: the source only
//! decides when the next frame is painted.

use windows::Win32::Foundation::HANDLE;

use super::input::{self, TIMER_FRAME};
use super::window::WinHost;
use crate::cx::{AnimId, Ease};
use crate::node::{NodeFlags, NodeId};

/// The two frame sources S-ANIM compares (P2-kit K3.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pacing {
    /// Candidate A: SetTimer(USER_TIMER_MINIMUM) per window while a tween lives.
    TimerMin,
    /// Candidate B: a high-resolution waitable timer per UI thread, due at the next vblank.
    HighRes,
}

/// Decided by S-ANIM (P2-kit K3.4, live session LIVE-K3 step 1): TimerMin until the spike's pass rule holds for
/// HighRes; only a separate commit changes it.
pub(crate) const PACING: Pacing = Pacing::TimerMin;

/// S-ANIM's rig only: the gallery's `--measure anim --pacing a|b` sets this variable to "a" or "b" in its own process to
/// run that candidate whatever PACING says. It is read for the gallery's AppId only, so no app follows it.
pub(crate) const RIG_VAR: &str = "OLED_UI_SPIKE_PACING"; // lint: not UI

/// The pacing of an app: PACING, or for the gallery the candidate its S-ANIM rig asked for.
pub(crate) fn pacing_for(app: &str, rig: Option<Pacing>) -> Pacing {
    match rig {
        Some(p) if app == "gallery" => p, // lint: not UI
        _ => PACING,
    }
}

/// The candidate the rig's variable names, if any.
fn rig_choice() -> Option<Pacing> {
    match std::env::var_os(RIG_VAR)?.to_str()? {
        "a" => Some(Pacing::TimerMin), // lint: not UI
        "b" => Some(Pacing::HighRes),  // lint: not UI
        _ => None,
    }
}

/// What a frame source does: started when a tween begins, re-armed after each frame, stopped after the last one.
pub(crate) trait FrameSource {
    fn start(&mut self) -> bool;
    fn next(&mut self);
    fn stop(&mut self);
}

/// The clock's rule over any source (the window's timer, or a fake in the tests): it starts when a tween lives and it is
/// not running, is re-armed after each frame while one lives, and stops after the frame where the last one ended (that
/// frame paints the end values). Outside a frame a running clock is left alone. Returns whether it runs afterwards.
pub(crate) fn drive(src: &mut impl FrameSource, running: bool, live: bool, after_frame: bool) -> bool {
    match (running, live, after_frame) {
        (false, true, _) => src.start(),
        (true, true, true) => {
            src.next();
            true
        }
        (true, false, true) => {
            src.stop();
            false
        }
        (running, _, _) => running,
    }
}

/// A window's frame source: its own frame timer (A), or its thread's high-resolution timer (B).
enum Source<'a> {
    TimerMin(&'a WinHost),
    HighRes(&'a WinHost),
}

impl<'a> Source<'a> {
    /// The source a starting clock uses: the app's pacing.
    fn for_start(host: &'a WinHost) -> Source<'a> {
        let app = host.lp.app.0;
        let rig = if app == "gallery" { rig_choice() } else { None }; // lint: not UI
        match pacing_for(app, rig) {
            Pacing::TimerMin => Source::TimerMin(host),
            Pacing::HighRes => Source::HighRes(host),
        }
    }

    /// The source a running clock uses: the thread timer if the window joined it, else its frame timer.
    fn running(host: &'a WinHost) -> Source<'a> {
        if high_res::holds(host.hwnd) {
            Source::HighRes(host)
        } else {
            Source::TimerMin(host)
        }
    }
}

impl FrameSource for Source<'_> {
    fn start(&mut self) -> bool {
        match self {
            Source::TimerMin(h) => {
                input::arm(h, TIMER_FRAME, 0, true);
                true
            }
            Source::HighRes(h) => {
                if !high_res::join(h.hwnd) {
                    // No high-resolution timer here: the frame timer paces this window.
                    return Source::TimerMin(h).start();
                }
                h.lp.count(|s| {
                    s.timers_armed += 1;
                    s.timers_live += 1;
                });
                true
            }
        }
    }

    fn next(&mut self) {
        // A's timer is periodic; B's is one-shot and aims at the next vblank again.
        if let Source::HighRes(h) = self {
            high_res::next(h.hwnd);
        }
    }

    fn stop(&mut self) {
        match self {
            Source::TimerMin(h) => input::kill(h, TIMER_FRAME),
            Source::HighRes(h) => {
                high_res::leave(h.hwnd);
                h.lp.count(|s| s.timers_live = s.timers_live.saturating_sub(1));
            }
        }
    }
}

/// Effect::Animate: the tween starts and the frame clock is armed.
pub(crate) fn arm(host: &mut WinHost, id: AnimId, to: f32, dur_ms: u32, ease: Ease) {
    let now = host.lp.now_ms();
    host.disp.anim.start(id, to, dur_ms, ease, now);
    if !host.animating.contains(&id.node) {
        host.animating.push(id.node);
    }
    keep_alive(host);
}

/// The hot node changed from `before`: the hover fades of the old and the new one are animated nodes too.
pub(crate) fn note_hot(host: &mut WinHost, before: Option<NodeId>) {
    let after = host.disp.state.hot;
    if before == after {
        return;
    }
    for id in [before, after].into_iter().flatten() {
        let fades = host.disp.frame.get(id).is_some_and(|n| n.flags.contains(NodeFlags::HOVER_FADE));
        if fades && !host.animating.contains(&id) {
            host.animating.push(id);
        }
    }
    keep_alive(host);
}

/// Starts the frame clock if a tween lives and it is not running.
pub(crate) fn keep_alive(host: &mut WinHost) {
    if host.frame_timer || !host.disp.anim.is_live(host.lp.now_ms()) {
        return;
    }
    let running = drive(&mut Source::for_start(host), false, true, false);
    host.frame_timer = running;
}

/// No tween left: the frame clock stops.
pub(crate) fn disarm(host: &mut WinHost) {
    if host.frame_timer {
        Source::running(host).stop();
        host.frame_timer = false;
    }
    host.animating.clear();
}

/// One frame of the clock (A: the window's WM_TIMER; B: the thread timer, which every window of the loop takes): the
/// animated nodes are repainted; the clock is re-armed while a tween lives and stops after the frame that shows the last
/// tween's end. A window whose clock is not running only lets B's timer forget windows that are gone.
pub(crate) fn on_frame_timer(host: &mut WinHost) {
    if !host.frame_timer {
        host.animating.clear();
        high_res::settle();
        return;
    }
    let rects: Vec<_> = host.animating.iter().filter_map(|id| host.disp.frame.get(*id)).map(|n| n.rect).collect();
    for r in rects {
        let halo = host.halo(r);
        host.add_damage(halo);
    }
    if host.disp.anim.is_live(host.lp.now_ms()) {
        drive(&mut Source::running(host), true, true, true);
    } else {
        disarm(host);
    }
}

/// The handle the loop also waits on: this thread's high-resolution frame timer while a window animates with it.
pub(crate) fn wait_handle() -> Option<HANDLE> {
    high_res::handle()
}

/// The relative due time (100 ns units, negative, as SetWaitableTimer takes it) of the next frame: the first vblank
/// after `now` on the grid `vblank + k * period` (QPC ticks at `freq` per second), or a 60 Hz period from now when DWM
/// gave no usable timing (none, a zero period, or one longer than a second). Never zero, never more than a second.
pub(crate) fn next_deadline(now: u64, timing: Option<(u64, u64)>, freq: u64) -> i64 {
    let freq = freq.max(1);
    let wait = match timing {
        // A vblank still ahead (at most a period: a farther one is not a vblank DWM saw).
        Some((vblank, period)) if period > 0 && period <= freq && vblank > now => (vblank - now).min(period),
        Some((vblank, period)) if period > 0 && period <= freq => period - (now - vblank) % period,
        _ => freq / 60,
    };
    let ticks = (u128::from(wait) * 10_000_000 / u128::from(freq)).clamp(1, i64::MAX as u128);
    -(ticks as i64)
}

/// Candidate B's timers: one per UI thread that has an animating window, with those windows. The loop's wait set is
/// read through wait_handle(), which has no window or loop to ask, so the table is keyed by the calling thread (a lock,
/// not a thread-local; held only for a few calls that send no message).
mod high_res {
    use std::sync::{Mutex, MutexGuard, PoisonError};
    use std::thread::ThreadId;

    use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND};
    use windows::Win32::Graphics::Dwm::{DwmGetCompositionTimingInfo, DWM_TIMING_INFO};
    use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
    use windows::Win32::System::Threading::{
        CancelWaitableTimer, CreateWaitableTimerExW, SetWaitableTimer, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
        TIMER_ALL_ACCESS,
    };
    use windows::Win32::UI::WindowsAndMessaging::IsWindow;

    use super::next_deadline;

    struct ThreadTimer {
        thread: ThreadId,
        /// The timer's HANDLE value.
        timer: usize,
        /// The HWND values of the windows animating on it.
        windows: Vec<usize>,
    }

    static TIMERS: Mutex<Vec<ThreadTimer>> = Mutex::new(Vec::new());

    fn table() -> MutexGuard<'static, Vec<ThreadTimer>> {
        TIMERS.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn here(t: &[ThreadTimer]) -> Option<usize> {
        let me = std::thread::current().id();
        t.iter().position(|e| e.thread == me)
    }

    fn handle_of(v: usize) -> HANDLE {
        HANDLE(v as *mut core::ffi::c_void)
    }

    /// Drops the windows that no longer exist (destroyed while animating); with none left the timer is closed.
    fn prune(t: &mut Vec<ThreadTimer>, i: usize) {
        // SAFETY: IsWindow only reads; a stale value at worst names another window, which is then kept one frame more.
        t[i].windows.retain(|w| unsafe { IsWindow(Some(HWND(*w as *mut core::ffi::c_void))) }.as_bool());
        if t[i].windows.is_empty() {
            let e = t.swap_remove(i);
            // SAFETY: the timer this table created; it leaves the table here, so nothing waits on it after this pass.
            unsafe {
                let _ = CancelWaitableTimer(handle_of(e.timer));
                let _ = CloseHandle(handle_of(e.timer));
            }
        }
    }

    /// A window starts animating on this thread's timer (made on first use, high resolution where Windows has it), which
    /// is aimed at the next vblank. False when no timer could be made or armed.
    pub(super) fn join(hwnd: HWND) -> bool {
        let mut t = table();
        let i = match here(&t) {
            Some(i) => i,
            None => {
                let Some(h) = create() else { return false };
                t.push(ThreadTimer { thread: std::thread::current().id(), timer: h.0 as usize, windows: Vec::new() });
                t.len() - 1
            }
        };
        let w = hwnd.0 as usize;
        if !t[i].windows.contains(&w) {
            t[i].windows.push(w);
        }
        if arm_next(t[i].timer) {
            return true;
        }
        t[i].windows.retain(|x| *x != w);
        prune(&mut t, i);
        false
    }

    /// After a frame: the timer aims at the next vblank again (every animating window asks; the aim is the same).
    pub(super) fn next(hwnd: HWND) {
        let t = table();
        if let Some(e) = here(&t).map(|i| &t[i]).filter(|e| e.windows.contains(&(hwnd.0 as usize))) {
            arm_next(e.timer);
        }
    }

    /// A window stops animating; the last one closes the timer.
    pub(super) fn leave(hwnd: HWND) {
        let mut t = table();
        if let Some(i) = here(&t) {
            t[i].windows.retain(|w| *w != hwnd.0 as usize);
            prune(&mut t, i);
        }
    }

    /// A frame reached a window that does not animate: windows destroyed while animating are dropped.
    pub(super) fn settle() {
        let mut t = table();
        if let Some(i) = here(&t) {
            prune(&mut t, i);
        }
    }

    /// True when the window animates on this thread's timer.
    pub(super) fn holds(hwnd: HWND) -> bool {
        let t = table();
        here(&t).is_some_and(|i| t[i].windows.contains(&(hwnd.0 as usize)))
    }

    /// This thread's timer, while a window animates on it.
    pub(super) fn handle() -> Option<HANDLE> {
        let t = table();
        here(&t).map(|i| handle_of(t[i].timer))
    }

    /// An unnamed synchronization timer, high resolution where Windows has it (10 1803 and later).
    fn create() -> Option<HANDLE> {
        let access = TIMER_ALL_ACCESS.0;
        // SAFETY: an unnamed timer with default security; the table owns it and closes it in prune.
        let high = unsafe { CreateWaitableTimerExW(None, None, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, access) };
        if let Ok(h) = high {
            return Some(h);
        }
        // SAFETY: as above, without the flag the older systems refuse.
        unsafe { CreateWaitableTimerExW(None, None, 0, access) }.ok()
    }

    /// Aims the timer at the next vblank (DwmGetCompositionTimingInfo's qpcVBlank + qpcRefreshPeriod grid).
    fn arm_next(timer: usize) -> bool {
        let (mut now, mut freq) = (0i64, 0i64);
        // SAFETY: a writable i64.
        if unsafe { QueryPerformanceCounter(&mut now) }.is_err() {
            return false;
        }
        // SAFETY: a writable i64.
        if unsafe { QueryPerformanceFrequency(&mut freq) }.is_err() {
            return false;
        }
        let mut info = DWM_TIMING_INFO { cbSize: std::mem::size_of::<DWM_TIMING_INFO>() as u32, ..Default::default() };
        // SAFETY: `info` is writable with cbSize set; a null window asks for the desktop's composition timing (the only
        // form Windows 8.1 and later accept).
        let timing = unsafe { DwmGetCompositionTimingInfo(HWND::default(), &mut info) }
            .ok()
            .map(|_| (info.qpcVBlank, info.qpcRefreshPeriod));
        let due = next_deadline(now.max(0) as u64, timing, freq.max(0) as u64);
        // SAFETY: a timer of this table, open while it is in the table; `due` lives for the call; no completion routine.
        unsafe { SetWaitableTimer(handle_of(timer), &due, 0, None, None, false) }.is_ok()
    }
}

#[cfg(test)]
mod tests {
    use windows::Win32::Foundation::HWND;

    use super::*;
    use crate::anim::AnimClock;
    use crate::node::WidgetId;

    #[test]
    fn next_deadline_from_timing_info() {
        let freq = 10_000_000; // 100 ns ticks: the due time equals the wait
        let period = 166_667; // 60 Hz
        let vblank = 5_000_000_000;
        assert_eq!(next_deadline(vblank + 50_000, Some((vblank, period)), freq), -116_667, "the rest of this period");
        assert_eq!(next_deadline(vblank, Some((vblank, period)), freq), -(period as i64), "on a vblank: the next one");
        let late = vblank + 7 * period + 1_000;
        assert_eq!(next_deadline(late, Some((vblank, period)), freq), -(period as i64 - 1_000), "many periods behind");
        assert_eq!(next_deadline(vblank - 300, Some((vblank, period)), freq), -300, "a vblank still ahead");
        // A 3 MHz counter and 144 Hz: 20,833 ticks a period; 10,000 ticks left are 33,333 x 100 ns.
        let (f3, p144) = (3_000_000, 20_833);
        assert_eq!(next_deadline(1_000 + p144 - 10_000, Some((1_000, p144)), f3), -33_333);
        // No usable timing: one 60 Hz period from now.
        for t in [None, Some((vblank, 0)), Some((vblank, freq * 2))] {
            assert_eq!(next_deadline(vblank, t, freq), -166_666, "{t:?}");
        }
        assert_eq!(next_deadline(0, Some((u64::MAX, 100)), 1_000), -1_000_000, "a far vblank waits one period");
        assert_eq!(next_deadline(u64::MAX, Some((0, 7)), 0), -1, "a zero frequency: never zero or positive");
        let end = next_deadline(u64::MAX, Some((0, 7_000_000)), freq);
        assert!((-7_000_000..0).contains(&end), "no overflow near the counter's end: {end}");
    }

    /// A fake source: what the clock asked of it.
    #[derive(Default)]
    struct Fake {
        log: Vec<&'static str>,
        armed: bool,
    }

    impl FrameSource for Fake {
        fn start(&mut self) -> bool {
            self.log.push("start");
            self.armed = true;
            true
        }

        fn next(&mut self) {
            assert!(self.armed, "re-armed while stopped");
            self.log.push("next");
        }

        fn stop(&mut self) {
            self.log.push("stop");
            self.armed = false;
        }
    }

    #[test]
    fn pacing_disarms_when_idle() {
        let id = AnimId { node: NodeId::new(WidgetId(1), 0), channel: 0 };
        let mut clock = AnimClock::new();
        let mut src = Fake::default();
        let mut running = false;
        // Nothing lives: nothing starts.
        running = drive(&mut src, running, clock.is_live(0), false);
        assert!(!running && src.log.is_empty());
        clock.start(id, 1.0, 100, Ease::Linear, 0);
        running = drive(&mut src, running, clock.is_live(0), false);
        // Input between frames never re-arms or stops a running clock.
        running = drive(&mut src, running, clock.is_live(5), false);
        let mut now = 0;
        while running {
            now += 16;
            running = drive(&mut src, running, clock.is_live(now), true);
        }
        assert_eq!(now, 112, "the frame after the tween's end paints its end value, then the clock stops");
        assert_eq!(src.log.first(), Some(&"start"));
        assert_eq!(src.log.iter().filter(|l| **l == "next").count(), 6);
        assert_eq!(src.log.last(), Some(&"stop"));
        assert!(!src.armed && !clock.is_live(now));
        // Idle afterwards: more frames or input change nothing.
        let before = src.log.len();
        for t in [now + 16, now + 1000] {
            running = drive(&mut src, running, clock.is_live(t), true);
            running = drive(&mut src, running, clock.is_live(t), false);
        }
        assert!(!running && src.log.len() == before && !src.armed);
    }

    #[test]
    fn pacing_is_decided_by_s_anim() {
        assert_eq!(PACING, Pacing::TimerMin, "HighRes only after S-ANIM's pass, in its own commit");
        assert_eq!(pacing_for("gallery", Some(Pacing::HighRes)), Pacing::HighRes, "the rig's candidate B");
        assert_eq!(pacing_for("gallery", Some(Pacing::TimerMin)), Pacing::TimerMin);
        assert_eq!(pacing_for("gallery", None), PACING);
        assert_eq!(pacing_for("tasks", Some(Pacing::HighRes)), PACING, "an app never follows the rig's variable");
        assert_eq!(pacing_for("files", None), PACING);
    }

    #[test]
    fn no_thread_timer_without_an_animating_window() {
        // A fresh test thread has never joined: nothing in the wait set, nothing held, settling changes nothing.
        let t = std::thread::spawn(|| {
            high_res::settle();
            (wait_handle().is_none(), high_res::holds(HWND::default()))
        });
        assert_eq!(t.join().ok(), Some((true, false)));
    }
}
