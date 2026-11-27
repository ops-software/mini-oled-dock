//! Type names (P3-files-app F2.3): the fallback until the shell host's name arrives, the one request in flight, the
//! cache in types.json and its caps. Plain tests over fakes (made-up names, no file system), and one test against a
//! debug shell host.

mod common;

use std::rc::Rc;

use files_engine::fakes::{FakeOwn, Fakes};
use files_engine::format::{Fmt, LocalTime};
use files_engine::listing::{Batch, EntryStore, Gen, RawEntry, WidePath};
use files_engine::model::{ColKind, FilesTable, ViewOpts};
use files_engine::msg::{Fwd, Msg};
use files_engine::names::TYPES_FILE;
use files_engine::services::{OwnFiles, ReqOwner};
use files_engine::session::Session;
use files_engine::shell_client::{FallbackWhy, ShellReply};
use files_engine::types::{self, TypeNames, FILE_CAP, MAX_NAMES, SAVE_EVERY_MS};
use files_engine::window::{self, Shared, WinKind, WinRec};
use oled_trust::frame::files::{ReqId, ShellEvt, ShellReq, TypeName};
use oled_ui::model::{CellOut, ColId, TableModel};
use oled_ui::UiLang;

/// The user's UI language id the tests' names are in (en-US), and another one (it-IT).
const EN_US: u16 = 0x0409;
const IT_IT: u16 = 0x0410;

/// Fakes with window 1 a main window that showed its first frame, over `types`.
fn shared_with(f: &Fakes, types: TypeNames) -> Rc<Shared> {
    let shared = Shared::new(std::sync::Arc::clone(&f.hub), f.services(), UiLang::EN, Session::default(), types);
    shared.windows.borrow_mut().push(WinRec { win: 1, kind: WinKind::Main, wref: None, hwnd: None, location: None });
    shared.first_frame_done.set(true);
    shared
}

fn exts<'a>(e: &'a [&'a str]) -> impl Iterator<Item = &'a [u8]> {
    e.iter().map(|s| s.as_bytes())
}

/// The last request FakeShell recorded: its id and extensions.
fn last_request(f: &Fakes) -> (ReqId, Vec<String>) {
    match f.shell.requests().last() {
        Some(ShellReq::TypeNames { req, exts }) => (*req, exts.iter().map(|e| e.to_string()).collect()),
        other => panic!("{other:?}"),
    }
}

/// The host answers `req` with these names, then ReqDone; the primary window drains the engines. Returns whether a
/// Bump reached window 1.
fn answer(f: &Fakes, shared: &Shared, req: ReqId, names: &[(&str, &str)]) -> bool {
    if !names.is_empty() {
        let names = names.iter().map(|(e, n)| TypeName { ext: (*e).into(), name: (*n).into() }).collect();
        f.shell.reply(ShellReply::Evt(ShellEvt::TypeNames { req, names }));
    }
    f.shell.reply(ShellReply::Evt(ShellEvt::ReqDone { req }));
    f.deliver();
    window::drain_engines(shared);
    f.posted.take().iter().any(|m| matches!(m, Msg::Fwd(Fwd::Bump)))
}

fn table(shared: &Shared, files: &[&str]) -> FilesTable {
    let mut b = Batch { gen: Gen(0), names: Vec::new(), entries: Vec::new(), done: true, error: None, elapsed_us: 0 };
    for name in files {
        let name_off = b.names.len() as u32;
        b.names.extend_from_slice(name.as_bytes());
        b.entries.push(RawEntry {
            name_off,
            name_len: name.len() as u16,
            attrs: 0x20,
            reparse_tag: 0,
            size: 1,
            mtime: 0,
            ctime: 0,
        });
    }
    let mut store = EntryStore::new(WidePath::parse(r"C:\mod-files-made-up\reports").unwrap(), Gen(0));
    store.append(b);
    store.merge_pending();
    let now = LocalTime { year: 2026, month: 11, day: 21, hour: 15, minute: 0 };
    FilesTable::new(Box::new(store), Rc::new(Fmt::fixed_en()), Rc::clone(&shared.types), ViewOpts::default(), now)
}

fn type_cell(t: &FilesTable, name: &str) -> String {
    let id = t.listing().find(name.as_bytes()).expect("listed");
    let row = t.listing().row_of(id).expect("visible") as usize;
    let mut out = CellOut::default();
    t.cell(row, ColId(ColKind::Type as u16), &mut out);
    out.text.to_string()
}

#[test]
fn ui_fallback_then_name() {
    let f = Fakes::new();
    let shared = shared_with(&f, TypeNames::empty());
    let mut t = table(&shared, &["report-final-v2.pdf", "notes.txt", "README"]);
    assert_eq!(type_cell(&t, "report-final-v2.pdf"), "PDF File", "File Explorer's fallback until a name arrives");
    assert_eq!(type_cell(&t, "README"), "File");
    assert!(types::request_for(&shared, exts(&["pdf", "pdf", "txt"])));
    let (req, asked) = last_request(&f);
    assert_eq!(asked, ["pdf", "txt"], "each extension once, in order");
    assert_eq!(shared.req_owner.borrow().get(&req), Some(&ReqOwner::TypeNames));
    assert_eq!(shared.types.in_flight(), Some(req));
    // One request in flight: nothing more goes out meanwhile.
    assert!(!types::request_for(&shared, exts(&["zip"])));
    assert_eq!(f.shell.requests().len(), 1);
    // The host's answer reaches the cache and bumps every main window's tables.
    assert!(answer(&f, &shared, req, &[("pdf", "Portable Document"), ("txt", "Text Document")]));
    assert_eq!(shared.types.get(b"pdf").as_deref(), Some("Portable Document"));
    assert!(shared.req_owner.borrow().get(&req).is_none(), "the request ended");
    assert_eq!(shared.types.in_flight(), None);
    t.bump();
    assert_eq!(type_cell(&t, "report-final-v2.pdf"), "Portable Document");
    assert_eq!(type_cell(&t, "notes.txt"), "Text Document");
    // Named extensions are not asked again; the next missing one is.
    assert!(!types::request_for(&shared, exts(&["pdf", "txt"])));
    assert!(types::request_for(&shared, exts(&["pdf", "zip"])));
    assert_eq!(last_request(&f).1, ["zip"]);
}

#[test]
fn unnamed_and_unkeyable_extensions_are_asked_once_or_never() {
    let f = Fakes::new();
    let shared = shared_with(&f, TypeNames::empty());
    // Keys the protocol refuses (upper case, a dot, over 64 bytes, not UTF-8, empty) keep their fallback.
    let long = "x".repeat(65);
    let bad: [&[u8]; 5] = [b"PDF", b"tar.gz", long.as_bytes(), &[0xED, 0xA0, 0x80], b""];
    assert!(!types::request_for(&shared, bad));
    assert!(f.shell.requests().is_empty());
    // The host names one of two: the other is not asked again, so a relayout never loops on it.
    assert!(types::request_for(&shared, exts(&["txt", "mod-files-noapp"])));
    let (req, _) = last_request(&f);
    assert!(answer(&f, &shared, req, &[("txt", "Text Document")]));
    assert!(!types::request_for(&shared, exts(&["txt", "mod-files-noapp"])));
    // A name for an extension not asked for, or an empty one, is ignored.
    assert!(types::request_for(&shared, exts(&["zip"])));
    let (req, _) = last_request(&f);
    answer(&f, &shared, req, &[("exe", "Application"), ("zip", "   ")]);
    assert_eq!((shared.types.get(b"exe"), shared.types.get(b"zip")), (None, None));
    // A request that falls back ends too, and its extensions wait for the next association change.
    assert!(types::request_for(&shared, exts(&["7z"])));
    let (req, _) = last_request(&f);
    f.shell.reply(ShellReply::Fallback { req, why: FallbackWhy::Deadline });
    f.deliver();
    window::drain_engines(&shared);
    assert_eq!(shared.types.in_flight(), None);
    assert!(!types::request_for(&shared, exts(&["7z"])));
}

#[test]
fn assoc_change_drops_names_and_asks_again() {
    let f = Fakes::new();
    let shared = shared_with(&f, TypeNames::empty());
    assert!(types::request_for(&shared, exts(&["txt", "pdf"])));
    let (first, _) = last_request(&f);
    assert!(answer(&f, &shared, first, &[("txt", "Text Document")]));
    assert!(types::request_for(&shared, exts(&["zip"])));
    let (second, _) = last_request(&f);
    // Associations change while the second request is out: every name goes, and the late answer is dropped.
    files_engine::icons::on_assoc_changed(&shared);
    assert!(shared.types.is_empty());
    assert!(!types::request_for(&shared, exts(&["txt"])), "still one in flight");
    answer(&f, &shared, second, &[("zip", "Compressed Folder")]);
    assert_eq!(shared.types.get(b"zip"), None, "a name from before the change");
    // Asked again, including the one named in vain before.
    assert!(types::request_for(&shared, exts(&["txt", "pdf", "zip"])));
    assert_eq!(last_request(&f).1, ["txt", "pdf", "zip"]);
}

#[test]
fn nothing_before_the_first_frame() {
    let f = Fakes::new();
    f.own.put(TYPES_FILE, br#"{"lang":1033,"names":{"pdf":"Portable Document"}}"#);
    let shared = Shared::new(
        std::sync::Arc::clone(&f.hub),
        f.services(),
        UiLang::EN,
        Session::default(),
        TypeNames::load(&*f.own, EN_US),
    );
    // The first frame shows the cached names; no host starts for the others before it.
    let t = table(&shared, &["report-final-v2.pdf", "notes.txt"]);
    assert_eq!(type_cell(&t, "report-final-v2.pdf"), "Portable Document");
    assert_eq!(type_cell(&t, "notes.txt"), "TXT File");
    assert!(!types::request_for(&shared, exts(&["txt"])));
    assert!(f.shell.requests().is_empty());
    // Nor is types.json written before it, however much is due.
    shared.types.clear();
    f.clock.set_now(10 * SAVE_EVERY_MS);
    types::on_reply(&shared, &ShellReply::Evt(ShellEvt::Busy { on: false }));
    assert!(f.own.writes().is_empty());
    shared.first_frame_done.set(true);
    types::on_reply(&shared, &ShellReply::Evt(ShellEvt::Busy { on: false }));
    assert_eq!(f.own.writes(), [TYPES_FILE]);
    assert!(types::request_for(&shared, exts(&["txt"])));
}

#[test]
fn types_json_round_trip_and_lang_mismatch() {
    let own = FakeOwn::default();
    own.put(TYPES_FILE, br#"{"lang":1033,"names":{"pdf":"Portable Document","txt":"Text Document"}}"#);
    let t = TypeNames::load(&own, EN_US);
    assert_eq!((t.lang(), t.len()), (EN_US, 2));
    assert_eq!(t.get(b"txt").as_deref(), Some("Text Document"));
    let other = TypeNames::load(&own, IT_IT);
    assert!(other.is_empty() && other.lang() == IT_IT, "names in another language are ignored");
    // Malformed, hostile or partly wrong files: defaults, or only the entries that keep the protocol's rules.
    let bad: [&[u8]; 7] =
        [b"", b"{", b"[]", b"null", br#"{"lang":"1033","names":{}}"#, br#"{"lang":1033}"#, b"\xff\xfe"];
    for bad in bad {
        own.put(TYPES_FILE, bad);
        assert!(TypeNames::load(&own, EN_US).is_empty(), "{:?}", String::from_utf8_lossy(bad));
    }
    let long = "n".repeat(261);
    let text = format!(
        r#"{{"lang":1033,"names":{{"PDF":"a","a.b":"b","":"c","e":"","n":5,"long":"{long}","ok":"  Fine  "}}}}"#
    );
    own.put(TYPES_FILE, text.as_bytes());
    let t = TypeNames::load(&own, EN_US);
    assert_eq!((t.len(), t.get(b"ok").as_deref()), (1, Some("Fine")));
    // Over the cap: not read at all.
    let big = format!(r#"{{"lang":1033,"names":{{"pdf":"{}"}}}}"#, "x".repeat(FILE_CAP));
    own.put(TYPES_FILE, big.as_bytes());
    assert!(TypeNames::load(&own, EN_US).is_empty());
    // Saved through Services.own (after the first frame: see nothing_before_the_first_frame), at most once a minute,
    // then read back.
    let f = Fakes::new();
    let shared = shared_with(&f, TypeNames::load(&*f.own, EN_US));
    assert!(types::request_for(&shared, exts(&["pdf", "txt"])));
    let (req, _) = last_request(&f);
    f.clock.set_now(2 * SAVE_EVERY_MS);
    assert!(answer(&f, &shared, req, &[("pdf", "Portable Document"), ("txt", "Text Document")]));
    assert_eq!(f.own.writes(), [TYPES_FILE]);
    let back = TypeNames::load(&*f.own, EN_US);
    assert_eq!((back.len(), back.get(b"pdf").as_deref()), (2, Some("Portable Document")));
    assert!(TypeNames::load(&*f.own, IT_IT).is_empty());
    // Nothing new: no write. A new name within the minute waits; after it, it is written.
    f.clock.set_now(2 * SAVE_EVERY_MS + 1_000);
    types::on_reply(&shared, &ShellReply::Evt(ShellEvt::Busy { on: false }));
    assert_eq!(f.own.writes().len(), 1);
    assert!(types::request_for(&shared, exts(&["zip"])));
    let (req, _) = last_request(&f);
    f.clock.set_now(2 * SAVE_EVERY_MS + SAVE_EVERY_MS / 2);
    answer(&f, &shared, req, &[("zip", "Compressed Folder")]);
    assert_eq!(f.own.writes().len(), 1, "at most once a minute");
    f.clock.set_now(3 * SAVE_EVERY_MS);
    types::on_reply(&shared, &ShellReply::Evt(ShellEvt::Busy { on: false }));
    assert_eq!(f.own.writes().len(), 2);
    assert_eq!(TypeNames::load(&*f.own, EN_US).get(b"zip").as_deref(), Some("Compressed Folder"));
}

/// The first names after the first frame are written at once, so an idle window keeps them; later changes at most
/// once a minute.
#[test]
fn first_names_saved_at_once_then_once_a_minute() {
    let f = Fakes::new();
    let shared = shared_with(&f, TypeNames::load(&*f.own, EN_US));
    f.clock.set_now(1_000);
    assert!(types::request_for(&shared, exts(&["pdf"])));
    let (req, _) = last_request(&f);
    assert!(answer(&f, &shared, req, &[("pdf", "Portable Document")]));
    assert_eq!(f.own.writes(), [TYPES_FILE], "never saved: written at once");
    f.clock.set_now(30_000);
    assert!(types::request_for(&shared, exts(&["txt"])));
    let (req, _) = last_request(&f);
    assert!(answer(&f, &shared, req, &[("txt", "Text Document")]));
    assert_eq!(f.own.writes().len(), 1, "within the minute of the first save");
    f.clock.set_now(1_000 + SAVE_EVERY_MS);
    types::on_reply(&shared, &ShellReply::Evt(ShellEvt::Busy { on: false }));
    assert_eq!(f.own.writes(), [TYPES_FILE, TYPES_FILE]);
    assert_eq!(TypeNames::load(&*f.own, EN_US).get(b"txt").as_deref(), Some("Text Document"));
}

/// A types.json read at start and not changed since is not written again, however long the window lives.
#[test]
fn a_loaded_types_json_is_not_rewritten() {
    let f = Fakes::new();
    f.own.put(TYPES_FILE, br#"{"lang":1033,"names":{"pdf":"Portable Document"}}"#);
    let shared = shared_with(&f, TypeNames::load(&*f.own, EN_US));
    f.clock.set_now(10 * SAVE_EVERY_MS);
    types::on_reply(&shared, &ShellReply::Evt(ShellEvt::Busy { on: false }));
    assert!(f.own.writes().is_empty(), "nothing changed since it was read");
}

#[test]
fn cap_256() {
    let f = Fakes::new();
    let shared = shared_with(&f, TypeNames::load(&*f.own, EN_US));
    let many: Vec<String> = (0..300).map(|i| format!("e{i:03}")).collect();
    let all = || many.iter().map(|s| s.as_bytes());
    // One request asks for at most 256 extensions.
    assert!(types::request_for(&shared, all()));
    let (req, asked) = last_request(&f);
    assert_eq!(asked.len(), MAX_NAMES);
    assert_eq!(asked, many[..MAX_NAMES]);
    let long = |e: &str| format!("{e} {}", "n".repeat(200));
    let names: Vec<(String, String)> = asked.iter().map(|e| (e.clone(), long(e))).collect();
    let pairs: Vec<(&str, &str)> = names.iter().map(|(e, n)| (e.as_str(), n.as_str())).collect();
    assert!(answer(&f, &shared, req, &pairs));
    assert_eq!(shared.types.len(), MAX_NAMES);
    // The rest: past 256 names the oldest go.
    assert!(types::request_for(&shared, all()));
    let (req, asked) = last_request(&f);
    assert_eq!(asked, many[MAX_NAMES..]);
    let names: Vec<(String, String)> = asked.iter().map(|e| (e.clone(), long(e))).collect();
    let pairs: Vec<(&str, &str)> = names.iter().map(|(e, n)| (e.as_str(), n.as_str())).collect();
    answer(&f, &shared, req, &pairs);
    assert_eq!(shared.types.len(), MAX_NAMES);
    assert!(shared.types.get(b"e299").is_some() && shared.types.get(b"e000").is_none());
    // types.json keeps the newest names that fit 16 KiB.
    f.clock.set_now(SAVE_EVERY_MS);
    types::on_reply(&shared, &ShellReply::Evt(ShellEvt::Busy { on: false }));
    let bytes = f.own.read(TYPES_FILE, usize::MAX).expect("written");
    assert!(bytes.len() <= FILE_CAP, "{} bytes", bytes.len());
    let back = TypeNames::load(&*f.own, EN_US);
    assert!(back.len() > 50 && back.len() < MAX_NAMES, "{} names", back.len());
    assert!(back.get(b"e299").is_some() && back.get(b"e044").is_none(), "the newest are kept");
    // A file of more than 256 names loads 256 of them.
    let entries: Vec<String> = (0..300).map(|i| format!(r#""x{i}":"X""#)).collect();
    f.own.put(TYPES_FILE, format!(r#"{{"lang":1033,"names":{{{}}}}}"#, entries.join(",")).as_bytes());
    assert_eq!(TypeNames::load(&*f.own, EN_US).len(), MAX_NAMES);
}

/// A debug shell host from the testkit names "txt" (P1-files-engine's hooks: its own data folder, no system UI).
#[cfg(debug_assertions)]
#[test]
fn type_names_end_to_end() {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use common::temp::TempRoot;
    use files_engine::shell_client::{ShellClient, ShellConfig};

    let root = TempRoot::new("types-host");
    let client =
        ShellClient::new(ShellConfig { exe: common::files_exe(), test: Some(common::hooks(&root)) }, Arc::new(|| {}));
    let req = client.next_req();
    client.request(ShellReq::TypeNames { req, exts: vec!["txt".into(), "mod-files-noapp".into()] });
    let (mut names, mut done) = (Vec::new(), false);
    let end = Instant::now() + Duration::from_secs(60);
    while !done {
        for r in client.drain() {
            match r {
                ShellReply::Evt(ShellEvt::TypeNames { req: r, names: n }) if r == req => names.extend(n),
                ShellReply::Evt(ShellEvt::ReqDone { req: r }) if r == req => done = true,
                ShellReply::Evt(ShellEvt::Failed { req: r, .. }) | ShellReply::Fallback { req: r, .. } if r == req => {
                    panic!("the request failed: {r:?}")
                }
                _ => {}
            }
        }
        assert!(Instant::now() < end, "timed out; names so far: {names:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
    let txt = names.iter().find(|n| &*n.ext == "txt").expect("a name for txt");
    assert!(!txt.name.trim().is_empty() && txt.name.len() <= 260, "{txt:?}");
    client.shutdown();
}
