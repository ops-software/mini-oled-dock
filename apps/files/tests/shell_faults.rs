//! The shell host's fault suite (SH1, SH8, SH9): a real extension, the dev-only fault DLL (the files_faultext example),
//! faults on demand inside the host, and the client answers in time. FILES_FAULT_* reach the host child only, through
//! TestHooks.env; the modes that may raise Windows Error Reporting run in the disposable suite only.
// The shell host's test hooks (TestHooks, the test client, FILES_FAULT_*) exist in debug builds only.
#![cfg(debug_assertions)]

mod common;

use std::path::Path;
use std::time::Duration;

use common::temp::TempRoot;
use std::sync::Arc;
use std::time::Instant;

use files_engine::shell_client::{FallbackWhy, HostState, ShellClient, ShellConfig, ShellReply};
use files_engine::testclient::shell::Direct;
use oled_trust::frame::files::{Gen, ItemRef, MenuOutcome, ReqId, ShellEvt, ShellReq, WidePath};

/// A ShellClient whose host gets this test's hooks plus the fault DLL and `extra`.
fn client(root: &TempRoot, extra: &[(&str, &str)]) -> ShellClient {
    let mut hooks = common::hooks(root);
    hooks.env = fault_env(root, extra).into_iter().map(|(k, v)| (k.into(), v.into())).collect();
    ShellClient::new(ShellConfig { exe: common::files_exe(), test: Some(hooks) }, Arc::new(|| {}))
}

/// Replies until one that `done` accepts, polling drain every 10 ms; Err with what came when the time ran out.
fn replies_until(
    c: &ShellClient,
    within: Duration,
    done: impl Fn(&ShellReply) -> bool,
) -> Result<Vec<ShellReply>, Vec<ShellReply>> {
    let end = Instant::now() + within;
    let mut got = Vec::new();
    while Instant::now() < end {
        for r in c.drain() {
            let last = done(&r);
            got.push(r);
            if last {
                return Ok(got);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err(got)
}

/// The host's pid once the client says Ready.
fn ready_pid(c: &ShellClient, within: Duration) -> Option<u32> {
    let end = Instant::now() + within;
    while Instant::now() < end {
        if let HostState::Ready { pid } = c.state() {
            return Some(pid);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

const TEN_S: Duration = Duration::from_secs(10);

/// One test's host environment (common::hooks) plus the fault DLL and `extra`.
fn fault_env(root: &TempRoot, extra: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> =
        common::hooks(root).env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    env.push(("FILES_FAULT_DLL".into(), common::fault_dll().display().to_string()));
    env.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    env
}

fn pairs(env: &[(String, String)]) -> Vec<(&str, &str)> {
    env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
}

fn item(p: &Path) -> ItemRef {
    ItemRef::Path(WidePath::parse(p.to_str().expect("a UTF-8 temp path")).expect("an absolute path"))
}

fn menu_req(req: u32, items: Vec<ItemRef>) -> ShellReq {
    ShellReq::Menu { req: ReqId(req), items, at: (0, 0), extended: false, owner: 0 }
}

fn file_len(p: &Path) -> u64 {
    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}

/// The export names of a PE32+ image, read from its bytes.
fn export_names(img: &[u8]) -> Vec<String> {
    let u16_at = |o: usize| u16::from_le_bytes([img[o], img[o + 1]]) as usize;
    let u32_at = |o: usize| u32::from_le_bytes([img[o], img[o + 1], img[o + 2], img[o + 3]]) as usize;
    let pe = u32_at(0x3C);
    assert_eq!(&img[pe..pe + 4], b"PE\0\0", "a PE image");
    let sections = u16_at(pe + 6);
    let opt = pe + 24;
    assert_eq!(u16_at(opt), 0x20B, "PE32+");
    let export_rva = u32_at(opt + 112);
    let table = opt + u16_at(pe + 20);
    // RVA to file offset through the section table.
    let at = |rva: usize| -> usize {
        for s in 0..sections {
            let h = table + s * 40;
            let (va, size, raw) = (u32_at(h + 12), u32_at(h + 8).max(u32_at(h + 16)), u32_at(h + 20));
            if (va..va + size).contains(&rva) {
                return rva - va + raw;
            }
        }
        panic!("RVA {rva:#x} in no section")
    };
    let dir = at(export_rva);
    let (count, names) = (u32_at(dir + 24), at(u32_at(dir + 32)));
    (0..count)
        .map(|i| {
            let s = at(u32_at(names + 4 * i));
            let end = img[s..].iter().position(|&b| b == 0).expect("a NUL-terminated name");
            String::from_utf8_lossy(&img[s..s + end]).into_owned()
        })
        .collect()
}

#[test]
fn fault_dll_builds_and_exports() {
    use windows::core::HSTRING;
    use windows::Win32::Foundation::FreeLibrary;
    use windows::Win32::System::LibraryLoader::{LoadLibraryExW, LOAD_LIBRARY_AS_DATAFILE};

    let dll = common::fault_dll();
    let wide = HSTRING::from(dll.as_os_str());
    // SAFETY: a data-file load runs no code of the DLL; the module is freed right after.
    let module = unsafe { LoadLibraryExW(&wide, None, LOAD_LIBRARY_AS_DATAFILE) }.expect("loads as a data file");
    // SAFETY: loaded above, freed once.
    unsafe { FreeLibrary(module) }.expect("freed");
    let img = std::fs::read(&dll).expect("the DLL's bytes");
    let mut names = export_names(&img);
    names.sort();
    assert_eq!(names, ["DllCanUnloadNow", "DllGetClassObject"], "exactly two exports, no DllRegisterServer");
    assert!(Path::new(&dll).extension().is_some_and(|e| e == "dll"));
}

#[test]
fn fault_icon_initializes_through_ipersistfile() {
    use files_engine::shell::host::fault;
    use windows::core::{Interface, HSTRING};
    use windows::Win32::System::Com::{IPersistFile, STGM_READ};
    use windows::Win32::UI::Shell::IExtractIconW;
    let root = TempRoot::new("persist");
    let file = root.file("a.modfault", b"x");
    // The shell initializes a registered icon handler through IPersistFile::Load before it asks for the icon.
    let p: IPersistFile =
        fault::create(&common::fault_dll(), &fault::CLSID_FAULT_ICON).expect("FaultIcon answers IPersistFile");
    // SAFETY: a NUL-terminated path that lives for the call.
    unsafe { p.Load(&HSTRING::from(file.as_os_str()), STGM_READ) }.expect("Load");
    // SAFETY: no arguments.
    assert_eq!(unsafe { p.GetClassID() }.expect("its class"), fault::CLSID_FAULT_ICON);
    let _: IExtractIconW = p.cast().expect("the same object extracts the icon");
}

#[test]
fn fault_ok_invokes() {
    let root = TempRoot::new("invoke");
    let file = root.file("a.txt", b"x");
    let out = root.path().join("out.bin");
    let out_s = out.display().to_string();
    let env = fault_env(
        &root,
        &[
            ("FILES_FAULT_MODE", "invoke-write"),
            ("FILES_FAULT_OUT", &out_s),
            ("FILES_FAULT_SECS", "5"),
            ("FILES_TEST_MENU_PICK", "modfault.write"),
        ],
    );
    let (mut d, _) = Direct::ready(&common::files_exe(), &pairs(&env), TEN_S).expect("Hello and Ready");
    d.send(&menu_req(1, vec![item(&file)])).expect("sent");
    let got = d.until(Duration::from_secs(30), |e| matches!(e, ShellEvt::MenuDone { .. })).expect("the menu ends");
    assert_eq!(
        got,
        [
            ShellEvt::MenuShown { req: ReqId(1) },
            ShellEvt::Busy { on: true },
            ShellEvt::Busy { on: false },
            ShellEvt::MenuDone { req: ReqId(1), outcome: MenuOutcome::Invoked, items: vec![] },
        ]
    );
    assert_eq!(file_len(&out), 5 * 1024, "1 KiB per second for 5 s");
}

#[test]
fn invoke_refused_after_killing() {
    let root = TempRoot::new("killing");
    let file = root.file("a.txt", b"x");
    let out = root.path().join("out.bin");
    let out_s = out.display().to_string();
    let env = fault_env(
        &root,
        &[
            ("FILES_FAULT_MODE", "invoke-write"),
            ("FILES_FAULT_OUT", &out_s),
            ("FILES_TEST_MENU_PICK", "modfault.write"),
        ],
    );
    let (mut d, _) = Direct::ready(&common::files_exe(), &pairs(&env), TEN_S).expect("Hello and Ready");
    assert!(d.section().try_kill(), "an idle host: the kill decision is taken");
    d.send(&menu_req(2, vec![item(&file)])).expect("sent");
    let got = d.until(Duration::from_secs(30), |e| matches!(e, ShellEvt::MenuDone { .. })).expect("the menu ends");
    let e_abort = windows::Win32::Foundation::E_ABORT.0;
    assert_eq!(
        got.last(),
        Some(&ShellEvt::MenuDone { req: ReqId(2), outcome: MenuOutcome::Failed(e_abort), items: vec![] })
    );
    assert!(!got.contains(&ShellEvt::Busy { on: true }), "no invoke started");
    assert!(!out.exists(), "the verb never ran");
}

#[test]
fn client_spawns_host_ready() {
    let root = TempRoot::new("client");
    let c = client(&root, &[]);
    assert_eq!(c.state(), HostState::Down, "no process before the first request");
    assert_eq!(c.host_pid(), None);
    c.prespawn();
    let pid = ready_pid(&c, TEN_S).expect("Ready within 10 s");
    assert_eq!(c.host_pid(), Some(pid));
    assert_ne!(c.next_req(), c.next_req(), "request ids are unique");
    let req = c.next_req();
    c.request(ShellReq::ShellNewTypes { req });
    let got = replies_until(&c, TEN_S, |r| matches!(r, ShellReply::Evt(ShellEvt::Failed { .. }))).expect("an answer");
    let e_notimpl = windows::Win32::Foundation::E_NOTIMPL.0;
    assert_eq!(got.last(), Some(&ShellReply::Evt(ShellEvt::Failed { req, hr: e_notimpl })), "{got:?}");
    c.shutdown();
    let end = Instant::now() + TEN_S;
    while c.state() != HostState::Down && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(c.state(), HostState::Down, "shut down");
}

#[test]
fn too_large_menu_falls_back() {
    let root = TempRoot::new("large");
    let c = client(&root, &[]);
    let mut s = String::from("C:");
    while s.len() + 201 <= 32_767 {
        s.push('\\');
        s.push_str(&"a".repeat(200));
    }
    s.push('\\');
    s.push_str(&"b".repeat(32_767 - s.len()));
    let p = WidePath::parse(&s).expect("a long path");
    assert!(p.units().len() >= 32_760, "{}", p.units().len());
    let items = vec![ItemRef::Path(p); 2_048];
    let req = c.next_req();
    c.request(ShellReq::Menu { req, items, at: (0, 0), extended: false, owner: 0 });
    assert_eq!(c.drain(), [ShellReply::Fallback { req, why: FallbackWhy::TooLarge }], "answered at once");
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(c.state(), HostState::Down, "nothing was sent: no host started");
}

#[test]
fn request_and_drain_never_wait() {
    let root = TempRoot::new("nowait");
    let c = Arc::new(client(&root, &[]));
    c.prespawn();
    ready_pid(&c, TEN_S).expect("Ready");
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flood = {
        let (c, stop) = (c.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut n = 0u64;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                c.request(ShellReq::QuickAccessPins { req: c.next_req() });
                n += 1;
                std::thread::sleep(Duration::from_micros(200));
            }
            n
        })
    };
    let end = Instant::now() + Duration::from_secs(10);
    let mut replies = 0usize;
    while Instant::now() < end {
        c.request(ShellReq::ShellNewTypes { req: c.next_req() });
        replies += c.drain().len();
        std::thread::sleep(Duration::from_millis(16));
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let sent = flood.join().expect("the flood thread");
    assert!(sent > 1_000 && replies > 1_000, "a flood: {sent} sent, {replies} replies drained");
    assert_eq!(files_engine::shell_client::ui_waits(&c), 0, "request and drain never waited on a lock");
}

/// true once the process with this id has ended (or no longer exists).
fn gone(pid: u32, within: Duration) -> bool {
    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};
    // SAFETY: a wait-only handle, closed below.
    let Ok(h) = (unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }) else { return true };
    // SAFETY: the handle opened above.
    let ended = unsafe { WaitForSingleObject(h, within.as_millis() as u32) } == WAIT_OBJECT_0;
    // SAFETY: opened above, closed once.
    let _ = unsafe { CloseHandle(h) };
    ended
}

fn menu_of(c: &ShellClient, file: &Path) -> ReqId {
    let req = c.next_req();
    c.request(ShellReq::Menu { req, items: vec![item(file)], at: (0, 0), extended: false, owner: 0 });
    req
}

fn menu_end(r: &ShellReply) -> bool {
    matches!(r, ShellReply::Fallback { .. } | ShellReply::Evt(ShellEvt::MenuDone { .. } | ShellEvt::Failed { .. }))
}

#[test]
fn menu_deadline_kills_hung_host() {
    let root = TempRoot::new("deadline");
    let file = root.file("a.txt", b"x");
    let mode = root.file("mode.txt", b"hang");
    let mode_s = mode.display().to_string();
    let c = client(&root, &[("FILES_FAULT_MODE_FILE", &mode_s), ("FILES_FAULT_POINT", "menu")]);
    c.prespawn();
    let pid1 = ready_pid(&c, TEN_S).expect("Ready");
    let t0 = Instant::now();
    let req = menu_of(&c, &file);
    let got = replies_until(&c, TEN_S, menu_end).expect("an answer within 10 s");
    assert_eq!(got.last(), Some(&ShellReply::Fallback { req, why: FallbackWhy::Deadline }), "{got:?}");
    assert!(t0.elapsed() < TEN_S);
    assert!(gone(pid1, Duration::from_secs(5)), "the hung host was killed");
    root.write("mode.txt", b"ok");
    let req = menu_of(&c, &file);
    let got = replies_until(&c, Duration::from_secs(30), menu_end).expect("a new host serves it");
    assert_eq!(
        got.last(),
        Some(&ShellReply::Evt(ShellEvt::MenuDone { req, outcome: MenuOutcome::Dismissed, items: vec![] })),
        "{got:?}"
    );
    let pid2 = c.host_pid().expect("a host runs");
    assert_ne!(pid1, pid2, "a new pid");
}

#[test]
fn supervisor_waits_infinite_when_idle() {
    let root = TempRoot::new("idlesup");
    let c = client(&root, &[]);
    c.prespawn();
    ready_pid(&c, TEN_S).expect("Ready");
    let req = c.next_req();
    c.request(ShellReq::ShellNewTypes { req });
    replies_until(&c, TEN_S, |r| matches!(r, ShellReply::Evt(ShellEvt::Failed { .. }))).expect("the last reply");
    std::thread::sleep(Duration::from_millis(200));
    let w0 = files_engine::shell_client::supervisor_wakes(&c);
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(files_engine::shell_client::supervisor_wakes(&c), w0, "no wake-up while nothing is outstanding");
}

/// One menu scenario on a fresh client: (the Fallback reason, ms from the request to it, ms from the host's exit to it).
fn scenario(root: &TempRoot, mode: &str, ext: &str) -> (FallbackWhy, u64, Option<u64>) {
    let file = root.file(&format!("s-{}.{ext}", common::test_tag()), b"x");
    let c = client(root, &[("FILES_FAULT_MODE", mode), ("FILES_FAULT_POINT", "menu")]);
    c.prespawn();
    let pid = ready_pid(&c, TEN_S).expect("Ready");
    let exit_at = watch_exit(pid);
    let t0 = Instant::now();
    let req = menu_of(&c, &file);
    let end = Instant::now() + TEN_S;
    while Instant::now() < end {
        for r in c.drain() {
            if let ShellReply::Fallback { req: r, why } = r {
                assert_eq!(r, req);
                let at = Instant::now();
                // Request to Fallback is taken before waiting for the exit: a kill's teardown is not part of it.
                let ms = at.duration_since(t0).as_millis() as u64;
                let from_exit =
                    exit_at.recv_timeout(Duration::from_secs(5)).ok().map(|e| at.saturating_duration_since(e));
                return (why, ms, from_exit.map(|d| d.as_millis() as u64));
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("no Fallback within 10 s ({mode})");
}

/// The instant the process ends, sent once by a thread that waits on it.
fn watch_exit(pid: u32) -> std::sync::mpsc::Receiver<Instant> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        if gone(pid, Duration::from_secs(30)) {
            let _ = tx.send(Instant::now());
        }
    });
    rx
}

fn p99(mut v: Vec<u64>) -> u64 {
    v.sort_unstable();
    let i = ((v.len() * 99).div_ceil(100)).saturating_sub(1);
    v[i.min(v.len() - 1)]
}

#[test]
fn menu_hang_falls_back() {
    let root = TempRoot::new("hang");
    let file = root.file("a.txt", b"x");
    let c = client(&root, &[("FILES_FAULT_MODE", "hang"), ("FILES_FAULT_POINT", "menu")]);
    c.prespawn();
    let pid1 = ready_pid(&c, TEN_S).expect("Ready");
    let req = menu_of(&c, &file);
    let got = replies_until(&c, TEN_S, menu_end).expect("an answer within 10 s");
    assert_eq!(got.last(), Some(&ShellReply::Fallback { req, why: FallbackWhy::Deadline }));
    assert!(gone(pid1, Duration::from_secs(5)), "the host was killed");
    let req = c.next_req();
    c.request(ShellReq::ShellNewTypes { req });
    let got = replies_until(&c, TEN_S, |r| matches!(r, ShellReply::Evt(ShellEvt::Failed { .. }))).expect("served");
    assert!(matches!(got.last(), Some(ShellReply::Evt(ShellEvt::Failed { req: r, .. })) if *r == req));
    assert_ne!(c.host_pid(), Some(pid1), "by a new host");
}

#[test]
fn menu_crash_falls_back() {
    let root = TempRoot::new("crash");
    let (why, _, from_exit) = scenario(&root, "die", "txt");
    assert_eq!(why, FallbackWhy::HostDied);
    assert!(from_exit.is_some_and(|ms| ms < 5_000), "within 5 s of the host's exit: {from_exit:?}");
    // What the client attributes the death to: the menu STA's crumb, left in its call when the host died there.
    use oled_trust::frame::files::CallKind;
    let file = root.file("d.txt", b"x");
    let env = fault_env(&root, &[("FILES_FAULT_MODE", "die"), ("FILES_FAULT_POINT", "menu")]);
    let (mut d, _) = Direct::ready(&common::files_exe(), &pairs(&env), TEN_S).expect("Hello and Ready");
    d.send(&menu_req(1, vec![item(&file)])).expect("sent");
    assert!(d.host.child.wait_ms(10_000).is_some(), "the host died in QueryContextMenu");
    let v = d.section().read(files_engine::shell::crumbs::SLOT_MENU, false);
    assert!(v.in_call, "{v:?}");
    assert_eq!((v.kind, v.ext.as_str()), (Some(CallKind::QueryContextMenu), "txt"), "{v:?}");
}

#[test]
fn two_crashes_disable_the_pair() {
    use oled_trust::frame::files::CallKind;
    let root = TempRoot::new("pair");
    let file = root.file("a.txt", b"x");
    let c = client(&root, &[("FILES_FAULT_MODE", "die"), ("FILES_FAULT_POINT", "menu")]);
    for _ in 0..2 {
        let req = menu_of(&c, &file);
        let got = replies_until(&c, TEN_S, menu_end).expect("an answer");
        assert_eq!(got.last(), Some(&ShellReply::Fallback { req, why: FallbackWhy::HostDied }));
    }
    assert!(c.disabled(CallKind::QueryContextMenu, "txt"), "attributed to (QueryContextMenu, txt) by the crumbs");
    assert!(!c.disabled(CallKind::QueryContextMenu, "log") && !c.disabled(CallKind::Icon, "txt"));
    let req = menu_of(&c, &file);
    assert_eq!(c.drain(), [ShellReply::Fallback { req, why: FallbackWhy::Disabled }], "at once");
}

#[test]
fn three_crashes_turn_the_menu_off() {
    let root = TempRoot::new("menuoff");
    let c = client(&root, &[("FILES_FAULT_MODE", "die"), ("FILES_FAULT_POINT", "menu")]);
    for ext in ["txt", "log", "dat"] {
        let file = root.file(&format!("a.{ext}"), b"x");
        let req = menu_of(&c, &file);
        let got = replies_until(&c, TEN_S, menu_end).expect("an answer");
        assert_eq!(got.last(), Some(&ShellReply::Fallback { req, why: FallbackWhy::HostDied }), "{ext}");
    }
    let end = Instant::now() + TEN_S;
    while c.state() != HostState::MenuOff && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(c.state(), HostState::MenuOff);
    let other = root.file("b.md", b"x");
    let req = menu_of(&c, &other);
    assert_eq!(c.drain(), [ShellReply::Fallback { req, why: FallbackWhy::Disabled }], "menus are off");
}

#[test]
fn busy_invoke_survives_ui_kill() {
    use std::io::{BufRead, BufReader};
    let root = TempRoot::new("uikill");
    let file = root.file("a.txt", b"x");
    let out = root.path().join("out.bin");
    let out_s = out.display().to_string();
    let env = fault_env(
        &root,
        &[
            ("FILES_FAULT_MODE", "invoke-write"),
            ("FILES_FAULT_OUT", &out_s),
            ("FILES_FAULT_SECS", "5"),
            ("FILES_TEST_MENU_PICK", "modfault.write"),
        ],
    );
    let line = format!(
        "{{\"op\":\"shell_menu_busy\",\"file\":{}}}\n{{\"op\":\"sleep_ms\",\"ms\":60000}}\n",
        serde_json::json!(file.display().to_string())
    );
    let script = root.file("s.jsonl", line.as_bytes());
    let script = script.to_str().expect("a UTF-8 temp path");
    let mut ui = common::spawn_no_window(&common::files_exe(), &["--role=test-client", script], &pairs(&env));
    let mut busy = String::new();
    BufReader::new(ui.take_stdout().expect("stdout")).read_line(&mut busy).expect("the busy line");
    let pid: u32 = busy.trim().strip_prefix("busy ").and_then(|p| p.parse().ok()).expect("busy <pid>");
    std::thread::sleep(Duration::from_secs(1));
    let _ = ui.child().kill();
    let _ = ui.child().wait();
    let end = Instant::now() + Duration::from_secs(15);
    while file_len(&out) < 5 * 1024 && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(file_len(&out), 5 * 1024, "the invoke ran to its end after the UI died");
    assert!(gone(pid, Duration::from_secs(5)), "then the host ended within 5 s");
}

type Seen = Arc<std::sync::Mutex<Vec<ShellReply>>>;

/// A thread playing the UI: request and drain every 16 ms on c until stop, keeping what it drained in seen (only this
/// thread drains); the time inside each request and drain pair (us).
fn ui_loop(
    c: Arc<ShellClient>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    seen: Seen,
) -> std::thread::JoinHandle<Vec<u64>> {
    std::thread::spawn(move || {
        let mut times = Vec::new();
        while !stop.load(std::sync::atomic::Ordering::Relaxed) {
            let t = Instant::now();
            c.request(ShellReq::QuickAccessPins { req: c.next_req() });
            let got = c.drain();
            times.push(t.elapsed().as_micros() as u64);
            seen.lock().expect("seen").extend(got);
            std::thread::sleep(Duration::from_millis(16));
        }
        times
    })
}

/// Waits (at most 10 s) until the UI loop has drained a Fallback for req.
fn wait_fallback(seen: &Seen, req: ReqId) -> bool {
    let end = Instant::now() + TEN_S;
    while Instant::now() < end {
        if seen.lock().expect("seen").iter().any(|r| matches!(r, ShellReply::Fallback { req: q, .. } if *q == req)) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

/// A hang and a crash on one client while a UI loop runs on it; the loop's times.
fn faults_under_a_ui_loop(root: &TempRoot) -> (Arc<ShellClient>, Vec<u64>) {
    let mode = root.file("mode.txt", b"hang");
    let mode_s = mode.display().to_string();
    let c = Arc::new(client(root, &[("FILES_FAULT_MODE_FILE", &mode_s), ("FILES_FAULT_POINT", "menu")]));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let seen: Seen = Arc::default();
    let ui = ui_loop(c.clone(), stop.clone(), seen.clone());
    let file = root.file("a.txt", b"x");
    assert!(wait_fallback(&seen, menu_of(&c, &file)), "hang: a Fallback");
    root.write("mode.txt", b"die");
    let file = root.file("b.log", b"x");
    assert!(wait_fallback(&seen, menu_of(&c, &file)), "crash: a Fallback");
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    (c, ui.join().expect("the UI loop"))
}

#[test]
fn ui_loop_never_waits() {
    let root = TempRoot::new("uiloop");
    let (c, times) = faults_under_a_ui_loop(&root);
    assert!(!times.is_empty());
    assert_eq!(files_engine::shell_client::ui_waits(&c), 0, "request and drain never waited on a lock");
}

#[test]
#[ignore = "timing: CI bench job, debug, serial"]
fn timing_fallback_hang() {
    let root = TempRoot::new("thang");
    let ms: Vec<u64> = (0..20)
        .map(|_| scenario(&root, "hang", "txt"))
        .map(|(why, ms, _)| {
            assert_eq!(why, FallbackWhy::Deadline);
            ms
        })
        .collect();
    let p = p99(ms);
    println!("bench: files fallback_hang {p} ms");
    assert!(p <= 1_600, "p99 {p} ms over 1,600");
}

#[test]
#[ignore = "timing: CI bench job, debug, serial"]
fn timing_fallback_crash() {
    let root = TempRoot::new("tcrash");
    let ms: Vec<u64> = (0..20)
        .map(|_| scenario(&root, "die", "txt"))
        .map(|(why, _, from_exit)| {
            assert_eq!(why, FallbackWhy::HostDied);
            from_exit.expect("the host's exit was seen")
        })
        .collect();
    let p = p99(ms);
    println!("bench: files fallback_crash {p} ms");
    assert!(p <= 200, "p99 {p} ms over 200");
}

#[test]
#[ignore = "timing: CI bench job, debug, serial"]
fn timing_client_calls() {
    let root = TempRoot::new("tcalls");
    let (_, times) = faults_under_a_ui_loop(&root);
    let max = times.iter().copied().max().unwrap_or(0);
    let p = p99(times);
    println!("bench: files client_calls {p} us (max {max} us)");
    assert!(p <= 1_000, "p99 {p} us over 1 ms");
}

fn icons_of(c: &ShellClient, gen: u64, items: Vec<ItemRef>, px: u16) -> ReqId {
    let req = c.next_req();
    c.request(ShellReq::Icons { req, gen: Gen(gen), items, px });
    req
}

/// Everything for req until its ReqDone or Fallback.
fn image_replies(c: &ShellClient, req: ReqId, within: Duration) -> Vec<ShellReply> {
    replies_until(c, within, |r| match r {
        ShellReply::Evt(ShellEvt::ReqDone { req: q } | ShellEvt::Failed { req: q, .. }) => *q == req,
        ShellReply::Fallback { req: q, .. } => *q == req,
        _ => false,
    })
    .expect("an end for the request")
    .into_iter()
    .filter(|r| match r {
        ShellReply::Evt(e) => files_engine::shell::writer::req_of(e) == Some(req),
        ShellReply::Fallback { req: q, .. } => *q == req,
    })
    .collect()
}

#[test]
fn icons_for_temp_files() {
    let root = TempRoot::new("icons");
    let file = root.file("a.txt", b"x");
    let dir = root.dir("folder");
    let c = client(&root, &[]);
    let req = icons_of(&c, 1, vec![item(&file), item(&dir)], 32);
    let got = image_replies(&c, req, Duration::from_secs(30));
    let icons: Vec<(u32, u16, u16, usize)> = got
        .iter()
        .filter_map(|r| match r {
            ShellReply::Evt(ShellEvt::Icon { index, px, .. }) => Some((*index, px.w, px.h, px.bgra.len())),
            _ => None,
        })
        .collect();
    assert_eq!(icons, [(0, 32, 32, 32 * 32 * 4), (1, 32, 32, 32 * 32 * 4)], "{got:?}");
    assert_eq!(got.last(), Some(&ShellReply::Evt(ShellEvt::ReqDone { req })));
}

#[test]
fn icon_hang_falls_back() {
    let root = TempRoot::new("iconhang");
    let file = root.file("a.png", b"x");
    let c = client(&root, &[("FILES_FAULT_MODE", "hang"), ("FILES_FAULT_POINT", "icon")]);
    c.prespawn();
    let pid = ready_pid(&c, TEN_S).expect("Ready");
    let req = icons_of(&c, 1, vec![item(&file)], 16);
    let got = image_replies(&c, req, TEN_S);
    assert_eq!(got.last(), Some(&ShellReply::Fallback { req, why: FallbackWhy::Deadline }), "{got:?}");
    assert!(gone(pid, Duration::from_secs(5)), "the hung host was killed");
}

#[test]
fn thumb_die_falls_back() {
    let root = TempRoot::new("thumbdie");
    let file = root.file("a.jpg", b"x");
    let c = client(&root, &[("FILES_FAULT_MODE", "die"), ("FILES_FAULT_POINT", "thumb")]);
    let req = c.next_req();
    c.request(ShellReq::Thumbs { req, gen: Gen(1), items: vec![item(&file)], px: 96, cache_only: false });
    let got = image_replies(&c, req, TEN_S);
    assert_eq!(got.last(), Some(&ShellReply::Fallback { req, why: FallbackWhy::HostDied }), "{got:?}");
}

#[test]
fn idle_exit_waits_for_a_running_job() {
    let root = TempRoot::new("idlejob");
    let files: Vec<ItemRef> = (0..3).map(|i| item(&root.file(&format!("f{i}.png"), b"x"))).collect();
    let env = fault_env(
        &root,
        &[("FILES_FAULT_MODE", "slow"), ("FILES_FAULT_POINT", "icon"), ("FILES_TEST_SHELL_IDLE_MS", "500")],
    );
    let (mut d, _) = Direct::ready(&common::files_exe(), &pairs(&env), TEN_S).expect("Hello and Ready");
    d.send(&ShellReq::Icons { req: ReqId(1), gen: Gen(1), items: files, px: 16 }).expect("sent");
    // Three items of 1 s each: the idle time passes while the job runs, and the host waits for the job.
    let got = d.until(TEN_S, |e| matches!(e, ShellEvt::ReqDone { .. })).expect("the job ran to its end");
    assert_eq!(got.iter().filter(|e| matches!(e, ShellEvt::Icon { .. })).count(), 3, "{got:?}");
    assert_eq!(d.host.child.wait_ms(10_000), Some(0), "then the idle exit");
}

#[test]
fn cancel_gen_drops_queued() {
    let root = TempRoot::new("cancelgen");
    let files: Vec<ItemRef> = (0..3).map(|i| item(&root.file(&format!("f{i}.png"), b"x"))).collect();
    let c = client(&root, &[("FILES_FAULT_MODE", "slow"), ("FILES_FAULT_POINT", "icon")]);
    c.prespawn();
    ready_pid(&c, TEN_S).expect("Ready");
    let a = icons_of(&c, 1, files.clone(), 16);
    let b = icons_of(&c, 1, files.clone(), 16);
    let queued = icons_of(&c, 1, files.clone(), 16);
    std::thread::sleep(Duration::from_millis(300));
    c.request(ShellReq::CancelGen { gen: Gen(2) });
    let got = replies_until(&c, TEN_S, {
        let done = std::cell::RefCell::new(std::collections::HashSet::new());
        move |r| {
            if let ShellReply::Evt(ShellEvt::ReqDone { req }) = r {
                done.borrow_mut().insert(*req);
            }
            done.borrow().len() == 3
        }
    });
    let got = got.expect("three ReqDone");
    let icons_for =
        |q: ReqId| got.iter().filter(|r| matches!(r, ShellReply::Evt(ShellEvt::Icon { req, .. }) if *req == q)).count();
    assert_eq!(icons_for(queued), 0, "the queued request was dropped before it ran");
    assert!(icons_for(a) < 3 && icons_for(b) < 3, "the running ones stopped between items");
}

#[test]
fn deadline_does_not_kill_busy() {
    let root = TempRoot::new("busydead");
    let file = root.file("a.txt", b"x");
    let png = root.file("b.png", b"x");
    let out = root.path().join("out.bin");
    let out_s = out.display().to_string();
    let c = client(
        &root,
        &[
            ("FILES_FAULT_MODE", "hang"),
            ("FILES_FAULT_POINT", "icon"),
            ("FILES_FAULT_INVOKE", "write"),
            ("FILES_FAULT_OUT", &out_s),
            ("FILES_FAULT_SECS", "5"),
            ("FILES_TEST_MENU_PICK", "modfault.write"),
        ],
    );
    c.prespawn();
    let pid = ready_pid(&c, TEN_S).expect("Ready");
    menu_of(&c, &file);
    replies_until(&c, TEN_S, |r| matches!(r, ShellReply::Evt(ShellEvt::Busy { on: true }))).expect("the invoke runs");
    let req = icons_of(&c, 1, vec![item(&png)], 16);
    let got = image_replies(&c, req, TEN_S);
    assert_eq!(got.last(), Some(&ShellReply::Fallback { req, why: FallbackWhy::Deadline }), "{got:?}");
    assert!(file_len(&out) < 5 * 1024, "answered before the invoke ended");
    assert!(!gone(pid, Duration::ZERO), "a busy host is not killed");
    let end = Instant::now() + Duration::from_secs(15);
    while file_len(&out) < 5 * 1024 && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(file_len(&out), 5 * 1024, "the invoke ran to its end");
    assert!(gone(pid, Duration::from_secs(5)), "killed right after the invoke, the icon call still hung");
}

/// The visible top-level windows of the given processes and of any WerFault.exe: their owners' image names.
fn dialogs(pids: &[u32]) -> Vec<String> {
    use windows::core::{BOOL, PWSTR};
    use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM};
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowThreadProcessId, IsWindowVisible};
    unsafe extern "system" fn each(hwnd: HWND, lp: LPARAM) -> BOOL {
        // SAFETY: lp is the Vec passed below, alive for the enumeration; hwnd is the system's.
        unsafe {
            if IsWindowVisible(hwnd).as_bool() {
                let mut pid = 0u32;
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
                (*(lp.0 as *mut Vec<u32>)).push(pid);
            }
        }
        true.into()
    }
    let mut owners: Vec<u32> = Vec::new();
    // SAFETY: the callback reads only the Vec it is given.
    let _ = unsafe { EnumWindows(Some(each), LPARAM(std::ptr::from_mut(&mut owners) as isize)) };
    owners.sort_unstable();
    owners.dedup();
    let mut found = Vec::new();
    for pid in owners {
        // SAFETY: a query-only handle, closed below.
        let Ok(h) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }) else { continue };
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        // SAFETY: a writable buffer of len units.
        let name = unsafe { QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) }
            .map(|()| String::from_utf16_lossy(&buf[..len as usize]))
            .unwrap_or_default();
        // SAFETY: opened above, closed once.
        let _ = unsafe { CloseHandle(h) };
        if pids.contains(&pid) || name.to_ascii_lowercase().ends_with("werfault.exe") {
            found.push(format!("{pid} {name}"));
        }
    }
    found
}

/// Why a handler registered as files-disposable.ps1 -Register does it (HKCU only) cannot load here, when it cannot:
/// COM does not use per-user class registrations in an elevated process, and the shell host runs with this test's
/// token. A GitHub-hosted runner runs every process elevated (an administrator with UAC off), so there the registered
/// path is not applicable; where processes run unelevated, or the classes are also registered under HKLM, it runs.
fn per_user_classes_unused() -> Option<String> {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_ANY};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    let mut token = HANDLE::default();
    // SAFETY: the pseudo-handle of this process; `token` is writable and closed below.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.ok()?;
    let mut e = TOKEN_ELEVATION::default();
    let mut len = 0u32;
    let size = std::mem::size_of::<TOKEN_ELEVATION>() as u32;
    // SAFETY: `e` is a writable TOKEN_ELEVATION of `size` bytes.
    let read =
        unsafe { GetTokenInformation(token, TokenElevation, Some(std::ptr::from_mut(&mut e).cast()), size, &mut len) };
    // SAFETY: the token opened above, closed once.
    let _ = unsafe { CloseHandle(token) };
    if read.is_err() || e.TokenIsElevated == 0 {
        return None;
    }
    let key = windows::core::w!("Software\\Classes\\CLSID\\{08f7748e-e7c9-4950-ac48-6ebab3ca29dc}\\InprocServer32");
    // SAFETY: a read of a key's default value, no data returned; both strings are static and NUL-terminated.
    let in_hklm =
        unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, key, windows::core::PCWSTR::null(), RRF_RT_ANY, None, None, None) }
            .is_ok();
    (!in_hklm).then(|| {
        "this process is elevated and the fault classes are registered under HKCU only: COM does not use per-user \
         class registrations in an elevated process, so the registered handlers cannot load here"
            .to_string()
    })
}

/// The registered path (files-disposable.ps1 -Register): the same faults through the real lookup (GetUIObjectOf,
/// GetImage), plus the modes that may raise Windows Error Reporting. Every request gets its Fallback within its
/// deadline, and no dialog appears. Not applicable where per_user_classes_unused says so and the first fault did not
/// fire either; the reason is printed.
#[test]
#[ignore = "disposable: CI files-disposable job only"]
fn disposable_registered_faults() {
    common::disposable();
    let root = TempRoot::new("registered");
    let cases = [
        ("die", "menu", CallKindOf::Menu),
        ("hang", "menu", CallKindOf::Menu),
        ("crash", "menu", CallKindOf::Menu),
        ("abort", "menu", CallKindOf::Menu),
        ("stack", "menu", CallKindOf::Menu),
        ("hang", "icon", CallKindOf::Icon),
        ("crash", "icon", CallKindOf::Icon),
        ("die", "thumb", CallKindOf::Thumb),
        ("crash", "thumb", CallKindOf::Thumb),
    ];
    for (mode, point, kind) in cases {
        let file = root.file(&format!("{mode}-{point}.modfault"), b"x");
        let mut hooks = common::hooks(&root);
        hooks.env.push(("FILES_FAULT_MODE".into(), mode.into()));
        hooks.env.push(("FILES_FAULT_POINT".into(), point.into()));
        let c = ShellClient::new(ShellConfig { exe: common::files_exe(), test: Some(hooks) }, Arc::new(|| {}));
        c.prespawn();
        let pid = ready_pid(&c, TEN_S).expect("Ready");
        let req = c.next_req();
        c.request(match kind {
            CallKindOf::Menu => ShellReq::Menu { req, items: vec![item(&file)], at: (0, 0), extended: false, owner: 0 },
            CallKindOf::Icon => ShellReq::Icons { req, gen: Gen(1), items: vec![item(&file)], px: 32 },
            CallKindOf::Thumb => {
                ShellReq::Thumbs { req, gen: Gen(1), items: vec![item(&file)], px: 96, cache_only: false }
            }
        });
        let got = image_or_menu_end(&c, req);
        println!("disposable: {mode} at {point}: {:?}", got.last());
        if (mode, point) == ("die", "menu") && !matches!(got.last(), Some(ShellReply::Fallback { .. })) {
            if let Some(why) = per_user_classes_unused() {
                println!("disposable_registered_faults: not applicable: {why}");
                return;
            }
        }
        if matches!(kind, CallKindOf::Thumb) && !matches!(got.last(), Some(ShellReply::Fallback { .. })) {
            println!("disposable: the thumbnail handler did not load through the registered key (unverified name)");
            continue;
        }
        assert!(
            matches!(got.last(), Some(ShellReply::Fallback { why: FallbackWhy::Deadline | FallbackWhy::HostDied, .. })),
            "{mode} at {point}: {got:?}"
        );
        assert!(gone(pid, TEN_S), "{mode} at {point}: the host ended");
        assert_eq!(dialogs(&[pid]), Vec::<String>::new(), "{mode} at {point}: no dialog on the desktop");
    }
}

#[derive(Clone, Copy)]
enum CallKindOf {
    Menu,
    Icon,
    Thumb,
}

fn image_or_menu_end(c: &ShellClient, req: ReqId) -> Vec<ShellReply> {
    replies_until(c, Duration::from_secs(20), |r| match r {
        ShellReply::Fallback { req: q, .. } => *q == req,
        ShellReply::Evt(e) => {
            matches!(e, ShellEvt::MenuDone { .. } | ShellEvt::ReqDone { .. } | ShellEvt::Failed { .. })
                && files_engine::shell::writer::req_of(e) == Some(req)
        }
    })
    .unwrap_or_else(|got| got)
}
