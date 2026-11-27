//! The shell host's STAs (SH4b): a menu STA (OleInitialize), an exec STA and two image STAs, each a thread with a
//! message loop and a work queue woken by PostThreadMessageW. Requests are routed by kind; image requests go round
//! robin. An STA retires after 50 calls (the menu STA after 50 menus) once its call has ended, and its successor
//! starts on demand on the same queue and the same crumbs slot, so leaks of third-party code do not pile up. The
//! message loop's dispatch (COM's calls into the apartment, the extensions' own windows) and the uninitialization at
//! retirement (releases, DllCanUnloadNow, DLL detach) run third-party code too: each inside a crumb of the STA's kind.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use oled_trust::frame::files::{CallKind, Gen, ReqId, ShellReq};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE};
use windows::Win32::System::Ole::{OleInitialize, OleUninitialize};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, PeekMessageW, PostThreadMessageW, TranslateMessage, MSG, PM_NOREMOVE, WM_APP,
};

use crate::shell::crumbs;

/// Jobs running now on the STAs (a menu on screen is one): the idle exit waits for them.
static RUNNING: AtomicU32 = AtomicU32::new(0);

/// The jobs running now on the STAs.
pub fn jobs_running() -> u32 {
    RUNNING.load(Ordering::Acquire)
}

/// Counts one job while it runs (a panic that unwinds out of the job uncounts it too).
struct Running;

impl Running {
    fn start() -> Running {
        RUNNING.fetch_add(1, Ordering::AcqRel);
        Running
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        RUNNING.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Calls after which an STA retires.
pub const RETIRE_AFTER: u32 = 50;
/// The thread message that wakes an STA for its queue.
const WM_WORK: u32 = WM_APP + 1;

/// Where a request runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sta {
    Menu,
    Exec,
    Image(u8),
}

/// The STA of `req`; `next_image` is the round-robin counter of image requests. None for CancelGen and Bye,
/// which the reader handles itself.
pub fn route(req: &ShellReq, next_image: &mut u8) -> Option<Sta> {
    use ShellReq::*;
    match req {
        Menu { .. } | ModernItems { .. } => Some(Sta::Menu),
        Exec { .. } | RunTyped { .. } | ShellNewTypes { .. } | QuickAccessPins { .. } => Some(Sta::Exec),
        Icons { .. } | Thumbs { .. } | ClassIcons { .. } | TypeNames { .. } => {
            let i = *next_image % 2;
            *next_image = next_image.wrapping_add(1);
            Some(Sta::Image(i))
        }
        CancelGen { .. } | Bye => None,
    }
}

/// Counts an STA's calls; due after `limit`.
#[derive(Clone, Copy, Debug)]
pub struct Retire {
    calls: u32,
    limit: u32,
}

impl Retire {
    pub fn new(limit: u32) -> Retire {
        Retire { calls: 0, limit }
    }

    /// Counts one call; true when the STA should retire after it.
    pub fn count(&mut self) -> bool {
        self.calls += 1;
        self.calls >= self.limit
    }
}

/// One unit of work on an STA. `gen` and `req` are set for image work, which CancelGen may drop.
pub struct Job {
    pub gen: Option<Gen>,
    pub req: Option<ReqId>,
    pub run: Box<dyn FnOnce() + Send>,
}

/// An STA's queue (pure).
#[derive(Default)]
pub struct Queue {
    jobs: VecDeque<Job>,
}

impl Queue {
    pub fn push(&mut self, j: Job) {
        self.jobs.push_back(j);
    }

    pub fn pop(&mut self) -> Option<Job> {
        self.jobs.pop_front()
    }

    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    /// Drops queued work of generations older than `gen`; the requests dropped, in order.
    pub fn drop_older(&mut self, gen: Gen) -> Vec<ReqId> {
        let mut dropped = Vec::new();
        self.jobs.retain(|j| match j.gen {
            Some(g) if g < gen => {
                dropped.extend(j.req);
                false
            }
            _ => true,
        });
        dropped
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Thread {
    None,
    Starting,
    Running(u32),
}

struct State {
    queue: Queue,
    thread: Thread,
}

struct Shared {
    name: &'static str,
    slot: u8,
    /// The crumb kind of the dispatch and the uninitialization.
    kind: CallKind,
    ole: bool,
    limit: u32,
    state: Mutex<State>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// One STA slot: its queue and, while work exists, its thread.
#[derive(Clone)]
pub struct StaSlot {
    shared: Arc<Shared>,
}

impl StaSlot {
    /// A slot whose thread binds crumbs slot `slot` and initializes OLE (the menu STA) or COM; `kind` is the crumb of
    /// the third-party code its loop runs outside a request (its deadline, if it has one, applies).
    pub fn new(name: &'static str, slot: u8, kind: CallKind, ole: bool, limit: u32) -> StaSlot {
        StaSlot {
            shared: Arc::new(Shared {
                name,
                slot,
                kind,
                ole,
                limit,
                state: Mutex::new(State { queue: Queue::default(), thread: Thread::None }),
            }),
        }
    }

    /// Queues `job` and wakes the thread, starting one when none runs.
    pub fn post(&self, job: Job) {
        let wake = {
            let mut st = self.shared.lock();
            st.queue.push(job);
            match st.thread {
                Thread::None => {
                    st.thread = Thread::Starting;
                    None
                }
                Thread::Starting => return,
                Thread::Running(tid) => Some(tid),
            }
        };
        match wake {
            // SAFETY: a thread message with no pointers; a thread that is ending loses it, and then finds the job in
            // its queue before it ends.
            Some(tid) => {
                let _ = unsafe { PostThreadMessageW(tid, WM_WORK, Default::default(), Default::default()) };
            }
            None => start(self.shared.clone()),
        }
    }

    /// Drops queued work older than `gen` (CancelGen); the requests dropped.
    pub fn drop_older(&self, gen: Gen) -> Vec<ReqId> {
        self.shared.lock().queue.drop_older(gen)
    }
}

fn start(shared: Arc<Shared>) {
    let name = shared.name;
    let spawned = std::thread::Builder::new().name(name.into()).stack_size(1 << 20).spawn(move || sta_main(&shared));
    if let Err(e) = spawned {
        oled_sys::log!("shell: cannot start the {name} STA ({e})");
    }
}

/// An STA thread: COM or OLE, its crumbs slot, the message loop, the queue; retires after its limit.
fn sta_main(sh: &Arc<Shared>) {
    // SAFETY: once per thread, undone below on the same thread.
    let inited = unsafe {
        if sh.ole {
            OleInitialize(None).is_ok()
        } else {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE).is_ok()
        }
    };
    crumbs::bind(sh.slot);
    let mut msg = MSG::default();
    // SAFETY: creates this thread's message queue so PostThreadMessageW reaches it; `msg` is writable.
    let _ = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE) };
    // SAFETY: no preconditions.
    let tid = unsafe { GetCurrentThreadId() };
    sh.lock().thread = Thread::Running(tid);
    let mut retire = Retire::new(sh.limit);
    'life: loop {
        // Work first: a wake lost inside a modal loop of a job is covered here.
        loop {
            let job = sh.lock().queue.pop();
            let Some(job) = job else { break };
            let running = Running::start();
            (job.run)();
            drop(running);
            if retire.count() {
                break 'life;
            }
        }
        // SAFETY: `msg` is writable; the loop runs on the thread that owns the queue.
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if got.0 <= 0 {
            break;
        }
        if msg.hwnd.0.is_null() && msg.message == WM_WORK {
            continue;
        }
        // SAFETY: a message this thread just received.
        unsafe {
            let _ = TranslateMessage(&msg);
            let _g = crumbs::enter(sh.kind, "");
            DispatchMessageW(&msg);
        }
    }
    if inited {
        let _g = crumbs::enter(sh.kind, "");
        // SAFETY: matches the successful initialization above, on the same thread.
        unsafe {
            if sh.ole {
                OleUninitialize();
            } else {
                CoUninitialize();
            }
        }
    }
    crumbs::unbind();
    let successor = {
        let mut st = sh.lock();
        if st.queue.is_empty() {
            st.thread = Thread::None;
            false
        } else {
            st.thread = Thread::Starting;
            true
        }
    };
    if successor {
        start(sh.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oled_trust::frame::files::{ItemRef, Verb, WidePath};
    use std::time::Duration;

    fn job(gen: Option<u64>, req: u32) -> Job {
        Job { gen: gen.map(Gen), req: Some(ReqId(req)), run: Box::new(|| {}) }
    }

    #[test]
    fn route_table() {
        let p = || vec![ItemRef::Path(WidePath::parse(r"C:\a.txt").unwrap())];
        let mut n = 0u8;
        let r = ReqId(1);
        let cases = [
            (ShellReq::Menu { req: r, items: p(), at: (0, 0), extended: false, owner: 0 }, Some(Sta::Menu)),
            (ShellReq::ModernItems { req: r, items: p() }, Some(Sta::Menu)),
            (ShellReq::Exec { req: r, verb: Verb::Open, items: p() }, Some(Sta::Exec)),
            (ShellReq::RunTyped { req: r, text: "x".into(), dir: WidePath::parse(r"C:\").unwrap() }, Some(Sta::Exec)),
            (ShellReq::ShellNewTypes { req: r }, Some(Sta::Exec)),
            (ShellReq::QuickAccessPins { req: r }, Some(Sta::Exec)),
            (ShellReq::Icons { req: r, gen: Gen(1), items: p(), px: 16 }, Some(Sta::Image(0))),
            (ShellReq::Thumbs { req: r, gen: Gen(1), items: p(), px: 96, cache_only: false }, Some(Sta::Image(1))),
            (ShellReq::ClassIcons { req: r, exts: vec!["txt".into()], px: 16 }, Some(Sta::Image(0))),
            (ShellReq::TypeNames { req: r, exts: vec!["txt".into()] }, Some(Sta::Image(1))),
            (ShellReq::CancelGen { gen: Gen(2) }, None),
            (ShellReq::Bye, None),
        ];
        for (req, want) in cases {
            assert_eq!(route(&req, &mut n), want, "{req:?}");
        }
        assert_eq!(n, 4, "four image requests counted");
    }

    #[test]
    fn retirement_counter() {
        let mut r = Retire::new(RETIRE_AFTER);
        for _ in 0..RETIRE_AFTER - 1 {
            assert!(!r.count());
        }
        assert!(r.count(), "the 50th call retires");
        assert_eq!(RETIRE_AFTER, 50);
    }

    #[test]
    fn cancel_gen_drops_older() {
        let mut q = Queue::default();
        q.push(job(Some(1), 10));
        q.push(job(None, 11));
        q.push(job(Some(2), 12));
        q.push(job(Some(1), 13));
        q.push(job(Some(3), 14));
        assert_eq!(q.drop_older(Gen(2)), [ReqId(10), ReqId(13)]);
        let left: Vec<_> = std::iter::from_fn(|| q.pop()).map(|j| j.req).collect();
        assert_eq!(left, [Some(ReqId(11)), Some(ReqId(12)), Some(ReqId(14))]);
        assert!(q.is_empty());
    }

    #[test]
    fn sta_runs_a_posted_job() {
        use windows::Win32::System::Com::{
            CoGetApartmentType, APTTYPE, APTTYPEQUALIFIER, APTTYPE_MAINSTA, APTTYPE_STA,
        };
        let slot = StaSlot::new("test-sta", crumbs::SLOT_EXEC, CallKind::Exec, false, 3);
        let (tx, rx) = std::sync::mpsc::channel();
        for i in 0..7u32 {
            let tx = tx.clone();
            slot.post(Job {
                gen: None,
                req: None,
                run: Box::new(move || {
                    let (mut t, mut q) = (APTTYPE::default(), APTTYPEQUALIFIER::default());
                    // SAFETY: two writable out values.
                    let ok = unsafe { CoGetApartmentType(&mut t, &mut q) }.is_ok();
                    let tid = unsafe { GetCurrentThreadId() };
                    let _ = tx.send((i, ok && (t == APTTYPE_STA || t == APTTYPE_MAINSTA), tid));
                }),
            });
        }
        let got: Vec<_> = (0..7).map(|_| rx.recv_timeout(Duration::from_secs(10)).expect("a job ran")).collect();
        assert_eq!(got.iter().map(|g| g.0).collect::<Vec<_>>(), (0..7).collect::<Vec<_>>(), "in order");
        assert!(got.iter().all(|g| g.1), "every job ran in a single-threaded apartment");
        let threads: std::collections::BTreeSet<_> = got.iter().map(|g| g.2).collect();
        assert!(threads.len() >= 3, "a new STA after each 3 calls: {threads:?}");
    }
}
