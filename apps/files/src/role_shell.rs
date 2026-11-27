//! files.exe --role=shell: the shell host (SH4a). It refuses a UAC-elevated token, turns off the system's error boxes
//! and Windows Error Reporting's UI before any extension can load (a crashing extension shows nothing on the user's
//! desktop), starts its log and hands over to shell::host.

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{
    GetTokenInformation, TokenElevationType, TokenElevationTypeFull, TOKEN_ELEVATION_TYPE, TOKEN_QUERY,
};
use windows::Win32::System::Diagnostics::Debug::{SetErrorMode, SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX};
use windows::Win32::System::ErrorReporting::{WerSetFlags, WER_FAULT_REPORTING, WER_FAULT_REPORTING_NO_UI};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use crate::exit;

/// `args` are the arguments after --role=shell. Returns the process exit code.
pub fn run(args: &[String]) -> i32 {
    if uac_elevated() {
        return exit::ELEVATED;
    }
    quiet_faults();
    let log = oled_sys::logger::start("", &crate::LOG_SHELL);
    let code = crate::shell::host::run(args);
    oled_sys::logger::stop(log);
    code
}

/// true for a UAC-elevated token (TokenElevationTypeFull): the helpers run de-elevated only.
pub fn uac_elevated() -> bool {
    let mut token = HANDLE::default();
    // SAFETY: this process's pseudo handle; `token` is writable and closed below.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.is_err() {
        return false;
    }
    let mut kind = TOKEN_ELEVATION_TYPE::default();
    let mut len = 0u32;
    // SAFETY: `kind` is as large as the size passed; `len` is writable.
    let read = unsafe {
        GetTokenInformation(
            token,
            TokenElevationType,
            Some(std::ptr::from_mut(&mut kind).cast()),
            std::mem::size_of::<TOKEN_ELEVATION_TYPE>() as u32,
            &mut len,
        )
    };
    // SAFETY: opened above, closed once.
    let _ = unsafe { CloseHandle(token) };
    read.is_ok() && kind == TokenElevationTypeFull
}

/// No critical-error box, no fault box and no Windows Error Reporting UI for this process.
fn quiet_faults() {
    // SAFETY: process-wide flags with no pointers.
    unsafe {
        SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX);
        if let Err(e) = WerSetFlags(WER_FAULT_REPORTING(WER_FAULT_REPORTING_NO_UI)) {
            oled_sys::log!("shell: WerSetFlags failed ({e})");
        }
    }
}
