//! T3.4 and T3.5: the resolver, the name book, names.bin and the dock's renames. Pure tests over fakes; the Start Menu
//! tests write shortcut files in a temp folder only. No window, no process, no desktop input.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use oled_shellmodel::names::{in_windows_dir_of, NameQuery, NameSources};
use oled_shellmodel::startmenu::{IndexedSources, StartMenuIndex};
use oled_shellmodel::version::FileInfo;
use oled_ui::gfx::image::IconBitmap;
use tasks::app::dock_names;
use tasks::app::names_bin::{self, Bad, Entry, NamesBin};
use tasks::app::resolver::{
    list_apps_lines, name_with, on_done, spawn_with, Answer, Ask, Backend, IconStore, NameBook, Queue, Request,
    ResolverDone, ShellBackend, BATCH, MAX_ICONS, MAX_ICON_BYTES, MAX_PENDING,
};
use tasks::model::group::{ExeId, IdentityId};
use windows::core::{Interface, HSTRING};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};

// ------------------------------------------------------------------ fakes and helpers

/// Made-up sources: what each program would have in the catalogue, the Start Menu and its version resource.
#[derive(Default)]
struct FakeSources {
    appsfolder: HashMap<&'static str, &'static str>,
    shortcuts: HashMap<&'static str, &'static str>,
    info: HashMap<&'static str, FileInfo>,
}

impl NameSources for FakeSources {
    fn appsfolder_name(&self, aumid: &str) -> Option<String> {
        self.appsfolder.get(aumid).map(|s| s.to_string())
    }
    fn start_menu_name(&self, exe_path: &str) -> Option<String> {
        self.shortcuts.get(exe_path).map(|s| s.to_string())
    }
    fn file_info(&self, exe_path: &str) -> FileInfo {
        self.info.get(exe_path).cloned().unwrap_or_default()
    }
    fn in_windows_dir(&self, exe_path: &str) -> bool {
        in_windows_dir_of(exe_path, r"C:\Windows")
    }
}

fn info(description: Option<&str>, product: Option<&str>, company: Option<&str>) -> FileInfo {
    let own = |s: Option<&str>| s.map(str::to_string);
    FileInfo { description: own(description), product: own(product), company: own(company) }
}

const NOTES_ID: &str = "Contoso.Notes_8wekyb3d8bbwe!App";
const DRAW: &str = r"C:\Program Files\Contoso\ctsdraw.exe";
const CHAT: &str = r"C:\Program Files\Example Chat\chat.exe";
const PLAYER: &str = r"C:\Program Files\Fabrikam\fabrikamplayer.exe";
const STREAM: &str = r"C:\Program Files\Stream\stremio-shell-ng.exe";
const TOYS: &str = r"C:\Program Files\PowerToys\PowerToys.QuickAccess.exe";
const EXPLORER: &str = r"C:\Windows\explorer.exe";
const ZEN: &str = r"C:\Tools\zen.exe";

/// The ten programs of the shared model's fixture (DK-SM1's names_of_ten_fixture_apps) with their made-up sources.
fn ten_sources() -> FakeSources {
    let mut src = FakeSources::default();
    src.appsfolder.insert(NOTES_ID, "Contoso Notes");
    src.shortcuts.insert(DRAW, "Contoso Draw");
    src.info.insert(CHAT, info(Some("Example Chat Client"), Some("Example Chat"), Some("Example Inc.")));
    src.info.insert(PLAYER, info(Some("Freedom to Stream"), Some("Fabrikam Player"), None));
    src.info.insert(STREAM, info(Some("Freedom to Stream"), None, None));
    src.info.insert(TOYS, info(Some("PowerToys.QuickAccess"), Some("PowerToys"), None));
    src.info.insert(EXPLORER, info(Some("Esplora risorse"), Some("Sistema operativo Microsoft Windows"), None));
    src
}

/// (key, exe, aumid, name, step) of the ten programs: the table of the shared model's test, unchanged.
fn ten_table() -> Vec<(&'static str, &'static str, Option<&'static str>, &'static str, &'static str)> {
    vec![
        (NOTES_ID, "", Some(NOTES_ID), "Contoso Notes", "aumid"),
        (DRAW, DRAW, None, "Contoso Draw", "shortcut"),
        (CHAT, CHAT, None, "Example Chat", "description"),
        (PLAYER, PLAYER, None, "Fabrikam Player", "description"),
        (STREAM, STREAM, None, "Stremio", "stem"),
        (TOYS, TOYS, None, "PowerToys", "description"),
        (EXPLORER, EXPLORER, None, "Esplora risorse", "description"),
        (ZEN, ZEN, None, "Zen", "stem"),
        ("Fabrikam.Calc_abc123!App", "", Some("Fabrikam.Calc_abc123!App"), "Calc", "aumid-id"),
        ("hwnd:42", "", None, "hwnd:42", "key"),
    ]
}

/// %TEMP% with its 8.3 components expanded (C:\Users\RUNNER~1 on the CI runner): the shell reports a link's target by
/// its long path, so the targets the test asks about are long too. %TEMP% itself when it cannot be read.
fn temp_long() -> PathBuf {
    let base = std::env::temp_dir();
    let long = std::fs::canonicalize(&base).ok().and_then(|p| {
        let s = p.to_str()?.to_string();
        s.strip_prefix(r"\\?\").filter(|rest| !rest.starts_with("UNC\\")).map(PathBuf::from)
    });
    long.unwrap_or(base)
}

/// A scratch folder of this test, removed at the end of it.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = temp_long().join(format!("tasks_names_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("test folder");
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// COM STA on the test thread for the length of a test.
struct Com(bool);

impl Com {
    fn new() -> Com {
        // SAFETY: COM STA for this test thread, paired with CoUninitialize in Drop.
        Com(unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok())
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: paired with the successful CoInitializeEx in new.
            unsafe { CoUninitialize() };
        }
    }
}

/// A file that stands for a program: it is never run.
fn fake_exe(dir: &Path, name: &str) -> String {
    let p = dir.join(name);
    std::fs::write(&p, b"MZ").expect("fake exe");
    p.to_string_lossy().into_owned()
}

/// Writes the shortcut `lnk` to `target` through IShellLinkW::SetPath and IPersistFile::Save (with `args` when given).
fn write_link(lnk: &Path, target: &str, args: Option<&str>) {
    // SAFETY: COM STA initialized by the caller; ShellLink CLSID with the requested interface.
    let link: IShellLinkW = unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER) }.expect("ShellLink");
    let file = link.cast::<IPersistFile>().expect("IPersistFile");
    // SAFETY: the strings are NUL-terminated and live for the calls.
    unsafe {
        link.SetPath(&HSTRING::from(target)).expect("SetPath");
        if let Some(a) = args {
            link.SetArguments(&HSTRING::from(a)).expect("SetArguments");
        }
        file.Save(&HSTRING::from(lnk.as_os_str()), true).expect("Save");
    }
}

fn rng(seed: u64) -> impl FnMut() -> u64 {
    let mut x = seed;
    move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    }
}

// ------------------------------------------------------------------ T3.4 names

#[test]
fn names_follow_shellmodel() {
    let src = ten_sources();
    for (i, (key, exe, aumid, want, step)) in ten_table().into_iter().enumerate() {
        let got = name_with(&src, &BTreeMap::new(), &NameQuery { key, exe_path: exe, aumid });
        assert_eq!((got.0.as_str(), got.1), (want, step), "app {i}");
    }
}

#[test]
fn names_with_a_real_start_menu() {
    let _com = Com::new();
    let s = Scratch::new("menu");
    let (all, user, apps) = (s.0.join("all"), s.0.join("user"), s.0.join("apps"));
    for d in [&all, &user, &apps] {
        std::fs::create_dir_all(d).expect("folder");
    }
    let (a, b, c) = (fake_exe(&apps, "orbit.exe"), fake_exe(&apps, "quill.exe"), fake_exe(&apps, "plain.exe"));
    write_link(&all.join("Orbit Suite (x64).lnk"), &a, None);
    write_link(&user.join("Quill Writer.lnk"), &b, None);
    write_link(&user.join("Plain Args.lnk"), &c, Some("--x"));
    let index = StartMenuIndex::build_from(&[(all, false), (user, true)]);
    let src = IndexedSources { index: &index, system_root: r"C:\Windows" };
    let ask = |exe: &str| {
        let key = oled_shellmodel::identity::key_from_exe_path(exe);
        name_with(&src, &BTreeMap::new(), &NameQuery { key: &key, exe_path: exe, aumid: None })
    };
    assert_eq!(ask(&a), ("Orbit Suite".to_string(), "shortcut"));
    assert_eq!(ask(&b), ("Quill Writer".to_string(), "shortcut"));
    assert_ne!(ask(&c).1, "shortcut", "a link with arguments names nothing");
}

#[test]
fn override_wins() {
    let src = ten_sources();
    let renames: BTreeMap<String, String> = [("ctsdraw.exe", "My Draw"), (NOTES_ID, "My Notes")]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let q = |key, exe_path, aumid| NameQuery { key, exe_path, aumid };
    assert_eq!(name_with(&src, &renames, &q(DRAW, DRAW, None)), ("My Draw".to_string(), "config"));
    assert_eq!(name_with(&src, &renames, &q(NOTES_ID, "", Some(NOTES_ID))), ("My Notes".to_string(), "config"));
    assert_eq!(name_with(&src, &renames, &q(CHAT, CHAT, None)).1, "description", "no rename: the natural name");
    // The backend asks the renames before names.bin and before the sources.
    let mut backend =
        ShellBackend::with_parts(StartMenuIndex::default(), renames, NamesBin::default(), None, String::new());
    assert_eq!(backend.name(DRAW, DRAW, None).as_deref(), Some("My Draw"));
}

// ------------------------------------------------------------------ T3.4 the queue and the store

fn ask(n: u32) -> Request {
    Request::Publisher { exe_id: ExeId(n), exe: format!("c:\\x\\{n}.exe") }
}

#[test]
fn queue_caps_and_dedups() {
    let mut q = Queue::default();
    assert!(q.push(ask(1), false));
    assert!(!q.push(ask(1), false), "a duplicate is dropped");
    assert!(!q.push(ask(1), true), "also at the front");
    for n in 2..=MAX_PENDING as u32 {
        assert!(q.push(ask(n), false));
    }
    assert_eq!(q.len(), MAX_PENDING);
    assert!(!q.push(ask(9999), false), "at most 512 pending");
    assert_eq!(q.pop(), Some(ask(1)));
    assert!(q.push(ask(9999), true), "room again");
    assert_eq!(q.pop(), Some(ask(9999)), "visible rows ask at the front");
    while q.pop().is_some() {}
    assert!(q.is_empty());
}

fn icon(side: u32) -> IconBitmap {
    IconBitmap { width: side, height: side, pixels: vec![0xFF10_2030; (side * side) as usize] }
}

#[test]
fn icon_store_evicts_lru_within_budget() {
    // Count: 400 at most (1 px icons).
    let mut s = IconStore::default();
    for n in 0..(MAX_ICONS as u32 + 50) {
        assert!(s.insert(IdentityId(n), icon(1)));
    }
    assert_eq!(s.len(), MAX_ICONS);
    assert!(s.get(IdentityId(0)).is_none() && s.get(IdentityId(49)).is_none(), "the oldest went first");
    assert!(s.get(IdentityId(50)).is_some());
    // Bytes: 32 x 32 x 4 = 4,096 each, at most 0.8 MiB.
    let mut s = IconStore::default();
    for n in 0..300u32 {
        assert!(s.insert(IdentityId(n), icon(32)));
        assert!(s.bytes() <= MAX_ICON_BYTES);
        // Row 0 is drawn after every insert: it is never the least recently drawn.
        assert!(s.get(IdentityId(0)).is_some());
    }
    assert_eq!(s.len(), MAX_ICON_BYTES / 4096);
    assert!(s.get(IdentityId(0)).is_some(), "a drawn icon stays");
    assert!(s.get(IdentityId(1)).is_none(), "an undrawn one went");
    // Replacing counts the new size, and a bitmap bigger than the budget is refused.
    assert!(s.insert(IdentityId(0), icon(16)));
    assert!(!s.insert(IdentityId(5000), icon(500)));
    assert!(s.bytes() <= MAX_ICON_BYTES);
}

// ------------------------------------------------------------------ T3.4 the thread

/// A backend that answers from a table and records the batch sizes through the sink.
struct Table;

impl Backend for Table {
    fn identity(&mut self, hwnd: isize, pid: u32, exe: &str) -> (String, Option<String>) {
        (format!("{hwnd}:{pid}:{exe}"), None)
    }
    fn name(&mut self, key: &str, _: &str, _: Option<&str>) -> Option<String> {
        Some(format!("name of {key}"))
    }
    fn icon(&mut self, _: &str, _: Option<&str>, px: u16) -> Option<IconBitmap> {
        Some(icon(u32::from(px)))
    }
    fn publisher(&mut self, _: &str) -> Option<String> {
        None
    }
}

#[test]
fn answers_come_in_batches_through_the_sink() {
    let (tx, rx) = mpsc::channel();
    let r = spawn_with(
        || Table,
        move |a| {
            let _ = tx.send(a);
        },
    )
    .expect("thread");
    for n in 0..100u32 {
        r.request(
            Request::Name {
                identity: Some(IdentityId(n)),
                exe_id: None,
                key: format!("k{n}"),
                exe: String::new(),
                aumid: None,
            },
            false,
        );
    }
    let mut got = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while got.len() < 100 && Instant::now() < deadline {
        if let Ok(batch) = rx.recv_timeout(Duration::from_millis(200)) {
            assert!(batch.len() <= BATCH, "{} answers in one message", batch.len());
            got.extend(batch);
        }
    }
    assert_eq!(got.len(), 100);
    assert!(got.contains(&Answer::Name {
        identity: Some(IdentityId(7)),
        exe_id: None,
        name: Some("name of k7".into())
    }));
    let started = Instant::now();
    r.stop();
    assert!(started.elapsed() < Duration::from_millis(900), "an idle thread ends at once");
}

/// A backend that is stuck in a call until the test lets it go.
struct Blocked {
    gate: Arc<Mutex<mpsc::Receiver<()>>>,
    entered: mpsc::Sender<()>,
    calls: Arc<AtomicUsize>,
}

impl Backend for Blocked {
    fn identity(&mut self, _: isize, _: u32, _: &str) -> (String, Option<String>) {
        (String::new(), None)
    }
    fn name(&mut self, _: &str, _: &str, _: Option<&str>) -> Option<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let _ = self.entered.send(());
        let _ = self.gate.lock().expect("gate").recv_timeout(Duration::from_secs(20));
        None
    }
    fn icon(&mut self, _: &str, _: Option<&str>, _: u16) -> Option<IconBitmap> {
        None
    }
    fn publisher(&mut self, _: &str) -> Option<String> {
        None
    }
}

#[test]
fn stop_within_a_second() {
    let (release, gate) = mpsc::channel();
    let (entered_tx, entered) = mpsc::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let posted = Arc::new(AtomicUsize::new(0));
    let (calls2, posted2) = (calls.clone(), posted.clone());
    let r = spawn_with(
        move || Blocked { gate: Arc::new(Mutex::new(gate)), entered: entered_tx, calls: calls2 },
        move |_| {
            posted2.fetch_add(1, Ordering::SeqCst);
        },
    )
    .expect("thread");
    r.request(Request::Name { identity: None, exe_id: None, key: "k".into(), exe: String::new(), aumid: None }, true);
    entered.recv_timeout(Duration::from_secs(10)).expect("the thread is inside the blocked call");
    let started = Instant::now();
    r.stop();
    let took = started.elapsed();
    assert!(
        took >= Duration::from_millis(900) && took < Duration::from_millis(2500),
        "{took:?}: the thread is let go after 1 s"
    );
    // The thread, let go, ends by itself when its call returns, and posts nothing on the way.
    let _ = release.send(());
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!((calls.load(Ordering::SeqCst), posted.load(Ordering::SeqCst)), (1, 0));
}

#[test]
fn list_apps_format() {
    assert_eq!(
        list_apps_lines("Contoso Draw", "shortcut", r"c:\program files\contoso\ctsdraw.exe"),
        [
            "- name   : Contoso Draw   [shortcut]".to_string(),
            r"  key    : c:\program files\contoso\ctsdraw.exe".to_string()
        ]
    );
}

// ------------------------------------------------------------------ the name book

#[test]
fn the_book_keeps_answers_and_says_when_a_name_changed() {
    let mut b = NameBook::default();
    assert!(b.first(Ask::Name(IdentityId(1))));
    assert!(!b.first(Ask::Name(IdentityId(1))), "each key once per session");
    let name = |id, n: Option<&str>| Answer::Name {
        identity: Some(IdentityId(id)),
        exe_id: Some(ExeId(id)),
        name: n.map(str::to_string),
    };
    assert!(on_done(&mut b, ResolverDone(vec![name(1, Some("Orbit"))])));
    assert_eq!((b.name_of_identity(IdentityId(1), "k"), b.name_of_exe(ExeId(1))), (Some("Orbit"), Some("Orbit")));
    assert!(!on_done(&mut b, ResolverDone(vec![name(1, Some("Orbit"))])), "the same name again changes nothing");
    assert!(!on_done(&mut b, ResolverDone(vec![name(2, None), Answer::Nothing])), "a failure is kept as none");
    assert_eq!(b.name_of_identity(IdentityId(2), "k2"), None);
    assert!(on_done(&mut b, ResolverDone(vec![name(1, Some("Orbit 2"))])));
    // An identity, an icon and a publisher.
    let id = Answer::Identity { hwnd: 5, pid: 6, key: "k".into(), aumid: Some("A!B".into()) };
    assert!(on_done(&mut b, ResolverDone(vec![id.clone()])));
    assert!(!on_done(&mut b, ResolverDone(vec![id])));
    assert_eq!(b.identity(5, 6).map(|i| (i.key.as_str(), i.aumid.as_deref())), Some(("k", Some("A!B"))));
    assert!(on_done(
        &mut b,
        ResolverDone(vec![Answer::Icon { identity: IdentityId(1), px: 16, bitmap: Some(icon(16)) }])
    ));
    assert_eq!(b.icon_of(IdentityId(1)).map(|i| i.width), Some(16));
    on_done(&mut b, ResolverDone(vec![Answer::Publisher { exe_id: ExeId(1), publisher: Some("Contoso".into()) }]));
    assert_eq!(b.publisher_of(ExeId(1)), Some("Contoso"));
    // Scenes' names, by identity key.
    b.set_fixture_names(&[("scene.app".to_string(), "Scene App".to_string())]);
    assert_eq!(b.name_of_identity(IdentityId(77), "scene.app"), Some("Scene App"));
}

// ------------------------------------------------------------------ T3.5 names.bin

fn sample_bin() -> NamesBin {
    let mut b = NamesBin::new(2);
    b.entries.push(Entry {
        hash: 1,
        size: 10,
        mtime: 20,
        name: "Orbit \u{e9}".into(),
        icon: Some(vec![1, 2, 3, 0xFF00_00FF]),
    });
    b.entries.push(Entry { hash: 2, size: 11, mtime: -21, name: String::new(), icon: None });
    b
}

#[test]
fn names_bin_roundtrip() {
    let b = sample_bin();
    let bytes = names_bin::encode(&b);
    assert_eq!(&bytes[..4], b"TNAM");
    assert_eq!(names_bin::parse(&bytes), Ok(b.clone()));
    // Through a file, atomically, and read back through the capped reader.
    let s = Scratch::new("bin");
    let mut b2 = b.clone();
    b2.fresh = 3;
    names_bin::save(Some(&s.0), &mut b2);
    assert_eq!(b2.fresh, 0);
    assert_eq!(names_bin::load(Some(&s.0)), Some(b));
    assert_eq!(names_bin::load(None), None);
    // 400 entries at most, the first (most recent) kept; and never more than 1 MiB.
    let mut many = NamesBin::new(32);
    for n in 0..450u64 {
        many.entries.push(Entry { hash: n, size: 0, mtime: 0, name: "x".into(), icon: Some(vec![7; 1024]) });
    }
    let bytes = names_bin::encode(&many);
    assert!(bytes.len() <= names_bin::MAX_BYTES);
    let back = names_bin::parse(&bytes).expect("a valid file");
    assert!(back.entries.len() <= names_bin::MAX_ENTRIES && !back.entries.is_empty());
    assert_eq!(back.entries[0].hash, 0, "the most recent first");
}

#[test]
fn names_bin_total() {
    let good = names_bin::encode(&sample_bin());
    // Every cut.
    for n in 0..good.len() {
        assert!(names_bin::parse(&good[..n]).is_err(), "cut at {n}");
    }
    // 10,000 random and mutated inputs: never a panic; an error discards the whole file.
    let mut next = rng(0x9E37_79B9_7F4A_7C15);
    let mut ok = 0;
    for i in 0..10_000u32 {
        let input: Vec<u8> = if i % 2 == 0 {
            (0..(next() % 300) as usize).map(|_| next() as u8).collect()
        } else {
            let mut b = good.clone();
            for _ in 0..1 + next() % 4 {
                let at = (next() as usize) % b.len();
                b[at] = next() as u8;
            }
            b
        };
        if let Ok(parsed) = names_bin::parse(&input) {
            ok += 1;
            assert!(parsed.entries.len() <= names_bin::MAX_ENTRIES && parsed.side <= names_bin::MAX_SIDE);
        }
    }
    assert!(ok > 0, "some mutations keep a valid file");
    // The named refusals.
    let mut bad = good.clone();
    bad[0] = b'X';
    assert_eq!(names_bin::parse(&bad), Err(Bad::Magic));
    let mut bad = good.clone();
    bad[4] = 9;
    assert_eq!(names_bin::parse(&bad), Err(Bad::Version));
    let mut bad = good.clone();
    bad[8..12].copy_from_slice(&401u32.to_le_bytes());
    assert_eq!(names_bin::parse(&bad), Err(Bad::Count));
    let mut bad = good.clone();
    bad[6..8].copy_from_slice(&65u16.to_le_bytes());
    assert_eq!(names_bin::parse(&bad), Err(Bad::Side));
    assert_eq!(names_bin::parse(&vec![0u8; names_bin::MAX_BYTES + 1]), Err(Bad::TooLarge));
}

#[test]
fn stale_entry_ignored() {
    let mut b = NamesBin::new(16);
    b.record_name(r"C:\Apps\Orbit.exe", (100, 5), "Orbit");
    b.record_name(r"C:\Apps\Quill.exe", (200, 6), "Quill");
    assert_eq!(b.fresh, 2);
    let stamp = |size: u64, time: i64| move |_: &str| Some((size, time));
    // The same size and time (any case of the path): the name is used, and the entry moves to the front.
    assert_eq!(b.lookup(r"c:\apps\ORBIT.exe", stamp(100, 5)).map(|e| e.name.as_str()), Some("Orbit"));
    assert_eq!(b.entries[0].name, "Orbit");
    // The size or the time changed, or the file cannot be read: ignored, and dropped.
    assert!(b.lookup(r"C:\Apps\Orbit.exe", stamp(101, 5)).is_none());
    assert_eq!(b.entries.len(), 1);
    assert!(b.lookup(r"C:\Apps\Quill.exe", stamp(200, 7)).is_none());
    assert!(b.entries.is_empty());
    // An icon of another side drops the others (the file holds one size); a wrong pixel count is refused.
    b.record_name(r"C:\Apps\Orbit.exe", (100, 5), "Orbit");
    b.record_icon(r"C:\Apps\Orbit.exe", (100, 5), 2, &[1, 2, 3, 4]);
    assert!(b.entries[0].icon.is_some());
    b.record_icon(r"C:\Apps\Orbit.exe", (100, 5), 3, &[1, 2, 3, 4]);
    assert_eq!(b.side, 2, "a wrong pixel count changes nothing");
    b.record_icon(r"C:\Apps\Quill.exe", (200, 6), 3, &[0; 9]);
    assert_eq!(b.side, 3);
    assert!(b.entries.iter().find(|e| e.name == "Orbit").is_some_and(|e| e.icon.is_none()));
    // A file the size and time of which are read now: the stamp of a real file.
    let s = Scratch::new("stamp");
    let exe = fake_exe(&s.0, "orbit.exe");
    let (size, _) = names_bin::stamp_of(&exe).expect("stamp");
    assert_eq!(size, 2);
    assert!(names_bin::stamp_of(&s.0.join("missing.exe").to_string_lossy()).is_none());
    assert_eq!(names_bin::path_hash("C:\\A.exe"), names_bin::path_hash("c:\\a.EXE"));
    assert!(!b.due());
    b.fresh = names_bin::WRITE_EVERY;
    assert!(b.due());
}

#[test]
fn a_cached_name_and_icon_answer_without_the_sources() {
    let s = Scratch::new("cache");
    let exe = fake_exe(&s.0, "orbit.exe");
    let stamp = names_bin::stamp_of(&exe).expect("stamp");
    let mut bin = NamesBin::new(2);
    bin.record_name(&exe, stamp, "Orbit Cached");
    bin.record_icon(&exe, stamp, 2, &[0xFF00_0001, 0xFF00_0002, 0xFF00_0003, 0xFF00_0004]);
    let mut backend = ShellBackend::with_parts(StartMenuIndex::default(), BTreeMap::new(), bin, None, String::new());
    assert_eq!(backend.name("k", &exe, None).as_deref(), Some("Orbit Cached"));
    let got = backend.icon(&exe, None, 2).expect("the cached icon");
    assert_eq!((got.width, got.pixels[3]), (2, 0xFF00_0004));
}

// ------------------------------------------------------------------ T3.5 the dock's renames

#[test]
fn dock_names_parse() {
    let text = b"{ // the dock's file\n \"other\": 1, /* c */ \"names\": { \" Chrome.exe \": \" Browser \", \"b\": 5, \"\": \"x\", \"c\": \"   \" } }";
    let got = dock_names::parse(text);
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got.get("Chrome.exe").map(String::as_str), Some("Browser"), "keys and values trimmed");
    // A non-object "names" and damage are ignored.
    assert!(dock_names::parse(br#"{"names": ["a", "b"]}"#).is_empty());
    assert!(dock_names::parse(br#"{"names": "x"}"#).is_empty());
    assert!(dock_names::parse(b"{ not json").is_empty());
    assert!(dock_names::parse(&[0xFF, 0xFE, 0x00]).is_empty());
    assert!(dock_names::parse(b"").is_empty());
    // Limits: 256 entries, keys of at most 260 and values of at most 64 characters.
    let mut obj = String::from("{\"names\":{");
    for n in 0..300 {
        obj.push_str(&format!("\"app{n:03}.exe\":\"Name {n}\","));
    }
    obj.push_str(&format!("\"{}\":\"long key\",", "k".repeat(261)));
    obj.push_str(&format!("\"long\":\"{}\"}}}}", "v".repeat(65)));
    let capped = dock_names::parse(obj.as_bytes());
    assert_eq!(capped.len(), dock_names::MAX_ENTRIES);
    assert!(!capped.contains_key("long") && capped.keys().all(|k| k.chars().count() <= 260));
    let edge = format!("{{\"names\":{{\"{}\":\"{}\"}}}}", "k".repeat(260), "v".repeat(64));
    assert_eq!(dock_names::parse(edge.as_bytes()).len(), 1, "the limits themselves are allowed");
    // The file: read through the capped reader; a 2 MiB file is refused; never written.
    let s = Scratch::new("names");
    assert!(dock_names::load(None).is_empty());
    assert!(dock_names::load(Some(&s.0)).is_empty(), "no file");
    let path = s.0.join("config.json");
    std::fs::write(&path, br#"{"names":{"a.exe":"A"}}"#).expect("config");
    assert_eq!(dock_names::load(Some(&s.0)).get("a.exe").map(String::as_str), Some("A"));
    let before = std::fs::read(&path).expect("read");
    let big = format!("{{\"names\":{{\"a.exe\":\"A\"}},\"pad\":\"{}\"}}", "x".repeat(2 << 20));
    std::fs::write(&path, big).expect("big config");
    assert!(dock_names::load(Some(&s.0)).is_empty(), "above 1 MiB");
    std::fs::write(&path, &before).expect("config");
    let _ = dock_names::load(Some(&s.0));
    assert_eq!(std::fs::read(&path).expect("read"), before, "read-only");
}
