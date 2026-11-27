//! The sampler around the window: when it runs and how fast (the control of the engine's SamplerHost), the status the
//! toolbar shows and the first-frame log line (T1.9, T1.11).

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use oled_ui::host::Proxy;
use oled_ui::{EventCx, TimerId};
use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

use super::options::TasksOptions;
use crate::engine::cadence::{self, Control, Refresh};
use crate::engine::frame::Frame;
use crate::engine::host::{default_samplers, FrameSink, HostStats, SamplerHost};
use crate::engine::need::{need_set, ViewNeeds};
use crate::model::view::{ColumnId, Page, ViewMode};
use crate::ui::screen::{FrameReady, TIMER_SNAPSHOT_B};

/// The columns of the beta, in the need set of every view (the column chooser is T8's).
pub const DEFAULT_COLUMNS: [ColumnId; 5] =
    [ColumnId::Name, ColumnId::Cpu, ColumnId::Memory, ColumnId::Io, ColumnId::Pid];

/// The machine's CPU load under which the idle rule may slow the sampler down.
const CALM_CPU_PCT: f32 = 10.0;
/// Snapshot B follows the first paint by this long (SPEC-tasks 5.5.8 step 6).
const SNAPSHOT_B_MS: u32 = 250;

/// The sampler of the window, or none (inert).
pub struct Sampling {
    host: Option<SamplerHost>,
    /// The last control set; tests read it.
    last: Cell<Option<Control>>,
    sample_now_calls: Cell<u32>,
    first_frame_lines: Cell<u32>,
    /// When the sampler stopped (minimised, cloaked or display off) and its counters then.
    stopped: Cell<Option<(u64, HostStats)>>,
    attacher: Attacher,
}

/// The rendezvous of the sampler thread's "a frame is ready" and the window's mailbox.
#[derive(Default)]
struct Link {
    post: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    /// A frame was ready before the window could be told.
    early: AtomicBool,
}

impl Link {
    fn lock(&self) -> MutexGuard<'_, Option<Box<dyn Fn() + Send + Sync>>> {
        self.post.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// From the sampler thread: tells the window, or remembers that it has to be told.
    fn ready(&self) {
        let g = self.lock();
        match g.as_ref() {
            Some(post) => post(),
            None => self.early.store(true, Ordering::SeqCst),
        }
    }

    fn attach(&self, post: Box<dyn Fn() + Send + Sync>) {
        let mut g = self.lock();
        if self.early.swap(false, Ordering::SeqCst) {
            post();
        }
        *g = Some(post);
    }
}

/// The engine's FrameSink: ready() posts FrameReady to the window.
struct ProxySink(Arc<Link>);

impl FrameSink for ProxySink {
    fn ready(&self) {
        self.0.ready();
    }
}

/// Hands the window's proxy to the sampler thread once the window exists; Clone + Send.
#[derive(Clone, Default)]
pub struct Attacher {
    link: Arc<Link>,
}

impl Attacher {
    /// A frame ready before the attach is posted now, once.
    pub fn attach(&self, p: Proxy) {
        self.attach_with(Box::new(move || {
            let _ = p.send(Box::new(FrameReady));
        }));
    }

    /// The same with any poster (tests count the posts).
    pub fn attach_with(&self, post: Box<dyn Fn() + Send + Sync>) {
        self.link.attach(post);
    }

    /// What the sampler's sink calls (tests stand in for the thread).
    pub fn frame_ready(&self) {
        self.link.ready();
    }
}

impl Sampling {
    /// No thread: set_control records, take is false, stats are zero.
    pub fn inert() -> Sampling {
        Sampling::with(None, Attacher::default())
    }

    fn with(host: Option<SamplerHost>, attacher: Attacher) -> Sampling {
        Sampling {
            host,
            last: Cell::new(None),
            sample_now_calls: Cell::new(0),
            first_frame_lines: Cell::new(0),
            stopped: Cell::new(None),
            attacher,
        }
    }

    /// Taken before Deps moves into the screen.
    pub fn attacher(&self) -> Attacher {
        self.attacher.clone()
    }

    pub fn set_control(&self, c: Control) {
        self.last.set(Some(c));
        if let Some(h) = &self.host {
            h.set_control(c);
        }
    }

    pub fn sample_now(&self) {
        self.sample_now_calls.set(self.sample_now_calls.get().saturating_add(1));
        if let Some(h) = &self.host {
            h.sample_now();
        }
    }

    /// The newest frame into `into`; false when there is none (always, inert).
    pub fn take(&self, into: &mut Frame) -> bool {
        self.host.as_ref().is_some_and(|h| h.take(into))
    }

    pub fn stats(&self) -> HostStats {
        self.host.as_ref().map(SamplerHost::stats).unwrap_or_default()
    }

    /// Stops the thread (at most engine::host::STOP_WAIT_MS).
    pub fn stop(self) {
        if let Some(h) = self.host {
            h.stop();
        }
    }

    pub fn last_control(&self) -> Option<Control> {
        self.last.get()
    }

    pub fn sample_now_calls(&self) -> u32 {
        self.sample_now_calls.get()
    }

    /// How many first-frame lines were written (tests read it; the screen writes one).
    pub fn first_frame_lines(&self) -> u32 {
        self.first_frame_lines.get()
    }

    /// Writes the restore line when the sampler leaves a stop (minimised, cloaked, display off) and remembers a stop
    /// when it enters one.
    fn track_stop(&self, new: &Control, now_ms: u64) {
        let stopped = new.minimised_or_cloaked || new.display_off || !new.visible;
        match (self.stopped.get(), stopped) {
            (None, true) => self.stopped.set(Some((now_ms, self.stats()))),
            (Some((since, then)), false) => {
                self.stopped.set(None);
                let (iterations, wakes) = stopped_counts(then, self.stats());
                oled_sys::log!("{}", stopped_line(now_ms.saturating_sub(since) / 1000, iterations, wakes));
            }
            _ => {}
        }
    }
}

/// The iterations and wakes to report for a stop, from the counters at the stop and at the restore. Applying the stop
/// costs the sampler one wake (the control event), which the restore reading already holds: it is left out, from the
/// wakes when there were any, else from the iterations (a sample that was in flight).
pub fn stopped_counts(then: HostStats, now: HostStats) -> (u64, u64) {
    let (i, w) = (now.iterations.saturating_sub(then.iterations), now.wakes.saturating_sub(then.wakes));
    if w > 0 {
        (i, w - 1)
    } else {
        (i.saturating_sub(1), 0)
    }
}

/// "tasks: stopped <s> s, <i> sampler iterations, <w> wakes" (the evidence LIVE-T1 reads).
pub fn stopped_line(secs: u64, iterations: u64, wakes: u64) -> String {
    format!("tasks: stopped {secs} s, {iterations} sampler iterations, {wakes} wakes")
}

/// "tasks: first frame <ms> ms, <n> processes".
pub fn first_frame_line(ms: u32, rows: usize) -> String {
    format!("tasks: first frame {ms} ms, {rows} processes")
}

/// Starts the sampler thread with control `c`. A thread that cannot start leaves the window with no sampler (it
/// shows no rows) and one log line.
pub fn start(c: Control) -> Sampling {
    let attacher = Attacher::default();
    let sink = ProxySink(Arc::clone(&attacher.link));
    let host = match SamplerHost::start(Box::new(sink), default_samplers(), c) {
        Ok(h) => Some(h),
        Err(e) => {
            oled_sys::log!("tasks: the sampler did not start ({e:?})");
            None
        }
    };
    let s = Sampling::with(host, attacher);
    s.last.set(Some(c));
    s
}

/// What the control is computed from; the screen keeps one and updates it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ControlInputs {
    pub minimised: bool,
    pub cloaked: bool,
    pub display_off: bool,
    pub active: bool,
    pub paused: bool,
    pub last_input_ms: u64,
    pub machine_cpu_pct: f32,
    pub mode: ViewMode,
    pub windows_rows_visible: bool,
}

/// The sampler's control, pure. Windows, Hung and PerCore follow need_set exactly.
pub fn control_of(i: &ControlInputs, o: &TasksOptions, now_ms: u64) -> Control {
    let minimised_or_cloaked = i.minimised || i.cloaked;
    let need = need_set(&ViewNeeds {
        page: Page::Processes,
        mode: i.mode,
        columns: &DEFAULT_COLUMNS,
        strips: &[],
        expanded: &[],
        windows_rows_visible: i.windows_rows_visible,
        verdict_on: false,
    });
    Control {
        visible: !minimised_or_cloaked && !i.display_off,
        foreground: i.active,
        minimised_or_cloaked,
        display_off: i.display_off,
        refresh: if i.paused { Refresh::Paused } else { o.refresh },
        idle: cadence::idle(i.last_input_ms, now_ms, i.machine_cpu_pct < CALM_CPU_PCT),
        need,
    }
}

/// What the toolbar's status shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Every { period_ms: u32 },
    EveryIdle { period_ms: u32 },
    Paused,
}

/// Pure. A stopped sampler shows the speed it will have at restore.
pub fn status(c: &Control, slow: bool) -> Status {
    if c.refresh == Refresh::Paused {
        return Status::Paused;
    }
    let period_ms = cadence::period_ms(c, slow).or(c.refresh.ms()).unwrap_or(1000);
    if c.idle {
        Status::EveryIdle { period_ms }
    } else {
        Status::Every { period_ms }
    }
}

/// The control of now, set on the sampler only when it changed; Some(the new control) then. Runs at the end of every
/// event and data call, never on a timer.
pub fn sync(s: &Sampling, i: &ControlInputs, o: &TasksOptions, now_ms: u64) -> Option<Control> {
    let c = control_of(i, o, now_ms);
    if s.last_control() == Some(c) {
        return None;
    }
    // The readings come before the signal: the stop's own wake is not in them.
    s.track_stop(&c, now_ms);
    s.set_control(c);
    Some(c)
}

/// A pointer, key, text or wheel input: the idle rule's last input, and a sampler that was idle samples at once.
pub fn on_input(s: &Sampling, i: &mut ControlInputs, now_ms: u64) {
    i.last_input_ms = now_ms;
    if s.last_control().is_some_and(|c| c.idle) {
        s.sample_now();
    }
}

/// Milliseconds from the creation of this process to now.
pub fn ms_since_start() -> u32 {
    let mut created = FILETIME::default();
    let (mut exited, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
    // SAFETY: the pseudo handle of this process and four writable FILETIMEs that live for the call.
    let ok = unsafe { GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user) }.is_ok();
    if !ok {
        return 0;
    }
    // SAFETY: no arguments; returns the time.
    let now = unsafe { GetSystemTimePreciseAsFileTime() };
    ms_between(filetime_u64(created), filetime_u64(now))
}

fn filetime_u64(f: FILETIME) -> u64 {
    (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime)
}

/// Milliseconds between two FILETIME values (100 ns units), saturating at both ends.
pub fn ms_between(from: u64, to: u64) -> u32 {
    u32::try_from(to.saturating_sub(from) / 10_000).unwrap_or(u32::MAX)
}

/// Once, at the first event after the first paint: the first-frame line, and snapshot B at +250 ms.
pub fn on_first_paint(cx: &mut EventCx, s: &Sampling, ms: u32, rows: usize) {
    oled_sys::log!("{}", first_frame_line(ms, rows));
    s.first_frame_lines.set(s.first_frame_lines.get().saturating_add(1));
    cx.set_timer(TIMER_SNAPSHOT_B, SNAPSHOT_B_MS);
}

/// TIMER_SNAPSHOT_B: the second snapshot, whose deltas (CPU, I/O) are the first real ones.
pub fn on_timer(cx: &mut EventCx, s: &Sampling, id: TimerId) {
    let _ = cx;
    if id == TIMER_SNAPSHOT_B {
        s.sample_now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::need::Need;
    use std::sync::atomic::AtomicU32;

    fn inputs() -> ControlInputs {
        ControlInputs { active: true, mode: ViewMode::Grouped, ..ControlInputs::default() }
    }

    #[test]
    fn control_rules() {
        let o = TasksOptions::default();
        let base = control_of(&inputs(), &o, 0);
        assert!(base.visible && base.foreground && !base.minimised_or_cloaked && !base.display_off && !base.idle);
        assert_eq!(base.refresh, o.refresh);
        let min = control_of(&ControlInputs { minimised: true, ..inputs() }, &o, 0);
        assert!(min.minimised_or_cloaked && !min.visible);
        let cloak = control_of(&ControlInputs { cloaked: true, ..inputs() }, &o, 0);
        assert!(cloak.minimised_or_cloaked && !cloak.visible);
        let off = control_of(&ControlInputs { display_off: true, ..inputs() }, &o, 0);
        assert!(off.display_off && !off.visible && !off.minimised_or_cloaked);
        let paused = control_of(&ControlInputs { paused: true, ..inputs() }, &o, 0);
        assert_eq!(paused.refresh, Refresh::Paused);
        // Idle: 30 s without input while the machine is calm.
        let calm = ControlInputs { machine_cpu_pct: 3.0, ..inputs() };
        assert!(!control_of(&calm, &o, 29_999).idle);
        assert!(control_of(&calm, &o, 30_000).idle);
        assert!(!control_of(&ControlInputs { machine_cpu_pct: 40.0, ..inputs() }, &o, 60_000).idle);
        // The need set: Services only while rows of Part of Windows show.
        let grouped = control_of(&inputs(), &o, 0).need;
        assert!(grouped.has(Need::Windows) && grouped.has(Need::Hung) && grouped.has(Need::PerCore));
        assert!(!grouped.has(Need::Services));
        let open = control_of(&ControlInputs { windows_rows_visible: true, ..inputs() }, &o, 0).need;
        assert!(open.has(Need::Services));
        for mode in [ViewMode::Tree, ViewMode::List] {
            assert!(!control_of(&ControlInputs { mode, ..inputs() }, &o, 0).need.has(Need::Windows));
        }
    }

    #[test]
    fn status_words() {
        let o = TasksOptions::default();
        let c = control_of(&inputs(), &o, 0);
        assert_eq!(status(&c, false), Status::Every { period_ms: 1000 });
        assert_eq!(status(&c, true), Status::Every { period_ms: 2000 });
        assert_eq!(status(&Control { foreground: false, ..c }, false), Status::Every { period_ms: 2000 });
        assert_eq!(status(&Control { idle: true, ..c }, false), Status::EveryIdle { period_ms: 2000 });
        assert_eq!(status(&Control { refresh: Refresh::Paused, ..c }, false), Status::Paused);
        assert_eq!(
            status(&Control { minimised_or_cloaked: true, visible: false, ..c }, false),
            Status::Every { period_ms: 1000 },
            "a stopped sampler shows the speed it will have at restore"
        );
    }

    #[test]
    fn sync_only_on_change() {
        let s = Sampling::inert();
        let o = TasksOptions::default();
        let i = inputs();
        assert!(sync(&s, &i, &o, 0).is_some());
        assert!(sync(&s, &i, &o, 10).is_none());
        assert!(sync(&s, &ControlInputs { minimised: true, ..i }, &o, 20).is_some());
        assert!(s.last_control().is_some_and(|c| c.minimised_or_cloaked));
    }

    #[test]
    fn sink_before_attach_posts_once() {
        let a = Attacher::default();
        a.frame_ready();
        a.frame_ready();
        let n = Arc::new(AtomicU32::new(0));
        let m = Arc::clone(&n);
        a.attach_with(Box::new(move || {
            m.fetch_add(1, Ordering::SeqCst);
        }));
        assert_eq!(n.load(Ordering::SeqCst), 1, "the early frame is posted once at attach");
        a.frame_ready();
        assert_eq!(n.load(Ordering::SeqCst), 2, "after the attach every frame posts");
        // A second attach with nothing early posts nothing.
        let k = Arc::new(AtomicU32::new(0));
        let k2 = Arc::clone(&k);
        a.attach_with(Box::new(move || {
            k2.fetch_add(1, Ordering::SeqCst);
        }));
        assert_eq!(k.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn stop_and_restore_lines() {
        assert_eq!(stopped_line(6, 0, 2), "tasks: stopped 6 s, 0 sampler iterations, 2 wakes");
        assert_eq!(first_frame_line(80, 142), "tasks: first frame 80 ms, 142 processes");
        // The stop is remembered on the way in and forgotten on the way out.
        let s = Sampling::inert();
        let o = TasksOptions::default();
        let i = inputs();
        sync(&s, &ControlInputs { minimised: true, ..i }, &o, 1_000);
        assert!(s.stopped.get().is_some());
        sync(&s, &i, &o, 7_000);
        assert!(s.stopped.get().is_none());
    }

    #[test]
    fn stopped_counts_leave_out_the_stop_itself() {
        let at = |iterations, wakes| HostStats { iterations, wakes, ..HostStats::default() };
        assert_eq!(stopped_counts(at(10, 20), at(10, 21)), (0, 0), "only the stop's wake");
        assert_eq!(stopped_counts(at(10, 20), at(10, 23)), (0, 2));
        assert_eq!(stopped_counts(at(10, 20), at(11, 20)), (0, 0), "a sample in flight");
        assert_eq!(stopped_counts(at(10, 20), at(10, 20)), (0, 0));
        assert_eq!(stopped_counts(at(10, 20), at(12, 22)), (2, 1));
    }

    #[test]
    fn filetime_difference() {
        assert_eq!(ms_between(0, 10_000), 1);
        assert_eq!(ms_between(5, 3), 0);
        assert_eq!(ms_between(0, u64::MAX), u32::MAX);
        assert!(ms_since_start() < 3_600_000, "a test process is young");
    }
}
