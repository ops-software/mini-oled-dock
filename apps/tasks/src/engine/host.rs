//! The sampler thread and its hand-over of frames.
use crate::engine::cadence::{period_ms, Control, Slow};
use crate::engine::frame::{Frame, Sampler};
use crate::engine::paths::{PathSampler, Win32Paths};
use crate::engine::services::{ScmServices, ServiceSampler};
use crate::engine::sys::{CoreSampler, ProcSampler};
use crate::engine::windows::{Win32Windows, WindowSampler};
use oled_sysmon::cpu::{logical_cpus, SystemCpu};
use oled_sysmon::mem::memory;
use oled_sysmon::{diff, SysmonError};
use std::os::windows::io::AsRawHandle;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use windows::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject, INFINITE};

/// stop() and Drop wait at most this long for the thread, then detach it.
pub const STOP_WAIT_MS: u32 = 1000;

/// P3: PostMessageW(hwnd, WM_TASKS_FRAME, 0, 0).
pub trait FrameSink: Send + 'static {
    fn ready(&self);
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostStats {
    pub wakes: u64,
    pub iterations: u64,
    pub slow: bool,
    pub last_tick_us: u32,
}

/// What the thread does next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Sample,
    /// Milliseconds; None = INFINITE.
    Wait(Option<u32>),
    Exit,
}

/// The decision core of the thread: pure, so every cadence rule is tested in virtual time.
#[derive(Debug, Default)]
struct Core {
    slow: Slow,
    last_start_us: Option<u64>,
    running: bool,
}

impl Core {
    fn next(&mut self, c: &Control, sample_now: bool, exit: bool, now_us: u64) -> Step {
        if exit {
            return Step::Exit;
        }
        let Some(p) = period_ms(c, self.slow.is_slow()) else {
            self.running = false;
            return if sample_now { Step::Sample } else { Step::Wait(None) };
        };
        let last = match self.last_start_us {
            Some(last) if self.running && !sample_now => last,
            _ => {
                self.running = true;
                return Step::Sample;
            }
        };
        let due = last.saturating_add(p as u64 * 1000);
        if now_us >= due {
            Step::Sample
        } else {
            Step::Wait(Some((due - now_us).div_ceil(1000).min(u32::MAX as u64) as u32))
        }
    }

    fn sampled(&mut self, start_us: u64, tick_us: u32) {
        self.slow.observe(tick_us);
        self.last_start_us = Some(start_us);
    }
}

struct Shared {
    control: Control,
    sample_now: bool,
    exit: bool,
    slot: Frame,
    fresh: bool,
    stats: HostStats,
}

fn lock(m: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// An auto-reset event.
struct Event(HANDLE);

// SAFETY: a kernel event handle may be signalled, waited on and closed from any thread.
unsafe impl Send for Event {}
// SAFETY: as above; SetEvent and WaitForSingleObject take the handle by value.
unsafe impl Sync for Event {}

impl Event {
    fn new() -> Result<Event, SysmonError> {
        // SAFETY: no pointers are passed (default security, no name).
        unsafe { CreateEventW(None, false, false, None) }
            // SAFETY: plain query of the calling thread's last error.
            .map_err(|_| SysmonError::Win32(unsafe { GetLastError() }.0))
            .map(Event)
    }
    fn set(&self) {
        // SAFETY: the handle is a live event owned by self.
        let _ = unsafe { SetEvent(self.0) };
    }
    fn wait(&self, ms: u32) {
        // SAFETY: the handle is a live event owned by self.
        unsafe { WaitForSingleObject(self.0, ms) };
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: the handle is owned by self and closed once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// QueryPerformanceCounter in microseconds.
#[derive(Clone, Copy)]
struct Clock {
    freq: u64,
}

impl Clock {
    fn new() -> Clock {
        let mut f = 0i64;
        // SAFETY: out pointer to a local.
        let _ = unsafe { QueryPerformanceFrequency(&mut f) };
        Clock { freq: f.max(1) as u64 }
    }
    fn now_us(&self) -> u64 {
        let mut c = 0i64;
        // SAFETY: out pointer to a local.
        let _ = unsafe { QueryPerformanceCounter(&mut c) };
        (c.max(0) as u128 * 1_000_000 / self.freq as u128) as u64
    }
}

pub struct SamplerHost {
    thread: Option<JoinHandle<()>>,
    shared: Arc<Mutex<Shared>>,
    event: Arc<Event>,
}

struct Worker {
    shared: Arc<Mutex<Shared>>,
    event: Arc<Event>,
    sink: Box<dyn FrameSink>,
    samplers: Vec<Box<dyn Sampler>>,
}

impl Worker {
    fn run(mut self) {
        let clock = Clock::new();
        let logical = logical_cpus();
        let mut core = Core::default();
        let mut work = Frame::default();
        let mut cpu = SystemCpu::new();
        let mut seq = 0u64;
        loop {
            let (step, control) = {
                let mut g = lock(&self.shared);
                let sample_now = std::mem::take(&mut g.sample_now);
                (core.next(&g.control, sample_now, g.exit, clock.now_us()), g.control)
            };
            match step {
                Step::Exit => break,
                Step::Wait(ms) => {
                    self.event.wait(ms.unwrap_or(INFINITE));
                    lock(&self.shared).stats.wakes += 1;
                }
                Step::Sample => {
                    let start = clock.now_us();
                    work.need = control.need;
                    work.paths_fresh = false;
                    work.windows_fresh = false;
                    work.services_fresh = false;
                    work.error = None;
                    for s in &mut self.samplers {
                        if control.need.has(s.need()) {
                            // A failed sampler leaves its error in the frame (ProcSampler) or its old data.
                            let _ = s.sample(start, &mut work);
                        }
                    }
                    work.sys.cpu = cpu.sample();
                    work.sys.mem = memory();
                    work.sys.logical_cpus = logical;
                    let tick_us = clock.now_us().saturating_sub(start).min(u32::MAX as u64) as u32;
                    core.sampled(start, tick_us);
                    seq += 1;
                    work.seq = seq;
                    work.t_us = start;
                    work.tick_us = tick_us;
                    let mut g = lock(&self.shared);
                    g.stats.iterations += 1;
                    g.stats.last_tick_us = tick_us;
                    g.stats.slow = core.slow.is_slow();
                    if g.exit {
                        break;
                    }
                    std::mem::swap(&mut work, &mut g.slot);
                    let was = std::mem::replace(&mut g.fresh, true);
                    drop(g);
                    if !was {
                        self.sink.ready();
                    }
                }
            }
        }
    }
}

impl SamplerHost {
    pub fn start(
        sink: Box<dyn FrameSink>,
        samplers: Vec<Box<dyn Sampler>>,
        control: Control,
    ) -> Result<SamplerHost, SysmonError> {
        let event = Arc::new(Event::new()?);
        let shared = Arc::new(Mutex::new(Shared {
            control,
            sample_now: false,
            exit: false,
            slot: Frame::default(),
            fresh: false,
            stats: HostStats::default(),
        }));
        let worker = Worker { shared: Arc::clone(&shared), event: Arc::clone(&event), sink, samplers };
        let thread = std::thread::Builder::new()
            .name("tasks-sampler".into())
            .spawn(move || worker.run())
            .map_err(|e| SysmonError::Win32(e.raw_os_error().unwrap_or(0) as u32))?;
        Ok(SamplerHost { thread: Some(thread), shared, event })
    }

    /// A change wakes the thread so the new period applies at once.
    pub fn set_control(&self, c: Control) {
        let changed = {
            let mut g = lock(&self.shared);
            let changed = g.control != c;
            g.control = c;
            changed
        };
        if changed {
            self.event.set();
        }
    }

    pub fn sample_now(&self) {
        lock(&self.shared).sample_now = true;
        self.event.set();
    }

    /// Swaps the newest frame into `into`; computes added/gone and dt_ms against into's previous content.
    pub fn take(&self, into: &mut Frame) -> bool {
        let mut g = lock(&self.shared);
        if !g.fresh {
            return false;
        }
        let dt_ms =
            if into.t_us == 0 { 0 } else { (g.slot.t_us.saturating_sub(into.t_us) / 1000).min(u32::MAX as u64) as u32 };
        if g.slot.error.is_some() {
            // Keep the consumer's last good facts: ReadError is shown over them.
            let slot = &g.slot;
            into.seq = slot.seq;
            into.t_us = slot.t_us;
            into.need = slot.need;
            into.sys.clone_from(&slot.sys);
            into.error = slot.error;
            into.tick_us = slot.tick_us;
            into.added.clear();
            into.gone.clear();
            into.dt_ms = dt_ms;
        } else {
            let slot = &mut *g;
            diff(&into.procs, &slot.slot.procs, &mut slot.slot.added, &mut slot.slot.gone);
            slot.slot.dt_ms = dt_ms;
            std::mem::swap(into, &mut slot.slot);
        }
        g.fresh = false;
        true
    }

    pub fn stats(&self) -> HostStats {
        lock(&self.shared).stats
    }

    /// Returns within STOP_WAIT_MS whatever a sampler is doing.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        let Some(thread) = self.thread.take() else { return };
        lock(&self.shared).exit = true;
        self.event.set();
        // SAFETY: the handle is owned by the JoinHandle and lives until it is dropped below.
        let wait = unsafe { WaitForSingleObject(HANDLE(thread.as_raw_handle()), STOP_WAIT_MS) };
        if wait == WAIT_OBJECT_0 {
            // Returns at once; a panic payload is dropped, not re-raised.
            let _ = thread.join();
        }
        // On a timeout the JoinHandle is dropped here and the thread detaches: it is inside a sampler call that does
        // not return. When that call returns it sees exit and ends without touching the sink or the slot.
    }
}

impl Drop for SamplerHost {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// ProcSampler, PathSampler<Win32Paths>, CoreSampler, WindowSampler<Win32Windows>, ServiceSampler<ScmServices>, in this order.
pub fn default_samplers() -> Vec<Box<dyn Sampler>> {
    vec![
        Box::new(ProcSampler::new()),
        Box::new(PathSampler::new(Win32Paths::new())),
        Box::new(CoreSampler::new()),
        Box::new(WindowSampler::new(Win32Windows)),
        Box::new(ServiceSampler::new(ScmServices)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::cadence::Refresh;
    use crate::engine::need::{Need, NeedSet};
    use crate::model::classify::{ImagePath, PathMap};
    use oled_sysmon::{ProcKey, ProcRow};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
    use std::sync::mpsc::{channel, Receiver};
    use std::time::{Duration, Instant};

    const MS: u64 = 1000;
    const S: u64 = 1_000_000;

    fn live() -> Control {
        Control {
            visible: true,
            foreground: true,
            minimised_or_cloaked: false,
            display_off: false,
            refresh: Refresh::One,
            idle: false,
            need: NeedSet::EMPTY.with(Need::Procs),
        }
    }

    // Core, in virtual time.

    #[test]
    fn core_one_second_cadence() {
        let c = live();
        let mut core = Core::default();
        let mut at = Vec::new();
        let mut now = 0;
        while now <= 3_500 * MS {
            if core.next(&c, false, false, now) == Step::Sample {
                at.push(now);
                core.sampled(now, 1_000);
                assert_eq!(core.next(&c, false, false, now), Step::Wait(Some(1000)));
            }
            now += MS;
        }
        assert_eq!(at, vec![0, S, 2 * S, 3 * S]);
        // A partial millisecond rounds the wait up.
        assert_eq!(core.next(&c, false, false, 3 * S + 1), Step::Wait(Some(1000)));
        assert_eq!(core.next(&c, false, false, 3 * S + 999), Step::Wait(Some(1000)));
        assert_eq!(core.next(&c, false, false, 4 * S - 1), Step::Wait(Some(1)));
    }

    #[test]
    fn core_stopped_waits_forever() {
        let c = Control { visible: false, ..live() };
        let mut core = Core::default();
        for t in [0, 5 * S, 90 * S] {
            assert_eq!(core.next(&c, false, false, t), Step::Wait(None));
        }
        assert_eq!(core.next(&c, true, false, 100 * S), Step::Sample);
        core.sampled(100 * S, 500);
        assert_eq!(core.next(&c, false, false, 100 * S), Step::Wait(None));
        assert_eq!(core.next(&c, false, false, 200 * S), Step::Wait(None));
    }

    #[test]
    fn core_restore_samples_at_once() {
        let mut c = live();
        let mut core = Core::default();
        assert_eq!(core.next(&c, false, false, 0), Step::Sample);
        core.sampled(0, 1_000);
        c.visible = false;
        assert_eq!(core.next(&c, false, false, 5 * S), Step::Wait(None));
        c.visible = true;
        // 10.3 s: the old due time is long past, but a restore is a start, not a late tick.
        assert_eq!(core.next(&c, false, false, 10 * S + 300 * MS), Step::Sample);
        core.sampled(10 * S + 300 * MS, 1_000);
        assert_eq!(core.next(&c, false, false, 10 * S + 300 * MS), Step::Wait(Some(1000)));
    }

    #[test]
    fn core_background_two_seconds() {
        let c = Control { foreground: false, ..live() };
        let mut core = Core::default();
        let mut at = Vec::new();
        let mut now = 0;
        while now <= 5 * S {
            if core.next(&c, false, false, now) == Step::Sample {
                at.push(now);
                core.sampled(now, 1_000);
            }
            now += 100 * MS;
        }
        assert_eq!(at, vec![0, 2 * S, 4 * S]);
    }

    #[test]
    fn core_slow_switches_to_two_seconds() {
        let c = live();
        let mut core = Core::default();
        assert_eq!(core.next(&c, false, false, 0), Step::Sample);
        for i in 0..5u64 {
            assert!(!core.slow.is_slow());
            core.sampled(i * S, 9_000);
        }
        assert!(core.slow.is_slow());
        // Last start at 4 s: the next is due at 6 s.
        assert_eq!(core.next(&c, false, false, 5 * S), Step::Wait(Some(1000)));
        assert_eq!(core.next(&c, false, false, 6 * S), Step::Sample);
        for i in 0..30u64 {
            core.sampled((6 + i) * S, 3_000);
        }
        assert!(!core.slow.is_slow());
        assert_eq!(core.next(&c, false, false, 35 * S + 500 * MS), Step::Wait(Some(500)));
        assert_eq!(core.next(&c, false, false, 36 * S), Step::Sample);
    }

    #[test]
    fn core_control_change_applies_on_the_next_call() {
        let mut c = live();
        let mut core = Core::default();
        assert_eq!(core.next(&c, false, false, 0), Step::Sample);
        core.sampled(0, 1_000);
        c.foreground = false;
        assert_eq!(core.next(&c, false, false, S), Step::Wait(Some(1000)));
        c.foreground = true;
        c.refresh = Refresh::Half;
        assert_eq!(core.next(&c, false, false, S), Step::Sample);
    }

    #[test]
    fn core_exit_wins() {
        let mut core = Core::default();
        assert_eq!(core.next(&live(), true, true, 0), Step::Exit);
        assert_eq!(core.next(&Control { visible: false, ..live() }, true, true, 0), Step::Exit);
    }

    // The thread, in real time with loose bounds.

    #[derive(Clone, Default)]
    struct CountSink(Arc<AtomicUsize>);

    impl FrameSink for CountSink {
        fn ready(&self) {
            self.0.fetch_add(1, SeqCst);
        }
    }

    struct Instant0;

    impl Sampler for Instant0 {
        fn need(&self) -> Need {
            Need::Procs
        }
        fn sample(&mut self, _now_us: u64, _out: &mut Frame) -> Result<(), SysmonError> {
            Ok(())
        }
    }

    /// Fills procs with the pids of call n; fails on call number \`fail_at\` (1-based) the way ProcSampler does.
    struct ListFake {
        calls: Vec<Vec<u32>>,
        n: usize,
        fail_at: Option<usize>,
    }

    impl Sampler for ListFake {
        fn need(&self) -> Need {
            Need::Procs
        }
        fn sample(&mut self, _now_us: u64, out: &mut Frame) -> Result<(), SysmonError> {
            let i = self.n.min(self.calls.len() - 1);
            self.n += 1;
            out.procs.clear();
            if self.fail_at == Some(self.n) {
                let e = SysmonError::Status(-7);
                out.error = Some(e);
                return Err(e);
            }
            for pid in &self.calls[i] {
                out.procs.push(ProcRow { key: key(*pid), ..ProcRow::default() });
            }
            out.procs.finish();
            Ok(())
        }
    }

    struct Blocking {
        entered: Arc<AtomicBool>,
        dropped: Arc<AtomicBool>,
        release: Receiver<()>,
    }

    impl Sampler for Blocking {
        fn need(&self) -> Need {
            Need::Procs
        }
        fn sample(&mut self, _now_us: u64, _out: &mut Frame) -> Result<(), SysmonError> {
            self.entered.store(true, SeqCst);
            let _ = self.release.recv();
            Ok(())
        }
    }

    impl Drop for Blocking {
        fn drop(&mut self) {
            self.dropped.store(true, SeqCst);
        }
    }

    fn key(pid: u32) -> ProcKey {
        ProcKey { pid, created: pid as u64 + 1 }
    }

    fn until(what: &str, mut f: impl FnMut() -> bool) {
        let t = Instant::now();
        while !f() {
            assert!(t.elapsed() < Duration::from_secs(5), "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn start(sampler: impl Sampler + 'static, control: Control) -> (SamplerHost, Arc<AtomicUsize>) {
        let sink = CountSink::default();
        let count = Arc::clone(&sink.0);
        (SamplerHost::start(Box::new(sink), vec![Box::new(sampler)], control).expect("start"), count)
    }

    #[test]
    fn stopped_host_never_wakes() {
        let (host, ready) = start(Instant0, Control { visible: false, ..live() });
        std::thread::sleep(Duration::from_secs(2));
        assert_eq!((host.stats().wakes, host.stats().iterations), (0, 0));
        assert_eq!(ready.load(SeqCst), 0);
        host.stop();
    }

    #[test]
    fn thread_smoke_cadence() {
        let (host, ready) = start(Instant0, live());
        std::thread::sleep(Duration::from_millis(2500));
        let it = host.stats().iterations;
        assert!((2..=4).contains(&it), "iterations {it}");
        assert_eq!(ready.load(SeqCst), 1, "no take, so one ready only");
        let mut f = Frame::default();
        assert!(host.take(&mut f));
        assert!(f.sys.mem.is_some() && f.sys.logical_cpus >= 1);
        assert!(f.t_us > 0 && f.need.has(Need::Procs));
        assert!(!host.take(&mut f));
        host.stop();
    }

    #[test]
    fn frames_coalesce() {
        let (host, ready) = start(Instant0, Control { refresh: Refresh::Paused, ..live() });
        for i in 1..=3u64 {
            host.sample_now();
            until("an iteration", || host.stats().iterations >= i);
        }
        assert_eq!(ready.load(SeqCst), 1);
        let mut f = Frame::default();
        assert!(host.take(&mut f));
        assert_eq!(f.seq, 3);
        assert!(!host.take(&mut f));
        // After a take the next frame announces itself again.
        host.sample_now();
        until("the fourth iteration", || host.stats().iterations >= 4);
        assert_eq!(ready.load(SeqCst), 2);
        host.stop();
    }

    #[test]
    fn set_control_wakes_the_thread() {
        let (host, _ready) = start(Instant0, Control { visible: false, ..live() });
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(host.stats().iterations, 0);
        host.set_control(live());
        until("the restore sample", || host.stats().iterations >= 1);
        host.set_control(Control { visible: false, ..live() });
        let wakes = {
            std::thread::sleep(Duration::from_millis(100));
            host.stats().wakes
        };
        std::thread::sleep(Duration::from_millis(1500));
        assert_eq!(host.stats().wakes, wakes, "hidden again: no more wake-ups");
        host.stop();
    }

    #[test]
    fn take_reports_added_and_gone() {
        let fake = ListFake { calls: vec![vec![1, 2, 3], vec![2, 3, 4]], n: 0, fail_at: None };
        let (host, _ready) = start(fake, Control { refresh: Refresh::Paused, ..live() });
        let mut f = Frame::default();
        host.sample_now();
        until("iteration 1", || host.stats().iterations >= 1);
        assert!(host.take(&mut f));
        assert_eq!((f.added.clone(), f.gone.clone()), (vec![key(1), key(2), key(3)], vec![]));
        assert_eq!(f.procs.len(), 3);
        host.sample_now();
        until("iteration 2", || host.stats().iterations >= 2);
        let first_t = f.t_us;
        assert!(host.take(&mut f));
        assert_eq!((f.added.clone(), f.gone.clone()), (vec![key(4)], vec![key(1)]));
        assert_eq!(f.procs.len(), 3);
        assert_eq!(f.dt_ms as u64, (f.t_us - first_t) / 1000);
        host.stop();
    }

    #[test]
    fn error_frame_keeps_the_consumers_procs() {
        let fake = ListFake { calls: vec![vec![1, 2, 3]], n: 0, fail_at: Some(2) };
        let (host, _ready) = start(fake, Control { refresh: Refresh::Paused, ..live() });
        let mut f = Frame::default();
        host.sample_now();
        until("iteration 1", || host.stats().iterations >= 1);
        assert!(host.take(&mut f));
        assert!(f.error.is_none() && !f.added.is_empty());
        f.paths = Arc::new(PathMap { entries: vec![(key(1), ImagePath::NoImage)] });
        let paths = Arc::clone(&f.paths);
        host.sample_now();
        until("iteration 2", || host.stats().iterations >= 2);
        assert!(host.take(&mut f));
        assert_eq!(f.seq, 2);
        assert_eq!(f.error, Some(SysmonError::Status(-7)));
        assert_eq!(f.procs.len(), 3);
        assert!(Arc::ptr_eq(&f.paths, &paths));
        assert!(f.added.is_empty() && f.gone.is_empty());
        // The sampler recovers: the next frame is a normal one against the consumer's procs.
        host.sample_now();
        until("iteration 3", || host.stats().iterations >= 3);
        assert!(host.take(&mut f));
        assert!(f.error.is_none());
        assert_eq!(f.procs.len(), 3);
        assert!(f.added.is_empty() && f.gone.is_empty());
        host.stop();
    }

    #[test]
    fn stop_returns_within_a_second_with_a_blocked_sampler() {
        let (entered, dropped) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
        let (tx, rx) = channel();
        let fake = Blocking { entered: Arc::clone(&entered), dropped: Arc::clone(&dropped), release: rx };
        let (host, ready) = start(fake, Control { refresh: Refresh::Paused, ..live() });
        host.sample_now();
        until("the sampler to be inside its call", || entered.load(SeqCst));
        let t = Instant::now();
        host.stop();
        assert!(t.elapsed() < Duration::from_millis(1500), "stop took {:?}", t.elapsed());
        assert!(!dropped.load(SeqCst), "the thread is detached inside the call");
        tx.send(()).unwrap();
        until("the detached thread to end", || dropped.load(SeqCst));
        assert_eq!(ready.load(SeqCst), 0, "a stopped host publishes nothing");
    }

    #[test]
    fn drop_stops_like_stop() {
        let dropped = Arc::new(AtomicBool::new(false));
        let (_tx, rx) = channel();
        let fake = Blocking { entered: Arc::new(AtomicBool::new(false)), dropped: Arc::clone(&dropped), release: rx };
        let (host, _ready) = start(fake, Control { visible: false, ..live() });
        drop(host);
        // An idle thread is joined, so its samplers are gone when drop returns.
        assert!(dropped.load(SeqCst));
    }
}
