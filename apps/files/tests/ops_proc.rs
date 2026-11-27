//! The ops host process (OP5c): a real host per test, with its own tags and data folder, that never sees a request.

mod common;

use std::os::windows::io::AsRawHandle;
use std::time::{Duration, Instant};

use common::temp::TempRoot;
use files_engine::host::named::{connect, FrameIo};
use oled_trust::frame::files::OPS;
use oled_trust::frame::{decode_hello, encode_hello};
use windows::Win32::Foundation::{FILETIME, HANDLE};
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

/// Starts files.exe --role=ops with the test's hooks; returns the child and its pipe name.
fn host(root: &TempRoot, args: &[&str]) -> (common::ChildGuard, String) {
    let hooks = common::hooks(root);
    let env: Vec<(&str, &str)> = hooks.env.iter().map(|(k, v)| (&**k, &**v)).collect();
    let tag = hooks.pipe_tag.as_deref().unwrap();
    let mut all = env.clone();
    all.push(("FILES_TEST_PIPE_TAG", tag));
    let child = common::spawn_no_window(&common::files_exe(), args, &all);
    let name = files_engine::role_ops::pipe_name(Some(tag), None).unwrap();
    (child, name)
}

#[test]
fn host_exits_when_idle() {
    let root = TempRoot::new("ops_idle_exit");
    let (mut child, name) = host(&root, &["--role=ops", "--proto=1"]);
    let mut conn = connect(&name, Duration::from_secs(20), true).expect("the host serves its pipe");
    assert_eq!(conn.server_pid(), Some(child.pid()));
    let hello = conn.recv_frame().unwrap().expect("the host's Hello");
    decode_hello(&OPS, &hello).expect("an ops Hello");
    let mut mine = Vec::new();
    encode_hello(&OPS, files_engine::BUILD_ID, &mut mine);
    conn.send_frame(&mine).unwrap();
    let start = Instant::now();
    drop(conn);
    let status = child.wait_timeout(Duration::from_secs(30)).expect("gone within 30 s (the design value is 5 s)");
    assert_eq!(status.code(), Some(0), "a normal end");
    println!("the idle host left after {} ms", start.elapsed().as_millis());
    assert!(root.at("data").is_dir(), "its own data folder");
}

#[test]
fn wrong_proto_exits_3() {
    let root = TempRoot::new("ops_wrong_proto");
    let (mut child, _) = host(&root, &["--role=ops", "--proto=2"]);
    let status = child.wait_timeout(Duration::from_secs(20)).expect("ends at once");
    assert_eq!(status.code(), Some(files_engine::exit::PROTO_MISMATCH));
    let (mut child, _) = host(&root, &["--role=ops"]);
    assert_eq!(child.wait_timeout(Duration::from_secs(20)).unwrap().code(), Some(files_engine::exit::USAGE));
}

/// The creation time (FILETIME) of the process `h`.
fn created(h: HANDLE) -> u64 {
    let mut t = [FILETIME::default(); 4];
    let [c, e, k, u] = &mut t;
    // SAFETY: a process handle of this test (its own pseudo-handle or a child's); four writable FILETIMEs.
    unsafe { GetProcessTimes(h, c, e, k, u) }.unwrap();
    (u64::from(t[0].dwHighDateTime) << 32) | u64::from(t[0].dwLowDateTime)
}

/// An interrupted operation's host is dead once it has exited, even while a handle (a parent's, or Windows Error
/// Reporting's after a crash) keeps its process object and its id: its journal is offered for resume at once.
#[test]
fn exited_host_is_dead_while_referenced() {
    use files_engine::ops::host::owner_alive;
    let root = TempRoot::new("ops_exited_owner");
    let (mut child, _) = host(&root, &["--role=ops", "--proto=2"]);
    child.wait_timeout(Duration::from_secs(20)).expect("ends at once");
    // The guard still holds the child's handle.
    let at = created(HANDLE(child.child().as_raw_handle()));
    assert!(!owner_alive(child.pid(), at), "an exited host is dead");
    // SAFETY: the pseudo-handle of this process.
    let own = created(unsafe { GetCurrentProcess() });
    assert!(owner_alive(std::process::id(), own), "this process lives");
    assert!(!owner_alive(std::process::id(), own + 1), "another creation time: another process");
}

#[test]
fn second_host_exits_4() {
    let root = TempRoot::new("ops_second_host");
    let hooks = common::hooks(&root);
    let tag = hooks.pipe_tag.as_deref().unwrap().to_string();
    let env: Vec<(&str, &str)> =
        hooks.env.iter().map(|(k, v)| (&**k, &**v)).chain([("FILES_TEST_PIPE_TAG", tag.as_str())]).collect();
    let first = common::spawn_no_window(&common::files_exe(), &["--role=ops", "--proto=1"], &env);
    let name = files_engine::role_ops::pipe_name(Some(&tag), None).unwrap();
    let conn = connect(&name, Duration::from_secs(20), true).expect("the first host serves");
    let mut second = common::spawn_no_window(&common::files_exe(), &["--role=ops", "--proto=1"], &env);
    let status = second.wait_timeout(Duration::from_secs(20)).expect("the second ends at once");
    assert_eq!(status.code(), Some(files_engine::exit::PIPE_TAKEN));
    drop(conn);
    drop(first);
}
