//! The single instance (P3-files-app F1a.15-F1a.17): the elevated-start guard, the mutex, the message-only IPC
//! window and the identity. Every child is files.exe --role=test-ipc with its own FILES_TEST_TAG, its data folder and
//! out file under its TempRoot: it creates only a message-only window and exits with this process (FILES_TEST_PARENT),
//! or when its guard kills it. The tests take one lock, so they run one at a time inside this binary.

mod common;

use std::io::{BufRead, BufReader};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use common::temp::TempRoot;
use common::{files_exe, spawn_no_window, test_tag, ChildGuard};
use files_engine::app::{start_plan, StartEnv, Step};
use files_engine::cli::Cli;
use files_engine::fakes::Fakes;
use files_engine::hub::{Collect, Hub};
use files_engine::instance::{self, Elevation, ForwardError, InstanceNames, MutexState};
use files_engine::msg::Msg;
use files_engine::names;
use files_engine::window::{FilesScreen, LoopReq, Show, WinKind, WinRec, STARTUP_TAB};
use oled_trust::frame::files::{InstanceCmd, Location, WidePath, INSTANCE};
use oled_ui::harness::Harness;
use oled_ui::{SizeL, Tokens};
use serde_json::{json, Value};
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{HANDLE, HLOCAL, HWND, LPARAM, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, FindWindowExW, PeekMessageW, SendMessageTimeoutW, HWND_MESSAGE, MSG, PM_REMOVE, SMTO_ABORTIFHUNG,
    WM_COPYDATA,
};

static LOCK: Mutex<()> = Mutex::new(());

/// The tests that start children or create windows run one at a time.
fn serial() -> MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

fn wp(s: &str) -> WidePath {
    WidePath::parse(s).unwrap_or_else(|e| panic!("{s}: {e:?}"))
}

fn wide(p: &Path) -> WidePath {
    wp(&p.display().to_string())
}

fn s(p: &Path) -> String {
    p.display().to_string()
}

/// Up to \`n\` lines of the child's stdout within \`within\`.
fn read_lines(g: &mut ChildGuard, n: usize, within: Duration) -> Vec<String> {
    let out = g.take_stdout().expect("piped stdout");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(out).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let end = Instant::now() + within;
    let mut lines = Vec::new();
    while lines.len() < n {
        match rx.recv_timeout(end.saturating_duration_since(Instant::now())) {
            Ok(l) => lines.push(l),
            Err(_) => break,
        }
    }
    lines
}

/// A files.exe --role=test-ipc child: it holds its tag's mutex and IPC window and writes every command it takes to
/// its out file. Dropped: killed first, then its TempRoot is removed.
struct IpcChild {
    _guard: ChildGuard,
    pid: u32,
    out: PathBuf,
    _root: TempRoot,
}

impl IpcChild {
    fn start(exe: &Path, tag: &str) -> IpcChild {
        let root = TempRoot::new("instance_ipc");
        let (out, data) = (root.at("out.jsonl"), root.at("data"));
        let (parent, out_s, data_s) = (std::process::id().to_string(), s(&out), s(&data));
        let env: [(&str, &str); 4] = [
            (names::TEST_TAG_VAR, tag),
            ("FILES_TEST_OUT", &out_s),
            ("FILES_TEST_PARENT", &parent),
            ("FILES_DATA_DIR", &data_s),
        ];
        let mut guard = spawn_no_window(exe, &["--role=test-ipc"], &env);
        let lines = read_lines(&mut guard, 1, Duration::from_secs(60));
        let pid = lines.first().and_then(|l| l.strip_prefix("ready ")).and_then(|p| p.parse().ok());
        let status = if pid.is_none() { guard.wait_timeout(Duration::from_secs(1)) } else { None };
        let pid = pid.unwrap_or_else(|| panic!("test-ipc child not ready: {lines:?}, exit {status:?}"));
        assert_eq!(pid, guard.pid());
        IpcChild { _guard: guard, pid, out, _root: root }
    }

    /// The commands it took, one JSON value each, in order.
    fn commands(&self) -> Vec<Value> {
        let text = std::fs::read_to_string(&self.out).unwrap_or_default();
        text.lines().map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{l}: {e}"))).collect()
    }
}

/// What the test-ipc child writes for a command.
fn command_json(c: &InstanceCmd) -> Value {
    let at = |l: &Location| match l {
        Location::Fs(p) => json!({ "fs": p.to_string_lossy() }),
        Location::Home => json!("home"),
        Location::Search { .. } => json!("search"),
        Location::Options => json!("options"),
    };
    match c {
        InstanceCmd::NewWindow(l) => json!({ "new_window": at(l) }),
        InstanceCmd::OpenTab(l) => json!({ "open_tab": at(l) }),
        InstanceCmd::Select { dir, item } => {
            json!({ "select": { "dir": dir.to_string_lossy(), "item": item.to_string_lossy() } })
        }
        InstanceCmd::Probe { path } => json!({ "probe": path.to_string_lossy() }),
    }
}

fn frame(c: &InstanceCmd) -> Vec<u8> {
    let mut v = Vec::new();
    oled_trust::frame::encode(c, &mut v).expect("encodes");
    v
}

/// The message-only window of the class.
fn ipc_window(class: &str) -> HWND {
    let class = HSTRING::from(class);
    // SAFETY: a NUL-terminated class name; the message-only parent.
    unsafe { FindWindowExW(Some(HWND_MESSAGE), None, &class, PCWSTR::null()) }.expect("the IPC window")
}

/// One WM_COPYDATA: the window's answer (0 when the send itself failed).
fn send_copydata(hwnd: HWND, dw_data: usize, bytes: &[u8]) -> usize {
    let cds = COPYDATASTRUCT { dwData: dw_data, cbData: bytes.len() as u32, lpData: bytes.as_ptr().cast_mut().cast() };
    let mut result = 0usize;
    // SAFETY: \`cds\` and the bytes it points to live for the call; \`result\` is writable.
    let sent = unsafe {
        SendMessageTimeoutW(
            hwnd,
            WM_COPYDATA,
            WPARAM(0),
            LPARAM(std::ptr::from_ref(&cds) as isize),
            SMTO_ABORTIFHUNG,
            5_000,
            Some(&mut result),
        )
    };
    assert_ne!(sent.0, 0, "the window answered");
    result
}

// ---- F1a.15 ---------------------------------------------------------------------------------------------------------

#[test]
fn elevation_decision_table() {
    use Elevation::*;
    // TokenElevationTypeDefault (1): UAC off, a standard user, a hosted runner; Full (2): UAC-elevated; Limited (3):
    // an administrator's filtered token (P1-files-engine OP5c's table).
    for (raw, e, hand_off) in [(1, Default, false), (2, Full, true), (3, Limited, false), (0, Default, false)] {
        assert_eq!(instance::elevation_of(raw), e, "{raw}");
        assert_eq!(instance::must_hand_off(e), hand_off, "{e:?}");
    }
    assert_eq!(instance::elevation_of(7), Default, "an unknown value runs normally");
    // This process's token reads as one of the three (no test asserts on a real token: a shell may be elevated).
    let _ = instance::elevation();
}

#[test]
fn elevated_plan_hands_off() {
    use Step::*;
    let window = |forward_only| Cli::Window {
        target: Some(r"C:\mod-files-made-up\projects".into()),
        tab: false,
        select: None,
        from_verb: true,
        forward_only,
    };
    let preview = Cli::Preview { dir: r"C:\mod-files-made-up\out".into(), scene: None, lang: None, dpi: None };
    let selftest = Cli::Selftest { nonce: "0123456789abcdef0123456789abcdef".into() };
    let env = |elevation| StartEnv { elevation };
    for forward_only in [false, true] {
        assert_eq!(start_plan(&window(forward_only), &env(Elevation::Full)), [Log, HandOff], "nothing else starts");
    }
    for e in [Elevation::Default, Elevation::Limited, Elevation::Full] {
        assert_eq!(start_plan(&preview, &env(e)), [Preview], "{e:?}");
        assert_eq!(start_plan(&selftest, &env(e)), [Selftest], "{e:?}");
    }
    for e in [Elevation::Default, Elevation::Limited] {
        let plan = start_plan(&window(false), &env(e));
        assert!(!plan.contains(&HandOff), "{e:?}: {plan:?}");
        assert!(plan.contains(&Forward) && plan.contains(&OpenWindow), "{e:?}: {plan:?}");
    }
    // Only a window start hands off: another form starts nothing here.
    assert_eq!(instance::elevated_handoff(&preview), names::EXIT_ELEVATED_HANDOFF);
    assert_eq!(instance::elevated_handoff(&selftest), names::EXIT_ELEVATED_HANDOFF);
}

/// CommandLineToArgvW of a whole command line.
fn argv(line: &[u16]) -> Vec<String> {
    use windows::Win32::Foundation::LocalFree;
    use windows::Win32::UI::Shell::CommandLineToArgvW;
    let z: Vec<u16> = line.iter().copied().chain([0]).collect();
    let mut n = 0i32;
    // SAFETY: \`z\` is NUL-terminated; \`n\` is writable; the array is freed with LocalFree below.
    let list = unsafe { CommandLineToArgvW(PCWSTR(z.as_ptr()), &mut n) };
    assert!(!list.is_null());
    // SAFETY: CommandLineToArgvW returned \`n\` NUL-terminated strings.
    let args = (0..n as usize).map(|i| unsafe { (*list.add(i)).to_string() }.expect("UTF-16")).collect();
    // SAFETY: allocated by CommandLineToArgvW, not used afterwards.
    unsafe { LocalFree(Some(HLOCAL(list.cast()))) };
    args
}

/// File Explorer's command line for \`args\`.
fn explorer_line(args: &[u16]) -> Vec<u16> {
    let mut line: Vec<u16> = r#""C:\Windows\explorer.exe" "#.encode_utf16().collect();
    line.extend_from_slice(args);
    line
}

#[test]
fn handoff_command_line_quoting() {
    let exe = r"C:\Windows\explorer.exe".to_string();
    for path in [r"C:\mod-files-made-up\annual reports", r"C:\", r"\\server\share\team notes", r"D:\a\b c\d"] {
        let p = wp(path);
        for cmd in [InstanceCmd::NewWindow(Location::Fs(p.clone())), InstanceCmd::OpenTab(Location::Fs(p.clone()))] {
            let line = explorer_line(&instance::handoff_args(&cmd));
            // No closing backslash is added (the shell's parser ignores it); only a drive root keeps its one.
            let want = if path.len() > 3 { path.trim_end_matches('\\') } else { path };
            assert_eq!(argv(&line), [exe.clone(), want.to_string()], "{path}");
        }
    }
    // A file: its folder opens with it selected; it is never opened.
    let item = wp(r"C:\mod-files-made-up\annual reports\report-final-v2.pdf");
    let select = InstanceCmd::Select { dir: wp(r"C:\mod-files-made-up\annual reports"), item: item.clone() };
    let line = explorer_line(&instance::handoff_args(&select));
    assert_eq!(argv(&line), [exe.clone(), format!("/select,{}", item.to_string_lossy())]);
    // Home: File Explorer with no argument.
    assert!(instance::handoff_args(&InstanceCmd::NewWindow(Location::Home)).is_empty());
    // The quoting itself: a trailing backslash, a run of them before an inner quote, an empty argument.
    for raw in [r"C:\x\", r"a\\\b", r#"a"b"#, r#"a\"b"#, r"\\", ""] {
        let units: Vec<u16> = raw.encode_utf16().collect();
        let line = explorer_line(&instance::quote_arg(&units));
        assert_eq!(argv(&line), [exe.clone(), raw.to_string()], "{raw}");
    }
}

#[test]
fn handoff_checks_the_target_by_handle() {
    let root = TempRoot::new("handoff_check");
    let folder = root.dir("a folder");
    let file = root.file("tool.exe", b"MZ");
    let cmds = |p: &Path| [InstanceCmd::NewWindow(Location::Fs(wide(p))), InstanceCmd::OpenTab(Location::Fs(wide(p)))];
    // %TEMP% may be an 8.3 form or differ in case from the disk: the arguments name the folder's final path, so the
    // expectation is built from the long, on-disk form (canonicalize is the same normalized final path).
    let long = std::fs::canonicalize(&folder).unwrap();
    let long = PathBuf::from(long.to_string_lossy().trim_start_matches(r"\\?\"));
    for (cmd, want) in cmds(&folder).into_iter().zip(cmds(&long)) {
        let h = instance::checked_handoff(&cmd).expect("a folder is handed off");
        assert_eq!(h.args, instance::handoff_args(&want));
        assert!(h.pin.is_some(), "the folder stays pinned while File Explorer starts");
    }
    for cmd in cmds(&file).into_iter().chain(cmds(&root.at("missing"))) {
        assert!(instance::checked_handoff(&cmd).is_none(), "a file or nothing is not handed off");
    }
    let select = InstanceCmd::Select { dir: wide(root.path()), item: wide(&file) };
    let h = instance::checked_handoff(&select).expect("Select is handed off");
    assert_eq!((h.args, h.pin.is_none()), (instance::handoff_args(&select), true), "Select is unchanged");
}

#[test]
fn a_pinned_folder_cannot_be_renamed_or_deleted() {
    let root = TempRoot::new("handoff_pin");
    root.dir("parent");
    let folder = root.dir("parent/setup.exe");
    let pin = instance::FolderPin::open(&wide(&folder)).expect("a folder pins");
    let canon = std::fs::canonicalize(&folder).unwrap();
    let want: Vec<u16> = canon.to_string_lossy().trim_start_matches(r"\\?\").encode_utf16().collect();
    assert_eq!(String::from_utf16_lossy(pin.path()).to_lowercase(), String::from_utf16_lossy(&want).to_lowercase());
    assert!(root.try_rename("parent/setup.exe", "parent/moved").is_err(), "the folder itself");
    assert!(root.try_remove_dir("parent/setup.exe").is_err(), "deleting it");
    assert!(root.try_rename("parent", "renamed").is_err(), "its parent");
    drop(pin);
    root.try_rename("parent", "renamed").expect("free again once the pin is dropped");
}

#[test]
fn a_pin_refuses_a_file_and_follows_a_link_to_its_target() {
    let root = TempRoot::new("handoff_pin_link");
    let file = root.file("tool.exe", b"MZ");
    let target = root.dir("real");
    root.junction("link", &target);
    assert!(instance::FolderPin::open(&wide(&file)).is_none(), "a file");
    assert!(instance::FolderPin::open(&wide(&root.at("missing"))).is_none(), "nothing");
    let pin = instance::FolderPin::open(&wide(&root.at("link"))).expect("a link to a folder");
    let end = String::from_utf16_lossy(pin.path()).to_lowercase();
    assert!(end.ends_with("\\real"), "the pinned folder is the link's target: {end}");
    assert!(root.try_rename("real", "elsewhere").is_err(), "the target is held, not the link");
}

#[test]
fn a_handoff_is_refused_when_the_shell_would_read_another_object() {
    let root = TempRoot::new("handoff_trim");
    let cmd = |p: &Path| InstanceCmd::NewWindow(Location::Fs(wide(p)));
    // A folder 'x.' next to a file 'x': the shell trims the dot and reads the file.
    let dotted = root.dir_exact("x.");
    root.file_wide(&"x".encode_utf16().collect::<Vec<_>>(), b"MZ");
    root.junction("link", &dotted);
    assert!(instance::checked_handoff(&cmd(&root.at("link"))).is_none(), "a link to a folder named 'x.'");
    // The same with a trailing space.
    let spaced = root.dir_exact("y ");
    root.file_wide(&"y".encode_utf16().collect::<Vec<_>>(), b"MZ");
    root.junction("link2", &spaced);
    assert!(instance::checked_handoff(&cmd(&root.at("link2"))).is_none(), "a link to a folder named 'y '");
    // An intermediate component: 'a.' holds 'inner', and so does the other folder 'a'.
    root.dir_exact("a./inner");
    root.dir_exact("a/inner");
    root.junction("link3", &root.at("a./inner"));
    assert!(instance::checked_handoff(&cmd(&root.at("link3"))).is_none(), "a link below a folder named 'a.'");
    // A plain folder is still handed off.
    let plain = root.dir("plain");
    root.junction("link4", &plain);
    assert!(instance::checked_handoff(&cmd(&root.at("link4"))).is_some(), "a link to a plain folder");
}

#[test]
fn the_wait_for_the_launcher_has_no_bound() {
    let ping = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_default().join(r"System32\ping.exe");
    let mut c = spawn_no_window(&ping, &["-n", "3", "127.0.0.1"], &[]);
    let h = HANDLE(c.child().as_raw_handle());
    let mut slices = 0;
    instance::wait_for_exit(h, 50, || slices += 1);
    assert!(slices >= 3, "the wait went on past its slice {slices} times");
    assert!(matches!(c.child().try_wait(), Ok(Some(_))), "it returned only once the process had ended");
}

// ---- F1a.16 ---------------------------------------------------------------------------------------------------------

#[test]
fn forward_reaches_the_running_instance() {
    let _l = serial();
    let tag = test_tag();
    let child = IpcChild::start(&files_exe(), &tag);
    let root = TempRoot::new("instance_forward");
    let dir = wide(&root.dir("annual reports"));
    let item = wide(&root.file("annual reports/report-final-v2.pdf", b"x"));
    let names = InstanceNames::tagged(&tag);
    let cmds = [
        InstanceCmd::NewWindow(Location::Fs(dir.clone())),
        InstanceCmd::NewWindow(Location::Home),
        InstanceCmd::OpenTab(Location::Fs(dir.clone())),
        InstanceCmd::Select { dir: dir.clone(), item },
        InstanceCmd::Probe { path: dir.clone() },
    ];
    let exe = files_exe();
    for c in &cmds {
        let ms = instance::try_forward_as(&names, c, &exe);
        assert!(ms.is_ok(), "{c:?}: {ms:?}");
    }
    assert_eq!(child.commands(), cmds.iter().map(command_json).collect::<Vec<_>>());
    // Another tag is another instance: nothing runs there.
    let other = InstanceNames::tagged(&test_tag());
    assert!(matches!(instance::try_forward_as(&other, &cmds[0], &exe), Err(ForwardError::NoInstance)));
    // A command the window refuses is answered 0.
    let search = InstanceCmd::NewWindow(Location::Search { root: dir, query: "x".into() });
    assert!(matches!(instance::try_forward_as(&names, &search, &exe), Err(ForwardError::Refused)));
    // This test's own image is the test binary: try_forward (this exe) sends nothing to a files.exe.
    assert!(matches!(instance::try_forward(&names, &cmds[0]), Err(ForwardError::NoInstance)));
    assert_eq!(child.commands().len(), cmds.len());
    assert!(child.pid != std::process::id());
}

#[test]
fn forward_refuses_another_image() {
    let _l = serial();
    let bin = TempRoot::new("instance_copy");
    let exe = std::fs::read(files_exe()).expect("files.exe");
    let copy = bin.file("bin/files.exe", &exe);
    let tag = test_tag();
    let child = IpcChild::start(&copy, &tag);
    let cmd = InstanceCmd::NewWindow(Location::Home);
    let names = InstanceNames::tagged(&tag);
    assert!(matches!(instance::try_forward_as(&names, &cmd, &files_exe()), Err(ForwardError::NoInstance)));
    assert!(child.commands().is_empty(), "a copy of the exe is another image: it is sent nothing");
    // The copy's own image passes: the check is the image, not the class.
    assert!(instance::try_forward_as(&names, &cmd, &copy).is_ok());
    assert_eq!(child.commands(), [command_json(&cmd)]);
    drop(child);
}

#[test]
fn garbage_is_refused() {
    let _l = serial();
    let tag = test_tag();
    let child = IpcChild::start(&files_exe(), &tag);
    let hwnd = ipc_window(&InstanceNames::tagged(&tag).ipc_class);
    let magic = INSTANCE.magic as usize;
    let dir = wp(r"C:\mod-files-made-up\aQb");
    let good = frame(&InstanceCmd::NewWindow(Location::Fs(dir.clone())));
    // The same frame with a control character in its path ('Q' is the one unit 0x51 in it).
    let mut control = good.clone();
    let at: Vec<usize> = control.windows(2).enumerate().filter(|(_, w)| *w == [0x51, 0]).map(|(i, _)| i).collect();
    assert_eq!(at.len(), 1);
    control[at[0]] = 0x07;
    let search = frame(&InstanceCmd::OpenTab(Location::Search { root: dir.clone(), query: "x".into() }));
    let trailing = [good.as_slice(), &[0u8]].concat();
    let big = vec![0u8; 70_000];
    let cases: [(&str, usize, &[u8]); 7] = [
        ("another dwData", magic ^ 1, good.as_slice()),
        ("a truncated frame", magic, &good[..good.len() - 1]),
        ("a 70,000-byte frame", magic, big.as_slice()),
        ("a path with a control character", magic, control.as_slice()),
        ("bytes after the frame", magic, trailing.as_slice()),
        ("a Search location", magic, search.as_slice()),
        ("no bytes", magic, &[]),
    ];
    for (what, dw, bytes) in cases {
        assert_eq!(send_copydata(hwnd, dw, bytes), 0, "{what}");
    }
    assert!(child.commands().is_empty(), "nothing was posted");
    let t = Instant::now();
    assert_eq!(send_copydata(hwnd, magic, &good), 1, "the valid frame is taken");
    let took = t.elapsed();
    assert_eq!(child.commands(), [command_json(&InstanceCmd::NewWindow(Location::Fs(dir)))]);
    assert!(took < Duration::from_secs(2), "answered in {took:?}");
}

#[test]
fn mutex_race_has_one_winner() {
    let _l = serial();
    let tag = test_tag();
    let root = TempRoot::new("instance_race");
    let parent = std::process::id().to_string();
    let data = s(&root.at("data"));
    let env: [(&str, &str); 3] =
        [(names::TEST_TAG_VAR, &tag), ("FILES_TEST_PARENT", &parent), ("FILES_DATA_DIR", &data)];
    let race = || spawn_no_window(&files_exe(), &["--role=test-ipc", "--mutex-only"], &env);
    let (mut a, mut b) = (race(), race());
    let lines =
        [read_lines(&mut a, 2, Duration::from_secs(60)), read_lines(&mut b, 2, Duration::from_secs(60))].concat();
    assert_eq!(lines.len(), 4, "{lines:?}");
    assert_eq!(lines.iter().filter(|l| *l == "owned").count(), 1, "{lines:?}");
    assert_eq!(lines.iter().filter(|l| *l == "busy").count(), 3, "{lines:?}");
}

#[test]
fn mutex_is_single_while_held() {
    let _l = serial();
    let names = InstanceNames::tagged(&test_tag());
    let first = instance::take_mutex(&names);
    assert!(matches!(first, MutexState::Owned(_)));
    assert!(matches!(instance::take_mutex(&names), MutexState::Busy), "held: a second start is busy");
    instance::release(first, None);
    assert!(matches!(instance::take_mutex(&names), MutexState::Owned(_)), "released: free again");
}

#[test]
fn test_ipc_refuses_the_real_names() {
    let _l = serial();
    let root = TempRoot::new("instance_untagged");
    let parent = std::process::id().to_string();
    let out = s(&root.at("out.jsonl"));
    // No FILES_TEST_TAG: the child would take the real names, so it refuses (exit 2) and creates nothing.
    let env: [(&str, &str); 3] = [("FILES_TEST_OUT", &out), ("FILES_TEST_PARENT", &parent), (names::TEST_TAG_VAR, "")];
    let mut c = spawn_no_window(&files_exe(), &["--role=test-ipc"], &env);
    let status = c.wait_timeout(Duration::from_secs(60)).expect("exits");
    assert_eq!(status.code(), Some(names::EXIT_USAGE));
}

/// The pid's top-level windows (EnumWindows).
fn top_level_windows_of(pid: u32) -> usize {
    use windows::core::BOOL;
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowThreadProcessId};
    unsafe extern "system" fn each(hwnd: HWND, lp: LPARAM) -> BOOL {
        // SAFETY: lp is the (pid, count) pair below, alive for the enumeration.
        let found = unsafe { &mut *(lp.0 as *mut (u32, usize)) };
        let mut pid = 0u32;
        // SAFETY: \`pid\` is writable.
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid == found.0 {
            found.1 += 1;
        }
        BOOL(1)
    }
    let mut found = (pid, 0usize);
    // SAFETY: the callback reads only \`found\`, which outlives the call.
    let _ = unsafe { EnumWindows(Some(each), LPARAM(std::ptr::from_mut(&mut found) as isize)) };
    found.1
}

#[test]
#[ignore = "after FA-WIN: app::run executes start_plan (F1a.10); until then a window start exits 2"]
fn forward_only_child_forwards_or_exits() {
    let _l = serial();
    let tag = test_tag();
    let child = IpcChild::start(&files_exe(), &tag);
    let root = TempRoot::new("instance_forward_only");
    let target = root.dir("annual reports");
    let (data, config) = (s(&root.at("data")), s(&root.at("config")));
    let start = |tag: &str| {
        let env: [(&str, &str); 3] =
            [(names::TEST_TAG_VAR, tag), ("FILES_DATA_DIR", &data), ("FILES_CONFIG_DIR", &config)];
        spawn_no_window(&files_exe(), &["--forward-only", &s(&target)], &env)
    };
    let mut fwd = start(&tag);
    let status = fwd.wait_timeout(Duration::from_secs(60)).expect("exits");
    assert_eq!(status.code(), Some(names::EXIT_OK));
    assert_eq!(child.commands(), [command_json(&InstanceCmd::NewWindow(Location::Fs(wide(&target))))]);
    // With no instance of its tag it exits 7 and never shows a window.
    let mut lone = start(&test_tag());
    let pid = lone.pid();
    let end = Instant::now() + Duration::from_secs(60);
    let status = loop {
        assert_eq!(top_level_windows_of(pid), 0, "a --forward-only start opened a window");
        if let Some(s) = lone.wait_timeout(Duration::from_millis(20)) {
            break s;
        }
        assert!(Instant::now() < end, "still running");
    };
    assert_eq!(status.code(), Some(names::EXIT_NOT_FORWARDED));
}

#[test]
fn apply_routes_through_loop_requests() {
    let f = Fakes::new();
    let shared = f.shared(oled_ui::UiLang::EN);
    let (a, b, c) = (wp(r"C:\mod-files-made-up\a"), wp(r"C:\mod-files-made-up\b"), wp(r"C:\mod-files-made-up\c"));
    for (win, at) in [(1, &a), (2, &b)] {
        let rec = WinRec { win, kind: WinKind::Main, wref: None, hwnd: None, location: Some(Location::Fs(at.clone())) };
        shared.windows.borrow_mut().push(rec);
    }
    let two = f.attach_window(2);
    let screen = FilesScreen::new(std::rc::Rc::clone(&shared), 1, Location::Fs(a.clone()), None, STARTUP_TAB);
    let mut h = Harness::new(screen, SizeL { w: 800, h: 500 }, 96, Tokens::oled());
    let name = |dir: &WidePath| dir.join(&"report-final-v2.pdf".encode_utf16().collect::<Vec<u16>>()).expect("a name");
    let mut send = |cmd: InstanceCmd| {
        h.send(Box::new(Msg::Instance(cmd)));
        std::mem::take(&mut *shared.loop_requests.borrow_mut())
    };
    // Select for window 2's folder goes to window 2 (which then selects in place), nothing opens.
    assert!(send(InstanceCmd::Select { dir: b.clone(), item: name(&b) }).is_empty());
    let got = two.take();
    assert!(
        matches!(got.as_slice(), [Msg::Instance(InstanceCmd::Select { dir, item })] if *dir == b && *item == name(&b)),
        "re-posted to window 2"
    );
    let main = |first: Location, select: Option<WidePath>| LoopReq::Main { first, select, cascade_from: Some(1) };
    assert_eq!(send(InstanceCmd::NewWindow(Location::Fs(c.clone()))), [main(Location::Fs(c.clone()), None)]);
    assert_eq!(send(InstanceCmd::NewWindow(Location::Home)), [main(Location::Home, None)]);
    let select_c = InstanceCmd::Select { dir: c.clone(), item: name(&c) };
    assert_eq!(send(select_c), [main(Location::Fs(c.clone()), Some(name(&c)))], "no window on c: a new one");
    // OpenTab before the tab strip (tab::open_here is false): a new window.
    assert_eq!(send(InstanceCmd::OpenTab(Location::Fs(c.clone()))), [main(Location::Fs(c.clone()), None)]);
    assert!(send(InstanceCmd::Probe { path: c.clone() }).is_empty(), "Probe opens nothing");
    assert!(send(InstanceCmd::NewWindow(Location::Options)).is_empty());
    // Select in this window's own folder: it comes to the front, through the loop.
    assert_eq!(
        send(InstanceCmd::Select { dir: a.clone(), item: name(&a) }),
        [LoopReq::Show { win: 1, how: Show::Front }]
    );
    assert!(two.is_empty());
}

#[test]
fn updated_notice_reaches_the_windows() {
    let _l = serial();
    let names = InstanceNames::tagged(&test_tag());
    let hub = Hub::new();
    let c = Arc::new(Collect::new());
    hub.attach(1, Box::new(Arc::clone(&c)));
    let before = top_level_windows_of(std::process::id());
    let ipc = instance::create_ipc(&names, Arc::clone(&hub)).expect("the IPC window");
    // The window is message-only: found under HWND_MESSAGE, and no top-level window was added.
    let _ = ipc_window(&names.ipc_class);
    assert_eq!(top_level_windows_of(std::process::id()), before);
    let exe = std::env::current_exe().expect("the test's image");
    assert_eq!(oled_sys::selftest::notify_updated(&names.ipc_class, true, &exe, (1, 8, 0)), 1);
    let end = Instant::now() + Duration::from_secs(10);
    while c.is_empty() && Instant::now() < end {
        let mut msg = MSG::default();
        // SAFETY: \`msg\` is writable; this thread's messages only.
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            // SAFETY: a message this thread just received.
            unsafe { DispatchMessageW(&msg) };
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let got = c.take();
    assert!(matches!(got.as_slice(), [Msg::Updated { version: 0x01_08_00 }]), "{}", got.len());
    instance::release(MutexState::Busy, Some(ipc));
    let cmd = InstanceCmd::NewWindow(Location::Home);
    assert!(matches!(instance::try_forward(&names, &cmd), Err(ForwardError::NoInstance)), "gone with its release");
}

#[test]
#[ignore = "bench: CI bench job, release, serial"]
fn bench_instance_forward() {
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::WindowsAndMessaging::{GetMessageW, PostThreadMessageW, WM_QUIT};
    let _l = serial();
    let names = InstanceNames::tagged(&test_tag());
    // Release builds have no test-ipc role: the instance is a thread of this process with its own message loop.
    let (tx, rx) = mpsc::channel();
    let served = names.clone();
    let server = std::thread::spawn(move || {
        let hub = Hub::new();
        let c = Arc::new(Collect::new());
        hub.attach(1, Box::new(Arc::clone(&c)));
        let ipc = instance::create_ipc(&served, hub).expect("the IPC window");
        // SAFETY: no arguments.
        tx.send(unsafe { GetCurrentThreadId() }).expect("the bench waits");
        let mut msg = MSG::default();
        // SAFETY: \`msg\` is writable; this thread's messages until WM_QUIT.
        while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {
            // SAFETY: a message this thread just received.
            unsafe { DispatchMessageW(&msg) };
        }
        drop(ipc);
        c.len()
    });
    let thread = rx.recv_timeout(Duration::from_secs(10)).expect("the instance thread is up");
    let cmd = InstanceCmd::NewWindow(Location::Fs(wp(r"C:\mod-files-made-up\annual reports")));
    let mut ms: Vec<f64> = Vec::new();
    for _ in 0..50 {
        let t = Instant::now();
        instance::try_forward(&names, &cmd).expect("forwarded");
        ms.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    // SAFETY: posts WM_QUIT to the instance thread's queue.
    unsafe { PostThreadMessageW(thread, WM_QUIT, WPARAM(0), LPARAM(0)) }.expect("posted");
    assert_eq!(server.join().expect("the instance thread"), 50, "every forward reached the hub");
    ms.sort_by(f64::total_cmp);
    let p99 = ms[(ms.len() * 99).div_ceil(100) - 1];
    println!("bench: files instance_forward {p99:.3} ms");
    assert!(p99 <= 5.0, "p99 of the forward {p99:.3} ms over 5 ms");
}

// ---- F1a.17 ---------------------------------------------------------------------------------------------------------

#[test]
fn aumid_constant_matches_p2_plumbing() {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::GetCurrentProcessExplicitAppUserModelID;
    // The id P2-plumbing's I1 writes on the shortcut.
    assert_eq!(names::AUMID, "MiniOledDock.Files");
    instance::set_identity();
    // SAFETY: returns a CoTaskMem string, freed below.
    let id = unsafe { GetCurrentProcessExplicitAppUserModelID() }.expect("an id is set");
    // SAFETY: a NUL-terminated string returned above.
    let text = unsafe { id.to_string() }.expect("UTF-16");
    // SAFETY: allocated by the call above, not used afterwards.
    unsafe { CoTaskMemFree(Some(id.0 as *const std::ffi::c_void)) };
    assert_eq!(text, names::AUMID);
}
