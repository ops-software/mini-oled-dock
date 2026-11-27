//! The shell host process (SH2, SH4a-SH4c, SH5a-SH5c, SH6): spawned directly with an explicit handle list, spoken to
//! frame by frame, and ended through the kill protocol. No test here shows a window or a menu.
// The shell host's test hooks (TestHooks, the test client, FILES_FAULT_*) exist in debug builds only.
#![cfg(debug_assertions)]

mod common;

use std::io::{BufRead, BufReader};
use std::time::Duration;

use common::temp::TempRoot;
use files_engine::host::{anon, proc};
use files_engine::testclient::shell::Direct;
use oled_trust::frame;
use oled_trust::frame::files::{CanonVerb, ItemRef, MenuOutcome, ReqId, ShellEvt, ShellReq, Verb, WidePath, SHELL};

/// The environment of one test's host: its own data folder, its root, no system UI, this process as the parent.
fn env_of(root: &TempRoot, extra: &[(&str, &str)]) -> Vec<(String, String)> {
    let h = common::hooks(root);
    let mut env: Vec<(String, String)> = h.env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    env.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    env
}

fn pairs(env: &[(String, String)]) -> Vec<(&str, &str)> {
    env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
}

const TEN_S: Duration = Duration::from_secs(10);

#[test]
fn only_listed_handles_are_inherited() {
    let root = TempRoot::new("inherit");
    let (report_r, report_w) = anon::pipe_pair().expect("pipe");
    let (other_r, other_w) = anon::pipe_pair().expect("pipe");
    let listed = anon::inheritable(&report_w).expect("inheritable");
    let unlisted = anon::inheritable(&other_w).expect("inheritable");
    let line =
        format!("{{\"op\":\"probe-handle\",\"out\":{},\"check\":{}}}\n", anon::arg_of(listed), anon::arg_of(unlisted));
    let script = root.file("probe.jsonl", line.as_bytes());
    let script = script.to_str().expect("a UTF-8 temp path");
    let child = proc::spawn_inheriting(&common::files_exe(), &["--role=test-client", script], &[], &[listed])
        .expect("the test client starts");
    drop(report_w);
    let mut report = String::new();
    std::io::BufReader::new(report_r).read_line(&mut report).expect("the report line");
    assert_eq!(child.wait_ms(10_000), Some(0), "the probe ends with 0");
    let report = report.trim();
    assert!(report.starts_with("listed=ok unlisted="), "{report}");
    assert_ne!(report, "listed=ok unlisted=pipe", "an unlisted inheritable handle reached the child");
    drop((other_r, other_w));
}

#[test]
fn host_says_ready() {
    let root = TempRoot::new("ready");
    let env = env_of(&root, &[]);
    let (d, pid) = Direct::ready(&common::files_exe(), &pairs(&env), TEN_S).expect("Hello and Ready");
    assert_eq!(pid, d.host.child.pid, "Ready names the host's own pid");
    assert_eq!(d.section().host_pid(), pid, "the host wrote its pid into the crumbs");
}

#[test]
fn proto_mismatch_exits_3() {
    let root = TempRoot::new("proto");
    let env = env_of(&root, &[]);
    let mut forged = Vec::new();
    frame::encode_hello(&frame::Proto { version: SHELL.version + 1, ..SHELL }, files_engine::BUILD_ID, &mut forged);
    let hp = files_engine::shell_client::spawn_host_with_hello(&common::files_exe(), &pairs(&env), &forged)
        .expect("spawn with a Hello of the next protocol version");
    assert_eq!(hp.child.wait_ms(10_000), Some(3), "protocol mismatch");
}

#[test]
fn idle_host_exits() {
    let root = TempRoot::new("idle");
    let env = env_of(&root, &[("FILES_TEST_SHELL_IDLE_MS", "500")]);
    let (d, _) = Direct::ready(&common::files_exe(), &pairs(&env), TEN_S).expect("Hello and Ready");
    assert_eq!(d.host.child.wait_ms(10_000), Some(0), "an idle host is gone within 10 s");
}

#[test]
fn parent_death_ends_idle_host() {
    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};
    let root = TempRoot::new("parent");
    let script = root.file("s.jsonl", b"{\"op\":\"shell_spawn\"}\n{\"op\":\"sleep_ms\",\"ms\":60000}\n");
    let env = env_of(&root, &[]);
    let script = script.to_str().expect("a UTF-8 temp path");
    let mut client = common::spawn_no_window(&common::files_exe(), &["--role=test-client", script], &pairs(&env));
    let mut line = String::new();
    BufReader::new(client.take_stdout().expect("stdout")).read_line(&mut line).expect("the host line");
    let pid: u32 = line.trim().strip_prefix("host ").and_then(|p| p.parse().ok()).expect("host <pid>");
    // SAFETY: a wait-only handle to the host, closed below.
    let host = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }.expect("the host runs");
    let _ = client.child().kill();
    let _ = client.child().wait();
    // SAFETY: the handle opened above.
    let gone = unsafe { WaitForSingleObject(host, 5_000) } == WAIT_OBJECT_0;
    // SAFETY: opened above, closed once.
    let _ = unsafe { CloseHandle(host) };
    assert!(gone, "the host outlived its UI by more than 5 s");
}

fn item(p: &std::path::Path) -> ItemRef {
    ItemRef::Path(WidePath::parse(p.to_str().expect("a UTF-8 temp path")).expect("an absolute path"))
}

fn menu_req(req: u32, items: Vec<ItemRef>) -> ShellReq {
    ShellReq::Menu { req: ReqId(req), items, at: (0, 0), extended: false, owner: 0 }
}

fn is_menu_end(e: &ShellEvt) -> bool {
    matches!(e, ShellEvt::MenuDone { .. } | ShellEvt::Failed { .. })
}

#[test]
fn menu_builds_for_a_temp_file() {
    let root = TempRoot::new("menu");
    let file = root.file("a.txt", b"text");
    let env = env_of(&root, &[]);
    let (mut d, _) = Direct::ready(&common::files_exe(), &pairs(&env), TEN_S).expect("Hello and Ready");
    d.send(&menu_req(1, vec![item(&file)])).expect("sent");
    let got = d.until(Duration::from_secs(30), is_menu_end).expect("the menu ends");
    assert_eq!(
        got,
        [
            ShellEvt::MenuShown { req: ReqId(1) },
            ShellEvt::MenuDone { req: ReqId(1), outcome: MenuOutcome::Dismissed, items: vec![] }
        ],
        "built, shown in test mode (no window), nothing picked"
    );
    let other = root.dir("sub");
    d.send(&menu_req(2, vec![item(&file), item(&other.join("x"))])).expect("sent");
    let got = d.until(TEN_S, is_menu_end).expect("an answer");
    assert!(matches!(got.last(), Some(ShellEvt::Failed { req: ReqId(2), .. })), "two folders: {got:?}");
}

#[test]
fn pick_rename_is_intercepted() {
    let root = TempRoot::new("rename");
    let file = root.file("b.txt", b"keep");
    let env = env_of(&root, &[("FILES_TEST_MENU_PICK", "rename")]);
    let (mut d, _) = Direct::ready(&common::files_exe(), &pairs(&env), TEN_S).expect("Hello and Ready");
    let items = vec![item(&file)];
    d.send(&menu_req(7, items.clone())).expect("sent");
    let got = d.until(Duration::from_secs(30), is_menu_end).expect("the menu ends");
    assert_eq!(
        got.last(),
        Some(&ShellEvt::MenuDone { req: ReqId(7), outcome: MenuOutcome::Intercepted(CanonVerb::Rename), items }),
        "{got:?}"
    );
    assert_eq!(std::fs::read(&file).expect("still there, same name"), b"keep");
}

#[test]
fn exec_unknown_verbs_fail_notimpl() {
    let root = TempRoot::new("notimpl");
    let file = root.file("a.txt", b"x");
    let env = env_of(&root, &[]);
    let (mut d, _) = Direct::ready(&common::files_exe(), &pairs(&env), TEN_S).expect("Hello and Ready");
    let e_notimpl = windows::Win32::Foundation::E_NOTIMPL.0;
    for (i, verb) in [Verb::Pin, Verb::Unpin, Verb::Share, Verb::InvokeModern(1)].into_iter().enumerate() {
        let req = ReqId(10 + i as u32);
        d.send(&ShellReq::Exec { req, verb, items: vec![item(&file)] }).expect("sent");
        let got =
            d.until(TEN_S, |e| matches!(e, ShellEvt::Failed { .. } | ShellEvt::ExecDone { .. })).expect("an answer");
        assert_eq!(got, [ShellEvt::Failed { req, hr: e_notimpl }], "{verb:?}");
    }
}

#[test]
fn exec_open_of_a_missing_file_fails() {
    let root = TempRoot::new("missing");
    let missing = root.path().join("missing.txt");
    let env = env_of(&root, &[]);
    let (mut d, _) = Direct::ready(&common::files_exe(), &pairs(&env), TEN_S).expect("Hello and Ready");
    d.send(&ShellReq::Exec { req: ReqId(3), verb: Verb::Open, items: vec![item(&missing)] }).expect("sent");
    let got = d.until(Duration::from_secs(30), |e| matches!(e, ShellEvt::ExecDone { .. } | ShellEvt::Failed { .. }));
    let got = got.expect("an answer");
    match got.as_slice() {
        [ShellEvt::Busy { on: true }, ShellEvt::Busy { on: false }, ShellEvt::ExecDone { req: ReqId(3), hr }] => {
            assert!(*hr < 0, "a failure HRESULT: {hr:#x}")
        }
        other => panic!("unexpected: {other:?}"),
    }
    assert!(!missing.exists());
}
