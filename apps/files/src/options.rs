//! files.json (P3-files-app 2.2; F1a.19): the window's options in the suite's roaming folder. The struct is declared
//! once through options!; its field names are the ones FZ2 froze.

use std::ffi::OsString;
use std::path::PathBuf;
use std::rc::Rc;

use oled_config::{jsonc, options, FileKind, FileSpec, Options, Store};

use crate::model::ViewOpts;
use crate::services::OptionsService;

/// The folder of the suite under %APPDATA%.
const SUITE_DIR: &str = "mini-oled-dock";

/// Debug builds only: the config folder of a test child (TestHooks.env sets it).
pub const CONFIG_DIR_VAR: &str = "FILES_CONFIG_DIR";

/// The most files.json may hold.
const FILE_CAP: u64 = 64 * 1024;

/// One column as files.json keeps it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ColumnPref {
    pub id: String,
    pub width: i32,
    pub visible: bool,
}

fn new_tab_text(s: &FilesOptions) -> String {
    jsonc::js(&s.new_tab)
}

options! {
    /// What the window keeps between runs.
    #[derive(Eq)]
    pub struct FilesOptions(
        file: "files.json",
        version: 1,
        mark: "// mini-oled-dock: files options",
        header: "// mini-oled-dock: files options\n// Read when a window opens; the window writes the same keys when you change a setting.\n// A wrong value loses only its own key (its last good value stays); a file that does not parse is ignored as a whole. The problem is logged."
    ) {
        /// Show file name extensions.
        pub show_extensions: bool = true => {
            json: "show_extensions", slot: 0, fmt: Raw, gap: false,
            guide: "Show file name extensions: true or false.",
            since: 1, reason: "Explorer's own switch, kept per user.", ui: None,
        },
        /// Sizes in Explorer's units (KB for 1024 bytes) instead of exact units.
        pub sizes_like_explorer: bool = false => {
            json: "sizes_like_explorer", slot: 1, fmt: Raw, gap: false,
            guide: "Write sizes the way Explorer does (KB for 1024 bytes): true or false.",
            since: 1, reason: "A habit of Explorer users.", ui: None,
        },
        /// Full dates instead of the short form.
        pub full_dates: bool = false => {
            json: "full_dates", slot: 2, fmt: Raw, gap: false,
            guide: "Write the whole date and time in the Modified column: true or false.",
            since: 1, reason: "The short form hides the seconds.", ui: None,
        },
        /// Hidden items are listed.
        pub hidden_items: bool = false => {
            json: "hidden_items", slot: 3, fmt: Raw, gap: false,
            guide: "List hidden items: true or false.",
            since: 1, reason: "Explorer's own switch, kept per user.", ui: None,
        },
        /// Protected system items are listed.
        pub protected_system: bool = false => {
            json: "protected_system", slot: 4, fmt: Raw, gap: false,
            guide: "List protected operating system items too: true or false.",
            since: 1, reason: "Explorer's own switch, kept per user.", ui: None,
        },
        /// Tighter rows.
        pub compact: bool = false => {
            json: "compact", slot: 5, fmt: Raw, gap: false,
            guide: "Use tighter rows: true or false.",
            since: 1, reason: "Row density.", ui: None,
        },
        /// The sidebar is shown.
        pub sidebar_visible: bool = true => {
            json: "sidebar_visible", slot: 6, fmt: Raw, gap: true,
            guide: "Show the sidebar: true or false.",
            since: 1, reason: "The sidebar can be hidden.", ui: None,
        },
        /// 160..=400.
        pub sidebar_width: u16 = 200 => {
            json: "sidebar_width", slot: 7, fmt: Raw, gap: false,
            guide: "The sidebar's width in pixels, from 160 to 400.",
            since: 1, reason: "The sidebar can be resized.", ui: None,
        },
        /// "home" or "current".
        pub new_tab: String = "home".to_string() => {
            json: "new_tab", slot: 8, fmt: Custom(new_tab_text), gap: false,
            guide: "What a new tab opens: \"home\" or \"current\" (the folder of the tab you are in).",
            since: 1, reason: "Where a new tab starts.", ui: None,
        },
        /// Downloads sorted by date.
        pub downloads_by_date: bool = true => {
            json: "downloads_by_date", slot: 9, fmt: Raw, gap: false,
            guide: "Sort the Downloads folder by date, newest first, when you open it: true or false.",
            since: 1, reason: "What most people look for in Downloads.", ui: None,
        },
        /// Empty: view::columns::default_columns.
        pub columns: Vec<ColumnPref> = Vec::new() => {
            json: "columns", slot: 10, fmt: Js, gap: true,
            guide: "The columns in order, each {\"id\", \"width\", \"visible\"}; empty means the default columns.",
            since: 1, reason: "The table's layout.", ui: None,
        },
        /// Paths.
        pub pins: Vec<String> = Vec::new() => {
            json: "pins", slot: 11, fmt: Js, gap: false,
            guide: "The folders pinned in the sidebar, as paths.",
            since: 1, reason: "The user's pinned folders.", ui: None,
        },
        pub pins_imported: bool = false => {
            json: "pins_imported", slot: 12, fmt: Raw, gap: false,
            guide: "Set once the pins of the system's Quick access have been imported: true or false.",
            since: 1, reason: "The import runs once.", ui: None,
        },
    }
}

impl FilesOptions {
    /// What the table shows.
    pub fn view_opts(&self) -> ViewOpts {
        ViewOpts {
            show_extensions: self.show_extensions,
            sizes_like_explorer: self.sizes_like_explorer,
            full_dates: self.full_dates,
            compact: self.compact,
        }
    }
}

/// %APPDATA%\mini-oled-dock\files.json, PerUser, cap 64 KiB; FILES_CONFIG_DIR in debug builds. None: no such folder
/// (the options then stay in memory).
pub fn spec() -> Option<FileSpec> {
    spec_in(cfg!(debug_assertions), std::env::var_os(CONFIG_DIR_VAR), || {
        oled_sys::datadir::known_folder(&windows::Win32::UI::Shell::FOLDERID_RoamingAppData)
    })
}

/// The choice of the file (pure): the override in a debug build when it is an absolute path, else the roaming
/// folder's suite folder.
pub fn spec_in(debug: bool, over: Option<OsString>, roaming: impl FnOnce() -> Option<PathBuf>) -> Option<FileSpec> {
    let dir = match over.filter(|v| debug && !v.is_empty()) {
        Some(v) => Some(PathBuf::from(v)).filter(|p| p.is_absolute()),
        None => roaming().map(|r| r.join(SUITE_DIR)),
    }?;
    Some(FileSpec { path: dir.join(FilesOptions::FILE), kind: FileKind::PerUser, cap: FILE_CAP })
}

/// The selftest's read-only parse: nothing is created, written or logged. Empty input is the defaults; a file with
/// a wrong type is an error that names its line.
pub fn parse_file(bytes: &[u8]) -> Result<FilesOptions, String> {
    let text = String::from_utf8_lossy(bytes);
    let text = jsonc::strip_bom(&text);
    if text.trim().is_empty() {
        return Ok(FilesOptions::default());
    }
    let value: serde_json::Value = serde_json::from_str(&jsonc::strip_comments(text))
        .map_err(|e| format!("files.json: line {} column {}", e.line(), e.column()))?;
    let (opts, errors) = FilesOptions::from_value(&value, text, None);
    match errors.first() {
        Some(e) => Err(format!("files.json: line {}: {:?}", e.line, e.problem)),
        None => Ok(opts),
    }
}

/// The options service app.rs uses: a Store over spec(), read once now. A change is written to the file; when it
/// cannot be written the window keeps it for the session.
pub fn real_service() -> Rc<dyn OptionsService> {
    let store = Store::new(spec(), |_| {});
    store.load(|| None);
    Rc::new(Real(store))
}

struct Real(Store<FilesOptions>);

impl OptionsService for Real {
    fn get(&self) -> FilesOptions {
        (*self.0.get()).clone()
    }

    fn modify(&self, f: &mut dyn FnMut(&mut FilesOptions)) -> bool {
        if self.0.modify(|c| f(c)).is_ok() {
            return true;
        }
        let mut c = self.get();
        f(&mut c);
        self.0.set(c);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHILD: &str = "FILES_OPT_TEST_CHILD";

    /// The child half of config_dir_override: prints where spec() points. Does nothing in a normal run.
    #[test]
    fn config_dir_child() {
        if std::env::var_os(CHILD).is_some() {
            let s = spec().expect("an absolute override is a folder");
            println!("SPEC:{}", s.path.display());
        }
    }

    #[test]
    fn config_dir_override() {
        let dir = std::env::temp_dir().join(format!("files-opt-override-{}", std::process::id()));
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .creation_flags(CREATE_NO_WINDOW)
            .args(["--exact", "options::tests::config_dir_child", "--nocapture", "--test-threads=1"])
            .env(CHILD, "1")
            .env(CONFIG_DIR_VAR, &dir)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        let line = text
            .lines()
            .find_map(|l| l.split_once("SPEC:").map(|(_, p)| p))
            .unwrap_or_else(|| panic!("no answer: {text}"));
        assert_eq!(PathBuf::from(line), dir.join("files.json"));
        assert!(!dir.exists(), "spec() creates nothing");
    }
}
