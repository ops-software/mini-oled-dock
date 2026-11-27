//! The user's Tasks options, %APPDATA%\mini-oled-dock\tasks.json (F-T2): typed fields, their defaults and a
//! hand-written oled_config::Options. No options page in the beta (open point 16): the More menu sets the four keys and
//! the template's guide lines document them. load (the Store on its file) is TA-T1b's.

use std::sync::Arc;

use oled_config::{
    jsonc, ConfigError, Descriptor, FileKind, FileSpec, Kept, LoadOutcome, MigrateError, Options, Problem, Store,
};
use serde_json::Value;

use super::paths::Paths;
use crate::engine::cadence::Refresh;

/// The refresh speed of a first start (open point 20: 2 s in a one-line commit if SP-T1 fails on this PC class).
pub const DEFAULT_REFRESH: Refresh = Refresh::One;

/// "standard" | "compact".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Density {
    #[default]
    Standard,
    Compact,
}

/// "auto" | "cpu" | "memory".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OpenSort {
    #[default]
    Auto,
    Cpu,
    Memory,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TasksOptions {
    /// "0.5" | "1" | "2" | "5" | "paused".
    pub refresh: Refresh,
    pub density: Density,
    pub always_on_top: bool,
    pub open_sorted_by: OpenSort,
}

impl Default for TasksOptions {
    fn default() -> TasksOptions {
        TasksOptions {
            refresh: DEFAULT_REFRESH,
            density: Density::Standard,
            always_on_top: false,
            open_sorted_by: OpenSort::Auto,
        }
    }
}

const REFRESH: [(&str, Refresh); 5] = [
    ("0.5", Refresh::Half),
    ("1", Refresh::One),
    ("2", Refresh::Two),
    ("5", Refresh::Five),
    ("paused", Refresh::Paused),
];
const REFRESH_NAMES: &[&str] = &["0.5", "1", "2", "5", "paused"];
const DENSITY: [(&str, Density); 2] = [("standard", Density::Standard), ("compact", Density::Compact)];
const DENSITY_NAMES: &[&str] = &["standard", "compact"];
const SORT: [(&str, OpenSort); 3] = [("auto", OpenSort::Auto), ("cpu", OpenSort::Cpu), ("memory", OpenSort::Memory)];
const SORT_NAMES: &[&str] = &["auto", "cpu", "memory"];

impl Options for TasksOptions {
    const VERSION: u16 = 1;
    const FILE: &'static str = "tasks.json";

    fn descriptors() -> &'static [Descriptor] {
        &[]
    }

    fn known_keys() -> &'static [&'static str] {
        &["version", "refresh", "density", "always_on_top", "open_sorted_by"]
    }

    /// Key by key: a wrong type is Problem::Type, an unknown string Problem::UnknownValue; the key keeps the last good
    /// value when there is one, else the default. A missing key takes the default without an error.
    fn from_value(v: &Value, text: &str, last_good: Option<&Self>) -> (Self, Vec<ConfigError>) {
        let kept = if last_good.is_some() { Kept::LastGood } else { Kept::Default };
        let base = last_good.cloned().unwrap_or_default();
        let mut errors = Vec::new();
        let mut r = Reader { v, text, kept, errors: &mut errors };
        let out = TasksOptions {
            refresh: r.named("refresh", &REFRESH, REFRESH_NAMES).unwrap_or(base.refresh),
            density: r.named("density", &DENSITY, DENSITY_NAMES).unwrap_or(base.density),
            always_on_top: r.bool("always_on_top").unwrap_or(base.always_on_top),
            open_sorted_by: r.named("open_sorted_by", &SORT, SORT_NAMES).unwrap_or(base.open_sorted_by),
        };
        (out, errors)
    }

    fn to_template(&self) -> String {
        let mut s = String::from(Self::template_mark());
        s.push_str("\n// Read when the window opens. The More menu writes the same keys.\n{\n  \"version\": 1,\n");
        s.push_str("  // How often the list refreshes: \"0.5\", \"1\", \"2\" or \"5\" seconds, or \"paused\".\n");
        s.push_str(&format!("  \"refresh\": \"{}\",\n", name_of(&REFRESH, self.refresh)));
        s.push_str("  // Row height: \"standard\" or \"compact\".\n");
        s.push_str(&format!("  \"density\": \"{}\",\n", name_of(&DENSITY, self.density)));
        s.push_str("  // Keep the window above other windows: true or false.\n");
        s.push_str(&format!("  \"always_on_top\": {},\n", self.always_on_top));
        s.push_str("  // The column the list opens sorted by: \"auto\", \"cpu\" or \"memory\".\n");
        s.push_str(&format!("  \"open_sorted_by\": \"{}\"\n}}\n", name_of(&SORT, self.open_sorted_by)));
        s
    }

    fn template_mark() -> &'static str {
        "// mini-oled-dock: tasks options"
    }

    fn migrations() -> &'static [fn(&mut Value) -> Result<(), MigrateError>] {
        &[]
    }
}

/// The name of a value in its table.
fn name_of<T: Copy + PartialEq>(table: &[(&'static str, T)], v: T) -> &'static str {
    table.iter().find(|(_, x)| *x == v).map_or("", |(n, _)| *n)
}

/// Reads one key at a time and records its error.
struct Reader<'a> {
    v: &'a Value,
    text: &'a str,
    kept: Kept,
    errors: &'a mut Vec<ConfigError>,
}

impl Reader<'_> {
    /// One of `table`'s names; None when missing or wrong (the error recorded).
    fn named<T: Copy>(&mut self, key: &str, table: &[(&str, T)], allowed: &'static [&'static str]) -> Option<T> {
        let raw = self.v.get(key)?;
        let Some(s) = raw.as_str() else {
            self.error(key, Problem::Type { expected: "a string" });
            return None;
        };
        let found = table.iter().find(|(n, _)| *n == s).map(|(_, x)| *x);
        if found.is_none() {
            self.error(key, Problem::UnknownValue { allowed });
        }
        found
    }

    fn bool(&mut self, key: &str) -> Option<bool> {
        let raw = self.v.get(key)?;
        let b = raw.as_bool();
        if b.is_none() {
            self.error(key, Problem::Type { expected: "true or false" });
        }
        b
    }

    fn error(&mut self, key: &str, problem: Problem) {
        let (line, col) = jsonc::locate(self.text, key).unwrap_or((0, 0));
        self.errors.push(ConfigError { path: key.to_string(), line, col, problem, kept: self.kept });
    }
}

/// The file's text read the way the Store reads it (comments and BOM stripped, then from_value with no last good
/// value). A text that is not a JSON object gives the defaults and one Problem::Parse.
pub fn parse_text(text: &str) -> (TasksOptions, Vec<ConfigError>) {
    let text = jsonc::strip_bom(text);
    match serde_json::from_str::<Value>(&jsonc::strip_comments(text)) {
        Ok(v) if v.is_object() => TasksOptions::from_value(&v, text, None),
        Ok(_) => (TasksOptions::default(), vec![whole_file("the file is not a JSON object".to_string(), 0, 0)]),
        Err(e) => {
            let (line, col) =
                (u32::try_from(e.line()).unwrap_or(u32::MAX), u32::try_from(e.column()).unwrap_or(u32::MAX));
            (TasksOptions::default(), vec![whole_file(e.to_string(), line, col)])
        }
    }
}

fn whole_file(message: String, line: u32, col: u32) -> ConfigError {
    ConfigError { path: String::new(), line, col, problem: Problem::Parse(message), kept: Kept::Default }
}

/// The Store's on_set: nothing to apply outside the window.
pub fn on_set(_: &TasksOptions) {}

/// The file's size cap.
pub(crate) const FILE_CAP: u64 = 64 * 1024;

/// The Store on %APPDATA%\mini-oled-dock\tasks.json, loaded (no legacy file). Without a roaming folder it holds the
/// defaults and never writes. Keys this version does not know stay in the file (the Store's rule).
pub fn load(p: &Paths) -> Arc<Store<TasksOptions>> {
    let spec = p.roaming.as_ref().map(|dir| FileSpec {
        path: dir.join(TasksOptions::FILE),
        kind: FileKind::PerUser,
        cap: FILE_CAP,
    });
    let store = Store::new(spec, on_set);
    let outcome = store.load(|| None);
    oled_sys::log!("tasks: options {}", outcome_word(&outcome));
    Arc::new(store)
}

/// A word for the log: no path, no text of the file.
fn outcome_word(o: &LoadOutcome) -> &'static str {
    match o {
        LoadOutcome::Read => "read",
        LoadOutcome::Rewritten => "rewritten",
        LoadOutcome::Created => "created",
        LoadOutcome::Invalid(_) => "invalid",
        LoadOutcome::Unreadable(_) => "unreadable",
        LoadOutcome::NoFolder => "no folder",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_reads_back_as_itself() {
        let o = TasksOptions {
            refresh: Refresh::Half,
            density: Density::Compact,
            always_on_top: true,
            open_sorted_by: OpenSort::Memory,
        };
        let text = o.to_template();
        assert!(text.starts_with(TasksOptions::template_mark()));
        assert_eq!(parse_text(&text), (o, Vec::new()));
        assert_eq!(parse_text(&TasksOptions::default().to_template()), (TasksOptions::default(), Vec::new()));
    }

    fn temp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("tasks-options-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn options_store_roundtrip() {
        let d = temp("rt");
        let paths = Paths { roaming: Some(d.clone()), local: None };
        let first = load(&paths);
        assert_eq!(*first.get(), TasksOptions::default());
        assert!(d.join("tasks.json").exists(), "the template was created");
        first
            .modify(|o| {
                o.refresh = Refresh::Five;
                o.density = Density::Compact;
                o.always_on_top = true;
                o.open_sorted_by = OpenSort::Cpu;
            })
            .unwrap();
        let second = load(&paths);
        assert_eq!(
            *second.get(),
            TasksOptions {
                refresh: Refresh::Five,
                density: Density::Compact,
                always_on_top: true,
                open_sorted_by: OpenSort::Cpu
            }
        );
        // No roaming folder: defaults, nothing written.
        let none = load(&Paths::default());
        assert_eq!(*none.get(), TasksOptions::default());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn wrong_values_keep_the_last_good_one() {
        let good = TasksOptions { refresh: Refresh::Five, ..TasksOptions::default() };
        let text = "{\n  \"refresh\": 2,\n  \"always_on_top\": \"yes\",\n  \"density\": \"compact\"\n}";
        let v: Value = serde_json::from_str(text).unwrap();
        let (o, errors) = TasksOptions::from_value(&v, text, Some(&good));
        assert_eq!(o, TasksOptions { density: Density::Compact, ..good });
        assert_eq!(errors.len(), 2);
        assert_eq!((errors[0].path.as_str(), errors[0].line, errors[0].kept), ("refresh", 2, Kept::LastGood));
        assert_eq!(errors[0].problem, Problem::Type { expected: "a string" });
        assert_eq!(errors[1].problem, Problem::Type { expected: "true or false" });
        assert_eq!(parse_text("[1]").1.len(), 1);
        assert!(matches!(parse_text("{").1[0].problem, Problem::Parse(_)));
    }
}
