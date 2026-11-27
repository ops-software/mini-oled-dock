//! The options store: load, validate, write atomically, watch.
//!
//! One `Store<C>` per config file holds what the dock's config.rs 1.5.2 kept in statics: the snapshot every thread
//! reads, the errors of the file on disk, the modification time of our last read or write, the write lock and the
//! folder watch. The rules are config.rs's, generic over `Options`:
//! - a missing file is created from the defaults (or the legacy import); a file without the template mark, or one a
//!   migration changed, is rewritten once; an invalid file keeps the defaults (at load) or the last good snapshot
//!   (at reload) and is never touched by a load;
//! - one bad value keeps the rest: a number outside its descriptor's range, or a key of the wrong type, is one error
//!   with its line, and the last good value (at load: the default) stands in for that key alone;
//! - every write is atomic (`oled_sys::datadir::write_atomic`) and is refused unless a file with errors could first
//!   be copied to `<file>.bak`;
//! - the watch is a folder change notification with a thread-pool wait: the callback re-arms the notification first,
//!   posts one message only when the file's modification time differs from the remembered one, and never logs (a
//!   log line in the watched folder would signal it again).
//!
//! THREADING: every method may be called from any thread. Reads of the snapshot never wait for a write.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::SystemTime;

use oled_sys::datadir::{read_file, write_atomic, FileAcl};
use serde_json::Value;
use windows::Win32::Foundation::{HANDLE, HWND, INVALID_HANDLE_VALUE, LPARAM, WPARAM};
use windows::Win32::Storage::FileSystem::{
    FindCloseChangeNotification, FindFirstChangeNotificationW, FindNextChangeNotification,
    FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE,
};
use windows::Win32::System::Threading::{
    RegisterWaitForSingleObject, UnregisterWaitEx, INFINITE, WT_EXECUTEINWAITTHREAD,
};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::desc::Descriptor;
use crate::error::{ConfigError, Kept, Problem};
use crate::jsonc;

pub trait Options: Sized + Default + Clone + PartialEq + Send + Sync + 'static {
    const VERSION: u16;
    const FILE: &'static str;
    fn descriptors() -> &'static [Descriptor];
    fn known_keys() -> &'static [&'static str]; // top-level JSON keys this version reads
    fn from_value(v: &serde_json::Value, text: &str, last_good: Option<&Self>) -> (Self, Vec<ConfigError>);
    fn to_template(&self) -> String; // the whole file as written (v1: today's render, byte for byte)
    fn template_mark() -> &'static str; // a file without it is rewritten once with the template
    #[allow(clippy::type_complexity)] // the frozen signature
    fn migrations() -> &'static [fn(&mut serde_json::Value) -> Result<(), MigrateError>]; // index = source version
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MigrateError(pub String);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    AdminRootUserEditable,
    AdminRootAdminOnly,
    PerUser,
}
#[derive(Clone, Debug)]
pub struct FileSpec {
    pub path: std::path::PathBuf,
    pub kind: FileKind,
    pub cap: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriteError {
    NotWritable,
    Backup(String),
    Io(String),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadOutcome {
    Read,
    Rewritten,
    Created,
    Invalid(String),
    Unreadable(String),
    NoFolder,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reload {
    Unchanged,
    Changed,
    Invalid(String),
    Removed,
}
#[derive(Clone, Copy, Debug)]
pub struct WatchNotify {
    pub hwnd: isize,
    pub msg: u32,
} // posts (msg, 0 changed | 1 watch broken, 0)

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WriteError::NotWritable => f.write_str("no writable data folder"),
            WriteError::Backup(s) | WriteError::Io(s) => f.write_str(s),
        }
    }
}

pub struct Store<C: Options> {
    /// The file; `None` = no usable folder: defaults, never written.
    spec: Option<FileSpec>,
    /// Runs after every snapshot change, on the thread that made it.
    on_set: fn(&C),
    snapshot: Mutex<Arc<C>>,
    /// The errors of the file as last read (empty after our own write).
    errors: Mutex<Arc<[ConfigError]>>,
    /// Modification time of the file at our last read or write (`None` = missing or unknown). A write holds it for
    /// its whole length, so the watch never takes our own write for a hand edit.
    mtime: Mutex<Option<SystemTime>>,
    /// Serializes read-modify-writes (`modify`) and writes (`save`) across threads.
    write: Mutex<()>,
    /// Raw handle of the folder's change notification, 0 = no watch.
    watch_change: AtomicIsize,
    /// Raw handle of the thread-pool wait registered on it, 0 = none.
    watch_wait: AtomicIsize,
    /// The window the callback posts to, and its message.
    notify_hwnd: AtomicIsize,
    notify_msg: AtomicU32,
}

impl<C: Options> Store<C> {
    /// None: defaults, never writes; on_set runs after every snapshot change.
    pub fn new(spec: Option<FileSpec>, on_set: fn(&C)) -> Self {
        Store {
            spec,
            on_set,
            snapshot: Mutex::new(Arc::new(C::default())),
            errors: Mutex::new(Arc::from(Vec::new())),
            mtime: Mutex::new(None),
            write: Mutex::new(()),
            watch_change: AtomicIsize::new(0),
            watch_wait: AtomicIsize::new(0),
            notify_hwnd: AtomicIsize::new(0),
            notify_msg: AtomicU32::new(0),
        }
    }

    /// Reads the file (never fails). Missing: creates it from `legacy` (a one-time import) or the defaults. Without
    /// the template mark, or changed by a migration: rewritten once with the template (not while it has errors).
    /// Invalid or unreadable: the defaults, and the file is left untouched. Remembers the modification time for
    /// `reload_if_changed`. Every step is logged as the dock's config.rs logged it.
    pub fn load(&self, legacy: impl FnOnce() -> Option<C>) -> LoadOutcome {
        let tag = tag::<C>();
        let Some(spec) = &self.spec else {
            oled_sys::log!("{tag}: no usable data folder: running with the defaults, nothing is saved");
            self.set_errors(Vec::new());
            self.set(C::default());
            return LoadOutcome::NoFolder;
        };
        let path = &spec.path;
        let outcome = match read_text(path, spec.cap) {
            Ok(text) => match decode::<C>(&text, None, Kept::Default) {
                Ok(d) => {
                    oled_sys::log!("{tag}: read from {}", self.shown(path));
                    log_errors::<C>(&d.errors);
                    let mut outcome = LoadOutcome::Read;
                    if d.errors.is_empty() && (d.migrated || !text.contains(C::template_mark())) {
                        // Plain JSON (or migrated): rewrite once with the commented template.
                        let _w = lock(&self.write);
                        match self.write_file(&d.value) {
                            Ok(()) => {
                                oled_sys::log!("{tag}: migrated to the commented format");
                                outcome = LoadOutcome::Rewritten;
                            }
                            Err(e) => oled_sys::log!("{tag}: migration failed: {e}"),
                        }
                    }
                    self.set_errors(d.errors);
                    self.set(d.value);
                    outcome
                }
                Err(e) => {
                    let msg = e.serde_text();
                    oled_sys::log!(
                        "{tag}: {} is invalid ({msg}): using defaults, the file is left untouched",
                        self.shown(path)
                    );
                    self.set_errors(vec![e]);
                    self.set(C::default());
                    LoadOutcome::Invalid(msg)
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let c = legacy().unwrap_or_default();
                oled_sys::log!("{tag}: missing, creating {}", self.shown(path));
                let written = {
                    let _w = lock(&self.write);
                    self.write_file(&c)
                };
                if let Err(e) = written {
                    oled_sys::log!("{tag}: creation failed: {e}");
                }
                self.set_errors(Vec::new());
                self.set(c);
                LoadOutcome::Created
            }
            Err(e) => {
                oled_sys::log!("{tag}: reading {} failed ({e}): using defaults", self.shown(path));
                self.set_errors(Vec::new());
                self.set(C::default());
                LoadOutcome::Unreadable(e.to_string())
            }
        };
        *lock(&self.mtime) = file_mtime(path);
        outcome
    }

    /// Current snapshot (cheap: clones an `Arc`). Before `load` it holds the defaults.
    pub fn get(&self) -> Arc<C> {
        Arc::clone(&lock(&self.snapshot))
    }

    /// Replaces the snapshot (nothing is written), then runs `on_set`.
    pub fn set(&self, c: C) {
        let new = Arc::new(c);
        *lock(&self.snapshot) = Arc::clone(&new);
        (self.on_set)(&new);
    }

    /// The errors of the file as last read; empty after our own write.
    pub fn errors(&self) -> Arc<[ConfigError]> {
        Arc::clone(&lock(&self.errors))
    }

    /// Atomic read-modify-write: applies `f` to a copy of the snapshot, writes the file and, if that succeeds,
    /// replaces the snapshot. On an error the snapshot stays as it was.
    pub fn modify(&self, f: impl FnOnce(&mut C)) -> Result<(), WriteError> {
        let _w = lock(&self.write);
        let mut c = (*self.get()).clone();
        f(&mut c);
        self.write_file(&c)?;
        self.set(c);
        Ok(())
    }

    /// Writes `c` and makes it the snapshot; the remembered modification time is our write's, so
    /// `reload_if_changed` does not re-read it.
    pub fn save(&self, c: &C) -> Result<(), WriteError> {
        let _w = lock(&self.write);
        self.write_file(c)?;
        self.set(c.clone());
        Ok(())
    }

    /// Checks the modification time; when it changed, re-reads the file. A valid content that differs from the
    /// snapshot replaces it (`Changed`); an invalid one keeps the snapshot and is logged once (`Invalid`); a removed
    /// file keeps the snapshot (it is created again by the next write). Does not block: one stat and the read of a
    /// small file.
    pub fn reload_if_changed(&self) -> Reload {
        let tag = tag::<C>();
        let Some(spec) = &self.spec else { return Reload::Unchanged };
        let path = &spec.path;
        {
            let mut last = lock(&self.mtime);
            let now = file_mtime(path);
            if *last == now {
                return Reload::Unchanged;
            }
            *last = now;
            if now.is_none() {
                drop(last);
                oled_sys::log!("{tag}: file removed, keeping the current configuration");
                return Reload::Removed;
            }
        }
        let text = match read_text(path, spec.cap) {
            Ok(t) => t,
            Err(e) => {
                oled_sys::log!("{tag}: re-read failed ({e})");
                return Reload::Invalid(e.to_string());
            }
        };
        let current = self.get();
        match decode::<C>(&text, Some(&current), Kept::LastGood) {
            Ok(d) => {
                log_errors::<C>(&d.errors);
                self.set_errors(d.errors);
                if d.value == *current {
                    return Reload::Unchanged;
                }
                oled_sys::log!("{tag}: changed on disk, reloaded");
                self.set(d.value);
                Reload::Changed
            }
            Err(e) => {
                let msg = e.serde_text();
                oled_sys::log!("{tag}: invalid change ({msg}), keeping the previous one");
                self.set_errors(vec![e]);
                Reload::Invalid(msg)
            }
        }
    }

    /// true if the file's modification time differs from the one of our last read or write (a hand edit that
    /// `reload_if_changed` has not read yet). Does not consume the change.
    pub fn external_edit_pending(&self) -> bool {
        let Some(spec) = &self.spec else { return false };
        let last = lock(&self.mtime);
        should_post(file_mtime(&spec.path), *last)
    }

    /// Starts the wait on the folder's change notification (no timer, no thread of ours: a thread-pool wait runs the
    /// callback in its wait thread). It posts `(notify.msg, 0, 0)` to `notify.hwnd` when the file changed, and
    /// `(notify.msg, 1, 0)` once when the watch broke (the caller then polls). Err when it cannot be set up.
    pub fn watch(&'static self, notify: WatchNotify) -> Result<(), String> {
        if self.watching() {
            return Ok(());
        }
        let dir = self.spec.as_ref().and_then(|s| s.path.parent()).ok_or_else(|| "no data folder".to_string())?;
        // A watch that broke left its notification open (its wait is already gone): close it first.
        close_change(self.watch_change.swap(0, Ordering::SeqCst));
        let wide: Vec<u16> = std::os::windows::ffi::OsStrExt::encode_wide(dir.as_os_str()).chain(Some(0)).collect();
        // SAFETY: `wide` is a NUL-terminated path that outlives the call.
        let change = unsafe {
            FindFirstChangeNotificationW(
                windows::core::PCWSTR(wide.as_ptr()),
                false,
                FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_SIZE,
            )
        }
        .map_err(|e| e.to_string())?;
        self.notify_hwnd.store(notify.hwnd, Ordering::SeqCst);
        self.notify_msg.store(notify.msg, Ordering::SeqCst);
        self.watch_change.store(change.0 as isize, Ordering::SeqCst);
        let mut wait = HANDLE::default();
        let context: *const Self = self;
        // SAFETY: `change` is a valid notification handle owned by this store until `unwatch`; the callback is an
        // `extern "system"` fn valid for the whole process and its context is `self`, which is 'static.
        // WT_EXECUTEINWAITTHREAD: one callback per notification, never two at once (the callback re-arms the handle
        // before the wait thread waits again).
        let reg = unsafe {
            RegisterWaitForSingleObject(
                &mut wait,
                change,
                Some(on_change::<C>),
                Some(context.cast::<c_void>()),
                INFINITE,
                WT_EXECUTEINWAITTHREAD,
            )
        };
        if let Err(e) = reg {
            close_change(self.watch_change.swap(0, Ordering::SeqCst));
            return Err(e.to_string());
        }
        self.watch_wait.store(wait.0 as isize, Ordering::Release);
        Ok(())
    }

    /// Ends the watch: waits for a running callback, then closes the notification. Idempotent; true when a
    /// notification handle was closed (the watch was still held).
    pub fn unwatch(&self) -> bool {
        let wait = self.watch_wait.swap(0, Ordering::SeqCst);
        if wait != 0 {
            // SAFETY: `wait` came from RegisterWaitForSingleObject and is unregistered once (the swap above);
            // INVALID_HANDLE_VALUE waits for a callback in progress, so the notification is idle when it is closed.
            if let Err(e) = unsafe { UnregisterWaitEx(HANDLE(wait as *mut c_void), Some(INVALID_HANDLE_VALUE)) } {
                oled_sys::log!("{}: UnregisterWaitEx failed: {e}", tag::<C>());
            }
        }
        let change = self.watch_change.swap(0, Ordering::SeqCst);
        close_change(change);
        change != 0
    }

    /// true while the folder watch runs; when it does not, the caller polls `reload_if_changed`.
    pub fn watching(&self) -> bool {
        self.watch_wait.load(Ordering::Acquire) != 0
    }

    /// The values that differ from the defaults (objects compared key by key), plus "version", as compact JSON
    /// (ROADMAP X27). Read from the templates, so it needs nothing beyond `Options`.
    pub fn export(&self) -> String {
        let now = template_value(&self.get().to_template());
        let default = template_value(&C::default().to_template());
        let mut out = match diff(&now, Some(&default)) {
            Some(Value::Object(m)) => m,
            _ => serde_json::Map::new(),
        };
        out.insert("version".to_string(), Value::from(C::VERSION));
        jsonc::js(&out)
    }

    /// `path` as logs show it: in full under the admin root, the file name alone in a user's folder (a user path
    /// never reaches a log).
    fn shown(&self, path: &Path) -> String {
        match self.spec.as_ref().map(|s| s.kind) {
            Some(FileKind::PerUser) => path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            _ => path.display().to_string(),
        }
    }

    /// An error text as logs and callers see it: in a user's folder the folder is cut from every path in it, so the
    /// file name alone remains (`write_atomic` names the full path it failed on).
    fn shown_error(&self, e: String) -> String {
        let Some(spec) = self.spec.as_ref().filter(|s| s.kind == FileKind::PerUser) else { return e };
        let Some(dir) = spec.path.parent().map(|d| d.display().to_string()).filter(|d| !d.is_empty()) else {
            return e;
        };
        if dir.ends_with(['\\', '/']) {
            return e.replace(&dir, "");
        }
        e.replace(&format!("{dir}\\"), "").replace(&format!("{dir}/"), "")
    }

    fn set_errors(&self, errors: Vec<ConfigError>) {
        *lock(&self.errors) = Arc::from(errors);
    }

    /// Atomic write and the remembered modification time. Call with the write lock held. The admin-root kinds need
    /// a writable root (an elevated process that registered it); the user-editable file gets its explicit DACL
    /// (`FileAcl::UserEditable`) before it replaces the old one, so hand edits keep working after every save.
    fn write_file(&self, c: &C) -> Result<(), WriteError> {
        let Some(spec) = &self.spec else { return Err(WriteError::NotWritable) };
        let acl = match spec.kind {
            FileKind::AdminRootUserEditable => FileAcl::UserEditable,
            FileKind::AdminRootAdminOnly | FileKind::PerUser => FileAcl::Inherit,
        };
        if spec.kind != FileKind::PerUser && !oled_sys::datadir::writable() {
            return Err(WriteError::NotWritable);
        }
        let text = c.to_template();
        // Never lose unparsed user edits: refuse to write if they cannot be saved aside.
        self.backup_before_write(spec)?;
        let mut last = lock(&self.mtime);
        write_atomic(&spec.path, text.as_bytes(), acl).map_err(|e| WriteError::Io(self.shown_error(e)))?;
        *last = file_mtime(&spec.path);
        drop(last);
        // The file just written holds no error.
        self.set_errors(Vec::new());
        Ok(())
    }

    /// If the file exists but does not load cleanly (the user is mid-edit, made a typo or wrote a wrong value),
    /// copies it to `<file>.bak` so the rewrite that follows does not destroy those edits.
    fn backup_before_write(&self, spec: &FileSpec) -> Result<(), WriteError> {
        let text = match read_text(&spec.path, spec.cap) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(WriteError::Backup(format!("reading {}: {e}", self.shown(&spec.path)))),
        };
        if decode::<C>(&text, None, Kept::Default).is_ok_and(|d| d.errors.is_empty()) {
            return Ok(());
        }
        let bak = backup_path(&spec.path);
        write_atomic(&bak, text.as_bytes(), FileAcl::Inherit)
            .map_err(|e| WriteError::Backup(format!("backup to {}: {}", self.shown(&bak), self.shown_error(e))))?;
        oled_sys::log!(
            "{}: {} has errors, copied to {} before rewriting",
            tag::<C>(),
            self.shown(&spec.path),
            self.shown(&bak)
        );
        Ok(())
    }
}

/// A file read and checked: the value, its errors (none of them `Problem::Parse`), and whether a migration changed it.
struct Decoded<C> {
    value: C,
    errors: Vec<ConfigError>,
    migrated: bool,
}

/// Text to options: JSONC (a leading BOM skipped, comments stripped), a JSON object, the migrations from the file's
/// "version" (absent = 0) up to `C::VERSION`, then `C::from_value`. One bad value keeps the rest: a top-level number
/// outside its descriptor's range, and a top-level key `from_value` cannot read alone, each give one error at their
/// line (`jsonc::locate`) and lose their value: the last good one stands in (none: the key is dropped, so the
/// reader's default applies). Err (the file as a whole is unusable, with `kept`) for a syntax error, a value that is
/// not an object, a failed migration, or a `Problem::Parse` that `from_value` still reports. Total.
fn decode<C: Options>(text: &str, last_good: Option<&C>, kept: Kept) -> Result<Decoded<C>, ConfigError> {
    let text = jsonc::strip_bom(text);
    let mut v: Value =
        serde_json::from_str(&jsonc::strip_comments(text)).map_err(|e| ConfigError::from_serde(&e, kept))?;
    if !v.is_object() {
        return Err(whole_file("the file is not a JSON object".to_string(), kept));
    }
    let source = v.get("version").and_then(Value::as_u64).map_or(0, |n| u16::try_from(n).unwrap_or(u16::MAX));
    let mut migrated = false;
    for i in source..C::VERSION {
        let Some(m) = C::migrations().get(usize::from(i)) else { continue };
        let before = v.clone();
        m(&mut v).map_err(|e| whole_file(format!("migration from version {i} failed: {}", e.0), kept))?;
        migrated |= v != before;
    }
    let mut stand_in = StandIn { last_good, template: None };
    let mut errors = out_of_range::<C>(&mut v, text, &mut stand_in);
    let (mut value, mut found) = C::from_value(&v, text, last_good);
    if found.iter().any(is_whole) {
        errors.extend(wrong_types::<C>(&mut v, text, &mut stand_in));
        (value, found) = C::from_value(&v, text, last_good);
    }
    if let Some(e) = found.iter().find(|e| is_whole(e)) {
        return Err(ConfigError { kept, ..e.clone() });
    }
    errors.extend(found);
    // In the order of the file (an error without a line first).
    errors.sort_by_key(|e| (e.line, e.col));
    Ok(Decoded { value, errors, migrated })
}

/// An error that makes the whole file unusable.
fn is_whole(e: &ConfigError) -> bool {
    matches!(e.problem, Problem::Parse(_))
}

/// What stands in for a key that lost its value: its value in the last good options' template, or none.
struct StandIn<'a, C> {
    last_good: Option<&'a C>,
    /// The last good options as JSON, made once, at the first key that needs it.
    template: Option<Value>,
}

impl<C: Options> StandIn<'_, C> {
    fn kept(&self) -> Kept {
        if self.last_good.is_some() {
            Kept::LastGood
        } else {
            Kept::Default
        }
    }

    /// Puts the stand-in of `key` in place of its value, or drops the key when there is none.
    fn replace(&mut self, o: &mut serde_json::Map<String, Value>, key: &str) {
        let with = match self.last_good {
            Some(lg) => self.template.get_or_insert_with(|| template_value(&lg.to_template())).get(key).cloned(),
            None => None,
        };
        match with {
            Some(x) => {
                o.insert(key.to_string(), x);
            }
            None => {
                o.remove(key);
            }
        }
    }
}

/// The top-level numbers outside the range of their descriptor: one `Problem::Range` each, the stand-in in place.
fn out_of_range<C: Options>(v: &mut Value, text: &str, stand_in: &mut StandIn<C>) -> Vec<ConfigError> {
    let mut out = Vec::new();
    let Some(o) = v.as_object_mut() else { return out };
    for d in C::descriptors() {
        let Some(r) = d.range else { continue };
        let Some(got) = o.get(d.json).and_then(number) else { continue };
        if (r.min..=r.max).contains(&got) {
            continue;
        }
        stand_in.replace(o, d.json);
        out.push(error_at(text, d.json, Problem::Range { min: r.min, max: r.max, got }, stand_in.kept()));
    }
    out
}

/// The top-level keys `from_value` cannot read on their own: one `Problem::Type` each, the stand-in in place. Only
/// the declared keys are read alone: an unknown key is kept aside as it is and cannot fail, and a file of many small
/// unknown keys costs no read per key.
fn wrong_types<C: Options>(v: &mut Value, text: &str, stand_in: &mut StandIn<C>) -> Vec<ConfigError> {
    let mut out = Vec::new();
    let Some(o) = v.as_object_mut() else { return out };
    for &key in C::known_keys() {
        let Some(x) = o.get(key) else { continue };
        let alone = Value::Object(std::iter::once((key.to_string(), x.clone())).collect());
        // Its own text too: a reader that words its error from the text (as `options!`'s does) then names this key.
        let (_, found) = C::from_value(&alone, &jsonc::js(&alone), None);
        let Some(e) = found.into_iter().find(is_whole) else { continue };
        let expected = expected::<C>(key, x, &e);
        stand_in.replace(o, key);
        out.push(error_at(text, key, Problem::Type { expected }, stand_in.kept()));
    }
    out
}

/// A JSON number as an i64: a fraction cut, a number beyond i64 saturated.
fn number(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_u64().map(|_| i64::MAX)).or_else(|| v.as_f64().map(|f| f as i64))
}

/// What a key of the wrong type should have been: from its descriptor's kind, else from serde's words. A value that
/// already is the object or list its kind says (the wrong value is inside it) takes serde's words too.
fn expected<C: Options>(key: &str, x: &Value, e: &ConfigError) -> &'static str {
    use crate::desc::ValueType;
    if let Some(d) = C::descriptors().iter().find(|d| d.json == key) {
        let inside = match d.kind {
            ValueType::List => x.is_array(),
            ValueType::Map | ValueType::Object => x.is_object(),
            _ => false,
        };
        if !inside {
            return match d.kind {
                ValueType::Bool => "a boolean",
                ValueType::Int => "a number",
                ValueType::Str => "a string",
                ValueType::Enum => "one of its values",
                ValueType::Color => "a colour",
                ValueType::List => "a list",
                ValueType::Map | ValueType::Object => "an object",
            };
        }
    }
    const SERDE: [(&str, &str); 12] = [
        ("a boolean", "a boolean"),
        ("a string", "a string"),
        ("a sequence", "a list"),
        ("an array", "a list"),
        ("a map", "an object"),
        ("struct", "an object"),
        ("u8", "a number"),
        ("u16", "a number"),
        ("u32", "a number"),
        ("u64", "a number"),
        ("i64", "a number"),
        ("f64", "a number"),
    ];
    let said = match &e.problem {
        Problem::Parse(m) => m.rsplit("expected ").next().unwrap_or(""),
        _ => "",
    };
    SERDE.iter().find(|(serde, _)| said.starts_with(serde)).map_or("another value", |(_, ours)| ours)
}

/// An error at the key `path` of `text` (line and column 0 when the key is not found).
fn error_at(text: &str, path: &str, problem: Problem, kept: Kept) -> ConfigError {
    let (line, col) = jsonc::locate(text, path).unwrap_or((0, 0));
    ConfigError { path: path.to_string(), line, col, problem, kept }
}

fn whole_file(message: String, kept: Kept) -> ConfigError {
    ConfigError { path: String::new(), line: 0, col: 0, problem: Problem::Parse(message), kept }
}

/// Reads a text as `Store::load` and `reload_if_changed` read their file (a BOM, comments, the migrations from the
/// file's version, `Options::from_value`): the options and their value errors, or the error that makes the text
/// unusable as a whole. For a copy that is not the store's own file (a one-time import), and for tests.
pub fn parse_text<C: Options>(text: &str, last_good: Option<&C>) -> Result<(C, Vec<ConfigError>), ConfigError> {
    let kept = if last_good.is_some() { Kept::LastGood } else { Kept::Default };
    decode::<C>(text, last_good, kept).map(|d| (d.value, d.errors))
}

/// Logs each value error of a load once, as "<tag>: <log_line>".
fn log_errors<C: Options>(errors: &[ConfigError]) {
    for e in errors {
        oled_sys::log!("{}: {}", tag::<C>(), e.log_line());
    }
}

/// The name of the file without its extension ("config" for config.json): the prefix of the store's log lines.
fn tag<C: Options>() -> &'static str {
    C::FILE.split_once('.').map_or(C::FILE, |(stem, _)| stem)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn file_mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).ok()?.modified().ok()
}

/// Reads the file through `datadir` (no reparse point followed, at most `cap` bytes). The content is untrusted anyway
/// (a user may edit it), so extra hard links do not matter for a read.
fn read_text(p: &Path, cap: u64) -> std::io::Result<String> {
    let bytes = read_file(p, cap, false)?;
    String::from_utf8(bytes).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// `<file>.bak` beside the file (config.json -> config.json.bak).
fn backup_path(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(".bak");
    PathBuf::from(s)
}

/// The change notification of a folder fires for every write in it (a log, a state file); only a modification time
/// of the file that differs from the remembered one is worth a message to the UI thread.
fn should_post(now: Option<SystemTime>, remembered: Option<SystemTime>) -> bool {
    now != remembered
}

fn close_change(raw: isize) {
    if raw != 0 {
        // SAFETY: the handle came from FindFirstChangeNotificationW and is closed once (the caller swapped it out).
        let _ = unsafe { FindCloseChangeNotification(HANDLE(raw as *mut c_void)) };
    }
}

/// The JSON value of a template (comments stripped); `Null` if it does not parse.
fn template_value(text: &str) -> Value {
    serde_json::from_str(&jsonc::strip_comments(text)).unwrap_or(Value::Null)
}

/// What of `now` differs from `default`: objects key by key, anything else as a whole; `None` when nothing does.
fn diff(now: &Value, default: Option<&Value>) -> Option<Value> {
    match (now, default) {
        (Value::Object(n), Some(Value::Object(d))) => {
            let m: serde_json::Map<String, Value> =
                n.iter().filter_map(|(k, v)| diff(v, d.get(k)).map(|v| (k.clone(), v))).collect();
            (!m.is_empty()).then_some(Value::Object(m))
        }
        _ => (Some(now) != default).then(|| now.clone()),
    }
}

/// Thread-pool callback of the folder notification (wait thread): re-arms the handle first, then posts one message
/// only when the file itself changed. Never logs (a log line in the watched folder would signal this callback again).
unsafe extern "system" fn on_change<C: Options>(context: *mut c_void, _timed_out: bool) {
    // SAFETY: `context` is the `&'static Store<C>` that `watch` registered.
    let Some(store) = (unsafe { context.cast_const().cast::<Store<C>>().as_ref() }) else { return };
    let change = store.watch_change.load(Ordering::SeqCst);
    let hwnd = store.notify_hwnd.load(Ordering::SeqCst);
    if change == 0 || hwnd == 0 {
        return;
    }
    let (ui, msg) = (HWND(hwnd as *mut c_void), store.notify_msg.load(Ordering::SeqCst));
    // SAFETY: the handle stays open until `unwatch`, which first waits for this callback to return.
    if unsafe { FindNextChangeNotification(HANDLE(change as *mut c_void)) }.is_err() {
        // wParam 1 = the watch is broken: the caller falls back to polling. First end the wait (non-blocking form,
        // allowed in the callback) so a still-signaled handle cannot re-fire this callback in a loop.
        let w = store.watch_wait.swap(0, Ordering::SeqCst);
        if w != 0 {
            // SAFETY: `w` came from RegisterWaitForSingleObject and is unregistered once (the swap above).
            let _ = unsafe { UnregisterWaitEx(HANDLE(w as *mut c_void), None) };
        }
        // SAFETY: PostMessageW to the caller's window from any thread; if it no longer exists it just fails.
        let _ = unsafe { PostMessageW(Some(ui), msg, WPARAM(1), LPARAM(0)) };
        return;
    }
    if store.external_edit_pending() {
        // SAFETY: as above.
        let _ = unsafe { PostMessageW(Some(ui), msg, WPARAM(0), LPARAM(0)) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_file_mtime_change_posts() {
        let t0 = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(100);
        let t1 = t0 + std::time::Duration::from_secs(1);
        assert!(!should_post(Some(t0), Some(t0)));
        assert!(should_post(Some(t1), Some(t0)));
        assert!(should_post(None, Some(t0)));
        assert!(should_post(Some(t0), None));
        assert!(!should_post(None, None));
    }

    #[test]
    fn diff_keeps_only_what_differs() {
        let now = serde_json::json!({"a": 1, "b": {"c": true, "d": [1]}, "e": "x"});
        let def = serde_json::json!({"a": 1, "b": {"c": false, "d": [1]}, "e": "y"});
        assert_eq!(diff(&now, Some(&def)), Some(serde_json::json!({"b": {"c": true}, "e": "x"})));
        assert_eq!(diff(&def, Some(&def)), None);
    }

    /// A config that is any JSON object: `decode`'s inputs without a template. "a" as a string is a whole-file
    /// error, "a" as a number a value error; migrations: 0 adds "m0", 1 fails on "fail", 2 renames "old" to "new".
    #[derive(Clone, Debug, Default, PartialEq)]
    struct Any(Value);

    #[cfg(test)]
    thread_local! {
        /// The calls of `Any::from_value` on this thread.
        static READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    fn m0(v: &mut Value) -> Result<(), MigrateError> {
        if let Some(o) = v.as_object_mut() {
            o.insert("m0".into(), Value::Bool(true));
        }
        Ok(())
    }
    fn m1(v: &mut Value) -> Result<(), MigrateError> {
        if v.get("fail").is_some() {
            return Err(MigrateError("asked to".into()));
        }
        Ok(())
    }
    fn m2(v: &mut Value) -> Result<(), MigrateError> {
        if let Some(o) = v.as_object_mut() {
            if let Some(x) = o.remove("old") {
                o.insert("new".into(), x);
            }
        }
        Ok(())
    }

    impl Options for Any {
        const VERSION: u16 = 3;
        const FILE: &'static str = "any.json";
        fn descriptors() -> &'static [Descriptor] {
            &[]
        }
        fn known_keys() -> &'static [&'static str] {
            &["a"]
        }
        fn from_value(v: &Value, _: &str, last_good: Option<&Self>) -> (Self, Vec<ConfigError>) {
            READS.with(|n| n.set(n.get() + 1));
            match v.get("a") {
                Some(Value::String(_)) => {
                    (last_good.cloned().unwrap_or_default(), vec![whole_file("a is a string".into(), Kept::Default)])
                }
                Some(x) if x.is_number() => {
                    let e = ConfigError {
                        path: "a".into(),
                        line: 1,
                        col: 2,
                        problem: Problem::Range { min: 0, max: 1, got: 5 },
                        kept: Kept::Default,
                    };
                    (Any(v.clone()), vec![e])
                }
                _ => (Any(v.clone()), Vec::new()),
            }
        }
        fn to_template(&self) -> String {
            jsonc::js(&self.0)
        }
        fn template_mark() -> &'static str {
            ""
        }
        fn migrations() -> &'static [fn(&mut Value) -> Result<(), MigrateError>] {
            &[m0, m1, m2]
        }
    }

    #[test]
    fn decode_runs_the_migrations_from_the_file_version() {
        let d = decode::<Any>("{\"old\": 1}", None, Kept::Default).ok().map(|d| (d.value.0, d.migrated));
        assert_eq!(d, Some((serde_json::json!({"m0": true, "new": 1}), true)));
        // From version 2 only the last one runs; from 3 (or a newer file) none.
        let d = decode::<Any>("{\"version\": 2, \"old\": 1}", None, Kept::Default).ok().map(|d| d.value.0);
        assert_eq!(d, Some(serde_json::json!({"version": 2, "new": 1})));
        let d = decode::<Any>("{\"version\": 9, \"old\": 1}", None, Kept::Default).ok().map(|d| d.migrated);
        assert_eq!(d, Some(false));
        // A migration that changes nothing is not a change.
        let d = decode::<Any>("{\"version\": 2}", None, Kept::Default).ok().map(|d| d.migrated);
        assert_eq!(d, Some(false));
        // A failed migration makes the file unusable.
        let e = decode::<Any>("{\"fail\": 1}", None, Kept::LastGood).err().map(|e| (e.problem, e.kept));
        let want = Problem::Parse("migration from version 1 failed: asked to".into());
        assert_eq!(e, Some((want, Kept::LastGood)));
    }

    #[test]
    fn decode_tells_a_whole_file_error_from_a_value_error() {
        let last = Any(serde_json::json!({"x": 1}));
        // A key the options cannot read on its own loses only itself: one Type error, the last good value standing in
        // (here it has none, so the key is dropped).
        let d = decode::<Any>("{\"version\": 3,\n \"a\": \"s\"}", Some(&last), Kept::LastGood).ok();
        let d = d.map(|d| (d.value.0, d.errors.iter().map(|e| (e.path.clone(), e.line, e.kept)).collect::<Vec<_>>()));
        assert_eq!(d, Some((serde_json::json!({"version": 3}), vec![("a".to_string(), 2, Kept::LastGood)])));
        let d = decode::<Any>("{\"version\": 3, \"a\": 4}", Some(&last), Kept::LastGood).ok();
        assert_eq!(d.map(|d| d.errors.len()), Some(1));
        for bad in ["", "[]", "3", "{", "// only a comment", "\u{feff}"] {
            assert!(decode::<Any>(bad, None, Kept::Default).is_err(), "{bad:?}");
        }
        let e = decode::<Any>("{\n  \"a\": trux\n}", None, Kept::Default).err();
        assert_eq!(e.as_ref().map(|e| (e.line, e.problem.clone())), Some((2, Problem::Parse("expected ident".into()))));
        assert_eq!(e.map(|e| e.serde_text()), Some("expected ident at line 2 column 11".to_string()));
    }

    #[test]
    fn a_wrong_type_is_looked_for_in_the_declared_keys_only() {
        // 100,000 unknown keys beside one key of the wrong type: that key is read alone once, no unknown key is (an
        // unknown key is kept aside as it is and cannot fail), so a large file of small keys costs no read per key.
        let mut text = String::from("{\"a\": \"s\"");
        for i in 0..100_000 {
            text.push_str(&format!(", \"k{i}\": {i}"));
        }
        text.push('}');
        READS.with(|n| n.set(0));
        let d = decode::<Any>(&text, None, Kept::Default).ok();
        assert_eq!(d.map(|d| d.errors.iter().map(|e| e.path.clone()).collect::<Vec<_>>()), Some(vec!["a".to_string()]));
        assert_eq!(READS.with(std::cell::Cell::get), 3, "the file, \"a\" alone, the file again");
    }

    /// xorshift64: the fuzz inputs, seeded, so a failure repeats.
    fn next(s: &mut u64) -> u64 {
        *s ^= *s << 13;
        *s ^= *s >> 7;
        *s ^= *s << 17;
        *s
    }

    /// A JSON-like text of up to 60 pieces, often almost valid, sometimes raw bytes.
    fn fuzz_text(s: &mut u64) -> String {
        const PIECES: [&str; 34] = [
            "{",
            "}",
            "[",
            "]",
            "\"",
            "\\",
            "/",
            "*",
            ":",
            ",",
            "\n",
            " ",
            "a",
            "1",
            "-",
            "true",
            "null",
            "\"a\"",
            "\"version\"",
            "\"old\"",
            "\"fail\"",
            "//",
            "/*",
            "*/",
            "\u{feff}",
            "\u{e9}",
            "\\u12",
            "2",
            "65536",
            "99999999999999999999",
            "1e999",
            "\"m0\"",
            "\r\n",
            "\t",
        ];
        let n = next(s) % 60;
        let mut t = String::new();
        if next(s).is_multiple_of(3) {
            t.push_str("{\"version\": ");
        }
        for _ in 0..n {
            let r = next(s);
            if r.is_multiple_of(17) {
                let bytes: Vec<u8> = (0..(r % 5)).map(|_| (next(s) & 0xff) as u8).collect();
                t.push_str(&String::from_utf8_lossy(&bytes));
            } else {
                t.push_str(PIECES.get((r % PIECES.len() as u64) as usize).copied().unwrap_or(""));
            }
        }
        t
    }

    fn fuzz_decode(runs: u32) {
        let mut s = 0x9e37_79b9_7f4a_7c15u64;
        let last = Any(serde_json::json!({}));
        for _ in 0..runs {
            let t = fuzz_text(&mut s);
            let _ = decode::<Any>(&t, None, Kept::Default).map(|d| d.errors.len());
            if let Err(e) = decode::<Any>(&t, Some(&last), Kept::LastGood) {
                assert!(matches!(e.problem, Problem::Parse(_)));
                let _ = (e.serde_text(), e.log_line());
            }
        }
    }

    #[test]
    fn fuzz_decode_10k() {
        fuzz_decode(10_000);
    }

    #[test]
    #[ignore = "nightly fuzz: 1,000,000 inputs"]
    fn fuzz_decode_1m() {
        fuzz_decode(1_000_000);
    }

    #[test]
    fn backup_path_appends_bak() {
        assert_eq!(backup_path(Path::new("x\\config.json")), PathBuf::from("x\\config.json.bak"));
    }
}
