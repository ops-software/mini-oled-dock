//! `dock.exe --recover [--dry-run]` and safe mode (J1d).
//!
//! --recover gives Windows back what the suite changed and pauses the dock's start at logon, for a desktop that went
//! wrong. Elevated (its role asks for elevation), it runs these steps in order and says one line per step with its
//! result (`logger::say`: the console, or the message box main shows when there is none):
//!  1. asks a running dock to exit and waits for it and every other process of the installed exe (`stop_and_wait`);
//!  2. restores the native taskbar (waiting up to 5 s for Explorer's);
//!  3. reverts the update runs update.jnl holds uncommitted (`update::suite::replay_open`), when it holds any;
//!  4. undoes the machine journal's takeovers, when it holds any;
//!  5. has `dock.exe --undo-user` undo the user journal's at medium, started through Explorer and waited for 15 s;
//!     without a desktop shell (none, or one that does not answer within 5 s), or without its answer, it says the
//!     command to run by hand. --undo-user answers whatever it found, so its line says that it ran, not its result,
//!     and how to see that result;
//!  6. writes `<root>\safe-mode` (safe mode).
//!
//! Exit code as --undo-user's: 0 every step done, 3 some value another program changed since (left as it is), 1 a step
//! that failed. --dry-run (any level) reads what the steps depend on, says the steps it would run and changes nothing.
//!
//! SAFE MODE: while `<root>\safe-mode` exists the logon task's start leaves at once (main, after the data folder is
//! initialized and before any taskbar call); a start by hand deletes it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use oled_sys::journal_os;
use oled_trust::journal::{Journal, Kind, Scope, UndoReport};

use crate::journal_ui::{self, report_line, EXIT_CHANGED_BY_OTHER, EXIT_DONE, EXIT_FAILED};

/// How long --recover waits for Explorer's taskbar before it restores anyway.
const TASKBAR_WAIT: Duration = Duration::from_secs(5);
/// The most bytes of update.jnl read to plan (the journal's own cap).
const JOURNAL_READ_MAX: u64 = 4 * 1024 * 1024;

/// What an `--autostart` start does about safe mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutostartAction {
    Proceed,
    LeaveSafe,
}

/// `--recover [--dry-run]`.
pub fn run(dry_run: bool) -> i32 {
    let steps = plan(&Facts::read());
    run_steps(&steps, dry_run, &mut RealEffects, &mut |line| {
        crate::logger::say(line);
        crate::log!("recover: {line}");
    })
}

/// Safe mode is on: `<root>\safe-mode` is a file (datadir's checked open). False before the data folder is initialized.
pub fn safe_mode() -> bool {
    safe_mode_at(crate::datadir::safe_mode_path().as_deref())
}

/// Leaves safe mode: deletes `<root>\safe-mode` (nothing to do when it is not there).
#[allow(dead_code)] // for the Suite and Safety pages (P3-dock-suite)
pub fn clear_safe_mode() -> std::io::Result<()> {
    clear_safe_mode_at(crate::datadir::safe_mode_path().as_deref())
}

/// Called by main for every Main start: a start by hand leaves safe mode.
pub fn leave_safe_mode_on_manual_start() {
    leave_safe_mode_at(crate::datadir::safe_mode_path().as_deref());
}

/// Pure: an autostart start proceeds unless safe mode is on, in which case it leaves quietly.
pub fn autostart_action(safe_mode: bool) -> AutostartAction {
    if safe_mode {
        AutostartAction::LeaveSafe
    } else {
        AutostartAction::Proceed
    }
}

/// `safe_mode` for the mark at `path` (None: no data folder, never safe mode). A folder or a link there is no mark.
pub(crate) fn safe_mode_at(path: Option<&Path>) -> bool {
    path.is_some_and(|p| crate::datadir::open_checked(p, true).is_ok())
}

fn clear_safe_mode_at(path: Option<&Path>) -> std::io::Result<()> {
    let Some(p) = path else { return Ok(()) };
    // Deleting a name removes the link itself, never a link target.
    match std::fs::remove_file(p) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// The manual start's rule: when the mark is there it is deleted and the log says so. True when safe mode was left.
pub(crate) fn leave_safe_mode_at(path: Option<&Path>) -> bool {
    if !safe_mode_at(path) {
        return false;
    }
    match clear_safe_mode_at(path) {
        Ok(()) => {
            crate::log!("safe mode: left (manual start)");
            true
        }
        Err(e) => {
            crate::log!("safe mode: could not be left ({})", e.kind());
            false
        }
    }
}

/// Writes the mark at `path`: the time (RFC 3339, UTC) and who wrote it, through `write_atomic` (the root's DACL).
pub(crate) fn enter_safe_mode_at(path: &Path, now_unix: u64) -> Result<(), String> {
    let text = format!("{} recover\n", oled_trust::manifest::release::rfc3339_utc(now_unix));
    crate::datadir::write_atomic(path, text.as_bytes(), crate::datadir::FileAcl::Inherit)
}

/// What a journal holds for --recover: nothing open, some open (a count), or a file that cannot be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Found {
    Nothing,
    Open(usize),
    Unreadable(String),
}

/// What the steps depend on, read without changing anything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Facts {
    /// A dock holds its single-instance mutex.
    pub(crate) dock_running: bool,
    /// update.jnl's uncommitted runs.
    pub(crate) updates: Found,
    /// The machine journal's open takeovers.
    pub(crate) takeovers: Found,
    /// The desktop shell answers (it can start --undo-user as the user).
    pub(crate) shell: bool,
}

impl Facts {
    fn read() -> Facts {
        let updates = match crate::datadir::update_journal_path() {
            None => Found::Nothing,
            Some(p) => match crate::datadir::read_file(&p, JOURNAL_READ_MAX, true) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Found::Nothing,
                Err(e) => Found::Unreadable(e.to_string()),
                Ok(bytes) => match Journal::parse(Scope::Machine, &bytes) {
                    Ok(j) => found(open_runs(&j)),
                    Err(e) => Found::Unreadable(format!("{e:?}")),
                },
            },
        };
        let takeovers = match journal_os::path(Scope::Machine) {
            Some(p) if p.exists() => match journal_os::load(Scope::Machine) {
                Ok(j) => found(open_takeovers(&j)),
                Err(e) => Found::Unreadable(e),
            },
            _ => Found::Nothing,
        };
        let shell = shell_answers(|| crate::shell_thread::explorer_pid().is_ok(), journal_ui::DESKTOP_WAIT);
        Facts { dock_running: crate::watchdog::instance_running(), updates, takeovers, shell }
    }
}

/// Whether the desktop shell answers `probe` within `within`: a wedged Explorer does not (its call is left behind),
/// so --recover still says its report.
fn shell_answers(probe: impl FnOnce() -> bool + Send + 'static, within: Duration) -> bool {
    journal_ui::on_com_thread("recover-shell", within, probe).unwrap_or(false)
}

fn found(n: usize) -> Found {
    if n == 0 {
        Found::Nothing
    } else {
        Found::Open(n)
    }
}

/// The runs a journal holds uncommitted (each has open entries), counted without a sort (a journal holds a few runs,
/// and a sort would add its code to the exe).
pub(crate) fn open_runs(j: &Journal) -> usize {
    let mut runs: Vec<u64> = Vec::new();
    for e in j.open_entries() {
        if !runs.contains(&e.run.0) {
            runs.push(e.run.0);
        }
    }
    runs.len()
}

/// The open takeover entries of a journal.
pub(crate) fn open_takeovers(j: &Journal) -> usize {
    j.open_entries().into_iter().filter(|e| matches!(e.kind, Kind::Takeover(_))).count()
}

/// One step of --recover.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// Asked even when no dock seems to run: the wait also covers a watchdog or a helper still running the exe.
    StopDock {
        running: bool,
    },
    RestoreTaskbar,
    ReplayUpdates(Found),
    UndoMachine(Found),
    UndoUser,
    /// No desktop shell to start --undo-user: the command to run by hand.
    UserByHand,
    SafeMode,
}

/// The steps for what was found, in their order (pure).
pub(crate) fn plan(f: &Facts) -> Vec<Step> {
    let mut steps = vec![Step::StopDock { running: f.dock_running }, Step::RestoreTaskbar];
    if f.updates != Found::Nothing {
        steps.push(Step::ReplayUpdates(f.updates.clone()));
    }
    if f.takeovers != Found::Nothing {
        steps.push(Step::UndoMachine(f.takeovers.clone()));
    }
    steps.push(if f.shell { Step::UndoUser } else { Step::UserByHand });
    steps.push(Step::SafeMode);
    steps
}

/// What the steps do to the machine; tests stand in for it.
pub(crate) trait Effects {
    fn stop_dock(&mut self) -> Result<(), String>;
    fn restore_taskbar(&mut self);
    /// The uncommitted runs reverted, and the undo's report.
    fn replay_updates(&mut self) -> Result<(u32, UndoReport), String>;
    fn undo_machine(&mut self) -> Result<UndoReport, String>;
    /// --undo-user as the shell's user: Ok(true) when it answered (it ran; its own result is not known here).
    fn undo_user(&mut self) -> Result<bool, String>;
    fn enter_safe_mode(&mut self) -> Result<(), String>;
    /// The installed dock.exe, for the line that says what to run by hand.
    fn installed_exe(&self) -> String;
}

/// How a step went, worst last.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Outcome {
    Done,
    ChangedByOther,
    Failed,
}

impl Outcome {
    fn of(r: &UndoReport) -> Outcome {
        match journal_ui::exit_code(r) {
            EXIT_DONE => Outcome::Done,
            EXIT_CHANGED_BY_OTHER => Outcome::ChangedByOther,
            _ => Outcome::Failed,
        }
    }

    fn code(self) -> i32 {
        match self {
            Outcome::Done => EXIT_DONE,
            Outcome::ChangedByOther => EXIT_CHANGED_BY_OTHER,
            Outcome::Failed => EXIT_FAILED,
        }
    }
}

/// Runs `steps` (or, with `dry_run`, only says them) and says one line per step; the exit code.
pub(crate) fn run_steps(steps: &[Step], dry_run: bool, fx: &mut dyn Effects, say: &mut dyn FnMut(&str)) -> i32 {
    if dry_run {
        say("--recover --dry-run: the steps --recover would run (nothing is changed now):");
        for (i, s) in steps.iter().enumerate() {
            say(&format!("{}. {}", i + 1, describe(s)));
        }
        return EXIT_DONE;
    }
    say("--recover:");
    let mut worst = Outcome::Done;
    for (i, s) in steps.iter().enumerate() {
        let (outcome, line) = perform(s, fx);
        say(&format!("{}. {line}", i + 1));
        worst = worst.max(outcome);
    }
    worst.code()
}

/// A step as --dry-run says it.
fn describe(s: &Step) -> String {
    match s {
        Step::StopDock { running: true } => "Ask the running dock to exit, and wait for it.".to_string(),
        Step::StopDock { running: false } => "Check that no dock is left running (none found).".to_string(),
        Step::RestoreTaskbar => "Restore the Windows taskbar.".to_string(),
        Step::ReplayUpdates(f) => format!("Revert the unfinished updates ({}).", what(f, "unfinished update run")),
        Step::UndoMachine(f) => format!("Undo the dock's changes to Windows ({}).", what(f, "change")),
        Step::UndoUser => {
            "Undo the dock's changes to your user account (--undo-user, as your normal user).".to_string()
        }
        Step::UserByHand => {
            "Say how to undo the dock's changes to your user account by hand (the desktop does not answer).".to_string()
        }
        Step::SafeMode => "Stop the dock from starting at logon until you start it by hand (safe mode).".to_string(),
    }
}

/// "<n> <noun>(s)", or why the journal cannot be read.
fn what(f: &Found, noun: &str) -> String {
    match f {
        Found::Nothing => format!("no {noun}"),
        Found::Open(1) => format!("1 {noun}"),
        Found::Open(n) => format!("{n} {noun}s"),
        Found::Unreadable(why) => format!("the journal cannot be read: {why}"),
    }
}

/// Runs one step: how it went and its line.
fn perform(s: &Step, fx: &mut dyn Effects) -> (Outcome, String) {
    match s {
        Step::StopDock { running } => match fx.stop_dock() {
            Ok(()) if *running => (Outcome::Done, "The dock: stopped.".to_string()),
            Ok(()) => (Outcome::Done, "The dock: none was running.".to_string()),
            Err(e) => (Outcome::Failed, format!("The dock: could not be stopped ({e}).")),
        },
        Step::RestoreTaskbar => {
            fx.restore_taskbar();
            (Outcome::Done, "Windows taskbar: restored.".to_string())
        }
        Step::ReplayUpdates(_) => match fx.replay_updates() {
            Ok((runs, r)) => (Outcome::of(&r), format!("Unfinished updates: {runs} reverted ({}).", report_line(&r))),
            Err(e) => (Outcome::Failed, format!("Unfinished updates: failed ({e}).")),
        },
        Step::UndoMachine(_) => match fx.undo_machine() {
            Ok(r) => (Outcome::of(&r), format!("The dock's changes to Windows: {}.", report_line(&r))),
            Err(e) => (Outcome::Failed, format!("The dock's changes to Windows: failed ({e}).")),
        },
        Step::UndoUser => match fx.undo_user() {
            Ok(true) => (Outcome::Done, user_part_ran(&fx.installed_exe())),
            other => (Outcome::Failed, by_hand(&journal_ui::user_part_why(&other), &fx.installed_exe())),
        },
        Step::UserByHand => (Outcome::Failed, by_hand("the desktop does not answer", &fx.installed_exe())),
        Step::SafeMode => match fx.enter_safe_mode() {
            Ok(()) => (Outcome::Done, "Safe mode: on (the dock starts again when you start it by hand).".to_string()),
            Err(e) => (Outcome::Failed, format!("Safe mode: failed ({e}).")),
        },
    }
}

/// Step 5's line when --undo-user answered: it ran as the user, but it answers whatever it found (all restored, a busy
/// or corrupt journal, a failure), so the line says how to see its result instead of claiming one.
fn user_part_ran(exe: &str) -> String {
    format!(
        "The dock's changes to your user account: --undo-user ran as your normal user (to see its result, run \"{exe}\" \
         --undo-user in a normal prompt)."
    )
}

fn by_hand(why: &str, exe: &str) -> String {
    format!("The dock's changes to your user account: not done ({why}). Run as your normal user: \"{exe}\" --undo-user")
}

/// The machine.
struct RealEffects;

impl Effects for RealEffects {
    fn stop_dock(&mut self) -> Result<(), String> {
        let exe = crate::autostart::installed_exe()?;
        crate::autostart::stop_and_wait(&exe).map_err(|e| e.to_string())
    }

    fn restore_taskbar(&mut self) {
        crate::taskbar::restore_force_waiting(TASKBAR_WAIT);
    }

    fn replay_updates(&mut self) -> Result<(u32, UndoReport), String> {
        let journal = crate::datadir::update_journal_path().ok_or("no data folder")?;
        let dir = install_dir()?;
        let mut env = crate::update::suite::RealEnv;
        let r = crate::update::suite::replay_open(&dir, &journal, &mut env).map_err(|e| e.what)?;
        Ok((r.runs, r.report))
    }

    fn undo_machine(&mut self) -> Result<UndoReport, String> {
        journal_ui::undo_machine_takeovers()
    }

    fn undo_user(&mut self) -> Result<bool, String> {
        journal_ui::undo_user_through_shell(journal_ui::USER_PART_WAIT)
    }

    fn enter_safe_mode(&mut self) -> Result<(), String> {
        let path = crate::datadir::safe_mode_path().ok_or("no data folder")?;
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        enter_safe_mode_at(&path, now)
    }

    fn installed_exe(&self) -> String {
        crate::autostart::installed_exe().map_or_else(|_| "dock.exe".to_string(), |p| p.display().to_string())
    }
}

/// The install folder (where the suite's components and update.jnl's paths live).
fn install_dir() -> Result<PathBuf, String> {
    let exe = crate::autostart::installed_exe()?;
    exe.parent().map(Path::to_path_buf).ok_or_else(|| "no install folder".to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use oled_trust::journal::{Change, Entry, Hive, Record, RunId, TakeoverId, Target};

    /// The machine of a test: each step's answer, and the steps run, in order.
    struct Fake {
        calls: Vec<&'static str>,
        stop: Result<(), String>,
        replay: Result<(u32, UndoReport), String>,
        machine: Result<UndoReport, String>,
        user: Result<bool, String>,
    }

    impl Fake {
        fn ok() -> Fake {
            Fake {
                calls: Vec::new(),
                stop: Ok(()),
                replay: Ok((1, UndoReport { restored: 2, ..UndoReport::default() })),
                machine: Ok(UndoReport { restored: 3, ..UndoReport::default() }),
                user: Ok(true),
            }
        }
    }

    impl Effects for Fake {
        fn stop_dock(&mut self) -> Result<(), String> {
            self.calls.push("stop");
            self.stop.clone()
        }
        fn restore_taskbar(&mut self) {
            self.calls.push("restore");
        }
        fn replay_updates(&mut self) -> Result<(u32, UndoReport), String> {
            self.calls.push("replay");
            self.replay.clone()
        }
        fn undo_machine(&mut self) -> Result<UndoReport, String> {
            self.calls.push("machine");
            self.machine.clone()
        }
        fn undo_user(&mut self) -> Result<bool, String> {
            self.calls.push("user");
            self.user.clone()
        }
        fn enter_safe_mode(&mut self) -> Result<(), String> {
            self.calls.push("safe");
            Ok(())
        }
        fn installed_exe(&self) -> String {
            r"C:\Program Files\mini-oled-dock\dock.exe".to_string()
        }
    }

    /// The exit code and the lines of a run of `steps` over `fx`.
    fn run(steps: &[Step], dry_run: bool, fx: &mut Fake) -> (i32, Vec<String>) {
        let mut lines = Vec::new();
        let code = run_steps(steps, dry_run, fx, &mut |l| lines.push(l.to_string()));
        (code, lines)
    }

    #[test]
    fn recover_plan() {
        use Step::*;
        let facts = |dock_running, updates, takeovers, shell| Facts { dock_running, updates, takeovers, shell };
        // Nothing found: the stop (it also waits out a watchdog or helper left), the taskbar, the user part, safe mode.
        let quiet = plan(&facts(false, Found::Nothing, Found::Nothing, true));
        assert_eq!(quiet, [StopDock { running: false }, RestoreTaskbar, UndoUser, SafeMode]);
        // Everything found: every step, in order.
        let all = plan(&facts(true, Found::Open(2), Found::Open(3), true));
        let every = [
            StopDock { running: true },
            RestoreTaskbar,
            ReplayUpdates(Found::Open(2)),
            UndoMachine(Found::Open(3)),
            UndoUser,
            SafeMode,
        ];
        assert_eq!(all, every);
        // A journal that cannot be read is still planned (its step says why); without a desktop, the line by hand.
        let (x, y) = (Found::Unreadable("x".into()), Found::Unreadable("y".into()));
        let bad = plan(&facts(false, x.clone(), y.clone(), false));
        assert_eq!(
            bad,
            [StopDock { running: false }, RestoreTaskbar, ReplayUpdates(x), UndoMachine(y), UserByHand, SafeMode]
        );

        // --dry-run: the same list said, no step run, exit 0.
        let mut fx = Fake::ok();
        let (code, lines) = run(&all, true, &mut fx);
        assert_eq!((code, fx.calls.len(), lines.len()), (EXIT_DONE, 0, all.len() + 1), "a dry run changes nothing");
        assert_eq!(lines[1], "1. Ask the running dock to exit, and wait for it.");
        assert_eq!(lines[3], "3. Revert the unfinished updates (2 unfinished update runs).");
        assert_eq!(lines[4], "4. Undo the dock's changes to Windows (3 changes).");
        let (_, lines) = run(&bad, true, &mut fx);
        assert_eq!(lines[3], "3. Revert the unfinished updates (the journal cannot be read: x).");
        assert!(fx.calls.is_empty());

        // The run: every step in order, one line each, exit 0.
        let (code, lines) = run(&all, false, &mut fx);
        assert_eq!(fx.calls, ["stop", "restore", "replay", "machine", "user", "safe"]);
        assert_eq!((code, lines.len()), (EXIT_DONE, all.len() + 1));
        assert_eq!(lines[3], "3. Unfinished updates: 1 reverted (2 restored).");
        // --undo-user answered: it ran, and its own result is not known here, so the line says how to see it.
        let ran = r#"5. The dock's changes to your user account: --undo-user ran as your normal user (to see its result, run "C:\Program Files\mini-oled-dock\dock.exe" --undo-user in a normal prompt)."#;
        assert_eq!(lines[5], ran);
        assert_eq!(lines[6], "6. Safe mode: on (the dock starts again when you start it by hand).");
        // A value another program changed since: 3. A step that fails: 1, and the steps after it still run.
        let other = Target::Reg { hive: Hive::Hklm, key: r"SOFTWARE\x".into(), name: None };
        let mut fx =
            Fake { machine: Ok(UndoReport { changed_by_other: vec![other], ..UndoReport::default() }), ..Fake::ok() };
        assert_eq!(run(&all, false, &mut fx).0, EXIT_CHANGED_BY_OTHER);
        let mut fx = Fake { stop: Err("still running".into()), ..Fake::ok() };
        let (code, lines) = run(&all, false, &mut fx);
        assert_eq!((code, fx.calls.len()), (EXIT_FAILED, 6));
        assert_eq!(lines[1], "1. The dock: could not be stopped (still running).");
        // The user part unanswered, or no desktop at all: the command to run by hand, and 1.
        let mut fx = Fake { user: Ok(false), ..Fake::ok() };
        let (code, lines) = run(&all, false, &mut fx);
        let by_hand = r#"Run as your normal user: "C:\Program Files\mini-oled-dock\dock.exe" --undo-user"#;
        assert_eq!(code, EXIT_FAILED);
        assert!(lines[5].starts_with("5. The dock's changes to your user account: not done (no answer within 15 s)."));
        assert!(lines[5].ends_with(by_hand), "{}", lines[5]);
        let mut fx = Fake::ok();
        let (code, lines) = run(&bad, false, &mut fx);
        assert_eq!(fx.calls, ["stop", "restore", "replay", "machine", "safe"], "no shell: --undo-user is not started");
        assert!(code == EXIT_FAILED && lines[5].ends_with(by_hand));

        // What the planning reads of a journal: its uncommitted runs, and its open takeovers.
        let entry = |seq: u64, run: u64, kind: Kind| {
            let change = Change::FileCreated { path: format!(r"C:\x\{seq}"), after: [0; 32] };
            Record::Entry(Entry { seq, run: RunId(run), ts: 0, kind, change })
        };
        let takeover = || Kind::Takeover(TakeoverId::FolderVerbs);
        let records = [
            entry(1, 1, Kind::Update),
            Record::Committed { run: RunId(1) },
            entry(2, 2, Kind::Update),
            entry(3, 2, Kind::Update),
            entry(4, 3, takeover()),
            entry(5, 4, takeover()),
            Record::Undone { seq: 5 },
            entry(6, 5, Kind::Install),
            entry(7, 6, takeover()),
        ];
        let bytes: Vec<u8> = records.iter().flat_map(|r| Journal::encode(r).unwrap()).collect();
        let j = Journal::parse(Scope::Machine, &bytes).unwrap();
        assert_eq!((open_runs(&j), open_takeovers(&j)), (4, 2));
        assert_eq!((found(0), found(2)), (Found::Nothing, Found::Open(2)));
    }

    #[test]
    fn a_wedged_desktop_still_gets_the_report() {
        // The shell probe stuck in a call into a wedged Explorer: past its bound the desktop counts as not answering,
        // and the report comes at once, with the command to run by hand for the user part.
        let bound = Duration::from_millis(300);
        let t = std::time::Instant::now();
        let shell = shell_answers(
            || -> bool {
                loop {
                    std::thread::park();
                }
            },
            bound,
        );
        let facts = Facts { dock_running: false, updates: Found::Nothing, takeovers: Found::Nothing, shell };
        let mut fx = Fake::ok();
        let (code, lines) = run(&plan(&facts), false, &mut fx);
        let took = t.elapsed();
        assert!(!shell && took < bound + Duration::from_secs(2), "{took:?}");
        assert_eq!(fx.calls, ["stop", "restore", "safe"], "--undo-user is not started");
        assert_eq!((code, lines.len()), (EXIT_FAILED, 5));
        let by_hand = "3. The dock's changes to your user account: not done (the desktop does not answer).";
        assert!(lines[3].starts_with(by_hand), "{}", lines[3]);
        assert!(shell_answers(|| true, Duration::from_secs(10)), "a desktop that answers");
    }

    #[test]
    fn safe_mode_round_trip() {
        let dir = std::env::temp_dir().join(format!("dock-safe-mode-{}", oled_sys::pipe::nonce()));
        std::fs::create_dir_all(&dir).unwrap();
        let mark = dir.join("safe-mode");
        assert!(!safe_mode_at(Some(&mark)));
        assert_eq!(autostart_action(safe_mode_at(Some(&mark))), AutostartAction::Proceed);
        enter_safe_mode_at(&mark, 1_794_227_696).unwrap();
        assert!(safe_mode_at(Some(&mark)));
        assert_eq!(std::fs::read_to_string(&mark).unwrap(), "2026-11-09T12:34:56Z recover\n");
        assert_eq!(autostart_action(safe_mode_at(Some(&mark))), AutostartAction::LeaveSafe);
        assert!(leave_safe_mode_at(Some(&mark)), "a start by hand leaves it");
        assert!(!safe_mode_at(Some(&mark)) && !mark.exists());
        assert!(!leave_safe_mode_at(Some(&mark)), "nothing to leave");
        assert!(clear_safe_mode_at(Some(&mark)).is_ok(), "a missing mark is no error");
        // A folder at the mark's name is no mark, and a start by hand leaves it alone.
        std::fs::create_dir(&mark).unwrap();
        assert!(!safe_mode_at(Some(&mark)) && !leave_safe_mode_at(Some(&mark)) && mark.is_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn safe_mode_is_false_before_init() {
        // The test binary never initializes the data folder: no path, so no safe mode, and nothing to leave or clear.
        // That is why main checks it after start_logging (dispatch_order_in_main pins the place).
        assert!(crate::datadir::safe_mode_path().is_none());
        assert!(!safe_mode());
        assert!(!safe_mode_at(None) && !leave_safe_mode_at(None));
        assert!(clear_safe_mode().is_ok());
        leave_safe_mode_on_manual_start();
    }
}
