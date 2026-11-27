//! The enumeration pool: two worker threads that start at the first submit, a job queue that waits without a timeout
//! (an idle pool has no wake-ups), and a supervisor thread that exists only while a job runs. A worker with no
//! progress for STUCK_AFTER_MS is marked stuck, its blocking call is cancelled once and a replacement starts while
//! fewer than MAX_THREADS threads exist. Network jobs start only while fewer than two workers are stuck; local jobs
//! never wait for them. `PoolStats::live` counts the workers that are not stuck.

use std::collections::VecDeque;
use std::os::windows::io::AsRawHandle;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::GetDriveTypeW;
use windows::Win32::System::IO::CancelSynchronousIo;

use super::enum_worker::{Enumerator, FindEnumerator};
use super::types::{Batch, CancelToken, TabId};
use super::{Gen, WidePath};

pub type BatchSink = std::sync::Arc<dyn Fn(TabId, Batch) + Send + Sync>;
pub type MakeEnumerator = std::sync::Arc<dyn Fn() -> Box<dyn Enumerator> + Send + Sync>;

pub struct EnumJob {
    pub tab: TabId,
    pub gen: Gen,
    pub dir: WidePath,
    pub cancel: CancelToken,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct PoolStats {
    pub live: u32,
    pub stuck: u32,
    pub queued_network: u32,
}

/// GetDriveTypeW's DRIVE_REMOTE (winbase.h; the constant sits behind a feature this package does not enable).
const DRIVE_REMOTE: u32 = 4;
/// Workers a healthy pool keeps.
const BASE_WORKERS: usize = 2;
/// Workers, stuck ones included, the pool never exceeds.
const MAX_THREADS: usize = 4;
/// No progress for this long marks a busy worker stuck.
const STUCK_AFTER_MS: u64 = 4_000;
/// Network jobs may start only while fewer workers than this are stuck.
const NETWORK_STUCK_LIMIT: usize = 2;
/// The supervisor's period.
const SUPERVISOR_PERIOD: Duration = Duration::from_secs(1);

struct Worker {
    id: u64,
    /// Milliseconds since the pool's epoch at the last batch flush (and at the start of a job).
    progress: Arc<AtomicU64>,
    busy: bool,
    stuck: bool,
    /// Keeps the thread handle valid for CancelSynchronousIo for as long as the worker is listed.
    thread: JoinHandle<()>,
}

#[derive(Default)]
struct State {
    workers: Vec<Worker>,
    started: bool,
    queue: VecDeque<EnumJob>,
    queued_network: VecDeque<EnumJob>,
    supervisor: bool,
    shutdown: bool,
    next_id: u64,
}

impl State {
    fn stuck(&self) -> usize {
        self.workers.iter().filter(|w| w.stuck).count()
    }

    fn healthy(&self) -> usize {
        self.workers.len() - self.stuck()
    }

    fn busy(&self) -> usize {
        self.workers.iter().filter(|w| w.busy).count()
    }

    fn remove(&mut self, id: u64) {
        self.workers.retain(|w| w.id != id);
    }
}

struct Shared {
    state: Mutex<State>,
    /// Workers wait here without a timeout.
    work: Condvar,
    /// The supervisor waits here, with its period as the timeout.
    supervisor_wake: Condvar,
    sink: BatchSink,
    make: MakeEnumerator,
    epoch: Instant,
    /// Timed waits that expired (only the supervisor waits with a timeout).
    timeouts: AtomicU64,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

pub struct EnumPool {
    shared: Arc<Shared>,
}

impl EnumPool {
    /// No thread until the first submit.
    pub fn new(sink: BatchSink) -> Self {
        Self::with_enumerator(sink, Arc::new(|| Box::new(FindEnumerator::default()) as Box<dyn Enumerator>))
    }

    /// Tests and probes inject fakes.
    pub fn with_enumerator(sink: BatchSink, make: MakeEnumerator) -> Self {
        EnumPool {
            shared: Arc::new(Shared {
                state: Mutex::new(State::default()),
                work: Condvar::new(),
                supervisor_wake: Condvar::new(),
                sink,
                make,
                epoch: Instant::now(),
                timeouts: AtomicU64::new(0),
            }),
        }
    }

    pub fn submit(&self, job: EnumJob) {
        let mut st = self.shared.lock();
        if !st.started {
            st.started = true;
            for _ in 0..BASE_WORKERS {
                spawn_worker(&self.shared, &mut st);
            }
        }
        st.queue.push_back(job);
        drop(st);
        self.shared.work.notify_one();
    }

    pub fn stats(&self) -> PoolStats {
        let st = self.shared.lock();
        PoolStats {
            live: st.healthy() as u32,
            stuck: st.stuck() as u32,
            queued_network: st.queued_network.len() as u32,
        }
    }

    /// Whether the supervisor thread exists, and how many timed waits have expired so far (workers never time out).
    #[cfg(test)]
    pub(crate) fn debug(&self) -> (bool, u64) {
        (self.shared.lock().supervisor, self.shared.timeouts.load(Ordering::Relaxed))
    }
}

impl Drop for EnumPool {
    /// Workers and the supervisor leave when they next look; one that is stuck in a call is left to finish alone.
    fn drop(&mut self) {
        let mut st = self.shared.lock();
        st.shutdown = true;
        st.queue.clear();
        st.queued_network.clear();
        drop(st);
        self.shared.work.notify_all();
        self.shared.supervisor_wake.notify_all();
    }
}

fn spawn_worker(sh: &Arc<Shared>, st: &mut State) {
    let id = st.next_id;
    st.next_id += 1;
    let progress = Arc::new(AtomicU64::new(sh.now_ms()));
    let shared = sh.clone();
    let p = progress.clone();
    let spawned = std::thread::Builder::new().name("files-enum".into()).spawn(move || worker_main(&shared, id, &p));
    if let Ok(thread) = spawned {
        // The thread blocks on the state lock the caller holds until it is listed.
        st.workers.push(Worker { id, progress, busy: false, stuck: false, thread });
    }
}

fn worker_main(sh: &Arc<Shared>, id: u64, progress: &AtomicU64) {
    let mut g = sh.lock();
    loop {
        if g.shutdown {
            g.remove(id);
            return;
        }
        // Local first, so a local job never waits behind a network one; then a parked network job if allowed.
        let mut job = None;
        if let Some(j) = g.queue.pop_front() {
            drop(g);
            let network = is_network(&j.dir);
            g = sh.lock();
            if network && g.stuck() >= NETWORK_STUCK_LIMIT {
                g.queued_network.push_back(j);
                continue;
            }
            job = Some(j);
        } else if g.stuck() < NETWORK_STUCK_LIMIT {
            job = g.queued_network.pop_front();
        }
        let Some(job) = job else {
            g = sh.work.wait(g).unwrap_or_else(|p| p.into_inner());
            continue;
        };

        progress.store(sh.now_ms(), Ordering::Relaxed);
        if let Some(w) = g.workers.iter_mut().find(|w| w.id == id) {
            w.busy = true;
        }
        ensure_supervisor(sh, &mut g);
        drop(g);

        let mut en = (sh.make)();
        let _ = catch_unwind(AssertUnwindSafe(|| {
            en.run(&job.dir, job.gen, &job.cancel, &mut |b| {
                progress.store(sh.now_ms(), Ordering::Relaxed);
                (sh.sink)(job.tab, b);
            });
        }));
        drop(en);

        g = sh.lock();
        let mut came_back = false;
        if let Some(w) = g.workers.iter_mut().find(|w| w.id == id) {
            w.busy = false;
            came_back = std::mem::take(&mut w.stuck);
        }
        if g.busy() == 0 {
            sh.supervisor_wake.notify_all();
        }
        if came_back {
            // A parked network job may start now.
            sh.work.notify_all();
            if g.healthy() > BASE_WORKERS {
                g.remove(id);
                return;
            }
        }
    }
}

fn ensure_supervisor(sh: &Arc<Shared>, st: &mut State) {
    if st.supervisor {
        return;
    }
    let shared = sh.clone();
    if std::thread::Builder::new().name("files-enum-watch".into()).spawn(move || supervisor_main(&shared)).is_ok() {
        st.supervisor = true;
    }
}

fn supervisor_main(sh: &Arc<Shared>) {
    let mut g = sh.lock();
    loop {
        if g.shutdown || g.busy() == 0 {
            g.supervisor = false;
            return;
        }
        let now = sh.now_ms();
        let mut newly_stuck = 0;
        for w in g.workers.iter_mut().filter(|w| w.busy && !w.stuck) {
            if now.saturating_sub(w.progress.load(Ordering::Relaxed)) >= STUCK_AFTER_MS {
                w.stuck = true;
                newly_stuck += 1;
                // SAFETY: the thread handle is owned by the listed worker's JoinHandle and valid while it is listed;
                // the call only cancels that thread's pending synchronous I/O, once.
                let _ = unsafe { CancelSynchronousIo(HANDLE(w.thread.as_raw_handle())) };
            }
        }
        for _ in 0..newly_stuck {
            if g.workers.len() < MAX_THREADS {
                spawn_worker(sh, &mut g);
            }
        }
        if newly_stuck > 0 {
            sh.work.notify_all();
        }
        let (guard, res) = sh.supervisor_wake.wait_timeout(g, SUPERVISOR_PERIOD).unwrap_or_else(|p| p.into_inner());
        g = guard;
        if res.timed_out() {
            sh.timeouts.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// UNC, or GetDriveTypeW == DRIVE_REMOTE; call off the UI thread.
pub fn is_network(dir: &WidePath) -> bool {
    if dir.is_unc() {
        return true;
    }
    let u = dir.units();
    if u.len() < 2 || u[1] != u16::from(b':') {
        return false;
    }
    let root = [u[0], u16::from(b':'), u16::from(b'\\'), 0];
    // SAFETY: `root` is a NUL-terminated drive root that lives for the call.
    unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) == DRIVE_REMOTE }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    use std::thread::{sleep, ThreadId};

    use super::*;

    type Behave = Arc<dyn Fn(&WidePath) + Send + Sync>;

    struct Fake(Behave);

    impl Enumerator for Fake {
        fn run(&mut self, dir: &WidePath, gen: Gen, _cancel: &CancelToken, out: &mut dyn FnMut(Batch)) {
            (self.0)(dir);
            out(Batch { gen, names: Vec::new(), entries: Vec::new(), done: true, error: None, elapsed_us: 0 });
        }
    }

    /// A pool of fakes; the sink records the tab of every finished job.
    fn pool(behave: impl Fn(&WidePath) + Send + Sync + 'static) -> (EnumPool, Arc<Mutex<Vec<u32>>>) {
        let done = Arc::new(Mutex::new(Vec::new()));
        let d = done.clone();
        let sink: BatchSink = Arc::new(move |tab, b| {
            if b.done {
                d.lock().unwrap().push(tab.0);
            }
        });
        let behave: Behave = Arc::new(behave);
        let make: MakeEnumerator = Arc::new(move || Box::new(Fake(behave.clone())) as Box<dyn Enumerator>);
        (EnumPool::with_enumerator(sink, make), done)
    }

    fn job(tab: u32, dir: &str) -> EnumJob {
        EnumJob { tab: TabId(tab), gen: Gen(1), dir: WidePath::parse(dir).unwrap(), cancel: CancelToken::new() }
    }

    fn wait_for(limit: Duration, mut ok: impl FnMut() -> bool) -> bool {
        let t = Instant::now();
        while t.elapsed() < limit {
            if ok() {
                return true;
            }
            sleep(Duration::from_millis(10));
        }
        ok()
    }

    /// Blocks until the flag is set (or 30 s).
    fn hold(release: &AtomicBool) {
        let t = Instant::now();
        while !release.load(Ordering::Acquire) && t.elapsed() < Duration::from_secs(30) {
            sleep(Duration::from_millis(10));
        }
    }

    fn done_has(done: &Arc<Mutex<Vec<u32>>>, tab: u32) -> bool {
        done.lock().unwrap().contains(&tab)
    }

    fn settled() -> PoolStats {
        PoolStats { live: 2, stuck: 0, queued_network: 0 }
    }

    #[test]
    fn jobs_run_on_two_threads() {
        let arrived = Arc::new(AtomicUsize::new(0));
        let threads = Arc::new(Mutex::new(HashSet::<ThreadId>::new()));
        let (a, t) = (arrived.clone(), threads.clone());
        let (p, done) = pool(move |_| {
            t.lock().unwrap().insert(std::thread::current().id());
            a.fetch_add(1, Ordering::SeqCst);
            // Each job waits for the other: they finish only if two threads run them at once.
            let start = Instant::now();
            while a.load(Ordering::SeqCst) < 2 && start.elapsed() < Duration::from_secs(5) {
                sleep(Duration::from_millis(5));
            }
        });
        assert_eq!(p.stats(), PoolStats::default(), "no worker before the first submit");
        p.submit(job(1, "C:\\one"));
        p.submit(job(2, "C:\\two"));
        assert!(wait_for(Duration::from_secs(5), || done.lock().unwrap().len() == 2));
        assert_eq!(threads.lock().unwrap().len(), 2);
        assert_eq!(p.stats(), settled());
    }

    #[test]
    fn a_stuck_job_gets_a_replacement() {
        let (p, done) = pool(|dir| {
            if dir.to_string_lossy().ends_with("block") {
                sleep(Duration::from_secs(6));
            }
        });
        let t0 = Instant::now();
        p.submit(job(1, "C:\\block"));
        p.submit(job(2, "C:\\block"));
        sleep(Duration::from_millis(200)); // both workers are inside their jobs
        p.submit(job(3, "C:\\quick"));
        assert!(wait_for(Duration::from_secs(5), || done_has(&done, 3)), "the quick job needs a replacement worker");
        assert!(t0.elapsed() < Duration::from_secs(5) && t0.elapsed() >= Duration::from_secs(3));
        let s = p.stats();
        assert!(s.stuck >= 1 && s.live + s.stuck <= 4, "{s:?}");
        // They come back at 6 s: the pool settles to two healthy workers.
        assert!(wait_for(Duration::from_secs(5), || done_has(&done, 1) && done_has(&done, 2)));
        assert!(wait_for(Duration::from_secs(3), || p.stats() == settled()));
    }

    #[test]
    fn at_most_four_threads() {
        let release = Arc::new(AtomicBool::new(false));
        let (now, peak) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let (r, n, pk) = (release.clone(), now.clone(), peak.clone());
        let (p, done) = pool(move |_| {
            let k = n.fetch_add(1, Ordering::SeqCst) + 1;
            pk.fetch_max(k, Ordering::SeqCst);
            hold(&r);
            n.fetch_sub(1, Ordering::SeqCst);
        });
        for i in 0..8 {
            p.submit(job(i, "C:\\hold"));
        }
        // The first pair is stuck at 4 s and replaced; the second pair at 8 s and not replaced: the cap holds.
        sleep(Duration::from_millis(9_500));
        let s = p.stats();
        assert_eq!((s.live + s.stuck, s.stuck), (4, 4), "{s:?}");
        assert_eq!(peak.load(Ordering::SeqCst), 4);
        release.store(true, Ordering::Release);
        assert!(wait_for(Duration::from_secs(5), || done.lock().unwrap().len() == 8));
        assert!(wait_for(Duration::from_secs(3), || p.stats() == settled()));
        assert_eq!(peak.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn network_waits_when_two_stuck() {
        let release = Arc::new(AtomicBool::new(false));
        let r = release.clone();
        let (p, done) = pool(move |dir| {
            if dir.to_string_lossy().ends_with("hold") {
                hold(&r);
            }
        });
        p.submit(job(1, "C:\\hold"));
        p.submit(job(2, "C:\\hold"));
        assert!(wait_for(Duration::from_secs(6), || p.stats().stuck == 2), "{:?}", p.stats());
        p.submit(job(3, "\\\\server\\share\\dir"));
        p.submit(job(4, "C:\\local"));
        assert!(wait_for(Duration::from_secs(2), || done_has(&done, 4)), "a local job never waits for a network one");
        assert!(wait_for(Duration::from_secs(2), || p.stats().queued_network == 1), "{:?}", p.stats());
        sleep(Duration::from_millis(300));
        assert!(!done_has(&done, 3), "the network job waits while two workers are stuck");
        release.store(true, Ordering::Release);
        assert!(wait_for(Duration::from_secs(3), || done_has(&done, 3)), "it starts when a stuck worker is back");
        assert_eq!(p.stats().queued_network, 0);
    }

    #[test]
    fn idle_pool_has_no_supervisor() {
        let (p, done) = pool(|_| sleep(Duration::from_millis(50)));
        for i in 0..4 {
            p.submit(job(i, "C:\\x"));
        }
        assert!(wait_for(Duration::from_secs(5), || done.lock().unwrap().len() == 4));
        assert!(wait_for(Duration::from_secs(2), || !p.debug().0), "the supervisor leaves when no job runs");
        let before = p.debug().1;
        sleep(Duration::from_secs(2));
        let (supervisor, timeouts) = p.debug();
        assert!(!supervisor);
        assert_eq!(timeouts, before, "idle workers wait on the condvar without a timeout");
        assert_eq!(p.stats(), settled());
    }

    #[test]
    fn network_is_unc_or_a_remote_drive() {
        assert!(is_network(&WidePath::parse("\\\\server\\share").unwrap()));
        assert!(is_network(&WidePath::parse("\\\\server\\share\\a\\b").unwrap()));
        let sys = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        assert!(!is_network(&WidePath::parse(&format!("{sys}\\")).unwrap()), "the system drive is local");
    }
}
