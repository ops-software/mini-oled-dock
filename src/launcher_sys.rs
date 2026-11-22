//! The launcher's port to Windows: what the session (`launcher::session`) and the chord flow (`popup_launcher`'s
//! controller) ask for, done with Win32, the popup host and the shell thread. `RealHook` and `RealSys` are the ports the
//! controller runs on in the dock (its tests run on recorders). The mapping from the session's launches to the shell
//! thread's requests (L1a-12i): every launch becomes one of the launcher's own requests, never the shared `Launch`,
//! `LaunchAppAdmin` or `RevealApp`, so only launcher launches push `LaunchFailed`; `ShellRequest::RunAsTyped` is built
//! here and nowhere else (lint 1).
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::sync::atomic::Ordering::Relaxed;

use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{
    MonitorFromPoint, MonitorFromWindow, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, VIRTUAL_KEY,
};
use windows::Win32::UI::Shell::{SHQueryUserNotificationState, QUNS_RUNNING_D3D_FULL_SCREEN};
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetForegroundWindow};

use crate::hook::{self, PassFlags};
use crate::launcher::session::LaunchReq;
use crate::log;
use crate::popup::popup_launcher::{HookPort, Launcher, MonitorPick, SysPort};
use crate::popup::{self, Centred, PopupKind};
use crate::shell_thread::ShellRequest;

/// The keyboard hook's side of the chord (hook.rs's statics).
pub(crate) struct RealHook;

impl HookPort for RealHook {
    fn ack(&mut self, seq: u32) {
        hook::ack_launcher(seq);
    }

    fn end(&mut self, seq: u32) {
        hook::end_typeahead(seq);
    }

    fn shown(&mut self, on: bool) {
        hook::set_launcher_shown(on);
    }

    fn pass(&mut self, flags: PassFlags) {
        hook::set_pass_flags(flags);
    }

    fn seqs(&self) -> (u32, u32) {
        (hook::LAUNCHER_SEQ.load(Relaxed), hook::LAST_FAILOPEN_SEQ.load(Relaxed))
    }

    fn alt_down(&self) -> bool {
        hook::alt_tracked_down()
    }
}

/// Windows and the popup host, for the controller (UI thread only).
pub(crate) struct RealSys;

/// The monitor `pick` names; the primary one when there is no better answer.
fn target_monitor(pick: MonitorPick) -> HMONITOR {
    let primary = POINT::default();
    // SAFETY: GetCursorPos writes the POINT it is given; the Monitor* calls accept any point or window (a null
    // foreground included) and fall back to the primary monitor.
    unsafe {
        match pick {
            MonitorPick::Pointer => {
                let mut pt = POINT::default();
                if GetCursorPos(&mut pt).is_ok() {
                    MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST)
                } else {
                    MonitorFromPoint(primary, MONITOR_DEFAULTTOPRIMARY)
                }
            }
            MonitorPick::Active => MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTOPRIMARY),
            MonitorPick::Primary => MonitorFromPoint(primary, MONITOR_DEFAULTTOPRIMARY),
        }
    }
}

/// The effective DPI of a monitor (96 when it cannot be read).
fn monitor_dpi(mon: HMONITOR) -> u32 {
    let (mut x, mut y) = (0u32, 0u32);
    // SAFETY: both out pointers are valid for the call; a stale monitor handle only makes it fail.
    match unsafe { GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut x, &mut y) } {
        Ok(()) if x >= 96 => x,
        _ => 96,
    }
}

impl SysPort for RealSys {
    fn now(&self) -> u32 {
        // SAFETY: no preconditions.
        unsafe { GetTickCount() }
    }

    fn d3d_fullscreen(&self) -> bool {
        // SAFETY: no preconditions; the state is returned by value.
        unsafe { SHQueryUserNotificationState() }.is_ok_and(|s| s == QUNS_RUNNING_D3D_FULL_SCREEN)
    }

    fn fullscreen_foreground(&self) -> bool {
        crate::ui::foreground_is_fullscreen()
    }

    fn launcher_open(&self) -> bool {
        popup::current() == Some(PopupKind::Launcher)
    }

    fn open(&mut self, pick: MonitorPick, top_percent: u8) -> bool {
        let mon = target_monitor(pick);
        let Some(mi) = crate::ui::monitor_info(mon) else {
            log!("launcher: no monitor to open on");
            return false;
        };
        popup::open_centred(PopupKind::Launcher, Centred { work: mi.rcWork, dpi: monitor_dpi(mon), top_percent })
    }

    fn close_launcher(&mut self) {
        popup::with_launcher(Launcher::chord_again);
    }

    fn inject(&mut self, keys: &[(u16, bool)]) {
        let inputs: Vec<INPUT> = keys
            .iter()
            .map(|&(vk, up)| INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VIRTUAL_KEY(vk),
                        wScan: 0,
                        dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                        time: 0,
                        dwExtraInfo: hook::INJECT_TAG,
                    },
                },
            })
            .collect();
        // SAFETY: a slice of initialized INPUTs; cbSize = size_of::<INPUT>() as required. The hook passes tagged events.
        let n = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
        if n as usize != inputs.len() {
            let count = inputs.len();
            log!("launcher: SendInput accepted {n} of {count} events");
        }
    }
}

/// The shell request of one launch. `setting` gives the target and arguments of a held setting
/// (`LaunchReq::Setting` carries only its index; the controller holds the list). `None`: no such setting,
/// or no Windows folder for the registry row; nothing is launched.
#[allow(dead_code)] // the chord flow's port calls it (L1a-15)
pub(crate) fn to_request(
    r: LaunchReq,
    setting: &dyn Fn(u32) -> Option<(String, Option<String>)>,
) -> Option<ShellRequest> {
    Some(match r {
        // The target Start builds for a catalogue app (popup_startmenu `after_close`).
        LaunchReq::App(id) => ShellRequest::LauncherLaunch { target: format!("shell:AppsFolder\\{id}"), args: None },
        LaunchReq::AppAdmin(id) => ShellRequest::LauncherAppAdmin { id: id.into() },
        LaunchReq::RevealApp(id) => ShellRequest::LauncherRevealApp { id: id.into() },
        LaunchReq::Setting { index } => {
            let (target, args) = setting(index)?;
            ShellRequest::LauncherLaunch { target, args }
        }
        LaunchReq::OpenPath(path) => ShellRequest::OpenPath { path: path.into() },
        LaunchReq::RevealPath(path) => ShellRequest::RevealPath { path: path.into() },
        LaunchReq::Command(r) => ShellRequest::LaunchCommand { line: r.line.into(), file: r.file, console: false },
        LaunchReq::Console(r) => ShellRequest::LaunchCommand { line: r.line.into(), file: r.file, console: true },
        LaunchReq::RunAsTyped { line, gesture } => {
            ShellRequest::RunAsTyped { line: line.line.into(), file: line.file, gesture }
        }
        LaunchReq::Url(url) => ShellRequest::OpenUrl { url: url.into() },
        LaunchReq::Registry => {
            let windows = crate::shell_thread::system_dir()?.parent()?.join("regedit.exe");
            ShellRequest::LauncherLaunch { target: windows.to_string_lossy().into_owned(), args: None }
        }
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::launcher::gesture::Gesture;
    use crate::launcher::runline;

    const CALC: &str = "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App";
    const SETTINGS: &str =
        "shell:AppsFolder\\windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel";

    /// The controller's held settings: one, at index 1.
    fn setting(i: u32) -> Option<(String, Option<String>)> {
        (i == 1).then(|| (SETTINGS.to_string(), Some("page=SettingsPageDisplay".to_string())))
    }

    /// Every variant, so a new one cannot miss its row (this match has no wildcard).
    fn variant(r: &LaunchReq) -> u8 {
        match r {
            LaunchReq::App(_) => 0,
            LaunchReq::AppAdmin(_) => 1,
            LaunchReq::RevealApp(_) => 2,
            LaunchReq::Setting { .. } => 3,
            LaunchReq::OpenPath(_) => 4,
            LaunchReq::RevealPath(_) => 5,
            LaunchReq::Command(_) => 6,
            LaunchReq::Console(_) => 7,
            LaunchReq::RunAsTyped { .. } => 8,
            LaunchReq::Url(_) => 9,
            LaunchReq::Registry => 10,
        }
    }

    #[test]
    fn to_request_table() {
        let run = runline::classify("ping 1.1.1.1").unwrap();
        let quoted = runline::classify("\"C:\\Program Files\\x.exe\" -y").unwrap();
        let regedit = crate::shell_thread::system_dir().unwrap().parent().unwrap().join("regedit.exe");
        let regedit = regedit.to_string_lossy().into_owned();
        let rows = [
            LaunchReq::App(CALC.into()),
            LaunchReq::AppAdmin(CALC.into()),
            LaunchReq::RevealApp(CALC.into()),
            LaunchReq::Setting { index: 1 },
            LaunchReq::OpenPath("C:\\a b\\c.txt".into()),
            LaunchReq::RevealPath("C:\\a b\\c.txt".into()),
            LaunchReq::Command(run.clone()),
            LaunchReq::Console(run.clone()),
            LaunchReq::RunAsTyped { line: quoted.clone(), gesture: Gesture::for_test(7) },
            LaunchReq::Url("https://a.it/x?q=1".into()),
            LaunchReq::Registry,
        ];
        let mut seen: Vec<u8> = rows.iter().map(variant).collect();
        seen.dedup();
        assert_eq!(seen, (0..=10).collect::<Vec<u8>>(), "one row per variant");
        let out: Vec<ShellRequest> = rows.into_iter().map(|r| to_request(r, &setting).expect("a request")).collect();
        let mut it = out.iter();
        let mut next = || it.next().unwrap();
        let app = format!("shell:AppsFolder\\{CALC}");
        assert!(matches!(next(), ShellRequest::LauncherLaunch { target, args: None } if *target == app));
        assert!(matches!(next(), ShellRequest::LauncherAppAdmin { id } if id == CALC));
        assert!(matches!(next(), ShellRequest::LauncherRevealApp { id } if id == CALC));
        assert!(matches!(next(), ShellRequest::LauncherLaunch { target, args: Some(a) }
            if target == SETTINGS && a == "page=SettingsPageDisplay"));
        assert!(matches!(next(), ShellRequest::OpenPath { path } if path == "C:\\a b\\c.txt"));
        assert!(matches!(next(), ShellRequest::RevealPath { path } if path == "C:\\a b\\c.txt"));
        assert!(matches!(next(), ShellRequest::LaunchCommand { line, file, console: false }
            if line == "ping 1.1.1.1" && *file == (0..4)));
        assert!(matches!(next(), ShellRequest::LaunchCommand { line, console: true, .. } if line == "ping 1.1.1.1"));
        assert!(matches!(next(), ShellRequest::RunAsTyped { line, file, gesture }
            if line == "\"C:\\Program Files\\x.exe\" -y" && *file == (1..23) && *gesture == Gesture::for_test(7)));
        assert!(matches!(next(), ShellRequest::OpenUrl { url } if url == "https://a.it/x?q=1"));
        assert!(matches!(next(), ShellRequest::LauncherLaunch { target, args: None } if *target == regedit));
        // None of them is a request whose failure the launcher could not tell from Start's.
        let shared = |r: &ShellRequest| {
            matches!(
                r,
                ShellRequest::Launch { .. } | ShellRequest::LaunchAppAdmin { .. } | ShellRequest::RevealApp { .. }
            )
        };
        assert!(!out.iter().any(shared), "{out:?}");
        // A setting the controller does not hold launches nothing.
        assert!(to_request(LaunchReq::Setting { index: 7 }, &setting).is_none());
    }
}
