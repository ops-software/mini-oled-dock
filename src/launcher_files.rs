//! The launcher's file search in the dock (plan L2-F3): the pump thread between the session and the files helper child
//! (`dock.exe --launcher-files`). The UI thread only sends: a requery's request (`request`, a channel send; the first
//! request of an open starts the thread) and the close (`end`). Each pump belongs to one open and tags its posts with
//! that open's number, so what it posts after its close (a kill's reap can take 2 s) never reaches a later open. The
//! thread runs `files::Pump`'s policy on its own clock and does its acts: it spawns the helper (the dock's exe, token
//! and job; stdin and stdout piped, stderr null, no window), writes one request line at a time, kills and reaps it, and
//! posts the rows, built here (re-sorted and capped), to the UI as `ShellEvent::LauncherFiles`. One reader thread per
//! helper decodes its stdout with `files::FrameReader`. Never on the UI thread: a spawn, a write, a read, a decode, a
//! sort or a wait. While the launcher is closed no thread and no helper exist (D2). This file makes no Win32 call of
//! its own; its logs carry kinds, generations and counts only.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use crate::launcher::files::{self, Act, FilesEvent, Frame, FrameError, FrameReader, KillWhy, Limits, Pump, Req};
use crate::log;
use crate::shell_thread::ShellEvent;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::{PipeReader, Write};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed, Ordering::SeqCst};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// What the pump thread hears.
pub(crate) enum Msg {
    /// A requery's request (None withdraws a waiting one).
    Request(Option<Req>),
    /// The launcher closed.
    Close,
    /// What the reader of helper n decoded.
    Frame(u32, Result<Option<Frame>, FrameError>),
}

/// The pump's inbox: the UI thread's requests and close and the readers' frames, in their order. A mutex and a condition
/// variable: one reader, and less code than a channel.
#[derive(Default)]
pub(crate) struct Inbox {
    q: Mutex<VecDeque<Msg>>,
    cv: Condvar,
}

impl Inbox {
    pub(crate) fn push(&self, m: Msg) {
        if let Ok(mut q) = self.q.lock() {
            q.push_back(m);
        }
        self.cv.notify_one();
    }

    /// The next message, waiting at most `ms` (None: as long as it takes); None when the time is up.
    fn pop(&self, ms: Option<u64>) -> Option<Msg> {
        let until = ms.and_then(|ms| Instant::now().checked_add(Duration::from_millis(ms)));
        let Ok(mut q) = self.q.lock() else { return Some(Msg::Close) };
        loop {
            if let Some(m) = q.pop_front() {
                return Some(m);
            }
            let waited = match until {
                None => self.cv.wait(q).ok(),
                Some(t) => {
                    let left = t.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return None;
                    }
                    self.cv.wait_timeout(q, left).ok().map(|(q, _)| q)
                }
            };
            let Some(next) = waited else { return Some(Msg::Close) };
            q = next;
        }
    }
}

thread_local! {
    /// The open's pump while its thread runs, with the open's number (the UI thread's only).
    static PUMP: RefCell<Option<(u32, Arc<Inbox>)>> = const { RefCell::new(None) };
}

const PUMP_STACK: usize = 128 * 1024;
const READER_STACK: usize = 64 * 1024;
/// A killed helper is waited for this long, in 10 ms steps; then its handle goes and the dock's job ends it.
const REAP_MS: u64 = 2_000;
const STEP_MS: u64 = 10;

/// True the first time for `flag`: a failure is logged once per process.
fn first(flag: &AtomicBool) -> bool {
    !flag.swap(true, Relaxed)
}

fn ms_since(t: Instant) -> u64 {
    u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// A requery's files request (`Effect::RequestFiles`) of open `open`, on the UI thread: with no pump of that open, None
/// does nothing and Some starts the thread (a pump of another open, whose close never came, is ended first); otherwise
/// one channel send. Never waits. `ui` is the window the pump's posts go to.
pub(crate) fn request(open: u32, r: Option<Req>, ui: isize) {
    PUMP.with(|p| {
        let Ok(mut p) = p.try_borrow_mut() else { return };
        match p.as_ref() {
            Some((id, inbox)) if *id == open => inbox.push(Msg::Request(r)),
            _ => {
                if let Some((_, old)) = p.take() {
                    old.push(Msg::Close);
                }
                *p = r.and_then(|r| start(open, r, ui)).map(|inbox| (open, inbox));
            }
        }
    });
}

/// The launcher closed (every close, `SysPort::files_end`): the pump retires its helper and ends. Never waits.
pub(crate) fn end() {
    PUMP.with(|p| {
        if let Some((_, inbox)) = p.try_borrow_mut().ok().and_then(|mut p| p.take()) {
            inbox.push(Msg::Close);
        }
    });
}

/// The pump thread of open `open`, with its first request queued.
fn start(open: u32, r: Req, ui: isize) -> Option<Arc<Inbox>> {
    let inbox = Arc::new(Inbox::default());
    inbox.push(Msg::Request(Some(r)));
    let own = inbox.clone();
    let body = move || {
        let mut post = |ev| crate::shell_thread::push_event(ui, ShellEvent::LauncherFiles(open, ev));
        run_pump(&own, &mut RealSpawner, &mut post, files::LIMITS);
        crate::mem::rearm();
    };
    match std::thread::Builder::new().name("launcher-files".into()).stack_size(PUMP_STACK).spawn(body) {
        Ok(_) => Some(inbox),
        Err(e) => {
            static WARNED: AtomicBool = AtomicBool::new(false);
            if first(&WARNED) {
                log!("launcher files: the files thread did not start ({e}); files are off for this open");
            }
            let off = FilesEvent::Off(files::Why::Unavailable);
            crate::shell_thread::push_event(ui, ShellEvent::LauncherFiles(open, off));
            None
        }
    }
}

/// One helper child as the pump drives it (a test double in the tests).
pub(crate) trait Helper {
    /// One request line to its stdin; false when the pipe failed (its end of output follows).
    fn write_line(&mut self, line: &[u8]) -> bool;
    /// Ends it at once.
    fn kill(&mut self);
    /// Closes its stdin: it ends by itself.
    fn close_stdin(&mut self);
    /// It has ended (or cannot be waited on).
    fn ended(&mut self) -> bool;
    /// Its private bytes, for the probe's lines.
    #[cfg(feature = "owner-probe")]
    fn private_bytes(&self) -> Option<usize>;
}

/// Starts helpers: the child and its stdout.
pub(crate) trait Spawner {
    fn spawn(&mut self) -> std::io::Result<(Box<dyn Helper>, PipeReader)>;
}

/// `dock.exe --launcher-files` through `update::spawn_in_job`, from the exe `update::child_exe` trusts.
struct RealSpawner;

impl Spawner for RealSpawner {
    fn spawn(&mut self) -> std::io::Result<(Box<dyn Helper>, PipeReader)> {
        let exe = crate::update::child_exe().map_err(std::io::Error::other)?;
        #[cfg(feature = "owner-probe")]
        let stall = crate::launcher_probe::files_stall_ms().map(|ms| ms.to_string());
        #[cfg(not(feature = "owner-probe"))]
        let stall: Option<String> = None;
        let mut args = vec!["--launcher-files"];
        if let Some(ms) = &stall {
            args.extend(["--stall", ms.as_str()]);
        }
        let mut child = crate::update::spawn_in_job(&exe, &args, true)?;
        let out = child.stdout.take().ok_or_else(|| std::io::Error::other("no stdout"))?;
        Ok((Box::new(RealHelper(child)), out))
    }
}

struct RealHelper(crate::update::JobChild);

impl Helper for RealHelper {
    fn write_line(&mut self, line: &[u8]) -> bool {
        self.0.stdin.as_mut().is_some_and(|w| w.write_all(line).and_then(|()| w.flush()).is_ok())
    }

    fn kill(&mut self) {
        let _ = self.0.kill();
    }

    fn close_stdin(&mut self) {
        self.0.stdin = None;
    }

    fn ended(&mut self) -> bool {
        !matches!(self.0.try_wait(), Ok(None))
    }

    #[cfg(feature = "owner-probe")]
    fn private_bytes(&self) -> Option<usize> {
        use std::os::windows::io::AsRawHandle;
        #[cfg(feature = "owner-probe")]
        let bytes = crate::launcher_probe::process_private(self.0.as_raw_handle());
        bytes
    }
}

/// The live helper: its number, the child and the clocks the log and the probe read.
struct Live {
    n: u32,
    child: Box<dyn Helper>,
    spawned: Instant,
    /// The request written last and when.
    written: Option<(u64, Instant)>,
    /// The frames the dock asked for: the state frame and one per line written (its reader stops past them).
    asked_frames: Arc<AtomicU32>,
    /// Spawn to state 0, until the first rows frame takes it (the probe's `spawn {m} ms`).
    #[cfg(feature = "owner-probe")]
    to_state: Option<u64>,
    #[cfg(feature = "owner-probe")]
    rows_seen: bool,
}

static SPAWN_FAILED: AtomicBool = AtomicBool::new(false);
static OUTPUT_REFUSED: AtomicBool = AtomicBool::new(false);
static ROWS_SKIPPED: AtomicBool = AtomicBool::new(false);
static NOT_REAPED: AtomicBool = AtomicBool::new(false);

/// The pump thread's state: the policy, the live helper and the thread's clock.
struct PumpThread<'a> {
    pump: Pump,
    live: Option<Live>,
    t0: Instant,
    inbox: &'a Arc<Inbox>,
    spawner: &'a mut dyn Spawner,
    post: &'a mut dyn FnMut(FilesEvent),
    /// The newest request and when it came (the probe's quiet time).
    #[cfg(feature = "owner-probe")]
    asked: Option<(u64, Instant)>,
}

/// The pump thread's body: messages in (with the policy's wake-ups as timeouts), acts out, until the close.
pub(crate) fn run_pump(
    inbox: &Arc<Inbox>,
    spawner: &mut dyn Spawner,
    post: &mut dyn FnMut(FilesEvent),
    limits: Limits,
) {
    let mut t = PumpThread {
        pump: Pump::new(limits),
        live: None,
        t0: Instant::now(),
        inbox,
        spawner,
        post,
        #[cfg(feature = "owner-probe")]
        asked: None,
    };
    loop {
        let msg = inbox.pop(t.pump.next_wake(t.now()));
        let now = t.now();
        let acts = match msg {
            None => t.pump.tick(now),
            Some(Msg::Request(r)) => {
                #[cfg(feature = "owner-probe")]
                if let Some(r) = &r {
                    t.asked = Some((r.gen, Instant::now()));
                }
                t.pump.request(r, now)
            }
            Some(Msg::Close) => t.pump.close(),
            Some(Msg::Frame(n, f)) => {
                t.observe(n, &f);
                t.pump.frame(n, f, now)
            }
        };
        if !t.run(acts) {
            return;
        }
    }
}

impl PumpThread<'_> {
    fn now(&self) -> u64 {
        ms_since(self.t0)
    }

    /// Runs the acts in order, with the ones they lead to (a failed start turns files off); false once the pump ended.
    fn run(&mut self, acts: Vec<Act>) -> bool {
        let mut queue = VecDeque::from(acts);
        while let Some(act) = queue.pop_front() {
            match act {
                Act::Spawn(n) => {
                    let ok = match self.spawner.spawn() {
                        Ok((child, out)) => {
                            let asked_frames = Arc::new(AtomicU32::new(1));
                            reader(n, out, self.inbox.clone(), asked_frames.clone());
                            self.live = Some(Live {
                                n,
                                child,
                                spawned: Instant::now(),
                                written: None,
                                asked_frames,
                                #[cfg(feature = "owner-probe")]
                                to_state: None,
                                #[cfg(feature = "owner-probe")]
                                rows_seen: false,
                            });
                            true
                        }
                        Err(e) => {
                            if first(&SPAWN_FAILED) {
                                log!("launcher files: the helper did not start ({e}); files are off for this open");
                            }
                            false
                        }
                    };
                    queue.extend(self.pump.spawned(n, ok, self.now()));
                }
                Act::Write(req) => self.write(&req),
                Act::Kill(why) => {
                    if let Some(l) = self.live.take() {
                        log_kill(why, &l);
                        let mut child = l.child;
                        child.kill();
                        reap(child);
                    }
                }
                Act::Retire => {
                    if let Some(l) = self.live.take() {
                        retire(l.child);
                    }
                }
                Act::Post(ev) => (self.post)(probe_verbs(ev)),
                Act::End => {
                    if let Some(l) = self.live.take() {
                        retire(l.child);
                    }
                    return false;
                }
            }
        }
        true
    }

    /// The line of a request to the live helper. A failed write needs nothing more: the helper's end of output follows.
    fn write(&mut self, req: &Req) {
        let Some(l) = self.live.as_mut() else { return };
        if let Some(line) = files::request_line(req) {
            // Counted before the write: its answer can only be read after it.
            l.asked_frames.fetch_add(1, SeqCst);
            let _ = l.child.write_line(line.as_bytes());
        }
        l.written = Some((req.gen, Instant::now()));
        #[cfg(feature = "owner-probe")]
        if let Some((gen, t)) = self.asked.filter(|(g, _)| *g == req.gen) {
            #[cfg(feature = "owner-probe")]
            crate::launcher_probe::files_written(gen, ms_since(t));
        }
    }

    /// What the live helper's frames say, before the policy reads them: the count of rows it skipped (logged once per
    /// process) and the probe's lines.
    fn observe(&mut self, n: u32, f: &Result<Option<Frame>, FrameError>) {
        let Some(l) = self.live.as_mut().filter(|l| l.n == n) else { return };
        match f {
            Ok(Some(Frame::Rows { skipped, .. })) if *skipped > 0 && first(&ROWS_SKIPPED) => {
                let n = *skipped;
                log!("launcher files: the helper skipped {n} rows (outside its roots, or refused)");
            }
            _ => {}
        }
        #[cfg(feature = "owner-probe")]
        probe_frame(l, f);
        #[cfg(not(feature = "owner-probe"))]
        let _ = l;
    }
}

/// The probe's lines of a frame of the live helper.
#[cfg(feature = "owner-probe")]
fn probe_frame(l: &mut Live, f: &Result<Option<Frame>, FrameError>) {
    match f {
        Ok(Some(Frame::State(files::STATE_READY))) => {
            let ms = ms_since(l.spawned);
            l.to_state = Some(ms);
            #[cfg(feature = "owner-probe")]
            crate::launcher_probe::files_state(ms, l.child.private_bytes());
        }
        Ok(Some(Frame::Rows { gen, hits, .. })) => {
            let ms = l.written.filter(|(g, _)| g == gen).map_or(0, |(_, t)| ms_since(t));
            let m = l.to_state.take().unwrap_or(0);
            let after = (!l.rows_seen).then(|| l.child.private_bytes()).flatten();
            l.rows_seen = true;
            #[cfg(feature = "owner-probe")]
            crate::launcher_probe::files_rows(*gen, hits.len(), ms, m, after);
        }
        _ => {}
    }
}

/// Owner-probe builds with the Mark of the Web switch (live check V-7): executable file rows may be opened.
#[cfg(feature = "owner-probe")]
fn probe_verbs(ev: FilesEvent) -> FilesEvent {
    use crate::launcher::model::Payload;
    #[cfg(feature = "owner-probe")]
    let motw = crate::launcher_probe::motw_test();
    let FilesEvent::Batch(mut b) = ev else { return ev };
    for it in b.items.iter_mut().filter(|_| motw) {
        if let Payload::Path { path, dir: false, class } = &it.payload {
            it.verbs = crate::launcher::verbs::verbs_for_file(path, *class, true, false);
        }
    }
    FilesEvent::Batch(b)
}

#[cfg(not(feature = "owner-probe"))]
fn probe_verbs(ev: FilesEvent) -> FilesEvent {
    ev
}

fn log_kill(why: KillWhy, l: &Live) {
    let ms = match (why, l.written) {
        (KillWhy::SpawnDeadline, _) | (_, None) => ms_since(l.spawned),
        (_, Some((_, t))) => ms_since(t),
    };
    match why {
        KillWhy::Stale => log!("launcher files: helper killed after {ms} ms (stale)"),
        KillWhy::Deadline => log!("launcher files: helper killed after {ms} ms (stall)"),
        KillWhy::SpawnDeadline => log!("launcher files: helper killed after {ms} ms (no state)"),
        KillWhy::Broken(kind) => {
            if first(&OUTPUT_REFUSED) {
                log!("launcher files: helper killed, its output was refused ({kind:?}); files are off for this open");
            }
        }
    }
}

/// Waits up to `REAP_MS` for a killed or retiring helper to end; past that its handle goes (the dock's job ends it).
fn reap(mut child: Box<dyn Helper>) {
    for _ in 0..REAP_MS / STEP_MS {
        if child.ended() {
            return;
        }
        std::thread::sleep(Duration::from_millis(STEP_MS));
    }
    if first(&NOT_REAPED) {
        log!("launcher files: a helper did not end within 2 s; the dock's job ends it");
    }
}

/// A helper that is done: its stdin closed, `CLOSE_GRACE_MS` to end by itself, then a kill; always reaped.
fn retire(mut child: Box<dyn Helper>) {
    child.close_stdin();
    for _ in 0..files::CLOSE_GRACE_MS / STEP_MS {
        if child.ended() {
            return;
        }
        std::thread::sleep(Duration::from_millis(STEP_MS));
    }
    child.kill();
    reap(child);
}

/// The reader thread of helper `n`: one frame at a time into the pump's inbox, until the end of the output or the first
/// error (the helper's frames are bounded, `files::FrameReader`). A frame past those the dock asked for (`asked`: the
/// state frame and one per line written) is `FrameError::Unasked` and the last one read: a helper that floods its
/// output never gets more than one frame queued.
fn reader(n: u32, out: PipeReader, inbox: Arc<Inbox>, asked: Arc<AtomicU32>) {
    let own = inbox.clone();
    let body = move || {
        let mut r = std::io::BufReader::new(out);
        let mut frames = FrameReader::default();
        let mut read = 0u32;
        loop {
            let mut f = frames.next(&mut r);
            if matches!(f, Ok(Some(_))) {
                read = read.saturating_add(1);
                if read > asked.load(SeqCst) {
                    f = Err(FrameError::Unasked);
                }
            }
            let last = !matches!(f, Ok(Some(_)));
            own.push(Msg::Frame(n, f));
            if last {
                return;
            }
        }
    };
    if std::thread::Builder::new().name("launcher-files-read".into()).stack_size(READER_STACK).spawn(body).is_err() {
        inbox.push(Msg::Frame(n, Err(FrameError::Io)));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::launcher::files::{encode_rows, encode_state, FileHit, Why, HEADER};
    use std::sync::mpsc::{self, Receiver};

    /// What the fake helpers were asked to do, in order.
    type Log = Arc<Mutex<Vec<String>>>;

    /// A helper that answers its first request with one row (twice when `twice`) and then never again (a stall). Its
    /// write returns once the reader has queued the answer in the pump's inbox, so the pump reads the answer before any
    /// of its clocks: no test races the reader thread against a deadline.
    struct FakeHelper {
        n: u32,
        answers: bool,
        twice: bool,
        out: Option<std::io::PipeWriter>,
        log: Log,
        ended: bool,
        inbox: Arc<Inbox>,
    }

    impl Helper for FakeHelper {
        fn write_line(&mut self, line: &[u8]) -> bool {
            let text = String::from_utf8_lossy(line).trim_end().to_string();
            self.log.lock().unwrap().push(format!("{} write {text}", self.n));
            if std::mem::take(&mut self.answers) {
                let gen = text.split(' ').next().unwrap().parse().unwrap();
                let hit = FileHit {
                    path: "C:\\u\\quokka.txt".into(),
                    name: "quokka.txt".into(),
                    folder: "u".into(),
                    dir: false,
                };
                let mut f = Vec::new();
                encode_rows(gen, 0, 0, &[hit], &mut f);
                if self.twice {
                    f.extend_from_slice(&f.clone());
                }
                self.out.as_mut().unwrap().write_all(&f).unwrap();
                // A guard against a hang, not a timing assertion: the inbox was empty at the write.
                let until = Instant::now() + Duration::from_secs(20);
                while self.inbox.q.lock().unwrap().is_empty() && Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            true
        }
        fn kill(&mut self) {
            self.log.lock().unwrap().push(format!("{} kill", self.n));
            (self.out, self.ended) = (None, true);
        }
        fn close_stdin(&mut self) {
            self.log.lock().unwrap().push(format!("{} close", self.n));
            (self.out, self.ended) = (None, true);
        }
        fn ended(&mut self) -> bool {
            self.ended
        }
        #[cfg(feature = "owner-probe")]
        fn private_bytes(&self) -> Option<usize> {
            None
        }
    }

    /// Helpers whose stdout is a pipe holding the header and state 0; only the first one ever answers.
    struct FakeSpawner {
        n: u32,
        log: Log,
        twice: bool,
        inbox: Arc<Inbox>,
    }

    impl Spawner for FakeSpawner {
        fn spawn(&mut self) -> std::io::Result<(Box<dyn Helper>, PipeReader)> {
            self.n += 1;
            let (r, mut w) = std::io::pipe()?;
            let mut head = HEADER.to_vec();
            encode_state(0, &mut head);
            w.write_all(&head)?;
            self.log.lock().unwrap().push(format!("{} spawn", self.n));
            let helper = FakeHelper {
                n: self.n,
                answers: self.n == 1,
                twice: self.twice,
                out: Some(w),
                log: self.log.clone(),
                ended: false,
                inbox: self.inbox.clone(),
            };
            Ok((Box::new(helper), r))
        }
    }

    fn req(gen: u64) -> Msg {
        Msg::Request(Some(Req { gen, text: format!("quokka {gen}").into(), quiet_ms: 0, cap: 20 }))
    }

    /// The pump thread's body over fake helpers; the events it posts, busy marks left out. The deadline is short, since
    /// it only ever ends a helper that never answers; no spawn deadline fires.
    fn pump_thread(log: &Log, twice: bool) -> (Arc<Inbox>, Receiver<FilesEvent>, std::thread::JoinHandle<()>) {
        let inbox = Arc::new(Inbox::default());
        let (ev_tx, ev_rx) = mpsc::channel();
        let own = inbox.clone();
        let log = log.clone();
        let limits = Limits { slow_ms: 20, stale_kill_ms: 20, deadline_ms: 60, spawn_deadline_ms: 600_000 };
        let thread = std::thread::spawn(move || {
            let mut spawner = FakeSpawner { n: 0, log, twice, inbox: own.clone() };
            let mut post = |ev| {
                let _ = ev_tx.send(ev);
            };
            run_pump(&own, &mut spawner, &mut post, limits);
        });
        (inbox, ev_rx, thread)
    }

    fn next(rx: &Receiver<FilesEvent>) -> FilesEvent {
        loop {
            // A guard against a hang, not a timing assertion.
            match rx.recv_timeout(Duration::from_secs(20)).expect("an event") {
                FilesEvent::Slow(_) => continue,
                e => return e,
            }
        }
    }

    #[test]
    fn pump_over_a_fake_helper() {
        let log = Log::default();
        let (tx, events, thread) = pump_thread(&log, false);
        tx.push(req(1));
        let FilesEvent::Batch(b) = next(&events) else { panic!("rows first") };
        assert_eq!((b.gen, b.items.len()), (1, 1));
        // Helper 1 stalls from now on: killed at the deadline and respawned at the next request; three kills turn files
        // off for the open.
        for gen in 2..=4 {
            tx.push(req(gen));
            assert_eq!(next(&events), FilesEvent::Stalled(gen));
        }
        assert_eq!(next(&events), FilesEvent::Off(Why::Stalled));
        tx.push(req(5));
        tx.push(Msg::Close);
        thread.join().unwrap();
        assert!(events.try_iter().all(|e| matches!(e, FilesEvent::Slow(_))), "nothing after Off but busy marks");
        let log = log.lock().unwrap().clone();
        assert_eq!(
            log,
            [
                "1 spawn",
                "1 write 1 quokka 1",
                "1 write 2 quokka 2",
                "1 kill",
                "2 spawn",
                "2 write 3 quokka 3",
                "2 kill",
                "3 spawn",
                "3 write 4 quokka 4",
                "3 kill"
            ],
            "every helper killed and reaped; none after files went off"
        );
    }

    #[test]
    fn close_retires_the_live_helper() {
        let log = Log::default();
        let (tx, events, thread) = pump_thread(&log, false);
        tx.push(req(1));
        assert!(matches!(next(&events), FilesEvent::Batch(_)));
        tx.push(Msg::Close);
        thread.join().unwrap();
        assert_eq!(
            *log.lock().unwrap(),
            ["1 spawn", "1 write 1 quokka 1", "1 close"],
            "stdin closed; it ended in its grace"
        );
    }

    #[test]
    fn a_frame_never_asked_for_ends_files() {
        let log = Log::default();
        let (tx, events, thread) = pump_thread(&log, true);
        tx.push(req(1));
        assert!(matches!(next(&events), FilesEvent::Batch(b) if b.gen == 1), "the answer to the request");
        assert_eq!(next(&events), FilesEvent::Off(Why::Broken), "the second frame was never asked for");
        tx.push(Msg::Close);
        thread.join().unwrap();
        assert_eq!(*log.lock().unwrap(), ["1 spawn", "1 write 1 quokka 1", "1 kill"]);
    }

    #[test]
    fn inbox_keeps_the_order_and_times_out() {
        let inbox = Inbox::default();
        assert!(inbox.pop(Some(0)).is_none());
        assert!(inbox.pop(Some(5)).is_none(), "a time-out, not a message");
        inbox.push(req(1));
        inbox.push(Msg::Close);
        assert!(matches!(inbox.pop(None), Some(Msg::Request(Some(r))) if r.gen == 1));
        assert!(matches!(inbox.pop(Some(0)), Some(Msg::Close)));
        let inbox = Arc::new(Inbox::default());
        let other = inbox.clone();
        let t = std::thread::spawn(move || other.push(Msg::Close));
        assert!(matches!(inbox.pop(None), Some(Msg::Close)), "a waiting pop wakes at a push");
        t.join().unwrap();
    }

    #[test]
    fn no_pump_no_thread() {
        request(1, None, 0);
        end();
        assert!(PUMP.with(|p| p.borrow().is_none()), "a withdrawal with no pump starts nothing");
    }
}
