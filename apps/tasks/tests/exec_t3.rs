//! The executor's order and checks over a fake system (T3.7): every key of a kill list is checked again just before it
//! is touched. Nothing here opens, closes or ends a real process or window.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use oled_sys::admin::policy::{HelperFacts, ProcEdge};
use oled_sysmon::ProcKey;
use oled_trust::frame::admin::DenyReason;
use oled_ui::geom::PointL;
use oled_ui::model::{ListModel, RowKey};
use tasks::app::access::Refusal;
use tasks::app::args::PreviewLang;
use tasks::app::exec::{self, ActVerb, ActionDone, Batch, KillGuard, Os, OsHandle, Outcome, RecordingExecutor, Target};
use tasks::app::launch::{LaunchReq, RecordingLauncher};
use tasks::model::group::AppKey;
use tasks::ui::rows::{group_key, proc_key};
use tasks::ui::scene::{self, Scene};

const ROOT: &str = "C:\\Windows";

fn key(pid: u32, created: u64) -> ProcKey {
    ProcKey { pid, created }
}

/// A process the fake system has now.
struct Proc {
    key: ProcKey,
    path: String,
    session: u32,
    critical: Option<bool>,
    open_error: Option<u32>,
    terminate_error: Option<u32>,
    path_error: Option<u32>,
}

#[derive(Default)]
struct FakeOs {
    procs: HashMap<u32, Proc>,
    handles: HashMap<isize, u32>,
    next: isize,
    opened: Vec<u32>,
    opens: u32,
    closes: u32,
    terminated: Vec<ProcKey>,
    windows: HashMap<isize, u32>,
    posted: Vec<isize>,
}

impl FakeOs {
    fn add(&mut self, k: ProcKey, path: &str, session: u32) -> &mut Proc {
        let p = Proc {
            key: k,
            path: path.to_string(),
            session,
            critical: Some(false),
            open_error: None,
            terminate_error: None,
            path_error: None,
        };
        self.procs.insert(k.pid, p);
        self.procs.get_mut(&k.pid).expect("just added")
    }

    fn of(&self, h: OsHandle) -> &Proc {
        let pid = self.handles.get(&h.0).expect("an open handle");
        self.procs.get(pid).expect("a live process")
    }
}

impl Os for FakeOs {
    fn open(&mut self, pid: u32) -> Result<OsHandle, u32> {
        self.opened.push(pid);
        let Some(p) = self.procs.get(&pid) else { return Err(87) };
        if let Some(code) = p.open_error {
            return Err(code);
        }
        self.next += 1;
        self.opens += 1;
        self.handles.insert(self.next, pid);
        Ok(OsHandle(self.next))
    }
    fn created(&mut self, h: OsHandle) -> Option<u64> {
        Some(self.of(h).key.created)
    }
    fn session(&mut self, pid: u32) -> Option<u32> {
        self.procs.get(&pid).map(|p| p.session)
    }
    fn image_path(&mut self, h: OsHandle) -> Result<String, u32> {
        let p = self.of(h);
        p.path_error.map_or_else(|| Ok(p.path.clone()), Err)
    }
    fn critical(&mut self, h: OsHandle) -> Option<bool> {
        self.of(h).critical
    }
    fn terminate(&mut self, h: OsHandle) -> Result<(), u32> {
        let pid = *self.handles.get(&h.0).expect("an open handle");
        if let Some(code) = self.procs.get(&pid).and_then(|p| p.terminate_error) {
            return Err(code);
        }
        let p = self.procs.remove(&pid).expect("a live process");
        self.terminated.push(p.key);
        Ok(())
    }
    fn close(&mut self, h: OsHandle) {
        assert!(self.handles.remove(&h.0).is_some(), "a handle closed twice or never opened");
        self.closes += 1;
    }
    fn window_pid(&mut self, hwnd: isize) -> Option<u32> {
        self.windows.get(&hwnd).copied()
    }
    fn post_close(&mut self, hwnd: isize) -> Result<(), u32> {
        if !self.windows.contains_key(&hwnd) {
            return Err(1400);
        }
        self.posted.push(hwnd);
        Ok(())
    }
}

/// Tasks is pid 100 in session 1, started by 90, started by 80; the shell is 70; the dock is 60.
fn guard() -> KillGuard {
    KillGuard {
        helper: HelperFacts {
            self_pid: 100,
            parent: key(90, 90),
            session: 1,
            dock_family: [Some(key(60, 60)), None, None, None],
        },
        own_chain: vec![key(90, 90), key(80, 80)],
        shell: Some(key(70, 70)),
    }
}

fn edge(k: ProcKey, parent_pid: u32) -> ProcEdge {
    ProcEdge { key: k, parent_pid }
}

fn end(members: Vec<ProcKey>, edges: Vec<ProcEdge>) -> Batch {
    Batch {
        verb: ActVerb::End,
        targets: vec![Target { row: RowKey(1), members, windows: Vec::new(), launch: None }],
        guard: guard(),
        edges,
    }
}

fn run(os: &mut FakeOs, b: &Batch) -> Vec<ActionDone> {
    exec::run(os, b, ROOT, &RecordingLauncher::default())
}

fn app(pid: u32, created: u64) -> (ProcKey, String) {
    (key(pid, created), format!("C:\\Apps\\p{pid}.exe"))
}

#[test]
fn order_root_first_then_descendants() {
    let mut os = FakeOs::default();
    let (root, a, b, c) = (key(10, 1000), key(20, 2000), key(30, 2100), key(40, 3000));
    // Pid 50's parent pid is the root's, but it was created before the root: a recycled pid, not a child.
    let reused = key(50, 500);
    for k in [root, a, b, c, reused] {
        os.add(k, &format!("C:\\Apps\\p{}.exe", k.pid), 1);
    }
    let edges = vec![edge(reused, 10), edge(c, 20), edge(b, 10), edge(a, 10), edge(root, 1)];
    assert_eq!(exec::kill_list(&[root], &edges), [root, b, a, c], "the root, its children, then theirs");
    let done = run(&mut os, &end(vec![root], edges));
    assert_eq!(os.terminated, [root, b, a, c]);
    assert!(os.procs.contains_key(&50), "the reused pid lives");
    assert_eq!(done, [ActionDone { row: RowKey(1), outcome: Outcome::Done, refused: 0 }]);
    // Every member is a root of its own: a group's members and their trees, each key once.
    assert_eq!(exec::kill_list(&[a, root], &[edge(c, 20), edge(a, 10)]), [a, c, root]);
}

#[test]
fn guarded_keys_are_never_terminated() {
    let mut os = FakeOs::default();
    let root = key(10, 1000);
    os.add(root, "C:\\Apps\\orbit.exe", 1);
    // Every guarded process sits under the root (created after it, so each counts as its child).
    let shell = key(70, 1070);
    let csrss = key(200, 2000);
    let flagged = key(210, 2100);
    let service = key(220, 2200);
    let me = key(100, 1100);
    let (parent, grandparent) = (key(90, 1090), key(80, 1080));
    let dock = key(60, 1060);
    let (kid1, kid2) = (key(300, 3000), key(310, 3100));
    // Below the shell (refused by its key) and below the flagged process (refused on its opened facts).
    let (under_shell, under_flagged) = (key(400, 4000), key(410, 4100));
    os.add(shell, "C:\\Windows\\explorer.exe", 1);
    os.add(csrss, "C:\\Windows\\System32\\csrss.exe", 1).critical = None;
    os.add(flagged, "C:\\Apps\\keeper.exe", 1).critical = Some(true);
    os.add(service, "C:\\Windows\\System32\\svchost.exe", 0);
    os.add(me, "C:\\Apps\\tasks.exe", 1);
    os.add(parent, "C:\\Apps\\launcher.exe", 1);
    os.add(grandparent, "C:\\Windows\\System32\\cmd.exe", 1);
    os.add(dock, "C:\\Apps\\dock.exe", 1);
    os.add(kid1, "C:\\Apps\\quill.exe", 1);
    os.add(kid2, "C:\\Apps\\quill.exe", 1);
    os.add(under_shell, "C:\\Apps\\orbit.exe", 1);
    os.add(under_flagged, "C:\\Apps\\orbit.exe", 1);
    let mut edges: Vec<ProcEdge> = [shell, csrss, flagged, service, me, parent, grandparent, dock, kid1, kid2]
        .into_iter()
        .map(|k| edge(k, 10))
        .collect();
    edges.extend([edge(under_shell, 70), edge(under_flagged, 210)]);
    let mut b = end(vec![root], edges);
    b.guard = KillGuard {
        helper: HelperFacts { self_pid: 100, parent, session: 1, dock_family: [Some(dock), None, None, None] },
        own_chain: vec![parent, grandparent],
        shell: Some(shell),
    };
    let done = run(&mut os, &b);
    assert_eq!(os.terminated, [root, kid1, kid2], "the root and the two ordinary children only");
    for k in [shell, me, parent, grandparent, dock, under_shell, under_flagged] {
        assert!(!os.opened.contains(&k.pid), "pid {} is refused by its key alone and never opened", k.pid);
    }
    for k in [csrss, flagged, service] {
        assert!(os.opened.contains(&k.pid), "pid {} is judged on its opened facts", k.pid);
    }
    assert_eq!(done[0].outcome, Outcome::Done);
    assert_eq!(done[0].refused, 10, "the eight guarded keys and the two below them");
    assert!(os.procs.contains_key(&400) && os.procs.contains_key(&410), "what a guarded process started lives");
    assert_eq!(os.opens, os.closes, "every handle closed");
}

#[test]
fn a_refused_member_keeps_its_tree() {
    // The shell is the group's only member: End and End now refuse it and never reach what it started.
    for verb in [ActVerb::End, ActVerb::EndNow] {
        let mut os = FakeOs::default();
        let shell = key(70, 70);
        let (a, b, a1) = (key(300, 3000), key(310, 3100), key(320, 3200));
        os.add(shell, "C:\\Windows\\explorer.exe", 1);
        for k in [a, b, a1] {
            os.add(k, &format!("C:\\Apps\\p{}.exe", k.pid), 1);
        }
        let mut batch = end(vec![shell], vec![edge(a, 70), edge(b, 70), edge(a1, 300)]);
        batch.verb = verb;
        let done = run(&mut os, &batch);
        assert!(os.terminated.is_empty() && os.opened.is_empty(), "{verb:?}: nothing opened, nothing ended");
        assert_eq!((done[0].outcome, done[0].refused), (Outcome::Refused(Refusal::Shell), 4), "{verb:?}");
    }
    // Tasks itself as the member (its Background row): the app it reopened lives.
    let mut os = FakeOs::default();
    let (me, reopened) = (key(100, 100), key(500, 5000));
    os.add(me, "C:\\Apps\\tasks.exe", 1);
    os.add(reopened, "C:\\Apps\\orbit.exe", 1);
    let done = run(&mut os, &end(vec![me], vec![edge(reopened, 100)]));
    assert!(os.terminated.is_empty() && os.opened.is_empty());
    assert_eq!((done[0].outcome, done[0].refused), (Outcome::Refused(Refusal::Deny(DenyReason::OwnProcess)), 2));
    // A member below a refused member is judged on its own: it and its tree end, the refused one's other child lives.
    let mut os = FakeOs::default();
    let (shell, member, its_child, other) = (key(70, 70), key(300, 3000), key(310, 3100), key(320, 3200));
    os.add(shell, "C:\\Windows\\explorer.exe", 1);
    for k in [member, its_child, other] {
        os.add(k, &format!("C:\\Apps\\p{}.exe", k.pid), 1);
    }
    let edges = vec![edge(member, 70), edge(other, 70), edge(its_child, 300)];
    let done = run(&mut os, &end(vec![shell, member], edges));
    assert_eq!(os.terminated, [member, its_child]);
    assert_eq!((done[0].outcome, done[0].refused), (Outcome::Done, 2));
}

/// Shift+Del on the File Explorer row of the T3c scene (the shell is its only member), and the batch the UI made run
/// over a fake system holding the scene's processes: nothing the shell started is ended.
#[test]
fn end_now_on_the_shell_row_ends_nothing() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("scenes").join("t3c_actions.json");
    let scene = Scene::load(&path).unwrap_or_else(|e| panic!("{e}"));
    let recorder = Arc::new(RecordingExecutor::default());
    let launcher = Arc::new(RecordingLauncher::default());
    let mut h = scene::open(&scene, PreviewLang::En, 96, launcher, recorder.clone()).unwrap_or_else(|e| panic!("{e}"));
    let row = {
        let grouping = h.screen().parts().grouped.grouping();
        let g = grouping.groups.iter().find(|g| g.members.first().is_some_and(|m| m.pid == 1000)).expect("its group");
        let key = match g.key {
            AppKey::Single(p) => proc_key(p),
            other => group_key(other),
        };
        h.screen().table().model().row_of(key).expect("the File Explorer row")
    };
    let r = h.screen().table().row_rect(row).expect("on screen");
    h.click(PointL { x: (r.l + r.r) / 2, y: (r.t + r.b) / 2 });
    h.key("Shift+Del");
    let batches = recorder.batches.lock().unwrap().clone();
    assert_eq!(batches.len(), 1);
    let b = &batches[0];
    assert_eq!((b.verb, b.targets[0].members.len()), (ActVerb::EndNow, 1), "End now on the shell alone");
    assert!(!b.edges.is_empty(), "a group row takes the descendants");
    let mut os = FakeOs::default();
    for p in &scene.procs {
        os.add(key(p.pid, p.created), p.path.as_deref().unwrap_or(""), p.session);
    }
    let done = run(&mut os, b);
    assert!(os.terminated.is_empty(), "ended {:?}", os.terminated);
    assert_eq!(done[0].outcome, Outcome::Refused(Refusal::Shell));
}

#[test]
fn late_child_is_checked() {
    // The menu's facts were computed over the root alone; by the time the batch runs, a child exists, and it is the
    // shell process (say the root launched a new desktop shell). It is checked like any other key.
    let mut os = FakeOs::default();
    let root = key(10, 1000);
    os.add(root, "C:\\Apps\\orbit.exe", 1);
    let late_shell = key(70, 70_000);
    os.add(late_shell, "C:\\Windows\\explorer.exe", 1);
    let mut b = end(vec![root], vec![edge(late_shell, 10)]);
    b.guard.shell = Some(late_shell);
    let done = run(&mut os, &b);
    assert_eq!(os.terminated, [root]);
    assert!(os.procs.contains_key(&70));
    assert_eq!((done[0].outcome, done[0].refused), (Outcome::Done, 1));
}

#[test]
fn stale_key_is_gone_and_untouched() {
    let mut os = FakeOs::default();
    // The row's key says pid 10 was created at 1000; pid 10 now belongs to a process created at 5000.
    os.add(key(10, 5000), "C:\\Apps\\quill.exe", 1);
    let done = run(&mut os, &end(vec![key(10, 1000)], Vec::new()));
    assert!(os.terminated.is_empty(), "a reused pid is never ended");
    assert_eq!(done[0].outcome, Outcome::Gone);
    assert_eq!((os.opens, os.closes), (1, 1));
    // A pid that is gone altogether.
    let mut os = FakeOs::default();
    let done = run(&mut os, &end(vec![key(11, 1100)], Vec::new()));
    assert_eq!((done[0].outcome, os.opens), (Outcome::Gone, 0));
}

#[test]
fn access_denied_is_needs_admin() {
    let mut os = FakeOs::default();
    let (elevated, stubborn) = app(10, 1000);
    os.add(elevated, &stubborn, 1).open_error = Some(5);
    let done = run(&mut os, &end(vec![elevated], Vec::new()));
    assert_eq!(done[0].outcome, Outcome::NeedsAdmin);
    // Opened, but TerminateProcess is refused.
    let mut os = FakeOs::default();
    let (k, path) = app(11, 1100);
    os.add(k, &path, 1).terminate_error = Some(5);
    let done = run(&mut os, &end(vec![k], Vec::new()));
    assert_eq!(done[0].outcome, Outcome::NeedsAdmin);
    assert_eq!(os.opens, os.closes);
}

#[test]
fn handle_closed_on_every_path() {
    let mut os = FakeOs::default();
    let keys: Vec<ProcKey> = (1..=6).map(|i| key(10 * i, 1000 * u64::from(i))).collect();
    // Ended; refused after the open (critical flag); image path unreadable; terminate fails; create time moved;
    // session 0.
    os.add(keys[0], "C:\\Apps\\a.exe", 1);
    os.add(keys[1], "C:\\Apps\\b.exe", 1).critical = Some(true);
    os.add(keys[2], "C:\\Apps\\c.exe", 1).path_error = Some(31);
    os.add(keys[3], "C:\\Apps\\d.exe", 1).terminate_error = Some(1);
    os.add(key(50, 1), "C:\\Apps\\e.exe", 1);
    os.add(keys[5], "C:\\Apps\\f.exe", 0);
    let done = run(&mut os, &end(keys, Vec::new()));
    assert_eq!(os.opens, 6);
    assert_eq!(os.closes, 6, "one close per open, whatever happened");
    assert!(os.handles.is_empty());
    assert_eq!(os.terminated, [key(10, 1000)]);
    assert_eq!(done[0].outcome, Outcome::Failed(31), "the worst of the members");
    assert_eq!(done[0].refused, 2);
}

#[test]
fn outcome_merge() {
    use Outcome::*;
    assert_eq!(exec::worst(Gone, Done), Done);
    assert_eq!(exec::worst(Done, Gone), Done);
    assert_eq!(exec::worst(Done, NeedsAdmin), NeedsAdmin);
    assert_eq!(exec::worst(NeedsAdmin, Failed(1)), Failed(1));
    assert_eq!(exec::worst(Failed(2), Done), Failed(2));
    // Every key refused: Refused, with the first refusal.
    let mut os = FakeOs::default();
    let b = end(vec![key(100, 1), key(60, 60)], Vec::new());
    let done = run(&mut os, &b);
    assert_eq!(done[0].outcome, Refused(Refusal::Deny(DenyReason::OwnProcess)));
    assert_eq!(done[0].refused, 2);
    // Some refused, the rest ended: Done, and the refused count says how many were kept.
    let mut os = FakeOs::default();
    let (k, path) = app(10, 1000);
    os.add(k, &path, 1);
    let done = run(&mut os, &end(vec![k, key(60, 60)], Vec::new()));
    assert_eq!((done[0].outcome, done[0].refused), (Done, 1));
    // One target per row, in order.
    let mut os = FakeOs::default();
    let (k2, p2) = app(20, 2000);
    os.add(k2, &p2, 1);
    let mut b = end(vec![key(30, 3000)], Vec::new());
    b.targets.push(Target { row: RowKey(2), members: vec![k2], windows: Vec::new(), launch: None });
    let done = run(&mut os, &b);
    assert_eq!(done.iter().map(|d| (d.row, d.outcome)).collect::<Vec<_>>(), [(RowKey(1), Gone), (RowKey(2), Done)]);
}

#[test]
fn close_posts_only_to_the_groups_windows() {
    let mut os = FakeOs::default();
    let (k, path) = app(10, 1000);
    os.add(k, &path, 1);
    os.windows.insert(0x10, 10);
    // A window handle that another process owns now, one that is gone, and the shell's.
    os.windows.insert(0x20, 99);
    os.windows.insert(0x30, 70);
    let b = Batch {
        verb: ActVerb::Close,
        targets: vec![Target {
            row: RowKey(1),
            members: vec![k, key(70, 70)],
            windows: vec![0x10, 0x20, 0x40, 0x30],
            launch: None,
        }],
        guard: guard(),
        edges: Vec::new(),
    };
    let done = run(&mut os, &b);
    assert_eq!(os.posted, [0x10], "WM_CLOSE only to a window a member owns, never to the shell's");
    assert!(os.terminated.is_empty() && os.opened.is_empty(), "Close never ends or opens a process");
    assert_eq!((done[0].outcome, done[0].refused), (Outcome::Posted, 1));
    // Nothing left to post to: Gone.
    let mut os = FakeOs::default();
    let done = run(&mut os, &b);
    assert_eq!(done[0].outcome, Outcome::Gone);
}

#[test]
fn close_posts_to_a_hosted_apps_frame() {
    // A packaged app's frame window is created by its host process, which is no member; window_pid answers the app's
    // process (the frame's CoreWindow child's, the engine's owner rule), so the frame qualifies and gets WM_CLOSE.
    let mut os = FakeOs::default();
    let (k, path) = app(10, 1000);
    os.add(k, &path, 1);
    os.windows.insert(0x50, 10);
    let b = Batch {
        verb: ActVerb::Close,
        targets: vec![Target { row: RowKey(1), members: vec![k], windows: vec![0x50], launch: None }],
        guard: guard(),
        edges: Vec::new(),
    };
    let done = run(&mut os, &b);
    assert_eq!(os.posted, [0x50]);
    assert_eq!((done[0].outcome, done[0].refused), (Outcome::Posted, 0));
}

#[test]
fn reopen_goes_through_the_launcher() {
    let launcher = RecordingLauncher::default();
    let mut os = FakeOs::default();
    let launch = "shell:AppsFolder\\Contoso.Orbit_8wekyb3d8bbwe!App".to_string();
    let b = Batch {
        verb: ActVerb::Reopen,
        targets: vec![Target {
            row: RowKey(5),
            members: Vec::new(),
            windows: Vec::new(),
            launch: Some(launch.clone()),
        }],
        guard: guard(),
        edges: Vec::new(),
    };
    let done = exec::run(&mut os, &b, ROOT, &launcher);
    assert_eq!(done[0].outcome, Outcome::Done);
    assert_eq!(*launcher.reqs.lock().unwrap(), [LaunchReq::Launch(launch)]);
    assert!(os.opened.is_empty());
}

#[test]
fn kill_list_never_repeats_a_key() {
    let (a, b) = (key(10, 1), key(20, 2));
    let list = exec::kill_list(&[a, b, a], &[edge(b, 10)]);
    let set: HashSet<_> = list.iter().collect();
    assert_eq!(list.len(), set.len());
    assert_eq!(list, [a, b]);
}
