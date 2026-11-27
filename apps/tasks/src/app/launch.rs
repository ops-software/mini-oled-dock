//! The only way UI code starts a process (house rule 1): Windows' Task Manager, a Reopen and the restart after an
//! update go through a Launcher. app::main installs the Win32 one; every Harness test and scene records instead.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use oled_shellmodel::startmenu::system_dir;
use oled_sys::pipe::Integrity;
use windows::core::{w, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, ERROR_PATH_NOT_FOUND};
use windows::Win32::System::Threading::{CreateProcessW, CREATE_NO_WINDOW, PROCESS_INFORMATION, STARTUPINFOW};
use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use super::access::{Prober, Win32Prober};

/// Err holds a Win32 or HRESULT code for the log.
pub trait Launcher: Send + Sync {
    /// taskmgr_file (never a bare name): CreateProcessW from an elevated Tasks (taskmgr_direct), else ShellExecuteExW,
    /// verb open, SW_SHOWNORMAL (Windows elevates it itself).
    fn open_taskmgr(&self) -> Result<(), u32>;
    /// ShellExecuteExW(launch, SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI); COM STA on the caller.
    fn open_launch(&self, launch: &str) -> Result<(), u32>;
    /// CreateProcessW(current_exe, no arguments).
    fn restart_self(&self) -> Result<(), u32>;
}

/// The real launcher; installed only by app::main.
pub struct Win32Launcher;

impl Launcher for Win32Launcher {
    fn open_taskmgr(&self) -> Result<(), u32> {
        let file = taskmgr_file(&system_dir().ok_or(ERROR_PATH_NOT_FOUND.0)?);
        let file = HSTRING::from(file.as_os_str());
        if taskmgr_direct(Win32Prober.own_level()) {
            create_process(&file)
        } else {
            shell_execute(&file, SEE_MASK_NOASYNC)
        }
    }

    fn open_launch(&self, launch: &str) -> Result<(), u32> {
        shell_execute(&HSTRING::from(launch), SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI)
    }

    fn restart_self(&self) -> Result<(), u32> {
        let exe = std::env::current_exe().map_err(|e| e.raw_os_error().map_or(1, |c| c as u32))?;
        create_process(&HSTRING::from(exe.as_os_str()))
    }
}

/// CreateProcessW(app, no arguments).
fn create_process(app: &HSTRING) -> Result<(), u32> {
    // The command line is the quoted path alone; CreateProcessW may write into it, so it is a buffer of ours.
    let mut line: Vec<u16> =
        std::iter::once(u16::from(b'"')).chain(app.iter().copied()).chain([u16::from(b'"'), 0]).collect();
    let si = STARTUPINFOW { cb: std::mem::size_of::<STARTUPINFOW>() as u32, ..Default::default() };
    let mut pi = PROCESS_INFORMATION::default();
    // CREATE_NO_WINDOW is ignored for a program of the windows subsystem (tasks.exe and Task Manager are): it is passed
    // so that every process the suite starts carries it.
    // SAFETY: app and line are NUL-terminated and live for the call; si and pi are valid for it.
    unsafe {
        CreateProcessW(
            PCWSTR(app.as_ptr()),
            Some(PWSTR(line.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_NO_WINDOW,
            None,
            PCWSTR::null(),
            &si,
            &mut pi,
        )
    }
    .map_err(|e| e.code().0 as u32)?;
    // SAFETY: both handles came from the successful CreateProcessW above and are closed once.
    unsafe {
        let _ = CloseHandle(pi.hThread);
        let _ = CloseHandle(pi.hProcess);
    }
    Ok(())
}

/// Windows' Task Manager: the file of `system_dir` (GetSystemDirectoryW), never the bare name, which ShellExecuteExW
/// would look for first in the working directory Tasks inherited from its starter, then along PATH.
pub fn taskmgr_file(system_dir: &Path) -> PathBuf {
    system_dir.join("Taskmgr.exe")
}

/// Whether a Tasks at `own` starts Task Manager with CreateProcessW: from High or System its manifest's
/// highestAvailable is met already and no per-user association of the shell is read on the way; from any other level,
/// or an unknown one, the shell starts it and shows Windows' elevation prompt.
pub fn taskmgr_direct(own: Option<Integrity>) -> bool {
    matches!(own, Some(Integrity::High | Integrity::System))
}

/// ShellExecuteExW(file) with the verb open and SW_SHOWNORMAL.
fn shell_execute(file: &HSTRING, mask: u32) -> Result<(), u32> {
    let mut sei = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: mask,
        lpVerb: w!("open"),
        lpFile: PCWSTR(file.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: sei carries its cbSize; the strings (a NUL-terminated HSTRING and a static) live for the call.
    unsafe { ShellExecuteExW(&mut sei) }.map_err(|e| e.code().0 as u32)
}

/// What a RecordingLauncher was asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchReq {
    TaskMgr,
    Launch(String),
    RestartSelf,
}

/// Records every request and starts nothing; returns Ok(()). Every test and scene uses it.
#[derive(Default)]
pub struct RecordingLauncher {
    pub reqs: Mutex<Vec<LaunchReq>>,
}

impl RecordingLauncher {
    fn push(&self, r: LaunchReq) {
        self.reqs.lock().unwrap_or_else(|p| p.into_inner()).push(r);
    }
}

impl Launcher for RecordingLauncher {
    fn open_taskmgr(&self) -> Result<(), u32> {
        self.push(LaunchReq::TaskMgr);
        Ok(())
    }

    fn open_launch(&self, launch: &str) -> Result<(), u32> {
        self.push(LaunchReq::Launch(launch.to_string()));
        Ok(())
    }

    fn restart_self(&self) -> Result<(), u32> {
        self.push(LaunchReq::RestartSelf);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_launcher_records_in_order() {
        let l = RecordingLauncher::default();
        assert_eq!(l.open_taskmgr(), Ok(()));
        assert_eq!(l.open_launch("shell:AppsFolder\\Orbit.App"), Ok(()));
        assert_eq!(l.restart_self(), Ok(()));
        let reqs = l.reqs.lock().unwrap().clone();
        assert_eq!(
            reqs,
            [LaunchReq::TaskMgr, LaunchReq::Launch("shell:AppsFolder\\Orbit.App".into()), LaunchReq::RestartSelf]
        );
    }

    /// Task Manager is the file of the system directory given, never a bare name that the shell would look up in the
    /// working directory first.
    #[test]
    fn taskmgr_is_the_file_of_the_system_directory() {
        let sys = Path::new(r"D:\OS\System32");
        let file = taskmgr_file(sys);
        assert!(file.is_absolute(), "{file:?}");
        assert_eq!(file.parent(), Some(sys));
        assert!(file.file_name().is_some_and(|n| n.eq_ignore_ascii_case("taskmgr.exe")), "{file:?}");
    }

    /// Only an elevated Tasks starts Task Manager itself; any other, or one that cannot read its own level, goes
    /// through the shell and its elevation prompt.
    #[test]
    fn only_an_elevated_tasks_starts_taskmgr_itself() {
        assert!(taskmgr_direct(Some(Integrity::High)) && taskmgr_direct(Some(Integrity::System)));
        for own in [None, Some(Integrity::Untrusted), Some(Integrity::Low), Some(Integrity::Medium)] {
            assert!(!taskmgr_direct(own), "{own:?}");
        }
    }
}
