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

use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::ProcessStatus::EnumProcesses;
use windows::Win32::System::Threading::{GetCurrentProcessId, CREATE_NO_WINDOW};
use windows::Win32::UI::Shell::{FOLDERID_ProgramFiles, SHGetKnownFolderPath, KF_FLAG_DEFAULT};

use crate::log;

/// Exit codes of the CLI modes (`--install`, `--uninstall`, argument errors).
pub const EXIT_OK: i32 = 0;
pub const EXIT_FAILED: i32 = 1;
pub const EXIT_BAD_ARGS: i32 = 2;
/// The old instance could not be stopped (used by the installer rework).
#[allow(dead_code)]
pub const EXIT_STOP_FAILED: i32 = 3;

/// Name of the task in the scheduler.
const TASK_NAME: &str = "Dock";
/// Maximum wait for the old instance and its watchdog to release the exe (outlasts the watchdog's
/// `RESTORE_WAIT` for Explorer).
const STOP_WAIT: Duration = Duration::from_secs(35);
/// After the stop signal, retry for at least this long even if nothing of the old instance is seen.
const STOP_GRACE: Duration = Duration::from_secs(3);
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

/// Retry loop of `copy_self`, with the copy (sleep included), the `(instance_up, exe_in_use)` check
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

/// Copies the current exe to `dest`. If the destination is locked and `allow_stop` is set, asks
/// the running instance to exit (`signal_stop_to_instance`) and retries while `keep_waiting` says
/// so: up to `STOP_WAIT` (35 s) while the old instance or its watchdog still runs the exe.
/// Returns `true` if it had to stop an instance.
fn copy_self(dest: &Path, allow_stop: bool) -> Result<bool, String> {
    let me = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    if same_file(&me, dest) {
        return Ok(false); // already running from the installed copy
    }
    if let Some(d) = dest.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("creating {}: {e}", d.display()))?;
    }
    let first = match std::fs::copy(&me, dest) {
        Ok(_) => return Ok(false),
        Err(e) => e,
    };
    if !allow_stop {
        return Err(format!("copy to {} failed: {first}", dest.display()));
    }
    log!("autostart: copy failed ({first}), asking the running instance to exit");
    let stopped = crate::watchdog::signal_stop_to_instance();
    if !stopped {
        return Err(format!("copy to {} failed: {first}", dest.display()));
    }
    let t = Instant::now();
    let copy = || {
        std::thread::sleep(Duration::from_millis(250));
        std::fs::copy(&me, dest).map(|_| ())
    };
    let check = || (crate::watchdog::instance_running(), exe_in_use_by_other(dest));
    let Err((old_alive, last)) = retry_copy(copy, check, || t.elapsed()) else {
        return Ok(true);
    };
    // The instance was stopped but the copy did not succeed: restart the already installed version,
    // so the user is not left without a dock.
    if dest.exists() {
        relaunch(dest);
    }
    let why = if old_alive {
        format!("the old instance or its watchdog did not exit within {} s", STOP_WAIT.as_secs())
    } else {
        "the file is locked by something else".to_string()
    };
    Err(format!("copy to {} failed: {why}: {last}", dest.display()))
}

/// Installs the exe copy and registers the task. `allow_stop`: may stop an instance that locks
/// the destination (only from `--install`; the popup toggle must not close itself).
/// Returns `true` if an instance was stopped.
pub fn enable(allow_stop: bool) -> Result<bool, String> {
    let dest = installed_exe()?;
    let stopped = copy_self(&dest, allow_stop)?;
    register_task(&dest)?;
    log!("autostart: task '{TASK_NAME}' registered -> {}", dest.display());
    Ok(stopped)
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
        Ok(_) => crate::logger::say("Dock restarted from the installed copy."),
        Err(e) => crate::logger::say(&format!("Unable to restart the dock: {e}")),
    }
}

/// `--install`: returns the exit code.
pub fn run_install_cli() -> i32 {
    let dest = match installed_exe() {
        Ok(d) => d,
        Err(e) => {
            log!("autostart: --install failed: {e}");
            crate::logger::say(&format!("Installation failed: {e}"));
            return EXIT_FAILED;
        }
    };
    match enable(true) {
        Ok(stopped) => {
            crate::logger::say(&format!("Installed to {}", dest.display()));
            crate::logger::say("Automatic start at logon enabled (scheduled task 'Dock', elevated).");
            report_legacy_copy();
            if stopped {
                relaunch(&dest);
            }
            EXIT_OK
        }
        Err(e) => {
            log!("autostart: --install failed: {e}");
            crate::logger::say(&format!("Installation failed: {e}"));
            EXIT_FAILED
        }
    }
}

/// `--uninstall`: returns the exit code.
pub fn run_uninstall_cli() -> i32 {
    match disable() {
        Ok(()) => {
            let where_ = installed_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| INSTALL_DIR_NAME.into());
            crate::logger::say(&format!(
                "Automatic start removed. The installed copy {where_} is kept (it may be running); \
                 delete its folder from an elevated prompt if you no longer need it."
            ));
            EXIT_OK
        }
        Err(e) => {
            log!("autostart: --uninstall failed: {e}");
            crate::logger::say(&format!("Removal failed: {e}"));
            EXIT_FAILED
        }
    }
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
}
