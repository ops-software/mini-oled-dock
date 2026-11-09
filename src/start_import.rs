//! "Import from Windows Start" (Settings -> Appearance): replaces `config.start_pins` with the pins
//! of the native Start menu, in the Windows order.
//!
//! JOB: one short-lived thread per click (`start`), ended when the job ends. The dock runs elevated
//! and never starts PowerShell itself (its modules are found through the user-writable
//! `PSModulePath`): it asks the desktop shell (Explorer, at the user's medium integrity,
//! `shell_thread::desktop_open`) to run `powershell.exe` with `export_args`. That de-elevated
//! PowerShell writes `Export-StartLayout` to `start-layout-<128-bit random id>.json.tmp` in
//! `<LocalAppData of the shell user>\mini-oled-dock\`, renames it to `.json` when complete (or
//! writes `.json.err` on failure) and deletes the files of that pattern older than one hour.
//! The dock only polls for the two names (`poll_step`, `LAYOUT_TIMEOUT`), never writes, renames or
//! deletes anything in that folder and never reads the `.err` content. The JSON is untrusted input:
//! read through `datadir::read_file` (no reparse points, single link, `MAX_JSON` cap), parsed by
//! serde_json (`map_layout`, pure), then the pins are saved through `config::modify`. The
//! AppsFolder catalog (`shell_thread::enum_apps_folder`) is enumerated on that same thread and only
//! when a `.lnk` entry needs it, so nothing stays in memory afterwards.
//!
//! RESULT: one atomic word (`STATUS`), read by the Settings popup on its 1 s tick (the popup's own
//! timer, alive only while it is open). No new timer, no thread left running.

use std::os::windows::io::{FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering::SeqCst};
use std::time::{Duration, Instant};

use windows::core::{Interface, HSTRING, PCWSTR};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Security::Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG};
use windows::Win32::Security::{TOKEN_DUPLICATE, TOKEN_IMPERSONATE, TOKEN_QUERY};
use windows::Win32::Storage::FileSystem::WIN32_FIND_DATAW;
use windows::Win32::System::Com::{
    CLSIDFromString, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, IPersistFile, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED, STGM_READ,
};
use windows::Win32::System::Threading::{OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::Shell::{FOLDERID_LocalAppData, IShellLinkW, SHGetKnownFolderPath, ShellLink, KF_FLAG_DEFAULT};

use crate::log;
use crate::shell_thread::StartApp;

/// Longest wait for the de-elevated export.
const LAYOUT_TIMEOUT: Duration = Duration::from_secs(15);
/// Interval between two checks for the layout file.
const POLL_MS: u64 = 200;
/// Largest layout file read back.
const MAX_JSON: u64 = 4 << 20;

/// `STATUS` values; `DONE + n` = n apps imported.
const IDLE: u32 = 0;
const RUNNING: u32 = 1;
const FAILED: u32 = 2;
const TIMEDOUT: u32 = 3;
const DONE: u32 = 4;

static STATUS: AtomicU32 = AtomicU32::new(IDLE);

/// State of the import, as shown by the Settings row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    Idle,
    Running,
    Failed,
    TimedOut,
    Imported(u32),
}

fn decode(v: u32) -> Status {
    match v {
        IDLE => Status::Idle,
        RUNNING => Status::Running,
        FAILED => Status::Failed,
        // Before the catch-all: `TIMEDOUT - DONE` would underflow.
        TIMEDOUT => Status::TimedOut,
        n => Status::Imported(n - DONE),
    }
}

/// Why a job ended without importing.
#[derive(Debug)]
enum JobError {
    Failed(String),
    TimedOut,
}

impl From<String> for JobError {
    fn from(e: String) -> Self {
        JobError::Failed(e)
    }
}

/// Status word for the result of a job.
fn finish_word(r: &Result<usize, JobError>) -> u32 {
    match r {
        Ok(n) => DONE + *n as u32,
        Err(JobError::TimedOut) => TIMEDOUT,
        Err(JobError::Failed(_)) => FAILED,
    }
}

/// Raw status word (cheap change detection for the popup tick).
pub(crate) fn status_word() -> u32 {
    STATUS.load(SeqCst)
}

pub(crate) fn status() -> Status {
    decode(status_word())
}

/// Forgets the result of a finished import (the Settings popup opens with the plain label).
pub(crate) fn reset() {
    let cur = STATUS.load(SeqCst);
    if cur != RUNNING {
        let _ = STATUS.compare_exchange(cur, IDLE, SeqCst, SeqCst);
    }
}

/// Starts the import on a short-lived thread. false if one is already running.
pub(crate) fn start() -> bool {
    let cur = STATUS.load(SeqCst);
    if cur == RUNNING || STATUS.compare_exchange(cur, RUNNING, SeqCst, SeqCst).is_err() {
        return false;
    }
    let spawned = std::thread::Builder::new().name("start-import".into()).spawn(|| {
        let r = job();
        match &r {
            Ok(n) => log!("start import: {n} apps imported"),
            Err(JobError::Failed(e)) => log!("start import: failed: {e}"),
            Err(JobError::TimedOut) => log!("start import: no layout file after {LAYOUT_TIMEOUT:?} (timeout)"),
        }
        STATUS.store(finish_word(&r), SeqCst);
    });
    if let Err(e) = spawned {
        log!("start import: thread not started: {e}");
        STATUS.store(FAILED, SeqCst);
    }
    true
}

// ------------------------------------------------------------------ the job (import thread)

/// Folder of the layout file: the data folder when the dock can write it, else the temp folder.
fn work_dir() -> PathBuf {
    if crate::datadir::writable() {
        if let Some(dir) = crate::datadir::config_path().and_then(|p| p.parent().map(Path::to_path_buf)) {
            return dir;
        }
    }
    std::env::temp_dir()
}

/// Next step of the wait for the de-elevated export.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Poll {
    Wait,
    Ready,
    Failed,
    TimedOut,
}

/// `file_exists` / `err_exists`: the `.json` / `.json.err` names exist. The `.json` only appears
/// complete (renamed from `.tmp`), so it wins over an `.err`.
fn poll_step(file_exists: bool, err_exists: bool, elapsed: Duration) -> Poll {
    if file_exists {
        Poll::Ready
    } else if err_exists {
        Poll::Failed
    } else if elapsed >= LAYOUT_TIMEOUT {
        Poll::TimedOut
    } else {
        Poll::Wait
    }
}

/// 32 lowercase hex chars.
fn hex16(id: &[u8; 16]) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

fn layout_file_name(id: &[u8; 16]) -> String {
    format!("start-layout-{}.json", hex16(id))
}

/// Whole argument string for `powershell.exe`: every path is a single-quoted PowerShell literal
/// (only the quote itself needs doubling); `-WindowStyle Hidden` because the shell starts it with
/// `SW_SHOWNORMAL`.
fn export_args(dir: &str, file: &str) -> String {
    let q = |s: &str| format!("'{}'", s.replace('\'', "''"));
    let (d, old, tmp, json, err) =
        (q(dir), q(&format!("{dir}\\start-layout-*")), q(&format!("{file}.tmp")), q(file), q(&format!("{file}.err")));
    format!(
        "-NoProfile -NonInteractive -ExecutionPolicy Bypass -WindowStyle Hidden -Command \"try {{ \
         New-Item -ItemType Directory -Force {d} | Out-Null; \
         Get-ChildItem {old} | Where-Object LastWriteTime -lt (Get-Date).AddHours(-1) | Remove-Item -Force; \
         Export-StartLayout -Path {tmp}; Move-Item -Force {tmp} {json} }} catch {{ Set-Content {err} $_ }}\""
    )
}

/// 128 random bits from the system-preferred RNG.
fn random_id() -> Result<[u8; 16], String> {
    let mut id = [0u8; 16];
    // SAFETY: null algorithm handle with BCRYPT_USE_SYSTEM_PREFERRED_RNG; `id` is writable.
    unsafe { BCryptGenRandom(None, &mut id, BCRYPT_USE_SYSTEM_PREFERRED_RNG) }
        .ok()
        .map_err(|e| format!("BCryptGenRandom failed: {e}"))?;
    Ok(id)
}

/// `<LocalAppData of the user owning process explorer_pid>\mini-oled-dock`, from that process's token
/// (the elevated dock's own profile may belong to another account and is never used).
fn shell_user_local_appdata(explorer_pid: u32) -> Result<PathBuf, String> {
    // SAFETY: query-only open of another process; the handle is owned by `_proc` below.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, explorer_pid) }
        .map_err(|e| format!("OpenProcess({explorer_pid}): {e}"))?;
    // SAFETY: fresh valid handle, ownership transferred (closed on every return path).
    let _proc = unsafe { OwnedHandle::from_raw_handle(h.0) };
    let mut token = HANDLE::default();
    // SAFETY: `h` is open while `_proc` lives; `token` is writable.
    unsafe { OpenProcessToken(h, TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_IMPERSONATE, &mut token) }
        .map_err(|e| format!("OpenProcessToken({explorer_pid}): {e}"))?;
    // SAFETY: fresh valid token handle, ownership transferred (closed on every return path).
    let _token = unsafe { OwnedHandle::from_raw_handle(token.0) };
    // SAFETY: valid known-folder id and token; the returned buffer is freed below.
    let p = unsafe { SHGetKnownFolderPath(&FOLDERID_LocalAppData, KF_FLAG_DEFAULT, Some(token)) }
        .map_err(|e| format!("SHGetKnownFolderPath(LocalAppData): {e}"))?;
    // SAFETY: NUL-terminated string returned by SHGetKnownFolderPath, read once.
    let base = unsafe { p.to_string() };
    // SAFETY: same pointer, allocated with CoTaskMemAlloc, not used after this line.
    unsafe { CoTaskMemFree(Some(p.0 as *const std::ffi::c_void)) };
    let base = base.map_err(|e| format!("LocalAppData path: {e}"))?;
    if base.is_empty() {
        return Err("empty LocalAppData path".into());
    }
    Ok(PathBuf::from(base).join("mini-oled-dock"))
}

/// Uninitializes COM when `job` returns (declared first there, so dropped after every COM object).
struct Com(bool);

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: paired with the successful CoInitializeEx in `job`; every COM object is dropped.
            unsafe { CoUninitialize() };
        }
    }
}

fn job() -> Result<usize, JobError> {
    // COM (STA) only for this job: the desktop shell, shell links and the AppsFolder catalog.
    // SAFETY: initializes COM on this thread; paired with CoUninitialize in `Com::drop`.
    let _com = Com(unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok());
    let pid = crate::shell_thread::explorer_pid().map_err(|e| {
        JobError::Failed(format!(
            "Explorer's desktop is not available (Explorer stopped, or restarted and not yet registered with ShellWindows): {e}"
        ))
    })?;
    let dir = shell_user_local_appdata(pid)?;
    let id = random_id()?;
    let file = dir.join(layout_file_name(&id));
    let mut err = file.clone().into_os_string();
    err.push(".err");
    let err = PathBuf::from(err);
    let ps = crate::shell_thread::system_dir()
        .ok_or_else(|| JobError::Failed("no system directory".into()))?
        .join(r"WindowsPowerShell\v1.0\powershell.exe");
    let args = export_args(&dir.to_string_lossy(), &file.to_string_lossy());
    crate::shell_thread::desktop_open(&ps.to_string_lossy(), Some(&args), None)
        .map_err(|e| JobError::Failed(format!("cannot start the de-elevated export: {e}")))?;
    let t = Instant::now();
    loop {
        // symlink_metadata never follows a link; only existence is checked, the `.err` is never read.
        let (ready, failed) = (std::fs::symlink_metadata(&file).is_ok(), std::fs::symlink_metadata(&err).is_ok());
        match poll_step(ready, failed, t.elapsed()) {
            Poll::Wait => std::thread::sleep(Duration::from_millis(POLL_MS)),
            Poll::Ready => break,
            Poll::Failed => return Err(JobError::Failed(format!("the export failed ({} exists)", err.display()))),
            Poll::TimedOut => return Err(JobError::TimedOut),
        }
    }
    let bytes =
        crate::datadir::read_file(&file, MAX_JSON, true).map_err(|e| format!("reading {}: {e}", file.display()))?;
    let text = String::from_utf8(bytes).map_err(|e| format!("layout file is not UTF-8: {e}"))?;
    let mapped = {
        let link = Links::new();
        let mut catalog = || -> Vec<(StartApp, Option<String>)> {
            let apps = crate::shell_thread::enum_apps_folder();
            log!("start import: {} apps in the catalog", apps.len());
            apps.into_iter()
                .map(|a| {
                    let p = known_folder_form(&a.id);
                    (a, p)
                })
                .collect()
        };
        map_layout(&text, &mut catalog, &|lnk| link.as_ref().and_then(|l| l.target(lnk)))
    };
    let pins = mapped?;
    if pins.is_empty() {
        return Err(String::from("no pinned app could be mapped").into());
    }
    let n = pins.len();
    crate::config::modify(move |c| c.start_pins = pins)?;
    Ok(n)
}

/// `{GUID}\rest` (an AppsFolder id of a classic program) as a file path, lowercase.
fn known_folder_form(id: &str) -> Option<String> {
    let close = id.find('}')?;
    if !id.starts_with('{') {
        return None;
    }
    let guid_text = HSTRING::from(&id[..=close]);
    // SAFETY: `guid_text` is NUL-terminated and lives for the call.
    let guid = unsafe { CLSIDFromString(PCWSTR(guid_text.as_ptr())) }.ok()?;
    // SAFETY: valid known-folder id, current user; the returned buffer is freed below.
    let p = unsafe { SHGetKnownFolderPath(&guid, KF_FLAG_DEFAULT, None) }.ok()?;
    // SAFETY: NUL-terminated string returned by SHGetKnownFolderPath, read once.
    let base = unsafe { p.to_string() }.ok();
    // SAFETY: same pointer, allocated with CoTaskMemAlloc, not used after this line.
    unsafe { CoTaskMemFree(Some(p.0 as *const std::ffi::c_void)) };
    Some(norm(&format!("{}{}", base?, &id[close + 1..])))
}

/// One `ShellLink` object for every `.lnk` of the layout.
struct Links {
    link: IShellLinkW,
    file: IPersistFile,
}

impl Links {
    fn new() -> Option<Links> {
        // SAFETY: COM STA initialized on this thread; ShellLink CLSID with the requested interface.
        let link: IShellLinkW = unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER) }.ok()?;
        let file = link.cast::<IPersistFile>().ok()?;
        Some(Links { link, file })
    }

    /// Target path of the shortcut `lnk` (no UI, no search).
    fn target(&self, lnk: &str) -> Option<String> {
        let wide = HSTRING::from(lnk);
        // SAFETY: `wide` is NUL-terminated and lives for the call.
        unsafe { self.file.Load(PCWSTR(wide.as_ptr()), STGM_READ) }.ok()?;
        let mut buf = [0u16; 520];
        let mut fd = WIN32_FIND_DATAW::default();
        // SAFETY: `buf` and `fd` are writable and sized as declared; flags 0 = long path.
        unsafe { self.link.GetPath(&mut buf, &mut fd, 0) }.ok()?;
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let s = String::from_utf16_lossy(&buf[..n]);
        Some(s).filter(|s| !s.is_empty())
    }
}

// ------------------------------------------------------------------ mapping (pure)

fn norm(p: &str) -> String {
    p.trim().replace('/', "\\").to_lowercase()
}

/// File name of `path` without the extension.
fn stem(path: &str) -> &str {
    let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
    match name.rfind('.') {
        Some(i) if i > 0 => &name[..i],
        _ => name,
    }
}

/// Maps the `Export-StartLayout` JSON to AppsFolder ids, in order, without duplicates.
/// `packagedAppId` / `desktopAppId` are kept as they are; a `desktopAppLink` (environment variables expanded) is matched against the
/// catalog: first an id equal to the shortcut's target (`target`), as a plain path or in its
/// `{KnownFolder}\...` form (the `Option` next to each app, lowercase), then the shortcut's file
/// name against the app names. `catalog` is called once, and only when a link needs it. Entries
/// that cannot be mapped are skipped (logged). Err = not a layout file.
pub(crate) fn map_layout(
    json: &str,
    catalog: &mut dyn FnMut() -> Vec<(StartApp, Option<String>)>,
    target: &dyn Fn(&str) -> Option<String>,
) -> Result<Vec<String>, String> {
    let v: serde_json::Value =
        serde_json::from_str(json.trim_start_matches('\u{feff}')).map_err(|e| format!("layout JSON: {e}"))?;
    let list = v.get("pinnedList").and_then(|l| l.as_array()).ok_or("layout JSON without pinnedList")?;
    let mut out: Vec<String> = Vec::new();
    let mut loaded: Option<Vec<(StartApp, Option<String>)>> = None;
    for entry in list {
        let id = if let Some(id) = entry.get("packagedAppId").or_else(|| entry.get("desktopAppId")).and_then(|x| x.as_str()) {
            Some(id.trim().to_string()).filter(|s| !s.is_empty())
        } else if let Some(link) = entry.get("desktopAppLink").and_then(|x| x.as_str()) {
            let link = crate::shell_thread::expand_env(link);
            let apps = loaded.get_or_insert_with(&mut *catalog);
            let by_target = target(&link).map(|t| norm(&crate::shell_thread::expand_env(&t))).and_then(|t| {
                apps.iter()
                    .find(|(a, kf)| norm(&a.id) == t || kf.as_deref() == Some(t.as_str()))
                    .map(|(a, _)| a.id.clone())
            });
            let found = by_target.or_else(|| {
                let s = stem(&link);
                apps.iter().find(|(a, _)| a.name.eq_ignore_ascii_case(s)).map(|(a, _)| a.id.clone())
            });
            if found.is_none() {
                log!("start import: no app for the shortcut {link}");
            }
            found
        } else {
            log!("start import: entry skipped: {entry}");
            None
        };
        if let Some(id) = id {
            if !out.iter().any(|o| o.eq_ignore_ascii_case(&id)) {
                out.push(id);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str, name: &str, kf: Option<&str>) -> (StartApp, Option<String>) {
        (StartApp { id: id.into(), name: name.into() }, kf.map(str::to_string))
    }

    #[test]
    fn layout_maps_ids_links_and_skips_the_rest() {
        let apps = vec![
            app("Microsoft.Windows.Explorer", "File Explorer", None),
            app("{6D809377-6AF0-444B-8957-A3773F02200E}\\Git\\git-bash.exe", "Git Bash", Some("c:\\program files\\git\\git-bash.exe")),
            app("C:\\Tools\\tool.exe", "Some Tool", None),
            app("Microsoft.WindowsStore_8wekyb3d8bbwe!App", "Microsoft Store", None),
        ];
        let json = r#"{"pinnedList":[
            {"packagedAppId":"Microsoft.WindowsStore_8wekyb3d8bbwe!App"},
            {"desktopAppLink":"%APPDATA%\\Microsoft\\Windows\\Start Menu\\Programs\\File Explorer.lnk"},
            {"desktopAppLink":"C:\\ProgramData\\Microsoft\\Windows\\Start Menu\\Programs\\Git\\Bash here.lnk"},
            {"desktopAppLink":"C:\\Links\\Tool.lnk"},
            {"desktopAppLink":"C:\\Links\\Missing.lnk"},
            {"desktopAppId":"Custom.App!Main"},
            {"packagedAppId":"microsoft.windowsstore_8wekyb3d8bbwe!app"},
            {"secondaryTile":"x"}
        ]}"#;
        let target = |lnk: &str| -> Option<String> {
            if lnk.ends_with("Bash here.lnk") {
                Some("C:/Program Files/Git/git-bash.exe".into())
            } else if lnk.ends_with("Tool.lnk") {
                Some("c:\\tools\\TOOL.exe".into())
            } else {
                None
            }
        };
        let mut calls = 0;
        let mut cat = || {
            calls += 1;
            apps.clone()
        };
        let got = map_layout(json, &mut cat, &target).unwrap();
        assert_eq!(
            got,
            vec![
                "Microsoft.WindowsStore_8wekyb3d8bbwe!App".to_string(),
                "Microsoft.Windows.Explorer".into(),
                "{6D809377-6AF0-444B-8957-A3773F02200E}\\Git\\git-bash.exe".into(),
                "C:\\Tools\\tool.exe".into(),
                "Custom.App!Main".into(),
            ]
        );
        assert_eq!(calls, 1, "the catalog is enumerated once");
    }

    #[test]
    fn layout_without_links_never_loads_the_catalog() {
        let mut cat = || -> Vec<(StartApp, Option<String>)> { panic!("catalog not needed") };
        let got = map_layout("\u{feff}{\"pinnedList\":[{\"packagedAppId\":\"A!B\"}]}", &mut cat, &|_| None).unwrap();
        assert_eq!(got, vec!["A!B".to_string()]);
        assert!(map_layout("{}", &mut cat, &|_| None).is_err());
        assert!(map_layout("not json", &mut cat, &|_| None).is_err());
        assert!(map_layout("{\"pinnedList\":[]}", &mut cat, &|_| None).unwrap().is_empty());
    }

    #[test]
    fn stems_and_status() {
        assert_eq!(stem("C:\\a\\File Explorer.lnk"), "File Explorer");
        assert_eq!(stem("x/y/Tool"), "Tool");
        assert_eq!(decode(IDLE), Status::Idle);
        assert_eq!(decode(DONE + 7), Status::Imported(7));
        assert_eq!(decode(FAILED), Status::Failed);
    }

    #[test]
    fn layout_file_name_is_128_bit_hex() {
        let id = [0xAB, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 0xFF];
        assert_eq!(hex16(&id), "ab000102030405060708090a0b0c0dff");
        assert_eq!(layout_file_name(&id), "start-layout-ab000102030405060708090a0b0c0dff.json");
    }

    #[test]
    fn export_args_are_one_quoted_literal_per_path() {
        let dir = r"C:\Users\O'Brien\AppData\Local\mini-oled-dock";
        let file = r"C:\Users\O'Brien\AppData\Local\mini-oled-dock\start-layout-ab.json";
        let a = export_args(dir, file);
        assert!(a.starts_with("-NoProfile -NonInteractive -ExecutionPolicy Bypass -WindowStyle Hidden -Command \""));
        assert!(a.contains(r"'C:\Users\O''Brien\AppData\Local\mini-oled-dock'"), "apostrophe doubled: {a}");
        assert!(a.contains(
            r"Export-StartLayout -Path 'C:\Users\O''Brien\AppData\Local\mini-oled-dock\start-layout-ab.json.tmp'"
        ));
        assert!(a.contains("Move-Item -Force") && a.contains(".json.err'") && a.contains("AddHours(-1)"));
        assert_eq!(a.matches('"').count(), 2, "only the -Command wrapper uses double quotes");
    }

    #[test]
    fn poll_decides_ready_failed_timeout() {
        let s = Duration::from_secs;
        assert_eq!(poll_step(false, false, s(0)), Poll::Wait);
        assert_eq!(poll_step(true, false, s(1)), Poll::Ready);
        assert_eq!(poll_step(false, true, s(1)), Poll::Failed);
        assert_eq!(poll_step(true, true, s(1)), Poll::Ready, "a complete file wins over a stale .err");
        assert_eq!(poll_step(false, false, s(14)), Poll::Wait);
        assert_eq!(poll_step(false, false, s(15)), Poll::TimedOut);
    }

    #[test]
    fn hostile_layout_json_is_rejected_or_skipped_without_panic() {
        let mut cat = || -> Vec<(StartApp, Option<String>)> { panic!("catalog not needed") };
        let deep = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
        assert!(map_layout(&deep, &mut cat, &|_| None).is_err(), "serde_json recursion limit");
        assert!(map_layout(r#"{"pinnedList":"x"}"#, &mut cat, &|_| None).is_err());
        let odd = r#"{"pinnedList":[1,null,[],{"packagedAppId":5},{"packagedAppId":""}]}"#;
        assert!(map_layout(odd, &mut cat, &|_| None).unwrap().is_empty());
        assert_eq!(decode(IDLE), Status::Idle);
        assert_eq!(decode(TIMEDOUT), Status::TimedOut, "the TIMEDOUT arm precedes the Imported catch-all");
        assert_eq!(decode(DONE + 7), Status::Imported(7));
    }

    #[test]
    fn job_results_map_to_status_words() {
        assert_eq!(finish_word(&Ok(7)), DONE + 7);
        assert_eq!(finish_word(&Err(JobError::TimedOut)), TIMEDOUT);
        assert_eq!(finish_word(&Err(JobError::Failed("x".into()))), FAILED);
        // A failing setup step (random id, shell user folder, Explorer not reachable) is reported like any
        // other failure: the `String` converts to `Failed`, the popup shows "Import failed".
        let setup: Result<usize, JobError> = Err(String::from("BCryptGenRandom failed").into());
        assert_eq!(finish_word(&setup), FAILED);
    }

    #[test]
    fn setup_steps_return_errors_instead_of_panicking() {
        let (a, b) = (random_id().expect("BCryptGenRandom"), random_id().unwrap());
        assert_ne!(a, b);
        assert_ne!(a, [0u8; 16]);
        assert!(shell_user_local_appdata(0).is_err(), "no such process: Err, never the dock's own folder");
        // If SHGetKnownFolderPath rejects the test process token on the build machine, drop only this
        // positive assertion (the owner test in Step 6 covers it); keep the three above.
        let dir = shell_user_local_appdata(std::process::id()).expect("own token");
        assert!(dir.is_absolute() && dir.ends_with("mini-oled-dock"), "{dir:?}");
    }
}
