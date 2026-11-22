//! SEARCH DATA of the Start menu (`popup_startmenu`): what the native Start menu searches with,
//! read from the files Windows itself ships. Nothing here is a list of ours.
//!
//! MEMORY: nothing here stays loaded longer than it is used. The app index is read for each
//! catalog and handed over to the menu, which keeps only the few keywords of the installed apps
//! (`take_app_index`). The typo rewrites (1.4 MB) are read from disk only for a query that finds
//! nothing (`converted`). The settings catalog is kept packed (`Keys`, a shared target: its ~1500
//! entries cost under 1 MB) while a Start menu is in use and freed with the rest of its data
//! (`release_settings`, a minute after the last one closed).
//!
//! APPS (`AppIndex`, read by the shell thread with each catalog):
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
//! SETTINGS (`settings`, built by a background thread, see `load_settings`): the
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
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use windows::core::HSTRING;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::Globalization::GetUserDefaultUILanguage;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::System::Registry::{
    RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RRF_SUBKEY_WOW6464KEY,
};
use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN};
use windows::Win32::UI::Shell::{FOLDERID_LocalAppData, SHGetKnownFolderPath, SHLoadIndirectString, KF_FLAG_DEFAULT};

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
        self.0.lines().filter_map(|l| l.rsplit_once('\t'))
    }

    /// The keywords and their penalties, in the order they were added.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&str, u32)> {
        self.0.lines().filter_map(|l| {
            let (key, penalty) = l.rsplit_once('\t')?;
            Some((key, penalty.parse().ok()?))
        })
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The same keywords sorted, each once with its best penalty, in exactly the memory they need.
    pub(crate) fn tidy(&self) -> Keys {
        let mut all: Vec<(&str, u32)> = self.iter().collect();
        all.sort();
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
// ponytail: a 1.4 MB read on the calling (UI) thread, a millisecond or two from the file cache;
// ask the shell thread instead if it ever shows in a trace.
pub(crate) fn converted(query: &str) -> Option<String> {
    let typed = query.trim().to_lowercase();
    if typed.is_empty() {
        return None;
    }
    convert(&read_conversions(), &typed).map(str::to_string)
}

/// `appsconversions.txt` from the profile of the dock's user (see the module notes), or empty.
fn read_conversions() -> String {
    // SAFETY: valid known-folder id; the returned buffer is freed below.
    let Ok(p) = (unsafe { SHGetKnownFolderPath(&FOLDERID_LocalAppData, KF_FLAG_DEFAULT, None) }) else {
        return String::new();
    };
    // SAFETY: `p` is a NUL-terminated string owned by us until CoTaskMemFree.
    let local = unsafe { p.to_string() }.unwrap_or_default();
    // SAFETY: `p` came from SHGetKnownFolderPath and is not used afterwards.
    unsafe { windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const _)) };
    if local.is_empty() {
        return String::new();
    }
    let root = Path::new(&local).join(r"Packages\MicrosoftWindows.Client.CBS_cw5n1h2txyewy\LocalState\ConstraintIndex");
    subdirs(&root)
        .iter()
        .find_map(|d| crate::datadir::read_file(&d.join("appsconversions.txt"), MAX_FILE, false).ok())
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
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

/// Reads the app index. On the shell thread, before each catalog is published.
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

/// Hands the app index over (`None` if there is none): the menu copies what the installed apps
/// need and drops the rest.
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

static SETTINGS: Mutex<Option<Arc<Vec<Setting>>>> = Mutex::new(None);
static SETTINGS_STARTED: AtomicBool = AtomicBool::new(false);

/// The settings catalog; empty until `load_settings` has finished (and after `release_settings`).
pub(crate) fn settings() -> Arc<Vec<Setting>> {
    SETTINGS.lock().unwrap_or_else(|p| p.into_inner()).clone().unwrap_or_default()
}

/// Builds the settings catalog on a background thread, unless it is there or being built. It takes
/// about a second (thousands of resource lookups), so it is never done on the UI or shell thread;
/// the menu uses the catalog from the first search after it is ready.
// The build is kept on disk (see `load_or_build_settings`): only the first run after a Windows
// update or a language change pays the second of resource lookups.
pub(crate) fn load_settings() {
    if SETTINGS_STARTED.swap(true, Relaxed) {
        return;
    }
    let spawned = std::thread::Builder::new().name("start-settings".into()).spawn(|| {
        // SAFETY: COM for this thread only, paired with CoUninitialize below; background priority
        // is a hint that fails harmlessly.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        // SAFETY: pseudo-handle of the current thread.
        let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) };
        let t = Instant::now();
        let (list, cached) = load_or_build_settings();
        log!(
            "start search: {} settings in {} ms{}",
            list.len(),
            t.elapsed().as_millis(),
            if cached { " (disk cache)" } else { "" }
        );
        // Released while it was being built: not kept.
        if SETTINGS_STARTED.load(Relaxed) {
            *SETTINGS.lock().unwrap_or_else(|p| p.into_inner()) = Some(Arc::new(list));
            // An open menu searches again: what is typed may match a setting.
            crate::shell_thread::request(crate::shell_thread::ShellRequest::StartSettingsReady);
        }
        if hr.is_ok() {
            // SAFETY: paired with the successful CoInitializeEx above.
            unsafe { CoUninitialize() };
        }
    });
    if let Err(e) = spawned {
        SETTINGS_STARTED.store(false, Relaxed);
        log!("start search: cannot start the settings thread: {e}");
    }
}

/// Who keeps the settings catalog loaded: the Start menu and the launcher share one copy, freed when the last of them
/// lets go.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Holder {
    Start = 1,
    Launcher = 2,
}

static HOLDERS: AtomicU8 = AtomicU8::new(0);

/// `holders` with the bit of `h` set or cleared; the bits left.
fn holders_step(holders: &AtomicU8, h: Holder, hold: bool) -> u8 {
    let bit = h as u8;
    if hold {
        holders.fetch_or(bit, Relaxed) | bit
    } else {
        holders.fetch_and(!bit, Relaxed) & !bit
    }
}

/// `h` needs the settings catalog: marks it and builds the catalog unless it is there.
pub(crate) fn hold_settings(h: Holder) {
    holders_step(&HOLDERS, h, true);
    load_settings();
}

/// `h` lets the settings catalog go; it is freed only when no holder is left.
pub(crate) fn release_settings_for(h: Holder) {
    if holders_step(&HOLDERS, h, false) == 0 {
        free_settings();
    }
}

/// `h` still holds the catalog.
pub(crate) fn holds_settings(h: Holder) -> bool {
    HOLDERS.load(Relaxed) & h as u8 != 0
}

/// Frees the settings catalog (the Start menu data was released): the next Start menu builds it again.
pub(crate) fn release_settings() {
    release_settings_for(Holder::Start);
}

fn free_settings() {
    SETTINGS_STARTED.store(false, Relaxed);
    *SETTINGS.lock().unwrap_or_else(|p| p.into_inner()) = None;
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

// ------------------------------------------------------------------ disk cache of the catalog

/// First line of `<root>\cache\settings-catalog.bin`; the digit is the format version.
const CACHE_HEADER: &str = "mini-oled-dock settings-catalog 1\n";
/// Largest cache file read or written.
const CACHE_MAX: u64 = 2 << 20;
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
/// modification time of each `AllSystemSettings_*.xml`. `None` when the build is unknown or no XML
/// file is there (nothing to key on: nothing cached).
fn catalog_key_of(dock: &str, build: &str, lang: u16, files: &[(String, u64, u64)]) -> Option<String> {
    if build.is_empty() || files.is_empty() {
        return None;
    }
    // "l3s1": the settings keywords carry `XML_KEYWORD_PENALTY` since L3; a file written with the old penalty is rebuilt once.
    let mut key = format!("{dock}|{build}|{lang:04x}|l3s1");
    for (name, len, mtime) in files {
        key.push('|');
        key.extend(name.chars().filter(|c| !c.is_control() && *c != '|'));
        key.push_str(&format!(":{len}:{mtime}"));
    }
    Some(key)
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
    files.sort();
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

fn fnv64(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf2_9ce4_8422_2325, |h, &c| (h ^ u64::from(c)).wrapping_mul(0x0100_0000_01b3))
}

fn put(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

/// The cache file: header, key line, entry count, the entries (name, keys, rank, target, args: each
/// string length-prefixed, a missing `args` is the length `u32::MAX`), a 64-bit checksum of all
/// of it. `None` if it would pass `CACHE_MAX`.
fn encode_catalog(key: &str, list: &[Setting]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(list.len() * 96 + 256);
    out.extend_from_slice(CACHE_HEADER.as_bytes());
    out.extend_from_slice(key.as_bytes());
    out.push(b'\n');
    out.extend_from_slice(&(list.len() as u32).to_le_bytes());
    for s in list {
        put(&mut out, &s.name);
        put(&mut out, &s.keys.0);
        out.extend_from_slice(&s.rank.to_le_bytes());
        put(&mut out, &s.target);
        match &s.args {
            Some(a) => put(&mut out, a),
            None => out.extend_from_slice(&u32::MAX.to_le_bytes()),
        }
    }
    let sum = fnv64(&out);
    out.extend_from_slice(&sum.to_le_bytes());
    (out.len() as u64 <= CACHE_MAX).then_some(out)
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn u32(&mut self) -> Option<u32> {
        let (h, rest) = self.0.split_first_chunk::<4>()?;
        self.0 = rest;
        Some(u32::from_le_bytes(*h))
    }
    fn str(&mut self, len: u32) -> Option<&'a str> {
        let len = len as usize;
        if len > self.0.len() {
            return None;
        }
        let (s, rest) = self.0.split_at(len);
        self.0 = rest;
        std::str::from_utf8(s).ok()
    }
    fn string(&mut self) -> Option<&'a str> {
        let n = self.u32()?;
        self.str(n)
    }
}

/// The catalog of `bytes`, or `None` for anything that is not exactly what `encode_catalog` wrote
/// for `key`: wrong header, key, checksum, count or lengths, bad UTF-8, trailing bytes. Every
/// length is checked against what is left before it allocates. Entries are rebuilt as
/// `build_settings` makes them (exact-size strings; the Settings app target is the shared constant).
fn decode_catalog(bytes: &[u8], key: &str) -> Option<Vec<Setting>> {
    let head = CACHE_HEADER.as_bytes();
    let body_len = bytes.len().checked_sub(8)?;
    let (body, sum) = bytes.split_at(body_len);
    if fnv64(body).to_le_bytes() != sum || !body.starts_with(head) {
        return None;
    }
    let rest = &body[head.len()..];
    let nl = rest.iter().position(|&c| c == b'\n')?;
    if &rest[..nl] != key.as_bytes() {
        return None;
    }
    let mut r = Reader(&rest[nl + 1..]);
    let count = r.u32()? as usize;
    // An entry is at least 20 bytes (four lengths and the rank): a larger count cannot be true.
    if count > r.0.len() / 20 {
        return None;
    }
    let mut list = Vec::with_capacity(count);
    for _ in 0..count {
        let name = r.string()?;
        let keys = r.string()?;
        let rank = r.u32()?;
        let target = r.string()?;
        let args = match r.u32()? {
            u32::MAX => None,
            n => Some(r.str(n)?.to_string()),
        };
        let target: Cow<'static, str> =
            if target == SETTINGS_APP { Cow::Borrowed(SETTINGS_APP) } else { Cow::Owned(target.to_string()) };
        list.push(Setting {
            name: name.to_string(),
            lower: name.to_lowercase(),
            keys: Keys(keys.to_string()),
            rank,
            target,
            args,
        });
    }
    r.0.is_empty().then_some(list)
}

/// The catalog: from the cache file when it matches this Windows build, language and XML files,
/// else built (and the cache rewritten). The bool is true when it came from the cache. A cache that
/// cannot be read, parsed or written only costs the build, as before.
fn load_or_build_settings() -> (Vec<Setting>, bool) {
    let Some(windir) = windows_dir() else { return (Vec::new(), false) };
    let key = catalog_key(&windir.join(r"ImmersiveControlPanel\Settings"));
    let path = crate::datadir::cache_dir().map(|d| d.join(CACHE_NAME));
    if let (Some(key), Some(path)) = (&key, &path) {
        if let Some(list) = crate::datadir::read_file(path, CACHE_MAX, true).ok().and_then(|b| decode_catalog(&b, key))
        {
            return (list, true);
        }
    }
    let list = build_settings();
    if let (Some(key), Some(path)) = (&key, &path) {
        // Unelevated, the data folder is read-only: reading the cache is fine, writing it is not tried.
        if !list.is_empty() && crate::datadir::writable() {
            if let Some(bytes) = encode_catalog(key, &list) {
                if let Err(e) = crate::datadir::write_atomic(path, &bytes, crate::datadir::FileAcl::Inherit) {
                    log!("start search: cannot write the settings cache: {e}");
                }
            }
        }
    }
    (list, false)
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

    const KEY: &str = "26100.1742|0409|allsystemsettings_x.xml:100:5";

    #[test]
    fn round_trip() {
        let list = sample(300);
        let bytes = encode_catalog(KEY, &list).unwrap();
        assert!(bytes.starts_with(b"mini-oled-dock settings-catalog 1\n"));
        let back = decode_catalog(&bytes, KEY).unwrap();
        assert_eq!(back, list);
        assert!(
            back.iter().filter(|s| matches!(s.target, Cow::Borrowed(_))).count() > 100,
            "the shared target stays shared"
        );
        assert_eq!(decode_catalog(&encode_catalog(KEY, &[]).unwrap(), KEY).unwrap(), Vec::new());
    }

    #[test]
    fn invalidated_by_build_or_language() {
        let files = vec![("allsystemsettings_a.xml".to_string(), 10u64, 20u64)];
        let k = catalog_key_of("1.5.0:9:9", "26100.1742", 0x409, &files).unwrap();
        let bytes = encode_catalog(&k, &sample(5)).unwrap();
        assert!(decode_catalog(&bytes, &k).is_some());
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
            assert!(decode_catalog(&bytes, &other).is_none(), "{other}");
        }
        // A key written before the XML sentinel (no salt) does not match.
        let old = k.replace("|l3s1", "");
        assert_ne!(old, k);
        assert!(decode_catalog(&bytes, &old).is_none(), "a pre-L3 cache is rebuilt once");
        assert!(catalog_key_of("1.5.0:9:9", "", 0x409, &files).is_none(), "unknown build: no cache");
        assert!(catalog_key_of("1.5.0:9:9", "26100.1", 0x409, &[]).is_none(), "no XML: no cache");
    }

    #[test]
    fn corrupt_ignored() {
        let bytes = encode_catalog(KEY, &sample(40)).unwrap();
        assert!(decode_catalog(&[], KEY).is_none());
        assert!(decode_catalog(&bytes[..bytes.len() - 1], KEY).is_none());
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(decode_catalog(&trailing, KEY).is_none());
        for i in (0..bytes.len()).step_by(7) {
            let mut b = bytes.clone();
            b[i] ^= 0x55;
            assert!(decode_catalog(&b, KEY).is_none(), "byte {i}");
        }
        // A forged file with a valid checksum but lies inside: an absurd count and a length past the end.
        let forge = |tail: &[u8]| {
            let mut b = CACHE_HEADER.as_bytes().to_vec();
            b.extend_from_slice(KEY.as_bytes());
            b.push(b'\n');
            b.extend_from_slice(tail);
            let sum = fnv64(&b);
            b.extend_from_slice(&sum.to_le_bytes());
            b
        };
        assert!(decode_catalog(&forge(&u32::MAX.to_le_bytes()), KEY).is_none());
        assert!(decode_catalog(&forge(&[1, 0, 0, 0, 0xff, 0xff, 0xff, 0x7f]), KEY).is_none());
        assert!(decode_catalog(&forge(&[1, 0, 0, 0, 2, 0, 0, 0, 0xc3, 0x28]), KEY).is_none(), "bad UTF-8");
        assert!(decode_catalog(&forge(&0u32.to_le_bytes()), KEY).is_some());
    }

    #[test]
    fn corrupt_ignored_seeded_100000() {
        let good = encode_catalog(KEY, &sample(12)).unwrap();
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
            // Half the runs get a fresh checksum, so the mutated body reaches the parser itself.
            if next() % 2 == 0 && b.len() >= 8 {
                b.truncate(b.len() - 8);
                let sum = fnv64(&b);
                b.extend_from_slice(&sum.to_le_bytes());
            }
            // Must never panic; anything it accepts is exactly what the encoder would write.
            if let Some(list) = decode_catalog(&b, KEY) {
                assert_eq!(encode_catalog(KEY, &list), Some(b));
            }
        }
    }

    #[test]
    #[ignore]
    fn real_windows_build() {
        let b = windows_build();
        println!("build {b}");
        assert!(b.contains('.') && b.chars().next().is_some_and(|c| c.is_ascii_digit()));
    }

    #[test]
    fn settings_freed_only_when_both_released() {
        let bits = AtomicU8::new(0);
        assert_eq!(holders_step(&bits, Holder::Start, true), 1);
        assert_eq!(holders_step(&bits, Holder::Launcher, true), 3);
        assert_eq!(holders_step(&bits, Holder::Start, false), 2, "the launcher still holds it");
        assert_eq!(holders_step(&bits, Holder::Start, false), 2, "releasing twice changes nothing");
        assert_eq!(holders_step(&bits, Holder::Launcher, false), 0, "the last one frees it");
        assert_eq!(holders_step(&bits, Holder::Launcher, true), 2);
        assert_eq!(holders_step(&bits, Holder::Start, true), 3);
        assert_eq!(holders_step(&bits, Holder::Launcher, false), 1);
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
}
