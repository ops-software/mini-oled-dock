//! W5a: the generic store over a hand-written options type, with its files in a temp folder of the test
//! (FileKind::PerUser), and the JSONC helpers.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant, SystemTime};

use oled_config::jsonc::{js, locate, render_extra, strip_bom, strip_comments};
use oled_config::*;
use serde_json::Value;

const MARK: &str = "// test configuration";

/// Three options and the keys of a newer version, read and written by hand.
#[derive(Clone, Debug, PartialEq)]
struct TestCfg {
    a: bool,
    n: u32,
    s: String,
    extra: serde_json::Map<String, Value>,
}

impl Default for TestCfg {
    fn default() -> Self {
        TestCfg { a: true, n: 7, s: "seven".into(), extra: serde_json::Map::new() }
    }
}

/// Version 0 called "n" "count".
fn rename_count(v: &mut Value) -> Result<(), MigrateError> {
    if let Some(o) = v.as_object_mut() {
        if let Some(x) = o.remove("count") {
            o.entry("n").or_insert(x);
        }
    }
    Ok(())
}

impl Options for TestCfg {
    const VERSION: u16 = 1;
    const FILE: &'static str = "test.json";
    fn descriptors() -> &'static [Descriptor] {
        &[]
    }
    fn known_keys() -> &'static [&'static str] {
        &["a", "n", "s"]
    }
    /// The whole file or nothing, as the dock's config.rs 1.5.2: a wrong type anywhere is one Parse error.
    fn from_value(v: &Value, _text: &str, last_good: Option<&Self>) -> (Self, Vec<ConfigError>) {
        let kept = if last_good.is_some() { Kept::LastGood } else { Kept::Default };
        let bad = |key: &str| {
            let problem = Problem::Parse(format!("{key} has the wrong type"));
            let e = ConfigError { path: key.to_string(), line: 0, col: 0, problem, kept };
            (last_good.cloned().unwrap_or_default(), vec![e])
        };
        let mut c = TestCfg::default();
        let Some(obj) = v.as_object() else { return bad("") };
        for (k, x) in obj {
            match k.as_str() {
                "a" => match x.as_bool() {
                    Some(b) => c.a = b,
                    None => return bad("a"),
                },
                "n" => match x.as_u64().and_then(|n| u32::try_from(n).ok()) {
                    Some(n) => c.n = n,
                    None => return bad("n"),
                },
                "s" => match x.as_str() {
                    Some(s) => c.s = s.to_string(),
                    None => return bad("s"),
                },
                _ => {
                    c.extra.insert(k.clone(), x.clone());
                }
            }
        }
        (c, Vec::new())
    }
    fn to_template(&self) -> String {
        format!(
            "{MARK}\n// Edit and save.\n{{\n  \"a\": {},\n  \"n\": {},\n  \"s\": {}{}\n}}\n",
            self.a,
            self.n,
            js(&self.s),
            render_extra(&self.extra)
        )
    }
    fn template_mark() -> &'static str {
        MARK
    }
    fn migrations() -> &'static [fn(&mut Value) -> Result<(), MigrateError>] {
        &[rename_count]
    }
}

/// A folder of its own under the temp folder, removed at the end.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        static N: AtomicU32 = AtomicU32::new(0);
        let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
        let name = format!("oled_config_{tag}_{}_{}_{nanos}", std::process::id(), N.fetch_add(1, Ordering::SeqCst));
        let p = std::env::temp_dir().join(name);
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }

    fn file(&self) -> PathBuf {
        self.0.join("test.json")
    }

    fn bak(&self) -> PathBuf {
        self.0.join("test.json.bak")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn store(dir: &TempDir) -> Store<TestCfg> {
    Store::new(Some(FileSpec { path: dir.file(), kind: FileKind::PerUser, cap: 64 * 1024 }), |_| {})
}

fn read(p: &Path) -> String {
    String::from_utf8(std::fs::read(p).unwrap()).unwrap()
}

/// A hand edit: new content and a modification time of its own (two writes within one clock tick can share one).
fn edit(p: &Path, text: &str) {
    static STEP: AtomicU32 = AtomicU32::new(1);
    std::fs::write(p, text).unwrap();
    let t = SystemTime::now() + Duration::from_secs(3_600 + u64::from(STEP.fetch_add(1, Ordering::SeqCst)));
    std::fs::File::options().write(true).open(p).unwrap().set_modified(t).unwrap();
}

#[test]
fn comments_are_stripped_outside_strings() {
    let t = "// hi\n{ /* b */ \"names\": {\"a\": \"x // y \\\" /* z */\"} } // end";
    let v: Value = serde_json::from_str(&strip_comments(t)).unwrap();
    assert_eq!(v["names"]["a"], "x // y \" /* z */");
}

#[test]
fn parse_bom_and_invalid() {
    let parse = |t: &str| serde_json::from_str::<Value>(&strip_comments(strip_bom(t)));
    assert!(parse("\u{feff}{}").is_ok());
    assert!(parse("{ not json").is_err());
}

#[test]
fn a_file_with_a_bom_loads() {
    let dir = TempDir::new("bom");
    std::fs::write(dir.file(), format!("\u{feff}{MARK}\n{{\"n\": 8}}")).unwrap();
    let s = store(&dir);
    assert_eq!(s.load(|| None), LoadOutcome::Read);
    assert_eq!(s.get().n, 8);
}

#[test]
fn load_creates_a_missing_file() {
    static SETS: AtomicU32 = AtomicU32::new(0);
    let dir = TempDir::new("create");
    let spec = FileSpec { path: dir.file(), kind: FileKind::PerUser, cap: 64 * 1024 };
    let s: Store<TestCfg> = Store::new(Some(spec), |_| {
        SETS.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(*s.get(), TestCfg::default());
    assert_eq!(s.load(|| None), LoadOutcome::Created);
    assert_eq!(SETS.load(Ordering::SeqCst), 1);
    assert_eq!(read(&dir.file()), TestCfg::default().to_template());
    assert_eq!(*s.get(), TestCfg::default());
    // The legacy import is used once, when the file is absent.
    let dir2 = TempDir::new("legacy");
    let s2 = store(&dir2);
    let legacy = TestCfg { n: 42, ..TestCfg::default() };
    assert_eq!(s2.load(|| Some(legacy.clone())), LoadOutcome::Created);
    assert_eq!(*s2.get(), legacy);
    assert_eq!(store(&dir2).load(|| unreachable!("the file exists: no import")), LoadOutcome::Read);
    // No file at all: the defaults, nothing is written.
    let none: Store<TestCfg> = Store::new(None, |_| {});
    assert_eq!(none.load(|| unreachable!()), LoadOutcome::NoFolder);
    assert_eq!(none.save(&legacy), Err(WriteError::NotWritable));
    assert_eq!(none.reload_if_changed(), Reload::Unchanged);
    assert!(!none.external_edit_pending());
}

#[test]
fn load_rewrites_a_file_without_the_mark_once() {
    let dir = TempDir::new("mark");
    std::fs::write(dir.file(), "{\"a\": false, \"n\": 3}").unwrap();
    let s = store(&dir);
    assert_eq!(s.load(|| None), LoadOutcome::Rewritten);
    let want = TestCfg { a: false, n: 3, ..TestCfg::default() };
    assert_eq!(*s.get(), want);
    let text = read(&dir.file());
    assert_eq!(text, want.to_template());
    assert_eq!(store(&dir).load(|| None), LoadOutcome::Read);
    assert_eq!(read(&dir.file()), text);
}

#[test]
fn a_migration_rewrites_the_file_once() {
    let dir = TempDir::new("migrate");
    std::fs::write(dir.file(), format!("{MARK}\n{{\"count\": 3}}")).unwrap();
    let s = store(&dir);
    assert_eq!(s.load(|| None), LoadOutcome::Rewritten);
    assert_eq!(s.get().n, 3);
    let text = read(&dir.file());
    assert!(text.contains("\"n\": 3") && !text.contains("count"), "{text}");
    assert_eq!(store(&dir).load(|| None), LoadOutcome::Read);
}

#[test]
fn an_invalid_file_keeps_defaults_and_is_left_untouched() {
    for bad in ["{ \"a\": fal", "[1, 2]", "{\"n\": \"three\"}", "\u{feff}// c\n{ \"a\": true,, }", "\u{0}\u{ff}"] {
        let dir = TempDir::new("invalid");
        std::fs::write(dir.file(), bad).unwrap();
        let s = store(&dir);
        assert!(matches!(s.load(|| None), LoadOutcome::Invalid(_)), "{bad:?}");
        assert_eq!(*s.get(), TestCfg::default());
        assert_eq!(std::fs::read(dir.file()).unwrap(), bad.as_bytes());
        let errors = s.errors();
        assert_eq!(errors.len(), 1, "{bad:?}");
        assert_eq!(errors[0].kept, Kept::Default);
        assert!(matches!(errors[0].problem, Problem::Parse(_)));
    }
    // A syntax error keeps its position: in the error, and in the text the dock has always logged.
    let dir = TempDir::new("position");
    std::fs::write(dir.file(), "{\n  \"a\": true\n  \"n\": 1\n}").unwrap();
    let s = store(&dir);
    let outcome = s.load(|| None);
    assert_eq!(outcome, LoadOutcome::Invalid("expected `,` or `}` at line 3 column 3".into()));
    assert_eq!((s.errors()[0].line, s.errors()[0].col), (3, 3));
    // Bytes that are not UTF-8 cannot be read at all.
    let dir = TempDir::new("utf8");
    std::fs::write(dir.file(), [b'{', 0xff, b'}']).unwrap();
    assert!(matches!(store(&dir).load(|| None), LoadOutcome::Unreadable(_)));
    assert_eq!(std::fs::read(dir.file()).unwrap(), [b'{', 0xff, b'}']);
}

#[test]
fn reload_sees_an_edit_and_ignores_our_own_write() {
    let dir = TempDir::new("reload");
    let s = store(&dir);
    assert_eq!(s.load(|| None), LoadOutcome::Created);
    assert_eq!(s.reload_if_changed(), Reload::Unchanged);
    s.save(&TestCfg { n: 1, ..TestCfg::default() }).unwrap();
    assert!(!s.external_edit_pending());
    assert_eq!(s.reload_if_changed(), Reload::Unchanged);
    edit(&dir.file(), &format!("{MARK}\n{{\"n\": 2}}"));
    assert!(s.external_edit_pending());
    assert_eq!(s.reload_if_changed(), Reload::Changed);
    assert_eq!(s.get().n, 2);
    assert!(!s.external_edit_pending());
    assert_eq!(s.reload_if_changed(), Reload::Unchanged);
    // An invalid edit keeps the snapshot and says so.
    edit(&dir.file(), "{ \"n\": ");
    assert!(matches!(s.reload_if_changed(), Reload::Invalid(_)));
    assert_eq!(s.get().n, 2);
    assert_eq!(s.errors()[0].kept, Kept::LastGood);
    // A good edit clears the errors.
    edit(&dir.file(), &format!("{MARK}\n{{\"n\": 3}}"));
    assert_eq!(s.reload_if_changed(), Reload::Changed);
    assert!(s.errors().is_empty());
    // An edit that changes nothing the options read.
    edit(&dir.file(), &format!("{MARK}\n{{ \"n\": 3 }}"));
    assert_eq!(s.reload_if_changed(), Reload::Unchanged);
    std::fs::remove_file(dir.file()).unwrap();
    assert_eq!(s.reload_if_changed(), Reload::Removed);
    assert_eq!(s.get().n, 3);
    // The next write creates it again.
    s.modify(|c| c.n = 4).unwrap();
    assert_eq!(read(&dir.file()), s.get().to_template());
}

#[test]
fn modify_backs_up_an_unparsable_file_first() {
    let dir = TempDir::new("backup");
    let s = store(&dir);
    s.load(|| None);
    for broken in [format!("{MARK}\n{{ \"n\": 5, oops }}"), format!("{MARK}\n{{ \"n\": \"five\" }}")] {
        edit(&dir.file(), &broken);
        assert!(matches!(s.reload_if_changed(), Reload::Invalid(_)));
        s.modify(|c| c.n += 1).unwrap();
        assert_eq!(read(&dir.bak()), broken);
        assert_eq!(read(&dir.file()), s.get().to_template());
        assert!(s.errors().is_empty());
    }
    assert_eq!(s.get().n, 9);
    // A clean file is not copied again.
    std::fs::remove_file(dir.bak()).unwrap();
    s.modify(|c| c.n = 10).unwrap();
    assert!(!dir.bak().exists());
    // Our own write refreshed the remembered time: nothing to reload.
    assert_eq!(s.reload_if_changed(), Reload::Unchanged);
}

#[test]
fn unknown_keys_survive_a_save() {
    let dir = TempDir::new("unknown");
    let text = format!("{MARK}\n{{\"n\": 4, \"zeta\": {{\"x\": [1, 2]}}, \"alpha\": \"new\"}}");
    std::fs::write(dir.file(), text).unwrap();
    let s = store(&dir);
    assert_eq!(s.load(|| None), LoadOutcome::Read);
    s.modify(|c| c.a = false).unwrap();
    let written = read(&dir.file());
    let tail =
        "  \"s\": \"seven\",\n\n  // Kept from a newer version of the dock. Leave as is.\n  \"alpha\": \"new\",\n  \
                \"zeta\": {\"x\":[1,2]}\n}\n";
    assert!(written.ends_with(tail), "{written}");
    let again = store(&dir);
    assert_eq!(again.load(|| None), LoadOutcome::Read);
    assert_eq!(*again.get(), *s.get());
}

#[test]
fn export_lists_what_differs_from_the_defaults() {
    let dir = TempDir::new("export");
    let s = store(&dir);
    s.load(|| None);
    assert_eq!(s.export(), "{\"version\":1}");
    s.modify(|c| c.n = 1).unwrap();
    assert_eq!(s.export(), "{\"n\":1,\"version\":1}");
}

#[test]
fn a_write_error_names_the_file_not_the_user_folder() {
    let dir = TempDir::new("private");
    let s = store(&dir);
    assert_eq!(s.load(|| None), LoadOutcome::Created);
    let temp = std::env::temp_dir().display().to_string();
    let private = temp.trim_end_matches(['\\', '/']);
    // A folder where the temp file goes: write_atomic cannot remove it, and its error names the path.
    std::fs::create_dir(dir.0.join("test.json.tmp")).unwrap();
    let r = s.save(&TestCfg { n: 1, ..TestCfg::default() });
    let Err(WriteError::Io(e)) = &r else { panic!("the write must fail: {r:?}") };
    assert!(e.contains("test.json.tmp") && !e.contains(private), "{e}");
    std::fs::remove_dir(dir.0.join("test.json.tmp")).unwrap();
    // The same for the backup of a file with errors.
    edit(&dir.file(), "{ \"n\": ");
    std::fs::create_dir(dir.0.join("test.json.bak.tmp")).unwrap();
    let r = s.modify(|c| c.n = 2);
    let Err(WriteError::Backup(e)) = &r else { panic!("the backup must fail: {r:?}") };
    assert!(e.contains("test.json.bak.tmp") && !e.contains(private), "{e}");
}

/// The first message `msg` posted to `hwnd` within `limit` (its wParam), or None.
fn wait_for(hwnd: windows::Win32::Foundation::HWND, msg: u32, limit: Duration) -> Option<usize> {
    use windows::Win32::UI::WindowsAndMessaging::{PeekMessageW, MSG, PM_REMOVE};
    let start = Instant::now();
    loop {
        let mut m = MSG::default();
        // SAFETY: `m` is a valid MSG; `hwnd` is a window of this thread.
        if unsafe { PeekMessageW(&mut m, Some(hwnd), msg, msg, PM_REMOVE) }.as_bool() {
            return Some(m.wParam.0);
        }
        if start.elapsed() > limit {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn watch_posts_once_per_change() {
    use windows::core::w;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP,
    };
    const MSG: u32 = WM_APP + 7;
    let dir = TempDir::new("watch");
    let s: &'static Store<TestCfg> = Box::leak(Box::new(store(&dir)));
    s.load(|| None);
    // SAFETY: a message-only STATIC window of this thread, destroyed below.
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            w!(""),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            None,
            None,
        )
    }
    .unwrap();
    assert!(!s.watching());
    s.watch(WatchNotify { hwnd: hwnd.0 as isize, msg: MSG }).unwrap();
    assert!(s.watching());
    // A hand edit is posted (wParam 0); the UI then reloads it.
    edit(&dir.file(), &format!("{MARK}\n{{\"n\": 11}}"));
    assert_eq!(wait_for(hwnd, MSG, Duration::from_secs(10)), Some(0));
    assert_eq!(s.reload_if_changed(), Reload::Changed);
    // Posts the callback made before the reload are still queued: drop them.
    while wait_for(hwnd, MSG, Duration::from_millis(300)).is_some() {}
    // Our own write is never posted, nor is a write of another file in the folder.
    s.save(&TestCfg { n: 12, ..TestCfg::default() }).unwrap();
    std::fs::write(dir.0.join("other.log"), "a line").unwrap();
    assert_eq!(wait_for(hwnd, MSG, Duration::from_millis(1_500)), None);
    assert!(s.unwatch());
    assert!(!s.watching());
    assert!(!s.unwatch());
    // SAFETY: the window created above, by this thread.
    unsafe { DestroyWindow(hwnd) }.unwrap();
}

#[test]
fn locate_finds_top_and_nested_keys() {
    let t = "// \"a\": 1\n{\n  \"s\": \"\\\"a\\\": 2\",\n  /* \"a\": 3 */\n  \"a\": 4,\n  \"o\": { \"a\": 5, \"l\": [ { \"a\": 6 } ], \
             \"e\\u0073c\": 7 }\n}";
    assert_eq!(locate(t, "s"), Some((3, 3)));
    assert_eq!(locate(t, "a"), Some((5, 3)));
    assert_eq!(locate(t, "o"), Some((6, 3)));
    assert_eq!(locate(t, "o.a"), Some((6, 10)));
    assert_eq!(locate(t, "o.esc"), Some((6, 39)));
    assert_eq!(locate(t, "o.l"), Some((6, 18)));
    assert_eq!(locate(t, "o.l.a"), None);
    assert_eq!(locate(t, "l"), None);
    assert_eq!(locate(t, "zz"), None);
    assert_eq!(locate(t, ""), None);
    // A value that looks like a key is not one.
    assert_eq!(locate("{\"k\": \"a\", \"a\": 1}", "a"), Some((1, 12)));
    // Columns count characters, not bytes.
    assert_eq!(locate("{\"\u{e9}\": 1, \"b\": 2}", "b"), Some((1, 10)));
}

#[test]
fn log_line_reads_like_a_sentence() {
    let e = |path: &str, line: u32, problem: Problem, kept: Kept| ConfigError {
        path: path.to_string(),
        line,
        col: 1,
        problem,
        kept,
    };
    let range = Problem::Range { min: 14, max: 32, got: 40 };
    assert_eq!(
        e("app_icon_size", 14, range, Kept::LastGood).log_line(),
        "line 14, app_icon_size: 40 is above the maximum 32 (kept the last good value)"
    );
    let low = Problem::Range { min: 14, max: 32, got: 2 };
    assert_eq!(e("a", 0, low, Kept::Default).log_line(), "a: 2 is below the minimum 14 (kept the default)");
    let odd = Problem::Range { min: 14, max: 32, got: 20 };
    assert_eq!(e("a", 0, odd, Kept::Default).log_line(), "a: 20 is outside 14..32 (kept the default)");
    let ty = Problem::Type { expected: "a number" };
    assert_eq!(e("n", 2, ty, Kept::Default).log_line(), "line 2, n: expected a number (kept the default)");
    let value = Problem::UnknownValue { allowed: &["top", "bottom"] };
    assert_eq!(e("p", 0, value, Kept::LastGood).log_line(), "p: not one of top, bottom (kept the last good value)");
    let long = Problem::TooLong { max: 64 };
    assert_eq!(e("t", 0, long, Kept::Default).log_line(), "t: longer than 64 characters (kept the default)");
    let parse = Problem::Parse("expected value".into());
    assert_eq!(e("", 1, parse, Kept::Default).log_line(), "line 1, expected value (kept the default)");
}

/// xorshift64: the fuzz inputs, seeded, so a failure repeats.
fn next(s: &mut u64) -> u64 {
    *s ^= *s << 13;
    *s ^= *s >> 7;
    *s ^= *s << 17;
    *s
}

/// JSONC-like texts: strip_comments, strip_bom and locate are total over them, and the stripped text of a valid
/// JSONC text keeps its line count.
fn fuzz_jsonc(runs: u32) {
    const PIECES: [&str; 26] = [
        "{", "}", "[", "]", "\"", "\\", "/", "*", ":", ",", "\n", " ", "a", "o", "1", "true", "\"a\"", "\"o\"", "//",
        "/*", "*/", "\u{feff}", "\u{e9}", "\\u00", "\r\n", "\t",
    ];
    let mut s = 0x2545_f491_4f6c_dd1du64;
    for _ in 0..runs {
        let n = next(&mut s) % 80;
        let mut t = String::new();
        for _ in 0..n {
            let r = next(&mut s);
            t.push_str(PIECES.get((r % PIECES.len() as u64) as usize).copied().unwrap_or(""));
        }
        let stripped = strip_comments(strip_bom(&t));
        assert!(stripped.len() <= t.len() * 2 + 1);
        for path in ["a", "o.a", "o.a.a", "", "."] {
            if let Some((line, col)) = locate(&t, path) {
                assert!(line >= 1 && col >= 1, "{t:?} {path}");
            }
        }
        if serde_json::from_str::<Value>(&stripped).is_ok() {
            assert_eq!(stripped.matches('\n').count(), strip_bom(&t).matches('\n').count(), "{t:?}");
        }
    }
}

#[test]
fn fuzz_jsonc_10k() {
    fuzz_jsonc(10_000);
}

#[test]
#[ignore = "nightly fuzz: 1,000,000 inputs"]
fn fuzz_jsonc_1m() {
    fuzz_jsonc(1_000_000);
}
