//! PF1: one line per frozen item of the plumbing plan (F-P1, F-P2) with its full signature, so a change fails to
//! compile.
// Function-pointer types spelled out in full are the point of this file.
#![allow(clippy::type_complexity)]

use std::path::{Path, PathBuf};

use oled_config::theme::*;
use oled_config::*;

#[test]
fn jsonc_items_keep_their_signatures() {
    let _a: fn(&str) -> String = jsonc::strip_comments;
    let _b: fn(&str) -> &str = jsonc::strip_bom;
    let _c: fn(&str, &str) -> Option<(u32, u32)> = jsonc::locate;
    let _d: fn(&ConfigError) -> String = ConfigError::log_line;
    let _e: fn(&[Descriptor], &dyn Fn(TextFn) -> &'static str) -> Vec<String> = lint;
    let _ = (Kept::LastGood, Kept::Default);
    let _ = Problem::Range { min: 0, max: 0, got: 0 };
    let _ = (
        Problem::Parse(String::new()),
        Problem::Type { expected: "" },
        Problem::UnknownValue { allowed: &[] },
        Problem::TooLong { max: 0 },
    );
    let _ = (ValueType::Bool, Unit::Px, Scope::PerApp, Takeover::FolderVerbs, PageId(""), SecId(""));
    let _ = (
        Range { min: 0, max: 0, step: 1, unit: Unit::None },
        Cond { json: "", equals: "" },
        CostNote { kib: 0, wakeups_milli_per_s: 0 },
    );
    let _ = (Control::Toggle, Control::Slider, Control::Text { max: 0 }, Control::Custom(""));
}

/// A type that satisfies the Options trait exactly as written (no default methods).
#[derive(Clone, Default, PartialEq)]
struct Probe;

impl Options for Probe {
    const VERSION: u16 = 1;
    const FILE: &'static str = "probe.json";
    fn descriptors() -> &'static [Descriptor] {
        &[]
    }
    fn known_keys() -> &'static [&'static str] {
        &[]
    }
    fn from_value(_: &serde_json::Value, _: &str, _: Option<&Self>) -> (Self, Vec<ConfigError>) {
        (Probe, Vec::new())
    }
    fn to_template(&self) -> String {
        String::new()
    }
    fn template_mark() -> &'static str {
        ""
    }
    fn migrations() -> &'static [fn(&mut serde_json::Value) -> Result<(), MigrateError>] {
        &[]
    }
}

/// The two generic methods cannot be named as fn pointers; calling them in a function that never runs pins them.
#[allow(dead_code)]
fn generic_store_methods(s: &Store<Probe>) {
    let _: LoadOutcome = s.load(|| None);
    let _: Result<(), WriteError> = s.modify(|_c: &mut Probe| {});
}

#[test]
fn store_items_keep_their_signatures() {
    let _a: fn(Option<FileSpec>, fn(&Probe)) -> Store<Probe> = Store::<Probe>::new;
    let _c: fn(&Store<Probe>) -> std::sync::Arc<Probe> = Store::<Probe>::get;
    let _d: fn(&Store<Probe>, Probe) = Store::<Probe>::set;
    let _e: fn(&Store<Probe>) -> std::sync::Arc<[ConfigError]> = Store::<Probe>::errors;
    let _g: fn(&Store<Probe>, &Probe) -> Result<(), WriteError> = Store::<Probe>::save;
    let _h: fn(&Store<Probe>) -> Reload = Store::<Probe>::reload_if_changed;
    let _i: fn(&Store<Probe>) -> bool = Store::<Probe>::external_edit_pending;
    let _j: fn(&'static Store<Probe>, WatchNotify) -> Result<(), String> = Store::<Probe>::watch;
    let _k: fn(&Store<Probe>) -> bool = Store::<Probe>::unwatch;
    let _l: fn(&Store<Probe>) -> bool = Store::<Probe>::watching;
    let _m: fn(&Store<Probe>) -> String = Store::<Probe>::export;
    let _ = FileSpec { path: PathBuf::new(), kind: FileKind::PerUser, cap: 0 };
    let _ = (
        FileKind::AdminRootUserEditable,
        FileKind::AdminRootAdminOnly,
        WriteError::NotWritable,
        WriteError::Backup(String::new()),
        WriteError::Io(String::new()),
    );
    let _ = (
        LoadOutcome::Read,
        LoadOutcome::NoFolder,
        Reload::Unchanged,
        Reload::Removed,
        MigrateError(String::new()),
        WatchNotify { hwnd: 0, msg: 0 },
    );
}

#[test]
fn theme_items_keep_their_signatures() {
    let _: (u16, &str, &str, u64) = (THEME_SCHEMA, THEME_DIR, THEME_FILE, THEME_MAX_BYTES);
    let _a: fn(&[u8]) -> Result<(ThemeFile, Vec<String>), String> = parse;
    let _b: fn(&ThemeFile) -> String = to_json;
    let _c: fn() -> Option<PathBuf> = theme_path;
    let _d: fn() -> ThemeWatch = ThemeWatch::open;
    let _e: fn(&Path) -> ThemeWatch = ThemeWatch::open_at;
    let _f: fn(&ThemeWatch) -> &ThemeFile = ThemeWatch::current;
    let _g: fn(&ThemeWatch) -> Option<windows::Win32::Foundation::HANDLE> = ThemeWatch::change_handle;
    let _h: fn(&mut ThemeWatch) = ThemeWatch::on_signaled;
    let _i: for<'a> fn(&'a mut ThemeWatch) -> Option<&'a ThemeFile> = ThemeWatch::reload;
    let _ = (
        Rgb(0, 0, 0),
        Lang::default(),
        Preset::Custom,
        Corners::Round,
        RimStyle::Strong,
        PlateStyle::Underline,
        IndicatorStyle::Dot,
        Motion::Off,
    );
    let _ = A11y::default();
    let _: fn() -> ThemeFile = ThemeFile::default;
}
