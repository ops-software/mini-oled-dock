//! A window whose WM_CLOSE asks to save (the Close smoke test and LIVE-T3): a plain top-level window of class
//! MiniOledTesteeClose; Close shows "Save changes?" (Yes, No, Cancel) and the window goes only on Yes or No.
//!
//! testee_close [--secs N]   N in 1..=300, default 60: the process exits by itself after N seconds, dialog or not.

use std::time::Duration;

const DEFAULT_SECS: u64 = 60;
const MAX_SECS: u64 = 300;

#[derive(Debug, PartialEq, Eq)]
enum ArgError {
    Unknown(String),
    MissingValue,
    BadValue(String),
}

/// The deadline in seconds.
fn parse_args(args: &[String]) -> Result<u64, ArgError> {
    let mut secs = DEFAULT_SECS;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--secs" => {
                let v = it.next().ok_or(ArgError::MissingValue)?;
                let n: u64 = v.parse().map_err(|_| ArgError::BadValue(v.clone()))?;
                secs = n.clamp(1, MAX_SECS);
            }
            other => return Err(ArgError::Unknown(other.to_string())),
        }
    }
    Ok(secs)
}

#[cfg(windows)]
mod os {
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, MessageBoxW, PostQuitMessage,
        RegisterClassW, TranslateMessage, CW_USEDEFAULT, IDNO, IDYES, MB_YESNOCANCEL, MSG, WINDOW_EX_STYLE, WM_CLOSE,
        WM_DESTROY, WNDCLASSW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
    };

    extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        match msg {
            WM_CLOSE => {
                // SAFETY: hwnd is this window; the strings are static.
                let answer =
                    unsafe { MessageBoxW(Some(hwnd), w!("Save changes?"), w!("testee_close"), MB_YESNOCANCEL) };
                if answer == IDYES || answer == IDNO {
                    // SAFETY: this thread created the window.
                    let _ = unsafe { DestroyWindow(hwnd) };
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                // SAFETY: no arguments but the exit code.
                unsafe { PostQuitMessage(0) };
                LRESULT(0)
            }
            // SAFETY: forwards the arguments the system gave.
            _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
        }
    }

    /// The window and its message loop, until the window is destroyed; false when it could not be made.
    pub fn run() -> bool {
        // SAFETY: plain calls on this thread with static strings; the class is registered once per process.
        unsafe {
            let Ok(hinst) = GetModuleHandleW(PCWSTR::null()) else { return false };
            let class = WNDCLASSW {
                lpfnWndProc: Some(wnd_proc),
                hInstance: hinst.into(),
                lpszClassName: w!("MiniOledTesteeClose"),
                ..Default::default()
            };
            if RegisterClassW(&class) == 0 {
                return false;
            }
            let made = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("MiniOledTesteeClose"),
                w!("Tasks testee (close)"),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                320,
                120,
                None,
                None,
                Some(hinst.into()),
                None,
            );
            if made.is_err() {
                return false;
            }
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        true
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let secs = match parse_args(&args) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("testee_close: {e:?}");
            std::process::exit(2)
        }
    };
    // The deadline holds even while the dialog is up.
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(secs));
        std::process::exit(0)
    });
    #[cfg(windows)]
    let code = if os::run() { 0 } else { 1 };
    #[cfg(not(windows))]
    let code = 1;
    std::process::exit(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn secs_default_and_caps() {
        assert_eq!(parse_args(&a("")), Ok(60));
        assert_eq!(parse_args(&a("--secs 5")), Ok(5));
        assert_eq!(parse_args(&a("--secs 301")), Ok(300));
        assert_eq!(parse_args(&a("--secs 0")), Ok(1), "it always exits by itself, but not at once");
    }

    #[test]
    fn bad_arguments() {
        assert_eq!(parse_args(&a("--secs")), Err(ArgError::MissingValue));
        assert_eq!(parse_args(&a("--secs x")), Err(ArgError::BadValue("x".into())));
        assert_eq!(parse_args(&a("--tree 3")), Err(ArgError::Unknown("--tree".into())));
    }
}
