//! files.json (F1a.19): the Store over a FileSpec in a TempRoot. No test here resolves the real folder.

mod common;

use std::ffi::OsString;
use std::path::PathBuf;

use common::temp::TempRoot;
use files_engine::options::{self, ColumnPref, FilesOptions};
use oled_config::{FileKind, FileSpec, LoadOutcome, Options, Store};

fn store(root: &TempRoot) -> (Store<FilesOptions>, PathBuf) {
    let path = root.at("files.json");
    let spec = FileSpec { path: path.clone(), kind: FileKind::PerUser, cap: 64 * 1024 };
    (Store::new(Some(spec), |_| {}), path)
}

#[test]
fn defaults_without_a_file() {
    let root = TempRoot::new("opt_defaults");
    let (s, path) = store(&root);
    assert_eq!(s.load(|| None), LoadOutcome::Created);
    let o = s.get();
    assert_eq!(*o, FilesOptions::default());
    assert!(o.show_extensions && o.sidebar_visible && o.downloads_by_date);
    assert!(!o.sizes_like_explorer && !o.full_dates && !o.hidden_items && !o.protected_system && !o.compact);
    assert_eq!((o.sidebar_width, o.new_tab.as_str()), (200, "home"));
    assert!(o.columns.is_empty() && o.pins.is_empty() && !o.pins_imported);
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text, FilesOptions::default().to_template());
    assert!(s.errors().is_empty());
}

#[test]
fn unknown_keys_kept() {
    let root = TempRoot::new("opt_unknown");
    let (s, path) = store(&root);
    root.file("files.json", b"{ \"compact\": true, \"from_the_future\": {\"a\": [1, 2]} }");
    s.load(|| None);
    let o = s.get();
    assert!(o.compact);
    assert!(o.extra.contains_key("from_the_future"));
    assert!(s.modify(|c| c.hidden_items = true).is_ok());
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("\"from_the_future\""), "{text}");
    let (again, _) = store(&root);
    again.load(|| None);
    assert!(again.get().hidden_items && again.get().compact);
    assert!(again.get().extra.contains_key("from_the_future"));
}

#[test]
fn bad_value_keeps_default_and_reports() {
    let root = TempRoot::new("opt_bad");
    let (s, path) = store(&root);
    let bytes = b"{\n  \"compact\": true,\n  \"sidebar_width\": \"wide\"\n}";
    root.file("files.json", bytes);
    // One bad value loses only its own key (P2-plumbing W5d): the rest is read, that key keeps its default.
    assert_eq!(s.load(|| None), LoadOutcome::Read);
    assert_eq!(*s.get(), FilesOptions { compact: true, ..FilesOptions::default() }, "only sidebar_width is lost");
    let errors = s.errors();
    assert_eq!(errors.len(), 1);
    assert_eq!((errors[0].path.as_str(), errors[0].line), ("sidebar_width", 3));
    assert_eq!(std::fs::read(&path).unwrap(), bytes, "a file with an error is left as it is");
    // A broken edit after a good load keeps the last good value.
    let good = TempRoot::new("opt_bad_last");
    let (g, _) = store(&good);
    g.load(|| None);
    assert!(g.modify(|c| c.compact = true).is_ok());
    good.write("files.json", b"{ \"compact\": 7 }");
    let _ = g.reload_if_changed();
    assert!(g.get().compact, "the last good value stays");
}

#[test]
fn round_trip() {
    let root = TempRoot::new("opt_round");
    let (s, _) = store(&root);
    s.load(|| None);
    let want = FilesOptions {
        show_extensions: false,
        sizes_like_explorer: true,
        full_dates: true,
        hidden_items: true,
        protected_system: true,
        compact: true,
        sidebar_visible: false,
        sidebar_width: 333,
        new_tab: "current".into(),
        downloads_by_date: false,
        columns: vec![
            ColumnPref { id: "name".into(), width: 280, visible: true },
            ColumnPref { id: "size".into(), width: 90, visible: false },
        ],
        pins: vec!["C:\\a \"quoted\" \\ folder".into(), "D:\\b".into()],
        pins_imported: true,
        ..FilesOptions::default()
    };
    assert!(s.modify(|c| *c = want.clone()).is_ok());
    let (back, _) = store(&root);
    assert_eq!(back.load(|| None), LoadOutcome::Read);
    assert_eq!(*back.get(), want);
    // The text itself parses to the same value.
    assert_eq!(options::parse_file(want.to_template().as_bytes()), Ok(want));
}

#[test]
fn parse_file_is_read_only() {
    let root = TempRoot::new("opt_parse");
    let bytes = b"// mine\n{ \"sidebar_width\": 250, \"x\": 1 }".to_vec();
    let path = root.file("files.json", &bytes);
    let before = root.hash("files.json");
    let o = options::parse_file(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(o.sidebar_width, 250);
    assert_eq!(root.hash("files.json"), before);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1, "nothing was created");
    assert!(options::parse_file(b"{ \"compact\": 1 }").is_err());
    assert!(options::parse_file(b"{ not json").is_err());
    assert_eq!(options::parse_file(b"\xEF\xBB\xBF{}"), Ok(FilesOptions::default()));
}

#[test]
fn config_dir_override() {
    // The pure choice: the override counts in a debug build only, and only when absolute.
    let dir = std::env::temp_dir().join("files-opt-pure");
    let roaming = || Some(PathBuf::from(r"C:\Roaming"));
    let over = || Some(OsString::from(&dir));
    let s = options::spec_in(true, over(), roaming).unwrap();
    assert_eq!((s.path, s.kind, s.cap), (dir.join("files.json"), FileKind::PerUser, 64 * 1024));
    let s = options::spec_in(false, over(), roaming).unwrap();
    assert_eq!(s.path, PathBuf::from(r"C:\Roaming\mini-oled-dock\files.json"));
    assert!(options::spec_in(true, Some(OsString::from("relative")), roaming).is_none());
    assert_eq!(
        options::spec_in(true, Some(OsString::new()), roaming).unwrap().path,
        PathBuf::from(r"C:\Roaming\mini-oled-dock\files.json")
    );
    assert!(options::spec_in(true, None, || None).is_none());
    // The child process half (spec() itself, with the variable in a child's environment) is the unit test
    // options::tests::config_dir_override in src/options.rs: a test file never calls the real resolver.
}
