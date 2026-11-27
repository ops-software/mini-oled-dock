//! The window's start (P3-files-app 2.2): the step plan (FZ2, pure) and run, which executes it (FA-WIN, F1a.10).

use crate::cli::Cli;
use crate::instance::{self, Elevation};
use crate::names;

/// One step of a start, in the order run executes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Log,
    HandOff,
    Forward,
    ExitNotForwarded,
    Mutex,
    Services,
    Enumerate,
    StartJobs,
    Config,
    Reopen,
    Identity,
    Loop,
    Ipc,
    WaitFirstBatch,
    OpenWindow,
    Follow,
    Run,
    Preview,
    Selftest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StartEnv {
    pub elevation: Elevation,
}

/// A window start: [Log, HandOff] when the token is UAC-elevated (nothing else); else [Log, Forward], then
/// [ExitNotForwarded] alone for --forward-only, else Mutex, Services, Enumerate (a folder or file target only),
/// StartJobs, Config, Reopen, Identity, Loop, Ipc, WaitFirstBatch, OpenWindow, Follow, Run. --preview and --selftest
/// are one step each and never hand off, take the mutex or open a window, whatever the elevation.
pub fn start_plan(cli: &Cli, env: &StartEnv) -> Vec<Step> {
    use Step::*;
    match cli {
        Cli::Preview { .. } => vec![Preview],
        Cli::Selftest { .. } => vec![Selftest],
        Cli::Window { .. } if instance::must_hand_off(env.elevation) => vec![Log, HandOff],
        Cli::Window { forward_only: true, .. } => vec![Log, Forward, ExitNotForwarded],
        Cli::Window { target, select, .. } => {
            let mut plan = vec![Log, Forward, Mutex, Services];
            if target.is_some() || select.is_some() {
                plan.push(Enumerate);
            }
            plan.extend([StartJobs, Config, Reopen, Identity, Loop, Ipc, WaitFirstBatch, OpenWindow, Follow, Run]);
            plan
        }
    }
}

/// Executes start_plan (F1a.10). Filled by FA-WIN; until then one log line and the usage code.
pub fn run(cli: Cli) -> i32 {
    let _ = cli;
    let log = oled_sys::logger::start("", &crate::LOG_UI);
    oled_sys::log!("files: the window is not built yet");
    oled_sys::logger::stop(log);
    names::EXIT_USAGE
}
