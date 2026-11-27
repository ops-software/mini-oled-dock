//! `--recover` and safe mode. Lands with J1d; until then there is no safe mode and the flag says so.
#![allow(dead_code, unused_variables)]

/// What an `--autostart` start does about safe mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutostartAction {
    Proceed,
    LeaveSafe,
}

/// `--recover [--dry-run]`: J1d.
pub fn run(dry_run: bool) -> i32 {
    crate::logger::say("--recover: not available in this build");
    2
}

/// Safe mode is on: J1d. Until then it never is.
pub fn safe_mode() -> bool {
    false
}

/// Leaves safe mode: J1d.
pub fn clear_safe_mode() -> std::io::Result<()> {
    Ok(())
}

/// Called by main for every Main start: J1d.
pub fn leave_safe_mode_on_manual_start() {}

/// Pure: an autostart start proceeds unless safe mode is on, in which case it leaves quietly.
pub fn autostart_action(safe_mode: bool) -> AutostartAction {
    if safe_mode {
        AutostartAction::LeaveSafe
    } else {
        AutostartAction::Proceed
    }
}
