//! The dock's side of the change journal (J1c): `dock.exe --undo-user`, and the elevated dock's helpers.
//!
//! --undo-user runs as the shell's user (its role is Medium; the elevated dock starts it through Explorer). Its first
//! act is a deadline thread that ends the process after 20 s whatever a registry or schtasks call is doing, so it never
//! keeps dock.exe mapped for long (the dock waits 15 s for it, the uninstaller's stop_and_wait 35 s). Then the user
//! journal under its lock: its Takeover entries undone, newest first, with F-P5's ownership rule; the file deleted when
//! nothing is left open; the event of the dock that asked signalled, whatever the result. Exit code: 0 every change
//! restored or already as it was, 3 some value changed by another program since (left as it is), 1 a failure, a
//! corrupt journal or one still busy after the wait. UserEdit entries (changes the user made through Tasks) are never
//! undone here.
//!
//! The elevated dock never reads user.jsonl (a file a medium process writes): `user_views` asks the agent, which
//! reads it at medium, and `undo_all_takeovers` undoes the machine journal in process and starts --undo-user for the
//! user's part. The desktop shell's calls need a COM apartment: those helpers make their calls on a thread of their own
//! (one call, then it ends), so any thread may call them; they block, so never the UI thread. A wedged Explorer blocks
//! COM calls into it with no time limit, so each wait for that thread is bounded (`DESKTOP_WAIT`): past it the desktop
//! counts as not answering and the thread is left to end on its own.

use std::path::Path;
use std::time::Duration;

use oled_sys::journal_os::{self, RealOs};
use oled_trust::journal::{self, Journal, JournalOs, KindClass, OsErr, Scope, Sink, UndoReport, UndoSet};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

use crate::shell_thread::Verb;

/// How long --undo-user lives at most.
pub(crate) const UNDO_USER_DEADLINE: Duration = Duration::from_secs(20);
/// How long the elevated dock waits for --undo-user's event: inside the child's own deadline, so a wait that ends in a
/// timeout leaves no child behind for long.
pub(crate) const USER_PART_WAIT: Duration = Duration::from_secs(15);
/// How long a journal's lock is waited for.
const LOCK_WAIT: Duration = Duration::from_secs(5);
/// How long the desktop shell may take to answer (finding it, starting --undo-user or an agent): past it the desktop
/// counts as not answering.
pub(crate) const DESKTOP_WAIT: Duration = Duration::from_secs(5);
/// The stack of the deadline thread: it only sleeps.
const DEADLINE_STACK: usize = 64 * 1024;
/// The event of --undo-user: `Local\mini-oled-dock.dock.undo-user.<nonce>`.
const EVENT_APP: &str = "dock";
const EVENT_PURPOSE: &str = "undo-user";

/// Exit codes of --undo-user (and of --recover, which reports the same way).
pub(crate) const EXIT_DONE: i32 = 0;
pub(crate) const EXIT_FAILED: i32 = 1;
pub(crate) const EXIT_CHANGED_BY_OTHER: i32 = 3;

/// `--undo-user [--signal <nonce>]`, at medium. The open of the user journal is here and nowhere else of the
/// dock's own code (`the_elevated_side_never_reads_user_jsonl`).
pub fn run_undo_user(signal: Option<&str>) -> i32 {
    // A run by hand shows its result in the prompt it was started from; under Explorer there is none.
    crate::attach_console();
    let (code, line) = if !start_deadline(UNDO_USER_DEADLINE) {
        (EXIT_FAILED, "--undo-user: no deadline thread, nothing done".to_string())
    } else if journal_absent(journal_os::path(Scope::User).as_deref()) {
        // Nothing to undo, and nothing created: the lock would create the file and its folder.
        (EXIT_DONE, "--undo-user: nothing to undo".to_string())
    } else {
        let undone = journal_os::open_locked(Scope::User, LOCK_WAIT).map(|(mut j, mut sink)| {
            let report = undo_takeovers(&mut j, &mut sink, &mut RealOs::for_scope(Scope::User));
            let _ = sink.delete_if_settled(&j);
            report
        });
        verdict("--undo-user", &undone)
    };
    crate::logger::say(&line);
    if let Some(nonce) = signal {
        let _ = oled_sys::selftest::signal(EVENT_APP, EVENT_PURPOSE, nonce);
    }
    code
}

/// What --undo-user and the dock's helpers undo: every open Takeover entry of `j`, newest first (F-P5's ownership
/// rule; a value another program changed since is left as it is). Nothing else: not UserEdit, Install or Update.
pub(crate) fn undo_takeovers(j: &mut Journal, sink: &mut dyn Sink, os: &mut dyn JournalOs) -> UndoReport {
    journal::undo(j, sink, os, &UndoSet::Classes(vec![KindClass::Takeover]))
}

/// The exit code of an undo (see the module doc).
pub(crate) fn exit_code(r: &UndoReport) -> i32 {
    if !r.failed.is_empty() {
        EXIT_FAILED
    } else if !r.changed_by_other.is_empty() {
        EXIT_CHANGED_BY_OTHER
    } else {
        EXIT_DONE
    }
}

/// The exit code and the line said for an undo, or for a journal that could not be opened (busy past the wait,
/// corrupt, refused): `who` names the command.
pub(crate) fn verdict(who: &str, undone: &Result<UndoReport, OsErr>) -> (i32, String) {
    match undone {
        Ok(r) => (exit_code(r), format!("{who}: {}", report_line(r))),
        Err(e) => (EXIT_FAILED, format!("{who}: {}", e.what)),
    }
}

/// An undo's report in words: counts only, never a path or a name.
pub(crate) fn report_line(r: &UndoReport) -> String {
    let parts: Vec<String> = [
        (r.restored as usize, "restored"),
        (r.already as usize, "already as it was"),
        (r.changed_by_other.len(), "changed by another program since, left as it is"),
        (r.failed.len(), "failed"),
        (r.unknown as usize, "of a newer version, left"),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, what)| format!("{n} {what}"))
    .collect();
    if parts.is_empty() {
        "nothing to undo".to_string()
    } else {
        parts.join(", ")
    }
}

/// True when no journal file is at `path` (or there is no path: no folder for it).
fn journal_absent(path: Option<&Path>) -> bool {
    path.is_none_or(|p| matches!(std::fs::symlink_metadata(p), Err(e) if e.kind() == std::io::ErrorKind::NotFound))
}

/// The deadline of --undo-user: a thread ("undo-deadline") that ends the process with exit code 1 after `after`,
/// whatever the main thread is doing (TerminateProcess, not ExitProcess: a thread stuck in a call may hold the loader
/// lock). False when the thread could not start.
pub(crate) fn start_deadline(after: Duration) -> bool {
    std::thread::Builder::new()
        .name("undo-deadline".into())
        .stack_size(DEADLINE_STACK)
        .spawn(move || {
            std::thread::sleep(after);
            // SAFETY: the pseudo-handle of this process; TerminateProcess on it does not return.
            let _ = unsafe { TerminateProcess(GetCurrentProcess(), EXIT_FAILED as u32) };
        })
        .is_ok()
}

/// Machine journal entries for the Safety page, in file order (open entries only); empty when there is no machine
/// journal or it cannot be read (logged).
#[allow(dead_code)] // read by the Safety page (P3-dock-suite)
pub fn machine_views() -> Vec<oled_trust::journal::EntryView> {
    if journal_absent(journal_os::path(Scope::Machine).as_deref()) {
        return Vec::new();
    }
    match journal_os::load(Scope::Machine) {
        Ok(j) => journal::views(&j),
        Err(e) => {
            crate::log!("journal: the machine journal could not be read ({e})");
            Vec::new()
        }
    }
}

/// User journal entries (its open takeovers, at most `agent::JOURNAL_RECS_MAX`), read by the agent at medium: an agent
/// started for this call answers within `timeout` (its start adds up to 5 s), then ends. Err when no agent can serve
/// (a dock that is not elevated, no desktop shell, one that does not answer) or it could not read the journal.
#[allow(dead_code)] // read by the Safety page (P3-dock-suite)
pub fn user_views(timeout: std::time::Duration) -> Result<Vec<crate::agent_proto::JournalRec>, String> {
    // The shell's calls before the agent's start, then its start and its answer.
    let within = timeout.saturating_add(DESKTOP_WAIT).saturating_add(DESKTOP_WAIT);
    on_com_thread("journal-agent", within, move || {
        let mut link = crate::agent::Link::shell();
        let records = link.user_journal(timeout);
        link.exit();
        records
    })?
}

/// Undo of every takeover: the machine journal's in this process, then the user journal's by --undo-user, started as
/// the shell's user and waited for 15 s. The flag says whether --undo-user answered: it ran as the shell's user, but it
/// answers whatever it found (all restored, some changed by another program, a busy or corrupt journal, a failure), so
/// its own result is not known here. Err when the machine journal could not be undone (busy, corrupt, not writable);
/// the user part was still tried, and the error says whether --undo-user answered.
#[allow(dead_code)] // called by the Safety page (P3-dock-suite) and the uninstaller (I1d)
pub fn undo_all_takeovers() -> Result<(oled_trust::journal::UndoReport, bool), String> {
    let machine = undo_machine_takeovers();
    let user = undo_user_through_shell(USER_PART_WAIT);
    let user_answered = matches!(user, Ok(true));
    if !user_answered {
        crate::log!("journal: --undo-user did not answer ({})", user_part_why(&user));
    }
    match machine {
        Ok(report) => Ok((report, user_answered)),
        Err(why) => Err(format!(
            "the machine journal: {why}; --undo-user {}",
            if user_answered { "answered" } else { "did not answer" }
        )),
    }
}

/// The machine journal's takeovers undone under its lock (5 s), the file deleted when nothing is left open. An empty
/// report when there is no machine journal. Elevated only (`open_locked` refuses a medium process).
pub(crate) fn undo_machine_takeovers() -> Result<UndoReport, String> {
    if journal_absent(journal_os::path(Scope::Machine).as_deref()) {
        return Ok(UndoReport::default());
    }
    let (mut j, mut sink) = journal_os::open_locked(Scope::Machine, LOCK_WAIT).map_err(|e| e.what)?;
    let report = undo_takeovers(&mut j, &mut sink, &mut RealOs::for_scope(Scope::Machine));
    let _ = sink.delete_if_settled(&j);
    Ok(report)
}

/// --undo-user started as the shell's user (`desktop_shell_execute`, hidden, with a fresh event only that user may
/// signal) and waited for up to `wait`: Ok(true) when it signalled (it ran, whatever its result), Ok(false) when it
/// did not in time, Err when it could not be started (no desktop shell or one that does not answer within
/// `DESKTOP_WAIT`, the dock's exe not trusted, a squatted event).
pub(crate) fn undo_user_through_shell(wait: Duration) -> Result<bool, String> {
    let waiter = on_com_thread("undo-user-start", DESKTOP_WAIT, || {
        let pid = crate::shell_thread::explorer_pid().map_err(|e| format!("no desktop shell ({:#x})", e.code().0))?;
        let sid = oled_sys::pipe::user_sid_of_pid(pid).map_err(|e| format!("the shell's user: {e}"))?;
        let (waiter, nonce) = oled_sys::selftest::Waiter::create(EVENT_APP, EVENT_PURPOSE, &sid)
            .map_err(|e| format!("its event: {e}"))?;
        let exe = crate::update::child_exe()?;
        let args = format!("--undo-user --signal {nonce}");
        crate::shell_thread::desktop_shell_execute(&exe.to_string_lossy(), Some(&args), None, Verb::Open, SW_HIDE.0)
            .map_err(|e| format!("the shell did not start it ({:#x})", e.code().0))?;
        Ok::<_, String>(waiter)
    })??;
    Ok(waiter.wait(wait))
}

/// Why the user part is not done, for a line.
pub(crate) fn user_part_why(r: &Result<bool, String>) -> String {
    match r {
        Ok(true) => "answered".to_string(),
        Ok(false) => format!("no answer within {} s", USER_PART_WAIT.as_secs()),
        Err(why) => why.clone(),
    }
}

/// Runs `f` on a new thread in a COM single-threaded apartment (the desktop shell's calls need one) and returns its
/// result when the thread gives it within `within`. A call into a wedged Explorer blocks with no time limit: past
/// `within` this is Err ("the desktop does not answer") and the thread is left to end on its own, or with the process.
/// Err as well when the thread or COM could not start.
pub(crate) fn on_com_thread<T: Send + 'static>(
    name: &str,
    within: Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    let slot = std::sync::Arc::new(std::sync::Mutex::new(None));
    let put = std::sync::Arc::clone(&slot);
    in_sta_within(
        name,
        within,
        Box::new(move || *put.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(f())),
    )?;
    let out = slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
    out.ok_or_else(|| "its thread failed".to_string())
}

/// The part of `on_com_thread` that is not generic (one copy of the thread and channel code in the exe): `f` run on a
/// thread named `name` in a COM STA, Ok when it ran within `within`.
fn in_sta_within(name: &str, within: Duration, f: Box<dyn FnOnce() + Send>) -> Result<(), String> {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            // SAFETY: the first COM call of this new thread; balanced below once `f` and its objects are gone.
            if unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok() {
                f();
                // SAFETY: balances the successful CoInitializeEx of this thread.
                unsafe { CoUninitialize() };
                // A caller that stopped waiting is gone: nothing to tell.
                let _ = tx.send(());
            }
            // Without an apartment `f` is not run and `tx` goes unsent: the caller sees the thread end.
        })
        .map_err(|e| format!("no thread: {e}"))?;
    match rx.recv_timeout(within) {
        Ok(()) => Ok(()),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            Err(format!("the desktop does not answer within {} s", within.as_secs()))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err("no COM apartment, or its thread failed".into()),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use oled_trust::journal::{AppId, Change, Hive, Kind, RegData, RegPath, TakeoverId, Target};
    use std::collections::BTreeMap;
    use std::io::BufRead;
    use std::path::PathBuf;
    use std::time::Instant;

    /// The registry of a test: HKCU values by (key, name); with `hang`, a value write never returns (a registry call
    /// that hangs); a value named in `refuse` cannot be written.
    #[derive(Default)]
    struct FakeOs {
        values: BTreeMap<(String, Option<String>), RegData>,
        hang: bool,
        refuse: Vec<String>,
    }

    fn slot(key: &RegPath, name: Option<&str>) -> (String, Option<String>) {
        (key.as_str().to_string(), name.map(String::from))
    }

    impl JournalOs for FakeOs {
        fn reg_get(&self, _: Hive, key: &RegPath, name: Option<&str>) -> Result<Option<RegData>, OsErr> {
            Ok(self.values.get(&slot(key, name)).cloned())
        }
        fn reg_set(&mut self, _: Hive, key: &RegPath, name: Option<&str>, data: Option<&RegData>) -> Result<(), OsErr> {
            if self.hang {
                loop {
                    std::thread::park();
                }
            }
            if name.is_some_and(|n| self.refuse.iter().any(|r| r == n)) {
                return Err(OsErr { code: 5, what: "access denied".into() });
            }
            match data {
                Some(d) => self.values.insert(slot(key, name), d.clone()),
                None => self.values.remove(&slot(key, name)),
            };
            Ok(())
        }
        fn reg_key_exists(&self, _: Hive, _: &RegPath) -> Result<bool, OsErr> {
            Ok(true)
        }
        fn reg_key_create(&mut self, _: Hive, _: &RegPath) -> Result<(), OsErr> {
            Ok(())
        }
        fn reg_key_delete_if_empty(&mut self, _: Hive, _: &RegPath) -> Result<bool, OsErr> {
            Ok(false)
        }
        fn reg_tree_get(&self, _: Hive, _: &RegPath) -> Result<Option<oled_trust::journal::RegSnapshot>, OsErr> {
            Ok(None)
        }
        fn reg_tree_put(
            &mut self,
            _: Hive,
            _: &RegPath,
            _: Option<&oled_trust::journal::RegSnapshot>,
        ) -> Result<(), OsErr> {
            Ok(())
        }
        fn task_xml(&self, _: &oled_trust::journal::TaskName) -> Result<Option<String>, OsErr> {
            Ok(None)
        }
        fn task_put(&mut self, _: &oled_trust::journal::TaskName, _: Option<&str>) -> Result<(), OsErr> {
            Ok(())
        }
        fn file_digest(&self, _: &str) -> Result<Option<oled_trust::journal::Digest>, OsErr> {
            Ok(None)
        }
        fn file_delete(&mut self, _: &str) -> Result<(), OsErr> {
            Ok(())
        }
        fn file_restore(&mut self, _: &str, _: &str) -> Result<(), OsErr> {
            Ok(())
        }
        fn digest(&self, bytes: &[u8]) -> Result<oled_trust::journal::Digest, OsErr> {
            // A test hash (FNV-1a 64 in the first lane), not cryptographic.
            let mut h: u64 = 0xcbf2_9ce4_8422_2325;
            for b in bytes {
                h = (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3);
            }
            let mut d = [0u8; 32];
            d[..8].copy_from_slice(&h.to_le_bytes());
            Ok(d)
        }
    }

    /// The journal file in memory.
    #[derive(Default)]
    struct Disk(Vec<u8>);

    impl Sink for Disk {
        fn append(&mut self, bytes: &[u8]) -> Result<(), OsErr> {
            self.0.extend_from_slice(bytes);
            Ok(())
        }
    }

    const KEY: &str = r"Software\Classes\Directory\shell\oled-test";
    const TAKEOVER: Kind = Kind::Takeover(TakeoverId::FolderVerbs);

    fn value(s: &str) -> RegData {
        RegData::Sz(s.to_string())
    }

    /// A value change `before` -> "ours" of `name` under KEY.
    fn change(name: &str, before: Option<&str>) -> Change {
        Change::RegValue {
            hive: Hive::Hkcu,
            key: RegPath::new(KEY).unwrap(),
            name: Some(name.to_string()),
            before: before.map(value),
            after: Some(value("ours")),
        }
    }

    /// A user journal with one entry per (kind, value name, value before), each its own run, and the fake registry as
    /// each change left it.
    fn journal_of(entries: &[(Kind, &str, Option<&str>)]) -> (Journal, Disk, FakeOs) {
        let mut j = Journal::empty(Scope::User);
        let mut disk = Disk::default();
        let mut os = FakeOs::default();
        for (kind, name, before) in entries {
            let run = j.begin_run();
            journal::record(&mut j, &mut disk, run, kind.clone(), change(name, *before), 1_700_000_000).unwrap();
            os.values.insert((KEY.to_string(), Some(name.to_string())), value("ours"));
        }
        (j, disk, os)
    }

    fn current(os: &FakeOs, name: &str) -> Option<String> {
        match os.values.get(&(KEY.to_string(), Some(name.to_string()))) {
            Some(RegData::Sz(s)) => Some(s.clone()),
            _ => None,
        }
    }

    #[test]
    fn undo_user_undoes_takeovers_only() {
        let (mut j, mut disk, mut os) = journal_of(&[
            (TAKEOVER, "restored", Some("theirs")),
            (Kind::UserEdit(AppId::Tasks), "edited", Some("before the edit")),
            (TAKEOVER, "created", None),
            (TAKEOVER, "already", Some("old")),
            (TAKEOVER, "other", Some("old")),
            (Kind::UserEdit(AppId::Files), "edited too", None),
        ]);
        // Since the takeovers: one value is back as it was, another program changed another.
        os.values.insert((KEY.to_string(), Some("already".into())), value("old"));
        os.values.insert((KEY.to_string(), Some("other".into())), value("another program's"));
        let report = undo_takeovers(&mut j, &mut disk, &mut os);
        assert_eq!((report.restored, report.already, report.failed.len(), report.unknown), (2, 1, 0, 0));
        let other = Target::Reg { hive: Hive::Hkcu, key: KEY.to_string(), name: Some("other".into()) };
        assert_eq!(report.changed_by_other, [other]);
        assert_eq!(current(&os, "restored").as_deref(), Some("theirs"));
        assert_eq!(current(&os, "created"), None, "a value the takeover created is deleted");
        assert_eq!(current(&os, "already").as_deref(), Some("old"));
        assert_eq!(current(&os, "other").as_deref(), Some("another program's"), "left as it is");
        // The user's own edits are untouched, and only they stay open on disk.
        assert_eq!(current(&os, "edited").as_deref(), Some("ours"));
        assert_eq!(current(&os, "edited too").as_deref(), Some("ours"));
        let reread = Journal::parse(Scope::User, &disk.0).unwrap();
        let open: Vec<Kind> = reread.open_entries().into_iter().map(|e| e.kind.clone()).collect();
        assert_eq!(open, [Kind::UserEdit(AppId::Tasks), Kind::UserEdit(AppId::Files)]);
        assert_eq!(verdict("--undo-user", &Ok(report)).0, EXIT_CHANGED_BY_OTHER);
        // A second run finds nothing left to do.
        let again = undo_takeovers(&mut j, &mut disk, &mut os);
        assert_eq!(again, UndoReport::default());
        assert_eq!(verdict("--undo-user", &Ok(again)), (EXIT_DONE, "--undo-user: nothing to undo".to_string()));
    }

    #[test]
    fn exit_codes() {
        let target = || Target::Reg { hive: Hive::Hkcu, key: KEY.to_string(), name: None };
        let err = || OsErr { code: 5, what: "access denied".into() };
        let done = UndoReport { restored: 2, already: 1, ..UndoReport::default() };
        assert_eq!(exit_code(&UndoReport::default()), EXIT_DONE);
        assert_eq!(exit_code(&done), EXIT_DONE);
        assert_eq!(
            exit_code(&UndoReport { unknown: 1, ..done.clone() }),
            EXIT_DONE,
            "a newer kind is left, no failure"
        );
        let other = UndoReport { changed_by_other: vec![target()], ..done.clone() };
        assert_eq!(exit_code(&other), EXIT_CHANGED_BY_OTHER);
        let failed = UndoReport { failed: vec![(target(), err())], ..other.clone() };
        assert_eq!(exit_code(&failed), EXIT_FAILED);
        assert_eq!(
            verdict("--undo-user", &Ok(failed)).1,
            "--undo-user: 2 restored, 1 already as it was, 1 changed by another program since, left as it is, 1 failed"
        );
        // A journal another writer holds past the wait, or one that does not parse: a failure, nothing undone.
        let busy = OsErr { code: 32, what: "journal busy".into() };
        assert_eq!(verdict("--undo-user", &Err(busy)), (EXIT_FAILED, "--undo-user: journal busy".to_string()));
        let dir = std::env::temp_dir().join(format!("dock-undo-user-{}", oled_sys::pipe::nonce()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("j.jsonl");
        std::fs::write(&file, b"00000000 {}\n").unwrap();
        let corrupt =
            journal_os::open_locked_at(&file, Scope::Machine, Duration::from_secs(1)).map(|_| UndoReport::default());
        assert_eq!(verdict("--undo-user", &corrupt).0, EXIT_FAILED);
        assert!(journal_absent(Some(dir.join("missing.jsonl").as_path())) && !journal_absent(Some(file.as_path())));
        assert!(journal_absent(None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn on_com_thread_is_bounded() {
        // A call into a wedged Explorer never returns: the wait ends at its bound and says so; the thread is left.
        let bound = Duration::from_millis(300);
        let t = Instant::now();
        let stuck = on_com_thread("test-wedged", bound, || -> bool {
            loop {
                std::thread::park();
            }
        });
        let took = t.elapsed();
        assert!(stuck.as_ref().is_err_and(|e| e.contains("does not answer")), "{stuck:?}");
        assert!(took >= bound && took < bound + Duration::from_secs(2), "{took:?}");
        // A call that answers gives its result.
        assert_eq!(on_com_thread("test-answers", Duration::from_secs(10), || 7), Ok(7));
    }

    /// The source of a file before its test module (a test may name what the code may not).
    fn code_part(src: &str) -> &str {
        let lines: Vec<&str> = src.split_inclusive('\n').collect();
        let mut at = 0;
        for (i, line) in lines.iter().enumerate() {
            if line.trim_end() == "#[cfg(test)]" {
                let next = lines[i + 1..].iter().find(|l| !l.trim_start().starts_with("#["));
                if next.is_some_and(|l| l.contains("mod ") && l.trim_end().ends_with('{')) {
                    return &src[..at];
                }
            }
            at += line.len();
        }
        src
    }

    fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                rust_files(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }

    /// The name of the function whose body holds byte `at` of `src` (the nearest `fn <name>` before it).
    fn enclosing_fn(src: &str, at: usize) -> String {
        let before = &src[..at];
        let mut from = before.len();
        while let Some(i) = before[..from].rfind("fn ") {
            let starts_word = i == 0 || !before[..i].ends_with(|c: char| c.is_alphanumeric() || c == '_');
            if starts_word {
                return before[i + 3..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            }
            from = i;
        }
        String::new()
    }

    /// Every call of the user journal's readers with Scope::User in `src`'s code (whitespace ignored): the byte
    /// offset of each.
    fn user_journal_calls(src: &str) -> Vec<usize> {
        // The code without whitespace, and where each of its bytes was.
        let (mut flat, mut at) = (String::new(), Vec::new());
        for (i, c) in src.char_indices().filter(|(_, c)| !c.is_whitespace()) {
            flat.push(c);
            at.extend(std::iter::repeat_n(i, c.len_utf8()));
        }
        let mut calls = Vec::new();
        for reader in ["load(", "open_locked(", "open_locked_at("] {
            for (i, _) in flat.match_indices(reader) {
                let args = &flat[i + reader.len()..];
                let mut depth = 1usize;
                let end = args
                    .char_indices()
                    .find(|(_, c)| {
                        match *c {
                            '(' => depth += 1,
                            ')' => depth -= 1,
                            _ => {}
                        }
                        depth == 0
                    })
                    .map_or(args.len(), |(k, _)| k);
                if args[..end].contains(concat!("Scope::", "User")) {
                    calls.push(at[i]);
                }
            }
        }
        calls
    }

    #[test]
    fn the_elevated_side_never_reads_user_jsonl() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = Vec::new();
        rust_files(&root.join("src"), &mut files);
        rust_files(&root.join("crates"), &mut files);
        let mut found = Vec::new();
        for f in &files {
            let src = std::fs::read_to_string(f).unwrap();
            let code = code_part(&src);
            let rel = f.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            for at in user_journal_calls(code) {
                found.push((rel.clone(), enclosing_fn(code, at)));
            }
        }
        found.sort();
        // At medium only, one call each: --undo-user's own run, and the agent's reader (its UserJournal answer). Each
        // has a name no other function of its file has, so a call added to the elevated side cannot hide behind it.
        let allowed = [
            ("src/agent.rs".to_string(), "read_user_journal_at_medium".to_string()),
            ("src/journal_ui.rs".into(), "run_undo_user".into()),
        ];
        assert_eq!(found, allowed, "the user journal is opened or read outside --undo-user and the agent");
        for (file, name) in &allowed {
            let src = std::fs::read_to_string(root.join(file)).unwrap();
            assert_eq!(code_part(&src).matches(&format!("fn {name}(")).count(), 1, "{file}: one fn {name}");
        }
    }

    /// Turns this test binary into the helper of `undo_user_ends_itself`; set by it only.
    const HELPER_VAR: &str = "DOCK_UNDO_USER_TEST_HELPER";

    /// A hard link of this test binary, for the helper process: a test that looks for processes running this binary by
    /// its path (`autostart::exe_in_use_by_other`) never sees it. Removed when dropped.
    struct HelperExe(PathBuf);

    impl Drop for HelperExe {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    #[ignore = "the helper process undo_user_ends_itself starts; does nothing on its own"]
    fn undo_user_hang_process() {
        if std::env::var(HELPER_VAR).as_deref() != Ok("hang") {
            return;
        }
        // --undo-user's work with a registry write that never returns: only the deadline ends the process.
        let (mut j, mut disk, mut os) = journal_of(&[(TAKEOVER, "stuck", None)]);
        os.hang = true;
        if !start_deadline(UNDO_USER_DEADLINE) {
            std::process::exit(2);
        }
        println!("deadline started");
        let _ = undo_takeovers(&mut j, &mut disk, &mut os);
        std::process::exit(0);
    }

    #[test]
    fn undo_user_ends_itself() {
        use std::os::windows::process::CommandExt;
        use windows::Win32::System::Threading::CREATE_NO_WINDOW;
        let exe = std::env::current_exe().unwrap();
        let link = HelperExe(exe.with_file_name(format!("undo-user-helper-{}.exe", std::process::id())));
        let _ = std::fs::remove_file(&link.0);
        std::fs::hard_link(&exe, &link.0).unwrap();
        let mut child = std::process::Command::new(&link.0)
            .creation_flags(CREATE_NO_WINDOW.0)
            .args([
                "journal_ui::tests::undo_user_hang_process",
                "--exact",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(HELPER_VAR, "hang")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let out = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        });
        // libtest's "test <name> ... " has no newline before the first line a test prints.
        let started = loop {
            match rx.recv_timeout(Duration::from_secs(60)) {
                Ok(l) if l.trim_end().ends_with("deadline started") => break Instant::now(),
                Ok(_) => {}
                Err(e) => {
                    let _ = child.kill();
                    panic!("the helper did not start its deadline: {e}");
                }
            }
        };
        let limit = UNDO_USER_DEADLINE + Duration::from_secs(1);
        let ended = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break Some((started.elapsed(), status.code()));
            }
            if started.elapsed() > limit {
                break None;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        if ended.is_none() {
            let _ = child.kill();
        }
        let _ = child.wait();
        let (took, code) = ended.expect("--undo-user outlived its deadline by more than 1 s");
        assert!(took >= UNDO_USER_DEADLINE - Duration::from_secs(2), "ended early: {took:?}");
        assert_eq!(code, Some(EXIT_FAILED), "ended by its deadline, not by the undo");
    }
}
