//! Windows entry points (Start menu shortcuts) of the suite apps (I1b).
//!
//! The elevated dock keeps one shortcut per installed, switched-on component in the all-users Programs folder, each
//! with its AppUserModelID, so the taskbar and the Start menu group the app's windows under it. The shortcuts are the
//! only thing written here: every one is journaled (Kind::Install, FileCreated with the digest of the bytes) in the
//! machine journal before it is written, so the suite's uninstall can undo it. Display names come from the kit's
//! string table; this file holds none.
//!
//! The decision (what is wanted, what is ours, what to create or remove) is pure and runs over `LinkOs`, so the
//! tests use a fake. `RealLinks` is the COM and file side.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use oled_trust::manifest::suite::ComponentId;

/// What one suite app's entry is: its component, the link's file name, the exe it starts and its AUMID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Want {
    pub component: ComponentId,
    /// File name with the extension, in the Programs folder.
    pub file_name: String,
    pub target: PathBuf,
    pub aumid: &'static str,
    /// Shown by the shell as the shortcut's tooltip: the app's display name.
    pub description: String,
}

/// The components that get a Start entry, with the AUMID of each.
const ENTRY_COMPONENTS: [(ComponentId, &str); 2] =
    [(ComponentId::Tasks, "MiniOledDock.Tasks"), (ComponentId::Files, "MiniOledDock.Files")];

fn display_name(c: ComponentId, italian: bool) -> String {
    let key = match c {
        ComponentId::Files => oled_ui::strings::k::KIT_APP_FILES,
        _ => oled_ui::strings::k::KIT_APP_TASKS,
    };
    let lang = if italian { oled_ui::strings::UiLang::IT } else { oled_ui::strings::UiLang::EN };
    lang.kit(key).into_owned()
}

fn switched_on(cfg: &crate::config::Config, c: ComponentId) -> bool {
    match c {
        ComponentId::Tasks => cfg.suite_tasks,
        ComponentId::Files => cfg.suite_files,
        _ => false,
    }
}

/// A display name that is one plain file name component: trimmed, no separator, drive colon, wildcard or control
/// character, not "." or "..", not ending in a dot, at most 200 UTF-16 units. The name comes from a data file, so the
/// link can never land outside the Programs folder whatever that file holds.
fn plain_name(n: &str) -> bool {
    !n.is_empty()
        && n == n.trim()
        && n != "."
        && n != ".."
        && !n.ends_with('.')
        && n.encode_utf16().count() <= 200
        && !n.chars().any(|c| c.is_control() || "\\/:*?\"<>|".contains(c))
}

/// The entries that should exist, each with the path of its link in `start_dir`: a component that is installed and
/// switched on, named in the dock's language. Pure.
pub(crate) fn wanted(
    cfg: &crate::config::Config,
    installed: &[ComponentId],
    start_dir: &Path,
    install_dir: &Path,
    italian: bool,
) -> Vec<(PathBuf, Want)> {
    let mut out = Vec::new();
    for (c, aumid) in ENTRY_COMPONENTS {
        if !installed.contains(&c) || !switched_on(cfg, c) {
            continue;
        }
        let name = display_name(c, italian);
        if !plain_name(&name) {
            static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if !SAID.swap(true, std::sync::atomic::Ordering::Relaxed) {
                crate::log!("entry points: a display name is not usable as a file name; skipped");
            }
            continue;
        }
        let file_name = format!("{name}.lnk");
        if Path::new(&file_name).file_name() != Some(std::ffi::OsStr::new(&file_name)) {
            continue;
        }
        out.push((
            start_dir.join(&file_name),
            Want { component: c, file_name, target: install_dir.join(c.file()), aumid, description: name },
        ));
    }
    out
}

/// A shortcut found in the Programs folder: its path and the exe it starts, when that could be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Found {
    pub path: PathBuf,
    pub target: Option<PathBuf>,
}

/// The side effects of a sync. Real: COM, files and the machine journal. Test: a fake.
pub(crate) trait LinkOs {
    /// The .lnk files directly in `dir`.
    fn list(&mut self, dir: &Path) -> Vec<Found>;
    /// The bytes the shortcut for `w` has once saved.
    fn build(&mut self, w: &Want) -> Result<Vec<u8>, String>;
    /// Records a link about to be written, before it is written (Kind::Install, FileCreated).
    fn record(&mut self, path: &Path, digest: oled_trust::journal::Digest) -> Result<(), String>;
    /// Creates the file; Ok(false) when something already stands at the path (left alone, nothing written).
    fn write(&mut self, path: &Path, bytes: &[u8]) -> Result<bool, String>;
    fn remove(&mut self, path: &Path) -> Result<(), String>;
}

fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.as_os_str().to_string_lossy().replace('/', "\\");
    norm(a).eq_ignore_ascii_case(&norm(b))
}

/// A shortcut of ours has the file name this component's entry has in either language and starts that component's
/// exe in `install_dir`. A link under another name (an administrator's own) is never ours.
fn is_ours(f: &Found, install_dir: &Path) -> bool {
    let (Some(t), Some(name)) = (&f.target, f.path.file_name().map(|n| n.to_string_lossy())) else { return false };
    ENTRY_COMPONENTS.iter().any(|(c, _)| {
        same_path(t, &install_dir.join(c.file()))
            && [false, true].iter().any(|&it| name.eq_ignore_ascii_case(&format!("{}.lnk", display_name(*c, it))))
    })
}

/// Brings `start_dir` in line with `want`: removes our shortcuts that are not wanted (a switched-off component, a
/// language change), creates the missing ones (journaled first) and leaves everything else alone, including a
/// shortcut of someone else that has a wanted name. Returns how many files it created or removed; the first error
/// is returned after every step has been tried.
pub(crate) fn sync_with(
    os: &mut dyn LinkOs,
    start_dir: &Path,
    install_dir: &Path,
    want: &[(PathBuf, Want)],
) -> Result<u32, String> {
    let found = os.list(start_dir);
    let mut changed = 0u32;
    let mut first_err: Option<String> = None;
    for f in &found {
        if is_ours(f, install_dir) && !want.iter().any(|(p, _)| same_path(p, &f.path)) {
            match os.remove(&f.path) {
                Ok(()) => changed += 1,
                Err(e) => first_err = first_err.or(Some(e)),
            }
        }
    }
    for (path, w) in want {
        if found.iter().any(|f| same_path(&f.path, path)) {
            continue; // ours already, or someone else's with this name: either way not touched
        }
        let step = (|| -> Result<bool, String> {
            let bytes = os.build(w)?;
            let digest = oled_sys::sha256::sha256(&bytes)?;
            os.record(path, digest)?;
            os.write(path, &bytes)
        })();
        match step {
            Ok(true) => changed += 1,
            Ok(false) => {}
            Err(e) => first_err = first_err.or(Some(e)),
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(changed),
    }
}

/// The components that have an entry and whose exe is in `install_dir`.
fn installed_in(install_dir: &Path) -> Vec<ComponentId> {
    ENTRY_COMPONENTS.iter().map(|(c, _)| *c).filter(|c| install_dir.join(c.file()).is_file()).collect()
}

/// Brings the entry points in line with `c`; elevated only. Returns how many shortcuts it created or removed.
pub fn sync(c: &crate::config::Config, install_dir: &Path) -> Result<u32, String> {
    if !crate::shell_thread::is_elevated() {
        return Err("entry points: not elevated".into());
    }
    let Some(start_dir) = crate::shell_thread::known_folder(&windows::Win32::UI::Shell::FOLDERID_CommonPrograms) else {
        return Err("entry points: no all-users Programs folder".into());
    };
    let lang = match c.language {
        crate::config::Language::Auto => oled_sys::i18n::Lang::Auto,
        crate::config::Language::En => oled_sys::i18n::Lang::En,
        crate::config::Language::It => oled_sys::i18n::Lang::It,
    };
    let italian = oled_ui::strings::UiLang::from_lang(lang).italian;
    let want = wanted(c, &installed_in(install_dir), &start_dir, install_dir, italian);
    let mut os = RealLinks::new(install_dir);
    let r = sync_with(&mut os, &start_dir, install_dir, &want);
    os.finish();
    match &r {
        Ok(n) => crate::log!("entry points: {n} changed"),
        Err(_) => crate::log!("entry points: not all could be brought in line"),
    }
    r
}

type Triple = (bool, bool, crate::config::Language);

/// The last (suite_tasks, suite_files, language) `on_config` saw.
static LAST: Mutex<Option<Triple>> = Mutex::new(None);

/// Stores `now` in `last`; true when it is a change of a triple already seen (the first call only remembers).
fn changed(last: &mut Option<Triple>, now: Triple) -> bool {
    let was = last.replace(now);
    matches!(was, Some(w) if w != now)
}

/// config's on_set hook: a change of one of the two switches or of the language asks the shell thread for a sync.
/// The first call (the start) only remembers: the start's sync is the update's or the installer's.
pub fn on_config(c: &crate::config::Config) {
    let ask = {
        let mut g = LAST.lock().unwrap_or_else(|p| p.into_inner());
        changed(&mut g, (c.suite_tasks, c.suite_files, c.language))
    };
    if ask {
        request_sync();
    }
}

/// Asks the shell thread to run `sync`.
pub fn request_sync() {
    crate::shell_thread::request(crate::shell_thread::ShellRequest::SyncEntryPoints);
}

// ================================================================== the real side

use windows::core::{Interface, HSTRING, PCWSTR};
use windows::Win32::System::Com::StructuredStorage::{PropVariantChangeType, PROPVAR_CHANGE_FLAGS};
use windows::Win32::System::Com::{IPersistStream, STREAM_SEEK_END, STREAM_SEEK_SET};
use windows::Win32::System::Variant::VT_LPWSTR;
use windows::Win32::UI::Shell::SHCreateMemStream;

/// A saved link is a few hundred bytes; anything larger is refused.
const LINK_MAX: u64 = 64 * 1024;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};

/// PKEY_AppUserModel_ID {9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3}, 5; defined here as apps.rs does, to keep the
/// Win32_Storage_EnhancedStorage feature (thousands of constants) out of the build.
const PKEY_AUMID: PROPERTYKEY =
    PROPERTYKEY { fmtid: windows::core::GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3), pid: 5 };

struct RealLinks {
    install_dir: PathBuf,
    /// The machine journal, opened when the first link is recorded.
    journal: Option<(oled_trust::journal::Journal, oled_sys::journal_os::FileSink, oled_trust::journal::RunId)>,
    com: bool,
}

impl RealLinks {
    fn new(install_dir: &Path) -> RealLinks {
        // SAFETY: plain call; a success is balanced by CoUninitialize in `finish`, a different apartment mode already
        // set on this thread (RPC_E_CHANGED_MODE) is used as it is and not balanced.
        let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
        RealLinks { install_dir: install_dir.to_path_buf(), journal: None, com }
    }

    /// Releases the journal and COM. The run is NOT committed: a committed run's entries are closed, and the suite's
    /// uninstall undoes only open ones, so the Install entries stay open like the other Install records.
    fn finish(&mut self) {
        self.journal = None;
        if self.com {
            self.com = false;
            // SAFETY: balances the successful CoInitializeEx of `new`.
            unsafe { CoUninitialize() };
        }
    }
}

/// Journals a link about to be written (Kind::Install, FileCreated with the digest of its bytes). An open entry for
/// these very bytes at this path already covers the file, so a link made again after a switch or a language change
/// is not journaled twice.
fn record_install(
    j: &mut oled_trust::journal::Journal,
    sink: &mut dyn oled_trust::journal::Sink,
    run: oled_trust::journal::RunId,
    path: &Path,
    digest: oled_trust::journal::Digest,
    now: u64,
) -> Result<(), String> {
    use oled_trust::journal::{Change, Kind};
    let text = path.to_str().ok_or("entry points: a path that is not text")?.to_string();
    let open = j.open_entries().into_iter().any(|e| {
        matches!(&e.change, Change::FileCreated { path: p, after } if *after == digest && same_path(Path::new(p), path))
    });
    if open {
        return Ok(());
    }
    let change = Change::FileCreated { path: text, after: digest };
    oled_trust::journal::record(j, sink, run, Kind::Install, change, now).map(|_| ()).map_err(|e| e.what)
}

fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl LinkOs for RealLinks {
    fn list(&mut self, dir: &Path) -> Vec<Found> {
        let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
        // SAFETY: COM is initialised by `new`; the ShellLink class with the interface it implements.
        let link = unsafe { CoCreateInstance::<_, IShellLinkW>(&ShellLink, None, CLSCTX_INPROC_SERVER) }.ok();
        let file = link.as_ref().and_then(|l| l.cast::<IPersistFile>().ok());
        let mut out = Vec::new();
        for e in rd.flatten() {
            let p = e.path();
            if !p.extension().is_some_and(|x| x.eq_ignore_ascii_case("lnk")) {
                continue;
            }
            let target = match (&link, &file) {
                (Some(l), Some(f)) => crate::shell_thread::lnk_target(l, f, &p).map(PathBuf::from),
                _ => None,
            };
            out.push(Found { path: p, target });
        }
        out
    }

    fn build(&mut self, w: &Want) -> Result<Vec<u8>, String> {
        let fail = |what: &str| format!("entry points: {what}");
        // SAFETY: COM is initialised by `new`; the ShellLink class with the interface it implements.
        let link = unsafe { CoCreateInstance::<_, IShellLinkW>(&ShellLink, None, CLSCTX_INPROC_SERVER) }
            .map_err(|_| fail("no shell link object"))?;
        let target = HSTRING::from(w.target.as_os_str());
        let dir = HSTRING::from(self.install_dir.as_os_str());
        let desc = HSTRING::from(w.description.as_str());
        // SAFETY: the HSTRINGs are NUL-terminated and live for the calls.
        unsafe {
            link.SetPath(PCWSTR(target.as_ptr())).map_err(|_| fail("SetPath"))?;
            link.SetWorkingDirectory(PCWSTR(dir.as_ptr())).map_err(|_| fail("SetWorkingDirectory"))?;
            link.SetDescription(PCWSTR(desc.as_ptr())).map_err(|_| fail("SetDescription"))?;
        }
        let store: IPropertyStore = link.cast().map_err(|_| fail("no property store"))?;
        // System.AppUserModel.ID is a VT_LPWSTR; PROPVARIANT::from(&str) would make a VT_BSTR.
        let bstr = PROPVARIANT::from(w.aumid);
        let mut id = PROPVARIANT::default();
        // SAFETY: both variants are valid; the converted one owns its string and clears it on drop.
        unsafe { PropVariantChangeType(&mut id, &bstr, PROPVAR_CHANGE_FLAGS(0), VT_LPWSTR) }
            .map_err(|_| fail("AUMID type"))?;
        // SAFETY: `id` is a valid string variant that lives for the calls.
        unsafe {
            store.SetValue(&PKEY_AUMID, &id).map_err(|_| fail("SetValue"))?;
            store.Commit().map_err(|_| fail("Commit"))?;
        }
        // Saved to memory: the journal needs the digest of the bytes before the link exists, and no file another
        // process could touch is involved.
        let persist: IPersistStream = link.cast().map_err(|_| fail("no persist stream"))?;
        // SAFETY: plain call.
        let mem = unsafe { SHCreateMemStream(None) }.ok_or_else(|| fail("no memory stream"))?;
        let mut end = 0u64;
        // SAFETY: `mem` is a valid stream; `end` and `got` are writable; the buffer has the length read.
        unsafe {
            persist.Save(&mem, true).map_err(|_| fail("Save"))?;
            mem.Seek(0, STREAM_SEEK_END, Some(&mut end)).map_err(|_| fail("Seek"))?;
            if end == 0 || end > LINK_MAX {
                return Err(fail("the link has an unexpected size"));
            }
            mem.Seek(0, STREAM_SEEK_SET, None).map_err(|_| fail("Seek"))?;
            let mut buf = vec![0u8; end as usize];
            let mut got = 0u32;
            mem.Read(buf.as_mut_ptr().cast(), end as u32, Some(&mut got)).ok().map_err(|_| fail("Read"))?;
            buf.truncate(got as usize);
            Ok(buf)
        }
    }

    fn record(&mut self, path: &Path, digest: oled_trust::journal::Digest) -> Result<(), String> {
        if self.journal.is_none() {
            let (mut j, sink) = oled_sys::journal_os::open_locked(
                oled_trust::journal::Scope::Machine,
                std::time::Duration::from_secs(10),
            )
            .map_err(|e| e.what)?;
            let run = j.begin_run();
            self.journal = Some((j, sink, run));
        }
        let Some((j, sink, run)) = self.journal.as_mut() else { return Err("entry points: no journal".into()) };
        record_install(j, sink, *run, path, digest, now_unix())
    }

    fn write(&mut self, path: &Path, bytes: &[u8]) -> Result<bool, String> {
        use std::io::Write;
        // create_new: a file that appeared since the listing (or whose name differs only in case the listing could
        // not see) is never overwritten.
        let mut f = match std::fs::OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
            Err(_) => return Err("entry points: a link could not be written".to_string()),
        };
        if f.write_all(bytes).is_err() {
            // The file was created by this call: a truncated link must not stay at the wanted name.
            drop(f);
            let _ = std::fs::remove_file(path);
            return Err("entry points: a link could not be written".to_string());
        }
        Ok(true)
    }

    fn remove(&mut self, path: &Path) -> Result<(), String> {
        std::fs::remove_file(path).map_err(|_| "entry points: a link could not be removed".to_string())
    }
}

impl Drop for RealLinks {
    fn drop(&mut self) {
        self.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Language};
    use oled_config::Options;

    const START: &str = "C:\\made-up\\Programs";
    const INSTALL: &str = "C:\\made-up\\Suite";

    fn cfg(tasks: bool, files: bool) -> Config {
        Config { suite_tasks: tasks, suite_files: files, ..Config::default() }
    }

    fn names(w: &[(PathBuf, Want)]) -> Vec<String> {
        w.iter().map(|(_, w)| w.file_name.clone()).collect()
    }

    fn en(c: ComponentId) -> String {
        format!("{}.lnk", display_name(c, false))
    }

    #[test]
    fn wanted_set() {
        let both = [ComponentId::Tasks, ComponentId::Files];
        let start = Path::new(START);
        let install = Path::new(INSTALL);
        let w = wanted(&cfg(true, true), &both, start, install, false);
        assert_eq!(names(&w), [en(ComponentId::Tasks), en(ComponentId::Files)]);
        assert_eq!(w[0].0, start.join(&w[0].1.file_name));
        assert_eq!(w[0].1.target, install.join("tasks.exe"));
        assert_eq!(w[0].1.aumid, "MiniOledDock.Tasks");
        assert_eq!(w[1].1.target, install.join("files.exe"));
        assert_eq!(w[1].1.aumid, "MiniOledDock.Files");
        // Italian: the table's other name for the first, the same for the second.
        let it = wanted(&cfg(true, true), &both, start, install, true);
        assert_eq!(it[0].1.file_name, format!("{}.lnk", display_name(ComponentId::Tasks, true)));
        assert_ne!(it[0].1.file_name, w[0].1.file_name);
        assert_eq!(it[1].1.file_name, w[1].1.file_name);
        // Switched off, or not installed: absent.
        assert_eq!(names(&wanted(&cfg(false, true), &both, start, install, false)), [en(ComponentId::Files)]);
        assert_eq!(names(&wanted(&cfg(true, false), &both, start, install, false)), [en(ComponentId::Tasks)]);
        assert!(wanted(&cfg(false, false), &both, start, install, false).is_empty());
        assert_eq!(
            names(&wanted(&cfg(true, true), &[ComponentId::Files], start, install, false)),
            [en(ComponentId::Files)]
        );
        assert!(wanted(&cfg(true, true), &[ComponentId::Dock, ComponentId::LangIt], start, install, false).is_empty());
    }

    #[derive(Default)]
    struct Fake {
        found: Vec<Found>,
        events: Vec<String>,
        fail_record: bool,
        /// Paths that appear between the listing and the write.
        exists_late: Vec<PathBuf>,
        digests: Vec<oled_trust::journal::Digest>,
    }

    impl LinkOs for Fake {
        fn list(&mut self, _dir: &Path) -> Vec<Found> {
            self.found.clone()
        }
        fn build(&mut self, w: &Want) -> Result<Vec<u8>, String> {
            self.events.push(format!("build {}", w.file_name));
            Ok(format!("{}|{}|{}", w.file_name, w.target.display(), w.aumid).into_bytes())
        }
        fn record(&mut self, path: &Path, digest: oled_trust::journal::Digest) -> Result<(), String> {
            if self.fail_record {
                return Err("journal busy".into());
            }
            self.digests.push(digest);
            self.events.push(format!("record {}", path.display()));
            Ok(())
        }
        fn write(&mut self, path: &Path, _bytes: &[u8]) -> Result<bool, String> {
            self.events.push(format!("write {}", path.display()));
            Ok(!self.exists_late.iter().any(|p| p == path))
        }
        fn remove(&mut self, path: &Path) -> Result<(), String> {
            self.events.push(format!("remove {}", path.display()));
            Ok(())
        }
    }

    fn link(name: &str, target: &str) -> Found {
        Found { path: Path::new(START).join(name), target: Some(PathBuf::from(target)) }
    }

    fn run(os: &mut Fake, c: &Config, italian: bool) -> Result<u32, String> {
        let both = [ComponentId::Tasks, ComponentId::Files];
        let want = wanted(c, &both, Path::new(START), Path::new(INSTALL), italian);
        sync_with(os, Path::new(START), Path::new(INSTALL), &want)
    }

    #[test]
    fn sync_converges() {
        // Nothing there: both created.
        let mut os = Fake::default();
        assert_eq!(run(&mut os, &cfg(true, true), false), Ok(2));
        assert_eq!(os.events.iter().filter(|e| e.starts_with("write ")).count(), 2);
        // Both there: nothing to do.
        let mut os = Fake {
            found: vec![
                link(&en(ComponentId::Tasks), &format!("{INSTALL}\\tasks.exe")),
                link(&en(ComponentId::Files), &format!("{INSTALL}\\FILES.EXE")),
            ],
            ..Fake::default()
        };
        assert_eq!(run(&mut os, &cfg(true, true), false), Ok(0));
        assert!(os.events.is_empty());
        // Files switched off: ours is removed, tasks stays.
        assert_eq!(run(&mut os, &cfg(true, false), false), Ok(1));
        assert_eq!(os.events, [format!("remove {}", Path::new(START).join(en(ComponentId::Files)).display())]);
        // A foreign shortcut with a wanted name (its target is elsewhere) is left alone and not replaced; an
        // unrelated shortcut is never touched either.
        let mut os = Fake {
            found: vec![
                link(&en(ComponentId::Tasks), "C:\\elsewhere\\other.exe"),
                link("Unrelated.lnk", "C:\\elsewhere\\unrelated.exe"),
            ],
            ..Fake::default()
        };
        assert_eq!(run(&mut os, &cfg(true, false), false), Ok(0));
        assert!(os.events.is_empty(), "{:?}", os.events);
        // A link of ours that was made while the component was on and is not wanted any more is removed even when a
        // foreign one stands beside it.
        let mut os = Fake {
            found: vec![
                link(&en(ComponentId::Files), &format!("{INSTALL}\\files.exe")),
                link("Unrelated.lnk", "C:\\elsewhere\\unrelated.exe"),
            ],
            ..Fake::default()
        };
        assert_eq!(run(&mut os, &cfg(false, false), false), Ok(1));
        assert_eq!(os.events.len(), 1);
        assert!(os.events[0].starts_with("remove "));
    }

    #[test]
    fn a_link_under_another_name_is_never_ours() {
        let mine = link("My Tasks.lnk", &format!("{INSTALL}\\tasks.exe"));
        for on in [true, false] {
            let mut os = Fake { found: vec![mine.clone()], ..Fake::default() };
            let c = cfg(on, on);
            let n = run(&mut os, &c, false).unwrap();
            assert!(!os.events.iter().any(|e| e.starts_with("remove ")), "{:?}", os.events);
            assert_eq!(n, if on { 2 } else { 0 });
        }
    }

    #[test]
    fn a_language_change_renames_by_delete_and_create() {
        let old = en(ComponentId::Tasks);
        let mut os = Fake { found: vec![link(&old, &format!("{INSTALL}\\tasks.exe"))], ..Fake::default() };
        let new = format!("{}.lnk", display_name(ComponentId::Tasks, true));
        assert_ne!(old, new);
        let n = run(&mut os, &cfg(true, false), true).unwrap();
        assert_eq!(n, 2);
        let remove = format!("remove {}", Path::new(START).join(&old).display());
        let write = format!("write {}", Path::new(START).join(&new).display());
        assert!(os.events.contains(&remove) && os.events.contains(&write), "{:?}", os.events);
    }

    #[test]
    fn journaled() {
        // Each link is recorded, with the digest of the bytes it is given, before it is written; a refused record
        // writes nothing and is reported.
        let mut os = Fake::default();
        assert_eq!(run(&mut os, &cfg(true, true), false), Ok(2));
        let pos = |p: &str| os.events.iter().position(|e| e.starts_with(p)).unwrap();
        for c in [ComponentId::Tasks, ComponentId::Files] {
            let at = Path::new(START).join(en(c)).display().to_string();
            let (b, r, w) =
                (pos(&format!("build {}", en(c))), pos(&format!("record {at}")), pos(&format!("write {at}")));
            assert!(b < r && r < w, "{:?}", os.events);
        }
        let tasks_bytes = format!("{}|{INSTALL}\\tasks.exe|MiniOledDock.Tasks", en(ComponentId::Tasks)).into_bytes();
        assert_eq!(os.digests[0], oled_sys::sha256::sha256(&tasks_bytes).unwrap());
        let mut os = Fake { fail_record: true, ..Fake::default() };
        assert!(run(&mut os, &cfg(true, true), false).is_err());
        assert!(os.events.iter().all(|e| !e.starts_with("write ")), "{:?}", os.events);
        // The journal's own shape for it: a FileCreated of the Install kind round-trips through the format.
        let c =
            oled_trust::journal::Change::FileCreated { path: "C:\\made-up\\Programs\\x.lnk".into(), after: [7; 32] };
        let e = oled_trust::journal::Entry {
            seq: 1,
            run: oled_trust::journal::RunId(1),
            ts: 0,
            kind: oled_trust::journal::Kind::Install,
            change: c,
        };
        let rec = oled_trust::journal::Record::Entry(e);
        let line = oled_trust::journal::Journal::encode(&rec).unwrap();
        let back = oled_trust::journal::Journal::parse(oled_trust::journal::Scope::Machine, &line).unwrap();
        assert_eq!(back.records(), [rec]);
    }

    #[derive(Default)]
    struct MemSink(Vec<u8>);

    impl oled_trust::journal::Sink for MemSink {
        fn append(&mut self, bytes: &[u8]) -> Result<(), oled_trust::journal::OsErr> {
            self.0.extend_from_slice(bytes);
            Ok(())
        }
    }

    /// Only the file calls are used by an undo of FileCreated; the rest is never reached.
    #[derive(Default)]
    struct FileOnlyOs {
        files: Vec<(String, oled_trust::journal::Digest)>,
        deleted: Vec<String>,
    }

    impl oled_trust::journal::JournalOs for FileOnlyOs {
        fn reg_get(
            &self,
            _: oled_trust::journal::Hive,
            _: &oled_trust::journal::RegPath,
            _: Option<&str>,
        ) -> Result<Option<oled_trust::journal::RegData>, oled_trust::journal::OsErr> {
            unreachable!()
        }
        fn reg_set(
            &mut self,
            _: oled_trust::journal::Hive,
            _: &oled_trust::journal::RegPath,
            _: Option<&str>,
            _: Option<&oled_trust::journal::RegData>,
        ) -> Result<(), oled_trust::journal::OsErr> {
            unreachable!()
        }
        fn reg_key_exists(
            &self,
            _: oled_trust::journal::Hive,
            _: &oled_trust::journal::RegPath,
        ) -> Result<bool, oled_trust::journal::OsErr> {
            unreachable!()
        }
        fn reg_key_create(
            &mut self,
            _: oled_trust::journal::Hive,
            _: &oled_trust::journal::RegPath,
        ) -> Result<(), oled_trust::journal::OsErr> {
            unreachable!()
        }
        fn reg_key_delete_if_empty(
            &mut self,
            _: oled_trust::journal::Hive,
            _: &oled_trust::journal::RegPath,
        ) -> Result<bool, oled_trust::journal::OsErr> {
            unreachable!()
        }
        fn reg_tree_get(
            &self,
            _: oled_trust::journal::Hive,
            _: &oled_trust::journal::RegPath,
        ) -> Result<Option<oled_trust::journal::RegSnapshot>, oled_trust::journal::OsErr> {
            unreachable!()
        }
        fn reg_tree_put(
            &mut self,
            _: oled_trust::journal::Hive,
            _: &oled_trust::journal::RegPath,
            _: Option<&oled_trust::journal::RegSnapshot>,
        ) -> Result<(), oled_trust::journal::OsErr> {
            unreachable!()
        }
        fn task_xml(&self, _: &oled_trust::journal::TaskName) -> Result<Option<String>, oled_trust::journal::OsErr> {
            unreachable!()
        }
        fn task_put(
            &mut self,
            _: &oled_trust::journal::TaskName,
            _: Option<&str>,
        ) -> Result<(), oled_trust::journal::OsErr> {
            unreachable!()
        }
        fn file_digest(&self, path: &str) -> Result<Option<oled_trust::journal::Digest>, oled_trust::journal::OsErr> {
            Ok(self.files.iter().find(|(p, _)| p == path).map(|(_, d)| *d))
        }
        fn file_delete(&mut self, path: &str) -> Result<(), oled_trust::journal::OsErr> {
            self.deleted.push(path.to_string());
            self.files.retain(|(p, _)| p != path);
            Ok(())
        }
        fn file_restore(&mut self, _: &str, _: &str) -> Result<(), oled_trust::journal::OsErr> {
            unreachable!()
        }
        fn digest(&self, bytes: &[u8]) -> Result<oled_trust::journal::Digest, oled_trust::journal::OsErr> {
            oled_sys::sha256::sha256(bytes).map_err(|what| oled_trust::journal::OsErr { code: 0, what })
        }
    }

    #[test]
    fn install_records_stay_open_and_are_not_doubled() {
        use oled_trust::journal::{Change, Journal, Kind, KindClass, Scope, UndoSet};
        let mut j = Journal::empty(Scope::Machine);
        let mut sink = MemSink::default();
        let run = j.begin_run();
        let path = Path::new(START).join("x.lnk");
        let digest = [9u8; 32];
        record_install(&mut j, &mut sink, run, &path, digest, 1).unwrap();
        // No Committed record is ever written for the run: the entry is open.
        let open = j.open_entries();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].kind, Kind::Install);
        assert!(matches!(&open[0].change, Change::FileCreated { after, .. } if *after == digest));
        // The same link again (same path ignoring case, same bytes) appends nothing.
        let before = sink.0.len();
        let upper = Path::new(START).join("X.LNK");
        record_install(&mut j, &mut sink, run, &upper, digest, 2).unwrap();
        assert_eq!(sink.0.len(), before);
        // What is on disk parses back to the same open entry.
        let back = Journal::parse(Scope::Machine, &sink.0).unwrap();
        assert_eq!(back.open_entries().len(), 1);
        // The uninstall's undo of the Install class selects it and deletes the file.
        let mut os = FileOnlyOs { files: vec![(path.to_str().unwrap().to_string(), digest)], ..FileOnlyOs::default() };
        let r = oled_trust::journal::undo(&mut j, &mut sink, &mut os, &UndoSet::Classes(vec![KindClass::Install]));
        assert_eq!(r.restored, 1, "{r:?}");
        assert_eq!(os.deleted, [path.to_str().unwrap().to_string()]);
        assert!(j.open_entries().is_empty());
    }

    #[test]
    fn a_file_that_appeared_is_left_alone() {
        let mut os = Fake { exists_late: vec![Path::new(START).join(en(ComponentId::Tasks))], ..Fake::default() };
        assert_eq!(run(&mut os, &cfg(true, false), false), Ok(0));
    }

    #[test]
    fn plain_names_only() {
        for ok in ["Tasks", "Attivit\u{e0}", "Two words"] {
            assert!(plain_name(ok), "{ok}");
        }
        let long = "a".repeat(201);
        for bad in [
            "",
            ".",
            "..",
            " x",
            "x ",
            "x.",
            "a\\b",
            "a/b",
            "..\\x",
            "C:x",
            "a*b",
            "a?b",
            "a\"b",
            "a<b",
            "a>b",
            "a|b",
            "a\u{1}b",
            long.as_str(),
        ] {
            assert!(!plain_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn a_real_link_has_target_and_a_string_aumid() {
        use windows::Win32::System::Com::StructuredStorage::PropVariantClear;
        use windows::Win32::System::Com::STGM_READ;
        let dir = std::env::temp_dir().join(format!("entry-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let install = dir.join("suite");
        let mut os = RealLinks::new(&install);
        let w = wanted(&cfg(true, false), &[ComponentId::Tasks], &dir, &install, false).remove(0);
        let bytes = os.build(&w.1).unwrap();
        assert!(!bytes.is_empty() && bytes.len() <= LINK_MAX as usize);
        let lnk = dir.join("probe.lnk");
        assert_eq!(os.write(&lnk, &bytes), Ok(true));
        assert_eq!(os.write(&lnk, &bytes), Ok(false), "an existing file is not overwritten");
        // SAFETY: COM is initialised by RealLinks::new on this thread; interfaces match the class; the wide string
        // lives for the calls; the variant is cleared after use.
        let (target, vt, id) = unsafe {
            let link = CoCreateInstance::<_, IShellLinkW>(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
            let file: IPersistFile = link.cast().unwrap();
            let wide = HSTRING::from(lnk.as_os_str());
            file.Load(PCWSTR(wide.as_ptr()), STGM_READ).unwrap();
            let target = crate::shell_thread::lnk_target(&link, &file, &lnk);
            let store: IPropertyStore = link.cast().unwrap();
            let mut pv = store.GetValue(&PKEY_AUMID).unwrap();
            let vt = pv.vt();
            let id = windows::core::PWSTR(pv.Anonymous.Anonymous.Anonymous.pwszVal.0).to_string().unwrap_or_default();
            let _ = PropVariantClear(&mut pv);
            (target, vt, id)
        };
        os.finish();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(target.map(PathBuf::from), Some(w.1.target.clone()));
        assert_eq!(vt, VT_LPWSTR);
        assert_eq!(id, "MiniOledDock.Tasks");
    }

    #[test]
    fn config_keys_default_on() {
        let d = Config::default();
        assert!(d.suite_tasks && d.suite_files);
        assert!(Config::known_keys().contains(&"suite_tasks") && Config::known_keys().contains(&"suite_files"));
        let off = cfg(false, true);
        let back: Config = serde_json::from_str(&serde_json::to_string(&off).unwrap()).unwrap();
        assert!(!back.suite_tasks && back.suite_files);
        // A file written before 1.8.0 has neither key: both read on.
        let old: Config = serde_json::from_str("{}").unwrap();
        assert!(old.suite_tasks && old.suite_files);
    }

    #[test]
    fn on_config_asks_only_for_a_change() {
        let mut last = None;
        let a = (true, true, Language::Auto);
        assert!(!changed(&mut last, a), "the first call only remembers");
        assert!(!changed(&mut last, a));
        assert!(changed(&mut last, (false, true, Language::Auto)));
        assert!(changed(&mut last, (false, true, Language::It)));
        assert!(!changed(&mut last, (false, true, Language::It)));
    }

    #[test]
    fn names_come_from_the_table() {
        // No quoted display name in the code before the tests: the names are the kit table's.
        let src = include_str!("entry_points.rs");
        let code = &src[..src.find("#[cfg(test)]").unwrap()];
        for name in ["Tasks", "Folio", "Attivit"] {
            let quoted = format!("\"{name}");
            assert!(!code.contains(&quoted), "a display name in the code: {name}");
        }
        assert_eq!(display_name(ComponentId::Tasks, false), "Tasks");
        assert_ne!(display_name(ComponentId::Tasks, true), "");
    }
}
