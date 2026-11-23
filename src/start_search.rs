//! SEARCH DATA of the Start menu (`popup_startmenu`): what the native Start menu searches with,
//! read from the files Windows itself ships. Nothing here is a list of ours.
//!
//! MEMORY: nothing here stays loaded longer than it is used. The app index is read for each
//! catalog by the `--start-apps` child (the shell thread only when the child failed), whose arena
//! carries only the few keywords of the installed apps (`take_app_index`, L3 T17). The typo
//! rewrites (1.4 MB) are read from disk only for a query that finds nothing (`converted`). The
//! settings catalog is built by the `--start-settings` child into `cache\settings-catalog.bin` (one
//! checked arena, L3 T18); Start keeps its list of it (`Keys`, a shared target: its ~1500 entries
//! cost under 1 MB) while a Start menu is in use and frees it with the rest of its data
//! (`release_settings`, a minute after the last one closed); the launcher maps the file at each open.
//!
//! APPS (`AppIndex`, read with each catalog in the `--start-apps` child):
//!  * `appssynonyms.txt`: `*|<app name>\t*|<query>[:wux:...]\t<penalty>`, e.g. "rdp" for Remote
//!    Desktop Connection, "lol" for League of Legends, and thousands of typo corrections;
//!  * `appsglobals.txt`: `<app id>\t<rank>`, how popular an app is worldwide (lower = more);
//!  * `appsconversions.txt`: `spelling\t<typed>\t<meant>\t<score>`, query rewrites.
//!
//! The first two are in `<Windows>\SystemApps\MicrosoftWindows.Client.CBS_cw5n1h2txyewy\Cortana.UI\
//! ConstraintIndex\<version>\<language>` (only administrators can write there). The conversions
//! exist only as the copy Windows Search unpacks in the user's profile: read as untrusted input
//! (`datadir::read_file`: no reparse points, capped) and used only to rewrite the typed text.
//!
//! SETTINGS (`settings`, built by a child of the dock, see `load_settings` and `serve`): the
//! catalog of the Settings app and of the Control Panel, `<Windows>\ImmersiveControlPanel\Settings\
//! AllSystemSettings_*.xml`. Names and keywords are resource references, resolved in the Windows
//! display language (`SHLoadIndirectString`). A setting opens through its page name
//! (`page=<name>` argument of the Settings app) or its Control Panel deep link. `settingssynonyms.txt`
//! and `settingsglobals.txt` (same folders as above, plus `...Client.Photon...\SettingsSearchData\
//! <language>`) add the synonyms and the ranks.
//!
//! All of it is undocumented: a missing or different file only means fewer results. The Windows
//! folder comes from the API, never from the environment.

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::core::HSTRING;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::Globalization::GetUserDefaultUILanguage;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::System::Registry::{
    RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RRF_SUBKEY_WOW6464KEY,
};
use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN};
use windows::Win32::UI::Shell::{FOLDERID_LocalAppData, SHGetKnownFolderPath, SHLoadIndirectString, KF_FLAG_DEFAULT};

use crate::launcher::arena::{self, Caps, CatalogView, Kind as ArenaKind, Layout};
use crate::launcher::catalog::encode_setting_list;
use crate::log;

/// Largest index file read.
const MAX_FILE: u64 = 16 * 1024 * 1024;
/// The penalty of a keyword that comes from Windows' XML bag of words (the settings catalogue's own keywords), not
/// from its synonym list. Above every penalty Windows gives a synonym and distinct from `parse_synonyms`'s fallback
/// (`u32::MAX / 2`), so `Keys::tidy` keeps a curated synonym over the same XML word and the launcher reads "XML"
/// exactly when the penalty equals this value.
pub(crate) const XML_KEYWORD_PENALTY: u32 = 999_999;
/// The Settings app inside `shell:AppsFolder`.
const SETTINGS_APP: &str =
    r"shell:AppsFolder\windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel";

/// Hidden search keywords of an entry, each with its penalty (lower = better). Packed in one
/// string, one `<keyword>\t<penalty>` per line, keywords lowercase: thousands of entries have a
/// dozen keywords each, and one allocation per entry instead of one per keyword is most of what
/// the settings catalog would otherwise cost.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Keys(String);

#[cfg(test)]
thread_local! {
    /// The non-empty keyword lists read through `Keys::raw` or `Keys::iter` on this thread (KEY-2: none per key).
    pub(crate) static KEY_SCANS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
static NO_KEYS: Keys = Keys(String::new());

impl Keys {
    /// No keywords, for an entry that has none.
    #[cfg(test)]
    pub(crate) fn none() -> &'static Keys {
        &NO_KEYS
    }

    /// Adds a keyword (lowercased here). One with a tab or a line break cannot be stored: skipped.
    pub(crate) fn push(&mut self, key: &str, penalty: u32) {
        let key = key.trim();
        if key.is_empty() || key.contains(['\t', '\n', '\r']) {
            return;
        }
        self.0.push_str(&key.to_lowercase());
        self.0.push('\t');
        self.0.push_str(&penalty.to_string());
        self.0.push('\n');
    }

    /// The keywords and their penalty texts, nothing parsed: the launcher parses a penalty only for a keyword that
    /// matched.
    pub(crate) fn raw(&self) -> impl Iterator<Item = (&str, &str)> {
        #[cfg(test)]
        if !self.0.is_empty() {
            KEY_SCANS.with(|n| n.set(n.get() + 1));
        }
        self.0.lines().filter_map(|l| l.rsplit_once('\t'))
    }

    /// The keywords and their penalties, in the order they were added.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&str, u32)> {
        #[cfg(test)]
        if !self.0.is_empty() {
            KEY_SCANS.with(|n| n.set(n.get() + 1));
        }
        self.0.lines().filter_map(|l| {
            let (key, penalty) = l.rsplit_once('\t')?;
            Some((key, penalty.parse().ok()?))
        })
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The packed text itself (one `<keyword>\t<penalty>` per line), nothing parsed: what an arena stores for Start.
    pub(crate) fn packed(&self) -> &str {
        &self.0
    }

    /// Keywords from packed text (`packed`'s, read back from an arena). Nothing is parsed here; a line that is not
    /// `<keyword>\t<penalty>` is skipped when the keywords are read, as for any `Keys`.
    pub(crate) fn from_packed(text: &str) -> Keys {
        Keys(text.to_string())
    }

    /// The same keywords sorted, each once with its best penalty, in exactly the memory they need.
    pub(crate) fn tidy(&self) -> Keys {
        let mut all: Vec<(&str, u32)> = self.iter().collect();
        // whole tuples: equal ones are the same, so an unstable sort gives the same list
        all.sort_unstable();
        all.dedup_by(|later, first| later.0 == first.0);
        let mut out = Keys(String::with_capacity(self.0.len()));
        for (key, penalty) in all {
            out.push(key, penalty);
        }
        out.0.shrink_to_fit();
        out
    }
}

#[cfg(test)]
impl From<&[(&str, u32)]> for Keys {
    fn from(pairs: &[(&str, u32)]) -> Keys {
        let mut k = Keys::default();
        for (key, penalty) in pairs {
            k.push(key, *penalty);
        }
        k
    }
}

// ------------------------------------------------------------------ files

fn windows_dir() -> Option<PathBuf> {
    crate::shell_thread::system_dir()?.parent().map(Path::to_path_buf)
}

fn subdirs(p: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(p).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect()
}

/// Folders holding the index files Windows ships: every `<version>\<language>` of the Cortana index
/// and every language of the Settings search data. Rows are keyed by display name or by id, so the
/// languages the user does not use simply match nothing.
fn index_dirs() -> Vec<PathBuf> {
    let Some(apps) = windows_dir().map(|w| w.join("SystemApps")) else { return Vec::new() };
    let cortana = apps.join(r"MicrosoftWindows.Client.CBS_cw5n1h2txyewy\Cortana.UI\ConstraintIndex");
    let mut v: Vec<PathBuf> = subdirs(&cortana).iter().flat_map(|ver| subdirs(ver)).collect();
    v.extend(subdirs(&apps.join(r"MicrosoftWindows.Client.Photon_cw5n1h2txyewy\Public\wsxpacks\SettingsSearchData")));
    v
}

fn read_text(path: &Path) -> Option<String> {
    let len = std::fs::metadata(path).ok()?.len();
    if len > MAX_FILE {
        return None;
    }
    Some(String::from_utf8_lossy(&std::fs::read(path).ok()?).into_owned())
}

/// `name` from every index folder, one after the other.
fn read_index(name: &str) -> String {
    index_dirs().iter().filter_map(|d| read_text(&d.join(name))).collect::<Vec<_>>().join("\n")
}

/// What Windows Search reads `query` as, when it has a rewrite for it ("rkdp" is "rdp"). Reads
/// `appsconversions.txt` from disk each time and keeps nothing: called only for a query that
/// found nothing.
// ponytail: a streamed read (64 KiB held, never the 1.4 MB file) on the calling (UI) thread, a millisecond or two from
// the file cache; ask the shell thread instead if it ever shows in a trace.
pub(crate) fn converted(query: &str) -> Option<String> {
    let typed = query.trim().to_lowercase();
    if typed.is_empty() {
        return None;
    }
    convert_stream(open_conversions()?, &typed)
}

/// The reader's buffer: the file is never held whole (it is about 1.4 MB), only this much of it.
const CONVERSIONS_BUF: usize = 64 * 1024;
/// A row of the file is a few dozen bytes; a longer line is not a row and is skipped.
const CONVERSIONS_LINE_MAX: u64 = 4096;

fn conversions_reader<R: Read>(r: R) -> BufReader<R> {
    BufReader::with_capacity(CONVERSIONS_BUF, r)
}

/// `convert` over a stream, line by line, stopping at the first row that matches. Same answer as `convert` over the
/// whole text (an invalid UTF-8 byte reads as U+FFFD, as before), at most `MAX_FILE` bytes read, about 68 KiB held.
pub(crate) fn convert_stream(r: impl Read, typed: &str) -> Option<String> {
    let typed = typed.trim();
    if typed.is_empty() {
        return None;
    }
    let mut r = conversions_reader(r.take(MAX_FILE));
    let mut line: Vec<u8> = Vec::with_capacity(256);
    loop {
        line.clear();
        let n = (&mut r).take(CONVERSIONS_LINE_MAX).read_until(b'\n', &mut line).ok()?;
        if n == 0 {
            return None;
        }
        if line.last() != Some(&b'\n') && u64::try_from(n).ok() == Some(CONVERSIONS_LINE_MAX) {
            // Too long for a row: drop the rest of the line.
            loop {
                let buf = r.fill_buf().ok()?;
                if buf.is_empty() {
                    return None;
                }
                match buf.iter().position(|&b| b == b'\n') {
                    Some(p) => {
                        r.consume(p.saturating_add(1));
                        break;
                    }
                    None => {
                        let all = buf.len();
                        r.consume(all);
                    }
                }
            }
            continue;
        }
        if let Some(meant) = convert(&String::from_utf8_lossy(&line), typed) {
            return Some(meant.to_string());
        }
    }
}

/// `appsconversions.txt` from the profile of the dock's user (see the module notes), opened through `datadir`'s checked
/// opener (no reparse point, a regular file), or nothing.
fn open_conversions() -> Option<std::fs::File> {
    // SAFETY: valid known-folder id; the returned buffer is freed below.
    let Ok(p) = (unsafe { SHGetKnownFolderPath(&FOLDERID_LocalAppData, KF_FLAG_DEFAULT, None) }) else {
        return None;
    };
    // SAFETY: `p` is a NUL-terminated string owned by us until CoTaskMemFree.
    let local = unsafe { p.to_string() }.unwrap_or_default();
    // SAFETY: `p` came from SHGetKnownFolderPath and is not used afterwards.
    unsafe { windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const _)) };
    if local.is_empty() {
        return None;
    }
    let root = Path::new(&local).join(r"Packages\MicrosoftWindows.Client.CBS_cw5n1h2txyewy\LocalState\ConstraintIndex");
    subdirs(&root).iter().find_map(|d| crate::datadir::open_checked(&d.join("appsconversions.txt"), false).ok())
}

// ------------------------------------------------------------------ parsers (pure)

/// Synonyms by lowercase name. Rows of `text` are `*|<name>\t*|<query>[:wux:...]\t<penalty>`. Every
/// row of a name is kept as Microsoft wrote it (typo corrections included): no filter of ours. Only
/// the rows without a name, which belong to nothing, and malformed ones are skipped.
pub(crate) fn parse_synonyms(text: &str) -> HashMap<String, Keys> {
    let mut map: HashMap<String, Keys> = HashMap::new();
    for line in text.lines() {
        let mut f = line.split('\t');
        let (Some(name), Some(q)) = (f.next(), f.next()) else { continue };
        let (Some(name), Some(q)) = (name.strip_prefix("*|"), q.strip_prefix("*|")) else { continue };
        let q = q.split(":wux:").next().unwrap_or(q).trim();
        if name.is_empty() || q.is_empty() {
            continue;
        }
        let penalty = f.next().and_then(|s| s.trim().parse().ok()).unwrap_or(u32::MAX / 2);
        map.entry(name.to_lowercase()).or_default().push(q, penalty);
    }
    map
}

/// Ranks by lowercase id (rows `<id>\t<rank>`, the best one when an id repeats) and the rank of an
/// id that is not listed: one more than the worst.
pub(crate) fn parse_ranks(text: &str) -> (HashMap<String, u32>, u32) {
    let mut map: HashMap<String, u32> = HashMap::new();
    for line in text.lines() {
        let Some((id, rank)) = line.rsplit_once('\t') else { continue };
        let (id, Ok(rank)) = (id.trim(), rank.trim().parse::<u32>()) else { continue };
        if !id.is_empty() {
            map.entry(id.to_lowercase()).and_modify(|r| *r = (*r).min(rank)).or_insert(rank);
        }
    }
    let default = map.values().max().map_or(0, |m| m.saturating_add(1));
    (map, default)
}

/// What Windows Search reads `typed` (lowercase) as, from the `spelling\t<typed>\t<meant>\t<score>`
/// rows of `conversions`; `None` when there is no row for it.
pub(crate) fn convert<'a>(conversions: &'a str, typed: &str) -> Option<&'a str> {
    let typed = typed.trim();
    if typed.is_empty() {
        return None;
    }
    conversions.lines().find_map(|l| {
        let mut f = l.strip_prefix("spelling\t")?.split('\t');
        let meant = (f.next()? == typed).then(|| f.next())??.trim();
        (!meant.is_empty() && meant != typed).then_some(meant)
    })
}

/// One entry of the settings catalog as written in the XML (references not resolved).
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RawSetting {
    pub id: String,
    pub description: String,
    pub keywords: String,
    /// First page of the Settings app that shows it.
    pub page: Option<String>,
    pub deep_link: Option<String>,
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// Text of the first `<name>...</name>` of `block`.
fn tag(block: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let start = block.find(&open)? + open.len();
    let end = block[start..].find('<')?;
    Some(unescape(block[start..start + end].trim())).filter(|s| !s.is_empty())
}

/// The `<SearchableContent>` entries of an `AllSystemSettings_*.xml`. A plain scan, no XML library:
/// the file is flat and written by Windows; an entry without id or description is skipped.
pub(crate) fn parse_settings_xml(xml: &str) -> Vec<RawSetting> {
    xml.split("<SearchableContent")
        .skip(1)
        .filter_map(|b| {
            let b = b.split("</SearchableContent>").next()?;
            let page = b.find("Type=\"Page\">").and_then(|i| {
                let rest = &b[i + 12..];
                let name = rest[..rest.find('<')?].trim();
                (!name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
                    .then(|| name.to_string())
            });
            Some(RawSetting {
                id: tag(b, "Filename")?,
                description: tag(b, "Description")?,
                keywords: tag(b, "HighKeywords").or_else(|| tag(b, "Keywords")).unwrap_or_default(),
                page,
                deep_link: tag(b, "DeepLink"),
            })
        })
        .collect()
}

/// What opens a setting: `(target, arguments)` for `ShellRequest::Launch`. `None` when the entry
/// cannot be opened from outside (Windows Security pages, unknown forms).
///
/// * a page of the Settings app: the app with `page=<name>`;
/// * a deep link `%windir%\system32\<tool>.exe <args>`: that exe, which must be inside `windir`;
/// * a Control Panel canonical name `Microsoft.Name[\page]`: `control.exe /name ... [/page ...]`;
/// * a `shell:` location: as it is.
pub(crate) fn setting_launch(raw: &RawSetting, windir: &Path) -> Option<(String, Option<String>)> {
    let Some(link) = raw.deep_link.as_deref().map(str::trim).filter(|l| !l.is_empty()) else {
        return raw.page.as_ref().map(|p| (SETTINGS_APP.to_string(), Some(format!("page={p}"))));
    };
    let win = windir.to_string_lossy();
    let lower = link.to_lowercase();
    if lower.starts_with("shell:") {
        return Some((link.to_string(), None));
    }
    if lower.starts_with('%') {
        let mut cmd = link.to_string();
        for var in ["%windir%", "%systemroot%"] {
            while let Some(i) = cmd.to_lowercase().find(var) {
                cmd.replace_range(i..i + var.len(), &win);
            }
        }
        let (exe, args) = cmd.split_once(' ').unwrap_or((&cmd, ""));
        let exe_l = exe.to_lowercase();
        let inside = exe_l.starts_with(&format!("{}\\", win.to_lowercase())) && !exe_l.contains("..");
        return (inside && exe_l.ends_with(".exe"))
            .then(|| (exe.to_string(), Some(args.trim().to_string()).filter(|a| !a.is_empty())));
    }
    if let Some(rest) = link.strip_prefix("Microsoft.") {
        let (name, page) = rest.split_once('\\').unwrap_or((rest, ""));
        let plain = |s: &str| s.chars().all(|c| c.is_ascii_alphanumeric() || "?=_".contains(c));
        if name.is_empty() || !plain(name) || !plain(page) {
            return None;
        }
        let mut args = format!("/name Microsoft.{name}");
        if !page.is_empty() {
            args.push_str(&format!(" /page {page}"));
        }
        return Some((windir.join(r"System32\control.exe").to_string_lossy().into_owned(), Some(args)));
    }
    None
}

/// The keywords of a resolved keyword string (`a|b|c|` or `a;b`), lowercase.
pub(crate) fn split_keywords(resolved: &str) -> Vec<String> {
    resolved.split(['|', ';']).map(|k| k.trim().to_lowercase()).filter(|k| !k.is_empty()).collect()
}

// ------------------------------------------------------------------ app index

/// The app search data of the module notes, parsed.
#[derive(Default)]
pub(crate) struct AppIndex {
    /// By lowercase app name.
    pub synonyms: HashMap<String, Keys>,
    /// By lowercase app id.
    pub ranks: HashMap<String, u32>,
    pub default_rank: u32,
}

static APP_INDEX: Mutex<Option<AppIndex>> = Mutex::new(None);

/// Reads the app index: in the `--start-apps` child, or on the shell thread when the child failed.
pub(crate) fn load_app_index() {
    let t = Instant::now();
    let synonyms = parse_synonyms(&read_index("appssynonyms.txt"));
    let (ranks, default_rank) = parse_ranks(&read_index("appsglobals.txt"));
    log!(
        "start search: {} apps with synonyms, {} ranks in {} ms",
        synonyms.len(),
        ranks.len(),
        t.elapsed().as_millis()
    );
    *APP_INDEX.lock().unwrap_or_else(|p| p.into_inner()) = Some(AppIndex { synonyms, ranks, default_rank });
}

/// Hands the app index over (`None` if there is none): the apps entry builder copies what the
/// installed apps need into the arena, and the rest is dropped.
pub(crate) fn take_app_index() -> Option<AppIndex> {
    APP_INDEX.lock().unwrap_or_else(|p| p.into_inner()).take()
}

/// Drops an app index nobody took (the Start menu data was released).
pub(crate) fn release_app_index() {
    *APP_INDEX.lock().unwrap_or_else(|p| p.into_inner()) = None;
}

// ------------------------------------------------------------------ settings

/// One setting the Start menu can find and open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Setting {
    pub name: String,
    pub lower: String,
    pub keys: Keys,
    pub rank: u32,
    /// What `ShellRequest::Launch` opens; shared when it is the Settings app (most entries).
    pub target: Cow<'static, str>,
    pub args: Option<String>,
}

/// What one short-lived "start-settings" thread shares with the two callers it serves (L3 T18): Start (`load_settings`,
/// `release_settings`) and the launcher (`ensure_settings_cache`). Each caller sets its flags, then starts the thread when
/// none runs; the thread takes the flags at the start of each run and runs again while one is set when it ends, so a
/// request that arrives during another caller's run is never lost. One static in the dock, one per test.
struct Shared {
    /// Start's list, decoded from the settings arena while Start holds the catalogue (`settings`).
    list: Mutex<Option<Arc<Vec<Setting>>>>,
    /// Start holds the catalogue (from `load_settings` to `release_settings`).
    held: AtomicBool,
    /// Asked of the thread: Start's list; the cache file made sure of; the cache file rebuilt whatever it holds.
    start: AtomicBool,
    ensure: AtomicBool,
    rebuild: AtomicBool,
    /// The key and the cache file's pages made ready for the launcher's first open (`prime_launcher_settings`).
    prime: AtomicBool,
    /// A settings thread runs (or is being started).
    thread: AtomicBool,
}

/// What one run of the settings thread serves: the flags it took.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct Asked {
    start: bool,
    ensure: bool,
    rebuild: bool,
    prime: bool,
}

impl Shared {
    const fn new() -> Shared {
        Shared {
            list: Mutex::new(None),
            held: AtomicBool::new(false),
            start: AtomicBool::new(false),
            ensure: AtomicBool::new(false),
            rebuild: AtomicBool::new(false),
            prime: AtomicBool::new(false),
            thread: AtomicBool::new(false),
        }
    }

    fn list(&self) -> std::sync::MutexGuard<'_, Option<Arc<Vec<Setting>>>> {
        self.list.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// A caller's request: its flags set first, then true when the caller must start the thread (none runs).
    fn ask(&self, a: Asked) -> bool {
        for (wanted, flag) in
            [(a.start, &self.start), (a.ensure, &self.ensure), (a.rebuild, &self.rebuild), (a.prime, &self.prime)]
        {
            if wanted {
                flag.store(true, SeqCst);
            }
        }
        !self.thread.swap(true, SeqCst)
    }

    /// The start of a run: what it serves.
    fn take(&self) -> Asked {
        Asked {
            start: self.start.swap(false, SeqCst),
            ensure: self.ensure.swap(false, SeqCst),
            rebuild: self.rebuild.swap(false, SeqCst),
            prime: self.prime.swap(false, SeqCst),
        }
    }

    /// The end of a run: true when the thread may exit. When something was asked meanwhile the thread takes itself back
    /// and runs again (false); if a caller started a new thread in between, that one serves it and this one exits.
    fn done(&self) -> bool {
        self.thread.store(false, SeqCst);
        let pending =
            self.start.load(SeqCst) || self.ensure.load(SeqCst) || self.rebuild.load(SeqCst) || self.prime.load(SeqCst);
        !(pending && self.thread.compare_exchange(false, true, SeqCst, SeqCst).is_ok())
    }

    /// Start holds the catalogue: true when its list is not there yet (the thread is asked for it).
    fn hold(&self) -> bool {
        self.held.store(true, SeqCst);
        self.list().is_none()
    }

    /// Start lets the catalogue go: its list is dropped (after the lock).
    fn release(&self) {
        self.held.store(false, SeqCst);
        let old = self.list().take();
        drop(old);
    }

    /// Start's list goes in only while Start holds the catalogue, decided under the lock `release_settings` clears it with
    /// (a release during the decode keeps nothing). What is not kept is dropped by the caller, after the lock.
    fn store_start(&self, list: &mut Option<Vec<Setting>>) -> bool {
        let mut slot = self.list();
        if !self.held.load(SeqCst) {
            return false;
        }
        *slot = list.take().map(Arc::new);
        true
    }
}

static SHARED: Shared = Shared::new();

/// The settings catalog; empty until the settings thread has decoded it for Start (and after `release_settings`).
pub(crate) fn settings() -> Arc<Vec<Setting>> {
    SHARED.list().clone().unwrap_or_default()
}

/// Start's settings list: held from now on and decoded from the cache file (built first by a child when the file is
/// missing or stale) on the settings thread, unless it is there. The menu uses it from the first search after
/// `StartSettingsReady`; nothing of it is done on the UI or shell thread.
pub(crate) fn load_settings() {
    if SHARED.hold() {
        request(Asked { start: true, ..Asked::default() });
    }
}

/// The launcher (L3 T18, T19): makes sure the cache file is there and matches this Windows, its language, its settings
/// files and this dock, for the next mapping (`rebuild`: rebuilt whatever the file holds, after one the launcher could not
/// map or check). The launcher never holds a list of its own: it maps the file at each open. When the file is good at
/// the end of the run, `StartSettingsReady` is posted (an open launcher maps it then).
pub(crate) fn ensure_settings_cache(rebuild: bool) {
    request(Asked { ensure: true, rebuild, ..Asked::default() });
}

/// The launcher (L3 KEY-11), when a chord is set (the dock's start included): what its first open would otherwise do on
/// the UI thread is done here, on the settings thread at background priority, once: the cache key computed and stored for
/// `expected_settings_key`, and the cache file's pages brought into the system cache (`touch_pages`: mapped read-only,
/// one byte read per page, unmapped; no heap, no check, nothing kept). Nothing is built, written or posted.
pub(crate) fn prime_launcher_settings() {
    request(Asked { prime: true, ..Asked::default() });
}

/// Maps `path` read-only and reads one byte of each 4 KiB page, so the next mapping of the file finds its pages in memory
/// (0 private bytes; the file is unmapped and closed on return). A missing or refused file is left alone.
pub(crate) fn touch_pages(path: &Path) {
    if let Ok(m) = crate::launcher_sys::MappedFile::open(path, CACHE_MAX) {
        let x = m.bytes().iter().step_by(4096).fold(0u8, |a, b| a ^ b);
        std::hint::black_box(x);
    }
}

fn request(a: Asked) {
    if SHARED.ask(a) {
        spawn_settings_thread();
    }
}

/// Who keeps the settings catalog loaded: Start alone (the launcher maps the cache file at each open instead, L3 T19).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Holder {
    Start,
}

/// `h` needs the settings catalog: marks it and decodes the catalog unless it is there.
pub(crate) fn hold_settings(h: Holder) {
    match h {
        Holder::Start => load_settings(),
    }
}

/// Frees Start's settings list (the Start menu data was released): the next Start menu decodes it again.
pub(crate) fn release_settings() {
    SHARED.release();
}

/// The one settings thread: COM (MTA) and background priority for the in-process fallback, then runs until nothing is
/// asked. It waits on the `--start-settings` child (up to 60 s): never the UI or the shell thread.
fn spawn_settings_thread() {
    let spawned = std::thread::Builder::new().name("start-settings".into()).spawn(|| {
        // SAFETY: COM for this thread only, paired with CoUninitialize below; background priority
        // is a hint that fails harmlessly.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        // SAFETY: pseudo-handle of the current thread.
        let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) };
        loop {
            let asked = SHARED.take();
            serve(&SHARED, asked, &mut RealIo);
            if SHARED.done() {
                break;
            }
        }
        if hr.is_ok() {
            // SAFETY: paired with the successful CoInitializeEx above.
            unsafe { CoUninitialize() };
        }
    });
    if let Err(e) = spawned {
        SHARED.thread.store(false, SeqCst);
        log!("start search: cannot start the settings thread: {e}");
    }
}

/// What a run of the settings thread does with the world: `RealIo` in the dock, a recorder in the tests.
trait SettingsIo {
    /// The cache file's key now (`catalog_key`), stored for the launcher (`expected_settings_key`).
    fn key(&mut self) -> Option<String>;
    /// The cache file whole (checked opener: one link, no reparse point; at most `CACHE_MAX`), if it can be read.
    fn read_cache(&mut self) -> Option<Vec<u8>>;
    /// A new cache file can be written (an elevated dock with its verified data folder).
    fn can_cache(&mut self) -> bool;
    /// What one `--start-settings` child printed, with its exit code.
    fn child(&mut self) -> Result<(i32, Vec<u8>), String>;
    /// The catalogue built and encoded in this process under `key` (the fallback when the child fails).
    fn build_here(&mut self, key: &str) -> Result<Vec<u8>, String>;
    fn write_cache(&mut self, bytes: &[u8]) -> Result<(), String>;
    /// `StartSettingsReady` to the UI: an open Start searches again, an open launcher maps the file.
    fn ready(&mut self);
    /// `prime_launcher_settings`: the key computed and stored (`key`), and with a key the cache file's pages read in.
    fn prime(&mut self) {
        let _ = self.key();
    }
}

/// A checked settings arena of this run, and whether the cache file holds exactly these bytes.
struct Made {
    bytes: Vec<u8>,
    layout: Layout,
    cached: bool,
}

/// One run of the settings thread, for what it took: the cache file used when it passes the whole check under the key
/// computed now (unless a rebuild was asked), else a new arena made (`make`); Start's list decoded from it when Start
/// still wants one; `StartSettingsReady` when the file is good for the launcher or Start's list came. Then the bytes go
/// (one block, back to the OS at once). A launcher-only run never fills Start's list.
fn serve(sh: &Shared, asked: Asked, io: &mut dyn SettingsIo) {
    let start = asked.start && sh.held.load(SeqCst) && sh.list().is_none();
    if !start && !asked.ensure {
        // A prime alone; with an ensure or Start's list the run computes the key and reads the file anyway.
        if asked.prime {
            io.prime();
        }
        return;
    }
    let t = Instant::now();
    let key = io.key();
    let mut got = None;
    if let (false, Some(k)) = (asked.rebuild, key.as_deref()) {
        got = io.read_cache().and_then(|bytes| match cache_use(&bytes, k) {
            CacheUse::Use(layout) => Some(Made { bytes, layout, cached: true }),
            CacheUse::Rebuild => None,
        });
    }
    // Without a key (Windows' build or its settings files unreadable), or where no file can be written (unelevated), a
    // new arena serves Start alone: the launcher's request makes none, so it never costs a child per open.
    if got.is_none() && (start || (key.is_some() && io.can_cache())) {
        got = make(io, key);
    }
    let Some(made) = got else { return };
    let mut stored = false;
    if start {
        let view = CatalogView::new(&made.bytes, &made.layout);
        let mut list = Some(start_rows(view));
        stored = sh.store_start(&mut list);
        if stored && made.cached {
            log!("start search: {} settings from the cache in {} ms", view.len(), t.elapsed().as_millis());
        }
        drop(list);
    }
    if stored || (asked.ensure && made.cached) {
        io.ready();
    }
}

/// A new settings arena: printed by an `--start-settings` child, whose key line must be the key computed right after it
/// exited (a settings file that changed while it ran: the child runs once more; a second mismatch is logged and the
/// bytes serve Start alone, never the in-process build, which would load the shell's resource code into the dock). A
/// child that cannot start, fails, or prints anything `arena::check` refuses: logged once, built in this process with the
/// same encoder. The arena is written to the cache when it carries the current key; a failed write (a sharing
/// violation, a full disk) leaves it to this run, and the launcher asks again at its next open.
fn make(io: &mut dyn SettingsIo, key: Option<String>) -> Option<Made> {
    static CHANGED: AtomicBool = AtomicBool::new(false);
    static FAILED: AtomicBool = AtomicBool::new(false);
    let t = Instant::now();
    let mut key_now = key;
    let mut verdict = ChildVerdict::Refused(String::new());
    for _ in 0..2 {
        let out = io.child();
        key_now = io.key();
        verdict = child_verdict(out, key_now.as_deref());
        if !matches!(verdict, ChildVerdict::Mismatch(..)) {
            break;
        }
    }
    let mut made = match verdict {
        ChildVerdict::Use(bytes, layout) => {
            log!("start search: {} settings in {} ms (child)", layout.entries(), t.elapsed().as_millis());
            Made { bytes, layout, cached: false }
        }
        ChildVerdict::Mismatch(bytes, layout) => {
            if !CHANGED.swap(true, SeqCst) {
                log!("start search: the settings files changed while the catalogue was built (twice): not cached this time");
            }
            return Some(Made { bytes, layout, cached: false });
        }
        ChildVerdict::Refused(why) => {
            if !FAILED.swap(true, SeqCst) {
                log!("start search: the settings process failed ({why}), building in-process");
            }
            let k = key_now.clone().unwrap_or_else(unkeyed_key);
            let built = io.build_here(&k).and_then(|bytes| {
                let layout =
                    arena::check(&bytes, ArenaKind::Settings, Some(&k), Caps::SETTINGS).map_err(|e| e.to_string())?;
                Ok(Made { bytes, layout, cached: false })
            });
            match built {
                Ok(m) => {
                    log!("start search: {} settings in {} ms", m.layout.entries(), t.elapsed().as_millis());
                    m
                }
                Err(e) => {
                    log!("start search: the settings catalogue was not built: {e}");
                    return None;
                }
            }
        }
    };
    // The child's and the fallback's garbage (or the read buffer of the pipe) is freed: the next hidden period trims.
    crate::mem::rearm();
    if key_now.is_some() {
        match io.write_cache(&made.bytes) {
            Ok(()) => made.cached = true,
            Err(e) => log!("start search: cannot write the settings cache: {e}"),
        }
    }
    Some(made)
}

/// What the dock makes of one child's output.
enum ChildVerdict {
    /// Checked, and keyed as the dock computes the key now (or no key is computed: the child's own line).
    Use(Vec<u8>, Layout),
    /// Checked, but its key line is not the key computed right after it exited.
    Mismatch(Vec<u8>, Layout),
    /// Not started, failed, or refused by `arena::check`: the reason.
    Refused(String),
}

/// The verdict on what an `--start-settings` child printed: exit code 0 and the whole output a settings arena under the
/// settings caps (header, sizes, every reference and order, the checksum); nothing is read from the untrusted bytes
/// before that check. Then its key line against `key_now`.
fn child_verdict(out: Result<(i32, Vec<u8>), String>, key_now: Option<&str>) -> ChildVerdict {
    let (code, bytes) = match out {
        Ok(o) => o,
        Err(e) => return ChildVerdict::Refused(e),
    };
    if code != 0 {
        return ChildVerdict::Refused(format!("exit code {code}"));
    }
    let layout = match arena::check(&bytes, ArenaKind::Settings, None, Caps::SETTINGS) {
        Ok(l) => l,
        Err(e) => return ChildVerdict::Refused(format!("{} bytes refused: {e}", bytes.len())),
    };
    match key_now {
        Some(k) if arena::key_line(&bytes) != Some(k) => ChildVerdict::Mismatch(bytes, layout),
        _ => ChildVerdict::Use(bytes, layout),
    }
}

/// What the settings thread does with the cache file's bytes.
#[derive(Debug, PartialEq, Eq)]
enum CacheUse {
    Use(Layout),
    Rebuild,
}

/// The cache file is used only when the whole of it passes `arena::check` under the expected key (the header, the key line,
/// every section and reference, the checksum): nothing is decided from the header and the key line alone, so a damaged
/// body or a file of another build, format or language is rebuilt.
fn cache_use(bytes: &[u8], expected: &str) -> CacheUse {
    match arena::check(bytes, ArenaKind::Settings, Some(expected), Caps::SETTINGS) {
        Ok(layout) => CacheUse::Use(layout),
        Err(_) => CacheUse::Rebuild,
    }
}

/// Start's rows of a checked settings arena, as `build_settings` makes them: the name and its lowercase, Start's keywords,
/// the rank, the launch target (the Settings app's is the shared constant) and the arguments.
fn start_rows(v: CatalogView<'_>) -> Vec<Setting> {
    (0..v.len())
        .map(|i| {
            let name = v.name(i);
            let (target, args) = v.target(i);
            Setting {
                name: name.to_string(),
                lower: name.to_lowercase(),
                keys: Keys::from_packed(v.start_keys(i)),
                rank: v.start_rank(i),
                target: if target == SETTINGS_APP {
                    Cow::Borrowed(SETTINGS_APP)
                } else {
                    Cow::Owned(target.to_string())
                },
                args: args.map(str::to_string),
            }
        })
        .collect()
}

/// The dock's side of a run.
struct RealIo;

impl SettingsIo for RealIo {
    fn key(&mut self) -> Option<String> {
        let key = current_key();
        store_key(key.as_deref());
        key
    }

    fn read_cache(&mut self) -> Option<Vec<u8>> {
        crate::datadir::read_file(&settings_cache_path()?, CACHE_MAX, true).ok()
    }

    fn can_cache(&mut self) -> bool {
        crate::datadir::writable() && settings_cache_path().is_some()
    }

    fn child(&mut self) -> Result<(i32, Vec<u8>), String> {
        let exe = crate::update::child_exe()?;
        crate::update::run_capture(&exe, &[SETTINGS_FLAG], SETTINGS_CHILD_TIMEOUT, CACHE_MAX)
    }

    fn build_here(&mut self, key: &str) -> Result<Vec<u8>, String> {
        encode_setting_list(key, &build_settings()).map_err(|e| e.to_string())
    }

    fn write_cache(&mut self, bytes: &[u8]) -> Result<(), String> {
        // Unelevated, the data folder is read-only: reading the cache is fine, writing it is not tried.
        if !crate::datadir::writable() {
            return Err("the data folder is read-only".into());
        }
        let path = settings_cache_path().ok_or("no cache folder")?;
        crate::datadir::write_atomic(&path, bytes, crate::datadir::FileAcl::Inherit)
    }

    fn ready(&mut self) {
        crate::shell_thread::request(crate::shell_thread::ShellRequest::StartSettingsReady);
    }

    fn prime(&mut self) {
        if self.key().is_some() {
            if let Some(path) = settings_cache_path() {
                touch_pages(&path);
            }
        }
    }
}

/// Whether a launcher chord is set, and the key the settings thread computed last (`None` before its first run, when no
/// key could be computed, or while no chord is set). One lock holds both, so a run that read "set" cannot store the key
/// after `chord_changed(false)` dropped it.
static EXPECTED: Mutex<(bool, Option<Arc<str>>)> = Mutex::new((false, None));

fn expected_slot() -> std::sync::MutexGuard<'static, (bool, Option<Arc<str>>)> {
    EXPECTED.lock().unwrap_or_else(|p| p.into_inner())
}

/// The launcher chord was set or switched off (`popup_launcher::on_config`). Off drops the stored key: nothing the
/// launcher needs stays resident without a chord.
pub(crate) fn chord_changed(on: bool) {
    let mut g = expected_slot();
    g.0 = on;
    if !on {
        g.1 = None;
    }
}

/// Each run of the settings thread stores the key it computed, so a settings file that changed during a session is seen
/// at the thread's next run, never turned into a permanent mismatch.
/// A launcher chord is set (`chord_changed`): only then does the key stay stored, because only the launcher reads it.
fn store_key(key: Option<&str>) {
    let mut g = expected_slot();
    if !g.0 {
        return;
    }
    if g.1.as_deref() != key {
        g.1 = key.map(Arc::from);
    }
}

/// The key the launcher checks the mapped cache file against (L3 T19): the one the settings thread stored, or computed
/// here (well under 1 ms: a folder listing, two registry values, the exe's size and time) and, while a chord is set,
/// stored when none is.
/// `None` when Windows' build or the settings files cannot be read: then no file is ever written.
pub(crate) fn expected_settings_key() -> Option<Arc<str>> {
    if let Some(k) = expected_slot().1.clone() {
        return Some(k);
    }
    let key: Arc<str> = current_key()?.into();
    let mut g = expected_slot();
    if g.0 {
        g.1 = Some(key.clone());
    }
    Some(key)
}

/// `<root>\cache\settings-catalog.bin` (admin-only, as the rest of the cache folder).
pub(crate) fn settings_cache_path() -> Option<PathBuf> {
    crate::datadir::cache_dir().map(|d| d.join(CACHE_NAME))
}

/// The flag of the child mode that builds the settings catalogue (`run_settings_child`); not combinable.
pub(crate) const SETTINGS_FLAG: &str = "--start-settings";
/// How long the settings thread waits for the child (it takes about a second; this thread waits, nobody else).
const SETTINGS_CHILD_TIMEOUT: Duration = Duration::from_secs(60);

/// `--start-settings`, in the child the settings thread starts: the settings catalogue built (thousands of resource
/// lookups, which load the shell's resource code into this short-lived process instead of the dock), folded and indexed
/// by the entry builder, and written to stdout as one settings arena under the key the child computes itself, before the
/// build (a settings file that changes during the build then makes a key the dock no longer computes, never a stale
/// catalogue under a new key). It touches no data folder, logger, mutex or console, reads no input and writes nothing but
/// stdout. Exit 0 = an arena written; 1 = COM, the encoder or stdout failed.
pub fn run_settings_child() -> i32 {
    use std::io::Write;
    // SAFETY: the first COM call of this thread (the child's main thread); paired with CoUninitialize below.
    let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if hr.is_err() {
        return 1;
    }
    // SAFETY: pseudo-handle of the current thread; a hint that fails harmlessly.
    let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) };
    let key = current_key().unwrap_or_else(unkeyed_key);
    let code = match encode_setting_list(&key, &build_settings()) {
        Ok(bytes) => {
            let mut stdout = std::io::stdout().lock();
            match stdout.write_all(&bytes).and_then(|()| stdout.flush()) {
                Ok(()) => 0,
                Err(_) => 1,
            }
        }
        Err(_) => 1,
    };
    // SAFETY: paired with the successful CoInitializeEx above.
    unsafe { CoUninitialize() };
    code
}

/// `@dll,-id` / `@{package?ms-resource://...}` resolved in the display language; plain text as it is.
fn resolve(reference: &str) -> Option<String> {
    if !reference.starts_with('@') {
        return Some(reference.to_string());
    }
    let mut buf = [0u16; 2048];
    // SAFETY: `buf` is writable and its length is passed by the wrapper; the source outlives the call.
    unsafe { SHLoadIndirectString(&HSTRING::from(reference), &mut buf, None) }.ok()?;
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..n]).trim().to_string()).filter(|s| !s.is_empty() && !s.starts_with('@'))
}

/// Keywords of an entry: one reference, or several glued together (`@@a.dll,-1@a.dll,-2`).
fn resolve_keywords(raw: &str) -> Vec<String> {
    if raw.starts_with("@{") || !raw.starts_with('@') {
        return resolve(raw).map(|s| split_keywords(&s)).unwrap_or_default();
    }
    raw.split('@')
        .filter(|p| !p.is_empty())
        .filter_map(|p| resolve(&format!("@{p}")))
        .flat_map(|s| split_keywords(&s))
        .collect()
}

/// Reads the largest `AllSystemSettings_*.xml` (the variants differ by a few entries) and turns
/// every entry that has a name and can be opened into a `Setting`. Entries with the same name
/// (Windows lists variants of one setting) are merged, the best rank kept.
fn build_settings() -> Vec<Setting> {
    let Some(windir) = windows_dir() else { return Vec::new() };
    let dir = windir.join(r"ImmersiveControlPanel\Settings");
    let xml = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| is_catalog_xml(&e.file_name().to_string_lossy()))
        .max_by_key(|e| e.metadata().map_or(0, |m| m.len()))
        .and_then(|e| read_text(&e.path()));
    let Some(xml) = xml else {
        log!("start search: no settings catalog in {}", dir.display());
        return Vec::new();
    };
    let synonyms = parse_synonyms(&read_index("settingssynonyms.txt"));
    let (ranks, default_rank) = parse_ranks(&read_index("settingsglobals.txt"));
    // Built loose, packed at the end (`Keys::tidy`, exact-size strings): what stays is small.
    let mut by_name: HashMap<String, Setting> = HashMap::new();
    for raw in parse_settings_xml(&xml) {
        let Some((target, args)) = setting_launch(&raw, &windir) else { continue };
        let Some(name) = resolve(&raw.description) else { continue };
        let lower = name.to_lowercase();
        let rank = ranks.get(&raw.id.to_lowercase()).copied().unwrap_or(default_rank);
        let target: Cow<'static, str> =
            if target == SETTINGS_APP { Cow::Borrowed(SETTINGS_APP) } else { Cow::Owned(target) };
        let keywords = resolve_keywords(&raw.keywords);
        let entry = by_name.entry(lower.clone()).or_insert_with(|| {
            let keys = synonyms.get(&lower).cloned().unwrap_or_default();
            Setting { name, lower, keys, rank: u32::MAX, target: target.clone(), args: args.clone() }
        });
        for k in &keywords {
            entry.keys.push(k, XML_KEYWORD_PENALTY);
        }
        if rank < entry.rank {
            (entry.rank, entry.target, entry.args) = (rank, target, args);
        }
    }
    let mut list: Vec<Setting> = by_name
        .into_values()
        .map(|mut s| {
            s.keys = s.keys.tidy();
            s.name.shrink_to_fit();
            s.lower.shrink_to_fit();
            if let Some(a) = s.args.as_mut() {
                a.shrink_to_fit();
            }
            s
        })
        .collect();
    list.sort_by(|a, b| a.lower.cmp(&b.lower));
    list.shrink_to_fit();
    list
}

/// True for the name of one of the catalog's XML files.
fn is_catalog_xml(name: &str) -> bool {
    let n = name.to_lowercase();
    n.starts_with("allsystemsettings_") && n.ends_with(".xml")
}

// ------------------------------------------------------------------ the cache file of the catalog

/// The largest cache file read, mapped or written, and the most read back from the settings child (version 2: one settings
/// arena, `arena::check` under `Caps::SETTINGS`).
pub(crate) const CACHE_MAX: u64 = Caps::SETTINGS.bytes as u64;
const CACHE_NAME: &str = "settings-catalog.bin";
const NT_KEY: windows::core::PCWSTR = windows::core::w!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion");

/// `CurrentBuildNumber` and `UBR` of the running Windows ("26100.1742"); empty where unreadable
/// (`catalog_key_of` then gives no key: an unreadable build means no cache at all).
fn windows_build() -> String {
    let mut buf = [0u16; 32];
    let mut size = std::mem::size_of_val(&buf) as u32;
    // SAFETY: `buf`/`size` are valid writable locals of the declared size (in bytes); the key and
    // value names are static NUL-terminated wide strings; RRF_RT_REG_SZ makes the call add a NUL.
    let r = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            NT_KEY,
            windows::core::w!("CurrentBuildNumber"),
            RRF_RT_REG_SZ | RRF_SUBKEY_WOW6464KEY,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if r != ERROR_SUCCESS {
        return String::new();
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let build = String::from_utf16_lossy(&buf[..n]);
    let mut ubr = 0u32;
    let mut size = 4u32;
    // SAFETY: as above, a DWORD of 4 bytes.
    let r = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            NT_KEY,
            windows::core::w!("UBR"),
            RRF_RT_REG_DWORD | RRF_SUBKEY_WOW6464KEY,
            None,
            Some((&mut ubr as *mut u32).cast()),
            Some(&mut size),
        )
    };
    if r == ERROR_SUCCESS {
        format!("{build}.{ubr}")
    } else {
        build
    }
}

/// What the cache depends on, one line: the dock's own identity (version, size and modification
/// time of its exe: a new build rebuilds the catalog once), the Windows build, the UI language, and name, size and
/// modification time of each `AllSystemSettings_*.xml`, then the arena's format (`|fmt <ARENA_FORMAT>`: a cache of another
/// layout is rebuilt once). Printable ASCII only, at most an arena key line long. `None` when the build is unknown or no
/// XML file is there (nothing to key on: nothing cached), or when the line would be too long.
fn catalog_key_of(dock: &str, build: &str, lang: u16, files: &[(String, u64, u64)]) -> Option<String> {
    if build.is_empty() || files.is_empty() {
        return None;
    }
    let printable = |c: &char| c.is_ascii_graphic() && *c != '|';
    // "l3s1": the settings keywords carry `XML_KEYWORD_PENALTY` since L3; a file written with the old penalty is rebuilt once.
    let mut key: String = dock.chars().filter(printable).collect();
    key.push('|');
    key.extend(build.chars().filter(printable));
    key.push_str(&format!("|{lang:04x}|l3s1"));
    for (name, len, mtime) in files {
        key.push('|');
        key.extend(name.chars().filter(printable));
        key.push_str(&format!(":{len}:{mtime}"));
    }
    key.push_str(&format!("|fmt {}", arena::ARENA_FORMAT));
    (key.len() <= arena::KEY_MAX).then_some(key)
}

/// The key line of an arena built without a key (Windows' build or its settings files unreadable): it serves Start for
/// one run and is never written to the cache.
fn unkeyed_key() -> String {
    format!("unkeyed|fmt {}", arena::ARENA_FORMAT)
}

/// The cache key of this Windows, its language, its settings files and this dock, now.
fn current_key() -> Option<String> {
    catalog_key(&windows_dir()?.join(r"ImmersiveControlPanel\Settings"))
}

fn catalog_key(dir: &Path) -> Option<String> {
    let mut files: Vec<(String, u64, u64)> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| is_catalog_xml(&e.file_name().to_string_lossy()))
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            let t = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
            Some((e.file_name().to_string_lossy().to_lowercase(), m.len(), t))
        })
        .collect();
    // whole tuples: equal ones are the same, so an unstable sort gives the same list
    files.sort_unstable();
    // SAFETY: no arguments, no preconditions.
    let lang = unsafe { GetUserDefaultUILanguage() };
    catalog_key_of(&dock_identity(), &windows_build(), lang, &files)
}

/// Version, size and modification time of the running exe: the cache never outlives a new dock.
fn dock_identity() -> String {
    let (len, mtime) = std::env::current_exe()
        .and_then(std::fs::metadata)
        .map(|m| {
            (
                m.len(),
                m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs()),
            )
        })
        .unwrap_or((0, 0));
    format!("{}:{len}:{mtime}", env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synonyms_keep_every_row_of_a_name() {
        let text = "*|\t*|.exe\t9120\r\n\
                    *|Connessione Desktop remoto\t*|mstsc\t2518\r\n\
                    *|connessione desktop remoto\t*|RDP\t2210\r\n\
                    *|connessione desktop remoto\t*|remtoe\t7740\r\n\
                    *|powerpoint\t*|ppt:wux:ppt\t3727\r\n\
                    garbage\r\n";
        let syn = parse_synonyms(text);
        assert_eq!(syn.len(), 2, "rows without a name and garbage are skipped");
        let rdp = syn["connessione desktop remoto"].iter().collect::<Vec<_>>();
        assert_eq!(rdp, vec![("mstsc", 2518), ("rdp", 2210), ("remtoe", 7740)], "every row kept, typos too");
        assert_eq!(syn["powerpoint"].iter().collect::<Vec<_>>(), vec![("ppt", 3727)]);
    }

    #[test]
    fn keys_are_packed_and_tidied() {
        let mut k = Keys::default();
        assert!(k.is_empty() && Keys::none().is_empty() && Keys::none().iter().next().is_none());
        k.push(" Wi-Fi ", 500);
        k.push("rete", 500);
        k.push("wi-fi", 120);
        k.push("", 1);
        k.push("a\tb", 1);
        assert_eq!(k.iter().collect::<Vec<_>>(), vec![("wi-fi", 500), ("rete", 500), ("wi-fi", 120)]);
        // Sorted, each keyword once with its best penalty.
        assert_eq!(k.tidy().iter().collect::<Vec<_>>(), vec![("rete", 500), ("wi-fi", 120)]);
        assert_eq!(Keys::from(&[("b", 2), ("a", 1)][..]).iter().collect::<Vec<_>>(), vec![("b", 2), ("a", 1)]);
    }

    #[test]
    fn ranks_and_conversions() {
        let (ranks, default) = parse_ranks("Chrome\t3266\r\n{1AC1}\\cmd.exe\t3525\r\nchrome\t3000\r\nbad\r\nx\ty\r\n");
        assert_eq!((ranks["chrome"], ranks[r"{1ac1}\cmd.exe"], ranks.len()), (3000, 3525, 2));
        assert_eq!(default, 3526);
        assert_eq!(parse_ranks("").1, 0);
        let conv =
            "market\t~\t~\t0\r\nspelling\truhn\trun\t10003\r\nspelling\trkdp\trdp\t10237\r\nspelling\ta\ta\t1\r\n";
        assert_eq!(convert(conv, "rkdp"), Some("rdp"));
        assert_eq!(convert(conv, " ruhn "), Some("run"));
        assert_eq!(convert(conv, "rdp"), None);
        assert_eq!(convert(conv, "a"), None, "a rewrite to itself is nothing");
        assert_eq!(convert(conv, ""), None);
    }

    const CONV: &str = "market\t~\t~\t0\r\nspelling\truhn\trun\t10003\r\nspelling\trkdp\trdp\t10237\r\nspelling\ta\ta\t1\r\nspelling\tx\n";

    #[test]
    fn streamed_convert_equals_whole_file() {
        for q in ["rkdp", " ruhn ", "rdp", "a", "", "x", "RKDP", "market", "spelling"] {
            let typed = q.trim().to_lowercase();
            assert_eq!(
                convert_stream(std::io::Cursor::new(CONV.as_bytes()), &typed),
                convert(CONV, &typed).map(str::to_string),
                "{q:?}"
            );
        }
        // No final newline, an invalid byte inside a line, a line longer than any row of Windows' file.
        let odd = b"spelling\tzz\tyy\t1\r\nspelling\tq\xff\tw\t1\r\nspelling\tlast\tfin";
        assert_eq!(convert_stream(std::io::Cursor::new(&odd[..]), "zz").as_deref(), Some("yy"));
        assert_eq!(convert_stream(std::io::Cursor::new(&odd[..]), "last").as_deref(), Some("fin"));
        let mut long = vec![b'z'; 100_000];
        long.extend_from_slice(b"\nspelling\tab\tcd\t1\n");
        assert_eq!(convert_stream(std::io::Cursor::new(long), "ab").as_deref(), Some("cd"), "a long line is skipped");
    }

    #[test]
    fn conversions_reader_is_64_kib() {
        assert_eq!(conversions_reader(std::io::empty()).capacity(), 64 * 1024);
    }

    #[test]
    fn typo_rewrite_peak_is_bounded() {
        // MEM-6: about 1.4 MB of rows, none for the query. The file is never held whole.
        let row = "spelling\tsomething\tsomewhere\t10003\r\n";
        let text: String = (0..1_400_000 / row.len()).map(|_| row).collect();
        assert!(text.len() >= 1_400_000 - row.len());
        let (hit, peak) = crate::launcher::testkit::peak_bytes_during(|| {
            convert_stream(std::io::Cursor::new(text.as_bytes()), "nothing like it")
        });
        assert_eq!(hit, None);
        assert!(peak <= 72 * 1024, "peak {peak} bytes");
    }

    #[test]
    fn settings_xml_and_what_opens_each_entry() {
        let xml = r#"<?xml version="1.0"?><PCSettings>
  <SearchableContent ExcludeWithFeature="X">
    <Filename>AAA_SettingsPageRemoteDesktop</Filename>
    <SettingIdentity><SettingPaths><Path>
      <Node Type="Page">SettingsPageRemoteDesktop</Node><Node Type="Group">G</Node></Path></SettingPaths></SettingIdentity>
    <SettingInformation><Description>@{windows?ms-resource://a/b/Description}</Description>
      <HighKeywords>@{windows?ms-resource://a/b/HighKeywords}</HighKeywords></SettingInformation>
  </SearchableContent>
  <SearchableContent>
    <Filename>Classic_{1}</Filename>
    <ApplicationInformation><DeepLink>%windir%\system32\rundll32.exe shell32.dll,Control_RunDLL a.cpl,,5 &amp; x</DeepLink></ApplicationInformation>
    <SettingInformation><Description>@shell32.dll,-24180</Description><Keywords>@@shell32.dll,-1@shell32.dll,-2</Keywords></SettingInformation>
  </SearchableContent>
  <SearchableContent><Filename>NoDescription</Filename></SearchableContent>
</PCSettings>"#;
        let raw = parse_settings_xml(xml);
        assert_eq!(raw.len(), 2);
        assert_eq!(raw[0].page.as_deref(), Some("SettingsPageRemoteDesktop"));
        assert_eq!(raw[0].keywords, "@{windows?ms-resource://a/b/HighKeywords}");
        assert_eq!(raw[1].keywords, "@@shell32.dll,-1@shell32.dll,-2");
        let win = Path::new(r"C:\Windows");
        assert_eq!(
            setting_launch(&raw[0], win),
            Some((SETTINGS_APP.to_string(), Some("page=SettingsPageRemoteDesktop".into())))
        );
        assert_eq!(
            setting_launch(&raw[1], win),
            Some((
                r"C:\Windows\system32\rundll32.exe".to_string(),
                Some("shell32.dll,Control_RunDLL a.cpl,,5 & x".into())
            ))
        );
        let link = |l: &str| {
            let r = RawSetting {
                id: "i".into(),
                description: "d".into(),
                keywords: String::new(),
                page: None,
                deep_link: Some(l.into()),
            };
            setting_launch(&r, win)
        };
        assert_eq!(
            link(r"Microsoft.PowerOptions\pagePlanSettings"),
            Some((
                r"C:\Windows\System32\control.exe".to_string(),
                Some("/name Microsoft.PowerOptions /page pagePlanSettings".into())
            ))
        );
        assert_eq!(link("Microsoft.Fonts").unwrap().1.as_deref(), Some("/name Microsoft.Fonts"));
        assert_eq!(link("shell:::{A}"), Some(("shell:::{A}".to_string(), None)));
        // Not openable from outside, or not inside the Windows folder: refused.
        assert_eq!(link("AccountProtection"), None);
        assert_eq!(link("ms-contact-support:x"), None);
        assert_eq!(link(r"%windir%\..\evil\x.exe"), None);
        assert_eq!(link(r"%temp%\x.exe"), None);
        assert_eq!(link(r"%windir%\system32\x.cmd"), None);
        assert_eq!(link(r#"Microsoft.Fonts" /x"#), None);
        assert_eq!(split_keywords("Wi-Fi|rete ; LAN||"), vec!["wi-fi", "rete", "lan"]);
    }

    fn sample(n: usize) -> Vec<Setting> {
        (0..n)
            .map(|i| {
                let name = format!("Setting Ünï {i}");
                let mut keys = Keys::default();
                for k in 0..(i % 4) {
                    keys.push(&format!("kw{i}x{k}"), XML_KEYWORD_PENALTY);
                }
                Setting {
                    lower: name.to_lowercase(),
                    name,
                    keys: keys.tidy(),
                    rank: i as u32 * 7,
                    target: if i % 3 == 0 {
                        Cow::Owned(format!(r"C:\Windows\System32\t{i}.exe"))
                    } else {
                        Cow::Borrowed(SETTINGS_APP)
                    },
                    args: (i % 5 != 0).then(|| format!("page=p{i}")),
                }
            })
            .collect()
    }

    /// The key of the sample cache (the dock's identity first, the arena's format last).
    fn key() -> String {
        catalog_key_of("1.5.0:9:9", "26100.1742", 0x409, &[("allsystemsettings_x.xml".to_string(), 100, 5)]).unwrap()
    }

    /// The same, after the settings file changed.
    fn key2() -> String {
        catalog_key_of("1.5.0:9:9", "26100.1742", 0x409, &[("allsystemsettings_x.xml".to_string(), 100, 6)]).unwrap()
    }

    /// The version 2 cache file (one settings arena) of `list` under `key`, as the child prints it.
    fn arena_of(key: &str, list: &[Setting]) -> Vec<u8> {
        encode_setting_list(key, list).unwrap()
    }

    fn child_out(key: &str, n: usize) -> Result<(i32, Vec<u8>), String> {
        Ok((0, arena_of(key, &sample(n))))
    }

    fn rows_of(bytes: &[u8], key: &str) -> Vec<Setting> {
        let CacheUse::Use(layout) = cache_use(bytes, key) else { panic!("the arena checks") };
        start_rows(CatalogView::new(bytes, &layout))
    }

    #[test]
    fn round_trip() {
        let list = sample(300);
        let bytes = arena_of(&key(), &list);
        assert!(bytes.starts_with(b"mini-oled-dock settings-catalog 2\n"));
        let back = rows_of(&bytes, &key());
        assert_eq!(back, list);
        assert!(
            back.iter().filter(|s| matches!(s.target, Cow::Borrowed(_))).count() > 100,
            "the shared target stays shared"
        );
        assert_eq!(rows_of(&arena_of(&key(), &[]), &key()), Vec::new());
    }

    #[test]
    fn invalidated_by_build_or_language() {
        let files = vec![("allsystemsettings_a.xml".to_string(), 10u64, 20u64)];
        let k = catalog_key_of("1.5.0:9:9", "26100.1742", 0x409, &files).unwrap();
        let bytes = arena_of(&k, &sample(5));
        assert!(matches!(cache_use(&bytes, &k), CacheUse::Use(_)));
        for other in [
            catalog_key_of("1.5.1:9:9", "26100.1742", 0x409, &files),
            catalog_key_of("1.5.0:10:9", "26100.1742", 0x409, &files),
            catalog_key_of("1.5.0:9:10", "26100.1742", 0x409, &files),
            catalog_key_of("1.5.0:9:9", "26100.1743", 0x409, &files),
            catalog_key_of("1.5.0:9:9", "26200.1742", 0x409, &files),
            catalog_key_of("1.5.0:9:9", "26100.1742", 0x410, &files),
            catalog_key_of("1.5.0:9:9", "26100.1742", 0x409, &[("allsystemsettings_a.xml".to_string(), 11, 20)]),
            catalog_key_of("1.5.0:9:9", "26100.1742", 0x409, &[("allsystemsettings_a.xml".to_string(), 10, 21)]),
            catalog_key_of("1.5.0:9:9", "26100.1742", 0x409, &[("allsystemsettings_b.xml".to_string(), 10, 20)]),
        ] {
            let other = other.unwrap();
            assert_ne!(other, k);
            assert_eq!(cache_use(&bytes, &other), CacheUse::Rebuild, "{other}");
        }
        // A key written before the XML sentinel (no salt) does not match.
        let old = k.replace("|l3s1", "");
        assert_ne!(old, k);
        assert_eq!(cache_use(&arena_of(&old, &sample(5)), &k), CacheUse::Rebuild, "a pre-L3 cache is rebuilt once");
        assert!(catalog_key_of("1.5.0:9:9", "", 0x409, &files).is_none(), "unknown build: no cache");
        assert!(catalog_key_of("1.5.0:9:9", "26100.1", 0x409, &[]).is_none(), "no XML: no cache");
        // The line is printable ASCII whatever a file is called, and one past an arena's key line gives no key at all.
        let odd = catalog_key_of("1.5.0:9:9", "26100.1742", 0x409, &[("allsystemsettings_é|\nx.xml".into(), 1, 2)]);
        assert!(odd.as_deref().is_some_and(|o| o.contains("|allsystemsettings_x.xml:1:2|") && o.is_ascii()), "{odd:?}");
        let many: Vec<(String, u64, u64)> =
            (0..40).map(|i| (format!("allsystemsettings_{}{i}.xml", "x".repeat(40)), 1, 2)).collect();
        assert!(catalog_key_of("1.5.0:9:9", "26100.1742", 0x409, &many).is_none());
    }

    #[test]
    fn the_cache_key_is_stored_only_while_a_chord_is_set() {
        // One test owns the statics: the others never touch them.
        chord_changed(false);
        store_key(Some("k1"));
        assert!(expected_slot().1.is_none(), "no chord: nothing stored");
        chord_changed(true);
        store_key(Some("k1"));
        assert_eq!(expected_slot().1.as_deref(), Some("k1"));
        store_key(Some("k2"));
        assert_eq!(expected_slot().1.as_deref(), Some("k2"), "each run replaces it");
        chord_changed(false);
        assert!(expected_slot().1.is_none(), "the chord off drops it");
        store_key(Some("k3"));
        assert!(expected_slot().1.is_none(), "and a later run does not bring it back");
    }

    /// L3 T18: the key ends with the arena's format, so a cache file of another layout is rebuilt once.
    #[test]
    fn settings_key_has_the_arena_format() {
        let k = key();
        let fmt = format!("|fmt {}", arena::ARENA_FORMAT);
        assert!(k.ends_with(&fmt) && k.starts_with("1.5.0:9:9|26100.1742|0409|l3s1|"), "{k}");
        let other = k.replace(&fmt, &format!("|fmt {}", arena::ARENA_FORMAT + 1));
        assert_eq!(cache_use(&arena_of(&other, &sample(3)), &k), CacheUse::Rebuild);
        assert!(matches!(cache_use(&arena_of(&k, &sample(3)), &k), CacheUse::Use(_)));
        assert!(unkeyed_key().ends_with(&fmt));
        assert_ne!(unkeyed_key(), k);
    }

    /// L3 T18: the cache file is used only when the whole of it passes `arena::check` under the expected key.
    #[test]
    fn cache_decision() {
        let k = key();
        let good = arena_of(&k, &sample(40));
        assert!(matches!(cache_use(&good, &k), CacheUse::Use(l) if l.entries() == 40));
        // L1b-8's version 1 file.
        let mut v1 = b"mini-oled-dock settings-catalog 1\n".to_vec();
        v1.extend_from_slice(k.as_bytes());
        v1.push(b'\n');
        v1.extend_from_slice(&[0; 12]);
        for (bad, why) in [
            (v1, "version 1"),
            (arena_of(&key2(), &sample(40)), "another key"),
            (good[..20].to_vec(), "a truncated header"),
            (Vec::new(), "an empty file"),
            (good[..good.len() - 1].to_vec(), "a truncated body"),
        ] {
            assert_eq!(cache_use(&bad, &k), CacheUse::Rebuild, "{why}");
        }
        // Header and key line as expected, a byte of a launch target damaged: nothing is decided from the head alone.
        let at = good.windows(6).position(|w| w == b"t0.exe").unwrap();
        let mut body = good.clone();
        body[at] ^= 0x01;
        assert!(body.starts_with(b"mini-oled-dock settings-catalog 2\n") && arena::key_line(&body) == Some(k.as_str()));
        assert_eq!(cache_use(&body, &k), CacheUse::Rebuild);
    }

    #[test]
    fn corrupt_ignored() {
        let k = key();
        let bytes = arena_of(&k, &sample(40));
        assert_eq!(cache_use(&[], &k), CacheUse::Rebuild);
        assert_eq!(cache_use(&bytes[..bytes.len() - 1], &k), CacheUse::Rebuild);
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert_eq!(cache_use(&trailing, &k), CacheUse::Rebuild);
        for i in (0..bytes.len()).step_by(7) {
            let mut b = bytes.clone();
            b[i] ^= 0x55;
            assert_eq!(cache_use(&b, &k), CacheUse::Rebuild, "byte {i}");
        }
    }

    #[test]
    fn corrupt_ignored_seeded_100000() {
        let k = key();
        let good = arena_of(&k, &sample(12));
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..100_000 {
            let mut b = if next() % 4 == 0 { Vec::new() } else { good.clone() };
            match next() % 4 {
                0 => b.truncate((next() as usize) % (b.len() + 1)),
                1 => (0..1 + next() % 4).for_each(|_| {
                    if !b.is_empty() {
                        let i = (next() as usize) % b.len();
                        b[i] = next() as u8;
                    }
                }),
                2 => (0..(next() % 64)).for_each(|_| b.push(next() as u8)),
                _ => b = (0..(next() % 200)).map(|_| next() as u8).collect(),
            }
            // Must never panic; anything it accepts is exactly what the encoder wrote, and decodes.
            if let CacheUse::Use(layout) = cache_use(&b, &k) {
                assert_eq!(b, good);
                assert_eq!(start_rows(CatalogView::new(&b, &layout)).len(), 12);
            }
        }
    }

    /// L3 T18: Start's rows of the arena are the ones `build_settings` made: names, Start's keywords as written, ranks, the
    /// shared Settings-app target as `Cow::Borrowed`, a Control Panel entry with arguments, one without.
    #[test]
    fn start_rows_from_the_arena() {
        let mut access = Keys::default();
        access.push("Contrasto elevato", 3);
        access.push("ipovedenti", XML_KEYWORD_PENALTY);
        let list = vec![
            Setting {
                name: "Accessibilità".into(),
                lower: "accessibilità".into(),
                keys: access.tidy(),
                rank: 2,
                target: Cow::Borrowed(SETTINGS_APP),
                args: Some("page=SettingsPageEaseOfAccess".into()),
            },
            Setting {
                name: "Mouse".into(),
                lower: "mouse".into(),
                keys: Keys::default(),
                rank: 9,
                target: Cow::Owned(r"C:\Windows\System32\control.exe".into()),
                args: Some("/name Microsoft.Mouse".into()),
            },
            Setting {
                name: "Schermo".into(),
                lower: "schermo".into(),
                keys: Keys::default(),
                rank: 1,
                target: Cow::Borrowed(SETTINGS_APP),
                args: None,
            },
        ];
        let rows = rows_of(&arena_of(&key(), &list), &key());
        assert_eq!(rows, list);
        assert!(matches!(rows[0].target, Cow::Borrowed(t) if t == SETTINGS_APP), "the shared constant, not a copy");
        assert!(matches!(&rows[1].target, Cow::Owned(t) if t.ends_with("control.exe")));
        assert_eq!((rows[1].args.as_deref(), rows[2].args.as_deref()), (Some("/name Microsoft.Mouse"), None));
        let keys: Vec<(&str, u32)> = rows[0].keys.iter().collect();
        assert_eq!(keys, [("contrasto elevato", 3), ("ipovedenti", XML_KEYWORD_PENALTY)], "as Start searches them");
    }

    /// A recorder of a settings thread run's world: the keys `key` answers in turn (the last one stays), a cache file,
    /// the children's outputs, what was built, written and posted, and a caller acting while a child runs or the file is
    /// read.
    #[derive(Default)]
    struct FakeIo<'a> {
        keys: Vec<Option<String>>,
        file: Option<Vec<u8>>,
        children: std::collections::VecDeque<Result<(i32, Vec<u8>), String>>,
        here: Vec<Setting>,
        write_fails: bool,
        /// No cache file can be written here (an unelevated dock).
        read_only: bool,
        calls: Vec<String>,
        /// Start asks (as `load_settings` does) while the first child runs.
        start_during_child: Option<&'a Shared>,
        /// Start lets go (as `release_settings` does) while the file is read.
        release_on_read: Option<&'a Shared>,
    }

    impl SettingsIo for FakeIo<'_> {
        fn key(&mut self) -> Option<String> {
            self.calls.push("key".into());
            let k = self.keys.first().cloned().flatten();
            if self.keys.len() > 1 {
                self.keys.remove(0);
            }
            k
        }
        fn read_cache(&mut self) -> Option<Vec<u8>> {
            self.calls.push("read".into());
            if let Some(sh) = self.release_on_read.take() {
                sh.release();
            }
            self.file.clone()
        }
        fn can_cache(&mut self) -> bool {
            !self.read_only
        }
        fn child(&mut self) -> Result<(i32, Vec<u8>), String> {
            self.calls.push("child".into());
            if let Some(sh) = self.start_during_child.take() {
                assert!(sh.hold());
                assert!(!sh.ask(Asked { start: true, ..Asked::default() }), "a thread runs: no second one");
            }
            self.children.pop_front().unwrap_or_else(|| Err("no child".into()))
        }
        fn build_here(&mut self, k: &str) -> Result<Vec<u8>, String> {
            self.calls.push(format!("build {}", k == key()));
            encode_setting_list(k, &self.here).map_err(|e| e.to_string())
        }
        fn write_cache(&mut self, bytes: &[u8]) -> Result<(), String> {
            self.calls.push("write".into());
            if self.write_fails || self.read_only {
                return Err("sharing violation".into());
            }
            self.file = Some(bytes.to_vec());
            Ok(())
        }
        fn ready(&mut self) {
            self.calls.push("ready".into());
        }
    }

    const ENSURE: Asked = Asked { start: false, ensure: true, rebuild: false, prime: false };
    const START: Asked = Asked { start: true, ensure: false, rebuild: false, prime: false };
    const PRIME: Asked = Asked { start: false, ensure: false, rebuild: false, prime: true };

    /// L3 KEY-11: a prime alone computes and stores the key and nothing else (no read, child, write or post); with an
    /// ensure in the same run the ensure's own key and read serve it.
    #[test]
    fn a_prime_only_computes_the_key() {
        let sh = Shared::new();
        assert!(sh.ask(PRIME), "the chord was set: the thread starts");
        let mut io = FakeIo { keys: vec![Some(key())], ..FakeIo::default() };
        let asked = sh.take();
        assert_eq!(asked, PRIME);
        serve(&sh, asked, &mut io);
        assert_eq!(io.calls, ["key"]);
        assert!(sh.done(), "nothing else asked: the thread exits");
        let mut io = FakeIo { keys: vec![Some(key())], ..FakeIo::default() };
        serve(&sh, Asked { prime: true, ..ENSURE }, &mut io);
        assert_eq!(io.calls.first().map(String::as_str), Some("key"));
        assert!(io.calls.iter().any(|c| c == "read"), "the ensure reads the file: {:?}", io.calls);
    }

    /// L3 KEY-11: `touch_pages` reads a file through a mapping and leaves nothing open (the file can be deleted at once).
    #[test]
    fn touched_file_is_released() {
        let dir = std::env::temp_dir().join(format!("dock_touch_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings-catalog.bin");
        std::fs::write(&path, vec![7u8; 3 * 4096 + 5]).unwrap();
        touch_pages(&path);
        touch_pages(&dir.join("none.bin"));
        std::fs::remove_file(&path).expect("unmapped and closed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// L3 T18: one thread serves both callers; a Start request that arrives while a launcher-only run works is served by
    /// the same thread right after it, from the file that run wrote, and a launcher-only run never fills Start's list.
    #[test]
    fn start_request_during_a_launcher_ensure_is_served() {
        let sh = Shared::new();
        assert!(sh.ask(ENSURE), "the launcher asks: the thread starts");
        let mut io = FakeIo {
            keys: vec![Some(key())],
            children: [child_out(&key(), 30)].into(),
            start_during_child: Some(&sh),
            ..FakeIo::default()
        };
        let asked = sh.take();
        assert_eq!(asked, ENSURE);
        serve(&sh, asked, &mut io);
        assert_eq!(io.calls, ["key", "read", "child", "key", "write", "ready"], "no file: one child, written, told");
        assert!(sh.list().is_none(), "a launcher-only run never fills Start's list");
        assert!(!sh.done(), "Start asked meanwhile: the thread runs again");
        let asked = sh.take();
        assert_eq!(asked, START);
        io.calls.clear();
        serve(&sh, asked, &mut io);
        assert_eq!(io.calls, ["key", "read", "ready"], "from the file just written, once, no child");
        assert_eq!(sh.list().as_ref().map(|l| l.len()), Some(30));
        assert!(sh.done(), "nothing left: it exits");
        // A valid file serves an ensure without a child; a rebuild never reads it.
        io.calls.clear();
        serve(&sh, ENSURE, &mut io);
        assert_eq!(io.calls, ["key", "read", "ready"]);
        io.calls.clear();
        io.children.push_back(child_out(&key(), 31));
        serve(&sh, Asked { rebuild: true, ..ENSURE }, &mut io);
        assert_eq!(io.calls, ["key", "child", "key", "write", "ready"]);
        assert_eq!(sh.list().as_ref().map(|l| l.len()), Some(30), "Start keeps the list it has");
        assert!(sh.ask(ENSURE), "after the exit, the next request starts a thread");
    }

    /// L3 T18: Start lets go while its list is being decoded: nothing is kept; held again, the next run keeps it; with the
    /// list there, nothing is asked or read.
    #[test]
    fn start_release_during_a_decode_keeps_nothing() {
        let sh = Shared::new();
        assert!(sh.hold());
        assert!(sh.ask(START));
        let mut io = FakeIo {
            keys: vec![Some(key())],
            file: Some(arena_of(&key(), &sample(20))),
            release_on_read: Some(&sh),
            ..FakeIo::default()
        };
        serve(&sh, sh.take(), &mut io);
        assert!(sh.list().is_none(), "released during the decode: not kept");
        assert_eq!(io.calls, ["key", "read"], "nothing to tell");
        assert!(sh.done());
        assert!(sh.hold());
        assert!(sh.ask(START));
        serve(&sh, sh.take(), &mut io);
        assert_eq!(sh.list().as_ref().map(|l| l.len()), Some(20));
        assert!(sh.done());
        assert!(!sh.hold(), "the list is there: nothing to ask");
        io.calls.clear();
        serve(&sh, START, &mut io);
        assert!(io.calls.is_empty(), "a late Start request reads nothing: {:?}", io.calls);
        sh.release();
        assert!(sh.list().is_none());
    }

    /// L3 T18: what the dock does after a child: a key line that is not the key computed right after it runs the child once
    /// more, and a second mismatch never leads to the in-process build; a failed child does; a failed write keeps the run's
    /// bytes for Start only; without a key nothing is cached.
    #[test]
    fn key_mismatch_retries_the_child_once() {
        let (k1, k2) = (key(), key2());
        assert!(matches!(child_verdict(child_out(&k1, 3), Some(&k1)), ChildVerdict::Use(..)));
        assert!(matches!(child_verdict(child_out(&k1, 3), Some(&k2)), ChildVerdict::Mismatch(..)));
        assert!(
            matches!(child_verdict(child_out(&k1, 3), None), ChildVerdict::Use(..)),
            "no key: the child's own line"
        );
        let refused = |out| matches!(child_verdict(out, Some(&k1)), ChildVerdict::Refused(_));
        assert!(
            matches!(child_verdict(Ok((1, arena_of(&k1, &sample(3)))), Some(&k1)), ChildVerdict::Refused(e) if e == "exit code 1")
        );
        assert!(refused(Ok((0, Vec::new()))));
        assert!(refused(Ok((0, b"mini-oled-dock settings-catalog 2\nk\n".to_vec()))));
        assert!(refused(Ok((0, crate::launcher::arena::test_apps(&[("a.exe", "A", &[], 1)]).raw().to_vec()))));
        let mut flipped = arena_of(&k1, &sample(3));
        let mid = flipped.len() / 2;
        flipped[mid] ^= 0x10;
        assert!(refused(Ok((0, flipped))));
        assert!(matches!(child_verdict(Err("timeout".into()), Some(&k1)), ChildVerdict::Refused(e) if e == "timeout"));
        let sh = Shared::new();
        // The settings files changed while the first child ran: once more, then cached under the new key.
        let mut io = FakeIo {
            keys: vec![Some(k1.clone()), Some(k2.clone())],
            children: [child_out(&k1, 3), child_out(&k2, 4)].into(),
            ..FakeIo::default()
        };
        serve(&sh, ENSURE, &mut io);
        assert_eq!(io.calls, ["key", "read", "child", "key", "child", "key", "write", "ready"]);
        let file = io.file.unwrap();
        assert_eq!((arena::key_line(&file), rows_of(&file, &k2).len()), (Some(k2.as_str()), 4));
        // Twice: no file and never the in-process build; Start still gets this run's list.
        let k3 = k2.replace(":100:6", ":100:7");
        let mut io = FakeIo {
            keys: vec![Some(k1.clone()), Some(k2.clone()), Some(k3)],
            children: [child_out(&k1, 3), child_out(&k2, 4)].into(),
            here: sample(9),
            ..FakeIo::default()
        };
        assert!(sh.hold());
        serve(&sh, Asked { start: true, ensure: true, rebuild: false, prime: false }, &mut io);
        assert_eq!(io.calls, ["key", "read", "child", "key", "child", "key", "ready"]);
        assert!(io.file.is_none());
        assert_eq!(sh.list().as_ref().map(|l| l.len()), Some(4));
        sh.release();
        // A child that fails: built in this process under the current key, and written.
        let mut io = FakeIo {
            keys: vec![Some(k1.clone())],
            children: [Ok((0, b"garbage".to_vec()))].into(),
            here: sample(5),
            ..FakeIo::default()
        };
        serve(&sh, ENSURE, &mut io);
        assert_eq!(io.calls, ["key", "read", "child", "key", "build true", "write", "ready"]);
        assert_eq!(rows_of(&io.file.unwrap(), &k1).len(), 5);
        // A failed write: the launcher is not told (it asks again at its next open).
        let mut io = FakeIo {
            keys: vec![Some(k1.clone())],
            children: [child_out(&k1, 3)].into(),
            write_fails: true,
            ..FakeIo::default()
        };
        serve(&sh, ENSURE, &mut io);
        assert_eq!(io.calls, ["key", "read", "child", "key", "write"]);
        // Unelevated (no file can be written): the launcher's request makes nothing, so no child per open; Start's does.
        let mut io = FakeIo {
            keys: vec![Some(k1.clone())],
            children: [child_out(&k1, 6)].into(),
            read_only: true,
            ..FakeIo::default()
        };
        serve(&sh, ENSURE, &mut io);
        assert_eq!(io.calls, ["key", "read"]);
        assert!(sh.hold());
        serve(&sh, START, &mut io);
        assert_eq!(&io.calls[2..], ["key", "read", "child", "key", "write", "ready"]);
        assert!(io.file.is_none());
        assert_eq!(sh.list().as_ref().map(|l| l.len()), Some(6));
        sh.release();
        // No key: nothing at all for the launcher; Start's list from the child's own line, never written.
        let mut io = FakeIo { keys: vec![None], children: [child_out(&unkeyed_key(), 2)].into(), ..FakeIo::default() };
        serve(&sh, ENSURE, &mut io);
        assert_eq!(io.calls, ["key"]);
        assert!(sh.hold());
        serve(&sh, START, &mut io);
        assert_eq!(io.calls, ["key", "key", "child", "key", "ready"]);
        assert!(io.file.is_none());
        assert_eq!(sh.list().as_ref().map(|l| l.len()), Some(2));
    }

    #[test]
    #[ignore]
    fn real_windows_build() {
        let b = windows_build();
        println!("build {b}");
        assert!(b.contains('.') && b.chars().next().is_some_and(|c| c.is_ascii_digit()));
    }

    /// Builds the REAL settings catalog and app index of this machine (run by hand:
    /// `cargo test -- --ignored --nocapture real_search_data`).
    #[test]
    #[ignore]
    fn real_search_data() {
        // SAFETY: COM for this test thread, paired with CoUninitialize below.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let t = Instant::now();
        let list = build_settings();
        println!("{} settings in {} ms", list.len(), t.elapsed().as_millis());
        for s in list.iter().filter(|s| s.lower.contains("bluetooth")).take(4) {
            println!("  {} | rank {} | {} {:?} | {} keys", s.name, s.rank, s.target, s.args, s.keys.iter().count());
        }
        assert!(list.len() > 300);
        assert!(list.iter().any(|s| s.args.as_deref().is_some_and(|a| a.starts_with("page="))));
        assert!(list.iter().any(|s| s.target.to_lowercase().ends_with("control.exe")));
        let bytes: usize = list
            .iter()
            .map(|s| {
                s.name.capacity()
                    + s.lower.capacity()
                    + s.keys.0.capacity()
                    + s.args.as_ref().map_or(0, String::capacity)
            })
            .sum::<usize>()
            + list.capacity() * std::mem::size_of::<Setting>();
        println!("settings catalog: about {} KB kept", bytes / 1024);
        assert!(list.iter().filter(|s| matches!(s.target, Cow::Borrowed(_))).count() > 300);
        load_app_index();
        let idx = take_app_index().expect("app index");
        assert!(take_app_index().is_none(), "handed over once");
        println!("{} synonym names, {} ranks, default {}", idx.synonyms.len(), idx.ranks.len(), idx.default_rank);
        assert!(idx.synonyms["league of legends"].iter().any(|(k, _)| k == "lol"));
        assert!(idx.ranks.len() > 1000 && idx.default_rank > 0);
        println!("rkdp -> {:?}", converted("rkdp"));
        release_app_index();
        if hr.is_ok() {
            // SAFETY: paired with the successful CoInitializeEx above.
            unsafe { CoUninitialize() };
        }
    }

    /// The arena of the settings of THIS machine, through `build_settings` and the entry builder (run by hand: `cargo test --
    /// --ignored --nocapture real_settings_arena_size_printed`): its size, and what `check` costs at every open.
    #[test]
    #[ignore]
    fn real_settings_arena_size_printed() {
        // SAFETY: COM for this test thread, paired with CoUninitialize below.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let list = build_settings();
        let encoded = encode_setting_list(&unkeyed_key(), &list).expect("the settings of this machine fit the arena");
        let bytes = encoded.as_slice();
        let kinds = crate::launcher::arena::Kind::Settings;
        let caps = crate::launcher::arena::Caps::SETTINGS;
        let mut times: Vec<std::time::Duration> = (0..51)
            .map(|_| {
                let t = Instant::now();
                std::hint::black_box(crate::launcher::arena::check(bytes, kinds, None, caps)).expect("checks");
                t.elapsed()
            })
            .collect();
        times.sort();
        if let Ok(l) = crate::launcher::arena::check(bytes, kinds, None, caps) {
            print!("{}", l.section_report());
        }
        println!(
            "{} settings, arena {} B (cap {}), check median {:?} over 51 runs",
            list.len(),
            bytes.len(),
            caps.bytes,
            times.get(25)
        );
        if hr.is_ok() {
            // SAFETY: paired with the successful CoInitializeEx above.
            unsafe { CoUninitialize() };
        }
    }
}
