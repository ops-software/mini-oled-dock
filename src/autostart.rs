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

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Storage::FileSystem::{
    MoveFileExW, FILE_ATTRIBUTE_REPARSE_POINT, MOVEFILE_DELAY_UNTIL_REBOOT, MOVEFILE_REPLACE_EXISTING,
    MOVEFILE_WRITE_THROUGH,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::ProcessStatus::EnumProcesses;
use windows::Win32::System::Threading::{GetCurrentProcessId, CREATE_NO_WINDOW};
use windows::Win32::UI::Shell::{FOLDERID_ProgramFiles, SHGetKnownFolderPath, KF_FLAG_DEFAULT};

use crate::log;

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
    // SAFETY: valid known-folder id, no token (current user); the returned buffer is freed below.
    let p = unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramFiles, KF_FLAG_DEFAULT, None) }
        .map_err(|e| format!("SHGetKnownFolderPath(ProgramFiles): {e}"))?;
    // SAFETY: `p` is the NUL-terminated string returned by SHGetKnownFolderPath, read once.
    let s = unsafe { p.to_string() };
    // SAFETY: same pointer, allocated with CoTaskMemAlloc, not used after this line.
    unsafe { CoTaskMemFree(Some(p.0 as *const std::ffi::c_void)) };
    let s = s.map_err(|e| format!("ProgramFiles path: {e}"))?;
    if s.trim().is_empty() {
        return Err("ProgramFiles path is empty".into());
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

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy()),
        _ => false,
    }
}

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
fn exe_in_use_by_other(dest: &Path) -> bool {
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

/// Copies the current exe to `dest` through `dock.exe.new` and a rename (`dest` is never written in
/// place). If the rename is refused and `allow_stop` is set, signals the running instance to exit
/// (`signal_stop_to_instance`; with no instance, e.g. after a crash, its watchdog may still hold the
/// exe) and retries while `keep_waiting` says so: up to `STOP_WAIT` (35 s) while the old instance or
/// its watchdog still runs the exe. Never relaunches: `run_install_cli` decides that.
fn copy_self(dest: &Path, allow_stop: bool) -> InstallReport {
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
    if let Err(e) = std::fs::copy(&me, &new) {
        let _ = std::fs::remove_file(&new);
        return fail(format!("copy to {} failed: {e}", new.display()));
    }
    let copied = |stop_signalled| InstallReport { copy: CopyOutcome::Copied, stop_signalled, error: None };
    let first = match replace_file(&new, dest) {
        Ok(()) => return copied(false),
        Err(e) => e,
    };
    if !allow_stop {
        let _ = std::fs::remove_file(&new);
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

/// Installs the exe copy and registers the task. `allow_stop`: may stop an instance that locks
/// the destination (only from `--install`; the popup toggle must not close itself).
/// Then writes the Add/Remove Programs entry (`arp::write`).
/// A task or ARP error after a good copy keeps `copy`, so the new exe is still relaunched.
pub(crate) fn enable(allow_stop: bool) -> InstallReport {
    let dest = match installed_exe() {
        Ok(d) => d,
        Err(e) => return InstallReport::failed(InstallError::Failed(e)),
    };
    let mut report = copy_self(&dest, allow_stop);
    if report.copy == CopyOutcome::Failed {
        return report;
    }
    match register_task(&dest) {
        Ok(()) => log!("autostart: task '{TASK_NAME}' registered -> {}", dest.display()),
        Err(e) => report.error = Some(InstallError::Failed(e)),
    }
    let arp = install_dir().and_then(|dir| crate::arp::write(&dir));
    match arp {
        Ok(()) => log!("autostart: Add/Remove Programs entry written"),
        Err(e) => {
            log!("autostart: Add/Remove Programs entry failed: {e}");
            // The first error wins (a task failure is reported before this one).
            report.error.get_or_insert(InstallError::Failed(format!("Add/Remove Programs entry: {e}")));
        }
    }
    report
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

/// `--install`: returns the exit code. Always starts the installed exe at the end (the new copy, or
/// the old one after a failed copy), unless `no_relaunch` or a dock is still running.
pub fn run_install_cli(no_relaunch: bool) -> i32 {
    let dest = match installed_exe() {
        Ok(d) => d,
        Err(e) => {
            log!("autostart: --install failed: {e}");
            crate::logger::say(&format!("Installation failed: {e}"));
            return EXIT_FAILED;
        }
    };
    let report = enable(true);
    // Sampled after `enable`: true only when re-run over its own running dock or the stop gave up.
    let still_running = crate::watchdog::instance_running();
    log!(
        "autostart: --install copy={:?} stop_signalled={} still_running={still_running} no_relaunch={no_relaunch}",
        report.copy,
        report.stop_signalled
    );
    match &report.error {
        None => {
            crate::logger::say(&format!("Installed to {}", dest.display()));
            crate::logger::say("Automatic start at logon enabled (scheduled task 'Dock', elevated).");
            report_legacy_copy();
        }
        Some(e) => {
            log!("autostart: --install failed: {e}");
            crate::logger::say(&format!("Installation failed: {e}"));
        }
    }
    if should_relaunch(still_running, no_relaunch) && dest.exists() {
        relaunch(&dest);
    }
    report.exit_code()
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
fn stop_and_wait(exe: &Path) -> Result<(), InstallError> {
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

/// The only files `--uninstall --remove` deletes from the install folder. Pure.
fn removal_targets(install_dir: &Path) -> [PathBuf; 3] {
    [install_dir.join("dock.exe"), install_dir.join("dock.exe.new"), install_dir.join("autostart-task.xml")]
}

/// Asks Windows to delete `path` (a file or an empty folder) at the next restart. Elevated only.
fn delete_at_reboot(path: &Path) -> Result<(), String> {
    let p = HSTRING::from(path.as_os_str());
    // SAFETY: `p` is NUL-terminated and lives for the call; a null new name means "delete".
    unsafe { MoveFileExW(&p, PCWSTR::null(), MOVEFILE_DELAY_UNTIL_REBOOT) }
        .map_err(|e| format!("scheduling the removal of {} at restart: {e}", path.display()))
}

/// Deletes `path`: `Ok(true)` deleted now (or already gone), `Ok(false)` scheduled for the next
/// restart (in practice only the exe of this very process, when started from Add/Remove Programs).
fn remove_or_schedule(path: &Path) -> Result<bool, String> {
    match retry_for(RELEASE_GRACE, RETRY_STEP, || std::fs::remove_file(path)) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(e) => {
            log!("autostart: deleting {} failed ({e}), scheduling it at restart", path.display());
            delete_at_reboot(path).map(|()| false)
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
    for p in removal_targets(dir) {
        if std::fs::symlink_metadata(&p).is_err() {
            continue;
        }
        match remove_or_schedule(&p) {
            Ok(now) => report.scheduled |= !now,
            Err(e) => report.failures.push(e),
        }
    }
    if let Err(e) = std::fs::remove_dir(dir) {
        log!("autostart: removing {} failed ({e}), scheduling it at restart", dir.display());
        match delete_at_reboot(dir) {
            Ok(()) => report.scheduled = true,
            Err(e) => report.failures.push(e),
        }
    }
}

/// `--uninstall`: removes the logon task and the Add/Remove Programs entry. With `remove` it first
/// stops the dock (the same wait as `--install`), restores the taskbar, then deletes the installed
/// files and the update staging folders. Returns the exit code.
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
    }
    report.failures.extend(disable().err());
    report.failures.extend(crate::arp::delete().err());
    if remove {
        if let Ok(d) = &dir {
            remove_install(d, &mut report);
        }
        report.failures.extend(crate::datadir::remove_update_dir().err().map(|e| format!("update folder: {e}")));
    }
    for f in &report.failures {
        log!("autostart: --uninstall: {f}");
        say(&format!("Removal failed: {f}"));
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
        let mut child = std::process::Command::new(&ping)
            .args(["-n", "8", "127.0.0.1"])
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
        assert_eq!(removal_targets(d), [d.join("dock.exe"), d.join("dock.exe.new"), d.join("autostart-task.xml")]);
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
}
