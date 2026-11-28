//! Automatic start at logon, ELEVATED and without a UAC prompt, via Task Scheduler.
//!
//! CHOICE: `schtasks.exe /Create /XML` instead of the `ITaskService` COM API. Reasons: the XML expresses
//! all the required settings (logon trigger, RunLevel highest, interactive token, no
//! time limit, no stop on battery, StartWhenAvailable) in a declarative and
//! documented format, without extra `windows` crate features, without COM vtables to manage and
//! without depending on the apartment of the calling thread; the result is the exit code (independent
//! of the system language). `schtasks.exe` is launched with an absolute path in System32 (the
//! dock is elevated: no PATH search) and with `CREATE_NO_WINDOW`.
//!
//! The "Dock" task runs the INSTALLED copy (`%ProgramFiles%\mini-oled-dock\dock.exe`, resolved with
//! `FOLDERID_ProgramFiles`) with `--autostart`: this way rebuilding in `target\release` does not touch
//! the exe started at logon, and updating is `cargo build --release` + `dock.exe --install`.
//!
//! SECURITY: the task runs ELEVATED, so the file it runs must not be writable without admin rights.
//! The install folder is created under Program Files and inherits its ACL (Administrators/SYSTEM/
//! TrustedInstaller write, Users read+execute); this module never sets or loosens an ACL. The task
//! XML is staged in the same admin-only folder (not in `%LOCALAPPDATA%`, where a medium process could
//! swap it between our write and `schtasks /Create`). The legacy `%LOCALAPPDATA%\Dock\bin\dock.exe`
//! (medium-writable) is NOT deleted by `--install`: an elevated delete inside a medium-writable folder
//! could be redirected by a junction/symlink to an arbitrary file. The user is told to remove it.
//!
//! THREADING: all functions block (child processes, waits): NEVER from the UI thread. The power
//! popup calls them from throwaway threads.

use std::ffi::c_void;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{
    MoveFileExW, FILE_ATTRIBUTE_REPARSE_POINT, FILE_SHARE_READ, MOVEFILE_DELAY_UNTIL_REBOOT, MOVEFILE_REPLACE_EXISTING,
    MOVEFILE_WRITE_THROUGH,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, UnmapViewOfFile, FILE_MAP_READ, MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READONLY,
};
use windows::Win32::System::ProcessStatus::EnumProcesses;
use windows::Win32::System::Threading::{GetCurrentProcessId, CREATE_NO_WINDOW};
use windows::Win32::UI::Shell::{
    FOLDERID_CommonPrograms, FOLDERID_ProgramFiles, SHGetKnownFolderPath, KF_FLAG_DEFAULT,
};

use oled_sys::journal_os::{self, RealOs};
use oled_trust::journal::{
    self, Change, Digest, Entry, Hive, Kind, KindClass, RegPath, Scope, Target, TaskName, UndoReport, UndoSet,
};
use oled_trust::manifest::suite::{
    parse_manifest, ComponentId, SuiteManifest, COMPONENT_MAX_BYTES, SUITE_ASSET, SUITE_MAX_BYTES,
};

use crate::log;
use crate::update_release::{parse_version_output, Version};

/// Exit codes of the CLI modes (`--install`, `--uninstall`, argument errors).
pub const EXIT_OK: i32 = 0;
pub const EXIT_FAILED: i32 = 1;
pub const EXIT_BAD_ARGS: i32 = 2;
/// The old instance (or its watchdog) did not release the installed exe in time.
pub const EXIT_STOP_FAILED: i32 = 3;

/// What `copy_self` did with the installed exe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CopyOutcome {
    /// This process already runs from the installed copy: nothing to copy.
    AlreadyInstalled,
    Copied,
    /// The installed exe (if any) is unchanged.
    Failed,
}

/// Why an install did not complete.
#[derive(Debug)]
pub(crate) enum InstallError {
    Failed(String),
    /// The running dock or its watchdog kept the exe past `STOP_WAIT`.
    CouldNotStop(String),
}

impl InstallError {
    pub(crate) fn exit_code(&self) -> i32 {
        match self {
            InstallError::Failed(_) => EXIT_FAILED,
            InstallError::CouldNotStop(_) => EXIT_STOP_FAILED,
        }
    }
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstallError::Failed(m) | InstallError::CouldNotStop(m) => f.write_str(m),
        }
    }
}

/// Result of `enable`: the copy and the first error, if any (the copy may succeed and the task fail).
#[derive(Debug)]
pub(crate) struct InstallReport {
    pub copy: CopyOutcome,
    /// The stop event of a running dock was signalled.
    pub stop_signalled: bool,
    pub error: Option<InstallError>,
}

impl InstallReport {
    fn failed(error: InstallError) -> Self {
        InstallReport { copy: CopyOutcome::Failed, stop_signalled: false, error: Some(error) }
    }

    pub(crate) fn exit_code(&self) -> i32 {
        self.error.as_ref().map_or(EXIT_OK, InstallError::exit_code)
    }
}

/// Whether `--install` starts the installed exe at the end. Pure. It always does, unless
/// `--no-relaunch` or a dock is still running (re-run over itself, or it could not be stopped).
pub(crate) fn should_relaunch(still_running: bool, no_relaunch: bool) -> bool {
    !still_running && !no_relaunch
}

/// Name of the task in the scheduler.
const TASK_NAME: &str = "Dock";
/// The task's name as the change journal writes it (its folder, the root, first).
const TASK_JOURNAL_NAME: &str = "\\Dock";
/// How long an install or an uninstall waits for the machine journal's lock (the running dock's Start entries hold it
/// for a moment).
const JOURNAL_WAIT: Duration = Duration::from_secs(10);
/// Maximum wait for the old instance and its watchdog to release the exe (outlasts the watchdog's
/// `RESTORE_WAIT` for Explorer).
const STOP_WAIT: Duration = Duration::from_secs(35);
/// After the stop signal, retry for at least this long even if nothing of the old instance is seen.
const STOP_GRACE: Duration = Duration::from_secs(3);
/// Once every process running the installed exe has exited, how long a delete is retried while the
/// kernel releases the image.
const RELEASE_GRACE: Duration = Duration::from_secs(2);
/// Pause between two tries of `retry_for`.
const RETRY_STEP: Duration = Duration::from_millis(250);
/// Maximum time for `schtasks.exe` before giving up on it.
const SCHTASKS_TIMEOUT: Duration = Duration::from_secs(20);

/// Name of the install folder under `%ProgramFiles%`.
const INSTALL_DIR_NAME: &str = "mini-oled-dock";

/// `%ProgramFiles%` from `FOLDERID_ProgramFiles` (never hardcoded).
fn program_files() -> Result<PathBuf, String> {
    known_folder(&FOLDERID_ProgramFiles, "ProgramFiles")
}

/// The all-users Start menu Programs folder (`FOLDERID_CommonPrograms`), admin-only to write.
fn common_programs() -> Result<PathBuf, String> {
    known_folder(&FOLDERID_CommonPrograms, "CommonPrograms")
}

/// A known folder's path, resolved by id (never from an environment variable).
fn known_folder(id: &windows::core::GUID, what: &str) -> Result<PathBuf, String> {
    // SAFETY: valid known-folder id, no token (current user); the returned buffer is freed below.
    let p = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) }
        .map_err(|e| format!("SHGetKnownFolderPath({what}): {e}"))?;
    // SAFETY: `p` is the NUL-terminated string returned by SHGetKnownFolderPath, read once.
    let s = unsafe { p.to_string() };
    // SAFETY: same pointer, allocated with CoTaskMemAlloc, not used after this line.
    unsafe { CoTaskMemFree(Some(p.0 as *const std::ffi::c_void)) };
    let s = s.map_err(|e| format!("{what} path: {e}"))?;
    if s.trim().is_empty() {
        return Err(format!("{what} path is empty"));
    }
    Ok(PathBuf::from(s))
}

/// `%ProgramFiles%\mini-oled-dock` (admin-only install folder).
fn install_dir() -> Result<PathBuf, String> {
    Ok(program_files()?.join(INSTALL_DIR_NAME))
}

/// `%ProgramFiles%\mini-oled-dock\dock.exe`, the installed copy run by the logon task.
pub fn installed_exe() -> Result<PathBuf, String> {
    Ok(install_dir()?.join("dock.exe"))
}

/// Legacy install location `%LOCALAPPDATA%\Dock\bin\dock.exe` (medium-writable: migrated away).
fn legacy_exe() -> Option<PathBuf> {
    crate::datadir::legacy_dir().map(|d| d.join("bin").join("dock.exe"))
}

/// true if `path` lies strictly inside `dir` (case-insensitive, separator-aware string check on
/// already-canonical paths; `\\?\` prefixes must match on both sides). Pure.
fn is_under(path: &str, dir: &str) -> bool {
    let dir = dir.trim_end_matches(['\\', '/']).to_lowercase();
    let path = path.to_lowercase();
    !dir.is_empty()
        && path.len() > dir.len() + 1
        && path.starts_with(&dir)
        && path[dir.len()..].starts_with(['\\', '/'])
}

/// With `--autostart`: logs a warning if the running exe is not under `%ProgramFiles%` (the elevated
/// logon task would then be running a file that a non-admin process may be able to replace).
/// No other behaviour change.
pub fn warn_if_not_installed() {
    let (me, pf) = match (std::env::current_exe(), program_files()) {
        (Ok(m), Ok(p)) => (m, p),
        (Err(e), _) => return log!("autostart: WARNING cannot check the exe location: current_exe: {e}"),
        (_, Err(e)) => return log!("autostart: WARNING cannot check the exe location: {e}"),
    };
    let me_c = std::fs::canonicalize(&me).unwrap_or_else(|_| me.clone());
    let pf_c = std::fs::canonicalize(&pf).unwrap_or_else(|_| pf.clone());
    if !is_under(&me_c.to_string_lossy(), &pf_c.to_string_lossy()) {
        log!(
            "autostart: WARNING running elevated at logon from {}, which is not under {}: a non-admin \
             process may be able to replace this file. Run `dock.exe --install` again to move it to Program Files.",
            me.display(),
            pf.display()
        );
    }
}

fn schtasks_path() -> PathBuf {
    let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    root.join("System32").join("schtasks.exe")
}

/// Runs `schtasks.exe` and returns the exit code (with a timeout).
fn run_schtasks(args: &[&std::ffi::OsStr]) -> Result<i32, String> {
    let mut child = std::process::Command::new(schtasks_path())
        .args(args)
        .creation_flags(CREATE_NO_WINDOW.0)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to start schtasks.exe: {e}"))?;
    let t = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(st)) => return Ok(st.code().unwrap_or(-1)),
            Ok(None) => {
                if t.elapsed() > SCHTASKS_TIMEOUT {
                    let _ = child.kill();
                    return Err("schtasks.exe is not responding (timeout)".into());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("waiting for schtasks.exe failed: {e}")),
        }
    }
}

/// true if the "Dock" task exists (query to `schtasks.exe`; any error counts as "does not exist").
pub fn is_enabled() -> bool {
    let name = std::ffi::OsStr::new(TASK_NAME);
    match run_schtasks(&[std::ffi::OsStr::new("/Query"), std::ffi::OsStr::new("/TN"), name]) {
        Ok(code) => code == 0,
        Err(e) => {
            log!("autostart: task query failed: {e}");
            false
        }
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

/// Current user in `DOMAIN\user` format (from the process environment variables).
/// NOTE: this is the account that elevates; with "over-the-shoulder" UAC (standard user typing the
/// credentials of an administrator) the task is registered for the administrator.
fn current_user() -> Result<String, String> {
    let user = std::env::var("USERNAME").map_err(|_| "USERNAME variable missing".to_string())?;
    Ok(match std::env::var("USERDOMAIN") {
        Ok(d) if !d.is_empty() => format!("{d}\\{user}"),
        _ => user,
    })
}

fn task_xml(user: &str, exe: &Path) -> String {
    let user = xml_escape(user);
    let cmd = xml_escape(&exe.to_string_lossy());
    let dir = xml_escape(&exe.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default());
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\r\n\
<Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\r\n\
  <RegistrationInfo><Description>Dock: start at logon with elevated privileges</Description></RegistrationInfo>\r\n\
  <Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId></LogonTrigger></Triggers>\r\n\
  <Principals><Principal id=\"Author\"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>HighestAvailable</RunLevel></Principal></Principals>\r\n\
  <Settings>\r\n\
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>\r\n\
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>\r\n\
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>\r\n\
    <AllowHardTerminate>true</AllowHardTerminate>\r\n\
    <StartWhenAvailable>true</StartWhenAvailable>\r\n\
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>\r\n\
    <AllowStartOnDemand>true</AllowStartOnDemand>\r\n\
    <Enabled>true</Enabled>\r\n\
    <Hidden>false</Hidden>\r\n\
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>\r\n\
    <Priority>4</Priority>\r\n\
  </Settings>\r\n\
  <Actions Context=\"Author\"><Exec><Command>{cmd}</Command><Arguments>--autostart</Arguments><WorkingDirectory>{dir}</WorkingDirectory></Exec></Actions>\r\n\
</Task>\r\n"
    )
}

/// Registers the task (replacing an existing one) that starts `exe --autostart` at logon.
fn register_task(exe: &Path) -> Result<(), String> {
    let user = current_user()?;
    // Staged in the admin-only install folder: see the SECURITY note in the module docs.
    let dir = install_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let xml_path = dir.join("autostart-task.xml");
    // schtasks reads the XML as UTF-16 LE with BOM.
    let mut bytes: Vec<u8> = vec![0xFF, 0xFE];
    for u in task_xml(&user, exe).encode_utf16() {
        bytes.extend_from_slice(&u.to_le_bytes());
    }
    std::fs::write(&xml_path, &bytes).map_err(|e| format!("writing {}: {e}", xml_path.display()))?;
    let r = run_schtasks(&[
        std::ffi::OsStr::new("/Create"),
        std::ffi::OsStr::new("/TN"),
        std::ffi::OsStr::new(TASK_NAME),
        std::ffi::OsStr::new("/XML"),
        xml_path.as_os_str(),
        std::ffi::OsStr::new("/F"),
    ]);
    let _ = std::fs::remove_file(&xml_path);
    match r? {
        0 => Ok(()),
        c => Err(format!("schtasks /Create returned {c} (administrator privileges are required)")),
    }
}

pub(crate) use oled_sys::ident::same_file;

/// Whether `copy_self` keeps retrying the copy, `elapsed` after the stop signal. Pure.
/// Past `STOP_GRACE`, with neither the instance nor any process running the destination exe left,
/// something else holds the file: stop waiting and report.
fn keep_waiting(elapsed: Duration, instance_up: bool, exe_in_use: bool) -> bool {
    if elapsed >= STOP_WAIT {
        false
    } else if elapsed < STOP_GRACE {
        true
    } else {
        instance_up || exe_in_use
    }
}

/// true if a process other than this one runs the image file `dest` (the old dock or its
/// `--watchdog` child, of any version: none needs to cooperate or create a named object).
/// Known limit: a process whose image path cannot be queried (a protected process) is invisible
/// here; such a holder is not a dock process, and the copy then fails after `STOP_GRACE` with the
/// "locked by something else" error.
pub(crate) fn exe_in_use_by_other(dest: &Path) -> bool {
    let mut pids = vec![0u32; 1024];
    let mut bytes = 0u32;
    loop {
        let cap = (pids.len() * std::mem::size_of::<u32>()) as u32;
        // SAFETY: `pids` has `cap` writable bytes; `bytes` receives the number of bytes written.
        if unsafe { EnumProcesses(pids.as_mut_ptr(), cap, &mut bytes) }.is_err() {
            log!("autostart: EnumProcesses failed");
            return false;
        }
        if bytes < cap {
            break;
        }
        pids.resize(pids.len() * 2, 0); // buffer full: the list may be truncated, grow and retry
    }
    pids.truncate(bytes as usize / std::mem::size_of::<u32>());
    // SAFETY: no preconditions.
    let me = unsafe { GetCurrentProcessId() };
    pids.into_iter()
        .filter(|&pid| pid != me)
        .filter_map(crate::apps::exe_path_of_pid)
        .any(|path| same_file(Path::new(&path), dest))
}

/// `<dir>\dock.exe.new`: the new exe is copied here, then renamed over `dest`. Pure.
fn staging_path(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_os_string();
    s.push(".new");
    PathBuf::from(s)
}

/// The replaced exe an upgrade keeps next to `dock.exe` (`copy_self`), for `--rollback` or a manual
/// `dock.prev.exe --install`.
pub(crate) const PREV_NAME: &str = "dock.prev.exe";

/// `<dir>\dock.prev.exe` for `dest` = `<dir>\dock.exe`. Pure.
pub(crate) fn prev_path(dest: &Path) -> PathBuf {
    dest.with_file_name(PREV_NAME)
}

/// `<dir>\dock.prev.exe.new`: the replaced exe is copied here, then renamed over `prev_path`. Pure.
fn prev_staging_path(dest: &Path) -> PathBuf {
    staging_path(&prev_path(dest))
}

/// true only for an upgrade (`installed` known and older): a reinstall, a downgrade (a rollback) or an
/// unreadable installed version leaves `dock.prev.exe` as it is. Pure.
pub(crate) fn keeps_prev(installed: Option<Version>, installing: Version) -> bool {
    installed.is_some_and(|v| v < installing)
}

/// The version `exe --version` prints (contract: `mini-oled-dock X.Y.Z`, nothing touched). `None` when the
/// file is missing; a failed run or another answer is logged and is `None`. A wedged exe costs up to the
/// 5 s timeout and is killed by `run_capture`.
pub(crate) fn exe_version(exe: &Path) -> Option<Version> {
    if !exe.is_file() {
        return None;
    }
    let max = crate::update::CAPTURE_MAX_BYTES;
    match crate::update::run_capture(exe, &["--version"], Duration::from_secs(5), max) {
        Ok((0, out)) => {
            let v = parse_version_output(&out);
            if v.is_none() {
                log!("autostart: {} --version printed no version", exe.display());
            }
            v
        }
        Ok((code, _)) => {
            log!("autostart: {} --version exited with {code}", exe.display());
            None
        }
        Err(e) => {
            log!("autostart: {} --version failed: {e}", exe.display());
            None
        }
    }
}

/// Renames `src` over `dest` in one step (`MoveFileExW`, same volume). `dest` is never written in
/// place: a running image (opened without delete sharing) makes the rename fail and stays intact.
fn replace_file(src: &Path, dest: &Path) -> std::io::Result<()> {
    let (s, d) = (HSTRING::from(src.as_os_str()), HSTRING::from(dest.as_os_str()));
    // SAFETY: both strings are NUL-terminated and live for the call.
    unsafe { MoveFileExW(&s, &d, MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) }
        .map_err(|e| std::io::Error::from_raw_os_error(e.code().0 & 0xFFFF))
}

/// Retry loop of `copy_self`, with the replace (sleep included), the `(instance_up, exe_in_use)` check
/// and the clock passed in. `Err((old_alive, last copy error))` when it gives up.
fn retry_copy(
    mut copy: impl FnMut() -> std::io::Result<()>,
    mut check: impl FnMut() -> (bool, bool),
    elapsed: impl Fn() -> Duration,
) -> Result<(), (bool, std::io::Error)> {
    loop {
        let (up, in_use) = check();
        let stop = !keep_waiting(elapsed(), up, in_use);
        // Always copy after the check: the holder may have left while it ran.
        match copy() {
            Ok(()) => return Ok(()),
            Err(e) if stop => return Err((up || in_use, e)),
            Err(_) => {}
        }
    }
}

/// `std::fs::copy` (CopyFileExW) also copies alternate data streams, so a browser-downloaded exe would keep
/// its `Zone.Identifier` (Mark of the Web) in the install folder, and the agent started through Explorer
/// would then be zone-checked on every start. The install is an explicit, elevated act, so the mark is dropped
/// from the copy (only the stream is removed, never the file). NotFound means there was no mark.
fn copy_without_zone_mark(src: &Path, dst: &Path) -> std::io::Result<u64> {
    let n = std::fs::copy(src, dst)?;
    let mut stream = dst.as_os_str().to_os_string();
    stream.push(":Zone.Identifier");
    match std::fs::remove_file(PathBuf::from(stream)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            log!("autostart: could not clear the download mark of {}: {e}", dst.display());
        }
        _ => {}
    }
    Ok(n)
}

/// Copies the current exe to `dest` through `dock.exe.new` and a rename (`dest` is never written in
/// place). If the rename is refused and `allow_stop` is set, signals the running instance to exit
/// (`signal_stop_to_instance`; with no instance, e.g. after a crash, its watchdog may still hold the
/// exe) and retries while `keep_waiting` says so: up to `STOP_WAIT` (35 s) while the old instance or
/// its watchdog still runs the exe. Never relaunches: `run_install_cli` decides that.
///
/// An upgrade also keeps the replaced exe as `dock.prev.exe` (`keeps_prev`), renamed into place only after
/// the new `dock.exe` is, so `dock.exe` is a complete image at every instant. To know the installed
/// version it runs `<dest> --version` first (also in the update guard's rollback, where `dest` is the image
/// of the dock it just ended): a wedged exe costs up to 5 s, nothing else.
///
/// `want` (SEC-4): the SHA-256 of the running image (`running_image_digest`), which the copy must have. It is read back
/// from `dock.exe.new`, in the install folder where only administrators write, before it replaces anything: the running
/// image's folder may be one its user can change, and a running exe can be renamed and another file put at the path
/// `current_exe` names.
fn copy_self(dest: &Path, allow_stop: bool, want: Option<Digest>) -> InstallReport {
    let fail = |m: String| InstallReport::failed(InstallError::Failed(m));
    let me = match std::env::current_exe() {
        Ok(m) => m,
        Err(e) => return fail(format!("current_exe: {e}")),
    };
    if same_file(&me, dest) {
        // Already running from the installed copy.
        return InstallReport { copy: CopyOutcome::AlreadyInstalled, stop_signalled: false, error: None };
    }
    if let Some(d) = dest.parent() {
        if let Err(e) = std::fs::create_dir_all(d) {
            return fail(format!("creating {}: {e}", d.display()));
        }
    }
    let new = staging_path(dest);
    match std::fs::remove_file(&new) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return fail(format!("removing the stale {}: {e}", new.display()));
        }
        _ => {}
    }
    let installed = exe_version(dest);
    let prev_new = prev_staging_path(dest);
    let _ = std::fs::remove_file(&prev_new);
    let mut keep = keeps_prev(installed, Version::current());
    if keep {
        if let Err(e) = copy_without_zone_mark(dest, &prev_new) {
            log!("autostart: previous version not kept (copy failed: {e})");
            let _ = std::fs::remove_file(&prev_new);
            keep = false;
        }
    }
    if let Err(e) = copy_without_zone_mark(&me, &new) {
        let _ = std::fs::remove_file(&new);
        let _ = std::fs::remove_file(&prev_new);
        return fail(format!("copy to {} failed: {e}", new.display()));
    }
    if let Some(e) = want.and_then(|w| copy_matches(&new, w).err()) {
        let _ = std::fs::remove_file(&new);
        let _ = std::fs::remove_file(&prev_new);
        return fail(e);
    }
    let copied = |stop_signalled| {
        if let (true, Some(v)) = (keep, installed) {
            match replace_file(&prev_new, &prev_path(dest)) {
                Ok(()) => log!("autostart: previous version {v} kept as {PREV_NAME}"),
                Err(e) => {
                    log!("autostart: previous version not kept ({e})");
                    let _ = std::fs::remove_file(&prev_new);
                }
            }
        }
        InstallReport { copy: CopyOutcome::Copied, stop_signalled, error: None }
    };
    let first = match replace_file(&new, dest) {
        Ok(()) => return copied(false),
        Err(e) => e,
    };
    if !allow_stop {
        let _ = std::fs::remove_file(&new);
        let _ = std::fs::remove_file(&prev_new);
        return fail(format!("replacing {} failed: {first}", dest.display()));
    }
    log!("autostart: replace failed ({first}), asking the running instance to exit");
    let signalled = crate::watchdog::signal_stop_to_instance();
    if !signalled {
        log!("autostart: no running instance answered the stop event, waiting for its watchdog if any");
    }
    let t = Instant::now();
    let replace = || {
        std::thread::sleep(Duration::from_millis(250));
        replace_file(&new, dest)
    };
    let check = || (crate::watchdog::instance_running(), exe_in_use_by_other(dest));
    let Err((old_alive, last)) = retry_copy(replace, check, || t.elapsed()) else {
        log!("autostart: {} replaced after {} ms", dest.display(), t.elapsed().as_millis());
        return copied(signalled);
    };
    let _ = std::fs::remove_file(&new);
    let _ = std::fs::remove_file(&prev_new);
    let error = if old_alive {
        InstallError::CouldNotStop(format!(
            "replacing {} failed: the old instance or its watchdog did not exit within {} s: {last}",
            dest.display(),
            STOP_WAIT.as_secs()
        ))
    } else {
        InstallError::Failed(format!(
            "replacing {} failed: the file is locked by something else: {last}",
            dest.display()
        ))
    };
    InstallReport { copy: CopyOutcome::Failed, stop_signalled: signalled, error: Some(error) }
}

/// Ok when the file at `copy` is `want` (SHA-256), read through a handle that follows no link and refuses a second
/// hard link: `copy_self`'s check of the copied dock.exe (SEC-4 (1)).
fn copy_matches(copy: &Path, want: Digest) -> Result<(), String> {
    let bytes = oled_sys::datadir::read_file(copy, COMPONENT_MAX_BYTES, true)
        .map_err(|e| format!("the copy of dock.exe could not be read back ({e})"))?;
    if oled_sys::sha256::sha256(&bytes)? != want {
        return Err("the copy of dock.exe is not the running dock.exe (SHA-256): nothing replaced".into());
    }
    Ok(())
}

/// Installs the exe copy and registers the task. `allow_stop`: may stop an instance that locks
/// the destination (only from `--install`; the popup toggle must not close itself).
/// Then writes the Add/Remove Programs entry (`arp::write`) and the Start entries of the components already in the
/// folder. Everything goes through `RealInstallOs` (`enable_with`), so the machine journal records the task as it was
/// and the ARP key it creates before either is touched. The copy must be the running image: its digest (`running_image_digest`) is read
/// before anything else, and without it nothing is changed.
/// A task or ARP error after a good copy keeps `copy`, so the new exe is still relaunched.
pub(crate) fn enable(allow_stop: bool) -> InstallReport {
    match running_image_digest() {
        Ok(own) => enable_with(&mut RealInstallOs { want: Some(own) }, allow_stop),
        Err(e) => {
            log!("autostart: the install is refused, nothing changed: {e}");
            InstallReport::failed(InstallError::Failed(e))
        }
    }
}

/// The machine side of an install (I1c): the machine journal, the copy, the task, the ARP entry, the leftovers and the
/// Start entries. `RealInstallOs` acts on %ProgramFiles%, the "Dock" task, HKLM and the all-users Programs folder;
/// tests use a fake, never it (`no_test_calls_the_live_installer`).
pub(crate) trait InstallOs {
    /// Appends `changes` to the machine journal as one Install run, each entry flushed, before anything is changed.
    fn record(&mut self, changes: &[Change]) -> Result<(), String>;
    fn copy_self(&mut self, dest: &Path, allow_stop: bool) -> InstallReport;
    fn register_task(&mut self, exe: &Path) -> Result<(), String>;
    /// SHA-256 of the task XML `register_task` writes (autostart-task.xml's text).
    fn task_xml_digest(&self) -> Result<Digest, String>;
    fn arp_write(&mut self, dir: &Path) -> Result<(), String>;
    /// The machine journal's open Install entries (none when there is no journal or it cannot be read).
    fn open_install_entries(&self) -> Vec<Entry>;
    /// What is there before anything is changed: whether the ARP key exists and the logon task's XML, read as the
    /// journal's undo reads them. Err when either cannot be read.
    fn before(&self) -> Result<InstallBefore, String>;
    /// The `*.old-<pid>` leftovers of an uninstall not yet followed by a restart.
    fn remove_old_copies(&mut self, dir: &Path);
    /// The Start entries of the components already in `dir` (I1b; journaled there).
    fn sync_entry_points(&mut self, dir: &Path) -> Result<u32, String>;
}

/// What an install finds before it changes anything (`InstallOs::before`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InstallBefore {
    /// The ARP key exists.
    pub arp_key: bool,
    /// The logon task's XML as Task Scheduler gives it back; None: there is no such task.
    pub task: Option<String>,
}

/// What an install journals before it changes anything (I1c), from the machine journal's open Install entries, what
/// the install finds (`before`) and the digest of the task XML it registers. Pure. What existed before is never
/// journaled as created, so an undo of the Install class puts back what was there or leaves it alone.
/// - The ARP key, with no open entry for it: RegKeyCreated when it is not there (this install creates it), nothing when
///   it is (journal::apply's rule: an existing key is not ours to delete; the uninstall's own removal deletes it).
/// - The task, when the newest open entry for it has `task_after`: nothing, so a reinstall does not grow the journal.
///   Otherwise a Task entry whose before is the task as found (none, or its XML): an undo puts that back. Never an
///   older entry's before: 1.8.0 wrote "no task" and "key created" without looking, over an install that had both.
///
/// Those 1.8.0 entries are kept as they are: no record tells them from a true one, and only the uninstall, which then
/// deletes the task and the key anyway, undoes the Install class (`only_the_uninstall_undoes_the_install`).
pub(crate) fn install_journal_plan(open_install: &[Entry], before: &InstallBefore, task_after: Digest) -> Vec<Change> {
    let ours = open_install.iter().filter(|e| e.kind == Kind::Install);
    let mut arp_open = false;
    // (seq, after) of the newest open Task entry for the logon task.
    let mut task_open: Option<(u64, Digest)> = None;
    for e in ours {
        match &e.change {
            Change::RegKeyCreated { hive: Hive::Hklm, key }
                if key.as_str().eq_ignore_ascii_case(crate::arp::ARP_KEY) =>
            {
                arp_open = true;
            }
            Change::Task { name, after, .. } if name.as_str().eq_ignore_ascii_case(TASK_JOURNAL_NAME) => {
                if task_open.is_none_or(|(seq, _)| e.seq > seq) {
                    task_open = Some((e.seq, *after));
                }
            }
            _ => {}
        }
    }
    let mut plan = Vec::new();
    if !arp_open && !before.arp_key {
        if let Ok(key) = RegPath::new(crate::arp::ARP_KEY) {
            plan.push(Change::RegKeyCreated { hive: Hive::Hklm, key });
        }
    }
    if task_open.is_some_and(|(_, after)| after == task_after) {
        return plan;
    }
    if let Ok(name) = TaskName::new(TASK_JOURNAL_NAME) {
        plan.push(Change::Task { name, before_xml: before.task.clone(), after: task_after });
    }
    plan
}

/// The install path (`enable`, `--install`, `--install --from`): what it replaces read and the journal plan recorded
/// first (a failed read or record changes nothing, as the journal's own planner refuses), then the copy, the leftovers,
/// the task, the ARP entry and the Start entries, as `enable` did before 1.8.0. A task or ARP error after a good copy
/// keeps `copy` (the first error wins); the Start entries only log theirs (an update's install must not fail for a
/// shortcut; the running dock brings them in line later).
pub(crate) fn enable_with(os: &mut dyn InstallOs, allow_stop: bool) -> InstallReport {
    let dir = match install_dir() {
        Ok(d) => d,
        Err(e) => return InstallReport::failed(InstallError::Failed(e)),
    };
    let dest = dir.join("dock.exe");
    let plan = match (os.task_xml_digest(), os.before()) {
        (Ok(task), Ok(before)) => install_journal_plan(&os.open_install_entries(), &before, task),
        (Err(e), _) => return InstallReport::failed(InstallError::Failed(format!("the logon task's XML: {e}"))),
        (_, Err(e)) => {
            log!("autostart: the install is refused, nothing changed: {e}");
            return InstallReport::failed(InstallError::Failed(format!("what the install replaces: {e}")));
        }
    };
    if let Err(e) = os.record(&plan) {
        log!("autostart: the change journal refused the install ({e}): nothing changed");
        return InstallReport::failed(InstallError::Failed(format!("the change journal: {e}")));
    }
    let mut report = os.copy_self(&dest, allow_stop);
    if report.copy == CopyOutcome::Failed {
        return report;
    }
    os.remove_old_copies(&dir);
    match os.register_task(&dest) {
        Ok(()) => log!("autostart: task '{TASK_NAME}' registered -> {}", dest.display()),
        Err(e) => report.error = Some(InstallError::Failed(e)),
    }
    match os.arp_write(&dir) {
        Ok(()) => log!("autostart: Add/Remove Programs entry written"),
        Err(e) => {
            log!("autostart: Add/Remove Programs entry failed: {e}");
            // The first error wins (a task failure is reported before this one).
            report.error.get_or_insert(InstallError::Failed(format!("Add/Remove Programs entry: {e}")));
        }
    }
    if let Err(e) = os.sync_entry_points(&dir) {
        log!("autostart: Start entries not in line ({e})");
    }
    report
}

/// The machine. Blocking (child processes, COM, file copies): never from the UI thread.
pub(crate) struct RealInstallOs {
    /// The SHA-256 the copy of the running dock.exe must have (`copy_self`): the running image's
    /// (`running_image_digest`).
    pub want: Option<Digest>,
}

impl InstallOs for RealInstallOs {
    fn record(&mut self, changes: &[Change]) -> Result<(), String> {
        if changes.is_empty() {
            return Ok(());
        }
        let (mut j, mut sink) = journal_os::open_locked(Scope::Machine, JOURNAL_WAIT).map_err(|e| e.what)?;
        let run = j.begin_run();
        let now = now_unix();
        for c in changes {
            journal::record(&mut j, &mut sink, run, Kind::Install, c.clone(), now).map_err(|e| e.what)?;
        }
        log!("autostart: {} install change(s) journaled", changes.len());
        Ok(())
    }

    fn copy_self(&mut self, dest: &Path, allow_stop: bool) -> InstallReport {
        copy_self(dest, allow_stop, self.want)
    }

    fn register_task(&mut self, exe: &Path) -> Result<(), String> {
        register_task(exe)
    }

    fn task_xml_digest(&self) -> Result<Digest, String> {
        oled_sys::sha256::sha256(task_xml(&current_user()?, &installed_exe()?).as_bytes())
    }

    fn arp_write(&mut self, dir: &Path) -> Result<(), String> {
        crate::arp::write(dir)
    }

    fn open_install_entries(&self) -> Vec<Entry> {
        if !journal_present(journal_os::path(Scope::Machine).as_deref()) {
            return Vec::new();
        }
        match journal_os::load(Scope::Machine) {
            Ok(j) => j.open_entries().into_iter().filter(|e| e.kind == Kind::Install).cloned().collect(),
            Err(e) => {
                log!("autostart: the machine journal could not be read ({e})");
                Vec::new()
            }
        }
    }

    fn before(&self) -> Result<InstallBefore, String> {
        use journal::JournalOs as _;
        let os = RealOs::for_scope(Scope::Machine);
        let key = RegPath::new(crate::arp::ARP_KEY).map_err(|_| "the ARP key's path".to_string())?;
        let name = TaskName::new(TASK_JOURNAL_NAME).map_err(|_| "the logon task's name".to_string())?;
        let arp_key = os.reg_key_exists(Hive::Hklm, &key).map_err(|e| format!("the ARP key: {}", e.what))?;
        let task = os.task_xml(&name).map_err(|e| format!("the logon task: {}", e.what))?;
        Ok(InstallBefore { arp_key, task })
    }

    fn remove_old_copies(&mut self, dir: &Path) {
        remove_old_copies(dir);
    }

    fn sync_entry_points(&mut self, dir: &Path) -> Result<u32, String> {
        crate::entry_points::sync(&entry_config(), dir)
    }
}

/// The config the Start entries of an install follow (their names' language and the two switches): config.json read as
/// the dock reads it, without writing anything (an install creates no config); the defaults when there is none or it
/// cannot be used.
fn entry_config() -> crate::config::Config {
    let text = crate::datadir::config_path()
        .and_then(|p| oled_sys::datadir::read_file(&p, crate::datadir::CONFIG_MAX_BYTES, false).ok())
        .and_then(|b| String::from_utf8(b).ok());
    match text.map(|t| oled_config::parse_text::<crate::config::Config>(&t, None)) {
        Some(Ok((c, _))) => c,
        _ => (*crate::config::get()).clone(),
    }
}

/// True when a journal file is at `path`.
fn journal_present(path: Option<&Path>) -> bool {
    path.is_some_and(|p| std::fs::symlink_metadata(p).is_ok())
}

fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Tells the user about a leftover legacy copy `%LOCALAPPDATA%\Dock\bin\dock.exe`. It is never deleted
/// here: this process is elevated and the folder is medium-writable, so a planted junction or symlink
/// could turn the delete into an arbitrary-file delete. Deleting it as the normal user is safe.
fn report_legacy_copy() {
    let Some(old) = legacy_exe() else { return };
    // symlink_metadata does not follow reparse points: this only reports, nothing is opened or changed.
    if std::fs::symlink_metadata(&old).is_ok() {
        log!("autostart: legacy copy {} still present (not deleted by the elevated installer)", old.display());
        if let Some(bin) = old.parent() {
            crate::logger::say(&format!(
                "Note: an old copy is left in {}. Delete that folder yourself (no admin rights needed).",
                bin.display()
            ));
        }
    }
}

/// Removes the task (the installed files stay). If it does not exist it is not an error.
pub fn disable() -> Result<(), String> {
    if !is_enabled() {
        return Ok(());
    }
    let r = run_schtasks(&[
        std::ffi::OsStr::new("/Delete"),
        std::ffi::OsStr::new("/TN"),
        std::ffi::OsStr::new(TASK_NAME),
        std::ffi::OsStr::new("/F"),
    ])?;
    if r == 0 {
        log!("autostart: task '{TASK_NAME}' removed");
        Ok(())
    } else {
        Err(format!("schtasks /Delete returned {r} (administrator privileges are required)"))
    }
}

/// Launches the installed copy (after `--install` has stopped the previous instance).
fn relaunch(exe: &Path) {
    // Null stdio: the long-lived dock must not inherit the pipes/console of the `--install` process.
    match std::process::Command::new(exe)
        .creation_flags(CREATE_NO_WINDOW.0)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(_) => crate::logger::say("Dock started from the installed copy."),
        Err(e) => crate::logger::say(&format!("Unable to start the dock: {e}")),
    }
}

/// `--install --from <folder>` (I1d): installs the running dock as `--install` does, then stages the suite's other
/// components from `<folder>` (a release's assets, unpacked) into `update\<own tag>\`, each verified there, so the
/// started dock's `--suite-sync` installs them from the stage, offline. The folder's suite.json must be the one this
/// release publishes (its SHA-256 against the release's, so the install needs the network) and name this very
/// dock.exe, or nothing is done (SEC-4).
pub fn run_install_from_cli(no_relaunch: bool, from: &Path) -> i32 {
    install_cli(no_relaunch, Some(from))
}

/// `--install`: returns the exit code. Always starts the installed exe at the end (the new copy, or
/// the old one after a failed copy), unless `no_relaunch` or a dock is still running.
pub fn run_install_cli(no_relaunch: bool) -> i32 {
    install_cli(no_relaunch, None)
}

/// `--install` and `--install --from`: the install, the components staged from the folder, the relaunch.
fn install_cli(no_relaunch: bool, from: Option<&Path>) -> i32 {
    let say = crate::logger::say;
    let dest = match installed_exe() {
        Ok(d) => d,
        Err(e) => {
            log!("autostart: --install failed: {e}");
            say(&format!("Installation failed: {e}"));
            return EXIT_FAILED;
        }
    };
    let (report, staged) = match from {
        None => (enable(true), None),
        Some(folder) => {
            let version = Version::current();
            let mut stage = DataFromStage { tag: version.tag() };
            let done = running_image_digest().and_then(|own| {
                let mut os = RealInstallOs { want: Some(own) };
                install_from_with(&mut os, &mut stage, &mut RealFromTrust, folder, own, version, true)
            });
            match done {
                Ok((r, s)) => (r, Some(s)),
                Err(e) => {
                    log!("autostart: --install --from refused: {e}");
                    say(&format!("Installation from the folder refused, nothing changed: {e}"));
                    return EXIT_FAILED;
                }
            }
        }
    };
    // Sampled after `enable`: true only when re-run over its own running dock or the stop gave up.
    let still_running = crate::watchdog::instance_running();
    log!(
        "autostart: --install copy={:?} stop_signalled={} still_running={still_running} no_relaunch={no_relaunch}",
        report.copy,
        report.stop_signalled
    );
    match &report.error {
        None => {
            say(&format!("Installed to {}", dest.display()));
            say("Automatic start at logon enabled (scheduled task 'Dock', elevated).");
            report_legacy_copy();
        }
        Some(e) => {
            log!("autostart: --install failed: {e}");
            say(&format!("Installation failed: {e}"));
        }
    }
    let mut code = report.exit_code();
    if let Some(s) = &staged {
        let ids = |v: &[ComponentId]| v.iter().map(|c| c.id()).collect::<Vec<_>>().join(", ");
        log!("autostart: --install --from staged {}, refused {}", s.staged.len(), s.refused.len());
        if !s.staged.is_empty() {
            say(&format!("Staged: {} (the dock installs them within a minute of its start).", ids(&s.staged)));
        }
        for (id, why) in &s.refused {
            say(&format!("Not staged: {} ({why}).", id.id()));
        }
        if !s.refused.is_empty() && code == EXIT_OK {
            code = EXIT_FAILED;
        }
    }
    if should_relaunch(still_running, no_relaunch) && dest.exists() {
        relaunch(&dest);
    }
    code
}

/// The stage `--install --from` fills: `<root>\update\<own tag>\` (tests: a temp folder).
pub(crate) trait FromStage {
    /// Copies `src` in as `name` (the kernel copies it: the file never sits in this process's memory).
    fn copy_in(&mut self, name: &str, src: &Path) -> Result<(), String>;
    fn write(&mut self, name: &str, bytes: &[u8]) -> Result<(), String>;
    /// The size and SHA-256 of the staged `name`, read back from the stage.
    fn staged(&mut self, name: &str) -> Result<(u64, Digest), String>;
    fn remove(&mut self, name: &str);
}

/// What `--install --from` staged, and what it did not (with why).
#[derive(Debug, Default)]
pub(crate) struct FromReport {
    pub staged: Vec<ComponentId>,
    pub refused: Vec<(ComponentId, String)>,
}

/// Where `--install --from` gets the trust its folder cannot give (SEC-4 (2)): whoever can write the folder can write a
/// suite.json that names this dock.exe. Tests use a fake: the real one reaches the network.
pub(crate) trait FromTrust {
    /// The SHA-256 of the suite.json the release of `tag` publishes, under the updater's release rules; Err when it
    /// cannot be had (offline, no such release, no suite.json in it).
    fn release_manifest(&mut self, tag: &str) -> Result<Digest, String>;
}

/// The release on the network.
struct RealFromTrust;

impl FromTrust for RealFromTrust {
    fn release_manifest(&mut self, tag: &str) -> Result<Digest, String> {
        release_manifest_digest(tag)
    }
}

/// The SHA-256 of the suite.json release `tag` publishes, as the sync takes a release's manifest (U1e): the release by
/// tag (allowed host, 200, the release of that tag), then GitHub's digest of its suite.json asset or, when GitHub sent
/// none, the asset itself, downloaded from an allowed host at its announced size. The GET runs in this process, never
/// in an `--update-fetch` child: that child would be the exe at current_exe's path, in a folder its user may change,
/// and a junction there can turn that path to another file between its check and the start. A one-shot install keeps
/// no handle budget the child would spare (M1 F2).
fn release_manifest_digest(tag: &str) -> Result<Digest, String> {
    let rel = crate::update::fetch_release(Some(tag), false).map_err(|e| e.to_string())?;
    let asset = rel.suite.ok_or("the release has no suite.json")?;
    match asset.digest {
        Some(d) => Ok(d),
        None => oled_sys::sha256::sha256(&crate::update::get_asset(&asset, SUITE_MAX_BYTES)?),
    }
}

/// `<from>\suite.json`, read once and authenticated (SEC-4 (2)): its SHA-256 must be that of the suite.json the
/// release of `version` publishes. When that cannot be had (offline) the install is refused before the folder is read:
/// no folder's rights stand in for the release. Those of a volume this machine formatted let every account add folders
/// at its root, and a share or a disk prepared elsewhere reports the owner and DACL its maker wrote, admin-only ones
/// included. A release digest that differs is a refusal too.
fn trusted_manifest(from: &Path, version: Version, trust: &mut dyn FromTrust) -> Result<Vec<u8>, String> {
    let tag = version.tag();
    let want = trust.release_manifest(&tag).map_err(|why| {
        format!(
            "{SUITE_ASSET} could not be checked against release {tag} ({why}): connect to the internet and try again"
        )
    })?;
    let bytes = oled_sys::datadir::read_file(&from.join(SUITE_ASSET), SUITE_MAX_BYTES, false)
        .map_err(|e| format!("{SUITE_ASSET}: {e}"))?;
    if oled_sys::sha256::sha256(&bytes)? != want {
        return Err(format!("{SUITE_ASSET} is not the one release {tag} publishes (SHA-256)"));
    }
    log!("autostart: --install --from: {SUITE_ASSET} is the one release {tag} publishes");
    Ok(bytes)
}

/// `<from>\suite.json`, authenticated (`trusted_manifest`), through the manifest's decoder: it must be of `version`
/// and name the running image (`own`) as its dock, or the whole `--from` is refused. The manifest and its bytes.
pub(crate) fn folder_manifest(
    from: &Path,
    own: Digest,
    version: Version,
    trust: &mut dyn FromTrust,
) -> Result<(SuiteManifest, Vec<u8>), String> {
    let bytes = trusted_manifest(from, version, trust)?;
    let m = parse_manifest(&bytes).map_err(|e| format!("{SUITE_ASSET}: {e}"))?;
    if m.version != version {
        return Err(format!("{SUITE_ASSET} is of {}, this dock is {version}", m.version));
    }
    if !m.components.iter().any(|c| c.id == ComponentId::Dock && c.sha256 == own) {
        return Err(format!("{SUITE_ASSET} is for another dock.exe than the one running"));
    }
    Ok((m, bytes))
}

/// Copies every component of `m` but the dock from `from` (by its asset name) into the stage and verifies the copy
/// there, size then SHA-256 against `m`: a copy that is not the release's file is deleted from the stage and reported.
/// The manifest goes in last, as the very bytes `folder_manifest` checked.
pub(crate) fn stage_from_folder(
    m: &SuiteManifest,
    manifest: &[u8],
    from: &Path,
    stage: &mut dyn FromStage,
) -> FromReport {
    let mut r = FromReport::default();
    for c in m.components.iter().filter(|c| c.id != ComponentId::Dock) {
        let name = c.id.asset();
        let src = from.join(name);
        // A plain file of the announced size; a link or anything else is not copied at all.
        let fits = std::fs::symlink_metadata(&src).is_ok_and(|md| md.is_file() && md.len() == c.size);
        let step = if !fits {
            Err("not in the folder at the size suite.json gives".to_string())
        } else if stage.copy_in(name, &src).is_err() {
            Err("it could not be copied into the stage".to_string())
        } else {
            match stage.staged(name) {
                Ok((size, hash)) if size == c.size && hash == c.sha256 => Ok(()),
                other => {
                    stage.remove(name);
                    Err(match other {
                        Ok(_) => "not the file suite.json names (SHA-256)".to_string(),
                        Err(e) => e,
                    })
                }
            }
        };
        match step {
            Ok(()) => r.staged.push(c.id),
            Err(why) => r.refused.push((c.id, why)),
        }
    }
    if let Err(e) = stage.write(SUITE_ASSET, manifest) {
        // Without its manifest the stage is never read: nothing counts as staged.
        r.refused.extend(r.staged.drain(..).map(|id| (id, format!("{SUITE_ASSET}: {e}"))));
    }
    r
}

/// `--install --from`'s path: the folder's manifest authenticated and checked first (a refusal changes nothing), then
/// the install (`enable_with`, so the journal plan is recorded before the copy), then, after a good copy, the
/// components staged.
pub(crate) fn install_from_with(
    os: &mut dyn InstallOs,
    stage: &mut dyn FromStage,
    trust: &mut dyn FromTrust,
    from: &Path,
    own: Digest,
    version: Version,
    allow_stop: bool,
) -> Result<(InstallReport, FromReport), String> {
    let (m, bytes) = folder_manifest(from, own, version, trust)?;
    let report = enable_with(os, allow_stop);
    if report.copy == CopyOutcome::Failed {
        return Ok((report, FromReport::default()));
    }
    let staged = stage_from_folder(&m, &bytes, from, stage);
    Ok((report, staged))
}

/// SHA-256 of the running image, the dock.exe an install copies (`running_image`; a link at its path is refused): a
/// file put at the path `current_exe` names after the start is refused, never hashed, so no install copies it
/// (`copy_self` checks its copy against this).
fn running_image_digest() -> Result<Digest, String> {
    let image = running_image(|me| oled_sys::datadir::open_checked_read_only(me, false))?;
    oled_sys::sha256::sha256(image.bytes())
}

/// The running image the dock starts its own watchdog from (`watchdog::launch_watchdog`): `running_image`, every link
/// at its path followed as a start follows them.
pub(crate) fn start_image() -> Result<RunningImage, String> {
    running_image(|me| std::fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ.0).open(me))
}

/// The file at current_exe's path, opened by `open` shared for reading only (while it is held nobody can write, rename
/// or delete it, nor rename a folder above it) and mapped read-only, when it is the file this process's image was
/// mapped from (`NtAreMappedFilesTheSame`). A running exe can be renamed and another file put at the path
/// `current_exe` still names, by whoever can change its folder (a download folder: every unelevated process of its
/// user): that file is refused.
fn running_image(open: impl FnOnce(&Path) -> std::io::Result<std::fs::File>) -> Result<RunningImage, String> {
    let me = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let read = |e: std::io::Error| format!("reading the running dock.exe: {e}");
    let file = open(&me).map_err(read)?;
    let size = file.metadata().map_err(read)?.len();
    if size == 0 || size > COMPONENT_MAX_BYTES {
        return Err(format!("the running dock.exe is {size} bytes"));
    }
    let len = usize::try_from(size).map_err(|e| e.to_string())?;
    // SAFETY: the handle of `file`, open for reading for the whole call; no security attributes, no name.
    let map = unsafe { CreateFileMappingW(HANDLE(file.as_raw_handle()), None, PAGE_READONLY, 0, 0, PCWSTR::null()) }
        .map_err(|e| format!("mapping the running dock.exe: {e}"))?;
    // SAFETY: a fresh handle, owned by nothing else.
    let map = unsafe { OwnedHandle::from_raw_handle(map.0) };
    // SAFETY: the mapping made above; the whole file, read-only.
    let view = unsafe { MapViewOfFile(HANDLE(map.as_raw_handle()), FILE_MAP_READ, 0, 0, 0) };
    if view.Value.is_null() {
        return Err(format!("mapping the running dock.exe: {}", windows::core::Error::from_thread()));
    }
    let image = RunningImage { view, len, _map: map, file };
    // SAFETY: no name: the exe of this process, mapped for its whole life.
    let base = unsafe { GetModuleHandleW(PCWSTR::null()) }.map_err(|e| format!("GetModuleHandleW: {e}"))?;
    // SAFETY: two views mapped in this process for the whole call, its image and the file's; neither is written.
    if unsafe { NtAreMappedFilesTheSame(base.0, image.view.Value) } != 0 {
        return Err(NOT_THE_IMAGE.into());
    }
    Ok(image)
}

/// `running_image`'s refusal of a file that is not the running image.
const NOT_THE_IMAGE: &str = "the file at its path is not the running dock.exe (renamed or replaced since it started)";

#[link(name = "ntdll")]
extern "system" {
    /// STATUS_SUCCESS (0) when `image`, an address in a mapped image, and `file`, one in a mapped view of a file, are
    /// backed by the same file.
    fn NtAreMappedFilesTheSame(image: *mut c_void, file: *mut c_void) -> i32;
}

/// The running image's file (`running_image`), held open and mapped read-only until it is dropped.
pub(crate) struct RunningImage {
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    len: usize,
    _map: OwnedHandle,
    file: std::fs::File,
}

impl RunningImage {
    fn bytes(&self) -> &[u8] {
        // SAFETY: the view maps the whole file, `len` bytes, read-only, until `drop`; nobody can change the file while
        // `file` is open (read sharing only), and the slice lives no longer than `&self`.
        unsafe { std::slice::from_raw_parts(self.view.Value.cast::<u8>().cast_const(), self.len) }
    }

    /// The path that starts this very file while it is held (`oled_sys::worker::bound::held_start_path`: its final
    /// path, every link and junction resolved, when it names this file and nothing else); None when it has none.
    pub(crate) fn start_path(&self) -> Option<PathBuf> {
        oled_sys::worker::bound::held_start_path(&self.file)
    }
}

impl Drop for RunningImage {
    fn drop(&mut self) {
        // SAFETY: the view mapped in `running_image`, unmapped once; the mapping and the file close after it.
        let _ = unsafe { UnmapViewOfFile(self.view) };
    }
}

/// `<root>\update\<tag>\` through datadir's checked stage functions (admin-only, no link followed).
struct DataFromStage {
    tag: String,
}

impl FromStage for DataFromStage {
    fn copy_in(&mut self, name: &str, src: &Path) -> Result<(), String> {
        crate::datadir::copy_into_stage(&self.tag, name, src)?;
        // CopyFileW copies the alternate streams too: the Mark of the Web of a downloaded zip would follow the file
        // into the install folder, and the shell would warn on every start of the app. A copy that keeps it goes.
        let unmarked = crate::datadir::checked_stage_path(&self.tag, name)
            .map_err(|e| e.to_string())
            .and_then(|p| drop_zone_mark(&p));
        if unmarked.is_err() {
            self.remove(name);
        }
        unmarked
    }

    fn write(&mut self, name: &str, bytes: &[u8]) -> Result<(), String> {
        crate::datadir::write_stage_file(&self.tag, name, bytes)
    }

    fn staged(&mut self, name: &str) -> Result<(u64, Digest), String> {
        let bytes = crate::datadir::read_stage_file(&self.tag, name, COMPONENT_MAX_BYTES)
            .map_err(|e| format!("the staged copy could not be read ({})", e.kind()))?;
        Ok((bytes.len() as u64, oled_sys::sha256::sha256(&bytes)?))
    }

    fn remove(&mut self, name: &str) {
        if let Ok(p) = crate::datadir::checked_stage_path(&self.tag, name) {
            // Deleting a name removes the link itself, never a link target.
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Deletes `path`'s Zone.Identifier stream, the Mark of the Web; a file without one is left as it is.
fn drop_zone_mark(path: &Path) -> Result<(), String> {
    let mut mark = path.as_os_str().to_owned();
    mark.push(":Zone.Identifier");
    match std::fs::remove_file(PathBuf::from(mark)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
        _ => Ok(()),
    }
}

/// Calls `f` until it succeeds or `limit` has passed since the first try, sleeping `step` in between.
/// Returns the first success or the last error.
fn retry_for<T, E>(limit: Duration, step: Duration, mut f: impl FnMut() -> Result<T, E>) -> Result<T, E> {
    let t = Instant::now();
    loop {
        match f() {
            Ok(v) => return Ok(v),
            Err(e) if t.elapsed() >= limit => return Err(e),
            Err(_) => std::thread::sleep(step),
        }
    }
}

/// Stops the running dock (`Local\DockStop`) and waits, up to `STOP_WAIT`, until neither its
/// single-instance mutex nor any other process running `exe` is left (after a crash its watchdog
/// may hold the exe while it restores the taskbar). Unlike `copy_self` there is no early exit:
/// the files are deleted only once nothing runs them. This process is never counted, so the wait
/// also ends when the uninstaller is the installed exe itself (started from Add/Remove Programs).
pub(crate) fn stop_and_wait(exe: &Path) -> Result<(), InstallError> {
    let signalled = crate::watchdog::signal_stop_to_instance();
    log!("autostart: stop requested (instance answered: {signalled}), waiting for {}", exe.display());
    let t = Instant::now();
    let gone = || {
        if crate::watchdog::instance_running() || exe_in_use_by_other(exe) {
            Err(())
        } else {
            Ok(())
        }
    };
    match retry_for(STOP_WAIT, RETRY_STEP, gone) {
        Ok(()) => {
            log!("autostart: old instance gone after {} ms", t.elapsed().as_millis());
            Ok(())
        }
        Err(()) => Err(InstallError::CouldNotStop(format!(
            "the running dock or its watchdog did not exit within {} s",
            STOP_WAIT.as_secs()
        ))),
    }
}

/// The install-relative names `--uninstall --remove` deletes: the dock's own five, then every other
/// component's file, staging copy, previous copy and its staging copy, then the suite's own files. Pure.
fn removal_names() -> Vec<String> {
    use oled_trust::manifest::suite::{ComponentId, SUITE_FILES};
    let mut v: Vec<String> =
        ["dock.exe", "dock.exe.new", PREV_NAME, "dock.prev.exe.new", "autostart-task.xml"].map(String::from).into();
    for c in ComponentId::ALL.into_iter().filter(|c| *c != ComponentId::Dock) {
        v.extend(c.install_names());
    }
    v.extend(SUITE_FILES.map(String::from));
    v
}

/// The only files `--uninstall --remove` deletes from the install folder. Pure.
fn removal_targets(install_dir: &Path) -> Vec<PathBuf> {
    removal_names().iter().map(|n| install_dir.join(n)).collect()
}

/// Asks Windows to delete `path` (a file or an empty folder) at the next restart. Elevated only.
fn delete_at_reboot(path: &Path) -> Result<(), String> {
    let p = HSTRING::from(path.as_os_str());
    // SAFETY: `p` is NUL-terminated and lives for the call; a null new name means "delete".
    unsafe { MoveFileExW(&p, PCWSTR::null(), MOVEFILE_DELAY_UNTIL_REBOOT) }
        .map_err(|e| format!("scheduling the removal of {} at restart: {e}", path.display()))
}

/// The unique name a file in use is renamed to before its delete is scheduled: `<name>.old-<pid>`
/// in the same folder. Pure.
fn old_copy_path(path: &Path, pid: u32) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".old-{pid}"));
    path.with_file_name(name)
}

/// True for a name (install-relative, so `lang\it.lang.old-7` for a file in `lang`) made by
/// `old_copy_path` from one of `removal_targets`. Pure.
fn is_old_copy_name(name: &str) -> bool {
    removal_names().iter().any(|t| {
        name.strip_prefix(t.as_str())
            .and_then(|rest| rest.strip_prefix(".old-"))
            .is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// Deletes the `*.old-<pid>` leftovers of an uninstall not yet followed by a restart (`--install`).
/// Only regular files (no reparse points); one still in use is left to its delete at restart.
fn remove_old_copies(dir: &Path) {
    remove_old_copies_in(dir, "");
    let lang = dir.join(oled_trust::manifest::suite::LANG_DIR);
    if std::fs::symlink_metadata(&lang).is_ok_and(|m| m.is_dir() && !is_reparse(&m)) {
        remove_old_copies_in(&lang, &format!("{}\\", oled_trust::manifest::suite::LANG_DIR));
    }
}

/// True for a folder or file that is a reparse point.
fn is_reparse(m: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
}

/// `remove_old_copies` for one folder; `rel` is its install-relative prefix ("" or "lang\").
fn remove_old_copies_in(dir: &Path, rel: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let name = e.file_name();
        if !name.to_str().is_some_and(|n| is_old_copy_name(&format!("{rel}{n}"))) {
            continue;
        }
        let p = e.path();
        if std::fs::symlink_metadata(&p).is_ok_and(|m| m.is_file()) {
            match std::fs::remove_file(&p) {
                Ok(()) => log!("autostart: leftover {} deleted", p.display()),
                Err(e) => log!("autostart: leftover {} kept ({e}), it goes at restart", p.display()),
            }
        }
    }
}

/// Deletes `path`: `Ok(true)` deleted now (or already gone), `Ok(false)` scheduled for the next
/// restart (in practice only the exe of this very process, when started from Add/Remove Programs).
/// The pending delete is stored by path, so a file in use is first renamed to `old_copy_path`
/// (Windows allows renaming a running image): a reinstall before the restart then puts a new
/// file at `path` that the restart does not touch.
fn remove_or_schedule(path: &Path) -> Result<bool, String> {
    match retry_for(RELEASE_GRACE, RETRY_STEP, || std::fs::remove_file(path)) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(e) => {
            // SAFETY: GetCurrentProcessId has no preconditions.
            let old = old_copy_path(path, unsafe { GetCurrentProcessId() });
            let target = match std::fs::rename(path, &old) {
                Ok(()) => old,
                Err(re) => {
                    log!("autostart: renaming {} to {} failed ({re})", path.display(), old.display());
                    path.to_path_buf()
                }
            };
            log!("autostart: deleting {} failed ({e}), scheduling {} at restart", path.display(), target.display());
            delete_at_reboot(&target).map(|()| false)
        }
    }
}

/// What `--uninstall` did.
#[derive(Debug, Default)]
pub(crate) struct UninstallReport {
    /// The running dock could not be stopped: nothing was touched.
    pub stop: Option<InstallError>,
    pub failures: Vec<String>,
    /// Some file is deleted at the next restart.
    pub scheduled: bool,
    /// Said after the failures: a step that did not complete without failing the uninstall (the user's part).
    pub notes: Vec<String>,
}

impl UninstallReport {
    pub(crate) fn exit_code(&self) -> i32 {
        match (&self.stop, self.failures.is_empty()) {
            (Some(e), _) => e.exit_code(),
            (None, false) => EXIT_FAILED,
            (None, true) => EXIT_OK,
        }
    }
}

/// The file part of `remove_install`: every `removal_targets` file, then `lang\` when it is a real, empty
/// folder. A `lang\` that is a junction or symlink is left alone: nothing inside it is touched.
fn remove_our_files(dir: &Path, report: &mut UninstallReport) {
    let lang_dir = oled_trust::manifest::suite::LANG_DIR;
    let lang = dir.join(lang_dir);
    let lang_meta = std::fs::symlink_metadata(&lang);
    let lang_real = lang_meta.as_ref().is_ok_and(|m| m.is_dir() && !is_reparse(m));
    if lang_meta.is_ok() && !lang_real {
        report.failures.push(format!("{} is not a real folder: nothing deleted there", lang.display()));
    }
    let lang_prefix = format!("{lang_dir}\\");
    for (name, p) in removal_names().into_iter().zip(removal_targets(dir)) {
        if name.starts_with(&lang_prefix) && !lang_real {
            continue;
        }
        if std::fs::symlink_metadata(&p).is_err() {
            continue;
        }
        match remove_or_schedule(&p) {
            Ok(now) => report.scheduled |= !now,
            Err(e) => report.failures.push(e),
        }
    }
    // lang\ goes only when it is a real folder and empty (remove_dir fails on a non-empty one).
    if lang_real {
        let _ = std::fs::remove_dir(&lang);
    }
}

/// Deletes the install folder for `--uninstall --remove`: only `removal_targets`, then the folder if
/// it is empty (nothing unknown is deleted). Every failure is pushed to `report.failures`.
fn remove_install(dir: &Path, report: &mut UninstallReport) {
    match std::fs::symlink_metadata(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => return report.failures.push(format!("{}: {e}", dir.display())),
        Ok(m) => {
            use std::os::windows::fs::MetadataExt;
            if !m.is_dir() || m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
                return report.failures.push(format!("{} is not a real folder: nothing deleted", dir.display()));
            }
        }
    }
    remove_our_files(dir, report);
    if let Err(e) = std::fs::remove_dir(dir) {
        log!("autostart: removing {} failed ({e}), scheduling it at restart", dir.display());
        match delete_at_reboot(dir) {
            Ok(()) => report.scheduled = true,
            Err(e) => report.failures.push(e),
        }
    }
}

/// Test helper, also for the update group's swap test: every path in `created` (a file the swap creates, as
/// an install-relative or absolute path under `install_dir`) is deleted by `--uninstall --remove`: it is in
/// `removal_targets`, or its `.old-3` copy passes `is_old_copy_name`.
#[cfg(test)]
pub(crate) fn assert_uninstall_removes(install_dir: &Path, created: &[PathBuf]) {
    let targets = removal_targets(install_dir);
    for c in created {
        let full = if c.is_absolute() { c.clone() } else { install_dir.join(c) };
        let rel = full.strip_prefix(install_dir).expect("created path is inside the install folder");
        let rel = rel.to_string_lossy().replace('/', "\\");
        assert!(targets.contains(&full), "{rel} is removed");
        assert!(is_old_copy_name(&format!("{rel}.old-3")), "{rel}.old-3 is ours");
    }
}

/// Lexical form for comparing two paths: backslashes, no `.` or `..` parts, no trailing slash, lower case.
fn normal_path(p: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    for s in p.replace('/', "\\").split('\\') {
        match s {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s.to_lowercase()),
        }
    }
    parts.join("\\")
}

/// True when `path` is a shortcut of ours: a `.lnk` (any name or case) directly in `programs_root` whose
/// `target` is `<install_dir>\tasks.exe` or `<install_dir>\files.exe`. Pure.
fn should_delete_shortcut(path: &Path, target: &str, programs_root: &Path, install_dir: &Path) -> bool {
    let is_lnk = path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("lnk"));
    let direct = path
        .parent()
        .is_some_and(|p| normal_path(&p.to_string_lossy()) == normal_path(&programs_root.to_string_lossy()));
    if !is_lnk || !direct {
        return false;
    }
    let t = normal_path(target);
    let dir = normal_path(&install_dir.to_string_lossy());
    [oled_trust::manifest::suite::ComponentId::Tasks, oled_trust::manifest::suite::ComponentId::Files]
        .iter()
        .any(|c| t == format!("{dir}\\{}", c.file()))
}

/// Deletes our shortcuts in the all-users Programs folder (no recursion; the target of each `.lnk` is read
/// with the bounded Rust parser, no COM). Every failure is pushed to `report.failures`.
fn remove_shortcuts(install_dir: &Path, report: &mut UninstallReport) {
    let root = match common_programs() {
        Ok(r) => r,
        Err(e) => {
            log!("autostart: Start menu folder not found ({e})");
            return report.failures.push(format!("Start menu folder not found: {e}"));
        }
    };
    let entries = match std::fs::read_dir(&root) {
        Ok(x) => x,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            log!("autostart: Start menu folder not listed ({e})");
            return report.failures.push(format!("{}: {e}", root.display()));
        }
    };
    for e in entries.flatten() {
        let p = e.path();
        if !p.extension().and_then(|x| x.to_str()).is_some_and(|x| x.eq_ignore_ascii_case("lnk")) {
            continue;
        }
        if !std::fs::symlink_metadata(&p).is_ok_and(|m| m.is_file()) {
            continue;
        }
        let Ok(facts) = crate::jumplist::read_lnk(&p) else { continue };
        if should_delete_shortcut(&p, &facts.target, &root, install_dir) {
            match std::fs::remove_file(&p) {
                Ok(()) => log!("autostart: shortcut {} deleted", p.display()),
                Err(e) => report.failures.push(format!("{}: {e}", p.display())),
            }
        }
    }
}

/// After the restore of `--uninstall --remove`: a taskbar state file that is still there means the restore was
/// not delivered (Explorer did not answer). Removing the exe, or the data folder, now would leave nothing that can
/// finish the restore, so the removal stops.
fn removal_after_restore(state_left: bool) -> Result<(), InstallError> {
    if state_left {
        Err(InstallError::Failed(
            "the Windows taskbar was not restored (Explorer did not answer) and its saved state is kept. \
             Run dock.exe --restore, then uninstall again."
                .into(),
        ))
    } else {
        Ok(())
    }
}

/// One step of `--uninstall` after the dock is stopped and the taskbar restored (`uninstall_steps`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UninstallStep {
    /// The user journal's takeovers, by `--undo-user --signal` started as the shell's user (J1c).
    UndoUser,
    /// The machine journal's open Takeover and Install entries, newest first, with F-P5's ownership rule.
    UndoMachine,
    /// Our Start entries, recognised by their target (FC1).
    RemoveEntryPoints,
    DeleteTask,
    DeleteArp,
    /// FC1's removal list, then the install folder when empty.
    RemoveInstall,
    RemoveUpdateDir,
    /// update.jnl, suite.json and theme.json in `<root>\suite`, `<root>\safe-mode`, then `suite\` and `journal\`
    /// when empty.
    RemoveSuiteData,
}

/// The steps of `--uninstall`, in order. Pure. With `remove`: the journals' undo first (the user's takeovers, then the
/// machine's takeovers and the suite's own Install entries: the ARP key, the task, the Start entries), then today's
/// explicit removal, which stays authoritative and removes what the journal could not (an install made before 1.8.0, a
/// missing journal), then the suite's files in the data root. Without `remove`: the task and the ARP entry, as before
/// (the installed copy, its Start entries and the journals stay).
pub(crate) fn uninstall_steps(remove: bool) -> Vec<UninstallStep> {
    use UninstallStep::*;
    if remove {
        vec![
            UndoUser,
            UndoMachine,
            RemoveEntryPoints,
            DeleteTask,
            DeleteArp,
            RemoveInstall,
            RemoveUpdateDir,
            RemoveSuiteData,
        ]
    } else {
        vec![DeleteTask, DeleteArp]
    }
}

/// What `--uninstall` undoes in a journal: its open Takeover and Install entries (never UserEdit: changes the user made
/// through Tasks stay, F-P5).
pub(crate) fn undo_suite_entries(
    j: &mut journal::Journal,
    sink: &mut dyn journal::Sink,
    os: &mut dyn journal::JournalOs,
) -> UndoReport {
    journal::undo(j, sink, os, &UndoSet::Classes(vec![KindClass::Takeover, KindClass::Install]))
}

/// `undo_suite_entries` on the machine journal under its lock, the file deleted when nothing is left open. An empty
/// report when there is no machine journal.
fn undo_machine_suite() -> Result<UndoReport, String> {
    if !journal_present(journal_os::path(Scope::Machine).as_deref()) {
        return Ok(UndoReport::default());
    }
    let (mut j, mut sink) = journal_os::open_locked(Scope::Machine, JOURNAL_WAIT).map_err(|e| e.what)?;
    let report = undo_suite_entries(&mut j, &mut sink, &mut RealOs::for_scope(Scope::Machine));
    let _ = sink.delete_if_settled(&j);
    Ok(report)
}

/// What an undo could not restore, in words (no path: the report is said and logged).
fn undo_failure(t: &Target, e: &journal::OsErr) -> String {
    let what = match t {
        Target::Reg { .. } => "a registry key",
        Target::Task(_) => "the logon task",
        Target::File(_) => "a Start menu entry",
    };
    format!("{what} could not be undone ({})", e.what)
}

/// The suite's own files in the data root, once the install is gone (`RemoveSuiteData`). Only in a root this process
/// verified as writable; `suite\` and `journal\` go only when empty (a machine journal that still holds entries, a
/// user's change made through Tasks, stays with its folder).
fn remove_suite_data(report: &mut UninstallReport) {
    if !crate::datadir::writable() {
        return;
    }
    let suite = oled_sys::datadir::suite_dir();
    let files = [
        crate::datadir::update_journal_path(),
        suite.as_ref().map(|d| d.join(oled_trust::manifest::suite::SUITE_ASSET)),
        suite.as_ref().map(|d| d.join(oled_config::theme::THEME_FILE)),
        crate::datadir::safe_mode_path(),
    ];
    for p in files.into_iter().flatten() {
        // Deleting a name removes the link itself, never a link target.
        match std::fs::remove_file(&p) {
            Ok(()) => log!("autostart: {} deleted", p.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => report.failures.push(format!("{}: {e}", p.display())),
        }
    }
    for d in [suite, oled_sys::datadir::journal_dir()].into_iter().flatten() {
        // remove_dir fails on a folder that is not empty: what is left is kept.
        let _ = std::fs::remove_dir(&d);
    }
}

/// Runs one step of the uninstall; its failures go to `report.failures`, its notes to `report.notes`.
fn run_uninstall_step(step: UninstallStep, dir: Option<&Path>, report: &mut UninstallReport) {
    match step {
        UninstallStep::UndoUser => {
            let r = crate::journal_ui::undo_user_through_shell(crate::journal_ui::USER_PART_WAIT);
            if matches!(r, Ok(true)) {
                log!("autostart: --undo-user answered");
            } else {
                let why = crate::journal_ui::user_part_why(&r);
                log!("autostart: --undo-user did not answer ({why}), the uninstall goes on");
                report.notes.push(format!(
                    "The dock's changes to your user account may not be undone ({why}). Install the dock again and run \
                     \"dock.exe --undo-user\" as your normal user to undo them."
                ));
            }
        }
        UninstallStep::UndoMachine => match undo_machine_suite() {
            Ok(r) => {
                log!("autostart: the machine journal: {}", crate::journal_ui::report_line(&r));
                report.failures.extend(r.failed.iter().map(|(t, e)| undo_failure(t, e)));
            }
            Err(e) => report.failures.push(format!("the machine journal: {e}")),
        },
        UninstallStep::RemoveEntryPoints => {
            if let Some(d) = dir {
                remove_shortcuts(d, report);
            }
        }
        UninstallStep::DeleteTask => report.failures.extend(disable().err()),
        UninstallStep::DeleteArp => report.failures.extend(crate::arp::delete().err()),
        UninstallStep::RemoveInstall => {
            if let Some(d) = dir {
                remove_install(d, report);
            }
        }
        UninstallStep::RemoveUpdateDir => {
            report.failures.extend(crate::datadir::remove_update_dir().err().map(|e| format!("update folder: {e}")));
        }
        UninstallStep::RemoveSuiteData => remove_suite_data(report),
    }
}

/// `--uninstall`: removes the logon task and the Add/Remove Programs entry. With `remove` it first
/// stops the dock (the same wait as `--install`), restores the taskbar, undoes the suite's journaled changes, then
/// deletes the installed files, the Start entries, the update staging folders and the suite's data files
/// (`uninstall_steps`). Returns the exit code.
pub fn run_uninstall_cli(remove: bool) -> i32 {
    let mut report = UninstallReport::default();
    let say = crate::logger::say;
    let dir = install_dir();
    if remove {
        let stopped = match &dir {
            Ok(d) => stop_and_wait(&d.join("dock.exe")),
            Err(e) => Err(InstallError::Failed(e.clone())),
        };
        if let Err(e) = stopped {
            log!("autostart: --uninstall --remove failed: {e}");
            say(&format!("Removal failed: {e}"));
            report.stop = Some(e);
            return report.exit_code();
        }
        crate::taskbar::restore_force_waiting(Duration::from_secs(5));
        if let Err(e) = removal_after_restore(crate::taskbar::state_file_exists()) {
            log!("autostart: --uninstall --remove failed: {e}");
            say(&format!("Removal failed: {e}"));
            report.stop = Some(e);
            return report.exit_code();
        }
    }
    for step in uninstall_steps(remove) {
        run_uninstall_step(step, dir.as_deref().ok(), &mut report);
    }
    for f in &report.failures {
        log!("autostart: --uninstall: {f}");
        say(&format!("Removal failed: {f}"));
    }
    for n in &report.notes {
        say(n);
    }
    let where_ = dir.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|_| INSTALL_DIR_NAME.into());
    match (remove, report.failures.is_empty()) {
        (false, true) => say(&format!(
            "Automatic start and the Add/Remove Programs entry removed. The installed copy in {where_} is kept \
             (it may be running); run `dock.exe --uninstall --remove` to delete it."
        )),
        (true, true) => say(&format!("Dock uninstalled: {where_} removed.")),
        (_, false) => {}
    }
    if report.scheduled {
        say("Some files are removed at the next restart.");
    }
    report.exit_code()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_install_copy_drops_the_download_mark() {
        let dir = std::env::temp_dir().join(format!("dock-zone-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.exe");
        let dst = dir.join("dst.exe");
        std::fs::write(&src, b"image").unwrap();
        let zone = |p: &Path| PathBuf::from(format!("{}:Zone.Identifier", p.display()));
        std::fs::write(zone(&src), b"[ZoneTransfer]\r\nZoneId=3\r\n").unwrap();
        // Plain copy keeps the stream (the behaviour this guards against).
        let plain = dir.join("plain.exe");
        std::fs::copy(&src, &plain).unwrap();
        assert!(zone(&plain).exists(), "std::fs::copy is expected to carry the stream");
        assert_eq!(copy_without_zone_mark(&src, &dst).unwrap(), 5);
        assert_eq!(std::fs::read(&dst).unwrap(), b"image");
        let e = std::fs::metadata(zone(&dst)).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::NotFound);
        // A source without a mark copies the same way.
        std::fs::remove_file(zone(&src)).unwrap();
        assert_eq!(copy_without_zone_mark(&src, &dst).unwrap(), 5);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn under_program_files() {
        let pf = r"C:\Program Files";
        assert!(is_under(r"C:\Program Files\mini-oled-dock\dock.exe", pf));
        assert!(is_under(r"c:\program files\x.exe", r"C:\Program Files\"));
        assert!(is_under(r"\\?\C:\Program Files\x.exe", r"\\?\C:\Program Files"));
        assert!(!is_under(r"C:\Program Files (x86)\x.exe", pf));
        assert!(!is_under(r"C:\Program FilesEvil\x.exe", pf));
        assert!(!is_under(r"C:\Users\u\AppData\Local\Dock\bin\dock.exe", pf));
        assert!(!is_under(pf, pf));
        assert!(!is_under(r"C:\x.exe", ""));
    }

    #[test]
    fn install_waits_longer_than_the_watchdog_restore() {
        assert!(STOP_WAIT >= crate::watchdog::RESTORE_WAIT + Duration::from_secs(5));
        assert_eq!(STOP_WAIT, Duration::from_secs(35));
    }

    #[test]
    fn keep_waiting_while_the_old_instance_or_watchdog_lives() {
        let s = Duration::from_secs;
        assert!(keep_waiting(s(0), false, false), "grace right after the stop signal");
        assert!(!keep_waiting(s(10), false, false), "both gone and the copy still fails: report, do not wait");
        assert!(keep_waiting(s(10), true, false));
        assert!(keep_waiting(s(34), false, true), "watchdog still waiting for Explorer");
        assert!(!keep_waiting(s(35), true, true), "hard limit");
    }

    #[test]
    fn a_copy_is_tried_after_the_old_watchdog_leaves() {
        // Past the grace: a copy fails while the watchdog lives, then the check sees both gone.
        let locked = || std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let mut copies = 0;
        let mut checks = vec![(false, false), (false, true)];
        let r = retry_copy(
            || {
                copies += 1;
                if copies == 1 { Err(locked()) } else { Ok(()) }
            },
            || checks.pop().unwrap_or((false, false)),
            || Duration::from_secs(10),
        );
        assert!(r.is_ok(), "the file freed during the last check is copied");
        assert_eq!(copies, 2);
    }

    #[test]
    fn an_old_watchdog_is_seen_by_its_image_not_by_a_mutex() {
        // A 1.3.0 watchdog is just a process running the installed exe; any running exe stands in for it.
        let ping = crate::shell_thread::system_dir().expect("system dir").join("ping.exe");
        // No console of its own: a console child of a console-less test run opens a visible window.
        let mut child = std::process::Command::new(&ping)
            .creation_flags(CREATE_NO_WINDOW.0)
            .args(["-n", "8", "127.0.0.1"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("spawn ping.exe");
        let seen = exe_in_use_by_other(&ping);
        let _ = child.kill();
        let _ = child.wait();
        assert!(seen, "a process running the destination exe is detected");
        assert!(!exe_in_use_by_other(&std::env::current_exe().unwrap()), "the installer itself does not count");
        assert!(!exe_in_use_by_other(Path::new(r"C:\no\such\dock.exe")));
    }

    #[test]
    fn removal_targets_are_only_our_files() {
        let d = Path::new(r"C:\Program Files\mini-oled-dock");
        let mut want: Vec<PathBuf> =
            ["dock.exe", "dock.exe.new", "dock.prev.exe", "dock.prev.exe.new", "autostart-task.xml"]
                .iter()
                .map(|n| d.join(n))
                .collect();
        for n in [
            "tasks.exe",
            "tasks.exe.new",
            "tasks.prev.exe",
            "tasks.prev.exe.new",
            "tasks-admin.exe",
            "tasks-admin.exe.new",
            "tasks-admin.prev.exe",
            "tasks-admin.prev.exe.new",
            "files.exe",
            "files.exe.new",
            "files.prev.exe",
            "files.prev.exe.new",
            r"lang\it.lang",
            r"lang\it.lang.new",
            r"lang\it.prev.lang",
            r"lang\it.prev.lang.new",
            "suite.json",
            "suite.json.tmp",
        ] {
            want.push(d.join(n));
        }
        assert_eq!(removal_targets(d), want);
    }

    #[test]
    fn every_name_the_swap_creates_is_removed() {
        use oled_trust::manifest::suite::ComponentId;
        let d = Path::new(r"C:\P\m");
        let created: Vec<PathBuf> =
            ComponentId::ALL.into_iter().flat_map(|c| c.install_names()).map(PathBuf::from).collect();
        assert_uninstall_removes(d, &created);
    }

    #[test]
    fn old_copies_of_component_names_are_ours() {
        for t in removal_names() {
            assert!(is_old_copy_name(&format!("{t}.old-12")));
        }
        assert!(is_old_copy_name(r"lang\it.lang.old-7"));
        assert!(!is_old_copy_name("tasks.exe.old-"));
        assert!(!is_old_copy_name("tasks.exe.old-1a"));
    }

    #[test]
    fn lang_folder_removed_only_when_empty() {
        let base = std::env::temp_dir().join(format!("dock-fc1-lang-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("lang")).unwrap();
        std::fs::write(base.join("lang").join("it.lang"), b"x").unwrap();
        std::fs::write(base.join("lang").join("mine.txt"), b"x").unwrap();
        let mut r = UninstallReport::default();
        remove_our_files(&base, &mut r);
        let kept = base.join("lang").join("mine.txt").exists() && !base.join("lang").join("it.lang").exists();
        std::fs::remove_file(base.join("lang").join("mine.txt")).unwrap();
        std::fs::write(base.join("lang").join("it.lang"), b"x").unwrap();
        let mut r2 = UninstallReport::default();
        remove_install(&base, &mut r2);
        let gone = !base.exists();
        let clean = r.failures.is_empty() && r2.failures.is_empty() && !r2.scheduled;
        let _ = std::fs::remove_dir_all(&base);
        assert!(clean, "{:?} {:?}", r.failures, r2.failures);
        assert!(kept, "a foreign file keeps lang\\ and only ours goes");
        assert!(gone, "an emptied lang\\ and the folder go");
    }

    #[test]
    fn lang_junction_is_not_followed() {
        let base = std::env::temp_dir().join(format!("dock-fc1-junction-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let outside = base.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("it.lang"), b"x").unwrap();
        let inst = base.join("inst");
        std::fs::create_dir_all(&inst).unwrap();
        use std::os::windows::process::CommandExt;
        let made = std::process::Command::new("cmd")
            .creation_flags(CREATE_NO_WINDOW.0)
            .args(["/C", "mklink", "/J"])
            .arg(inst.join("lang"))
            .arg(&outside)
            .output()
            .is_ok_and(|o| o.status.success());
        let mut r = UninstallReport::default();
        if made {
            remove_our_files(&inst, &mut r);
        }
        let survives = outside.join("it.lang").exists();
        let _ = std::process::Command::new("cmd")
            .creation_flags(CREATE_NO_WINDOW.0)
            .args(["/C", "rmdir"])
            .arg(inst.join("lang"))
            .output();
        let _ = std::fs::remove_dir_all(&base);
        assert!(made, "the junction could be made");
        assert!(survives, "a file behind a lang junction is not deleted");
        assert!(!r.failures.is_empty(), "the junction is reported");
    }

    #[test]
    fn our_shortcuts_only() {
        let root = Path::new(r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs");
        let d = Path::new(r"C:\Program Files\mini-oled-dock");
        let lnk = root.join("Anything.LNK");
        assert!(should_delete_shortcut(&lnk, r"C:\Program Files\mini-oled-dock\tasks.exe", root, d));
        assert!(should_delete_shortcut(&lnk, r"c:\program files\MINI-oled-dock\Files.exe", root, d));
        assert!(should_delete_shortcut(&lnk, r"C:\Program Files\mini-oled-dock\x\..\files.exe", root, d));
        assert!(!should_delete_shortcut(
            &root.join("sub").join("a.lnk"),
            r"C:\Program Files\mini-oled-dock\tasks.exe",
            root,
            d
        ));
        assert!(!should_delete_shortcut(&lnk, r"C:\Program Files\mini-oled-dock\dock.exe", root, d));
        assert!(!should_delete_shortcut(&root.join("a.url"), r"C:\Program Files\mini-oled-dock\tasks.exe", root, d));
        assert!(!should_delete_shortcut(&lnk, r"C:\Other\tasks.exe", root, d));
    }

    #[test]
    fn no_display_name_literal() {
        let src = include_str!("autostart.rs");
        let body = &src[..src.find("#[cfg(test)]").unwrap()];
        for w in ["Tasks", "Folio", "Attivit\u{e0}"] {
            assert!(!body.contains(&format!("\"{w}\"")), "no quoted {w}");
        }
    }

    #[test]
    fn previous_exe_is_kept_only_on_an_upgrade() {
        let v = |s| Version::parse(s).unwrap();
        assert!(!keeps_prev(None, v("1.5.1"))); // nothing installed, or its version unreadable
        assert!(keeps_prev(Some(v("1.5.0")), v("1.5.1")));
        assert!(!keeps_prev(Some(v("1.5.1")), v("1.5.1"))); // a reinstall
        assert!(!keeps_prev(Some(v("1.5.2")), v("1.5.1"))); // a downgrade: a rollback never touches it
    }

    #[test]
    fn previous_exe_paths_sit_next_to_dock_exe() {
        let dest = Path::new(r"C:\P\m\dock.exe");
        assert_eq!(prev_path(dest), PathBuf::from(r"C:\P\m\dock.prev.exe"));
        assert_eq!(prev_staging_path(dest), PathBuf::from(r"C:\P\m\dock.prev.exe.new"));
        assert!(prev_staging_path(dest).to_string_lossy().ends_with("dock.prev.exe.new"));
    }

    #[test]
    fn uninstall_removes_the_previous_exe_too() {
        let d = Path::new(r"C:\Program Files\mini-oled-dock");
        let targets = removal_targets(d);
        assert!(targets.contains(&d.join("dock.prev.exe")) && targets.contains(&d.join("dock.prev.exe.new")));
        assert!(is_old_copy_name("dock.prev.exe.old-12"));
        assert!(is_old_copy_name("dock.prev.exe.new.old-7"));
        assert!(!is_old_copy_name("dock.prev.exe.old-"));
        assert!(!is_old_copy_name("dock.prev.exe"));
    }

    #[test]
    fn a_file_in_use_is_renamed_to_a_unique_old_copy() {
        let exe = Path::new(r"C:\Program Files\mini-oled-dock\dock.exe");
        let old = old_copy_path(exe, 4242);
        assert_eq!(old, PathBuf::from(r"C:\Program Files\mini-oled-dock\dock.exe.old-4242"));
        assert_ne!(old, old_copy_path(exe, 4243));
        assert!(is_old_copy_name("dock.exe.old-4242"));
        assert!(is_old_copy_name("autostart-task.xml.old-1"));
        assert!(!is_old_copy_name("dock.exe"));
        assert!(!is_old_copy_name("dock.exe.old-"));
        assert!(!is_old_copy_name("dock.exe.old-12x"));
        assert!(!is_old_copy_name("other.exe.old-12"));
        assert!(!is_old_copy_name("dock.exe.old-12.dll"));
    }

    #[test]
    fn removal_stops_while_the_taskbar_state_is_kept() {
        assert!(removal_after_restore(false).is_ok());
        let e = removal_after_restore(true).unwrap_err();
        assert!(matches!(e, InstallError::Failed(_)));
        assert_ne!(UninstallReport { stop: Some(e), ..Default::default() }.exit_code(), EXIT_OK);
    }

    #[test]
    fn uninstall_report_exit_codes() {
        assert_eq!(UninstallReport::default().exit_code(), EXIT_OK);
        // A file in use deleted at the next restart is still a successful uninstall.
        assert_eq!(UninstallReport { scheduled: true, ..Default::default() }.exit_code(), EXIT_OK);
        assert_eq!(UninstallReport { failures: vec!["ARP".into()], ..Default::default() }.exit_code(), EXIT_FAILED);
        let stuck = UninstallReport { stop: Some(InstallError::CouldNotStop("x".into())), ..Default::default() };
        assert_eq!(stuck.exit_code(), EXIT_STOP_FAILED);
    }

    #[test]
    fn retry_for_retries_until_success_or_the_limit() {
        let mut calls = 0;
        let r = retry_for(Duration::from_secs(5), Duration::from_millis(1), || {
            calls += 1;
            if calls < 3 {
                Err(calls)
            } else {
                Ok(calls)
            }
        });
        assert_eq!(r, Ok(3));
        let mut calls = 0;
        let r: Result<(), i32> = retry_for(Duration::ZERO, Duration::from_millis(1), || {
            calls += 1;
            Err(calls)
        });
        assert_eq!(r, Err(1), "past the limit: one try, the last error");
        assert!(RELEASE_GRACE < STOP_WAIT && RETRY_STEP < RELEASE_GRACE);
    }

    #[test]
    fn relaunch_rules() {
        // --install always relaunches, unless --no-relaunch or a dock is still running at the end
        // (the installed exe re-run over its own running dock, or a dock that could not be stopped).
        assert!(should_relaunch(false, false));
        assert!(!should_relaunch(true, false));
        assert!(!should_relaunch(false, true), "--no-relaunch wins");
        assert!(!should_relaunch(true, true));
    }

    #[test]
    fn install_stages_next_to_the_destination() {
        assert_eq!(
            staging_path(Path::new(r"C:\Program Files\mini-oled-dock\dock.exe")),
            PathBuf::from(r"C:\Program Files\mini-oled-dock\dock.exe.new")
        );
        assert_eq!(InstallError::Failed("x".into()).exit_code(), EXIT_FAILED);
        assert_eq!(InstallError::CouldNotStop("x".into()).exit_code(), EXIT_STOP_FAILED);
        assert_eq!(InstallError::CouldNotStop("held".into()).to_string(), "held");
    }

    #[test]
    fn install_report_exit_codes() {
        let r = |copy, error| InstallReport { copy, stop_signalled: false, error };
        assert_eq!(r(CopyOutcome::Copied, None).exit_code(), EXIT_OK);
        assert_eq!(r(CopyOutcome::AlreadyInstalled, None).exit_code(), EXIT_OK);
        // Copied, but schtasks failed: the new exe is relaunched and the exit code is 1.
        assert_eq!(r(CopyOutcome::Copied, Some(InstallError::Failed("schtasks".into()))).exit_code(), EXIT_FAILED);
        assert_eq!(
            r(CopyOutcome::Failed, Some(InstallError::CouldNotStop("held".into()))).exit_code(),
            EXIT_STOP_FAILED
        );
    }

    #[test]
    fn replace_is_refused_while_the_destination_is_held() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = std::env::temp_dir().join(format!("dock-replace-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (dest, new) = (dir.join("dock.exe"), dir.join("dock.exe.new"));
        std::fs::write(&dest, b"old").unwrap();
        std::fs::write(&new, b"new").unwrap();
        // A running image is open without FILE_SHARE_DELETE: a rename over it is refused.
        let held = std::fs::OpenOptions::new().read(true).share_mode(1).open(&dest).unwrap();
        let refused = replace_file(&new, &dest).is_err();
        drop(held);
        let replaced = replace_file(&new, &dest);
        let (content, new_left) = (std::fs::read(&dest).unwrap(), new.exists());
        let _ = std::fs::remove_dir_all(&dir);
        assert!(refused, "the held file is never written");
        assert!(replaced.is_ok(), "{replaced:?}");
        assert_eq!(content, b"new");
        assert!(!new_left, "the staged copy is moved, not copied");
    }

    // ------------------------------------------------------------------------- I1c: the install journal, the uninstall

    use oled_trust::journal::{Journal, OsErr, RegData, RegSnapshot, RunId, TakeoverId};

    fn digest(b: u8) -> Digest {
        [b; 32]
    }

    fn entry(seq: u64, kind: Kind, change: Change) -> Entry {
        Entry { seq, run: RunId(1), ts: 0, kind, change }
    }

    fn arp_created() -> Change {
        Change::RegKeyCreated { hive: Hive::Hklm, key: RegPath::new(crate::arp::ARP_KEY).unwrap() }
    }

    fn task(before: Option<&str>, after: Digest) -> Change {
        Change::Task { name: TaskName::new(TASK_JOURNAL_NAME).unwrap(), before_xml: before.map(String::from), after }
    }

    /// What an install finds: whether the ARP key exists, and the logon task's XML.
    fn found(arp_key: bool, task: Option<&str>) -> InstallBefore {
        InstallBefore { arp_key, task: task.map(String::from) }
    }

    #[test]
    fn install_journal_plan() {
        let plan = super::install_journal_plan;
        let t = digest(7);
        let fresh = found(false, None);
        // A fresh install creates the key and the task: both are journaled as created, so an undo deletes them.
        assert_eq!(plan(&[], &fresh, t), [arp_created(), task(None, t)]);
        // Over an install that left no journal (a version before 1.8.0) nothing is journaled as created: a key that
        // is there is not ours to delete (journal::apply's rule; the uninstall's own removal deletes it), and the
        // task's XML as found is its before, so an undo puts it back.
        assert_eq!(plan(&[], &found(true, Some("<older/>")), t), [task(Some("<older/>"), t)]);
        assert_eq!(plan(&[], &found(true, None), t), [task(None, t)]);
        assert_eq!(plan(&[], &found(false, Some("<older/>")), t), [arp_created(), task(Some("<older/>"), t)]);
        // An open entry stands for its target.
        assert_eq!(plan(&[entry(1, Kind::Install, arp_created())], &fresh, t), [task(None, t)]);
        assert_eq!(plan(&[entry(1, Kind::Install, task(None, t))], &fresh, t), [arp_created()]);
        // Both open and the newest task entry has this XML: nothing, so a reinstall does not grow the journal.
        let both = [
            entry(1, Kind::Install, arp_created()),
            entry(2, Kind::Install, task(Some("<first/>"), digest(1))),
            entry(3, Kind::Install, task(Some("<later/>"), t)),
        ];
        let installed = found(true, Some("<registered/>"));
        assert!(plan(&both, &installed, t).is_empty());
        // Another XML: one new entry whose before is the task as this install finds it, never an older entry's (one
        // that 1.8.0 wrote without looking may say there was no task).
        assert_eq!(plan(&both, &installed, digest(9)), [task(Some("<registered/>"), digest(9))]);
        // An older entry with this XML does not count: the newest decides.
        let older = [
            entry(1, Kind::Install, arp_created()),
            entry(2, Kind::Install, task(None, t)),
            entry(5, Kind::Install, task(None, digest(3))),
        ];
        assert_eq!(plan(&older, &installed, t), [task(Some("<registered/>"), t)]);
        // Only the install's own entries stand for it, and names compare as Windows compares them.
        let takeover = entry(1, Kind::Takeover(TakeoverId::FolderVerbs), arp_created());
        assert_eq!(plan(&[takeover], &fresh, t), [arp_created(), task(None, t)]);
        let lower = Change::Task { name: TaskName::new("\\dock").unwrap(), before_xml: None, after: t };
        let upper =
            Change::RegKeyCreated { hive: Hive::Hklm, key: RegPath::new(&crate::arp::ARP_KEY.to_uppercase()).unwrap() };
        assert!(plan(&[entry(1, Kind::Install, lower), entry(2, Kind::Install, upper)], &installed, t).is_empty());
        // Another hive is another key.
        let hkcu = Change::RegKeyCreated { hive: Hive::Hkcu, key: RegPath::new(crate::arp::ARP_KEY).unwrap() };
        assert_eq!(plan(&[entry(1, Kind::Install, hkcu)], &fresh, t), [arp_created(), task(None, t)]);
    }

    /// The machine of an install test: the calls in order, and the answers the test sets.
    #[derive(Default)]
    struct FakeInstall {
        calls: Vec<&'static str>,
        open: Vec<Entry>,
        recorded: Vec<Change>,
        fail_record: bool,
        fail_task: bool,
        copy: Option<CopyOutcome>,
        /// Runs in the copy, between `--from`'s check and its staging.
        on_copy: Option<Box<dyn FnMut()>>,
        /// What the install finds; None: a fresh machine (no ARP key, no task).
        found: Option<InstallBefore>,
        fail_before: bool,
    }

    impl InstallOs for FakeInstall {
        fn record(&mut self, changes: &[Change]) -> Result<(), String> {
            self.calls.push("record");
            if self.fail_record {
                return Err("journal busy".into());
            }
            self.recorded.extend_from_slice(changes);
            Ok(())
        }
        fn copy_self(&mut self, dest: &Path, _: bool) -> InstallReport {
            self.calls.push("copy_self");
            assert!(dest.ends_with("dock.exe"));
            if let Some(f) = self.on_copy.as_mut() {
                f();
            }
            InstallReport { copy: self.copy.unwrap_or(CopyOutcome::Copied), stop_signalled: false, error: None }
        }
        fn register_task(&mut self, _: &Path) -> Result<(), String> {
            self.calls.push("register_task");
            if self.fail_task {
                Err("schtasks /Create returned 1".into())
            } else {
                Ok(())
            }
        }
        fn task_xml_digest(&self) -> Result<Digest, String> {
            Ok(digest(7))
        }
        fn arp_write(&mut self, _: &Path) -> Result<(), String> {
            self.calls.push("arp_write");
            Ok(())
        }
        fn open_install_entries(&self) -> Vec<Entry> {
            self.open.clone()
        }
        fn before(&self) -> Result<InstallBefore, String> {
            if self.fail_before {
                return Err("the logon task: schtasks /Query failed".into());
            }
            Ok(self.found.clone().unwrap_or_else(|| found(false, None)))
        }
        fn remove_old_copies(&mut self, _: &Path) {
            self.calls.push("remove_old_copies");
        }
        fn sync_entry_points(&mut self, _: &Path) -> Result<u32, String> {
            self.calls.push("sync_entry_points");
            Err("no shell".into())
        }
    }

    #[test]
    fn enable_goes_through_install_os() {
        let mut os = FakeInstall::default();
        let r = enable_with(&mut os, true);
        assert!(r.error.is_none(), "{:?}", r.error);
        assert_eq!(r.copy, CopyOutcome::Copied);
        // The record first, then each step once; a Start entries error is only logged.
        let all = ["record", "copy_self", "remove_old_copies", "register_task", "arp_write", "sync_entry_points"];
        assert_eq!(os.calls, all);
        assert_eq!(os.recorded, [arp_created(), task(None, digest(7))], "the ARP key and the task, journaled first");
        // A second install over the same journal records nothing new and goes the same way.
        let open = os.recorded.iter().enumerate().map(|(i, c)| entry(i as u64 + 1, Kind::Install, c.clone())).collect();
        let mut again = FakeInstall { open, found: Some(found(true, Some("<registered/>"))), ..FakeInstall::default() };
        assert!(enable_with(&mut again, false).error.is_none());
        assert!(again.recorded.is_empty());
        assert_eq!(again.calls, all);
        // A failed record changes nothing.
        let mut refused = FakeInstall { fail_record: true, ..FakeInstall::default() };
        let r = enable_with(&mut refused, true);
        assert_eq!(r.copy, CopyOutcome::Failed);
        assert!(r.error.as_ref().is_some_and(|e| e.to_string().contains("journal busy")), "{:?}", r.error);
        assert_eq!(refused.calls, ["record"]);
        // A failed task keeps the copy and reports it, as before 1.8.0; the ARP entry is still written.
        let mut task_fails = FakeInstall { fail_task: true, ..FakeInstall::default() };
        let r = enable_with(&mut task_fails, true);
        assert_eq!((r.copy, r.exit_code()), (CopyOutcome::Copied, EXIT_FAILED));
        assert!(r.error.as_ref().is_some_and(|e| e.to_string().contains("schtasks")), "{:?}", r.error);
        assert_eq!(task_fails.calls, all);
        // A failed copy stops there.
        let mut copy_fails = FakeInstall { copy: Some(CopyOutcome::Failed), ..FakeInstall::default() };
        assert_eq!(enable_with(&mut copy_fails, true).copy, CopyOutcome::Failed);
        assert_eq!(copy_fails.calls, ["record", "copy_self"]);
    }

    #[test]
    fn an_install_over_an_older_one_journals_what_it_replaces() {
        // An older install that left no journal: the key is left out, the task's XML is its before.
        let mut over = FakeInstall { found: Some(found(true, Some("<older/>"))), ..FakeInstall::default() };
        assert!(enable_with(&mut over, true).error.is_none());
        assert_eq!(over.recorded, [task(Some("<older/>"), digest(7))]);
        // Undone (the Install class: the uninstall's undo), the task gets its older XML back and the key stays.
        let mut j = Journal::empty(Scope::Machine);
        let mut sink = VecSink::default();
        let run = j.begin_run();
        for c in over.recorded {
            journal::record(&mut j, &mut sink, run, Kind::Install, c, 1).unwrap();
        }
        // The fake digests by length: "<seven>" is the XML the entry's after names, the one this install registered.
        let mut os = FakeJournalOs {
            keys: vec![crate::arp::ARP_KEY.to_string()],
            task: Some("<seven>".into()),
            ..FakeJournalOs::default()
        };
        let r = undo_suite_entries(&mut j, &mut sink, &mut os);
        assert_eq!((r.restored, r.failed.len()), (1, 0), "{r:?}");
        assert_eq!(os.task.as_deref(), Some("<older/>"));
        assert_eq!(os.keys, [crate::arp::ARP_KEY]);
        // A machine that cannot be read is not a fresh one: the install is refused before anything is recorded or
        // changed.
        let mut unread = FakeInstall { fail_before: true, ..FakeInstall::default() };
        let r = enable_with(&mut unread, true);
        assert_eq!(r.copy, CopyOutcome::Failed);
        assert!(r.error.as_ref().is_some_and(|e| e.to_string().contains("schtasks /Query")), "{:?}", r.error);
        assert!(unread.calls.is_empty() && unread.recorded.is_empty(), "{:?}", unread.calls);
    }

    #[test]
    fn uninstall_steps() {
        use UninstallStep::*;
        let steps = super::uninstall_steps(true);
        assert_eq!(
            steps,
            [
                UndoUser,
                UndoMachine,
                RemoveEntryPoints,
                DeleteTask,
                DeleteArp,
                RemoveInstall,
                RemoveUpdateDir,
                RemoveSuiteData
            ]
        );
        assert_eq!(super::uninstall_steps(false), [DeleteTask, DeleteArp], "a plain uninstall keeps the files");
    }

    /// The registry, the task and the files of an undo test.
    #[derive(Default)]
    struct FakeJournalOs {
        values: std::collections::BTreeMap<(String, Option<String>), RegData>,
        keys: Vec<String>,
        task: Option<String>,
        files: Vec<(String, Digest)>,
    }

    impl journal::JournalOs for FakeJournalOs {
        fn reg_get(&self, _: Hive, key: &RegPath, name: Option<&str>) -> Result<Option<RegData>, OsErr> {
            Ok(self.values.get(&(key.as_str().to_string(), name.map(String::from))).cloned())
        }
        fn reg_set(&mut self, _: Hive, key: &RegPath, name: Option<&str>, data: Option<&RegData>) -> Result<(), OsErr> {
            let slot = (key.as_str().to_string(), name.map(String::from));
            match data {
                Some(d) => self.values.insert(slot, d.clone()),
                None => self.values.remove(&slot),
            };
            Ok(())
        }
        fn reg_key_exists(&self, _: Hive, key: &RegPath) -> Result<bool, OsErr> {
            Ok(self.keys.iter().any(|k| k == key.as_str()))
        }
        fn reg_key_create(&mut self, _: Hive, key: &RegPath) -> Result<(), OsErr> {
            self.keys.push(key.as_str().to_string());
            Ok(())
        }
        fn reg_key_delete_if_empty(&mut self, _: Hive, _: &RegPath) -> Result<bool, OsErr> {
            Ok(false)
        }
        fn reg_tree_get(&self, _: Hive, _: &RegPath) -> Result<Option<RegSnapshot>, OsErr> {
            Ok(None)
        }
        fn reg_tree_put(&mut self, _: Hive, key: &RegPath, snap: Option<&RegSnapshot>) -> Result<(), OsErr> {
            assert!(snap.is_none(), "an install key is only ever deleted");
            self.keys.retain(|k| k != key.as_str());
            Ok(())
        }
        fn task_xml(&self, _: &TaskName) -> Result<Option<String>, OsErr> {
            Ok(self.task.clone())
        }
        fn task_put(&mut self, _: &TaskName, xml: Option<&str>) -> Result<(), OsErr> {
            self.task = xml.map(String::from);
            Ok(())
        }
        fn file_digest(&self, path: &str) -> Result<Option<Digest>, OsErr> {
            Ok(self.files.iter().find(|(p, _)| p == path).map(|(_, d)| *d))
        }
        fn file_delete(&mut self, path: &str) -> Result<(), OsErr> {
            self.files.retain(|(p, _)| p != path);
            Ok(())
        }
        fn file_restore(&mut self, _: &str, _: &str) -> Result<(), OsErr> {
            Ok(())
        }
        fn digest(&self, bytes: &[u8]) -> Result<Digest, OsErr> {
            Ok(digest(bytes.len() as u8))
        }
    }

    #[derive(Default)]
    struct VecSink(Vec<u8>);

    impl journal::Sink for VecSink {
        fn append(&mut self, bytes: &[u8]) -> Result<(), OsErr> {
            self.0.extend_from_slice(bytes);
            Ok(())
        }
    }

    #[test]
    fn uninstall_undoes_takeovers_and_installs_but_never_user_edits() {
        let key = |s: &str| RegPath::new(s).unwrap();
        let verb = (r"SOFTWARE\Classes\Folder\shell\open\command".to_string(), None);
        let edit = (r"SOFTWARE\MadeUp\Policy".to_string(), Some("Value".to_string()));
        let link = r"C:\made-up\Programs\App.lnk";
        let mut os = FakeJournalOs {
            keys: vec![crate::arp::ARP_KEY.to_string()],
            task: Some("<Task registered and rewritten by the scheduler/>".into()),
            files: vec![(link.to_string(), digest(4))],
            ..FakeJournalOs::default()
        };
        os.values.insert(verb.clone(), RegData::Sz("ours".into()));
        os.values.insert(edit.clone(), RegData::Dword(2));
        let mut j = Journal::empty(Scope::Machine);
        let mut sink = VecSink::default();
        let run = j.begin_run();
        let changes = [
            (Kind::Install, arp_created()),
            (Kind::Install, task(None, digest(7))),
            (Kind::Install, Change::FileCreated { path: link.into(), after: digest(4) }),
            (
                Kind::Takeover(TakeoverId::FolderVerbs),
                Change::RegValue {
                    hive: Hive::Hklm,
                    key: key(&verb.0),
                    name: None,
                    before: None,
                    after: Some(RegData::Sz("ours".into())),
                },
            ),
            (
                Kind::UserEdit(journal::AppId::Tasks),
                Change::RegValue {
                    hive: Hive::Hklm,
                    key: key(&edit.0),
                    name: edit.1.clone(),
                    before: Some(RegData::Dword(1)),
                    after: Some(RegData::Dword(2)),
                },
            ),
        ];
        for (kind, change) in changes {
            journal::record(&mut j, &mut sink, run, kind, change, 1).unwrap();
        }
        let r = undo_suite_entries(&mut j, &mut sink, &mut os);
        // The ARP key, the Start entry and the takeover are restored; the scheduler's own copy of the task XML is
        // never the one we wrote, so the task is left to the explicit removal that follows.
        assert_eq!(r.restored, 3, "{r:?}");
        assert_eq!(r.changed_by_other, [Target::Task(TASK_JOURNAL_NAME.into())]);
        assert!(r.failed.is_empty());
        assert!(os.keys.is_empty() && os.files.is_empty() && !os.values.contains_key(&verb));
        assert_eq!(os.values.get(&edit), Some(&RegData::Dword(2)), "a change the user made through Tasks stays");
        // On disk: only the user's change is still open.
        let reread = Journal::parse(Scope::Machine, &sink.0).unwrap();
        let open: Vec<Kind> = reread.open_entries().into_iter().map(|e| e.kind.clone()).collect();
        assert_eq!(open, [Kind::UserEdit(journal::AppId::Tasks)]);
    }

    /// The machine journal as 1.8.0 wrote it when it was installed over an older version: the ARP key as created and
    /// the logon task with no before, although both were there; then the Start entries of the first suite sync, and a
    /// takeover. The lines are the frozen format's, crc included.
    const JOURNAL_1_8_0: [&str; 5] = [
        r#"528a976d {"r":"entry","v":1,"seq":1,"run":1,"ts":1795000000,"kind":"install","change":{"t":"reg_key_created","hive":"hklm","key":"SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\mini-oled-dock"}}"#,
        r#"fe779772 {"r":"entry","v":1,"seq":2,"run":1,"ts":1795000000,"kind":"install","change":{"t":"task","name":"\\Dock","before_xml":null,"after":"0707070707070707070707070707070707070707070707070707070707070707"}}"#,
        r#"000abc4f {"r":"entry","v":1,"seq":3,"run":2,"ts":1795000066,"kind":"install","change":{"t":"file_created","path":"C:\\made-up\\Programs\\Tasks.lnk","after":"0404040404040404040404040404040404040404040404040404040404040404"}}"#,
        r#"5f9abd50 {"r":"entry","v":1,"seq":4,"run":2,"ts":1795000066,"kind":"install","change":{"t":"file_created","path":"C:\\made-up\\Programs\\Folio.lnk","after":"0505050505050505050505050505050505050505050505050505050505050505"}}"#,
        r#"37007203 {"r":"entry","v":1,"seq":5,"run":3,"ts":1795000100,"kind":"takeover.folder_verbs","change":{"t":"reg_value","hive":"hklm","key":"SOFTWARE\\Classes\\Folder\\shell\\open\\command","name":null,"before":null,"after":{"t":"sz","s":"ours"}}}"#,
    ];

    const FOLDER_VERB: &str = r"SOFTWARE\Classes\Folder\shell\open\command";

    /// The machine a 1.8.0 journal describes: the ARP key, the logon task (its XML one the entry's after names: the
    /// fake digests by length, so the task counts as ours), the two Start entries and the takeover's value.
    fn machine_of_1_8_0() -> FakeJournalOs {
        let mut os = FakeJournalOs {
            keys: vec![crate::arp::ARP_KEY.to_string()],
            task: Some("<seven>".into()),
            files: vec![
                (r"C:\made-up\Programs\Tasks.lnk".into(), digest(4)),
                (r"C:\made-up\Programs\Folio.lnk".into(), digest(5)),
            ],
            ..FakeJournalOs::default()
        };
        os.values.insert((FOLDER_VERB.into(), None), RegData::Sz("ours".into()));
        os
    }

    #[test]
    fn undo_all_leaves_the_install_of_a_1_8_0_journal() {
        let written: String = JOURNAL_1_8_0.iter().map(|l| format!("{l}\n")).collect();
        let mut j = Journal::parse(Scope::Machine, written.as_bytes()).unwrap();
        // What 1.8.0 wrote is what the frozen encoder writes, line for line.
        let again: Vec<u8> = j.records().iter().flat_map(|r| Journal::encode(r).unwrap()).collect();
        assert_eq!(again, written.as_bytes());
        let mut os = machine_of_1_8_0();
        let mut sink = VecSink::default();
        // Settings > Safety's Undo all, --recover and --undo-user undo through this helper and no other
        // (only_the_uninstall_undoes_the_install).
        let r = crate::journal_ui::undo_takeovers(&mut j, &mut sink, &mut os);
        assert_eq!((r.restored, r.already, r.changed_by_other.len(), r.failed.len()), (1, 0, 0, 0), "{r:?}");
        assert!(!os.values.contains_key(&(FOLDER_VERB.to_string(), None)), "the takeover is undone");
        assert_eq!(os.keys, [crate::arp::ARP_KEY], "the ARP entry stays");
        assert_eq!(os.task.as_deref(), Some("<seven>"), "the logon task stays");
        assert_eq!(os.files.len(), 2, "the Start entries stay");
        let mut disk = written.into_bytes();
        disk.extend_from_slice(&sink.0);
        let reread = Journal::parse(Scope::Machine, &disk).unwrap();
        let open: Vec<u64> = reread.open_entries().iter().map(|e| e.seq).collect();
        assert_eq!(open, [1, 2, 3, 4], "the install's entries stay open, for the uninstall");
        // The uninstall undoes them, and deletes the task and the ARP entry right after, whatever the journal says.
        let r = undo_suite_entries(&mut j, &mut sink, &mut os);
        assert_eq!((r.restored, r.failed.len()), (4, 0), "{r:?}");
        assert!(os.keys.is_empty() && os.task.is_none() && os.files.is_empty());
    }

    #[test]
    fn the_next_install_over_a_1_8_0_journal_claims_nothing_it_found() {
        let written: String = JOURNAL_1_8_0.iter().map(|l| format!("{l}\n")).collect();
        let j = Journal::parse(Scope::Machine, written.as_bytes()).unwrap();
        let open: Vec<Entry> = j.open_entries().into_iter().filter(|e| e.kind == Kind::Install).cloned().collect();
        let installed = found(true, Some("<seven>"));
        // The same task XML: nothing new. The 1.8.0 entries stay as they are (no record tells a true one from a false
        // one; only the uninstall undoes them, and it removes the task and the key anyway).
        let plan = super::install_journal_plan;
        assert!(plan(&open, &installed, digest(7)).is_empty());
        // A new XML: the task as found is the new entry's before, never the 1.8.0 entry's "none"; the key is not
        // claimed again.
        assert_eq!(plan(&open, &installed, digest(9)), [task(Some("<seven>"), digest(9))]);
        // The same through the install path.
        let mut os = FakeInstall { open, found: Some(installed), ..FakeInstall::default() };
        assert!(enable_with(&mut os, false).error.is_none());
        assert!(os.recorded.is_empty(), "{:?}", os.recorded);
    }

    /// The name of the function whose code holds `code[at]` (the last `fn ` before it).
    fn enclosing_fn(code: &str, at: usize) -> &str {
        let start = code[..at].rfind("fn ").map_or(0, |i| i + 3);
        let rest = &code[start..];
        &rest[..rest.find(|c: char| !(c.is_alphanumeric() || c == '_')).unwrap_or(rest.len())]
    }

    /// Every undo the dock runs is one of three, and only the uninstall's takes the Install class (the logon task, the
    /// ARP entry and the Start entries of the installed dock): Settings > Safety's Undo all, --recover and --undo-user
    /// undo takeovers only. So no install entry, a 1.8.0 one that calls a task or key it found "created" included, is
    /// undone while the dock stays installed.
    #[test]
    fn only_the_uninstall_undoes_the_install() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = Vec::new();
        rust_files(&root.join("src"), &mut files);
        let test_only: Vec<PathBuf> =
            files.iter().flat_map(|f| test_only_modules(f, &std::fs::read_to_string(f).unwrap())).collect();
        let (mut sets, mut callers) = (Vec::new(), Vec::new());
        for f in files.iter().filter(|f| !test_only.contains(f)) {
            let src = std::fs::read_to_string(f).unwrap();
            let code = &src[..src.len() - test_part(&src).len()];
            let rel = f.strip_prefix(root).unwrap().display().to_string().replace('\\', "/");
            for (i, _) in code.match_indices("UndoSet::") {
                sets.push(format!("{rel}: {})", code[i..].split(')').next().unwrap_or_default()));
            }
            for p in ["undo_suite_entries(", concat!("undo_machine", "_suite(")] {
                callers.extend(code.match_indices(p).map(|(i, _)| format!("{rel}: {p} in {}", enclosing_fn(code, i))));
            }
        }
        sets.sort();
        assert_eq!(
            sets,
            [
                "src/autostart.rs: UndoSet::Classes(vec![KindClass::Takeover, KindClass::Install])",
                "src/journal_ui.rs: UndoSet::Classes(vec![KindClass::Takeover])",
                "src/update/suite.rs: UndoSet::Run(run)",
            ],
            "a new undo: keep the Install class to the uninstall"
        );
        // The Install class is reached from the uninstall's step and nowhere else.
        let step = concat!("undo_machine", "_suite(");
        assert_eq!(
            callers,
            [
                "src/autostart.rs: undo_suite_entries( in undo_suite_entries".to_string(),
                format!("src/autostart.rs: undo_suite_entries( in {}", &step[..step.len() - 1]),
                format!("src/autostart.rs: {step} in {}", &step[..step.len() - 1]),
                format!("src/autostart.rs: {step} in run_uninstall_step"),
            ]
        );
        // The update journal's undo is its own file's (update.jnl), never the machine journal.
        let suite = std::fs::read_to_string(root.join("src").join("update").join("suite.rs")).unwrap();
        let at = suite.find("UndoSet::Run(run)").unwrap();
        assert!(suite[..at].rfind("FolderOs").is_some_and(|f| at - f < 200), "update.jnl's undo");
    }

    /// The code of a file that only tests compile: from its first `#[cfg(test)]` that starts a module (J1c's rule).
    fn test_part(src: &str) -> &str {
        let lines: Vec<&str> = src.split_inclusive('\n').collect();
        let mut at = 0;
        for (i, line) in lines.iter().enumerate() {
            if line.trim_end() == "#[cfg(test)]" {
                let next = lines[i + 1..].iter().find(|l| !l.trim_start().starts_with("#["));
                if next.is_some_and(|l| l.contains("mod ") && l.trim_end().ends_with('{')) {
                    return &src[at..];
                }
            }
            at += line.len();
        }
        ""
    }

    /// The files of the `mod <name>;` items `src` (the file at `path`) declares under `#[cfg(test)]`: test code whole.
    fn test_only_modules(path: &Path, src: &str) -> Vec<PathBuf> {
        let dir = match path.file_name().and_then(|n| n.to_str()) {
            Some("main.rs" | "lib.rs" | "mod.rs") => path.parent().unwrap().to_path_buf(),
            _ => path.with_extension(""),
        };
        let lines: Vec<&str> = src.lines().collect();
        let mut out = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            if line.trim() != "#[cfg(test)]" {
                continue;
            }
            let Some(next) = lines[i + 1..].iter().find(|l| !l.trim_start().starts_with("#[")) else { continue };
            let item = next.trim().trim_start_matches("pub(crate) ").trim_start_matches("pub ");
            if let Some(name) = item.strip_prefix("mod ").and_then(|r| r.strip_suffix(';')) {
                out.push(dir.join(format!("{name}.rs")));
            }
        }
        out
    }

    fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                rust_files(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }

    /// What acts on %ProgramFiles%, the "Dock" task, HKLM and the data root of the machine, or reaches the network: the
    /// installer's free functions, its private steps and the real side of InstallOs, FromStage and FromTrust. Each is
    /// written in two halves, so this list is no match of itself.
    const LIVE_INSTALLER: [&str; 17] = [
        concat!("en", "able("),
        concat!("enable_with(&mut Real", "InstallOs"),
        concat!("register", "_task("),
        concat!("run_install", "_cli("),
        concat!("run_install", "_from_cli("),
        concat!("run_uninstall", "_cli("),
        concat!("arp::", "write("),
        concat!("arp::", "delete("),
        concat!("RealInstall", "Os"),
        concat!("install", "_cli("),
        concat!("run_uninstall", "_step("),
        concat!("undo_machine", "_suite("),
        concat!("remove_suite", "_data("),
        concat!("DataFrom", "Stage"),
        concat!("dis", "able("),
        concat!("RealFrom", "Trust"),
        concat!("release_manifest", "_digest("),
    ];

    /// Where in `code` a live installer call stands: a pattern that is not the end of a longer name (`dis` and `able(`
    /// is another function), not a method (`.`, a fake's) and not a definition (`fn `, a fake's own method).
    fn live_calls(code: &str) -> Vec<usize> {
        let mut at = Vec::new();
        for p in LIVE_INSTALLER {
            for (i, _) in code.match_indices(p) {
                let before = &code[..i];
                let glued = before.ends_with(|c: char| c.is_alphanumeric() || c == '_' || c == '.');
                if !glued && !before.ends_with("fn ") {
                    at.push(i);
                }
            }
        }
        at
    }

    #[test]
    fn no_test_calls_the_live_installer() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let (mut src_files, mut test_files) = (Vec::new(), Vec::new());
        rust_files(&root.join("src"), &mut src_files);
        rust_files(&root.join("tests"), &mut test_files);
        let test_only: Vec<PathBuf> =
            src_files.iter().flat_map(|f| test_only_modules(f, &std::fs::read_to_string(f).unwrap())).collect();
        assert!(test_only.iter().any(|p| p.ends_with("freeze_tests.rs")), "test-only files: {test_only:?}");
        let mut found = Vec::new();
        let mut scanned = 0;
        for f in src_files.iter().chain(&test_files) {
            let src = std::fs::read_to_string(f).unwrap();
            let code = if test_files.contains(f) || test_only.contains(f) { src.as_str() } else { test_part(&src) };
            scanned += usize::from(!code.is_empty());
            let rel = f.strip_prefix(root).unwrap().display().to_string();
            found.extend(
                live_calls(code)
                    .into_iter()
                    .map(|i| format!("{rel}: {}", code[i..].chars().take(48).collect::<String>())),
            );
        }
        assert!(scanned > 50, "the test code of the package is read ({scanned} files)");
        assert!(found.is_empty(), "a test reaches the live installer: {found:#?}");
        // A call is seen however it is reached; a fake's method or a longer name is not a call.
        let calls = format!("fn t() {{ super::{}true); crate::{}d); }}", LIVE_INSTALLER[0], LIVE_INSTALLER[6]);
        assert_eq!(live_calls(&calls).len(), 2);
        let fakes =
            format!("fn {}x: &Path) {{ os.{}x); dis{}) }}", LIVE_INSTALLER[2], LIVE_INSTALLER[2], LIVE_INSTALLER[0]);
        assert!(live_calls(&fakes).is_empty());
        // The private steps too, and the task's removal however it is reached; a longer name is not the step.
        let (step, task) = (LIVE_INSTALLER[9], LIVE_INSTALLER[14]);
        let steps = format!("fn t() {{ {step}false, None); my_{step}false, None); super::{task}); x.{task}); }}");
        assert_eq!(live_calls(&steps).len(), 2);
    }

    // ------------------------------------------------------------------------- I1d: --install --from <folder>

    use oled_trust::manifest::suite::{encode_manifest, Component, Stage};

    /// A folder of a test under the temp folder, removed when dropped.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(what: &str) -> TempDir {
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos();
            let p = std::env::temp_dir().join(format!("dock-i1d-{what}-{}-{nanos}", std::process::id()));
            std::fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A stage in a temp folder.
    struct TempStage(PathBuf);

    impl FromStage for TempStage {
        fn copy_in(&mut self, name: &str, src: &Path) -> Result<(), String> {
            std::fs::copy(src, self.0.join(name)).map(|_| ()).map_err(|e| e.to_string())
        }
        fn write(&mut self, name: &str, bytes: &[u8]) -> Result<(), String> {
            std::fs::write(self.0.join(name), bytes).map_err(|e| e.to_string())
        }
        fn staged(&mut self, name: &str) -> Result<(u64, Digest), String> {
            let b = std::fs::read(self.0.join(name)).map_err(|e| e.to_string())?;
            Ok((b.len() as u64, oled_sys::sha256::sha256(&b)?))
        }
        fn remove(&mut self, name: &str) {
            let _ = std::fs::remove_file(self.0.join(name));
        }
    }

    /// The trust of a `--from` test: what the release answers, and the tags it was asked for.
    struct FakeTrust {
        release: Result<Digest, String>,
        tags: Vec<String>,
    }

    impl FromTrust for FakeTrust {
        fn release_manifest(&mut self, tag: &str) -> Result<Digest, String> {
            self.tags.push(tag.to_string());
            self.release.clone()
        }
    }

    /// The release publishes `text` as its suite.json.
    fn published(text: &str) -> FakeTrust {
        FakeTrust { release: Ok(oled_sys::sha256::sha256(text.as_bytes()).unwrap()), tags: Vec::new() }
    }

    /// The release cannot be reached.
    fn offline() -> FakeTrust {
        FakeTrust { release: Err("no network".into()), tags: Vec::new() }
    }

    /// `path`'s Zone.Identifier stream, the Mark of the Web.
    fn zone_mark(path: &Path) -> PathBuf {
        let mut s = path.as_os_str().to_owned();
        s.push(":Zone.Identifier");
        PathBuf::from(s)
    }

    #[test]
    fn a_staged_copy_loses_the_mark_of_the_web() {
        let t = TempDir::new("motw");
        let (src, staged) = (t.0.join("tasks.exe"), t.0.join("staged.exe"));
        std::fs::write(&src, b"tasks of the release").unwrap();
        std::fs::write(zone_mark(&src), "[ZoneTransfer]\r\nZoneId=3\r\n").unwrap();
        // The kernel's copy (CopyFileW, as copy_into_stage; CopyFileExW, as std::fs::copy) takes the stream along.
        std::fs::copy(&src, &staged).unwrap();
        assert!(zone_mark(&staged).exists(), "the copy carries the mark");
        drop_zone_mark(&staged).unwrap();
        assert_eq!(std::fs::metadata(zone_mark(&staged)).unwrap_err().kind(), std::io::ErrorKind::NotFound);
        assert_eq!(std::fs::read(&staged).unwrap(), b"tasks of the release");
        assert!(zone_mark(&src).exists(), "the folder's file is not touched");
        // A file without a mark is fine.
        drop_zone_mark(&staged).unwrap();
    }

    fn staged_names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> =
            std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    #[test]
    fn from_folder_stages_verified_copies() {
        let sha = |b: &[u8]| oled_sys::sha256::sha256(b).unwrap();
        let version = Version::current();
        let (dock, tasks, files) = (b"the running dock".to_vec(), b"tasks of the release".to_vec(), b"files".to_vec());
        let part = |id, bytes: &[u8], hash| Component {
            id,
            version,
            sha256: hash,
            size: bytes.len() as u64,
            stage: Stage::Beta,
        };
        // files.exe in the folder has the announced size but other bytes than the manifest names.
        let manifest = SuiteManifest {
            schema: 1,
            version,
            components: vec![
                part(ComponentId::Dock, &dock, sha(&dock)),
                part(ComponentId::Tasks, &tasks, sha(&tasks)),
                part(ComponentId::Files, &files, sha(b"FILES")),
                part(ComponentId::LangIt, b"lang", sha(b"lang")),
            ],
        };
        let text = encode_manifest(&manifest);
        let download = TempDir::new("download");
        for (name, bytes) in [
            ("dock.exe", &dock[..]),
            ("tasks.exe", &tasks[..]),
            ("files.exe", &files[..]),
            ("suite.json", text.as_bytes()),
        ] {
            std::fs::write(download.0.join(name), bytes).unwrap();
        }
        let stage_dir = TempDir::new("stage");
        let mut stage = TempStage(stage_dir.0.clone());
        let mut os = FakeInstall::default();
        let mut trust = published(&text);
        let (report, staged) =
            install_from_with(&mut os, &mut stage, &mut trust, &download.0, sha(&dock), version, true).unwrap();
        // The install path is enable_with's: the journal record before the copy.
        assert!(report.error.is_none(), "{:?}", report.error);
        assert_eq!(os.calls[..2], ["record", "copy_self"]);
        assert_eq!(staged.staged, [ComponentId::Tasks]);
        let refused: Vec<ComponentId> = staged.refused.iter().map(|(id, _)| *id).collect();
        assert_eq!(refused, [ComponentId::Files, ComponentId::LangIt], "{:?}", staged.refused);
        assert!(staged.refused[0].1.contains("SHA-256"), "{:?}", staged.refused);
        // tasks.exe verified in the stage; files.exe deleted from it; the manifest is the bytes checked.
        assert_eq!(staged_names(&stage_dir.0), ["suite.json", "tasks.exe"]);
        assert_eq!(std::fs::read(stage_dir.0.join("tasks.exe")).unwrap(), tasks);
        assert_eq!(std::fs::read(stage_dir.0.join("suite.json")).unwrap(), text.as_bytes());
        // A failed copy of the dock stages nothing.
        let empty = TempDir::new("stage-empty");
        let mut copy_fails = FakeInstall { copy: Some(CopyOutcome::Failed), ..FakeInstall::default() };
        let (r, s) = install_from_with(
            &mut copy_fails,
            &mut TempStage(empty.0.clone()),
            &mut trust,
            &download.0,
            sha(&dock),
            version,
            true,
        )
        .unwrap();
        assert_eq!(r.copy, CopyOutcome::Failed);
        assert!(s.staged.is_empty() && s.refused.is_empty() && staged_names(&empty.0).is_empty());
        // A suite.json for another dock.exe, or of another release, refuses the whole --from: nothing is done.
        let other = Version { patch: version.patch + 1, ..version };
        for (own, v) in [(sha(b"another dock"), version), (sha(&dock), other)] {
            let mut refused_os = FakeInstall::default();
            let r = install_from_with(
                &mut refused_os,
                &mut TempStage(empty.0.clone()),
                &mut trust,
                &download.0,
                own,
                v,
                true,
            );
            assert!(r.is_err());
            assert!(refused_os.calls.is_empty() && staged_names(&empty.0).is_empty());
        }
        // No suite.json in the folder: refused too.
        std::fs::remove_file(download.0.join("suite.json")).unwrap();
        assert!(folder_manifest(&download.0, sha(&dock), version, &mut trust).is_err());
    }

    /// A stage whose copy is followed at once by a swap of the folder's file: what the check reads is the stage's.
    struct SwapAfterCopy {
        inner: TempStage,
        evil: Vec<u8>,
    }

    impl FromStage for SwapAfterCopy {
        fn copy_in(&mut self, name: &str, src: &Path) -> Result<(), String> {
            self.inner.copy_in(name, src)?;
            std::fs::write(src, &self.evil).map_err(|e| e.to_string())
        }
        fn write(&mut self, name: &str, bytes: &[u8]) -> Result<(), String> {
            self.inner.write(name, bytes)
        }
        fn staged(&mut self, name: &str) -> Result<(u64, Digest), String> {
            self.inner.staged(name)
        }
        fn remove(&mut self, name: &str) {
            self.inner.remove(name)
        }
    }

    /// The manifest of a `--from` test: this dock and one tasks.exe.
    fn from_manifest(dock: &[u8], tasks: &[u8]) -> String {
        let sha = |b: &[u8]| oled_sys::sha256::sha256(b).unwrap();
        let version = Version::current();
        let part = |id, bytes: &[u8]| Component {
            id,
            version,
            sha256: sha(bytes),
            size: bytes.len() as u64,
            stage: Stage::Beta,
        };
        encode_manifest(&SuiteManifest {
            schema: 1,
            version,
            components: vec![part(ComponentId::Dock, dock), part(ComponentId::Tasks, tasks)],
        })
    }

    /// SEC-4 (1), (3): files swapped in the folder after the check, during the install or right after the copy into the
    /// stage, change nothing that is staged: suite.json is never read again and each copy is verified in the stage.
    #[test]
    fn a_swap_between_check_and_staging_changes_nothing_staged() {
        let sha = |b: &[u8]| oled_sys::sha256::sha256(b).unwrap();
        let version = Version::current();
        let (dock, tasks, evil) = (b"the running dock".to_vec(), b"tasks of the release".to_vec(), b"EVIL".repeat(5));
        let text = from_manifest(&dock, &tasks);
        let forged = from_manifest(&dock, &evil);
        let download = TempDir::new("swap");
        let write_folder = |dir: &Path, manifest: &str, tasks: &[u8]| {
            std::fs::write(dir.join("suite.json"), manifest).unwrap();
            std::fs::write(dir.join("tasks.exe"), tasks).unwrap();
        };
        write_folder(&download.0, &text, &tasks);
        // During the install (after the check, before the staging) the folder becomes a forged suite.json and the
        // tasks.exe it names, of the size the checked one gives: only the hash tells them apart.
        let (dir, f, e) = (download.0.clone(), forged.clone(), evil.clone());
        assert_eq!(e.len(), tasks.len());
        let mut os =
            FakeInstall { on_copy: Some(Box::new(move || write_folder(&dir, &f, &e))), ..FakeInstall::default() };
        let stage_dir = TempDir::new("swap-stage");
        let mut trust = published(&text);
        let (report, staged) = install_from_with(
            &mut os,
            &mut TempStage(stage_dir.0.clone()),
            &mut trust,
            &download.0,
            sha(&dock),
            version,
            true,
        )
        .unwrap();
        assert!(report.error.is_none(), "{:?}", report.error);
        assert!(staged.staged.is_empty(), "{:?}", staged.staged);
        assert_eq!(staged.refused.len(), 1);
        assert!(staged.refused[0].1.contains("SHA-256"), "{:?}", staged.refused);
        assert_eq!(staged_names(&stage_dir.0), ["suite.json"], "the swapped tasks.exe is not staged");
        assert_eq!(std::fs::read(stage_dir.0.join("suite.json")).unwrap(), text.as_bytes(), "the bytes checked");
        // A swap right after the copy into the stage: the stage's copy is the one verified, and it is the release's.
        write_folder(&download.0, &text, &tasks);
        let after = TempDir::new("swap-after");
        let mut stage = SwapAfterCopy { inner: TempStage(after.0.clone()), evil: evil.clone() };
        let (_, staged) = install_from_with(
            &mut FakeInstall::default(),
            &mut stage,
            &mut trust,
            &download.0,
            sha(&dock),
            version,
            true,
        )
        .unwrap();
        assert_eq!(staged.staged, [ComponentId::Tasks]);
        assert_eq!(std::fs::read(download.0.join("tasks.exe")).unwrap(), evil, "the folder's file was swapped");
        assert_eq!(std::fs::read(after.0.join("tasks.exe")).unwrap(), tasks, "the stage keeps the verified copy");
    }

    /// SEC-4 (1): the dock.exe `--from` installs is checked where it was copied (the install folder), before any
    /// replace: the file at the running image's path may have been swapped after the check.
    #[test]
    fn the_dock_copy_is_checked_before_it_replaces_anything() {
        let sha = |b: &[u8]| oled_sys::sha256::sha256(b).unwrap();
        let t = TempDir::new("copy");
        let new = t.0.join("dock.exe.new");
        std::fs::write(&new, b"the release's dock").unwrap();
        assert_eq!(copy_matches(&new, sha(b"the release's dock")), Ok(()));
        let e = copy_matches(&new, sha(b"the running dock, swapped")).unwrap_err();
        assert!(e.contains("SHA-256") && e.contains("nothing replaced"), "{e}");
        assert!(copy_matches(&t.0.join("missing.exe"), sha(b"x")).is_err());
        // copy_self checks the copy before its first replace (a source test: copy_self is the live installer). A CRLF
        // checkout gives the file CRLF line ends.
        let src = include_str!("autostart.rs").replace("\r\n", "\n");
        let body = &src[src.find("fn copy_self(dest").unwrap()..];
        let body = &body[..body.find("\n}\n").unwrap()];
        let (check, replace) =
            (body.find("copy_matches(&new, w)").unwrap(), body.find("replace_file(&new, dest)").unwrap());
        assert!(check < replace, "the check comes before the replace");
        // And `--install --from` hands the copy the running image's digest: with `want: None` it is not checked.
        let cli = &src[src.find(concat!("fn install", "_cli(no_relaunch")).unwrap()..];
        let cli = &cli[..cli.find("\n}\n").unwrap()];
        assert!(cli.contains(concat!("RealInstall", "Os { want: Some(own) }")), "the --from install checks its copy");
        // And it asks the release in this process (SEC-4 review): an `--update-fetch` child would be the exe at
        // current_exe's path, which a junction in a folder its user can change turns to another file between the check
        // of that path and the start, and that file would run with the installer's token.
        let digest = &src[src.find(concat!("fn release_manifest", "_digest(tag")).unwrap()..];
        let digest = &digest[..digest.find("\n}\n").unwrap()];
        assert!(digest.contains(concat!("fetch_release(Some(tag), ", "false)")), "the release is asked in-process");
        assert!(!digest.contains(concat!("fetch_release(Some(tag), ", "true)")), "no child is started for it");
    }

    /// Set in the child of `the_install_copies_only_the_running_image`, a copy of this test exe.
    const SWAP_CHILD: &str = "DOCK_TEST_SWAPPED_IMAGE";

    /// SEC-4 review: a plain `--install` (and `enable`, the logon switch) copies the running image and nothing else. A
    /// running exe can be renamed and another file put at the path `current_exe` still names, by whoever can change its
    /// folder (a download folder: every unelevated process of its user), between its start and the copy. The copy is
    /// checked against the digest of the running image, read through a handle on the file this process was mapped from,
    /// before anything is journaled, copied or replaced: a child, a copy of this test exe, renames its own exe and puts
    /// another file at its path, and gets no digest at all. The watchdog, which runs with the dock's token, starts the
    /// running image by its final path (`start_image`, `held_start_path`) and refuses that file the same way.
    #[test]
    fn the_install_copies_only_the_running_image() {
        let sha = |b: &[u8]| oled_sys::sha256::sha256(b).unwrap();
        let me = std::env::current_exe().unwrap();
        if std::env::var_os(SWAP_CHILD).is_some() {
            std::fs::rename(&me, me.with_extension("moved.exe")).unwrap();
            std::fs::write(&me, b"MZ planted").unwrap();
            let got = running_image_digest();
            assert_ne!(got, Ok(sha(b"MZ planted")), "the file put at the running image's path");
            assert!(got.as_ref().is_err_and(|e| e.contains("not the running")), "{got:?}");
            assert!(start_image().is_err(), "nor is it started (the watchdog)");
            return;
        }
        assert_eq!(running_image_digest(), Ok(sha(&std::fs::read(&me).unwrap())), "the running image itself");
        let real = std::fs::canonicalize(&me).unwrap();
        let start = start_image().map(|i| i.start_path());
        assert_eq!(start, Ok(Some(PathBuf::from(real.to_str().unwrap().trim_start_matches(r"\\?\")))));
        let t = TempDir::new("image");
        let copy = t.0.join("dock-test.exe");
        std::fs::copy(&me, &copy).unwrap();
        let out = std::process::Command::new(&copy)
            .args(["--exact", "autostart::tests::the_install_copies_only_the_running_image", "--test-threads=1"])
            .env(SWAP_CHILD, "1")
            .creation_flags(CREATE_NO_WINDOW.0)
            .output()
            .unwrap();
        let said = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success() && said.contains("1 passed"), "the child: {said}");
        // `enable` (every install but `--from`'s, which hands its own) reads the image before anything else.
        let src = include_str!("autostart.rs").replace("\r\n", "\n");
        let live = &src[..src.find("\nmod tests {").unwrap()];
        let body = &live[live.find(concat!("pub(crate) fn en", "able(allow_stop")).unwrap()..];
        let body = &body[..body.find("\n}\n").unwrap()];
        let (read, install) = (body.find("running_image_digest()").unwrap(), body.find("enable_with(").unwrap());
        assert!(read < install, "the image is read before anything is journaled or copied");
        assert!(!live.contains(concat!("want: ", "None")), "every install checks its copy");
        // The watchdog's path is the one the worker gives an elevated dock's children (`held_start_path`): a strip of
        // its own would start, through a mount folder or a name the Win32 parser changes, a file the image cannot pin.
        let path = &live[live.find("pub(crate) fn start_path(&self)").unwrap()..];
        let path = &path[..path.find("\n    }\n").unwrap()];
        assert!(path.contains("held_start_path(&self.file)"), "the watchdog starts by the worker's held_start_path");
    }

    /// SEC-4 (2), (3): a suite.json that names this very dock.exe proves nothing by itself. Against the release's digest a
    /// forged one is refused; offline, nothing can check it, and the install is refused with the reason and the way out.
    #[test]
    fn a_forged_manifest_naming_this_dock_is_refused() {
        let sha = |b: &[u8]| oled_sys::sha256::sha256(b).unwrap();
        let version = Version::current();
        let (dock, tasks, evil) = (b"the running dock".to_vec(), b"tasks of the release".to_vec(), b"EVIL".repeat(5));
        let (genuine, forged) = (from_manifest(&dock, &tasks), from_manifest(&dock, &evil));
        let download = TempDir::new("forged");
        std::fs::write(download.0.join("suite.json"), &forged).unwrap();
        std::fs::write(download.0.join("tasks.exe"), &evil).unwrap();
        let stage_dir = TempDir::new("forged-stage");
        let run = |trust: &mut FakeTrust| {
            let mut os = FakeInstall::default();
            let mut stage = TempStage(stage_dir.0.clone());
            let r = install_from_with(&mut os, &mut stage, trust, &download.0, sha(&dock), version, true);
            (r.map(|(_, s)| s), os.calls)
        };
        // The release publishes another suite.json: refused before anything is done.
        let mut trust = published(&genuine);
        let (r, calls) = run(&mut trust);
        let e = r.unwrap_err();
        assert!(e.contains("is not the one release") && e.contains("SHA-256"), "{e}");
        assert!(calls.is_empty() && staged_names(&stage_dir.0).is_empty());
        assert_eq!(trust.tags, [version.tag()]);
        // Offline: refused, nothing done, the reason and the way out said.
        let (r, calls) = run(&mut offline());
        let e = r.unwrap_err();
        for part in ["could not be checked against release", "no network", "connect"] {
            assert!(e.contains(part), "{part}: {e}");
        }
        assert!(calls.is_empty() && staged_names(&stage_dir.0).is_empty());
    }

    /// SEC-4 (2), (3): a folder its user can write (a temp folder) is refused when the release cannot be reached; the same
    /// folder is fine when suite.json is the release's.
    #[test]
    fn a_folder_users_can_write_is_refused_or_checked_against_the_release() {
        let sha = |b: &[u8]| oled_sys::sha256::sha256(b).unwrap();
        let version = Version::current();
        let (dock, tasks) = (b"the running dock".to_vec(), b"tasks of the release".to_vec());
        let text = from_manifest(&dock, &tasks);
        let download = TempDir::new("users");
        std::fs::write(download.0.join("suite.json"), &text).unwrap();
        std::fs::write(download.0.join("tasks.exe"), &tasks).unwrap();
        let stage_dir = TempDir::new("users-stage");
        let mut os = FakeInstall::default();
        let mut stage = TempStage(stage_dir.0.clone());
        let e =
            install_from_with(&mut os, &mut stage, &mut offline(), &download.0, sha(&dock), version, true).unwrap_err();
        assert!(e.contains("could not be checked against release") && e.contains("connect"), "{e}");
        assert!(os.calls.is_empty() && staged_names(&stage_dir.0).is_empty());
        // Checked against the release's digest instead, the same folder stages its verified copy.
        let mut trust = published(&text);
        let (_, s) = install_from_with(
            &mut FakeInstall::default(),
            &mut stage,
            &mut trust,
            &download.0,
            sha(&dock),
            version,
            true,
        )
        .unwrap();
        assert_eq!(s.staged, [ComponentId::Tasks]);
        assert_eq!(trust.tags, [version.tag()]);
    }

    /// SEC-4 (2): offline, `--from` is refused before its folder is read, whatever the folder: no folder's rights stand in
    /// for the release (a missing folder would otherwise fail on reading suite.json).
    #[test]
    fn offline_the_install_is_refused_before_the_folder_is_read() {
        let t = TempDir::new("offline");
        let version = Version::current();
        let mut trust = offline();
        let e = trusted_manifest(&t.0.join("not there"), version, &mut trust).unwrap_err();
        let tag = version.tag();
        let want = format!(
            "suite.json could not be checked against release {tag} (no network): connect to the internet and try again"
        );
        assert_eq!(e, want);
        assert_eq!(trust.tags, [tag]);
    }
}
