//! The role table (F-P10): what each command line of `dock.exe` is, which integrity level it needs, and what `main`
//! does when the process runs at another one (`gate`). `main` matches on the gate before any other work.
//!
//! The manifest asks for no elevation (asInvoker): a role that needs administrator rights gets them from the "Dock"
//! task's highest-privileges principal or from a "runas" relaunch (`elevate_and_wait`), and the roles that act for the
//! signed-in user (the agent, --undo-user) run at their level instead of refusing to start without a consent prompt.

use std::os::windows::ffi::OsStrExt;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE};
use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE};
use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

/// One command line of `dock.exe`: the branch `main` takes for it (`role_of`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    Main,
    Autostart,
    Watchdog,
    Install,
    Uninstall,
    Rollback,
    Restore,
    Settings,
    UpdateCheck,
    UpdateInstall,
    UpdateGuard,
    UpdateFetch,
    StartApps,
    StartSettings,
    StartIcons,
    LauncherFiles,
    Preview,
    ListApps,
    Version,
    Agent,
    UndoUser,
    Recover,
    RecoverDryRun,
    SuiteSync,
}

/// The integrity level a role runs at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Level {
    /// Whatever level it was started at.
    Any,
    /// The signed-in user's, never elevated (the agent, --undo-user).
    Medium,
    /// Elevated; a start without elevation asks for it.
    HighInteractive,
    /// Elevated; started only by the dock, the updater or the logon task, so a start without elevation never asks.
    HighInternal,
}

/// What `main` does with a role at the level it runs at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Gate {
    /// Go on.
    Run,
    /// Start this exe again with the same arguments through "runas" (`elevate_and_wait`).
    Elevate,
    /// Say why and exit with `autostart::EXIT_BAD_ARGS`.
    Refuse(&'static str),
    /// Leave with exit 0 before the single-instance mutex, the logger and any taskbar call; the reason goes to the
    /// debugger output only.
    LeaveQuietly(&'static str),
}

/// The role of a parsed command line, in `main`'s precedence: the first branch `main` takes for these arguments.
pub(crate) fn role_of(a: &crate::Args) -> Role {
    if a.version {
        Role::Version
    } else if a.update_fetch {
        Role::UpdateFetch
    } else if a.start_apps {
        Role::StartApps
    } else if a.start_settings {
        Role::StartSettings
    } else if a.start_icons.is_some() {
        Role::StartIcons
    } else if a.launcher_files {
        Role::LauncherFiles
    } else if a.agent.is_some() {
        Role::Agent
    } else if a.suite_sync {
        Role::SuiteSync
    } else if a.undo_user {
        Role::UndoUser
    } else if a.preview.is_some() {
        Role::Preview
    } else if a.list_apps {
        Role::ListApps
    } else if a.watchdog.is_some() {
        Role::Watchdog
    } else if a.install {
        Role::Install
    } else if a.uninstall {
        Role::Uninstall
    } else if a.rollback {
        Role::Rollback
    } else if a.recover {
        if a.dry_run {
            Role::RecoverDryRun
        } else {
            Role::Recover
        }
    } else if a.restore {
        Role::Restore
    } else if a.settings {
        Role::Settings
    } else if a.update_check {
        Role::UpdateCheck
    } else if a.update_install.is_some() {
        Role::UpdateInstall
    } else if a.update_guard.is_some() {
        Role::UpdateGuard
    } else if a.autostart {
        Role::Autostart
    } else {
        Role::Main
    }
}

/// The level each role needs. The elevated set is every role that runs elevated under the old requireAdministrator
/// manifest and changes the machine or the taskbar; a new elevated role changes this table and its test.
pub(crate) fn level(r: Role) -> Level {
    match r {
        Role::Main
        | Role::Install
        | Role::Uninstall
        | Role::Rollback
        | Role::Restore
        | Role::UpdateCheck
        | Role::UpdateInstall
        | Role::Recover => Level::HighInteractive,
        Role::Autostart | Role::Watchdog | Role::UpdateGuard | Role::SuiteSync => Level::HighInternal,
        Role::Agent | Role::UndoUser => Level::Medium,
        Role::Settings
        | Role::UpdateFetch
        | Role::StartApps
        | Role::StartSettings
        | Role::StartIcons
        | Role::LauncherFiles
        | Role::Preview
        | Role::ListApps
        | Role::Version
        | Role::RecoverDryRun => Level::Any,
    }
}

/// What `main` does with role `r` when the process is elevated (`Some(true)`), not elevated (`Some(false)`) or
/// its token cannot be read (`None`). An unknown elevation runs the elevated roles as the old manifest did (they fail
/// on their own where rights are missing) and refuses the medium ones.
pub(crate) fn gate(r: Role, elevated: Option<bool>) -> Gate {
    match (level(r), elevated) {
        (Level::Any, _) => Gate::Run,
        (Level::HighInteractive | Level::HighInternal, Some(true) | None) => Gate::Run,
        (Level::HighInteractive, Some(false)) => Gate::Elevate,
        (Level::HighInternal, Some(false)) if r == Role::Autostart => {
            Gate::LeaveQuietly("autostart: the logon task ran without elevation: leaving")
        }
        (Level::HighInternal, Some(false)) => {
            Gate::Refuse("This mode is started by the dock and needs administrator rights.")
        }
        (Level::Medium, Some(false)) => Gate::Run,
        // Elevated, or rights that cannot be read.
        (Level::Medium, _) => Gate::Refuse("This mode runs only as your normal user, without administrator rights."),
    }
}

/// Whether the process started without elevation waits for the elevated child `elevate_and_wait` starts. Never for the
/// dock itself, --update-install and --rollback (they hand over to long-lived or replacing processes), and never from
/// the installed `dock.exe`: a waiting parent keeps that image mapped, and the copy, the stop and the uninstaller would
/// wait on it and fail. A copy elsewhere (an installer in a download folder) waits and returns the child's exit code.
pub(crate) fn waits(r: Role, runs_installed_image: bool) -> bool {
    !runs_installed_image && !matches!(r, Role::Main | Role::UpdateInstall | Role::Rollback)
}

/// Exit code of a declined consent prompt.
const EXIT_DECLINED: i32 = 5;

/// The variable `elevate_and_wait` sets for its child: a child that "runas" started without elevation (a standard
/// account with UAC off) inherits it, and `main` stops it instead of relaunching again.
pub(crate) const RELAUNCHED: &str = "DOCK_RELAUNCHED";

// Declared here rather than through std::env::set_var, whose panic path costs a KiB of the exe.
#[link(name = "kernel32")]
extern "system" {
    fn SetEnvironmentVariableW(name: PCWSTR, value: PCWSTR) -> i32;
}

/// Starts this exe again through "runas" (the consent prompt) with the arguments it was given (`parameters`), and
/// returns the elevated child's exit code when `wait`, else 0 once it started. A declined prompt says so and returns 5.
pub(crate) fn elevate_and_wait(wait: bool) -> i32 {
    let Ok(exe) = std::env::current_exe() else {
        crate::logger::say("Administrator rights could not be obtained.");
        return crate::autostart::EXIT_FAILED;
    };
    let file: Vec<u16> = exe.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let params = parameters(std::env::args().skip(1));
    let mut sei = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: two NUL-terminated literals; the process has one thread here. A failure leaves the child unmarked.
    let _ = unsafe { SetEnvironmentVariableW(w!("DOCK_RELAUNCHED"), w!("1")) };
    // SAFETY: COM on this thread for the shell's verb handling, released below.
    let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
    // SAFETY: `sei` carries its cbSize; its strings are NUL-terminated and outlive the call, which returns only once
    // the start is done (SEE_MASK_NOASYNC).
    let started = unsafe { ShellExecuteExW(&mut sei) };
    if com.is_ok() {
        // SAFETY: paired with the successful CoInitializeEx above.
        unsafe { CoUninitialize() };
    }
    if let Err(e) = started {
        if e.code() == ERROR_CANCELLED.to_hresult() {
            crate::logger::say("Administrator rights are needed; the request was declined.");
            return EXIT_DECLINED;
        }
        crate::logger::say("Administrator rights could not be obtained.");
        return crate::autostart::EXIT_FAILED;
    }
    let process = sei.hProcess;
    if process.is_invalid() {
        return crate::autostart::EXIT_OK;
    }
    let mut code = 0u32;
    if wait {
        // SAFETY: `process` came from ShellExecuteExW (SEE_MASK_NOCLOSEPROCESS) and stays open until closed below.
        let read = unsafe {
            WaitForSingleObject(process, INFINITE);
            GetExitCodeProcess(process, &mut code)
        };
        if read.is_err() {
            code = crate::autostart::EXIT_FAILED as u32;
        }
    }
    // SAFETY: the process handle of ShellExecuteExW, closed once, here.
    let _ = unsafe { CloseHandle(process) };
    // An exit code outside 0-255 (an exception status) is a failure, never a success after the clamp in main.
    i32::try_from(code).ok().filter(|c| (0..=255).contains(c)).unwrap_or(crate::autostart::EXIT_FAILED)
}

/// The arguments as one NUL-terminated UTF-16 parameter string for `ShellExecuteExW`, each written by the rules of
/// `update::command_line` (those of `std::process::Command`): in quotes when empty or holding a space or a tab, a quote
/// escaped, backslashes doubled before a quote and before a closing quote. Windows' parser reads the same strings back.
fn parameters<S: AsRef<str>>(args: impl Iterator<Item = S>) -> Vec<u16> {
    const QUOTE: u16 = b'"' as u16;
    const BACKSLASH: u16 = b'\\' as u16;
    let mut line = Vec::new();
    for (i, arg) in args.enumerate() {
        let arg = arg.as_ref();
        if i > 0 {
            line.push(u16::from(b' '));
        }
        let quoted = arg.is_empty() || arg.contains([' ', '\t']);
        if quoted {
            line.push(QUOTE);
        }
        let mut backslashes = 0;
        for c in arg.encode_utf16() {
            if c == BACKSLASH {
                backslashes += 1;
            } else {
                if c == QUOTE {
                    line.extend(std::iter::repeat_n(BACKSLASH, backslashes + 1));
                }
                backslashes = 0;
            }
            line.push(c);
        }
        if quoted {
            line.extend(std::iter::repeat_n(BACKSLASH, backslashes));
            line.push(QUOTE);
        }
    }
    line.push(0);
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every role, once.
    pub(crate) const ALL: [Role; 24] = [
        Role::Main,
        Role::Autostart,
        Role::Watchdog,
        Role::Install,
        Role::Uninstall,
        Role::Rollback,
        Role::Restore,
        Role::Settings,
        Role::UpdateCheck,
        Role::UpdateInstall,
        Role::UpdateGuard,
        Role::UpdateFetch,
        Role::StartApps,
        Role::StartSettings,
        Role::StartIcons,
        Role::LauncherFiles,
        Role::Preview,
        Role::ListApps,
        Role::Version,
        Role::Agent,
        Role::UndoUser,
        Role::Recover,
        Role::RecoverDryRun,
        Role::SuiteSync,
    ];

    /// The place of `r` in `ALL`: a match without wildcard, so a new role does not compile until it is placed here
    /// (and in `ALL`, which the test checks through this index).
    fn placed(r: Role) -> usize {
        match r {
            Role::Main => 0,
            Role::Autostart => 1,
            Role::Watchdog => 2,
            Role::Install => 3,
            Role::Uninstall => 4,
            Role::Rollback => 5,
            Role::Restore => 6,
            Role::Settings => 7,
            Role::UpdateCheck => 8,
            Role::UpdateInstall => 9,
            Role::UpdateGuard => 10,
            Role::UpdateFetch => 11,
            Role::StartApps => 12,
            Role::StartSettings => 13,
            Role::StartIcons => 14,
            Role::LauncherFiles => 15,
            Role::Preview => 16,
            Role::ListApps => 17,
            Role::Version => 18,
            Role::Agent => 19,
            Role::UndoUser => 20,
            Role::Recover => 21,
            Role::RecoverDryRun => 22,
            Role::SuiteSync => 23,
        }
    }

    #[test]
    fn the_exact_role_levels() {
        for (i, r) in ALL.into_iter().enumerate() {
            assert_eq!(placed(r), i, "{r:?} is in ALL once, at its place");
        }
        let interactive = [
            Role::Main,
            Role::Install,
            Role::Uninstall,
            Role::Rollback,
            Role::Restore,
            Role::UpdateCheck,
            Role::UpdateInstall,
            Role::Recover,
        ];
        let internal = [Role::Autostart, Role::Watchdog, Role::UpdateGuard, Role::SuiteSync];
        let medium = [Role::Agent, Role::UndoUser];
        for r in ALL {
            let want = if interactive.contains(&r) {
                Level::HighInteractive
            } else if internal.contains(&r) {
                Level::HighInternal
            } else if medium.contains(&r) {
                Level::Medium
            } else {
                Level::Any
            };
            assert_eq!(level(r), want, "{r:?}");
        }
        let any = ALL.into_iter().filter(|r| level(*r) == Level::Any).count();
        assert_eq!(any, 10, "Settings, the five children, Preview, ListApps, Version and RecoverDryRun");
    }

    #[test]
    fn gate_truth_table() {
        for r in ALL {
            for e in [Some(true), Some(false), None] {
                let g = gate(r, e);
                let ok = match (level(r), e) {
                    (Level::Any, _) => g == Gate::Run,
                    (Level::HighInteractive | Level::HighInternal, Some(true) | None) => g == Gate::Run,
                    (Level::HighInteractive, Some(false)) => g == Gate::Elevate,
                    (Level::HighInternal, Some(false)) if r == Role::Autostart => matches!(g, Gate::LeaveQuietly(_)),
                    (Level::HighInternal, Some(false)) => matches!(g, Gate::Refuse(_)),
                    (Level::Medium, Some(false)) => g == Gate::Run,
                    (Level::Medium, Some(true) | None) => matches!(g, Gate::Refuse(_)),
                };
                assert!(ok, "{r:?} at {e:?}: {g:?}");
                match g {
                    Gate::Refuse(why) => assert!(why.is_ascii() && why.len() > 20 && why.ends_with('.'), "{why}"),
                    Gate::LeaveQuietly(why) => assert!(why.starts_with("autostart: ") && why.is_ascii(), "{why}"),
                    Gate::Run | Gate::Elevate => {}
                }
            }
        }
    }
    #[test]
    fn gate_wiring_never_relaunches_internal_roles() {
        // A fake elevation: every role at every token state. Only an interactive elevated role started without
        // elevation is relaunched; the internal ones (logon task, watchdog, guard, sync), the children and the medium
        // roles never ask for the consent prompt.
        for r in ALL {
            for e in [Some(true), Some(false), None] {
                let relaunch = gate(r, e) == Gate::Elevate;
                assert_eq!(relaunch, level(r) == Level::HighInteractive && e == Some(false), "{r:?} at {e:?}");
            }
        }
        for r in [Role::Autostart, Role::Watchdog, Role::UpdateGuard, Role::SuiteSync, Role::Agent, Role::UndoUser] {
            assert_ne!(gate(r, Some(false)), Gate::Elevate, "{r:?}");
        }
        // main: the gate is the first work after the arguments, and its Elevate arm is the only relaunch.
        let main = include_str!("main.rs");
        let body = &main[main.find("\nfn main() -> std::process::ExitCode {").expect("main")..];
        let gate_at = body.find("match roles::gate(role, shell_thread::elevation()) {").expect("the gate in main");
        assert!(gate_at < body.find("if args.version {").expect("--version"), "the gate runs before --version");
        assert!(
            body.find("ARGS.get_or_init(|| args)").is_some_and(|set| set < gate_at),
            "the gate reads the arguments"
        );
        let relaunches: Vec<_> = main.match_indices("roles::elevate_and_wait(").collect();
        assert_eq!(relaunches.len(), 1, "one relaunch, in the gate's Elevate arm");
        let elevate_arm = body.find("roles::Gate::Elevate => {").expect("the Elevate arm");
        assert!(body.find("roles::elevate_and_wait(").is_some_and(|at| at > elevate_arm));
    }

    /// "runas" can start the child without elevation (a standard account with UAC off), and that child gets Elevate
    /// again: the relaunch marks its child, and a marked child stops instead of relaunching without end.
    #[test]
    fn a_relaunched_child_never_relaunches_again() {
        let src = include_str!("roles.rs");
        let start = src.find("pub(crate) fn elevate_and_wait(").expect("elevate_and_wait");
        // "\n}" and not "\n}\n": the function ends there on an LF and on a CRLF (autocrlf) checkout alike.
        let body = &src[start..start + src[start..].find("\n}").expect("its end")];
        assert_eq!(RELAUNCHED, "DOCK_RELAUNCHED");
        let mark = body.find("SetEnvironmentVariableW(w!(\"DOCK_RELAUNCHED\"), w!(\"1\"))").expect("the mark");
        assert!(mark < body.find("ShellExecuteExW(&mut sei)").expect("the relaunch"));
        let main = include_str!("main.rs");
        let body = &main[main.find("\nfn main() -> std::process::ExitCode {").expect("main")..];
        let marked = body.find("roles::Gate::Elevate if std::env::var_os(roles::RELAUNCHED).is_some() => {");
        let marked = marked.expect("the arm of a marked child");
        let relaunch = body.find("roles::elevate_and_wait(").expect("the relaunch");
        assert!(marked < body.find("roles::Gate::Elevate => {").expect("the Elevate arm") && marked < relaunch);
        assert!(body[marked..relaunch].contains("return std::process::ExitCode::from(autostart::EXIT_FAILED as u8);"));
    }

    #[test]
    fn build_rs_asks_for_as_invoker() {
        let build = include_str!("../build.rs");
        assert!(build.contains("ExecutionLevel::AsInvoker"));
        assert!(!build.contains("RequireAdministrator") && !build.contains("requireAdministrator"));
    }

    #[test]
    fn never_wait_from_the_installed_image() {
        for r in ALL {
            assert!(!waits(r, true), "{r:?} from the installed dock.exe");
        }
        for r in [Role::Main, Role::UpdateInstall, Role::Rollback] {
            assert!(!waits(r, false), "{r:?}");
        }
        for r in [Role::Install, Role::Uninstall, Role::Restore, Role::UpdateCheck, Role::Recover] {
            assert!(waits(r, false), "{r:?} from a download folder waits for its elevated child");
        }
    }

    /// The arguments re-quoted for the relaunch split back (Windows' parser) into the same strings, for the fixtures
    /// of `update::command_line`'s round trip.
    #[test]
    fn relaunch_arguments_round_trip() {
        use windows::Win32::Foundation::{LocalFree, HLOCAL};
        use windows::Win32::UI::Shell::CommandLineToArgvW;
        let exe = r"C:\Program Files\mini dock\dock.exe";
        let fixtures = [
            &[
                "--start-icons",
                "32",
                "",
                "a b",
                "tab\there",
                "q\"uote",
                "\"",
                "end\\",
                "end space\\",
                "two\\\\\"q",
                r"\\srv\a b\",
            ][..],
            &["--install", "--from", r"D:\new kit\", "--quiet"],
            &["--undo-user", "--signal", "0123456789abcdef0123456789abcdef"],
            &[],
        ];
        for args in fixtures {
            let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
            let params = parameters(args.iter());
            assert_eq!(params.last(), Some(&0));
            // ShellExecuteExW starts the program as "<lpFile>" <lpParameters>.
            let line: Vec<u16> = format!("\"{exe}\" ").encode_utf16().chain(params.iter().copied()).collect();
            let mut n = 0;
            // SAFETY: `line` is NUL-terminated; the array returned holds `n` strings, read once, then freed once.
            let parsed: Vec<String> = unsafe {
                let argv = CommandLineToArgvW(PCWSTR(line.as_ptr()), &mut n);
                assert!(!argv.is_null());
                let parsed = (0..n as usize).map(|i| (*argv.add(i)).to_string().unwrap()).collect();
                let _ = LocalFree(Some(HLOCAL(argv.cast())));
                parsed
            };
            let want: Vec<String> = std::iter::once(exe.to_string()).chain(args).collect();
            assert_eq!(parsed, want);
        }
    }
}
