//! Kill, session-end and coexistence tests of the ops host (OP8). Each test makes its own TempRoot, data folder and
//! hooks, so tests in parallel threads, and two worktrees on one PC, never share a host, a pipe or a data folder. The
//! UI is files.exe --role=test-client running a script; FILES_TEST_SLOW_POST_MS (debug builds) slows the host after
//! each top-level item so a kill lands mid-operation. Only our own child's window gets session messages.

mod common;
mod ops_util;

use std::collections::HashSet;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::temp::TempRoot;
use files_engine::host::TestHooks;
use files_engine::ops::host::read_done;
use files_engine::ops::journal::{journal_rel, parse, St};
use files_engine::ops_client::{OpsClient, OpsConfig, OpsEvent};
use files_engine::own_files::OwnDir;
use oled_trust::frame::files::{OpEvt, OpId};
use ops_util::wp;
use serde_json::{json, Value};
use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, WAIT_OBJECT_0, WPARAM};
use windows::Win32::System::Shutdown::ShutdownBlockReasonQuery;
use windows::Win32::System::Threading::{
    OpenProcess, TerminateProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    PROCESS_TERMINATE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowThreadProcessId, SendMessageTimeoutW, ENDSESSION_LOGOFF, SMTO_ABORTIFHUNG,
    WM_ENDSESSION, WM_QUERYENDSESSION,
};

const FILES: u32 = 64;

/// The hooks of one test as a child's environment, plus the slow-down after each top-level item.
fn env_of(h: &TestHooks, slow_ms: u32) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = h.env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    if let Some(t) = &h.pipe_tag {
        v.push(("FILES_TEST_PIPE_TAG".into(), t.to_string()));
    }
    if let Some(t) = &h.build_tag {
        v.push(("FILES_TEST_BUILD_TAG".into(), t.to_string()));
    }
    v.push(("FILES_TEST_SLOW_POST_MS".into(), slow_ms.to_string()));
    v
}

/// A test client running `lines`, and its stdout as JSON values.
fn ui(root: &TempRoot, h: &TestHooks, slow_ms: u32, lines: &[Value]) -> (common::ChildGuard, Receiver<Value>) {
    let script = lines.iter().map(Value::to_string).collect::<Vec<_>>().join("\n");
    let path = root.file(&format!("script-{}.jsonl", common::test_tag()), script.as_bytes());
    let env = env_of(h, slow_ms);
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let mut child =
        common::spawn_no_window(&common::files_exe(), &["--role=test-client", &path.display().to_string()], &env);
    let out = child.take_stdout().unwrap();
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                if tx.send(v).is_err() {
                    break;
                }
            }
        }
    });
    (child, rx)
}

/// The first line for which `f` holds (60 s at most).
fn line_where(rx: &Receiver<Value>, mut f: impl FnMut(&Value) -> bool) -> Value {
    let end = Instant::now() + Duration::from_secs(60);
    loop {
        let left = end.saturating_duration_since(Instant::now());
        let v = rx.recv_timeout(left).expect("the line within 60 s");
        if f(&v) {
            return v;
        }
    }
}

fn files(root: &TempRoot, size: usize) -> Vec<Value> {
    (0..FILES)
        .map(|i| json!(root.file(&format!("src/f{i:02}.bin"), &vec![i as u8; size]).display().to_string()))
        .collect()
}

fn hashes(root: &TempRoot, dir: &str) -> Vec<u64> {
    (0..FILES).map(|i| root.hash(&format!("{dir}/f{i:02}.bin"))).collect()
}

/// Terminates the host `pid` and waits for its end, after checking it runs our files.exe; false when it does not.
fn end_host(pid: u32) -> bool {
    let ours = oled_sys::pipe::peer_of_pid(pid)
        .is_ok_and(|peer| files_engine::host::named::peer_ok(&common::files_exe(), &peer.image, false));
    if !ours {
        return false;
    }
    // SAFETY: a process this test's client started (checked above); the handle is closed below.
    let Ok(h) = (unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, false, pid) }) else {
        return false;
    };
    // SAFETY: as above.
    unsafe {
        let _ = TerminateProcess(h, 9);
        WaitForSingleObject(h, 10_000);
        let _ = CloseHandle(h);
    }
    true
}

/// Our own host, by the pid its client printed.
fn kill_host(pid: u32) {
    assert!(end_host(pid), "{pid} is our running host");
}

/// Whether the process `pid` ends within `ms`.
fn ends_within(pid: u32, ms: u32) -> bool {
    // SAFETY: a plain open for waiting; the handle is closed below.
    let Ok(h) = (unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }) else {
        return true;
    };
    // SAFETY: as above.
    let ended = unsafe { WaitForSingleObject(h, ms) } == WAIT_OBJECT_0;
    // SAFETY: the handle opened above.
    let _ = unsafe { CloseHandle(h) };
    ended
}

fn in_process_client(root: &TempRoot, hooks: TestHooks) -> OpsClient {
    let cfg = OpsConfig { exe: common::files_exe(), data_dir: root.at("data"), owner: 0, test: Some(hooks) };
    OpsClient::new(cfg, Arc::new(|| {}))
}

/// Ends the host of an in-process client when the test ends, passed or failed. A host left running a move stays
/// queued on the shell's session-wide jump-list lock (see host_killed_mid_move) and delays every later move of the
/// session, other tests' and the user's own.
struct HostGuard(Option<u32>);

impl Drop for HostGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            end_host(pid);
        }
    }
}

fn until_finished(c: &OpsClient, op: OpId) -> Vec<OpsEvent> {
    let end = Instant::now() + Duration::from_secs(120);
    let mut all = Vec::new();
    loop {
        let evs = c.drain();
        let done = evs.iter().any(|e| matches!(e, OpsEvent::Evt(OpEvt::Finished { op: o, .. }) if *o == op));
        all.extend(evs);
        if done {
            return all;
        }
        assert!(Instant::now() < end, "no Finished: {all:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn ui_killed_mid_copy() {
    let root = TempRoot::new("kill_ui");
    let hooks = common::hooks(&root);
    let items = files(&root, 1 << 20);
    let dst = root.dir("dst").display().to_string();
    let script = [json!({"op": "ops_copy", "items": items, "dest": dst}), json!({"op": "ops_wait", "ms": 600_000})];
    let (mut client, rx) = ui(&root, &hooks, 30, &script);
    let sent = line_where(&rx, |v| v.get("sent").is_some());
    let (op, host) = (sent["sent"].as_u64().unwrap() as u32, sent["host_pid"].as_u64().unwrap() as u32);
    line_where(&rx, |v| v["evt"] == "progress");
    client.child().kill().unwrap();
    let data = OwnDir::open(&root.at("data")).unwrap();
    let start = Instant::now();
    while !read_done(&data).iter().any(|d| d.op == op) {
        assert!(start.elapsed() < Duration::from_secs(60), "ops-done.json names the operation");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(hashes(&root, "dst"), hashes(&root, "src"), "every destination equals its source");
    // The shell can hold the host after its last item (see host_killed_mid_move), so the bound is generous, and a host
    // still there is ended so that it does not keep the session's jump-list lock.
    let ended = ends_within(host, 120_000);
    if !ended {
        end_host(host);
    }
    assert!(ended, "the host leaves within 120 s of finishing");
}

/// Kills the host after item 10's Post; returns the operation, the done set of its journal and the hooks.
fn interrupted(root: &TempRoot, verb: &str) -> (u32, HashSet<String>, TestHooks) {
    let hooks = common::hooks(root);
    let items = files(root, 1 << 20);
    let dst = root.dir("dst").display().to_string();
    let script = [json!({"op": verb, "items": items, "dest": dst}), json!({"op": "ops_wait", "ms": 600_000})];
    let (mut client, rx) = ui(root, &hooks, 50, &script);
    let sent = line_where(&rx, |v| v.get("sent").is_some());
    let (op, host) = (sent["sent"].as_u64().unwrap() as u32, sent["host_pid"].as_u64().unwrap() as u32);
    let mut done = 0;
    line_where(&rx, |v| {
        done += if v["evt"] == "items_done" { v["n"].as_u64().unwrap() } else { 0 };
        done >= 10
    });
    kill_host(host);
    client.child().kill().unwrap();
    let data = OwnDir::open(&root.at("data")).unwrap();
    let j = parse(&data.read(&journal_rel(OpId(op), host)).unwrap()).expect("the journal parses");
    assert!(j.end.is_none(), "an interrupted operation has no end");
    let finished: HashSet<String> =
        j.latest().iter().filter(|r| r.st == St::Done).map(|r| format!("{:?}", r.src)).collect();
    assert!(finished.len() >= 10 && finished.len() < FILES as usize, "killed mid-way: {}", finished.len());
    (op, finished, hooks)
}

#[test]
fn host_killed_mid_copy() {
    let root = TempRoot::new("kill_host_copy");
    let (op, done_before, hooks) = interrupted(&root, "ops_copy");
    let c = in_process_client(&root, hooks);
    let r = c.resume_rest(OpId(op)).unwrap();
    let _host = HostGuard(c.host_pid());
    let evs = until_finished(&c, r);
    assert!(
        evs.iter().any(|e| matches!(e, OpsEvent::Evt(OpEvt::Pending { of, .. }) if of.0 == op)),
        "Pending: {evs:?}"
    );
    assert!(evs
        .iter()
        .any(|e| matches!(e, OpsEvent::Evt(OpEvt::Finished { result: oled_trust::frame::files::OpResult::Done, .. }))));
    assert_eq!(hashes(&root, "dst"), hashes(&root, "src"), "all 64 hashes equal");
    for e in &evs {
        if let OpsEvent::Evt(OpEvt::ItemsDone { changes, .. }) = e {
            for ch in changes {
                let from = ch.from.as_ref().map(|p| format!("{:?}", files_engine::ops::journal::PathRepr::of(p)));
                assert!(!from.is_some_and(|f| done_before.contains(&f)), "an item done before was copied again");
            }
        }
    }
}

/// Whether item `i` exists exactly once: at its destination, no longer at its source.
fn moved(root: &TempRoot, i: u32) -> bool {
    root.at(&format!("dst/f{i:02}.bin")).exists() && !root.at(&format!("src/f{i:02}.bin")).exists()
}

#[test]
fn host_killed_mid_move() {
    let root = TempRoot::new("kill_host_move");
    let originals: Vec<Vec<u8>> = (0..FILES).map(|i| vec![i as u8; 1 << 20]).collect();
    let (op, _, hooks) = interrupted(&root, "ops_move");
    let left = (0..FILES).filter(|i| root.at(&format!("src/f{i:02}.bin")).exists()).count() as u32;
    let c = in_process_client(&root, hooks);
    let r = c.resume_rest(OpId(op)).unwrap();
    assert!(c.host_pid().is_some(), "the resume runs in a host of its own");
    let _host = HostGuard(c.host_pid());
    // Proved from the disk and from the events that precede the shell's own end of the move, not from Finished: after
    // its last item IFileOperation brings the session's jump lists up to date under one session-wide mutex
    // (Mutex<AppID>.automaticDestinations-ms) that every process moving files takes in turn, so the other moves of
    // the PC, not this host, decide when Finished comes. Every item reaches its destination before that.
    let end = Instant::now() + Duration::from_secs(120);
    let mut evs = Vec::new();
    loop {
        evs.extend(c.drain());
        let started = evs.iter().any(|e| matches!(e, OpsEvent::Evt(OpEvt::Started { op: o, .. }) if *o == r));
        if started && (0..FILES).all(|i| moved(&root, i)) {
            break;
        }
        assert!(Instant::now() < end, "every item exists exactly once, moved: {evs:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        evs.iter().any(|e| matches!(e, OpsEvent::Evt(OpEvt::Pending { of, .. }) if of.0 == op)),
        "the interrupted move is reported: {evs:?}"
    );
    assert!(
        evs.iter()
            .any(|e| matches!(e, OpsEvent::Evt(OpEvt::Started { op: o, items, .. }) if *o == r && *items == left)),
        "the resume takes exactly the {left} items still at the source: {evs:?}"
    );
    let names = |dir: &str| std::fs::read_dir(root.at(dir)).unwrap().count();
    assert_eq!((names("dst"), names("src")), (FILES as usize, 0), "no copy under another name, nothing left behind");
    for (i, bytes) in originals.iter().enumerate() {
        assert_eq!(root.hash(&format!("dst/f{i:02}.bin")), common::temp::fnv1a64(bytes), "f{i:02}: nothing lost");
    }
    evs.extend(c.drain());
    for e in &evs {
        match e {
            OpsEvent::Evt(OpEvt::ItemFailed { op: o, .. } | OpEvt::Refused { op: o, .. }) => assert_ne!(*o, r, "{e:?}"),
            // Only checked when Finished came before the end of the test; the disk is what is proved above.
            OpsEvent::Evt(OpEvt::Finished { op: o, result, .. }) if *o == r => {
                assert_eq!(*result, oled_trust::frame::files::OpResult::Done)
            }
            _ => {}
        }
    }
}

/// Our own host's hidden window: its pid and its class, never the class alone.
fn host_window(pid: u32) -> Option<HWND> {
    struct Find {
        pid: u32,
        found: Option<HWND>,
    }
    unsafe extern "system" fn each(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
        // SAFETY: `lparam` is the address of the Find below, alive for the enumeration.
        let f = unsafe { &mut *(lparam.0 as *mut Find) };
        let mut pid = 0u32;
        // SAFETY: a window handle the enumeration passes; `pid` is writable.
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid == f.pid {
            let mut class = [0u16; 64];
            // SAFETY: as above; the buffer is writable.
            let n = unsafe { GetClassNameW(hwnd, &mut class) } as usize;
            if String::from_utf16_lossy(&class[..n]) == files_engine::role_ops::WINDOW_CLASS {
                f.found = Some(hwnd);
                return false.into();
            }
        }
        true.into()
    }
    let mut f = Find { pid, found: None };
    // SAFETY: the callback only writes into `f`, which outlives the call.
    let _ = unsafe { EnumWindows(Some(each), LPARAM(std::ptr::from_mut(&mut f) as isize)) };
    f.found
}

fn send(hwnd: HWND, msg: u32, wparam: usize) -> usize {
    let mut result = 0usize;
    // SAFETY: our own child's window; a bounded, abort-if-hung send.
    unsafe {
        SendMessageTimeoutW(
            hwnd,
            msg,
            WPARAM(wparam),
            LPARAM(ENDSESSION_LOGOFF as isize),
            SMTO_ABORTIFHUNG,
            2_000,
            Some(&mut result),
        )
    };
    result
}

#[test]
fn end_session_blocks_then_flushes() {
    let root = TempRoot::new("kill_endsession");
    let hooks = common::hooks(&root);
    let items = files(&root, 64 << 10);
    let dst = root.dir("dst").display().to_string();
    let script = [json!({"op": "ops_copy", "items": items, "dest": dst}), json!({"op": "ops_wait", "ms": 600_000})];
    let (_client, rx) = ui(&root, &hooks, 100, &script);
    let sent = line_where(&rx, |v| v.get("sent").is_some());
    let (op, host) = (sent["sent"].as_u64().unwrap() as u32, sent["host_pid"].as_u64().unwrap() as u32);
    line_where(&rx, |v| v["evt"] == "items_done");
    let hwnd = host_window(host).expect("the host's own window, by its pid and class");
    assert_eq!(send(hwnd, WM_QUERYENDSESSION, 0), 0, "a running copy blocks the sign-out");
    let mut buf = [0u16; 256];
    let mut len = buf.len() as u32;
    // SAFETY: our own child's window; the buffer holds `len` units.
    let q = unsafe { ShutdownBlockReasonQuery(hwnd, Some(PWSTR(buf.as_mut_ptr())), &mut len) };
    if q.is_ok() {
        let text = String::from_utf16_lossy(&buf[..buf.iter().position(|&u| u == 0).unwrap_or(0)]);
        assert!(text == "Copying 64 items" || text == "Copia di 64 elementi in corso", "{text}");
    } else {
        println!("ShutdownBlockReasonQuery from another process: {q:?}");
    }
    send(hwnd, WM_ENDSESSION, 1);
    let data = OwnDir::open(&root.at("data")).unwrap();
    let j = parse(&data.read(&journal_rel(OpId(op), host)).unwrap()).expect("the journal's last record parses");
    assert!(!j.records.is_empty());
    line_where(&rx, |v| v["evt"] == "finished");
    assert_eq!(send(hwnd, WM_QUERYENDSESSION, 0), 1, "nothing runs: the sign-out may go on");
    assert_eq!(hashes(&root, "dst"), hashes(&root, "src"));
}

#[test]
fn two_builds_coexist() {
    let root = TempRoot::new("kill_two_builds");
    let real = common::hooks(&root);
    let other = TestHooks { build_tag: Some("aaaaaaaa".into()), ..real.clone() };
    let (a, b) = (in_process_client(&root, real), in_process_client(&root, other));
    let item = [oled_trust::frame::files::ItemRef::Path(wp(&root.file("src/x.txt", b"x")))];
    let oa = a.copy(&item, &wp(&root.dir("dst_a")), oled_trust::frame::files::Conflicts::AskSystem).unwrap();
    let ob = b.copy(&item, &wp(&root.dir("dst_b")), oled_trust::frame::files::Conflicts::AskSystem).unwrap();
    until_finished(&a, oa);
    until_finished(&b, ob);
    assert_ne!(a.host_pid(), b.host_pid(), "the same pipe tag, two builds: two hosts side by side");
    assert!(Path::new(&root.at("dst_a/x.txt")).exists() && Path::new(&root.at("dst_b/x.txt")).exists());
}

// ---- disposable (CI files-disposable job only: production flags, recycling and system dialogs) --------------------

/// Hooks with the production flags: no FILES_TEST_NO_UI; the fence names \`extra\` roots too.
fn production_hooks(root: &TempRoot, extra: &[String], dialogs_under_silent: bool) -> TestHooks {
    let mut h = common::hooks(root);
    h.env.retain(|(k, _)| &**k != "FILES_TEST_NO_UI" && &**k != "FILES_TEST_ROOT");
    let fence =
        std::iter::once(root.path().display().to_string()).chain(extra.iter().cloned()).collect::<Vec<_>>().join(";");
    h.env.push(("FILES_TEST_ROOT".into(), fence.into()));
    if dialogs_under_silent {
        h.env.push(("FILES_TEST_DIALOGS_UNDER_SILENT".into(), "1".into()));
    }
    h
}

/// Closes the visible dialogs (class #32770) and the shell's operation window (class OperationStatusWindow, where some
/// Windows builds ask about a collision even under FOF_SILENT) of our own host `pid` with WM_CLOSE; how many there
/// were.
fn close_dialogs(pid: u32) -> usize {
    struct Find {
        pid: u32,
        n: usize,
    }
    unsafe extern "system" fn each(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
        // SAFETY: \`lparam\` is the address of the Find below, alive for the enumeration.
        let f = unsafe { &mut *(lparam.0 as *mut Find) };
        let mut pid = 0u32;
        // SAFETY: a window handle the enumeration passes; \`pid\` is writable.
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        let mut class = [0u16; 64];
        // SAFETY: as above; the buffer is writable.
        let n = unsafe { GetClassNameW(hwnd, &mut class) } as usize;
        // SAFETY: as above.
        let visible = unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(hwnd) }.as_bool();
        let class = String::from_utf16_lossy(&class[..n]);
        if pid == f.pid && visible && (class == "#32770" || class == "OperationStatusWindow") {
            // SAFETY: a dialog of our own child host, on the throwaway runner only.
            let _ = unsafe {
                windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                    Some(hwnd),
                    windows::Win32::UI::WindowsAndMessaging::WM_CLOSE,
                    WPARAM(0),
                    LPARAM(0),
                )
            };
            f.n += 1;
        }
        true.into()
    }
    let mut f = Find { pid, n: 0 };
    // SAFETY: the callback only writes into \`f\`, which outlives the call.
    let _ = unsafe { EnumWindows(Some(each), LPARAM(std::ptr::from_mut(&mut f) as isize)) };
    f.n
}

/// Every top-level window of process `pid` (hwnd, thread, class, visible, owner), and whether this session has a
/// shell tray and a foreground window: what a run that waits with no NeedsInput reports.
fn windows_of(pid: u32) -> String {
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowW, GetForegroundWindow, GetWindow, IsWindowVisible, GW_OWNER,
    };
    struct Find {
        pid: u32,
        lines: Vec<String>,
    }
    unsafe extern "system" fn each(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
        // SAFETY: `lparam` is the address of the Find below, alive for the enumeration.
        let f = unsafe { &mut *(lparam.0 as *mut Find) };
        let mut pid = 0u32;
        // SAFETY: a window handle the enumeration passes; `pid` is writable.
        let tid = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid == f.pid {
            let mut class = [0u16; 64];
            // SAFETY: as above; the buffer is writable.
            let n = unsafe { GetClassNameW(hwnd, &mut class) } as usize;
            // SAFETY: as above.
            let (visible, owner) = unsafe { (IsWindowVisible(hwnd).as_bool(), GetWindow(hwnd, GW_OWNER).ok()) };
            f.lines.push(format!(
                "{:?} thread {tid} class {} visible {visible} owner {:?}",
                hwnd.0,
                String::from_utf16_lossy(&class[..n]),
                owner.map(|o| o.0)
            ));
        }
        true.into()
    }
    let mut f = Find { pid, lines: Vec::new() };
    // SAFETY: the callback only writes into `f`, which outlives the call.
    let _ = unsafe { EnumWindows(Some(each), LPARAM(std::ptr::from_mut(&mut f) as isize)) };
    // SAFETY: plain queries of this session's windows.
    let (tray, foreground) =
        unsafe { (FindWindowW(windows::core::w!("Shell_TrayWnd"), None).is_ok(), !GetForegroundWindow().is_invalid()) };
    format!("host {pid}: [{}]; shell tray {tray}, a foreground window {foreground}", f.lines.join("; "))
}

/// Runs `op` to its end, closing any dialog the NeedsInput watcher reports; whether one was seen within `within`.
/// A wait of 10 s with no NeedsInput prints the host's windows once, and the panic at 120 s repeats them.
fn finish_closing_dialogs(c: &OpsClient, op: OpId, within: Duration) -> (Vec<OpsEvent>, bool) {
    let start = Instant::now();
    let (mut all, mut prompted) = (Vec::new(), false);
    let mut reported: Option<String> = None;
    loop {
        let evs = c.drain();
        for e in &evs {
            if let OpsEvent::Evt(OpEvt::NeedsInput { on: true, .. }) = e {
                prompted |= start.elapsed() <= within;
                if let Some(pid) = c.host_pid() {
                    close_dialogs(pid);
                }
            }
        }
        let done = evs.iter().any(|e| matches!(e, OpsEvent::Evt(OpEvt::Finished { op: o, .. }) if *o == op));
        all.extend(evs);
        if done {
            return (all, prompted);
        }
        if reported.is_none() && !prompted && start.elapsed() >= Duration::from_secs(10) {
            let w = c.host_pid().map_or_else(|| "no host".to_string(), windows_of);
            println!("no NeedsInput after 10 s: {w}");
            reported = Some(w);
        }
        assert!(start.elapsed() < Duration::from_secs(120), "no Finished: {all:?}; at 10 s: {reported:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
#[ignore = "disposable: CI files-disposable job only"]
fn disposable_recycle_classes() {
    common::disposable();
    let root = TempRoot::new("disp_recycle");
    let vars = ["FILES_DISP_UNC", "FILES_DISP_FAT", "FILES_DISP_QUOTA", "FILES_DISP_NUKE"];
    let dirs: Vec<String> = vars.iter().map(|k| std::env::var(k).expect("set by files-disposable.ps1")).collect();
    let c = in_process_client(&root, production_hooks(&root, &dirs, false));
    let mut record = serde_json::Map::new();
    let mut lost = Vec::new();
    for (k, dir) in vars.iter().zip(&dirs) {
        let name = format!("disp-{}.bin", k.to_lowercase());
        let src = wp(&root.file(&name, &vec![7u8; 2 << 20]));
        let dest = oled_trust::frame::files::WidePath::parse(dir).unwrap();
        let op = c
            .copy(
                &[oled_trust::frame::files::ItemRef::Path(src)],
                &dest,
                oled_trust::frame::files::Conflicts::ReplaceAll,
            )
            .unwrap();
        until_finished(&c, op);
        let target = dest.join(&name.encode_utf16().collect::<Vec<_>>()).unwrap();
        let op = c.recycle(&[oled_trust::frame::files::ItemRef::Path(target.clone())]).unwrap();
        let (evs, prompted) = finish_closing_dialogs(&c, op, Duration::from_secs(120));
        let in_bin = evs.iter().any(|e| {
            matches!(e, OpsEvent::Evt(OpEvt::ItemsDone { changes, .. }) if changes.iter().any(|ch| ch.kind == oled_trust::frame::files::ChangeKind::Recycled && ch.to.is_some()))
        });
        let at_source = Path::new(&target.to_string_lossy()).exists();
        let failed: Vec<i32> = evs
            .iter()
            .filter_map(|e| match e {
                OpsEvent::Evt(OpEvt::ItemFailed { hr, .. }) => Some(*hr),
                _ => None,
            })
            .collect();
        let row = json!({"at_source": at_source, "in_bin": in_bin, "prompted": prompted, "failed_hr": failed});
        println!("FS-3 c {k}: {row}");
        record.insert(k.to_string(), row);
        if !(at_source || in_bin || prompted) {
            lost.push(format!("{k}: gone, not in the bin, and no dialog was seen"));
        }
    }
    // The record is written whatever the outcome, so a failing row still leaves the facts of every row.
    let path = common::temp::artifact("disposable/fs3c.json", Value::Object(record).to_string().as_bytes());
    println!("FS-3 c record: {}", path.display());
    assert!(lost.is_empty(), "{lost:?}");
}

#[test]
#[ignore = "disposable: CI files-disposable job only"]
fn disposable_conflict_dialog() {
    common::disposable();
    let mut record = serde_json::Map::new();
    for under_silent in [true, false] {
        let root = TempRoot::new("disp_conflict");
        root.file("dst/a.txt", b"older");
        let before = root.hash("dst/a.txt");
        let src = oled_trust::frame::files::ItemRef::Path(wp(&root.file("src/a.txt", b"newer")));
        let c = in_process_client(&root, production_hooks(&root, &[], under_silent));
        let op = c.copy(&[src], &wp(&root.at("dst")), oled_trust::frame::files::Conflicts::AskSystem).unwrap();
        let (_, prompted) = finish_closing_dialogs(&c, op, Duration::from_secs(3));
        record.insert(format!("dialogs_under_silent_{under_silent}"), json!({"needs_input_within_3s": prompted}));
        assert_eq!(root.hash("dst/a.txt"), before, "the destination is unchanged");
    }
    let path = common::temp::artifact("disposable/fs3a.json", Value::Object(record).to_string().as_bytes());
    println!("FS-3 a record: {}", path.display());
}
