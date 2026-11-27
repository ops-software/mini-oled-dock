//! Small OS reads off the UI thread (P3-files-app 2.2, F1a.6): free space, drive facts, children, stats. Each lane has
//! one lazily started thread that waits on a condition variable without a timeout, so an idle lane never wakes. A
//! network job running over 4 s is abandoned and a fresh network thread takes the next job; at most 2 threads are
//! abandoned at a time, after that network jobs queue until one returns.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use crate::drives::DriveCache;
use crate::hub::Hub;
use crate::listing::WidePath;
use crate::msg::{JobDone, Msg};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lane {
    Local,
    Network,
}

pub type JobFn = Box<dyn FnOnce() -> Option<JobDone> + Send>;

/// A network job running longer than this is abandoned.
const ABANDON_AFTER: Duration = Duration::from_secs(4);
/// Threads abandoned (still running a stuck job) at once; past it network jobs queue.
const MAX_ABANDONED: u32 = 2;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Default)]
struct LaneState {
    jobs: VecDeque<(u32, JobFn)>,
    /// The id of the thread that takes jobs; None: none yet, or it was abandoned.
    worker: Option<u64>,
    next_id: u64,
    threads: u32,
    abandoned: u32,
    closed: bool,
}

struct LaneQ {
    network: bool,
    st: Mutex<LaneState>,
    cv: Condvar,
}

struct Inner {
    hub: Arc<Hub>,
    local: LaneQ,
    net: LaneQ,
    /// Loop turns of every worker (a test reads it to see that an idle lane stays still).
    turns: AtomicU64,
}

pub struct Bg {
    inner: Arc<Inner>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BgStats {
    pub threads: u32,
    pub abandoned: u32,
    pub queued: u32,
}

impl Inner {
    fn lane(&self, lane: Lane) -> &LaneQ {
        match lane {
            Lane::Local => &self.local,
            Lane::Network => &self.net,
        }
    }
}

/// Starts the lane's thread when jobs wait and it has none (never started, or abandoned).
fn ensure_worker(inner: &Arc<Inner>, lane: Lane, st: &mut LaneState) {
    if st.worker.is_some() || st.closed || st.jobs.is_empty() {
        return;
    }
    let id = st.next_id;
    st.next_id += 1;
    st.worker = Some(id);
    let me = Arc::clone(inner);
    let spawned = std::thread::Builder::new().name("files-bg".into()).spawn(move || worker(me, lane, id));
    if spawned.is_ok() {
        st.threads += 1;
    } else {
        st.worker = None;
    }
}

fn worker(inner: Arc<Inner>, lane: Lane, id: u64) {
    let q = inner.lane(lane);
    loop {
        let (win, job) = {
            let mut st = lock(&q.st);
            loop {
                inner.turns.fetch_add(1, Ordering::Relaxed);
                if st.closed || st.worker != Some(id) {
                    st.threads = st.threads.saturating_sub(1);
                    return;
                }
                if let Some(j) = st.jobs.pop_front() {
                    break j;
                }
                st = q.cv.wait(st).unwrap_or_else(PoisonError::into_inner);
            }
        };
        let finished = Arc::new((Mutex::new(false), Condvar::new()));
        if q.network {
            let (me, f) = (Arc::clone(&inner), Arc::clone(&finished));
            // The supervisor exists only while a network job runs.
            let _ = std::thread::Builder::new().name("files-bg-watch".into()).spawn(move || supervise(me, id, f));
        }
        let out = job();
        {
            let (flag, cv) = &*finished;
            *lock(flag) = true;
            cv.notify_all();
        }
        if let Some(done) = out {
            inner.hub.post(Msg::Job { win, done });
        }
        let mut st = lock(&q.st);
        if st.worker != Some(id) {
            // Abandoned while it ran: this thread is done; the lane may start a fresh one.
            st.abandoned = st.abandoned.saturating_sub(1);
            st.threads = st.threads.saturating_sub(1);
            ensure_worker(&inner, lane, &mut st);
            return;
        }
    }
}

/// Waits for the network job of thread `id` to end; every 4 s it is still running, the thread is abandoned (when
/// fewer than MAX_ABANDONED are) and a fresh one is started for the queue.
fn supervise(inner: Arc<Inner>, id: u64, finished: Arc<(Mutex<bool>, Condvar)>) {
    let (flag, cv) = &*finished;
    let mut ended = lock(flag);
    loop {
        if *ended {
            return;
        }
        let (g, t) = cv.wait_timeout(ended, ABANDON_AFTER).unwrap_or_else(PoisonError::into_inner);
        ended = g;
        if *ended {
            return;
        }
        if !t.timed_out() {
            continue;
        }
        let mut st = lock(&inner.net.st);
        if st.worker == Some(id) && st.abandoned < MAX_ABANDONED {
            st.worker = None;
            st.abandoned += 1;
            ensure_worker(&inner, Lane::Network, &mut st);
            return;
        }
        if st.worker != Some(id) {
            return;
        }
    }
}

impl Bg {
    pub fn new(hub: Arc<Hub>) -> Bg {
        let lane = |network| LaneQ { network, st: Mutex::new(LaneState::default()), cv: Condvar::new() };
        Bg { inner: Arc::new(Inner { hub, local: lane(false), net: lane(true), turns: AtomicU64::new(0) }) }
    }

    /// Runs `job` off the UI thread, after the earlier jobs of its lane, and posts Msg::Job { win, done } when it
    /// returns a result.
    pub fn run(&self, win: u32, lane: Lane, job: JobFn) {
        let q = self.inner.lane(lane);
        let mut st = lock(&q.st);
        st.jobs.push_back((win, job));
        ensure_worker(&self.inner, lane, &mut st);
        q.cv.notify_one();
    }

    pub fn stats(&self) -> BgStats {
        let (a, b) = (lock(&self.inner.local.st), lock(&self.inner.net.st));
        BgStats { threads: a.threads + b.threads, abandoned: b.abandoned, queued: (a.jobs.len() + b.jobs.len()) as u32 }
    }

    /// Loop turns of every worker so far: constant while the lanes are idle.
    pub fn turns(&self) -> u64 {
        self.inner.turns.load(Ordering::Relaxed)
    }
}

impl Drop for Bg {
    fn drop(&mut self) {
        for q in [&self.inner.local, &self.inner.net] {
            lock(&q.st).closed = true;
            q.cv.notify_all();
        }
    }
}

/// UNC -> Network; a drive letter: the cache's lane (drives.rs fills it from GetDriveTypeW); an unknown letter counts
/// as Network.
pub fn lane_of(p: &WidePath, drives: &DriveCache) -> Lane {
    if p.is_unc() {
        return Lane::Network;
    }
    let letter = p.units().first().and_then(|&u| u8::try_from(u).ok()).map(|b| b.to_ascii_uppercase());
    letter.and_then(|l| drives.lane(l)).unwrap_or(Lane::Network)
}
