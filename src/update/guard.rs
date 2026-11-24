//! The update guard (`--update-guard <tag>`) and the way back (`--rollback`).
//!
//! Every update is judged by the version that ran before it. The updater copies its own (old) image to
//! `update\<tag>\dock-guard.exe` and starts it with `--update-guard <tag>`. The guard checks the stage again, runs
//! the new release's `--install --quiet --no-relaunch`, starts the new dock and keeps its process handle. If that
//! process exits (an exit at a session end gets no verdict), or its `DockWindow` does not answer `WM_NULL` between
//! `MIN_ALIVE` and `MAX_WAIT` after the start, the guard ends it, writes `update\<tag>\rolled-back` (the automatic
//! install never retries that release), installs itself back with its own `--install` and starts the old dock. The
//! judge is the old binary because the build under judgement cannot be trusted to judge itself. The guard makes no
//! taskbar call: the ended dock's own watchdog gives the taskbar back, and `copy_self` waits for that watchdog to
//! leave.
//!
//! Contracts later releases keep (pinned by the tests below):
//! - C1: the dock's top-level window class is `DockWindow`, owned by its UI thread, which pumps within 30 s of the
//!   start.
//! - C2: `--install --quiet --no-relaunch` copies the exe over `dock.exe`, keeps `dock.prev.exe` on an upgrade,
//!   registers the "Dock" task, writes the Add/Remove Programs entry and starts no dock: an older guard runs a newer
//!   installer with exactly these arguments.
//! - C5: the stage `update\<tag>\` holds `dock.exe`, `dock.exe.sha256`, `dock-guard.exe` and `rolled-back`; releases
//!   before `FIRST_RELEASE_WITH_MARKS` do not read `rolled-back`, and `--rollback` to one of them says so.
//! - C6: the exit codes `EXIT_COMMITTED` to `EXIT_ROLLBACK_FAILED` (0 to 4).
//!
//! `--rollback` installs `dock.prev.exe` again through that binary's own `--install`.
//!
//! `panic = "abort"`: a guard that aborts mid-update leaves the new version installed but not judged, so this module
//! denies the panicking shortcuts.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::io::Read;
use std::os::windows::io::{AsRawHandle, OwnedHandle};
use std::path::Path;
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::{GetProcessId, TerminateProcess};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, GetSystemMetrics, GetWindowThreadProcessId, SendMessageTimeoutW, SMTO_ABORTIFHUNG, SMTO_BLOCK,
    SM_SHUTTINGDOWN, WM_NULL,
};

use crate::update_release::{
    parse_tag, Version, EXE_ASSET, EXE_MAX_BYTES, GUARD_FILE, ROLLED_BACK_FILE, SHA_ASSET, SHA_MAX_BYTES,
};
use crate::{autostart, datadir, log, watchdog};

/// The arguments the guard starts the new release's installer with (contract C2).
pub(crate) const NEW_INSTALL_ARGS: &str = "--install --quiet --no-relaunch";

/// The arguments `--rollback` starts `dock.prev.exe` with: without `--quiet` the older installer shows its own result.
pub(crate) fn rollback_install_args(quiet: bool) -> &'static str {
    if quiet {
        "--install --quiet"
    } else {
        "--install"
    }
}

/// The first release that reads `update\<tag>\rolled-back`.
pub(crate) const FIRST_RELEASE_WITH_MARKS: Version = Version { major: 1, minor: 5, patch: 1 };

/// true when `target` does not read rolled-back marks: with automatic installs on it installs the newer release
/// again at its next daily check. Pure.
fn target_ignores_marks(target: Version) -> bool {
    target < FIRST_RELEASE_WITH_MARKS
}

/// The new dock must live this long, and its window must answer by `MAX_WAIT`.
pub(crate) const MIN_ALIVE: Duration = Duration::from_secs(30);
pub(crate) const MAX_WAIT: Duration = Duration::from_secs(60);
/// The wait on the new dock's process per round: its exit is seen at once.
const POLL_MS: u32 = 1000;
/// How long the new dock's window may take to answer `WM_NULL`.
const ANSWER_MS: u32 = 2000;
/// How long the guard waits for the dock it ended to go.
const KILL_WAIT_MS: u32 = 5000;

/// `--update-guard` exit codes (contract C6). The updater shows a code that comes back while the old dock still
/// runs as "Installation failed (exit code n)".
pub(crate) const EXIT_COMMITTED: i32 = 0;
pub(crate) const EXIT_ROLLED_BACK: i32 = 1;
pub(crate) const EXIT_BAD_ARGS: i32 = 2;
/// The old version stays: nothing was installed.
pub(crate) const EXIT_NOT_INSTALLED: i32 = 3;
pub(crate) const EXIT_ROLLBACK_FAILED: i32 = 4;

/// The guard's command line after the exe path.
pub(crate) fn guard_args(tag: &str) -> String {
    format!("--update-guard {tag}")
}

/// The verdict on the new dock so far.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Health {
    Wait,
    Healthy,
    Failed(String),
    /// It exited while the session ends (sign-out, shutdown), which ends every dock: no verdict, no mark, nothing
    /// installed or started; it stays installed.
    SessionEnd,
}

/// `elapsed` since the new dock started, its exit code if it ended, whether its window answered, whether the session
/// is ending. An exit fails (even with code 0) unless the session is ending; an answer counts from `MIN_ALIVE` on; no
/// answer by `MAX_WAIT` fails. Pure.
pub(crate) fn health(elapsed: Duration, exit: Option<u32>, answered: bool, session_ending: bool) -> Health {
    if exit.is_some() && session_ending {
        Health::SessionEnd
    } else if let Some(c) = exit {
        Health::Failed(format!("exited with code 0x{c:X} after {} s", elapsed.as_secs()))
    } else if answered && elapsed >= MIN_ALIVE {
        Health::Healthy
    } else if elapsed >= MAX_WAIT {
        Health::Failed(format!("its window did not answer within {} s", MAX_WAIT.as_secs()))
    } else {
        Health::Wait
    }
}

/// What the guard does once the new release's installer has returned.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AfterInstall {
    /// `dock.exe` is the new release: start and judge it.
    Judge,
    /// `dock.exe` is still this (old) version: nothing to undo.
    NotInstalled,
    /// Anything else: install this version back.
    Restore(String),
}

/// `installed` = the hash of `dock.exe` after the installer, `new` = the staged release's, `old` = this exe's. Pure.
pub(crate) fn after_install(installed: Option<[u8; 32]>, new: &[u8; 32], old: &[u8; 32]) -> AfterInstall {
    match installed {
        Some(h) if h == *new => AfterInstall::Judge,
        Some(h) if h == *old => AfterInstall::NotInstalled,
        Some(_) => AfterInstall::Restore("left a dock.exe that is neither the new nor the old version".into()),
        None => AfterInstall::Restore("left dock.exe missing or unreadable".into()),
    }
}

/// The version `--rollback` goes back to: `dock.prev.exe`'s (`prev`), which must be older than the installed one when
/// that is known (a broken installed exe that cannot print its version may still be left). Pure.
pub(crate) fn rollback_target(prev: Option<Version>, installed: Option<Version>) -> Result<Version, String> {
    let Some(p) = prev else {
        return Err("no previous version to go back to (dock.prev.exe is missing or does not start)".into());
    };
    match installed {
        Some(i) if p >= i => Err(format!("dock.prev.exe is {p}, not older than the installed {i}")),
        _ => Ok(p),
    }
}

/// SHA-256 of the file at `p` (at most `EXE_MAX_BYTES`), or None.
fn hash_file(p: &Path) -> Option<[u8; 32]> {
    let mut bytes = Vec::new();
    std::fs::File::open(p).ok()?.take(EXE_MAX_BYTES + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > EXE_MAX_BYTES {
        return None;
    }
    crate::sha256::sha256(&bytes).ok()
}

/// The exit code of the process `h` if it ends within `wait_ms`, else None.
fn exit_of(h: &OwnedHandle, wait_ms: u32) -> Option<u32> {
    super::wait_exit(h, Duration::from_millis(u64::from(wait_ms))).map(|c| c as u32)
}

/// true when the top-level `DockWindow` of process `pid` answers `WM_NULL` within `ANSWER_MS` (contract C1). A window
/// of that class owned by another process (a squatter) is skipped.
fn window_answers(pid: u32) -> bool {
    let mut after: Option<HWND> = None;
    // SAFETY: the class is a static NUL-terminated literal and a null name matches any window; `after` is a window
    // this loop found (one destroyed meanwhile ends the search with an error).
    while let Ok(h) = unsafe { FindWindowExW(None, after, crate::CLASS_NAME, PCWSTR::null()) } {
        after = Some(h);
        let mut owner = 0u32;
        // SAFETY: `owner` is a valid out pointer; a window gone meanwhile yields 0, no process's id.
        unsafe { GetWindowThreadProcessId(h, Some(&mut owner)) };
        if owner != pid {
            continue;
        }
        // The guard has no window to dispatch for: SMTO_BLOCK keeps the timeout a true bound (launcher lint 19).
        let flags = SMTO_BLOCK | SMTO_ABORTIFHUNG;
        // SAFETY: any handle is accepted (a stale one fails at once); WM_NULL does nothing.
        return unsafe { SendMessageTimeoutW(h, WM_NULL, WPARAM(0), LPARAM(0), flags, ANSWER_MS, None) }.0 != 0;
    }
    false
}

/// true while the session ends (sign-out, shutdown).
fn session_ending() -> bool {
    // SAFETY: GetSystemMetrics has no preconditions.
    unsafe { GetSystemMetrics(SM_SHUTTINGDOWN) != 0 }
}

/// Writes `update\<tag>\rolled-back` (the reason inside): the automatic install skips `tag` from now on.
fn mark_rolled_back(tag: &str, why: &str) {
    if let Err(e) = datadir::write_stage_file(tag, ROLLED_BACK_FILE, why.as_bytes()) {
        log!("update-guard: cannot write the rolled-back mark of {tag}: {e}");
    }
}

/// true when this process runs the installed `dock.exe`.
pub(crate) fn runs_installed() -> bool {
    match (std::env::current_exe(), autostart::installed_exe()) {
        (Ok(me), Ok(dest)) => autostart::same_file(&me, &dest),
        _ => false,
    }
}

/// Starts the installed dock with no argument, as an installer's relaunch does, and lets it go.
fn start_dock(dest: &Path) {
    if let Err(e) = watchdog::spawn_breakaway(dest, "") {
        log!("update-guard: cannot start {}: {e}", dest.display());
    }
}

/// The staged `dock.exe` checked again against the staged `.sha256`; returns its hash (the bytes are dropped).
fn staged_hash(tag: &str) -> Result<[u8; 32], String> {
    let exe = datadir::read_stage_file(tag, EXE_ASSET, EXE_MAX_BYTES).map_err(|e| format!("{EXE_ASSET}: {e}"))?;
    let sha = datadir::read_stage_file(tag, SHA_ASSET, SHA_MAX_BYTES).map_err(|e| format!("{SHA_ASSET}: {e}"))?;
    super::verify_bytes(&exe, &sha, None)
}

/// `--update-guard <tag>`: installs the staged release `tag`, starts it and judges it (module doc). Runs only as
/// `update\<tag>\dock-guard.exe`; returns one of the `EXIT_` codes. Every line goes to dock.log with the `ug` tag.
pub(crate) fn run_cli(tag: &str) -> i32 {
    if parse_tag(tag).is_none() {
        log!("update-guard: invalid tag {tag:?}");
        return EXIT_BAD_ARGS;
    }
    if !datadir::writable() {
        log!("update-guard: no writable data folder (run elevated)");
        return EXIT_NOT_INSTALLED;
    }
    let guard = datadir::checked_stage_path(tag, GUARD_FILE).ok();
    let me = std::env::current_exe().ok().filter(|m| guard.as_deref().is_some_and(|g| autostart::same_file(m, g)));
    let Some(me) = me else {
        log!("update-guard: runs only as update\\{tag}\\{GUARD_FILE}");
        return EXIT_BAD_ARGS;
    };
    let (Some(old), Ok(dest)) = (hash_file(&me), autostart::installed_exe()) else {
        log!("update-guard: cannot hash this exe or find the installed one; nothing changed");
        return EXIT_NOT_INSTALLED;
    };
    let new = match staged_hash(tag) {
        Ok(h) => h,
        Err(e) => {
            log!("update-guard: {tag}: the stage no longer verifies ({e}); nothing changed");
            return EXIT_NOT_INSTALLED;
        }
    };
    let current = Version::current();
    log!("update-guard: installing {tag} over {current}");
    let installer = datadir::checked_stage_path(tag, EXE_ASSET)
        .map_err(|e| e.to_string())
        .and_then(|p| watchdog::spawn_breakaway(&p, NEW_INSTALL_ARGS));
    let child = match installer {
        Ok(c) => c,
        Err(e) => {
            log!("update-guard: cannot start the installer of {tag} ({e}); nothing changed");
            return EXIT_NOT_INSTALLED;
        }
    };
    let code = super::wait_exit(&child, super::INSTALL_WAIT);
    drop(child);
    log!("update-guard: installer exit {code:?}");
    match after_install(hash_file(&dest), &new, &old) {
        AfterInstall::NotInstalled => {
            // The image-path check `copy_self` uses: a named object (`instance_running`) could be squatted.
            if !autostart::exe_in_use_by_other(&dest) {
                start_dock(&dest);
            }
            log!("update-guard: {tag} not installed; the old version stays");
            return EXIT_NOT_INSTALLED;
        }
        AfterInstall::Restore(why) => return rollback(tag, &why, &old, &dest),
        AfterInstall::Judge => {}
    }
    let h = match watchdog::spawn_breakaway(&dest, "") {
        Ok(h) => h,
        Err(e) => return rollback(tag, &format!("could not be started: {e}"), &old, &dest),
    };
    // SAFETY: `h` is a process handle owned by this function for the call; 0 on failure, which no window's pid is.
    let pid = unsafe { GetProcessId(HANDLE(h.as_raw_handle())) };
    let t0 = Instant::now();
    loop {
        let exit = exit_of(&h, POLL_MS);
        let elapsed = t0.elapsed();
        let answered = exit.is_none() && elapsed >= MIN_ALIVE && window_answers(pid);
        match health(elapsed, exit, answered, exit.is_some() && session_ending()) {
            Health::Wait => {}
            Health::SessionEnd => {
                log!("update-guard: {tag} exited with the session end (pid {pid}): no verdict, it stays installed");
                return EXIT_COMMITTED;
            }
            Health::Healthy => {
                log!("update-guard: {tag} started correctly (pid {pid})");
                return EXIT_COMMITTED;
            }
            Health::Failed(why) => {
                if exit.is_none() {
                    log!("update-guard: {tag} terminated pid {pid} ({why})");
                    // SAFETY: `h` is the new dock's process handle, owned here; its watchdog gives the taskbar back.
                    if let Err(e) = unsafe { TerminateProcess(HANDLE(h.as_raw_handle()), 1) } {
                        log!("update-guard: TerminateProcess failed: {e}");
                    }
                    let _ = exit_of(&h, KILL_WAIT_MS);
                }
                return rollback(tag, &why, &old, &dest);
            }
        }
    }
}

/// Puts this (old) version back over `dest` and starts it. `autostart::run_install_cli` runs in this process: its
/// `copy_self` waits up to 35 s for the ended dock's watchdog to give the taskbar back and leave, and leaves
/// `dock.prev.exe` as it is (not an upgrade). No taskbar call here.
fn rollback(tag: &str, why: &str, old: &[u8; 32], dest: &Path) -> i32 {
    let current = Version::current();
    log!("update-guard: {tag} {why}: rolling back to {current}");
    mark_rolled_back(tag, why);
    let code = autostart::run_install_cli(true);
    if hash_file(dest).as_ref() == Some(old) {
        start_dock(dest);
        log!("update-guard: rolled back to {current}");
        EXIT_ROLLED_BACK
    } else {
        log!("update-guard: rollback failed (install exit {code}); dock.exe is not {current}");
        EXIT_ROLLBACK_FAILED
    }
}

/// `--rollback [--quiet]`: installs `dock.prev.exe` again through that binary's own `--install`. Messages through
/// `logger::say`; returns the exit code.
pub(crate) fn run_rollback_cli(quiet: bool) -> i32 {
    let say = crate::logger::say;
    if !datadir::writable() {
        say("--rollback needs the data folder: run it elevated");
        return 1;
    }
    let dest = match autostart::installed_exe() {
        Ok(d) => d,
        Err(e) => {
            say(&format!("Rollback failed: {e}"));
            return 1;
        }
    };
    let prev = autostart::prev_path(&dest);
    let installed = autostart::exe_version(&dest);
    let target = match rollback_target(autostart::exe_version(&prev), installed) {
        Ok(t) => t,
        Err(m) => {
            say(&m);
            return 1;
        }
    };
    if target_ignores_marks(target) {
        let newer = installed.map_or_else(|| "the newer release".to_string(), |v| v.to_string());
        say(&format!(
            "{target} does not know rolled-back releases: switch off Settings > Updates > Install automatically, \
             or it installs {newer} again at its next daily check"
        ));
    }
    if let Some(v) = installed {
        mark_rolled_back(&v.tag(), "rolled back by hand");
    }
    if runs_installed() {
        // This process keeps dock.exe mapped while it lives, and the older installer's `copy_self` counts it as a
        // holder: start it and leave at once.
        if let Err(e) = watchdog::spawn_breakaway(&prev, rollback_install_args(quiet)) {
            say(&format!("Cannot start {}: {e}", prev.display()));
            return 1;
        }
        let msg = format!("Going back to {target}: the dock restarts in a few seconds (see dock.log).");
        log!("{msg}");
        say(&msg);
        return 0;
    }
    let child = match watchdog::spawn_breakaway(&prev, rollback_install_args(true)) {
        Ok(c) => c,
        Err(e) => {
            say(&format!("Cannot start {}: {e}", prev.display()));
            return 1;
        }
    };
    match super::wait_exit(&child, super::INSTALL_WAIT) {
        Some(0) => {
            say(&format!("Went back to {target}."));
            0
        }
        Some(code) => {
            say(&format!("The installer of {target} exited with {code}."));
            super::cli_code(code)
        }
        None => {
            say(&format!("The installer of {target} did not finish within {} s.", super::INSTALL_WAIT.as_secs()));
            1
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;

    const fn v(major: u32, minor: u32, patch: u32) -> Version {
        Version { major, minor, patch }
    }

    #[test]
    fn guard_arguments_are_fixed() {
        assert_eq!(guard_args("v1.5.3"), "--update-guard v1.5.3");
        assert_eq!(NEW_INSTALL_ARGS, "--install --quiet --no-relaunch");
        assert_eq!(rollback_install_args(true), "--install --quiet");
        assert_eq!(rollback_install_args(false), "--install");
    }

    #[test]
    fn health_verdicts() {
        let s = Duration::from_secs;
        let failed = |h: Health, part: &str| matches!(h, Health::Failed(w) if w.contains(part));
        assert_eq!(health(s(0), None, false, false), Health::Wait);
        assert!(failed(health(s(5), Some(1), false, false), "0x1"));
        assert_eq!(health(s(29), None, true, false), Health::Wait);
        assert_eq!(health(s(30), None, true, false), Health::Healthy);
        assert_eq!(health(s(45), None, false, false), Health::Wait);
        assert!(failed(health(s(60), None, false, false), "did not answer"));
        assert_eq!(health(s(60), None, true, false), Health::Healthy);
        assert!(failed(health(s(31), Some(0), true, false), "0x0"), "an exit wins over an answer, even code 0");
    }

    #[test]
    fn an_exit_at_the_session_end_has_no_verdict() {
        let s = Duration::from_secs;
        assert_eq!(health(s(5), Some(0), false, true), Health::SessionEnd);
        assert_eq!(health(s(61), Some(1), true, true), Health::SessionEnd);
        assert_eq!(health(s(5), None, false, true), Health::Wait, "no exit: the session end changes nothing");
        assert_eq!(health(s(30), None, true, true), Health::Healthy);
        assert!(matches!(health(s(60), None, false, true), Health::Failed(_)));
    }

    #[test]
    fn after_install_decisions() {
        let (new, old, other) = ([1u8; 32], [2u8; 32], [3u8; 32]);
        assert_eq!(after_install(Some(new), &new, &old), AfterInstall::Judge);
        assert_eq!(after_install(Some(new), &new, &new), AfterInstall::Judge, "the same build again is judged");
        assert_eq!(after_install(Some(old), &new, &old), AfterInstall::NotInstalled);
        assert!(matches!(after_install(None, &new, &old), AfterInstall::Restore(w) if w.contains("missing")));
        assert!(matches!(after_install(Some(other), &new, &old), AfterInstall::Restore(w) if w.contains("neither")));
    }

    #[test]
    fn rollback_needs_an_older_previous_version() {
        assert!(matches!(rollback_target(None, Some(v(1, 5, 1))), Err(m) if m.contains("no previous version")));
        assert!(rollback_target(Some(v(1, 5, 1)), Some(v(1, 5, 1))).is_err());
        assert!(rollback_target(Some(v(1, 5, 2)), Some(v(1, 5, 1))).is_err());
        assert_eq!(rollback_target(Some(v(1, 5, 0)), Some(v(1, 5, 1))), Ok(v(1, 5, 0)));
        // A broken installed exe that cannot print its version may still be left.
        assert_eq!(rollback_target(Some(v(1, 5, 0)), None), Ok(v(1, 5, 0)));
    }

    #[test]
    fn a_rollback_to_a_release_without_marks_warns() {
        assert!(target_ignores_marks(v(1, 5, 0)));
        assert!(target_ignores_marks(v(1, 4, 3)));
        assert!(!target_ignores_marks(FIRST_RELEASE_WITH_MARKS));
        assert!(!target_ignores_marks(v(1, 5, 2)));
        // The release that ships the marks (the release task checks it against Cargo.toml).
        assert_eq!(FIRST_RELEASE_WITH_MARKS, v(1, 5, 1));
    }

    #[test]
    fn guard_exit_codes_are_distinct_and_stable() {
        let codes = (EXIT_COMMITTED, EXIT_ROLLED_BACK, EXIT_BAD_ARGS, EXIT_NOT_INSTALLED, EXIT_ROLLBACK_FAILED);
        assert_eq!(codes, (0, 1, 2, 3, 4));
        assert_eq!(EXIT_BAD_ARGS, crate::autostart::EXIT_BAD_ARGS);
    }

    #[test]
    fn dock_window_class_is_a_cross_version_contract() {
        // Older update guards look for this class (C1): never rename it.
        // SAFETY: CLASS_NAME is a static NUL-terminated literal.
        assert_eq!(unsafe { crate::CLASS_NAME.to_string() }.ok().as_deref(), Some("DockWindow"));
    }
}
