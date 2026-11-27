//! CI-only smoke tests of the executor (T3.7): they start real processes (the engine's testee and testee_close), so
//! they return at once unless the CI variable is set (house rule 1), and they act only on processes they started.
//! They need the example binaries, which cargo test does not build: cargo build -p tasks --examples --locked, in the
//! same target folder, before the tests (CI's test job does). In CI a missing example fails the test, never passes it.

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use oled_sysmon::{ProcKey, ProcTable, SnapshotReader};
use oled_ui::model::RowKey;
use tasks::app::access;
use tasks::app::exec::{ActVerb, Batch, Executor, KillGuard, Target, Win32Executor};
use tasks::app::launch::RecordingLauncher;
use tasks::model::classify::PathMap;
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetClassNameW, GetWindowThreadProcessId};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn ci() -> bool {
    std::env::var_os("CI").is_some()
}

/// target/<profile>/examples/<name>.exe beside this test's deps folder; the test fails when it was not built.
fn example(name: &str) -> PathBuf {
    let exe = std::env::current_exe().expect("the test's own path");
    let dir = exe.parent().and_then(|deps| deps.parent()).expect("the test's target/<profile> folder");
    let p = dir.join("examples").join(format!("{name}.exe"));
    assert!(p.is_file(), "{} is missing: cargo build -p tasks --examples --locked", p.display());
    p
}

fn snapshot() -> ProcTable {
    let mut t = ProcTable::new();
    SnapshotReader::new().read(&mut t).expect("a snapshot");
    t
}

/// The key of `pid` and of every process below it, root first.
fn tree_of(t: &ProcTable, pid: u32) -> Vec<ProcKey> {
    let Some(root) = t.find_pid(pid).map(|r| r.key) else { return Vec::new() };
    let mut out = Vec::new();
    oled_sys::admin::policy::descendants(root, &access::edges(t), &mut out);
    out
}

/// This test process's guard: itself, its ancestors and the shell can never be reached.
fn own_guard(t: &ProcTable) -> KillGuard {
    // SAFETY: no arguments.
    let me = unsafe { GetCurrentProcessId() };
    let session = t.find_pid(me).map_or(1, |r| r.session);
    access::guard(t, &PathMap::default(), me, None, session, None)
}

fn end_batch(keys: Vec<ProcKey>, t: &ProcTable) -> Batch {
    Batch {
        verb: ActVerb::End,
        targets: vec![Target { row: RowKey(1), members: keys, windows: Vec::new(), launch: None }],
        guard: own_guard(t),
        edges: access::edges(t),
    }
}

fn start(exe: &PathBuf, args: &[&str]) -> Child {
    Command::new(exe)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .expect("the testee starts")
}

fn alive(t: &ProcTable, k: ProcKey) -> bool {
    t.find(k).is_some()
}

/// The processes of `k`'s tree whose image is `exe`'s file: Windows gives each console testee started without a
/// window a conhost.exe child of its own, which is not one of the levels.
fn of_image(t: &ProcTable, keys: Vec<ProcKey>, exe: &std::path::Path) -> Vec<ProcKey> {
    let name = exe.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    keys.into_iter()
        .filter(|k| t.find(*k).is_some_and(|r| t.names.to_string_lossy(r.image).to_lowercase() == name))
        .collect()
}

#[test]
fn end_a_three_level_tree() {
    if !ci() {
        return;
    }
    let exe = example("testee");
    let mut child = start(&exe, &["--tree", "3", "--secs", "30"]);
    let end = Instant::now() + Duration::from_secs(10);
    let keys = loop {
        let t = snapshot();
        let k = of_image(&t, tree_of(&t, child.id()), &exe);
        if k.len() >= 4 || Instant::now() > end {
            break k;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(keys.len(), 4, "the testee and its three levels");
    let exec = Win32Executor::new(Arc::new(RecordingLauncher::default()));
    exec.submit(end_batch(keys.clone(), &snapshot()));
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let t = snapshot();
        if keys.iter().all(|k| !alive(&t, *k)) {
            break;
        }
        assert!(Instant::now() < deadline, "all four gone within 2 s");
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.wait();
}

/// The top-level windows of `pid` whose class is `class`.
fn windows_of(pid: u32, class: &str) -> Vec<isize> {
    struct Probe {
        pid: u32,
        class: Vec<u16>,
        found: Vec<isize>,
    }
    unsafe extern "system" fn each(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: lparam is the address of the Probe on the caller's stack, alive for the whole EnumWindows call.
        let p = unsafe { &mut *(lparam.0 as *mut Probe) };
        let mut owner = 0u32;
        // SAFETY: owner is writable; a window that is gone gives 0.
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut owner)) };
        let mut buf = [0u16; 64];
        // SAFETY: buf is writable for its length.
        let n = unsafe { GetClassNameW(hwnd, &mut buf) } as usize;
        if owner == p.pid && buf.get(..n) == Some(p.class.as_slice()) {
            p.found.push(hwnd.0 as isize);
        }
        BOOL(1)
    }
    let mut p = Probe { pid, class: class.encode_utf16().collect(), found: Vec::new() };
    // SAFETY: the callback only reads its arguments and writes the probe, which outlives the call.
    let _ = unsafe { EnumWindows(Some(each), LPARAM(&mut p as *mut Probe as isize)) };
    p.found
}

#[test]
fn close_leaves_a_save_prompt() {
    if !ci() {
        return;
    }
    let exe = example("testee_close");
    let mut child = start(&exe, &["--secs", "30"]);
    let end = Instant::now() + Duration::from_secs(10);
    let hwnds = loop {
        let w = windows_of(child.id(), "MiniOledTesteeClose");
        if !w.is_empty() || Instant::now() > end {
            break w;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(hwnds.len(), 1, "its window");
    let t = snapshot();
    let key = t.find_pid(child.id()).expect("the testee").key;
    let exec = Win32Executor::new(Arc::new(RecordingLauncher::default()));
    exec.submit(Batch {
        verb: ActVerb::Close,
        targets: vec![Target { row: RowKey(1), members: vec![key], windows: hwnds, launch: None }],
        guard: own_guard(&t),
        edges: Vec::new(),
    });
    std::thread::sleep(Duration::from_secs(5));
    assert!(child.try_wait().expect("wait").is_none(), "still running 5 s later");
    assert_eq!(windows_of(child.id(), "#32770").len(), 1, "with its Save changes? box up");
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn stale_key_kills_nothing() {
    if !ci() {
        return;
    }
    let exe = example("testee");
    let mut old = start(&exe, &["--busy", "1", "--secs", "30"]);
    std::thread::sleep(Duration::from_millis(500));
    let stale = snapshot().find_pid(old.id()).expect("the first testee").key;
    let _ = old.kill();
    let _ = old.wait();
    let mut new = start(&exe, &["--busy", "1", "--secs", "30"]);
    std::thread::sleep(Duration::from_millis(500));
    let t = snapshot();
    let fresh = t.find_pid(new.id()).expect("the second testee").key;
    // The old key, whatever process holds its pid now.
    let exec = Win32Executor::new(Arc::new(RecordingLauncher::default()));
    exec.submit(end_batch(vec![stale], &t));
    std::thread::sleep(Duration::from_secs(1));
    assert!(alive(&snapshot(), fresh), "the new testee lives");
    assert!(new.try_wait().expect("wait").is_none());
    let _ = new.kill();
    let _ = new.wait();
}
